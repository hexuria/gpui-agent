use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{Op, Response};
use gpui_agent::{CallFailure, error_code_of};
use thiserror::Error;

use crate::plan::{Plan, PlannedStep};
use crate::receipt::{RECEIPT_FORMAT_VERSION, Receipt, ScreenshotReceipt, StepReceipt};
use crate::schema::Effect;

/// Why a plan run stopped before completing.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunError {
    /// The recipe declares shutdown/exit effects and `yes` was not set.
    #[error("recipe has shutdown/exit effects; pass --yes to run it")]
    NeedsYes,
    /// A step's request failed; carries the receipt so far.
    #[error("step `{id}` failed: {error}")]
    Step {
        /// The failing step's id.
        id: String,
        /// Host-returned or transport error text.
        error: String,
        /// Stable code of the failure ([`gpui_agent::error_code`]).
        error_code: String,
        /// Receipt so far; a failed pipelined read wave keeps every
        /// reply of that wave, including steps after the failing one.
        receipt: Box<Receipt>,
    },
}

/// After selected recipe steps, ask the host to write an app-surface PNG.
///
/// Default (dir set): every step. `flagged_only`: only steps with
/// `screenshot: true` in JSON / `--screenshot` in wants.
///
/// Build with [`ScreenshotCapture::new`]; new options arrive as new setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ScreenshotCapture {
    /// Directory the step PNGs are written into.
    pub dir: PathBuf,
    /// True: only steps flagged `screenshot: true` are captured.
    pub flagged_only: bool,
}

impl ScreenshotCapture {
    /// Capture after every step, into `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            flagged_only: false,
        }
    }

    /// Capture only after steps flagged `screenshot: true` (or after every
    /// step again).
    #[must_use]
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "the flag mirrors the CLI's --screenshot-flagged"
    )]
    pub fn with_flagged_only(mut self, flagged_only: bool) -> Self {
        self.flagged_only = flagged_only;
        self
    }

    /// Filename `{index:03}-{sanitized step id}.png` under `dir`.
    pub fn step_path(&self, index: u32, step_id: &str) -> PathBuf {
        self.dir
            .join(format!("{index:03}-{}.png", sanitize_step_id(step_id)))
    }

    fn wants(&self, step: &PlannedStep) -> bool {
        !self.flagged_only || step.screenshot
    }
}

/// How [`run_plan`] executes a plan. Build with [`RunOptions::new`] and
/// the `with_*` setters; new options arrive as new setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunOptions {
    /// Allow plans with exit effects (`shutdown`, a quit keybinding).
    pub yes: bool,
    /// Per-step app-surface PNG capture.
    pub screenshots: Option<ScreenshotCapture>,
}

impl RunOptions {
    /// Defaults: exit effects refused, no screenshots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow (or refuse) plans with exit effects.
    #[must_use]
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "the flag mirrors the CLI's --yes"
    )]
    pub fn with_yes(mut self, yes: bool) -> Self {
        self.yes = yes;
        self
    }

    /// Capture an app-surface PNG after steps.
    #[must_use]
    pub fn with_screenshots(mut self, screenshots: Option<ScreenshotCapture>) -> Self {
        self.screenshots = screenshots;
        self
    }
}

/// Execute a compiled plan on one reused TCP session.
///
/// Each step is still a normal protocol request (token, version, caps).
/// All-Read waves (no `Write`/`Exit`, no `--screenshot-dir`) use
/// [`AgentClient::rpc_pipeline`](gpui_agent::client::AgentClient::rpc_pipeline)
/// to hide localhost RTT. Write, Exit, mixed, and screenshot-dir waves stay
/// sequential fail-fast (no later sibling). A failed wave does not start the
/// next one. The recipe layer never talks to a shell and never skips
/// `authorize_request`.
///
/// # Errors
/// [`RunError::NeedsYes`] on shutdown/exit effects without `yes`;
/// [`RunError::Step`] on the first failing request.
///
/// # Panics
/// On a wave/id mismatch between the plan and its schedule (a bug,
/// not reachable via host failures).
///
/// The client best-effort creates `screenshots.dir`. A host that lists
/// `screenshot_inline` sends the PNG over the wire and the client writes
/// it there. Any other host is sent only a filename and writes under its
/// own screenshot directory; the receipt then records the path the host
/// reports.
#[expect(
    clippy::too_many_lines,
    reason = "one sequential wave loop; splitting pipeline/sequential paths would duplicate the step accounting"
)]
/// Screenshot unavailability is recorded on the receipt and does **not**
/// fail the recipe. No fake PNG is written.
pub fn run_plan(
    client: &mut AgentClient,
    plan: &Plan,
    options: &RunOptions,
) -> Result<Receipt, RunError> {
    let screenshots = options.screenshots.as_ref();
    if plan.requires_yes && !options.yes {
        return Err(RunError::NeedsYes);
    }

    let mut shot_transport = ShotTransport::HostFile;
    if let Some(capture) = screenshots {
        let _ = std::fs::create_dir_all(&capture.dir);
        if client
            .features()
            .is_ok_and(|features| features.iter().any(|f| f == "screenshot_inline"))
        {
            shot_transport = ShotTransport::Inline;
        }
    }

    let started = Instant::now();
    let mut receipt = Receipt {
        v: RECEIPT_FORMAT_VERSION,
        ok: true,
        recipe: plan.name.clone(),
        fingerprint: plan.fingerprint.clone(),
        session_reused: false,
        steps: Vec::with_capacity(plan.steps.len()),
        screenshots: Vec::new(),
        elapsed_ms: 0,
    };

    let mut shot_index = 0u32;
    for wave in &plan.waves {
        let wave_steps: Vec<&PlannedStep> = wave
            .iter()
            .map(|id| {
                plan.steps
                    .iter()
                    .find(|step| step.id == *id)
                    .expect("wave id is a planned step")
            })
            .collect();
        if wave_can_pipeline(&wave_steps, screenshots) {
            let ops: Vec<&Op> = wave_steps.iter().map(|step| &step.op).collect();
            let step_started = Instant::now();
            match client.rpc_pipeline(&ops) {
                Ok(resps) => {
                    receipt.session_reused = client.has_session();
                    let elapsed =
                        u64::try_from(step_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let mut failed: Option<(String, String, String)> = None;
                    for (step, resp) in wave_steps.iter().zip(resps) {
                        let ok = resp.ok;
                        let error = resp.error.clone();
                        let error_code = resp.code().map(str::to_string);
                        if !ok && failed.is_none() {
                            failed = Some((
                                step.id.clone(),
                                error.clone().unwrap_or_else(|| "request failed".into()),
                                error_code
                                    .clone()
                                    .unwrap_or_else(|| gpui_agent::error_code::HOST_ERROR.into()),
                            ));
                        }
                        receipt.steps.push(StepReceipt {
                            id: step.id.clone(),
                            ok,
                            error,
                            error_code,
                            // Pipelined waves are read-only: nothing to deliver.
                            delivery_status: None,
                            elapsed_ms: elapsed,
                            result: resp.result,
                            screenshot: None,
                        });
                    }
                    if let Some((id, error, error_code)) = failed {
                        receipt.ok = false;
                        receipt.elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        return Err(RunError::Step {
                            id,
                            error,
                            error_code,
                            receipt: Box::new(receipt),
                        });
                    }
                }
                Err(error) => {
                    receipt.ok = false;
                    receipt.session_reused = client.has_session();
                    let first = wave_steps[0];
                    receipt.steps.push(StepReceipt {
                        id: first.id.clone(),
                        ok: false,
                        error_code: Some(error_code_of(&error).to_string()),
                        delivery_status: None,
                        error: Some(error.clone()),
                        elapsed_ms: u64::try_from(step_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        result: None,
                        screenshot: None,
                    });
                    receipt.elapsed_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Err(RunError::Step {
                        id: first.id.clone(),
                        error_code: error_code_of(&error).to_string(),
                        error,
                        receipt: Box::new(receipt),
                    });
                }
            }
            continue;
        }

        for step in wave_steps {
            let step_started = Instant::now();
            shot_index += 1;
            match send_step(client, step) {
                Ok(resp) => {
                    receipt.session_reused = client.has_session();
                    let ok = resp.ok;
                    let error = resp.error.clone();
                    let error_code = resp.code().map(str::to_string);
                    let screenshot =
                        capture_screenshot(client, screenshots, shot_transport, shot_index, step);
                    if let Some(shot) = screenshot.clone() {
                        receipt.screenshots.push(shot);
                    }
                    receipt.steps.push(StepReceipt {
                        id: step.id.clone(),
                        ok,
                        error: error.clone(),
                        error_code: error_code.clone(),
                        delivery_status: resp.delivery_status,
                        elapsed_ms: u64::try_from(step_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        result: resp.result,
                        screenshot,
                    });
                    if !ok {
                        receipt.ok = false;
                        receipt.elapsed_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        return Err(RunError::Step {
                            id: step.id.clone(),
                            error: error.unwrap_or_else(|| "request failed".into()),
                            error_code: error_code
                                .unwrap_or_else(|| gpui_agent::error_code::HOST_ERROR.into()),
                            receipt: Box::new(receipt),
                        });
                    }
                }
                Err(failure) => {
                    let error = failure.error;
                    let error_code = failure.error_code;
                    receipt.ok = false;
                    receipt.session_reused = client.has_session();
                    let screenshot =
                        capture_screenshot(client, screenshots, shot_transport, shot_index, step);
                    if let Some(shot) = screenshot.clone() {
                        receipt.screenshots.push(shot);
                    }
                    receipt.steps.push(StepReceipt {
                        id: step.id.clone(),
                        ok: false,
                        error: Some(error.clone()),
                        error_code: Some(error_code.clone()),
                        delivery_status: failure.delivery_status,
                        elapsed_ms: u64::try_from(step_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        result: None,
                        screenshot,
                    });
                    receipt.elapsed_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Err(RunError::Step {
                        id: step.id.clone(),
                        error,
                        error_code,
                        receipt: Box::new(receipt),
                    });
                }
            }
        }
    }

    receipt.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    receipt.session_reused = client.has_session();
    Ok(receipt)
}

/// Sends one sequential step: its op as one request, or a paced `type`
/// ([`PlannedStep::key_delay_ms`]) as one `type` request per character with
/// the pause between two of them. A paced step stops at the first key the
/// host refuses and answers with that refusal; otherwise with the last
/// key's answer. Only the first key carries the step's fingerprint gate:
/// each key changes the node's value, and so its fingerprint.
fn send_step(client: &mut AgentClient, step: &PlannedStep) -> Result<Response, CallFailure> {
    let (
        Some(delay),
        Op::Type {
            target,
            text,
            delivery,
            expect_fingerprint,
        },
    ) = (step.key_delay_ms, &step.op)
    else {
        return client.call_response(&step.op);
    };
    let keys = paced_keys(text);
    if keys.len() < 2 {
        return client.call_response(&step.op);
    }
    let mut last = None;
    for (index, key) in keys.into_iter().enumerate() {
        if index > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        let op = Op::Type {
            target: target.clone(),
            text: key,
            delivery: *delivery,
            expect_fingerprint: if index == 0 {
                *expect_fingerprint
            } else {
                None
            },
        };
        let resp = client.call_response(&op)?;
        if !resp.ok {
            return Ok(resp);
        }
        last = Some(resp);
    }
    Ok(last.expect("a paced step sends at least two keys"))
}

/// The keys a paced `type` of `text` sends: one per character (Unicode
/// scalar value). A line break written `\r\n` stays one key.
fn paced_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for ch in text.chars() {
        match keys.last_mut() {
            Some(last) if last == "\r" && ch == '\n' => last.push(ch),
            _ => keys.push(ch.to_string()),
        }
    }
    keys
}

/// How a step PNG reaches `--screenshot-dir`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShotTransport {
    /// The host sends the bytes; the client writes the file.
    Inline,
    /// The host writes the file under its own screenshot directory.
    HostFile,
}

fn capture_screenshot(
    client: &mut AgentClient,
    capture: Option<&ScreenshotCapture>,
    transport: ShotTransport,
    index: u32,
    step: &PlannedStep,
) -> Option<ScreenshotReceipt> {
    let capture = capture?;
    if !capture.wants(step) {
        return None;
    }
    let path = capture.step_path(index, &step.id);
    let path_str = path.to_string_lossy().into_owned();
    if transport == ShotTransport::Inline {
        let written = client
            .screenshot_bytes(gpui_agent::ScreenshotMode::Viewport, None, None)
            // Through a temp file and a rename: a write that fails leaves an
            // earlier capture at this path as it was.
            .and_then(|png| {
                gpui_agent::atomic_write_png(&path, &png)
                    .map_err(|err| format!("write {path_str}: {err}"))
            });
        return Some(ScreenshotReceipt {
            path: path_str,
            ok: written.is_ok(),
            error: written.err(),
        });
    }
    let wire_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("step.png")
        .to_string();
    match client.rpc(Op::Screenshot {
        path: Some(wire_name),
        mode: gpui_agent::ScreenshotMode::Viewport,
        target: None,
        max_height_px: None,
        inline: false,
    }) {
        Ok(resp) if resp.ok => Some(ScreenshotReceipt {
            // The host wrote the file, so report where it says it is.
            path: resp
                .result
                .as_ref()
                .and_then(|result| result.get("path"))
                .and_then(serde_json::Value::as_str)
                .map_or(path_str, str::to_string),
            ok: true,
            error: None,
        }),
        Ok(resp) => Some(ScreenshotReceipt {
            path: path_str,
            ok: false,
            error: resp.error.or_else(|| Some("screenshot failed".into())),
        }),
        Err(error) => Some(ScreenshotReceipt {
            path: path_str,
            ok: false,
            error: Some(error),
        }),
    }
}

/// Pipeline only wide, all-Read waves when no per-step PNG capture is on.
/// Empty or unknown effects are treated as Write (fail closed).
fn wave_can_pipeline(steps: &[&PlannedStep], screenshots: Option<&ScreenshotCapture>) -> bool {
    screenshots.is_none() && steps.len() > 1 && steps.iter().all(|step| step_is_read_only(step))
}

fn step_is_read_only(step: &PlannedStep) -> bool {
    !step.effects.is_empty()
        && step
            .effects
            .iter()
            .all(|effect| matches!(effect, Effect::Read))
}

fn sanitize_step_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { "step".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_ids() {
        assert_eq!(sanitize_step_id("wait"), "wait");
        assert_eq!(sanitize_step_id("add-1"), "add-1");
        assert_eq!(sanitize_step_id("weird/id"), "weird_id");
        assert_eq!(sanitize_step_id(""), "step");
    }

    fn read_step(id: &str) -> PlannedStep {
        PlannedStep {
            id: id.into(),
            op: Op::Hello,
            needs: Vec::new(),
            screenshot: false,
            key_delay_ms: None,
            effects: vec![Effect::Read],
            schema: Some("hello".into()),
            idempotent: true,
        }
    }

    fn write_step(id: &str) -> PlannedStep {
        PlannedStep {
            id: id.into(),
            op: Op::click("todo-add"),
            needs: Vec::new(),
            screenshot: false,
            key_delay_ms: None,
            effects: vec![Effect::Write],
            schema: Some("click".into()),
            idempotent: false,
        }
    }

    #[test]
    fn paced_keys_are_one_per_character() {
        assert_eq!(
            paced_keys("1,234.56"),
            ["1", ",", "2", "3", "4", ".", "5", "6"]
        );
        assert_eq!(paced_keys("ñé"), ["ñ", "é"]);
        assert_eq!(paced_keys("a\r\nb"), ["a", "\r\n", "b"]);
        assert_eq!(paced_keys(""), Vec::<String>::new());
    }

    #[test]
    fn pipeline_only_wide_read_waves_without_screenshots() {
        let a = read_step("a");
        let b = read_step("b");
        let click = write_step("click");
        let unknown = PlannedStep {
            id: "unk".into(),
            op: Op::Hello,
            needs: Vec::new(),
            screenshot: false,
            key_delay_ms: None,
            effects: Vec::new(),
            schema: None,
            idempotent: false,
        };
        let capture = ScreenshotCapture {
            dir: PathBuf::from("/tmp"),
            flagged_only: false,
        };
        assert!(wave_can_pipeline(&[&a, &b], None));
        assert!(!wave_can_pipeline(&[&a], None));
        assert!(!wave_can_pipeline(&[&a, &click], None));
        assert!(!wave_can_pipeline(&[&a, &unknown], None));
        assert!(!wave_can_pipeline(&[&a, &b], Some(&capture)));
    }
}
