mod mcp;
mod recipe_cmd;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use gpui_agent::DEFAULT_ADDR_STR;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op};
use recipe_cmd::RecipeCommand;

/// Talk to an embedded AgentHost over the opt-in agent protocol (not CDP).
///
/// The host must implement `AgentHost`, publish stable ids, and start the
/// server under `GPUI_AGENT=1`. This CLI does not attach to an arbitrary
/// GPUI Kit process.
///
/// First-class commands are the protocol ops only. App-specific verbs
/// (`todo.add`, `nav.go`, …) belong in the host (`invoke`) or in agent
/// prompts — not as CLI subcommands.
#[derive(Parser)]
#[command(name = "gpui-agent", after_help = AFTER_HELP)]
struct Cli {
    /// Host:port of the automation server. Loopback by default; non-loopback
    /// needs `--allow-remote` and a non-empty token.
    #[arg(long, default_value = DEFAULT_ADDR_STR, env = "GPUI_AGENT_ADDR")]
    addr: SocketAddr,
    /// Shared secret; must match `GPUI_AGENT_TOKEN` on the host when the host
    /// has one. Required (non-empty) for `recipe run` and `mcp`.
    #[arg(long, env = "GPUI_AGENT_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Connect to a non-loopback host. Requires a non-empty token. Plaintext
    /// TCP — lab / trusted network only. Prefer SSH or Tailscale until TLS.
    #[arg(long, env = "GPUI_AGENT_ALLOW_REMOTE")]
    allow_remote: bool,
    #[command(subcommand)]
    command: Command,
}

const AFTER_HELP: &str = "\
App-specific helpers (for example the sample todo app) live in examples/,
not in this CLI. Navigate pages with click + assert on stable ids, or invoke
a command the host registered. Experimental: batch many ops in one process
with `recipe validate|plan|run|resolve` (JSON canonical; `.wants` also
accepted — see docs/RECIPES.md). Host bind is default-deny: set
GPUI_AGENT_TOKEN on the host. `recipe run` and `mcp` require a non-empty
GPUI_AGENT_TOKEN or --token; set the same value on the host. Non-loopback
`--addr` also needs `--allow-remote` (or GPUI_AGENT_ALLOW_REMOTE=1) and a
token so a mistyped address cannot leak the secret (see docs/SECURITY.md).";

#[derive(Debug, Subcommand)]
enum Command {
    /// Block until the host answers hello/ready.
    Wait {
        /// How long the client retries connecting (milliseconds).
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Handshake: protocol version, app name, platform, ready.
    Hello,
    /// Print the semantic UI tree as JSON.
    Snapshot {
        #[arg(long)]
        pretty: bool,
    },
    /// Observe-only PNG of the **app surface**. Host writes `--out`.
    /// Headless / Linux / Windows: `screenshot_unavailable` (no fake file).
    /// macOS desktop: PNG of **this window** via `screencapture -l`.
    Screenshot {
        /// Destination PNG on this machine so the image does not ride NDJSON.
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Activate a widget by stable id (`nav-settings`, `submit`, …).
    Click {
        target: String,
        /// `semantic` (default) calls the handler. `virtual` synthesizes GPUI events.
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
    },
    /// Append text to an editable widget.
    Type {
        target: String,
        text: String,
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
    },
    /// Replace the value of an editable widget.
    SetValue { target: String, value: String },
    /// Send a key (`Enter`, `Backspace`) to a widget.
    Key {
        target: String,
        key: String,
        #[arg(long, default_value = "semantic")]
        delivery: DeliveryMode,
    },
    /// Assert fields on a node from the current snapshot.
    Assert {
        /// Stable id of the node (protocol field: `target`).
        #[arg(long, visible_alias = "target")]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        role: Option<String>,
        /// `--checked` or `--checked true|false`. A following `true`/`false` is the value, not a positional.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        checked: Option<bool>,
        /// `--exists` or `--exists true|false` (default true).
        #[arg(long, num_args = 0..=1, default_missing_value = "true", default_value_t = true)]
        exists: bool,
        /// Invert `--exists` (node must be absent). Flag only; takes no value.
        #[arg(long)]
        absent: bool,
    },
    /// Call a named host command the app registered.
    Invoke {
        name: String,
        /// Repeatable `key=value` pairs. Values are JSON if they parse, else strings.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
    },
    /// Ask the host to exit.
    Shutdown,
    /// Tiny MCP stdio server exposing the same generic tools. Requires `--token` / `GPUI_AGENT_TOKEN`.
    Mcp,
    /// Experimental: validate / plan / run / resolve a JSON recipe of protocol ops (`.wants` also accepted). `run` requires a token.
    Recipe {
        /// App invoke/id schema JSON. Repeatable. Also `GPUI_AGENT_SCHEMA` (OS path list).
        #[arg(long = "schema", value_name = "PATH", global = true)]
        schema: Vec<std::path::PathBuf>,
        #[command(subcommand)]
        action: RecipeCommand,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn take_non_empty_token(token: Option<String>) -> Option<String> {
    token.filter(|s| !s.is_empty())
}

const RECIPE_MCP_TOKEN_REQUIRED: &str = "recipe run and mcp require a non-empty GPUI_AGENT_TOKEN or --token \
     (set the same token on the host)";

fn require_recipe_mcp_token(token: Option<&str>) -> Result<&str> {
    match token {
        Some(t) if !t.is_empty() => Ok(t),
        _ => Err(anyhow!("{RECIPE_MCP_TOKEN_REQUIRED}")),
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let token = take_non_empty_token(cli.token);
    gpui_agent::authorize_client(cli.addr, token.as_deref(), cli.allow_remote)
        .with_context(|| format!("refusing agent address {}", cli.addr))?;

    if matches!(cli.command, Command::Mcp) {
        let token = require_recipe_mcp_token(token.as_deref())?;
        return mcp::run(cli.addr, token.to_string());
    }
    if let Command::Recipe { schema, action } = cli.command {
        let client = if matches!(action, RecipeCommand::Run { .. }) {
            let token = require_recipe_mcp_token(token.as_deref())?;
            Some(AgentClient::connect(cli.addr).with_token(token))
        } else {
            None
        };
        return recipe_cmd::run(client, action, &schema);
    }

    let mut client = AgentClient::connect(cli.addr);
    if let Some(token) = token {
        client = client.with_token(token);
    }

    match cli.command {
        Command::Wait { timeout_ms } => {
            if let Some(ms) = timeout_ms {
                client = client.with_timeout(Duration::from_millis(ms));
            }
            print_resp(rpc(client.expect_ok(Op::Wait { timeout_ms }))?)
        }
        Command::Hello => print_resp(rpc(client.expect_ok(Op::Hello))?),
        Command::Snapshot { pretty } => {
            let resp = rpc(client.snapshot())?;
            if pretty {
                println!("{}", serde_json::to_string_pretty(&resp.tree)?);
            } else {
                print_resp(resp);
            }
        }
        Command::Screenshot { out } => {
            print_resp(rpc(client.screenshot(out.to_string_lossy().into_owned()))?)
        }
        Command::Click { target, delivery } => {
            print_resp(rpc(client.click_with_delivery(target, delivery))?)
        }
        Command::Type {
            target,
            text,
            delivery,
        } => print_resp(rpc(client.type_with_delivery(target, text, delivery))?),
        Command::SetValue { target, value } => print_resp(rpc(client.set_value(target, value))?),
        Command::Key {
            target,
            key,
            delivery,
        } => print_resp(rpc(client.key_with_delivery(target, key, delivery))?),
        Command::Assert {
            id,
            name,
            value,
            role,
            checked,
            exists,
            absent,
        } => {
            let spec = AssertSpec {
                target: id,
                name,
                value,
                role,
                checked,
                exists: Some(if absent { false } else { exists }),
            };
            print_resp(rpc(client.assert(spec))?);
        }
        Command::Invoke { name, args } => print_resp(rpc(client.invoke(name, parse_args(&args)?))?),
        Command::Shutdown => print_resp(rpc(client.expect_ok(Op::Shutdown))?),
        Command::Mcp | Command::Recipe { .. } => unreachable!(),
    }
    Ok(())
}

fn rpc<T>(result: std::result::Result<T, String>) -> Result<T> {
    result.map_err(|e| anyhow!("{e}"))
}

fn parse_args(pairs: &[String]) -> Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (key, raw) = pair
            .split_once('=')
            .with_context(|| format!("expected KEY=VALUE, got {pair}"))?;
        let value =
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.into()));
        map.insert(key.to_string(), value);
    }
    Ok(serde_json::Value::Object(map))
}

fn print_resp(resp: gpui_agent::Response) {
    println!("{}", serde_json::to_string_pretty(&resp).unwrap());
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn first_class_commands_are_generic_ops() {
        let cmd = Cli::command();
        let names: Vec<String> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "wait",
                "hello",
                "snapshot",
                "screenshot",
                "click",
                "type",
                "set-value",
                "key",
                "assert",
                "invoke",
                "shutdown",
                "mcp",
                "recipe",
            ]
        );
        assert!(!names.iter().any(|n| n == "todo"));
    }

    #[test]
    fn smoke_desktop_script_starts_todo_headless() {
        let script = include_str!("../../../scripts/smoke-desktop.sh");
        assert!(
            script.contains("todo-headless"),
            "desktop smoke must start the daemon SoT (ADR-001), not only the GUI:\n{script}"
        );
        assert!(
            script.contains("ADR-001"),
            "script header must cite ADR-001:\n{script}"
        );
        assert!(
            !script.lines().any(|line| {
                let t = line.trim_start();
                t.starts_with("cargo ") && t.contains("--features embedded-host")
            }),
            "must not silently use embedded-host as the only path:\n{script}"
        );
    }

    #[test]
    fn help_does_not_advertise_todo() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with("todo")),
            "help should not list a todo subcommand:\n{help}"
        );
        assert!(
            help.to_ascii_lowercase().contains("experimental"),
            "top-level help should label recipes experimental:\n{help}"
        );
    }

    #[test]
    fn cli_help_does_not_claim_cdp_attach() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help.contains("Drive any GPUI Kit app"),
            "help must not claim CDP-like attach to any process:\n{help}"
        );
        assert!(
            help.contains("AgentHost") || help.to_ascii_lowercase().contains("embed"),
            "help must mention AgentHost or embed:\n{help}"
        );
    }

    #[test]
    fn recipe_help_is_experimental_and_json_first() {
        let mut recipe = Cli::command()
            .find_subcommand("recipe")
            .expect("recipe")
            .clone();
        let about = recipe
            .get_about()
            .map(|s| s.to_string())
            .unwrap_or_default();
        assert!(
            about.to_ascii_lowercase().contains("experimental"),
            "recipe about should say experimental: {about}"
        );
        assert!(
            about.to_ascii_lowercase().contains("json"),
            "recipe about should mention JSON: {about}"
        );

        let help = recipe.render_long_help().to_string();
        for name in ["validate", "plan", "run", "resolve"] {
            assert!(help.contains(name), "missing {name} in:\n{help}");
        }
        let validate = recipe.find_subcommand("validate").expect("validate");
        let about = validate
            .get_about()
            .map(|s| s.to_string())
            .unwrap_or_default();
        assert!(
            about.to_ascii_lowercase().contains("experimental"),
            "{about}"
        );
        assert!(about.to_ascii_lowercase().contains("json"), "{about}");
    }

    #[test]
    fn invoke_args_parse_json_or_string() {
        let value = parse_args(&["title=Buy milk".into(), "id=1".into()]).unwrap();
        assert_eq!(value["title"], "Buy milk");
        assert_eq!(value["id"], 1);
    }

    #[test]
    fn assert_bool_flags_do_not_leave_positional_true() {
        let checked_true = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--checked",
            "true",
        ])
        .expect("checked true");
        match checked_true.command {
            Command::Assert {
                id,
                checked,
                exists,
                absent,
                ..
            } => {
                assert_eq!(id, "page-root");
                assert_eq!(checked, Some(true));
                assert!(exists);
                assert!(!absent);
            }
            other => panic!("unexpected {other:?}"),
        }

        let checked_false = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--checked",
            "false",
        ])
        .expect("checked false");
        match checked_false.command {
            Command::Assert { checked, .. } => assert_eq!(checked, Some(false)),
            other => panic!("unexpected {other:?}"),
        }

        let exists_true = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--exists",
            "true",
        ])
        .expect("exists true");
        match exists_true.command {
            Command::Assert { exists, absent, .. } => {
                assert!(exists);
                assert!(!absent);
            }
            other => panic!("unexpected {other:?}"),
        }

        let exists_false = Cli::try_parse_from([
            "gpui-agent",
            "assert",
            "--id",
            "page-root",
            "--exists",
            "false",
        ])
        .expect("exists false");
        match exists_false.command {
            Command::Assert { exists, .. } => assert!(!exists),
            other => panic!("unexpected {other:?}"),
        }

        let absent = Cli::try_parse_from(["gpui-agent", "assert", "--id", "page-root", "--absent"])
            .expect("absent");
        match absent.command {
            Command::Assert { absent, .. } => assert!(absent),
            other => panic!("unexpected {other:?}"),
        }

        let bare_checked =
            Cli::try_parse_from(["gpui-agent", "assert", "--id", "page-root", "--checked"])
                .expect("bare checked");
        match bare_checked.command {
            Command::Assert { checked, .. } => assert_eq!(checked, Some(true)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn click_delivery_defaults_to_semantic() {
        let cli = Cli::try_parse_from(["gpui-agent", "click", "todo-add"]).unwrap();
        match cli.command {
            Command::Click { target, delivery } => {
                assert_eq!(target, "todo-add");
                assert_eq!(delivery, DeliveryMode::Semantic);
            }
            other => panic!("unexpected {other:?}"),
        }

        let virt =
            Cli::try_parse_from(["gpui-agent", "click", "--delivery", "virtual", "todo-add"])
                .unwrap();
        match virt.command {
            Command::Click { delivery, .. } => assert_eq!(delivery, DeliveryMode::Virtual),
            other => panic!("unexpected {other:?}"),
        }

        let typed = Cli::try_parse_from([
            "gpui-agent",
            "type",
            "--delivery",
            "virtual",
            "todo-input",
            "Hi",
        ])
        .unwrap();
        match typed.command {
            Command::Type { delivery, text, .. } => {
                assert_eq!(delivery, DeliveryMode::Virtual);
                assert_eq!(text, "Hi");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn recipe_subcommands_are_validate_plan_run_resolve() {
        let validate = Cli::try_parse_from(["gpui-agent", "recipe", "validate", "x.json"]).unwrap();
        match validate.command {
            Command::Recipe {
                schema: _,
                action: RecipeCommand::Validate { path },
            } => assert_eq!(path.as_os_str(), "x.json"),
            other => panic!("unexpected {other:?}"),
        }

        let run = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "run",
            "x.wants",
            "--set",
            "title=Milk",
            "--yes",
        ])
        .unwrap();
        match run.command {
            Command::Recipe {
                schema: _,
                action: RecipeCommand::Run { yes, set, .. },
            } => {
                assert!(yes);
                assert_eq!(set, vec!["title=Milk"]);
            }
            other => panic!("unexpected {other:?}"),
        }

        let with_schema = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "validate",
            "x.json",
            "--schema",
            "examples/schemas/todo.json",
        ])
        .unwrap();
        match with_schema.command {
            Command::Recipe { schema, .. } => {
                assert_eq!(
                    schema.as_slice(),
                    [std::path::PathBuf::from("examples/schemas/todo.json")]
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn screenshot_and_recipe_screenshot_dir_parse() {
        let shot = Cli::try_parse_from([
            "gpui-agent",
            "screenshot",
            "--out",
            "artifacts/steps/001-wait.png",
        ])
        .unwrap();
        match shot.command {
            Command::Screenshot { out } => {
                assert_eq!(out.as_os_str(), "artifacts/steps/001-wait.png");
            }
            other => panic!("unexpected {other:?}"),
        }

        let run = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "run",
            "x.json",
            "--screenshot-dir",
            "artifacts/steps",
            "--screenshot-flagged",
        ])
        .unwrap();
        match run.command {
            Command::Recipe {
                schema: _,
                action:
                    RecipeCommand::Run {
                        screenshot_dir,
                        screenshot_flagged,
                        ..
                    },
            } => {
                assert_eq!(
                    screenshot_dir.as_deref(),
                    Some(std::path::Path::new("artifacts/steps"))
                );
                assert!(screenshot_flagged);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn invoke_without_args_is_empty_object() {
        let cli = Cli::try_parse_from(["gpui-agent", "invoke", "demo.ping"]).unwrap();
        match cli.command {
            Command::Invoke { name, args } => {
                assert_eq!(name, "demo.ping");
                assert!(args.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cli_addr_must_be_loopback() {
        let remote =
            Cli::try_parse_from(["gpui-agent", "--addr", "8.8.8.8:17421", "hello"]).unwrap();
        assert!(gpui_agent::ensure_loopback(remote.addr).is_err());
        assert!(
            gpui_agent::authorize_client(remote.addr, remote.token.as_deref(), remote.allow_remote)
                .is_err()
        );

        let local = Cli::try_parse_from(["gpui-agent", "hello"]).unwrap();
        assert!(gpui_agent::ensure_loopback(local.addr).is_ok());
        assert!(gpui_agent::authorize_client(local.addr, None, local.allow_remote).is_ok());
    }

    #[test]
    fn cli_remote_requires_allow_and_token() {
        let denied = Cli::try_parse_from([
            "gpui-agent",
            "--addr",
            "8.8.8.8:17421",
            "--token",
            "lab",
            "hello",
        ])
        .unwrap();
        assert!(!denied.allow_remote);
        assert!(
            gpui_agent::authorize_client(denied.addr, denied.token.as_deref(), denied.allow_remote)
                .is_err()
        );

        let no_token = Cli::try_parse_from([
            "gpui-agent",
            "--addr",
            "8.8.8.8:17421",
            "--allow-remote",
            "hello",
        ])
        .unwrap();
        assert!(no_token.allow_remote);
        assert!(
            gpui_agent::authorize_client(
                no_token.addr,
                take_non_empty_token(no_token.token).as_deref(),
                no_token.allow_remote
            )
            .is_err()
        );

        let allowed = Cli::try_parse_from([
            "gpui-agent",
            "--addr",
            "8.8.8.8:17421",
            "--allow-remote",
            "--token",
            "lab",
            "hello",
        ])
        .unwrap();
        assert!(allowed.allow_remote);
        assert!(
            gpui_agent::authorize_client(
                allowed.addr,
                take_non_empty_token(allowed.token).as_deref(),
                allowed.allow_remote
            )
            .is_ok()
        );
    }

    #[test]
    fn empty_cli_token_is_treated_as_unset() {
        assert_eq!(take_non_empty_token(None), None);
        assert_eq!(take_non_empty_token(Some(String::new())), None);
        assert_eq!(
            take_non_empty_token(Some("secret".into())),
            Some("secret".into())
        );
    }

    #[test]
    fn recipe_run_and_mcp_require_non_empty_token() {
        assert!(require_recipe_mcp_token(None).is_err());
        assert!(require_recipe_mcp_token(Some("")).is_err());
        let err = require_recipe_mcp_token(None).unwrap_err().to_string();
        assert!(err.contains("GPUI_AGENT_TOKEN"), "{err}");
        assert!(err.contains("recipe run"), "{err}");
        assert!(err.contains("mcp"), "{err}");
        assert_eq!(require_recipe_mcp_token(Some("secret")).unwrap(), "secret");
    }

    #[test]
    fn help_mentions_recipe_mcp_token() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("GPUI_AGENT_TOKEN") && help.to_ascii_lowercase().contains("recipe run"),
            "{help}"
        );
    }
}
