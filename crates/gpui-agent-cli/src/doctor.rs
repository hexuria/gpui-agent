//! `targets` and `doctor`: what can be driven from here, and why a host
//! that should answer does not.

use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

use gpui_agent::client::{AgentClient, NO_REPLY};
use gpui_agent::protocol::Op;
use gpui_agent::tree::UiNode;
use serde_json::{Value, json};

/// Hosts that published themselves on this machine, and the devices `adb`
/// sees with the ports forwarded to them.
pub fn targets() -> Value {
    let hosts = gpui_agent::live_hosts(&gpui_agent::runtime_root());
    json!({ "hosts": hosts, "adb": adb_devices() })
}

fn adb(args: &[&str]) -> Option<String> {
    let out = Command::new("adb").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `None` when `adb` is not installed or does not answer.
fn adb_devices() -> Option<Vec<Value>> {
    let devices = adb(&["devices"])?;
    let forwards = adb(&["forward", "--list"]).unwrap_or_default();
    Some(parse_adb(&devices, &forwards))
}

fn parse_adb(devices: &str, forwards: &str) -> Vec<Value> {
    devices
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;
            // `<serial> tcp:<local> tcp:<remote>`
            let forwarded: Vec<String> = forwards
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split_whitespace();
                    (parts.next()? == serial).then_some(())?;
                    let local = parts.next()?.strip_prefix("tcp:")?;
                    Some(format!("127.0.0.1:{local}"))
                })
                .collect();
            Some(json!({ "serial": serial, "state": state, "forwarded": forwarded }))
        })
        .collect()
}

fn count(nodes: &[UiNode]) -> usize {
    nodes.iter().map(|node| 1 + count(&node.children)).sum()
}

/// Runs the checks in order and prints one line each; `false` when the host
/// cannot be driven as it stands.
pub fn doctor(addr: SocketAddr, mut client: AgentClient, has_token: bool) -> bool {
    let connect = Duration::from_secs(2);
    if let Err(err) = TcpStream::connect_timeout(&addr, connect) {
        println!("fail  connect {addr}: {err}");
        println!("      Start the app with GPUI_AGENT=1 (a development build).");
        println!("      Android: `adb forward tcp:{0} tcp:{0}`.", addr.port());
        println!("      `gpui-agent targets` lists hosts and devices found here.");
        return false;
    }
    println!("ok    connect {addr}");

    let hello = match client.expect_ok(Op::Hello) {
        Ok(resp) => resp.hello,
        Err(err) => {
            println!("fail  hello: {err}");
            if err.starts_with(NO_REPLY) {
                println!("      The port is open but nothing answered: a forwarded port");
                println!("      with no host behind it, or an app too busy to reply.");
            } else if has_token {
                println!("      If this is an auth error, the token differs from the host's.");
            } else {
                println!("      No token given: set GPUI_AGENT_TOKEN to the host's value.");
            }
            return false;
        }
    };
    let Some(hello) = hello else {
        println!("fail  hello: the reply carried no hello");
        return false;
    };
    println!(
        "ok    hello: {app} ({framework}, {platform:?} on {os}), protocol {protocol}",
        app = hello.app,
        framework = hello.framework.as_deref().unwrap_or("framework not stated"),
        platform = hello.platform,
        os = hello.os,
        protocol = hello.protocol,
    );
    if !hello.ready {
        println!("warn  the host says it is not ready yet (`gpui-agent wait`)");
    }
    if hello.features.iter().any(|f| f == "screenshot_inline") {
        println!("ok    features: {}", hello.features.join(", "));
    } else {
        println!("warn  a 0.1 host: press, scroll, hover, drag and viewport go through `invoke`;");
        println!(
            "      swipe, long-press, pinch, paste, system and inline screenshots are refused"
        );
    }

    let start = Instant::now();
    match client.snapshot() {
        Ok(resp) => {
            let took = start.elapsed();
            let nodes = resp.tree.map_or(0, |tree| count(&tree.nodes));
            let slow = took > Duration::from_secs(2);
            println!(
                "{}  snapshot: {nodes} nodes in {} ms",
                if slow { "warn" } else { "ok  " },
                took.as_millis()
            );
            if slow {
                println!("      Slow answers: pass --reply-timeout-ms on this host.");
            }
            true
        }
        Err(err) => {
            println!("fail  snapshot: {err}");
            if err.starts_with(NO_REPLY) {
                println!("      The app answers hello but not its UI thread in time:");
                println!("      retry with a larger --reply-timeout-ms, or pause what animates.");
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adb_devices_carry_their_forwarded_ports() {
        let devices = "List of devices attached\nemulator-5554\tdevice\nR58M\tunauthorized\n\n";
        let forwards = "emulator-5554 tcp:17421 tcp:17421\nother tcp:1 tcp:1\n";
        assert_eq!(
            parse_adb(devices, forwards),
            vec![
                json!({"serial": "emulator-5554", "state": "device", "forwarded": ["127.0.0.1:17421"]}),
                json!({"serial": "R58M", "state": "unauthorized", "forwarded": []}),
            ]
        );
    }
}
