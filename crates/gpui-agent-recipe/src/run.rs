use std::path::PathBuf;
use std::time::Instant;

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::Op;
use thiserror::Error;

use crate::plan::{Plan, PlannedStep};
use crate::receipt::{Receipt, ScreenshotReceipt, StepReceipt};

#[derive(Debug, Error)]
pub enum RunError {
    #[error("recipe has shutdown/exit effects; pass --yes to run it")]
    NeedsYes,
    #[error("step `{id}` failed: {error}")]
    Step {
        id: String,
        error: String,
        receipt: Box<Receipt>,
    },
}

/// After selected recipe steps, ask the host to write an app-surface PNG.
///
/// Default (dir set): every step. `flagged_only`: only steps with
/// `screenshot: true` in JSON / `--screenshot` in wants.
#[derive(Debug, Clone)]
pub struct ScreenshotCapture {
    pub dir: PathBuf,
    pub flagged_only: bool,
}

impl ScreenshotCapture {
    pub fn step_path(&self, index: u32, step_id: &str) -> PathBuf {
        self.dir
            .join(format!("{index:03}-{}.png", sanitize_step_id(step_id)))
    }

    fn wants(&self, step: &PlannedStep) -> bool {
        !self.flagged_only || step.screenshot
    }
}

/// Execute a compiled plan on one reused TCP session.
///
/// Each step is still a normal protocol request (token, version, caps).
/// Waves are scheduled for documentation; P1 runs them **sequentially**
/// and stops on the first failed step (no next sibling, no next wave).
/// The recipe layer never talks to a shell and never skips `authorize_request`.
pub fn run_plan(client: &mut AgentClient, plan: &Plan, yes: bool) -> Result<Receipt, RunError> {
    run_plan_with_screenshots(client, plan, yes, None)
}

/// [`run_plan`] plus optional per-step app-surface PNGs.
///
/// Screenshot unavailability is recorded on the receipt and does **not**
/// fail the recipe. No fake PNG is written.
pub fn run_plan_with_screenshots(
    client: &mut AgentClient,
    plan: &Plan,
    yes: bool,
    screenshots: Option<&ScreenshotCapture>,
) -> Result<Receipt, RunError> {
    if plan.requires_yes && !yes {
        return Err(RunError::NeedsYes);
    }

    if let Some(capture) = screenshots {
        let _ = std::fs::create_dir_all(&capture.dir);
    }

    let started = Instant::now();
    let mut receipt = Receipt {
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
        for id in wave {
            let step = plan
                .steps
                .iter()
                .find(|step| step.id == *id)
                .expect("wave id is a planned step");
            let step_started = Instant::now();
            shot_index += 1;
            match client.rpc_op(&step.op) {
                Ok(resp) => {
                    receipt.session_reused = client.has_session();
                    let ok = resp.ok;
                    let error = resp.error.clone();
                    let screenshot = capture_screenshot(client, screenshots, shot_index, step);
                    if let Some(shot) = screenshot.clone() {
                        receipt.screenshots.push(shot);
                    }
                    receipt.steps.push(StepReceipt {
                        id: step.id.clone(),
                        ok,
                        error: error.clone(),
                        elapsed_ms: step_started.elapsed().as_millis() as u64,
                        result: resp.result,
                        screenshot,
                    });
                    if !ok {
                        receipt.ok = false;
                        receipt.elapsed_ms = started.elapsed().as_millis() as u64;
                        return Err(RunError::Step {
                            id: step.id.clone(),
                            error: error.unwrap_or_else(|| "request failed".into()),
                            receipt: Box::new(receipt),
                        });
                    }
                }
                Err(error) => {
                    receipt.ok = false;
                    receipt.session_reused = client.has_session();
                    let screenshot = capture_screenshot(client, screenshots, shot_index, step);
                    if let Some(shot) = screenshot.clone() {
                        receipt.screenshots.push(shot);
                    }
                    receipt.steps.push(StepReceipt {
                        id: step.id.clone(),
                        ok: false,
                        error: Some(error.clone()),
                        elapsed_ms: step_started.elapsed().as_millis() as u64,
                        result: None,
                        screenshot,
                    });
                    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
                    return Err(RunError::Step {
                        id: step.id.clone(),
                        error,
                        receipt: Box::new(receipt),
                    });
                }
            }
        }
    }

    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
    receipt.session_reused = client.has_session();
    Ok(receipt)
}

fn capture_screenshot(
    client: &mut AgentClient,
    capture: Option<&ScreenshotCapture>,
    index: u32,
    step: &PlannedStep,
) -> Option<ScreenshotReceipt> {
    let capture = capture?;
    if !capture.wants(step) {
        return None;
    }
    let path = capture.step_path(index, &step.id);
    let path_str = path.to_string_lossy().into_owned();
    match client.rpc(Op::Screenshot {
        path: Some(path_str.clone()),
    }) {
        Ok(resp) if resp.ok => Some(ScreenshotReceipt {
            path: path_str,
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
}
