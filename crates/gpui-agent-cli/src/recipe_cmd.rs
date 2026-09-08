use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use gpui_agent::client::AgentClient;
use gpui_agent_recipe::{
    RunError, compile_plan, order_check, parse_recipe, resolve_intent, run_plan, todo_registry,
    validate_recipe,
};

#[derive(Debug, Subcommand)]
pub enum RecipeCommand {
    /// Lint a recipe (JSON or line-based wants). No host required.
    Validate {
        /// Recipe path (`.json` or `.wants`). `-` reads stdin.
        path: PathBuf,
    },
    /// Compile to a dependency DAG / wave plan. No host required.
    Plan {
        path: PathBuf,
        /// Bind `$params` (`title=Buy milk`). Repeatable.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Compile twice (listed order vs reversed) and diff waves.
        #[arg(long)]
        order_check: bool,
    },
    /// Run the compiled plan on one reused TCP session.
    Run {
        path: PathBuf,
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Required when the plan includes `shutdown` / exit effects.
        #[arg(long)]
        yes: bool,
        /// Write the receipt JSON to this path.
        #[arg(long)]
        receipt_out: Option<PathBuf>,
    },
    /// Map a natural-language intent through the local schema registry.
    Resolve { intent: String },
}

pub fn run(client: Option<AgentClient>, command: RecipeCommand) -> Result<()> {
    let registry = todo_registry();
    match command {
        RecipeCommand::Validate { path } => {
            let recipe = parse_recipe(&path).map_err(|err| anyhow!("{err}"))?;
            validate_recipe(&recipe, &registry).map_err(|err| anyhow!("{err}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "name": recipe.name,
                    "steps": recipe.steps.len(),
                    "params": recipe.params,
                    "explicit_needs": recipe.uses_explicit_needs(),
                }))?
            );
            Ok(())
        }
        RecipeCommand::Plan {
            path,
            set,
            order_check: do_order,
        } => {
            let recipe = parse_recipe(&path).map_err(|err| anyhow!("{err}"))?;
            let set = parse_set(&set)?;
            if do_order {
                let check =
                    order_check(&recipe, &set, &registry).map_err(|err| anyhow!("{err}"))?;
                println!("{}", serde_json::to_string_pretty(&check)?);
            } else {
                let plan =
                    compile_plan(&recipe, &set, &registry).map_err(|err| anyhow!("{err}"))?;
                println!("{}", serde_json::to_string_pretty(&plan)?);
            }
            Ok(())
        }
        RecipeCommand::Run {
            path,
            set,
            yes,
            receipt_out,
        } => {
            let mut client = client.context("recipe run needs a client")?;
            let recipe = parse_recipe(&path).map_err(|err| anyhow!("{err}"))?;
            let set = parse_set(&set)?;
            let plan = compile_plan(&recipe, &set, &registry).map_err(|err| anyhow!("{err}"))?;
            let receipt = match run_plan(&mut client, &plan, yes) {
                Ok(receipt) => receipt,
                Err(RunError::Step { receipt, error, .. }) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    return Err(anyhow!("{error}"));
                }
                Err(err) => return Err(anyhow!("{err}")),
            };
            print_receipt(&receipt, receipt_out.as_ref())?;
            Ok(())
        }
        RecipeCommand::Resolve { intent } => {
            let result = resolve_intent(&intent, &registry).map_err(|err| anyhow!("{err}"))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

fn parse_set(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .with_context(|| format!("expected KEY=VALUE, got {pair}"))?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

fn print_receipt(receipt: &gpui_agent_recipe::Receipt, path: Option<&PathBuf>) -> Result<()> {
    let json = serde_json::to_string_pretty(receipt)?;
    println!("{json}");
    if let Some(path) = path {
        std::fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}
