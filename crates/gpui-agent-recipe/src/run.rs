use std::time::Instant;

use gpui_agent::client::AgentClient;
use thiserror::Error;

use crate::plan::Plan;
use crate::receipt::{Receipt, StepReceipt};

#[derive(Debug, Error)]
pub enum RunError {
    #[error("{0}")]
    Plan(String),
    #[error("recipe has shutdown/exit effects; pass --yes to run it")]
    NeedsYes,
    #[error("step `{id}` failed: {error}")]
    Step {
        id: String,
        error: String,
        receipt: Receipt,
    },
}

/// Execute a compiled plan on one reused TCP session.
///
/// Each step is still a normal protocol request (token, version, caps).
/// The recipe layer never talks to a shell and never skips `authorize_request`.
pub fn run_plan(client: &mut AgentClient, plan: &Plan, yes: bool) -> Result<Receipt, RunError> {
    if plan.requires_yes && !yes {
        return Err(RunError::NeedsYes);
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
                if !ok {
                    receipt.ok = false;
                    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
                    return Err(RunError::Step {
                        id: step.id.clone(),
                        error: error.unwrap_or_else(|| "request failed".into()),
                        receipt,
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
                receipt.elapsed_ms = started.elapsed().as_millis() as u64;
                return Err(RunError::Step {
                    id: step.id.clone(),
                    error,
                    receipt,
                });
            }
        }
    }

    receipt.elapsed_ms = started.elapsed().as_millis() as u64;
    receipt.session_reused = client.has_session();
    Ok(receipt)
}
