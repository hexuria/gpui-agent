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
    #[error(
        "refusing non-loopback address {0} without GPUI_AGENT_REMOTE=1 (host) or --allow-remote / GPUI_AGENT_ALLOW_REMOTE=1 (client)"
    )]
    RemoteDisabled(SocketAddr),
    #[error("non-loopback address {0} requires a non-empty GPUI_AGENT_TOKEN")]
    RemoteRequiresToken(SocketAddr),
    #[error("invalid GPUI_AGENT_ADDR: {0}")]
    BadAddr(String),
}

/// Runtime gate for the control plane.
///
/// Trust model:
/// - Off unless `GPUI_AGENT` is a truthy value (`1`, `true`, `yes`).
/// - Release binaries also require `GPUI_AGENT_ALLOW_RELEASE=1`.
/// - Default listen address is loopback (`127.0.0.1:17421`). Token optional
///   on loopback so one-off `click`/`snapshot` smoke still works.
/// - Non-loopback bind requires `GPUI_AGENT_REMOTE=1` **and** a non-empty
///   `GPUI_AGENT_TOKEN`. Fail closed: `0.0.0.0` without that triple is
///   refused. Transport is still plaintext TCP — lab / trusted network
///   only until TLS or an SSH/Tailscale hop exists.
/// - CLI remote connect requires the same token **and** `--allow-remote`
///   / `GPUI_AGENT_ALLOW_REMOTE=1` so a mistyped `--addr` cannot leak
///   the token off-box (M1).
/// CLI `recipe run` and `mcp` still require a non-empty client token (P2)
/// even on loopback.
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

    let token = std::env::var("GPUI_AGENT_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let allow_remote = truthy_env("GPUI_AGENT_REMOTE");
    authorize_bind(addr, token.as_deref(), allow_remote)?;

    Ok(Some(AgentConfig { addr, token }))
}

/// Host bind policy: loopback always; non-loopback only with token + flag.
pub fn authorize_bind(
    addr: SocketAddr,
    token: Option<&str>,
    allow_remote: bool,
) -> Result<SocketAddr, SecurityError> {
    authorize_endpoint(addr, token, allow_remote)
}

/// Client connect policy (same gates as bind; flag is `--allow-remote`).
pub fn authorize_client(
    addr: SocketAddr,
    token: Option<&str>,
    allow_remote: bool,
) -> Result<SocketAddr, SecurityError> {
    authorize_endpoint(addr, token, allow_remote)
}

fn authorize_endpoint(
    addr: SocketAddr,
    token: Option<&str>,
    allow_remote: bool,
) -> Result<SocketAddr, SecurityError> {
    if is_loopback_addr(addr) {
        return Ok(addr);
    }
    if !allow_remote {
        return Err(SecurityError::RemoteDisabled(addr));
    }
    match token {
        Some(t) if !t.is_empty() => Ok(addr),
        _ => Err(SecurityError::RemoteRequiresToken(addr)),
    }
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

    #[test]
    fn loopback_bind_does_not_need_token_or_remote_flag() {
        let addr: SocketAddr = "127.0.0.1:17421".parse().unwrap();
        assert!(authorize_bind(addr, None, false).is_ok());
        assert!(authorize_client(addr, None, false).is_ok());
    }

    #[test]
    fn public_bind_without_flag_fails_closed() {
        for raw in ["1.2.3.4:17421", "0.0.0.0:17421", "[::]:17421"] {
            let addr: SocketAddr = raw.parse().unwrap();
            let err = authorize_bind(addr, Some("secret"), false).unwrap_err();
            assert!(
                matches!(err, SecurityError::RemoteDisabled(_)),
                "{raw}: {err}"
            );
            assert!(ensure_loopback(addr).is_err(), "{raw}");
        }
    }

    #[test]
    fn public_bind_with_flag_but_no_token_fails_closed() {
        let addr: SocketAddr = "10.0.0.2:17421".parse().unwrap();
        let err = authorize_bind(addr, None, true).unwrap_err();
        assert!(
            matches!(err, SecurityError::RemoteRequiresToken(_)),
            "{err}"
        );
        let err = authorize_bind(addr, Some(""), true).unwrap_err();
        assert!(
            matches!(err, SecurityError::RemoteRequiresToken(_)),
            "{err}"
        );
    }

    #[test]
    fn public_bind_with_flag_and_token_is_allowed() {
        let addr: SocketAddr = "10.0.0.2:17421".parse().unwrap();
        assert_eq!(authorize_bind(addr, Some("lab-token"), true).unwrap(), addr);
        assert_eq!(
            authorize_client(addr, Some("lab-token"), true).unwrap(),
            addr
        );
    }

    #[test]
    fn client_remote_without_allow_does_not_send_token() {
        let addr: SocketAddr = "8.8.8.8:17421".parse().unwrap();
        let err = authorize_client(addr, Some("lab-token"), false).unwrap_err();
        assert!(matches!(err, SecurityError::RemoteDisabled(_)), "{err}");
    }
}
