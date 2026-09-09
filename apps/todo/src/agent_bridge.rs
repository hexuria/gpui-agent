use std::time::Duration;

use gpui_agent::mailbox::AgentMailbox;
use gpui_agent::security::from_env;
use gpui_agent::server::spawn_mailbox;

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
            match spawn_mailbox(
                config.addr,
                config.token.clone(),
                mailbox.clone(),
                Duration::from_secs(8),
            ) {
                Ok((addr, _)) => {
                    eprintln!("gpui-agent listening on {addr} (platform=desktop, app=todo)");
                    eprintln!("opt-in: GPUI_AGENT=1 · loopback only · protocol v1");
                    eprintln!(
                        "delivery: semantic (default) or virtual (in-window GPUI events, no OS HID)"
                    );
                    config.eprint_token_banner();
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
