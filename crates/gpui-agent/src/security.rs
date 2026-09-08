use std::net::{IpAddr, SocketAddr};

use thiserror::Error;

use crate::server::default_addr;

#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("automation is disabled (set GPUI_AGENT=1 to opt in)")]
    Disabled,
    #[error("refusing to enable automation in a release build without GPUI_AGENT_ALLOW_RELEASE=1")]
    ReleaseBlocked,
    #[error("automation must bind a loopback address, got {0}")]
    NonLoopback(SocketAddr),
    #[error("invalid GPUI_AGENT_ADDR: {0}")]
    BadAddr(String),
}

/// Runtime gate for the control plane.
///
/// Trust model:
/// - Off unless `GPUI_AGENT` is a truthy value (`1`, `true`, `yes`).
/// - Release binaries also require `GPUI_AGENT_ALLOW_RELEASE=1`.
/// - The listen address must be loopback (default `127.0.0.1:17421`).
/// - Optional shared secret via `GPUI_AGENT_TOKEN`; when set, every request
///   must carry the same token. Anyone who can reach the socket can drive
///   the UI, so this is a local developer/agent tool, not a remote API.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub addr: SocketAddr,
    pub token: Option<String>,
}

pub fn from_env() -> Result<Option<AgentConfig>, SecurityError> {
    if !truthy_env("GPUI_AGENT") {
        return Ok(None);
    }

    if !cfg!(debug_assertions) && !truthy_env("GPUI_AGENT_ALLOW_RELEASE") {
        return Err(SecurityError::ReleaseBlocked);
    }

    let addr = match std::env::var("GPUI_AGENT_ADDR") {
        Ok(raw) => raw
            .parse::<SocketAddr>()
            .map_err(|_| SecurityError::BadAddr(raw))?,
        Err(_) => default_addr(),
    };

    if !is_loopback(addr.ip()) {
        return Err(SecurityError::NonLoopback(addr));
    }

    let token = std::env::var("GPUI_AGENT_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());

    Ok(Some(AgentConfig { addr, token }))
}

pub fn truthy_env(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_addr_is_loopback() {
        assert!(is_loopback(default_addr().ip()));
    }

    #[test]
    fn rejects_public_bind() {
        let addr: SocketAddr = "1.2.3.4:9".parse().unwrap();
        assert!(!is_loopback(addr.ip()));
    }
}
