use std::path::PathBuf;
use std::time::Instant;

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::Op;
use thiserror::Error;

use crate::plan::{Plan, PlannedStep};
use crate::receipt::{Receipt, ScreenshotReceipt, StepReceipt};
use crate::record::{sanitize_step_id, RecipeRecorder};

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
/// Independent DAG waves pipeline several lines before reading when this
/// path has no screenshot/recorder extras. Because those lines are already
/// on the wire, a failed sibling does not un-run the rest of the wave; the
/// receipt includes every response, then the run stops (no next wave).
/// The recipe layer never talks to a shell and never skips `authorize_request`.
pub fn run_plan(client: &mut AgentClient, plan: &Plan, yes: bool) -> Result<Receipt, RunError> {
    run_plan_with_extras(client, plan, yes, None, None)
}

/// Like [`run_plan`], and optionally paints observe-only frames after each step.
///
/// Recording uses extra `snapshot` RPCs on the **same** session. Recorder I/O
/// errors are ignored so a full disk cannot mask a recipe result; the
/// caller should still call `on_finish` and surface that error.
pub fn run_plan_with_recorder(
    client: &mut AgentClient,
    plan: &Plan,
    yes: bool,
    recorder: Option<&mut dyn RecipeRecorder>,
) -> Result<Receipt, RunError> {
    run_plan_with_extras(client, plan, yes, recorder, None)
}

/// [`run_plan_with_recorder`] plus optional per-step app-surface PNGs.
///
/// Screenshot unavailability is recorded on the receipt and does **not**
/// fail the recipe. No fake PNG is written. When `screenshots` or
/// `recorder` is set, steps stay sequential so a PNG/frame can land
/// after each op.
pub fn run_plan_with_extras(
    client: &mut AgentClient,
    plan: &Plan,
    yes: bool,
    mut recorder: Option<&mut dyn RecipeRecorder>,
    screenshots: Option<&ScreenshotCapture>,
) -> Result<Receipt, RunError> {
    if plan.requires_yes && !yes {
        return Err(RunError::NeedsYes);
    }

    if let Some(rec) = recorder.as_mut() {
        let _ = rec.on_start(plan);
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

    let mut frame_index = 0u32;
    capture_frame(client, &mut recorder, frame_index, "_start", true);
    frame_index += 1;

    let pipeline_waves = recorder.is_none() && screenshots.is_none();
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
        if pipeline_waves && wave_steps.len() > 1 {
            let ops: Vec<&Op> = wave_steps.iter().map(|step| &step.op).collect();
            let step_started = Instant::now();
            match client.rpc_pipeline(&ops) {
                Ok(resps) => {
                    receipt.session_reused = client.has_session();
                    let elapsed = step_started.elapsed().as_millis() as u64;
                    // All lines were already written; record every sibling even
                    // if one failed so the receipt matches what the host ran.
                    let mut failed: Option<(String, String)> = None;
                    for (step, resp) in wave_steps.iter().zip(resps) {
                        shot_index += 1;
                        let ok = resp.ok;
                        let error = resp.error.clone();
                        if !ok && failed.is_none() {
                            failed = Some((
                                step.id.clone(),
                                error.clone().unwrap_or_else(|| "request failed".into()),
                            ));
                        }
                        receipt.steps.push(StepReceipt {
                            id: step.id.clone(),
                            ok,
                            error,
                            elapsed_ms: elapsed,
                            result: resp.result,
                            screenshot: None,
                        });
                    }
                    if let Some((id, error)) = failed {
                        receipt.ok = false;
                        receipt.elapsed_ms = started.elapsed().as_millis() as u64;
                        return Err(RunError::Step {
                            id,
                            error,
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
                        error: Some(error.clone()),
                        elapsed_ms: step_started.elapsed().as_millis() as u64,
                        result: None,
                        screenshot: None,
                    });
                    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
                    return Err(RunError::Step {
                        id: first.id.clone(),
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
                    capture_frame(client, &mut recorder, frame_index, &step.id, ok);
                    frame_index += 1;
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
                    capture_frame(client, &mut recorder, frame_index, &step.id, false);
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

fn capture_frame(
    client: &mut AgentClient,
    recorder: &mut Option<&mut dyn RecipeRecorder>,
    index: u32,
    step_id: &str,
    step_ok: bool,
) {
    let Some(rec) = recorder.as_mut() else {
        return;
    };
    let tree = client.rpc(Op::Snapshot).ok().and_then(|resp| resp.tree);
    let _ = rec.on_frame(index, step_id, tree.as_ref(), step_ok);
}
