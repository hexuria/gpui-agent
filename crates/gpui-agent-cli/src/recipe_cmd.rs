use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use gpui_agent::client::AgentClient;
use gpui_agent_recipe::{
    RunError, ScreenshotCapture, compile_plan, order_check, parse_recipe, resolve_intent,
    run_plan_with_screenshots, todo_registry, validate_recipe,
};

#[derive(Debug, Subcommand)]
pub enum RecipeCommand {
    /// [experimental] Lint a JSON recipe (`.wants` also accepted). No host required.
    Validate {
        /// Recipe path (`.json` canonical; `.wants` also accepted). `-` reads stdin.
        path: PathBuf,
    },
    /// [experimental] Compile a JSON recipe to a dependency DAG / wave plan. No host required.
    Plan {
        path: PathBuf,
        /// Bind `$params` (`title=Buy milk`). Repeatable.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Compile twice (listed order vs reversed) and diff waves.
        #[arg(long)]
        order_check: bool,
    },
    /// [experimental] Run the compiled plan sequentially on one reused TCP session.
    /// Requires a non-empty `--token` / `GPUI_AGENT_TOKEN` (same value as the host).
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
        /// After steps, ask the host for an app-surface PNG (not the desktop).
        /// Headless / Linux / Windows list `screenshot_unavailable` (no fake file).
        /// macOS desktop `todo` writes this window via `screencapture -l`.
        #[arg(long, value_name = "DIR")]
        screenshot_dir: Option<PathBuf>,
        /// With `--screenshot-dir`, only steps marked `screenshot: true` / `--screenshot`.
        #[arg(long)]
        screenshot_flagged: bool,
    },
    /// [experimental] Map a natural-language intent through the local schema registry.
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
            screenshot_dir,
            screenshot_flagged,
        } => {
            let mut client = client.context("recipe run needs a client")?;
            if screenshot_flagged && screenshot_dir.is_none() {
                return Err(anyhow!(
                    "--screenshot-flagged requires --screenshot-dir DIR"
                ));
            }
            let recipe = parse_recipe(&path).map_err(|err| anyhow!("{err}"))?;
            let set = parse_set(&set)?;
            let plan = compile_plan(&recipe, &set, &registry).map_err(|err| anyhow!("{err}"))?;
            let capture = screenshot_dir.as_ref().map(|dir| ScreenshotCapture {
                dir: dir.clone(),
                flagged_only: screenshot_flagged,
            });
            match run_plan_with_screenshots(&mut client, &plan, yes, capture.as_ref()) {
                Ok(receipt) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    Ok(())
                }
                Err(RunError::Step { receipt, error, .. }) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    Err(anyhow!("{error}"))
                }
                Err(err) => Err(anyhow!("{err}")),
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_requires_equals() {
        let err = parse_set(&["title".into()]).unwrap_err();
        assert!(err.to_string().contains("KEY=VALUE"), "{err}");
        let map = parse_set(&["title=Buy milk".into()]).unwrap();
        assert_eq!(map.get("title").map(String::as_str), Some("Buy milk"));
    }

    #[test]
    fn screenshot_flagged_requires_dir() {
        let client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let err = run(
            Some(client),
            RecipeCommand::Run {
                path: PathBuf::from("examples/recipes/todo-crud.json"),
                set: vec!["title=x".into()],
                yes: false,
                receipt_out: None,
                screenshot_dir: None,
                screenshot_flagged: true,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--screenshot-dir"), "{err}");
    }
}
