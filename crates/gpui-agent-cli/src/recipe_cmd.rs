use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use gpui_agent::CallFailure;
use gpui_agent::client::AgentClient;
use gpui_agent_recipe::{
    RecipeError, RecipeErrorCode, RunError, RunOptions, ScreenshotCapture, compile_plan,
    order_check, parse_recipe, registry_from_schema_paths, resolve_intent, run_plan,
    validate_recipe,
};

#[derive(Debug, Subcommand)]
pub(crate) enum RecipeCommand {
    /// Lint a JSON recipe (`.wants` also accepted). No host required.
    Validate {
        /// Recipe path (`.json` canonical; `.wants` also accepted). `-` reads stdin.
        path: PathBuf,
    },
    /// Compile a JSON recipe to a dependency DAG / wave plan. No host required.
    Plan {
        path: PathBuf,
        /// Bind a declared `$param` (`title=Buy milk`). Repeatable; every
        /// declared param exactly once, no others.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Compile twice (listed order vs reversed) and diff waves.
        #[arg(long)]
        order_check: bool,
    },
    /// Run the compiled plan on one reused TCP session; prints the receipt JSON.
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
    /// \[experimental\] Map a natural-language intent through the local schema registry.
    Resolve { intent: String },
}

pub(crate) fn run(
    client: Option<AgentClient>,
    command: RecipeCommand,
    schema_paths: &[PathBuf],
) -> Result<()> {
    let registry = registry_from_schema_paths(schema_paths).map_err(anyhow::Error::new)?;
    match command {
        RecipeCommand::Validate { path } => {
            let recipe = parse_recipe(&path).map_err(anyhow::Error::new)?;
            validate_recipe(&recipe, &registry).map_err(anyhow::Error::new)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ok": true,
                    "name": recipe.name,
                    "steps": recipe.steps.len(),
                    "params": recipe.param_names(),
                    "input_schema": recipe.input_schema(),
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
            let recipe = parse_recipe(&path).map_err(anyhow::Error::new)?;
            let set = parse_set(&set)?;
            if do_order {
                let check = order_check(&recipe, &set, &registry).map_err(anyhow::Error::new)?;
                println!("{}", serde_json::to_string_pretty(&check)?);
            } else {
                let plan = compile_plan(&recipe, &set, &registry).map_err(anyhow::Error::new)?;
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
            // Rejected before running: typed, so `main` prints its envelope.
            if screenshot_flagged && screenshot_dir.is_none() {
                return Err(anyhow::Error::new(CallFailure::with_code(
                    "--screenshot-flagged requires --screenshot-dir DIR",
                    gpui_agent::error_code::BAD_REQUEST,
                    None,
                )));
            }
            let recipe = parse_recipe(&path).map_err(anyhow::Error::new)?;
            let set = parse_set(&set)?;
            let plan = compile_plan(&recipe, &set, &registry).map_err(anyhow::Error::new)?;
            let capture = screenshot_dir
                .as_ref()
                .map(|dir| ScreenshotCapture::new(dir).with_flagged_only(screenshot_flagged));
            let options = RunOptions::new().with_yes(yes).with_screenshots(capture);
            match run_plan(&mut client, &plan, &options) {
                Ok(receipt) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    Ok(())
                }
                // The receipt on stdout already carries the failed step's
                // code, so no second envelope is printed.
                Err(RunError::Step { receipt, error, .. }) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    Err(anyhow!("{error}"))
                }
                Err(RunError::NeedsYes) => Err(anyhow::Error::new(RecipeError::new(
                    RecipeErrorCode::Unconfirmed,
                    RunError::NeedsYes.to_string(),
                ))),
                Err(err) => Err(anyhow::Error::new(CallFailure::from_message(
                    err.to_string(),
                ))),
            }
        }
        RecipeCommand::Resolve { intent } => {
            let result = resolve_intent(&intent, &registry).map_err(anyhow::Error::new)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

fn parse_set(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(anyhow::Error::new(RecipeError::new(
                RecipeErrorCode::InvalidParam,
                format!("--set expects KEY=VALUE, got {pair}"),
            )));
        };
        if map.insert(key.to_string(), value.to_string()).is_some() {
            return Err(anyhow::Error::new(RecipeError::new(
                RecipeErrorCode::InvalidParam,
                format!("--set {key}=… is given more than once"),
            )));
        }
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
        let map = parse_set(&["q=a=b".into()]).unwrap();
        assert_eq!(map.get("q").map(String::as_str), Some("a=b"));
    }

    #[test]
    fn parse_set_rejects_repeated_key() {
        let err = parse_set(&["title=a".into(), "title=b".into()]).unwrap_err();
        let err = err.downcast_ref::<RecipeError>().expect("typed");
        assert_eq!(err.code, RecipeErrorCode::InvalidParam);
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
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("--screenshot-dir"), "{err}");
        let failure = err.downcast_ref::<CallFailure>().expect("typed");
        assert_eq!(failure.error_code, gpui_agent::error_code::BAD_REQUEST);
    }
}
