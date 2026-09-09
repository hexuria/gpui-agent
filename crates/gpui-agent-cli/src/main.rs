mod mcp;
mod recipe_cmd;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op};
use gpui_agent::DEFAULT_ADDR_STR;
use recipe_cmd::RecipeCommand;

/// Drive any GPUI Kit app over the opt-in agent protocol (not CDP).
///
/// First-class commands are the protocol ops only. App-specific verbs
/// (`todo.add`, `nav.go`, …) belong in the host (`invoke`) or in agent
/// prompts — not as CLI subcommands.
#[derive(Parser)]
#[command(name = "gpui-agent", after_help = AFTER_HELP)]
struct Cli {
    /// Host:port of the automation server (loopback only on the app side).
    #[arg(long, default_value = DEFAULT_ADDR_STR, env = "GPUI_AGENT_ADDR")]
    addr: SocketAddr,
    /// Shared secret; must match the host. Required unless `--allow-empty-token`.
    #[arg(long, env = "GPUI_AGENT_TOKEN")]
    token: Option<String>,
    /// Connect with no token (loopback labs only). Also `GPUI_AGENT_ALLOW_EMPTY_TOKEN=1`.
    /// The host must be started the same way, or requests fail auth.
    #[arg(long)]
    allow_empty_token: bool,
    #[command(subcommand)]
    command: Command,
}

const AFTER_HELP: &str = "\
App-specific helpers (for example the sample todo app) live in examples/,
not in this CLI. Navigate pages with click + assert on stable ids, or invoke
a command the host registered. Batch many ops in one process with
`recipe validate|plan|run` (experimental; see docs/RECIPES.md).";

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
    /// Tiny MCP stdio server exposing the same generic tools.
    Mcp,
    /// Experimental: validate / plan / run a recipe of protocol ops.
    Recipe {
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

const TOKEN_REQUIRED: &str = "\
refusing to connect without a token. Pass --token / GPUI_AGENT_TOKEN \
(must match the host), or --allow-empty-token / GPUI_AGENT_ALLOW_EMPTY_TOKEN=1 \
for loopback-only labs. Hosts mint an ephemeral token at bind when \
GPUI_AGENT_TOKEN is unset.";

fn command_connects_to_host(command: &Command) -> bool {
    match command {
        Command::Recipe { action } => matches!(action, RecipeCommand::Run { .. }),
        _ => true,
    }
}

fn resolve_client_token(explicit: Option<&str>, allow_empty: bool) -> Result<Option<String>> {
    if let Some(t) = explicit.filter(|s| !s.is_empty()) {
        return Ok(Some(t.to_string()));
    }
    if allow_empty {
        return Ok(None);
    }
    Err(anyhow!(TOKEN_REQUIRED))
}

fn allow_empty_token(flag: bool) -> bool {
    flag || gpui_agent::truthy_env("GPUI_AGENT_ALLOW_EMPTY_TOKEN")
}

fn make_client(addr: SocketAddr, token: Option<String>) -> AgentClient {
    let mut client = AgentClient::connect(addr);
    if let Some(token) = token {
        client = client.with_token(token);
    }
    client
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    gpui_agent::ensure_loopback(cli.addr)
        .with_context(|| format!("refusing non-loopback agent address {}", cli.addr))?;
    let allow_empty = allow_empty_token(cli.allow_empty_token);
    let token = if command_connects_to_host(&cli.command) {
        resolve_client_token(cli.token.as_deref(), allow_empty)?
    } else {
        None
    };
    if matches!(cli.command, Command::Mcp) {
        return mcp::run(cli.addr, token);
    }
    if let Command::Recipe { action } = cli.command {
        let client = if matches!(action, RecipeCommand::Run { .. }) {
            Some(make_client(cli.addr, token))
        } else {
            None
        };
        return recipe_cmd::run(client, action);
    }

    let mut client = make_client(cli.addr, token);

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
    fn help_does_not_advertise_todo() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with("todo")),
            "help should not list a todo subcommand:\n{help}"
        );
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
                action: RecipeCommand::Validate { path },
            } => assert_eq!(path.as_os_str(), "x.json"),
            other => panic!("unexpected {other:?}"),
        }

        let run = Cli::try_parse_from([
            "gpui-agent",
            "recipe",
            "run",
            "x.json",
            "--set",
            "title=Milk",
            "--yes",
        ])
        .unwrap();
        match run.command {
            Command::Recipe {
                action: RecipeCommand::Run { yes, set, .. },
            } => {
                assert!(yes);
                assert_eq!(set, vec!["title=Milk"]);
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

        let local = Cli::try_parse_from(["gpui-agent", "hello"]).unwrap();
        assert!(gpui_agent::ensure_loopback(local.addr).is_ok());
    }

    #[test]
    fn connecting_commands_require_a_token() {
        let err = resolve_client_token(None, false).unwrap_err();
        assert!(err
            .to_string()
            .contains("refusing to connect without a token"));
        assert!(resolve_client_token(Some(""), false).is_err());
        assert_eq!(
            resolve_client_token(Some("secret"), false)
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        assert_eq!(resolve_client_token(None, true).unwrap(), None);
        assert_eq!(
            resolve_client_token(Some("secret"), true)
                .unwrap()
                .as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn recipe_validate_plan_resolve_do_not_connect() {
        let validate = Cli::try_parse_from(["gpui-agent", "recipe", "validate", "x.json"]).unwrap();
        assert!(!command_connects_to_host(&validate.command));

        let plan = Cli::try_parse_from(["gpui-agent", "recipe", "plan", "x.json"]).unwrap();
        assert!(!command_connects_to_host(&plan.command));

        let resolve = Cli::try_parse_from(["gpui-agent", "recipe", "resolve", "add milk"]).unwrap();
        assert!(!command_connects_to_host(&resolve.command));

        let run = Cli::try_parse_from(["gpui-agent", "recipe", "run", "x.json"]).unwrap();
        assert!(command_connects_to_host(&run.command));

        let hello = Cli::try_parse_from(["gpui-agent", "hello"]).unwrap();
        assert!(command_connects_to_host(&hello.command));

        let mcp = Cli::try_parse_from(["gpui-agent", "mcp"]).unwrap();
        assert!(command_connects_to_host(&mcp.command));
    }

    #[test]
    fn allow_empty_token_flag_parses() {
        let cli = Cli::try_parse_from(["gpui-agent", "--allow-empty-token", "hello"]).unwrap();
        assert!(cli.allow_empty_token);
        assert!(command_connects_to_host(&cli.command));
        // `--allow-empty-token` is the opt-out. A token still present in the
        // process env (clap `GPUI_AGENT_TOKEN`) wins; that is tested via
        // `resolve_client_token` below, not via parse, so this test is not
        // flaky under smoke.sh leftovers.
        assert_eq!(resolve_client_token(None, true).unwrap(), None);
    }
}
