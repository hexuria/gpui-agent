use std::time::Instant;

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::Op;
use thiserror::Error;

use crate::plan::Plan;
use crate::receipt::{Receipt, StepReceipt};
use crate::record::RecipeRecorder;

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

/// Execute a compiled plan on one reused TCP session.
///
/// Each step is still a normal protocol request (token, version, caps).
/// The recipe layer never talks to a shell and never skips `authorize_request`.
pub fn run_plan(client: &mut AgentClient, plan: &Plan, yes: bool) -> Result<Receipt, RunError> {
    run_plan_with_recorder(client, plan, yes, None)
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
    mut recorder: Option<&mut dyn RecipeRecorder>,
) -> Result<Receipt, RunError> {
    if plan.requires_yes && !yes {
        return Err(RunError::NeedsYes);
    }

    if let Some(rec) = recorder.as_mut() {
        let _ = rec.on_start(plan);
    }

    let started = Instant::now();
    let mut receipt = Receipt {
        ok: true,
        recipe: plan.name.clone(),
        fingerprint: plan.fingerprint.clone(),
        session_reused: false,
        steps: Vec::with_capacity(plan.steps.len()),
        elapsed_ms: 0,
    };

    let mut frame_index = 0u32;
    capture_frame(client, &mut recorder, frame_index, "_start", true);
    frame_index += 1;

    for step in &plan.steps {
        let step_started = Instant::now();
        match client.rpc(step.op.clone()) {
            Ok(resp) => {
                receipt.session_reused = client.has_session();
                let ok = resp.ok;
                let error = resp.error.clone();
                receipt.steps.push(StepReceipt {
                    id: step.id.clone(),
                    ok,
                    error: error.clone(),
                    elapsed_ms: step_started.elapsed().as_millis() as u64,
                    result: resp.result,
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
                receipt.steps.push(StepReceipt {
                    id: step.id.clone(),
                    ok: false,
                    error: Some(error.clone()),
                    elapsed_ms: step_started.elapsed().as_millis() as u64,
                    result: None,
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

    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
    receipt.session_reused = client.has_session();
    Ok(receipt)
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
