//! `gpui-agent run --goal "<text>"` (feature `instinct`, PRD M6.3):
//! the local observe → space → policy → gate → dispatch → diff loop in
//! `gpui-agent-policy::runner`. Prints a deterministic receipt; exit
//! codes 0 done, 2 abstained/blocked/cap, 1 error.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::DeliveryMode;
use gpui_agent_policy::{
    CommandEscalation, EscalatingPolicy, InstinctPolicy, OfferedInvoke, Profile, RunReceipt,
    Runner, UiPolicy,
};
use gpui_agent_recipe::{invoke_args, registry_from_schema_paths};
use serde_json::Value;

/// Options for `gpui-agent run` (M6 + M7/M8 escalation knobs).
pub struct RunOpts<'a> {
    pub goal: &'a str,
    pub profile: &'a str,
    pub json_out: bool,
    pub escalate: Option<&'a str>,
    pub escalate_cmd: &'a [String],
    pub escalate_timeout_ms: u64,
    pub schema: &'a [PathBuf],
}

pub fn run(mut client: AgentClient, opts: RunOpts<'_>) -> Result<()> {
    let RunOpts {
        goal,
        profile,
        json_out,
        escalate,
        escalate_cmd,
        escalate_timeout_ms,
        schema,
    } = opts;
    let profile = match profile {
        "fast" => Profile::Fast,
        "standard" => Profile::Standard,
        "deep" => Profile::Deep,
        other => return Err(anyhow!("unknown --profile {other} (fast|standard|deep)")),
    };
    let registry = if schema.is_empty() {
        None
    } else {
        Some(registry_from_schema_paths(schema).map_err(|e| anyhow!("{e}"))?)
    };
    let invokes: Vec<OfferedInvoke> = registry
        .as_ref()
        .map(crate::instinct_wiring::offered_invokes)
        .unwrap_or_default();
    // Invoke args resolve against the same registry, from the clause text.
    let mut args_for = move |name: &str, clause: &str| -> Result<Value, String> {
        let registry = registry
            .as_ref()
            .ok_or_else(|| format!("invoke `{name}` needs --schema"))?;
        let s = registry
            .get(name)
            .ok_or_else(|| format!("invoke `{name}` not in schema"))?;
        invoke_args(s, clause)
    };
    // M7: `--escalate-cmd` wraps the local policy in an abstain-only
    // fallback and enables the `kind:"text"` tier on deterministic
    // Missing. M8: `--escalate jev` substitutes the System One
    // fallback (no text tier). Without either the run is exactly the
    // M6 local loop.
    let escalation = if escalate_cmd.is_empty() {
        None
    } else {
        Some(
            CommandEscalation::new(escalate_cmd)
                .map_err(anyhow::Error::msg)?
                .with_timeout(std::time::Duration::from_millis(escalate_timeout_ms)),
        )
    };
    let policy: Box<dyn UiPolicy> = match (escalate, escalation.clone()) {
        (Some(engine), _) => jev_policy(profile, engine)?,
        (None, Some(esc)) => Box::new(EscalatingPolicy::new(
            InstinctPolicy::new(profile),
            Some(esc),
        )),
        (None, None) => Box::new(InstinctPolicy::new(profile)),
    };
    let receipt = Runner::new(
        &mut client,
        policy,
        DeliveryMode::Semantic,
        invokes,
        &mut args_for,
    )
    .with_text_escalation(escalation)
    .run(goal)
    .map_err(|e| anyhow!("{e}"))?;
    let code = receipt.outcome_kind.exit_code();
    print_receipt(&receipt, json_out)?;
    std::process::exit(code.into());
}

/// `--escalate <engine>` → the named fallback. `jev` needs feature
/// `jev` and TYPESAFE_API_KEY — a missing key is a config error,
/// never a silent disable.
#[cfg(feature = "jev")]
fn jev_policy(profile: Profile, engine: &str) -> Result<Box<dyn UiPolicy>> {
    match engine {
        "jev" => Ok(Box::new(EscalatingPolicy::new(
            InstinctPolicy::new(profile),
            Some(gpui_agent_policy::JevEscalation::from_env().map_err(anyhow::Error::msg)?),
        ))),
        other => Err(anyhow!("unknown --escalate engine {other}")),
    }
}

#[cfg(not(feature = "jev"))]
fn jev_policy(_profile: Profile, engine: &str) -> Result<Box<dyn UiPolicy>> {
    Err(anyhow!(
        "--escalate {engine} requires this binary to be built with feature `jev`"
    ))
}

fn print_receipt(receipt: &RunReceipt, json_out: bool) -> Result<()> {
    if json_out {
        println!("{}", serde_json::to_string_pretty(receipt)?);
        return Ok(());
    }
    println!("goal: {}", receipt.goal);
    println!(
        "outcome: {}{}",
        receipt.outcome,
        receipt
            .reason
            .as_deref()
            .map(|r| format!(" — {r}"))
            .unwrap_or_default()
    );
    for step in &receipt.steps {
        println!(
            "  step {} [{}] {} {} → {} (confidence {}‰)",
            step.step, step.clause, step.op, step.id, step.verification, step.confidence_millis,
        );
    }
    println!("final_rev: {:?}", receipt.final_rev);
    println!(
        "engine: gpui-agent-policy {} / instinct {}",
        receipt.engine.policy, receipt.engine.instinct
    );
    Ok(())
}
