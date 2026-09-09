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
    #[error("could not read OS entropy to mint an ephemeral token")]
    Entropy,
}

/// How the host chose its shared secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPolicy {
    /// Non-empty `GPUI_AGENT_TOKEN` from the environment.
    Env,
    /// Minted because the env token was unset and empty-token was not allowed.
    Ephemeral,
    /// Explicit lab opt-out: `GPUI_AGENT_ALLOW_EMPTY_TOKEN=1`.
    Open,
}

/// Result of [`resolve_token`]: either a secret or an explicit open socket.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedToken {
    Provided(String),
    Ephemeral(String),
    Open,
}

impl fmt::Debug for ResolvedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provided(_) => f.write_str("Provided(<redacted>)"),
            Self::Ephemeral(_) => f.write_str("Ephemeral(<redacted>)"),
            Self::Open => f.write_str("Open"),
        }
    }
}

impl ResolvedToken {
    pub fn token(&self) -> Option<&str> {
        match self {
            Self::Provided(t) | Self::Ephemeral(t) => Some(t),
            Self::Open => None,
        }
    }

    pub fn policy(&self) -> TokenPolicy {
        match self {
            Self::Provided(_) => TokenPolicy::Env,
            Self::Ephemeral(_) => TokenPolicy::Ephemeral,
            Self::Open => TokenPolicy::Open,
        }
    }

    fn into_token(self) -> Option<String> {
        match self {
            Self::Provided(t) | Self::Ephemeral(t) => Some(t),
            Self::Open => None,
        }
    }
}

/// Runtime gate for the control plane.
///
/// Trust model:
/// - Off unless `GPUI_AGENT` is a truthy value (`1`, `true`, `yes`).
/// - Release binaries also require `GPUI_AGENT_ALLOW_RELEASE=1`.
/// - The listen address must be loopback (default `127.0.0.1:17421`).
/// - A shared secret is **required** by default. Missing `GPUI_AGENT_TOKEN`
///   mints a 32-byte ephemeral token (printed once via
///   [`AgentConfig::eprint_token_banner`]). `GPUI_AGENT_ALLOW_EMPTY_TOKEN=1`
///   restores the old optional-token lab behavior. `AgentServer::bind` with
///   `token: None` stays allowed for in-process tests.
#[derive(Clone)]
pub struct AgentConfig {
    pub addr: SocketAddr,
    pub token: Option<String>,
    pub token_policy: TokenPolicy,
}

impl fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConfig")
            .field("addr", &self.addr)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("token_policy", &self.token_policy)
            .finish()
    }
}

impl AgentConfig {
    /// Print the token policy to stderr once after a successful bind.
    ///
    /// Ephemeral tokens are written in `GPUI_AGENT_TOKEN=…` form so a human
    /// or wrapper can copy them. Env-provided tokens are never echoed.
    pub fn eprint_token_banner(&self) {
        match self.token_policy {
            TokenPolicy::Env => {
                eprintln!("gpui-agent: token required (from GPUI_AGENT_TOKEN)");
            }
            TokenPolicy::Ephemeral => {
                let token = self.token.as_deref().unwrap_or("");
                eprintln!("gpui-agent: minted ephemeral token (GPUI_AGENT_TOKEN was unset)");
                eprintln!("GPUI_AGENT_TOKEN={token}");
                eprintln!(
                    "clients must send this token. set GPUI_AGENT_ALLOW_EMPTY_TOKEN=1 to restore optional-token labs"
                );
            }
            TokenPolicy::Open => {
                eprintln!(
                    "gpui-agent: token disabled (GPUI_AGENT_ALLOW_EMPTY_TOKEN=1); any local process can drive this socket"
                );
            }
        }
    }
}

struct EnvInputs {
    enabled: bool,
    allow_release: bool,
    addr: Option<String>,
    token: Option<String>,
    allow_empty_token: bool,
    debug_assertions: bool,
}

pub fn from_env() -> Result<Option<AgentConfig>, SecurityError> {
    from_parts(EnvInputs {
        enabled: truthy_env("GPUI_AGENT"),
        allow_release: truthy_env("GPUI_AGENT_ALLOW_RELEASE"),
        addr: std::env::var("GPUI_AGENT_ADDR").ok(),
        token: std::env::var("GPUI_AGENT_TOKEN").ok(),
        allow_empty_token: truthy_env("GPUI_AGENT_ALLOW_EMPTY_TOKEN"),
        debug_assertions: cfg!(debug_assertions),
    })
}

fn from_parts(inputs: EnvInputs) -> Result<Option<AgentConfig>, SecurityError> {
    if !inputs.enabled {
        return Ok(None);
    }

    if !inputs.debug_assertions && !inputs.allow_release {
        return Err(SecurityError::ReleaseBlocked);
    }

    let addr = match inputs.addr {
        Some(raw) => raw
            .parse::<SocketAddr>()
            .map_err(|_| SecurityError::BadAddr(raw))?,
        None => default_addr(),
    };

    ensure_loopback(addr)?;

    let resolved = resolve_token(inputs.token.as_deref(), inputs.allow_empty_token)?;
    Ok(Some(AgentConfig {
        addr,
        token_policy: resolved.policy(),
        token: resolved.into_token(),
    }))
}

/// Pick a host token from env-shaped inputs (no process env required).
///
/// - Non-empty `token_env` wins (even if `allow_empty` is set).
/// - Else `allow_empty` → open socket (tests / `--allow-empty-token` labs).
/// - Else mint a 32-byte hex secret.
///
/// An empty `token_env` is treated as unset (mint), not as an opt-out.
pub fn resolve_token(
    token_env: Option<&str>,
    allow_empty: bool,
) -> Result<ResolvedToken, SecurityError> {
    match token_env {
        Some(t) if !t.is_empty() => Ok(ResolvedToken::Provided(t.to_string())),
        _ if allow_empty => Ok(ResolvedToken::Open),
        _ => Ok(ResolvedToken::Ephemeral(mint_ephemeral_token()?)),
    }
}

fn mint_ephemeral_token() -> Result<String, SecurityError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| SecurityError::Entropy)?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
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
            token_policy: TokenPolicy::Env,
        };
        let rendered = format!("{cfg:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
        assert!(rendered.contains("Env"), "{rendered}");
    }

    #[test]
    fn resolved_token_debug_redacts() {
        let rendered = format!(
            "{:?}",
            ResolvedToken::Ephemeral("super-secret-token".into())
        );
        assert_eq!(rendered, "Ephemeral(<redacted>)");
        assert!(!rendered.contains("super-secret-token"));
    }

    #[test]
    fn resolve_token_mints_64_hex_when_unset() {
        let a = resolve_token(None, false).unwrap();
        let b = resolve_token(None, false).unwrap();
        match (&a, &b) {
            (ResolvedToken::Ephemeral(left), ResolvedToken::Ephemeral(right)) => {
                assert_eq!(left.len(), 64);
                assert_eq!(right.len(), 64);
                assert!(left
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
                assert_ne!(left, right, "two mints must not collide");
            }
            other => panic!("expected two ephemeral tokens, got {other:?}"),
        }
        assert_eq!(a.policy(), TokenPolicy::Ephemeral);
    }

    #[test]
    fn resolve_token_allow_empty_is_open() {
        let open = resolve_token(None, true).unwrap();
        assert_eq!(open, ResolvedToken::Open);
        assert_eq!(open.token(), None);
        assert_eq!(open.policy(), TokenPolicy::Open);
    }

    #[test]
    fn resolve_token_env_wins_over_allow_empty() {
        let got = resolve_token(Some("from-env"), true).unwrap();
        assert_eq!(got, ResolvedToken::Provided("from-env".into()));
        assert_eq!(got.policy(), TokenPolicy::Env);
    }

    #[test]
    fn resolve_token_empty_string_mints_not_open() {
        let got = resolve_token(Some(""), false).unwrap();
        assert!(matches!(got, ResolvedToken::Ephemeral(_)), "{got:?}");
    }

    #[test]
    fn from_parts_disabled_without_gpui_agent() {
        let cfg = from_parts(EnvInputs {
            enabled: false,
            allow_release: false,
            addr: None,
            token: None,
            allow_empty_token: false,
            debug_assertions: true,
        })
        .unwrap();
        assert!(cfg.is_none());
    }

    #[test]
    fn from_parts_release_blocked() {
        let err = from_parts(EnvInputs {
            enabled: true,
            allow_release: false,
            addr: None,
            token: Some("x".into()),
            allow_empty_token: false,
            debug_assertions: false,
        })
        .unwrap_err();
        assert!(matches!(err, SecurityError::ReleaseBlocked));
    }

    #[test]
    fn from_parts_mints_when_token_unset() {
        let cfg = from_parts(EnvInputs {
            enabled: true,
            allow_release: false,
            addr: None,
            token: None,
            allow_empty_token: false,
            debug_assertions: true,
        })
        .unwrap()
        .expect("enabled");
        assert_eq!(cfg.token_policy, TokenPolicy::Ephemeral);
        assert_eq!(cfg.token.as_ref().map(String::len), Some(64));
    }

    #[test]
    fn from_parts_open_when_allow_empty() {
        let cfg = from_parts(EnvInputs {
            enabled: true,
            allow_release: false,
            addr: None,
            token: None,
            allow_empty_token: true,
            debug_assertions: true,
        })
        .unwrap()
        .expect("enabled");
        assert_eq!(cfg.token_policy, TokenPolicy::Open);
        assert_eq!(cfg.token, None);
    }

    #[test]
    fn from_parts_uses_provided_token() {
        let cfg = from_parts(EnvInputs {
            enabled: true,
            allow_release: true,
            addr: Some("127.0.0.1:9".into()),
            token: Some("shared".into()),
            allow_empty_token: true,
            debug_assertions: false,
        })
        .unwrap()
        .expect("enabled");
        assert_eq!(cfg.addr, "127.0.0.1:9".parse().unwrap());
        assert_eq!(cfg.token.as_deref(), Some("shared"));
        assert_eq!(cfg.token_policy, TokenPolicy::Env);
    }
}
