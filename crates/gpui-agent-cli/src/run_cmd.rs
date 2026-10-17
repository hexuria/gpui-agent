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
use gpui_agent_recipe::{Effect, SchemaKind, invoke_args, registry_from_schema_paths};
use serde_json::{Value, json};

pub fn run(
    mut client: AgentClient,
    goal: &str,
    profile: &str,
    json_out: bool,
    escalate_cmd: &[String],
    escalate_timeout_ms: u64,
    schema: &[PathBuf],
) -> Result<()> {
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
        .map(|r| {
            r.iter()
                .filter(|s| s.kind == SchemaKind::Invoke)
                .map(|s| OfferedInvoke {
                    name: s.name.clone(),
                    description: format!("{} {}", s.description, s.keywords.join(" ")),
                    dangerous: s.effects.contains(&Effect::Exit),
                })
                .collect()
        })
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
    // Missing. Without it the run is exactly the M6 local loop.
    let escalation = if escalate_cmd.is_empty() {
        None
    } else {
        Some(
            CommandEscalation::new(escalate_cmd)
                .map_err(anyhow::Error::msg)?
                .with_timeout(std::time::Duration::from_millis(escalate_timeout_ms)),
        )
    };
    let policy: Box<dyn UiPolicy> = match escalation.clone() {
        Some(esc) => Box::new(EscalatingPolicy::new(
            InstinctPolicy::new(profile),
            Some(esc),
        )),
        None => Box::new(InstinctPolicy::new(profile)),
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
    let code = exit_code_of(&receipt);
    print_receipt(&receipt, json_out)?;
    std::process::exit(code);
}

fn exit_code_of(receipt: &RunReceipt) -> i32 {
    match receipt.outcome.as_str() {
        "done" => 0,
        "blocked" | "abstained" | "cap_hit" => 2,
        _ => 1,
    }
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
    let _ = json!(receipt); // keep serde derive reachable pre-M7
    Ok(())
}
