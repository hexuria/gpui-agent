use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use gpui_agent::client::AgentClient;
use gpui_agent_recipe::{
    RecipeRecorder, RecordBackend, RunError, ScreenshotCapture, SemanticRecorder, compile_plan,
    order_check, os_record_help, parse_recipe, resolve_intent, resolve_record_target,
    run_plan_with_extras, todo_registry, validate_recipe,
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
        /// Observe-only recording. Directory of SVG/PPM frames, or a `.mp4` path
        /// (frames go to `*.mp4.frames`; mux with ffmpeg yourself).
        #[arg(long, value_name = "PATH")]
        record: Option<PathBuf>,
        /// `semantic` (default, headless-safe) or `os` (not in-process; see docs).
        #[arg(long, value_name = "semantic|os", default_value = "semantic")]
        record_backend: String,
        /// Include snapshot `value` fields in semantic frames (still redacts `role=password`).
        #[arg(long)]
        record_values: bool,
        /// After steps, ask the host for an app-surface PNG (not the desktop).
        /// Headless lists `screenshot_unavailable` on the receipt and does not fake a file.
        #[arg(long, value_name = "DIR")]
        screenshot_dir: Option<PathBuf>,
        /// With `--screenshot-dir`, only steps marked `screenshot: true` / `--screenshot`.
        #[arg(long)]
        screenshot_flagged: bool,
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
            record,
            record_backend,
            record_values,
            screenshot_dir,
            screenshot_flagged,
        } => {
            let mut client = client.context("recipe run needs a client")?;
            let backend = RecordBackend::parse(&record_backend).map_err(|err| anyhow!("{err}"))?;
            if record.is_some() && backend == RecordBackend::Os {
                return Err(anyhow!("{}", os_record_help()));
            }
            if screenshot_flagged && screenshot_dir.is_none() {
                return Err(anyhow!(
                    "--screenshot-flagged requires --screenshot-dir DIR"
                ));
            }
            let recipe = parse_recipe(&path).map_err(|err| anyhow!("{err}"))?;
            let set = parse_set(&set)?;
            let plan = compile_plan(&recipe, &set, &registry).map_err(|err| anyhow!("{err}"))?;
            let mut recorder = if let Some(record_path) = record {
                let target = resolve_record_target(&record_path);
                Some(
                    SemanticRecorder::create(target, record_values)
                        .map_err(|err| anyhow!("{err}"))?,
                )
            } else {
                None
            };
            let capture = screenshot_dir.as_ref().map(|dir| ScreenshotCapture {
                dir: dir.clone(),
                flagged_only: screenshot_flagged,
            });
            let run_result = run_plan_with_extras(
                &mut client,
                &plan,
                yes,
                recorder.as_mut().map(|rec| rec as &mut dyn RecipeRecorder),
                capture.as_ref(),
            );
            let finish_err = if let Some(rec) = recorder.as_mut() {
                match &run_result {
                    Ok(receipt) => rec.on_finish(receipt).err(),
                    Err(RunError::Step { receipt, .. }) => rec.on_finish(receipt).err(),
                    Err(_) => rec
                        .on_finish(&gpui_agent_recipe::Receipt {
                            ok: false,
                            recipe: plan.name.clone(),
                            fingerprint: plan.fingerprint.clone(),
                            session_reused: false,
                            steps: vec![],
                            screenshots: vec![],
                            elapsed_ms: 0,
                        })
                        .err(),
                }
            } else {
                None
            };
            if let Some(rec) = recorder.as_ref() {
                eprintln!("record: semantic frames in {}", rec.dir().display());
            }
            let receipt_result = match run_result {
                Ok(receipt) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    Ok(())
                }
                Err(RunError::Step { receipt, error, .. }) => {
                    print_receipt(&receipt, receipt_out.as_ref())?;
                    Err(anyhow!("{error}"))
                }
                Err(err) => Err(anyhow!("{err}")),
            };
            if let Some(err) = finish_err {
                return Err(anyhow!("record finish: {err}"));
            }
            receipt_result
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
    fn os_record_backend_is_stubbed() {
        assert_eq!(RecordBackend::parse("os").unwrap(), RecordBackend::Os);
        let help = os_record_help();
        assert!(help.contains("record-window.sh"), "{help}");
        assert!(help.contains("semantic"), "{help}");
    }

    #[test]
    fn recipe_run_os_record_errors_before_connect() {
        let client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let err = run(
            Some(client),
            RecipeCommand::Run {
                path: PathBuf::from("examples/recipes/todo-crud.json"),
                set: vec!["title=x".into()],
                yes: false,
                receipt_out: None,
                record: Some(PathBuf::from("artifacts/x")),
                record_backend: "os".into(),
                record_values: false,
                screenshot_dir: None,
                screenshot_flagged: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("record-window.sh"), "{err}");
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
                record: None,
                record_backend: "semantic".into(),
                record_values: false,
                screenshot_dir: None,
                screenshot_flagged: true,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--screenshot-dir"), "{err}");
    }
}
