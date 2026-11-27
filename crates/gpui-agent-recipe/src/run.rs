use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{DeliveryMode, Op, Response};
use gpui_agent::{CallFailure, DeliveryStatus, error_code_of};
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

/// The longest a paced `type` may take, its pauses together
/// (`key_delay_ms` times the keys after the first): two minutes.
/// [`crate::compile_plan`] refuses a longer one before anything is sent.
pub const MAX_PACED_TYPE_MS: u64 = 120_000;

/// The pauses of a paced `type` of `text` at `delay` ms, together.
pub(crate) fn paced_type_ms(text: &str, delay: u64) -> u64 {
    let pauses = u64::try_from(paced_keys(text).len().saturating_sub(1)).unwrap_or(u64::MAX);
    pauses.saturating_mul(delay)
}

/// Sends one sequential step: its op as one request, or a paced `type`
/// ([`PlannedStep::key_delay_ms`]) as one `type` request per character,
/// `key_delay_ms` apart, as a person types.
///
/// - **All or nothing before the first key:** a virtual step whose text
///   has a character with no keystroke (`text_keystrokes`) fails before
///   any key is sent, as one virtual `type` does.
/// - **The focus is the target's once:** the first key goes to the step's
///   target, which the host focuses. A later key goes to the box the key
///   before went to, or to the next text box after it that the focus moved
///   on to (the next cell of a code box), read from a snapshot before the
///   key: a host that focuses only an unfocused `type` target never presses
///   the box again, so the caret stays after the typed text
///   ([`next_key_target`]). Focus that goes anywhere else stops the step as
///   a partial delivery.
/// - Between that snapshot and the key the focus could still move (an
///   app's own timer): the key is not gated on a fingerprint, since the
///   value a controlled input echoes back may change in that window.
/// - **A key the host refuses stops the step.** After the first key that
///   is a partial delivery: the error says how many keys reached the app
///   and the status is `delivery_uncertain`, so a caller does not retry
///   the whole text on top of what was typed.
/// - Only the first key carries the step's fingerprint gate: each key
///   changes the node's value, and so its fingerprint.
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
    if *delivery == DeliveryMode::Virtual
        && let Err(error) = gpui_agent::text_keystrokes(text)
    {
        let code = error_code_of(&error);
        return Err(CallFailure::with_code(
            error,
            code,
            Some(DeliveryStatus::NotDelivered),
        ));
    }
    let total = keys.len();
    // Who had the focus before the first key: a host whose `type` does not
    // move the focus leaves it there, and the keys still go to the target.
    let before = focus_before(client)?;
    let mut aim = Aim {
        target,
        before: before.as_deref(),
        to: target.clone(),
        focus: Focus::NotYet,
    };
    let mut after = After::Text;
    let mut last = None;
    let mut due = Instant::now();
    for (index, key) in keys.into_iter().enumerate() {
        if index > 0 {
            follow(client, &mut aim, after, index, total)?;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
        }
        due = Instant::now() + Duration::from_millis(delay);
        after = After::key(&key);
        let op = Op::Type {
            target: aim.to.clone(),
            text: key,
            delivery: *delivery,
            expect_fingerprint: if index == 0 {
                *expect_fingerprint
            } else {
                None
            },
        };
        match client.call_response(&op) {
            Ok(resp) if resp.ok => last = Some(resp),
            Ok(mut resp) => {
                if index > 0 {
                    let error = resp.error.take().unwrap_or_else(|| "request failed".into());
                    resp.error = Some(partial(index, total, &error));
                    resp.delivery_status = Some(DeliveryStatus::DeliveryUncertain);
                }
                return Ok(resp);
            }
            Err(mut failure) => {
                if index > 0 {
                    failure.error = partial(index, total, &failure.error);
                    failure.delivery_status = Some(DeliveryStatus::DeliveryUncertain);
                }
                return Err(failure);
            }
        }
    }
    Ok(last.expect("a paced step sends at least two keys"))
}

/// The error of a paced `type` that stopped at key `index` of `total`.
fn partial(index: usize, total: usize, error: &str) -> String {
    format!(
        "paced type stopped at key {} of {total}: the first {index} reached the app \
         (snapshot before retrying): {error}",
        index + 1
    )
}

/// Whether the keys of a paced `type` have been on the target's focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// The focus has not been on the target or the box after it yet.
    NotYet,
    /// It has: a key now follows it.
    Followed,
}

/// Where the keys of a paced `type` aimed at `target` go.
#[derive(Debug)]
struct Aim<'a> {
    target: &'a str,
    /// Who had the focus before the first key.
    before: Option<&'a str>,
    /// The node the key before went to.
    to: String,
    focus: Focus,
}

/// The key before, as far as the focus goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// A character: the focus may move on to the next cell of a code box.
    Text,
    /// A line break, which may submit a form: the focus must stay.
    LineBreak,
    /// A tab, which moves to the next field: the focus must stay.
    Tab,
}

impl After {
    fn key(key: &str) -> Self {
        match key {
            "\n" | "\r" | "\r\n" => Self::LineBreak,
            "\t" => Self::Tab,
            _ => Self::Text,
        }
    }
}

/// Who has the focus before a paced `type`'s first key.
fn focus_before(client: &mut AgentClient) -> Result<Option<String>, CallFailure> {
    match client.snapshot() {
        Ok(resp) => Ok(resp.tree.as_ref().and_then(|tree| focused_in(&tree.nodes))),
        Err(error) => {
            let code = error_code_of(&error);
            Err(CallFailure::with_code(
                format!("paced type: no snapshot before the first key: {error}"),
                code,
                Some(DeliveryStatus::NotDelivered),
            ))
        }
    }
}

/// Aims key `index` of `total` ([`next_key_target`]), from a snapshot; a
/// partial delivery when there is none or the focus left.
fn follow(
    client: &mut AgentClient,
    aim: &mut Aim<'_>,
    after: After,
    index: usize,
    total: usize,
) -> Result<(), CallFailure> {
    let tree = match client.snapshot() {
        Ok(resp) => resp.tree.unwrap_or_else(empty_tree),
        Err(error) => {
            let code = error_code_of(&error);
            return Err(CallFailure::with_code(
                partial(
                    index,
                    total,
                    &format!("no snapshot to follow the focus: {error}"),
                ),
                code,
                Some(DeliveryStatus::DeliveryUncertain),
            ));
        }
    };
    match next_key_target(&tree.nodes, aim, after) {
        Ok((next, now)) => {
            aim.to = next;
            if now == Focus::Followed {
                aim.focus = Focus::Followed;
            }
            Ok(())
        }
        Err(why) => Err(CallFailure::with_code(
            partial(index, total, &why),
            gpui_agent::error_code::HOST_ERROR,
            Some(DeliveryStatus::DeliveryUncertain),
        )),
    }
}

/// Where the next key of a paced `type` goes, from the tree after the key
/// before it (`after`): `(id, Followed when it follows the focus)`.
///
/// - The node the key before went to (`aim.to`) has the focus: that node.
/// - The focus moved on to the next cell after `aim.to` ([`next_cell`]),
///   after a key that was not a line break or a tab: that cell (the next
///   cell of a code box).
/// - The focus has not moved since before the first key, and never went
///   to the target or the cell after it (a host whose `type` does not move
///   the focus): the target.
/// - No node has the focus and it never was on the target or the cell
///   after it (a host that reports no focus): the target.
/// - Anything else (an autocomplete or a dialog took the focus, a full box
///   moved it to the next field of a screen, a line break or a tab moved
///   it, it went back or past the next cell): `Err`, naming where the
///   focus went.
fn next_key_target(
    nodes: &[gpui_agent::UiNode],
    aim: &Aim<'_>,
    after: After,
) -> Result<(String, Focus), String> {
    let focused = focused_in(nodes);
    let not_yet = aim.focus == Focus::NotYet;
    let target = aim.target;
    match focused {
        Some(id) if id == aim.to => Ok((id, Focus::Followed)),
        Some(id) if not_yet && aim.before == Some(id.as_str()) => {
            Ok((aim.to.clone(), Focus::NotYet))
        }
        Some(id)
            if after == After::Text && next_cell(nodes, &aim.to).is_some_and(|next| next == id) =>
        {
            Ok((id, Focus::Followed))
        }
        None if not_yet => Ok((aim.to.clone(), Focus::NotYet)),
        Some(id) => Err(format!(
            "the focus left `{target}` for `{id}`{}",
            match after {
                After::Text => "",
                After::LineBreak => " after a line break",
                After::Tab => " after a tab",
            }
        )),
        None => Err(format!("the focus left `{target}` and no node has it")),
    }
}

/// The next cell after `to`: the first enabled text box after it among its
/// parent's children, in document order. None when `to` is at the root of
/// the tree or directly in a window, a page or a dialog: a screen whose
/// fields sit side by side is not a code box.
fn next_cell(nodes: &[gpui_agent::UiNode], to: &str) -> Option<String> {
    fn parent_of<'a>(nodes: &'a [gpui_agent::UiNode], to: &str) -> Option<&'a gpui_agent::UiNode> {
        nodes.iter().find_map(|node| {
            if node.children.iter().any(|child| child.id == to) {
                Some(node)
            } else {
                parent_of(&node.children, to)
            }
        })
    }
    use gpui_agent::tree::role;
    let parent = parent_of(nodes, to)?;
    if matches!(
        parent.role.as_str(),
        role::WINDOW | role::PAGE | role::DIALOG | "screen" | "root"
    ) {
        return None;
    }
    parent
        .children
        .iter()
        .skip_while(|child| child.id != to)
        .skip(1)
        .find(|child| child.enabled && matches!(child.role.as_str(), role::TEXTBOX | "textfield"))
        .map(|child| child.id.clone())
}

/// A tree with no nodes (a snapshot reply without one).
fn empty_tree() -> gpui_agent::UiTree {
    gpui_agent::UiTree {
        app: String::new(),
        platform: gpui_agent::PlatformKind::Headless,
        ready: false,
        nodes: Vec::new(),
        rev: None,
    }
}

/// The innermost focused node of `nodes`, depth first.
fn focused_in(nodes: &[gpui_agent::UiNode]) -> Option<String> {
    nodes.iter().find_map(|node| {
        focused_in(&node.children).or_else(|| node.focused.then(|| node.id.clone()))
    })
}

/// The keys a paced `type` of `text` sends: one per character (Unicode
/// scalar value, not grapheme: a combining mark or a joined emoji goes as
/// its parts). A line break written `\r\n` stays one key.
pub(crate) fn paced_keys(text: &str) -> Vec<String> {
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

    fn boxes(focused: Option<&str>) -> Vec<gpui_agent::UiNode> {
        let input = |id: &str| {
            let mut node = gpui_agent::UiNode::textbox(id, id);
            node.focused = focused == Some(id);
            node
        };
        let mut code = gpui_agent::UiNode::new("code", "group", "Code");
        code.children = vec![input("cell-0"), input("cell-1"), input("cell-2")];
        let mut form = gpui_agent::UiNode::new("form", "group", "Form");
        form.children = vec![input("amount"), code];
        // A screen of fields side by side, directly in its window.
        let mut window = gpui_agent::UiNode::window("window", "Window");
        window.children = vec![input("name"), input("tin")];
        vec![form, input("elsewhere"), window]
    }

    /// Where the next key goes: `focused` in [`boxes`], after keys aimed
    /// at `target` whose last went to `to`.
    fn next(
        focused: Option<&str>,
        (target, to): (&str, &str),
        before: Option<&str>,
        focus: Focus,
        after: After,
    ) -> Result<(String, bool), String> {
        let aim = Aim {
            target,
            before,
            to: to.into(),
            focus,
        };
        next_key_target(&boxes(focused), &aim, after).map(|(id, now)| (id, now == Focus::Followed))
    }

    #[test]
    fn the_next_key_follows_the_focus_only_inside_the_targets_group() {
        use After::Text;
        use Focus::{Followed, NotYet};
        assert_eq!(
            next(Some("cell-1"), ("cell-0", "cell-0"), None, NotYet, Text),
            Ok(("cell-1".into(), true))
        );
        assert_eq!(
            next(Some("cell-2"), ("cell-0", "cell-1"), None, Followed, Text),
            Ok(("cell-2".into(), true)),
            "on from the cell the key before went to"
        );
        assert_eq!(
            next(Some("amount"), ("amount", "amount"), None, NotYet, Text),
            Ok(("amount".into(), true))
        );
        // A host whose type does not move the focus: the keys stay on the target.
        assert_eq!(
            next(
                Some("elsewhere"),
                ("amount", "amount"),
                Some("elsewhere"),
                NotYet,
                Text
            ),
            Ok(("amount".into(), false))
        );
        assert_eq!(
            next(None, ("amount", "amount"), None, NotYet, Text),
            Ok(("amount".into(), false))
        );
        // Anywhere else, or gone after the focus was on the target.
        assert!(
            next(Some("elsewhere"), ("amount", "amount"), None, NotYet, Text)
                .unwrap_err()
                .contains("`elsewhere`")
        );
        assert!(
            next(Some("cell-0"), ("amount", "amount"), None, Followed, Text).is_err(),
            "not the cell after amount"
        );
        assert!(
            next(None, ("cell-0", "cell-0"), None, Followed, Text)
                .unwrap_err()
                .contains("no node has it")
        );
    }

    #[test]
    fn the_focus_is_followed_only_to_the_next_cell_after_the_last_key() {
        use After::{LineBreak, Tab, Text};
        use Focus::{Followed, NotYet};
        // Past the next cell, or back to one already typed into.
        assert!(next(Some("cell-2"), ("cell-0", "cell-0"), None, Followed, Text).is_err());
        assert!(next(Some("cell-0"), ("cell-0", "cell-1"), None, Followed, Text).is_err());
        // G2: a host whose type does not move the focus, which sat on a
        // sibling: the keys do not follow that sibling's own move.
        assert!(
            next(
                Some("cell-2"),
                ("cell-0", "cell-0"),
                Some("cell-1"),
                NotYet,
                Text
            )
            .unwrap_err()
            .contains("for `cell-2`")
        );
        // Fields side by side directly in a window are not a code box.
        assert!(
            next(Some("tin"), ("name", "name"), None, Followed, Text)
                .unwrap_err()
                .contains("the focus left `name` for `tin`")
        );
        // A line break or a tab that moved the focus stops the keys, even
        // to the next cell; one that left it in the box does not.
        assert!(
            next(
                Some("cell-1"),
                ("cell-0", "cell-0"),
                None,
                Followed,
                LineBreak
            )
            .unwrap_err()
            .contains("after a line break")
        );
        assert!(
            next(Some("cell-1"), ("cell-0", "cell-0"), None, Followed, Tab)
                .unwrap_err()
                .contains("after a tab")
        );
        assert_eq!(
            next(
                Some("amount"),
                ("amount", "amount"),
                None,
                Followed,
                LineBreak
            ),
            Ok(("amount".into(), true))
        );
        assert_eq!(After::key("\r\n"), LineBreak);
        assert_eq!(After::key("\t"), Tab);
        assert_eq!(After::key("1"), Text);
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
