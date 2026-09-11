use std::time::Duration;

use gpui_agent::mailbox::AgentMailbox;
use gpui_agent::security::from_env;
use gpui_agent::server::spawn_mailbox;

fn auth_banner(token_set: bool) -> &'static str {
    if token_set {
        "auth: required (GPUI_AGENT_TOKEN set; recipe/MCP clients must send the same token)"
    } else {
        "auth: none (GPUI_AGENT_INSECURE_NO_TOKEN=1 — any local process can drive this host)"
    }
}

/// Start the localhost control plane when `GPUI_AGENT=1`.
///
/// The TCP thread posts onto `AgentMailbox`; `TodoApp::render` drains it on
/// the GPUI UI thread so actions mutate the same store the widgets use.
pub fn maybe_start() -> Option<AgentMailbox> {
    match from_env() {
        Ok(None) => {
            eprintln!("agent control plane off (set GPUI_AGENT=1 to opt in)");
            None
        }
        Err(err) => {
            eprintln!("agent control plane refused: {err}");
            None
        }
        Ok(Some(config)) => {
            let mailbox = AgentMailbox::new();
            let auth = auth_banner(config.token.is_some());
            match spawn_mailbox(
                config.addr,
                config.token,
                mailbox.clone(),
                Duration::from_secs(8),
            ) {
                Ok((addr, _)) => {
                    eprintln!("gpui-agent listening on {addr} (platform=desktop, app=todo)");
                    eprintln!("opt-in: GPUI_AGENT=1 · bind via from_env · protocol v2");
                    eprintln!("{auth}");
                    eprintln!(
                        "delivery: semantic (default) or virtual (in-window GPUI events, no OS HID)"
                    );
                    eprintln!(
                        "screenshot: macOS writes this window via screencapture -l (Screen Recording); \
                         other OSes and headless stay screenshot_unavailable"
                    );
                    Some(mailbox)
                }
                Err(err) => {
                    eprintln!("gpui-agent failed to bind: {err}");
                    None
                }
            }
        }
    }
}
