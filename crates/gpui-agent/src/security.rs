use std::fmt;
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
/// Optional shared secret via `GPUI_AGENT_TOKEN`; when set, every request
/// must carry the same token. Anyone who can reach the socket can drive
/// the UI, so this is a local developer/agent tool, not a remote API.
/// Host token stays optional so one-off `click`/`snapshot` smoke still
/// works. CLI `recipe run` and `mcp` require a non-empty client token
/// (P2); set the same value on host and client for those workflows.
#[derive(Clone)]
pub struct AgentConfig {
    pub addr: SocketAddr,
    pub token: Option<String>,
}

impl fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConfig")
            .field("addr", &self.addr)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
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

    ensure_loopback(addr)?;

    let token = std::env::var("GPUI_AGENT_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());

    Ok(Some(AgentConfig { addr, token }))
}

pub fn truthy_env(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// True for IPv4 `127.0.0.0/8` and IPv6 `::1` only.
///
/// IPv4-mapped loopback (`::ffff:127.0.0.1`) is **not** treated as loopback
/// so bind checks fail closed.
pub fn is_loopback_addr(addr: SocketAddr) -> bool {
    is_loopback_ip(addr.ip())
}

pub fn ensure_loopback(addr: SocketAddr) -> Result<SocketAddr, SecurityError> {
    if is_loopback_addr(addr) {
        Ok(addr)
    } else {
        Err(SecurityError::NonLoopback(addr))
    }
}

fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback(),
    }
}

/// Best-effort constant-time compare for the optional shared secret.
///
/// Still leaks the longer length via runtime; that is acceptable for a
/// local developer token. Do not use this for high-value remote secrets.
pub fn tokens_match(got: &str, expected: &str) -> bool {
    tokens_match_bytes(got.as_bytes(), expected.as_bytes())
}

#[inline(never)]
fn tokens_match_bytes(got: &[u8], expected: &[u8]) -> bool {
    let len = got.len().max(expected.len());
    let mut acc = got.len() ^ expected.len();
    let mut i = 0;
    while i < len {
        let a = got.get(i).copied().unwrap_or(0);
        let b = expected.get(i).copied().unwrap_or(0);
        acc |= usize::from(a ^ b);
        i += 1;
    }
    acc == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_addr_is_loopback() {
        assert!(is_loopback_addr(default_addr()));
    }

    #[test]
    fn rejects_public_bind() {
        let addr: SocketAddr = "1.2.3.4:9".parse().unwrap();
        assert!(!is_loopback_addr(addr));
        assert!(ensure_loopback(addr).is_err());
    }

    #[test]
    fn accepts_loopback_variants() {
        for raw in ["127.0.0.1:17421", "127.0.0.2:9", "[::1]:17421"] {
            let addr: SocketAddr = raw.parse().unwrap();
            assert!(is_loopback_addr(addr), "{raw}");
            assert!(ensure_loopback(addr).is_ok(), "{raw}");
        }
    }

    #[test]
    fn rejects_unspecified_and_mapped_loopback() {
        for raw in ["0.0.0.0:17421", "[::]:17421", "[::ffff:127.0.0.1]:17421"] {
            let addr: SocketAddr = raw.parse().unwrap();
            assert!(!is_loopback_addr(addr), "{raw}");
        }
    }

    #[test]
    fn tokens_match_compares_full_secret() {
        assert!(tokens_match("secret", "secret"));
        assert!(!tokens_match("secret", "secreT"));
        assert!(!tokens_match("secret", "secret!"));
        assert!(!tokens_match("", "x"));
        assert!(tokens_match("", ""));
    }

    #[test]
    fn config_debug_redacts_token() {
        let cfg = AgentConfig {
            addr: default_addr(),
            token: Some("super-secret-token".into()),
        };
        let rendered = format!("{cfg:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
    }
}
