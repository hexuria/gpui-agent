use std::fmt;

use serde::{Deserialize, Serialize};

use crate::tree::UiTree;

/// Bump when introducing a breaking change to the wire format.
pub const PROTOCOL_VERSION: u32 = 2;

/// Which runtime is serving the protocol. Only `Desktop` and `Headless`
/// are implemented in this repository; `Web` and `Mobile` are reserved
/// so later hosts can speak the same messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    Desktop,
    Headless,
    Web,
    Mobile,
}

impl PlatformKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Headless => "headless",
            Self::Web => "web",
            Self::Mobile => "mobile",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// HMAC-SHA256(token, session nonce) as lowercase hex. Never the raw token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(flatten)]
    pub op: Op,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("v", &self.v)
            .field("id", &self.id)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .field("op", &self.op)
            .finish()
    }
}

/// How click / type / key are delivered into the app.
///
/// `semantic` (default) calls the widget handler by stable id. `virtual`
/// synthesizes GPUI pointer/key events on the UI thread — never OS HID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    #[default]
    Semantic,
    Virtual,
}

impl DeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Virtual => "virtual",
        }
    }

    pub fn is_semantic(&self) -> bool {
        matches!(self, Self::Semantic)
    }

    pub fn is_virtual(&self) -> bool {
        matches!(self, Self::Virtual)
    }
}

impl std::fmt::Display for DeliveryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DeliveryMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "semantic" | "" => Ok(Self::Semantic),
            "virtual" => Ok(Self::Virtual),
            other => Err(format!(
                "unknown delivery `{other}` (want semantic or virtual)"
            )),
        }
    }
}

/// Which keymap an Action-id `keybinding` fire must use.
///
/// `focused` is this app’s focused-window map (requires OS focus unless
/// `activate: true`). `global` is **this app’s** global map only — never OS
/// HID into another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeybindingScope {
    Focused,
    Global,
}

impl KeybindingScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Focused => "focused",
            Self::Global => "global",
        }
    }
}

impl std::fmt::Display for KeybindingScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for KeybindingScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "focused" => Ok(Self::Focused),
            "global" => Ok(Self::Global),
            other => Err(format!(
                "unknown keybinding scope `{other}` (want focused or global)"
            )),
        }
    }
}

/// How `screenshot` captures pixels.
///
/// `viewport` (default) is the painted window. `scrolled` scrolls a named
/// target and stitches tiles — it does **not** mean offscreen
/// `render_to_image` or “full content including unloaded virtualized rows”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotMode {
    #[default]
    Viewport,
    Scrolled,
}

impl ScreenshotMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewport => "viewport",
            Self::Scrolled => "scrolled",
        }
    }

    pub fn is_viewport(&self) -> bool {
        matches!(self, Self::Viewport)
    }

    pub fn is_scrolled(&self) -> bool {
        matches!(self, Self::Scrolled)
    }
}

impl std::fmt::Display for ScreenshotMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ScreenshotMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewport" | "" => Ok(Self::Viewport),
            "scrolled" => Ok(Self::Scrolled),
            other => Err(format!(
                "unknown screenshot mode `{other}` (want viewport or scrolled)"
            )),
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Hello,
    Snapshot,
    Click {
        target: String,
        /// Omitted / `semantic` is v1. `virtual` is the in-window event path.
        #[serde(default, skip_serializing_if = "DeliveryMode::is_semantic")]
        delivery: DeliveryMode,
    },
    Type {
        target: String,
        text: String,
        #[serde(default, skip_serializing_if = "DeliveryMode::is_semantic")]
        delivery: DeliveryMode,
    },
    SetValue {
        target: String,
        value: String,
    },
    Key {
        target: String,
        key: String,
        #[serde(default, skip_serializing_if = "DeliveryMode::is_semantic")]
        delivery: DeliveryMode,
    },
    /// Fire a GPUI **Action** by stable id (keymap path). Never OS HID.
    ///
    /// Wire field is `binding` (not `id`) so it does not collide with the
    /// RPC correlation `id`. CLI/MCP `--id` maps here.
    Keybinding {
        binding: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chord: Option<String>,
        scope: KeybindingScope,
        #[serde(default, skip_serializing_if = "is_false")]
        confirm: bool,
        /// Opt-in host GPUI activate for `scope=focused` only. Default false.
        #[serde(default, skip_serializing_if = "is_false")]
        activate: bool,
    },
    /// List registered Action-id bindings: `{ id, chord, scope, dangerous }`.
    ///
    /// Alias `keybinding.list` is accepted on the wire.
    #[serde(alias = "keybinding.list")]
    Keybindings,
    Assert {
        #[serde(flatten)]
        spec: AssertSpec,
    },
    Invoke {
        name: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    Wait {
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Poll [`AssertSpec`] until it succeeds or `timeout_ms` elapses.
    ///
    /// Distinct from [`Op::Wait`], which only polls `hello.ready` / first
    /// paint. Use this for toasts, overlays, and post-keybinding UI.
    WaitUntil {
        timeout_ms: u64,
        #[serde(flatten)]
        spec: AssertSpec,
    },
    Shutdown,
    /// Observe-only PNG of the app surface (not the full desktop).
    ///
    /// The host writes `path` on the same machine so the image does not
    /// ride the 1 MiB NDJSON line. Headless hosts return
    /// `screenshot_unavailable` instead of a fake image. macOS desktop
    /// writes **this window** via `screencapture -l`. Linux/Windows
    /// desktop stays unavailable (no full-desktop capture).
    ///
    /// `mode=scrolled` is opt-in: the host scrolls `target` (stable
    /// scroll-view id), captures tiles, stitches, and restores the
    /// original offset. Default `mode` is `viewport` (unchanged).
    Screenshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "ScreenshotMode::is_viewport")]
        mode: ScreenshotMode,
        /// Required when `mode` is `scrolled`. Ignored for viewport.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_height_px: Option<u32>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssertSpec {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// Host-declared visibility (`UiNode.visible`). Distinct from `exists`
    /// (tree presence) and `in_viewport` (geometry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Non-empty intersection of node bounds with the painted window clip.
    /// Fail closed (`in_viewport_unavailable`) when bounds are zero or the
    /// host is headless — never invent geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_viewport: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hello: Option<HelloInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<UiTree>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Whether the host will enforce `GPUI_AGENT_TOKEN` on every request.
///
/// Filled by the server from its configured token, not by `AgentHost::hello`.
/// `"required"` means a matching token is mandatory; `"none"` means the host
/// will accept unauthenticated requests (`GPUI_AGENT_INSECURE_NO_TOKEN=1`).
/// CLI `recipe run` and `mcp` still require a client token either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloAuth {
    #[default]
    None,
    Required,
}

impl HelloAuth {
    pub fn from_token_configured(token: Option<&str>) -> Self {
        if token.is_some() {
            Self::Required
        } else {
            Self::None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloInfo {
    pub protocol: u32,
    pub app: String,
    pub platform: PlatformKind,
    /// The host's operating system (`std::env::consts::OS`: `macos`, `linux`,
    /// `windows`, …). `platform` says *what kind* of host answered; `os` says
    /// *which machine*. A smoke script that expects a Mac can refuse a Linux
    /// host that happens to be forwarded to the same loopback port.
    #[serde(default = "host_os")]
    pub os: String,
    pub ready: bool,
    /// Input delivery modes this host actually implements.
    /// Headless typically lists only `semantic`. Desktop GPUI lists both.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<DeliveryMode>,
    /// `required` when the host was started with a non-empty token.
    #[serde(default)]
    pub auth: HelloAuth,
}

/// The operating system this process runs on, as `hello.os` reports it.
pub fn host_os() -> String {
    std::env::consts::OS.to_string()
}

impl Request {
    pub fn new(id: impl Into<String>, op: Op) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            token: None,
            auth: None,
            op,
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_auth(mut self, auth: impl Into<String>) -> Self {
        self.auth = Some(auth.into());
        self
    }
}

impl Op {
    pub fn click(target: impl Into<String>) -> Self {
        Self::Click {
            target: target.into(),
            delivery: DeliveryMode::Semantic,
        }
    }

    pub fn click_virtual(target: impl Into<String>) -> Self {
        Self::Click {
            target: target.into(),
            delivery: DeliveryMode::Virtual,
        }
    }

    pub fn type_text(target: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Type {
            target: target.into(),
            text: text.into(),
            delivery: DeliveryMode::Semantic,
        }
    }

    pub fn type_text_virtual(target: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Type {
            target: target.into(),
            text: text.into(),
            delivery: DeliveryMode::Virtual,
        }
    }

    pub fn key(target: impl Into<String>, key: impl Into<String>) -> Self {
        Self::Key {
            target: target.into(),
            key: key.into(),
            delivery: DeliveryMode::Semantic,
        }
    }

    pub fn key_virtual(target: impl Into<String>, key: impl Into<String>) -> Self {
        Self::Key {
            target: target.into(),
            key: key.into(),
            delivery: DeliveryMode::Virtual,
        }
    }

    pub fn keybinding(binding: impl Into<String>, scope: KeybindingScope) -> Self {
        Self::Keybinding {
            binding: binding.into(),
            chord: None,
            scope,
            confirm: false,
            activate: false,
        }
    }

    pub fn is_keybinding(&self) -> bool {
        matches!(self, Self::Keybinding { .. } | Self::Keybindings)
    }

    pub fn screenshot(path: impl Into<String>) -> Self {
        Self::Screenshot {
            path: Some(path.into()),
            mode: ScreenshotMode::Viewport,
            target: None,
            max_height_px: None,
        }
    }

    pub fn screenshot_scrolled(
        path: impl Into<String>,
        target: impl Into<String>,
        max_height_px: Option<u32>,
    ) -> Self {
        Self::Screenshot {
            path: Some(path.into()),
            mode: ScreenshotMode::Scrolled,
            target: Some(target.into()),
            max_height_px,
        }
    }

    pub fn wait_until(spec: AssertSpec, timeout_ms: u64) -> Self {
        Self::WaitUntil { timeout_ms, spec }
    }

    pub fn delivery(&self) -> DeliveryMode {
        match self {
            Self::Click { delivery, .. }
            | Self::Type { delivery, .. }
            | Self::Key { delivery, .. } => *delivery,
            _ => DeliveryMode::Semantic,
        }
    }

    pub fn is_virtual_input(&self) -> bool {
        matches!(
            self,
            Self::Click { .. } | Self::Type { .. } | Self::Key { .. }
        ) && self.delivery().is_virtual()
    }
}

impl Response {
    pub fn ok(id: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            ok: true,
            error: None,
            hello: None,
            tree: None,
            result: None,
        }
    }

    pub fn err(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            ok: false,
            error: Some(error.into()),
            hello: None,
            tree: None,
            result: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_op_roundtrip() {
        let req = Request::new("4", Op::screenshot("artifacts/steps/001-wait.png"));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"], "screenshot");
        assert_eq!(json["path"], "artifacts/steps/001-wait.png");
        assert!(
            json.get("mode").is_none(),
            "default viewport must omit mode so v2 clients stay valid: {json}"
        );
        assert!(json.get("target").is_none());
        assert!(json.get("max_height_px").is_none());
        let back: Request = serde_json::from_value(json).unwrap();
        match back.op {
            Op::Screenshot {
                path,
                mode,
                target,
                max_height_px,
            } => {
                assert_eq!(path.as_deref(), Some("artifacts/steps/001-wait.png"));
                assert_eq!(mode, ScreenshotMode::Viewport);
                assert!(target.is_none());
                assert!(max_height_px.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn screenshot_scrolled_roundtrip() {
        let req = Request::new(
            "5",
            Op::screenshot_scrolled("tall.png", "todo-list-scroll", Some(4096)),
        );
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"], "screenshot");
        assert_eq!(json["mode"], "scrolled");
        assert_eq!(json["target"], "todo-list-scroll");
        assert_eq!(json["max_height_px"], 4096);
        assert!(json.get("stitched").is_none());
        let back: Request = serde_json::from_value(json).unwrap();
        match back.op {
            Op::Screenshot {
                mode,
                target,
                max_height_px,
                ..
            } => {
                assert_eq!(mode, ScreenshotMode::Scrolled);
                assert_eq!(target.as_deref(), Some("todo-list-scroll"));
                assert_eq!(max_height_px, Some(4096));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn screenshot_legacy_json_is_viewport() {
        let req: Request =
            serde_json::from_str(r#"{"v":2,"id":"1","op":"screenshot","path":"a.png"}"#).unwrap();
        match req.op {
            Op::Screenshot { mode, target, .. } => {
                assert_eq!(mode, ScreenshotMode::Viewport);
                assert!(target.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn screenshot_mode_from_str_rejects_overpromises() {
        assert_eq!(
            "scrolled".parse::<ScreenshotMode>().unwrap(),
            ScreenshotMode::Scrolled
        );
        for bad in ["full_content", "stitched", "full"] {
            let err = bad.parse::<ScreenshotMode>().unwrap_err();
            assert!(err.contains("viewport or scrolled"), "{err}");
        }
    }

    #[test]
    fn click_without_delivery_is_semantic() {
        let req: Request =
            serde_json::from_str(r#"{"v":1,"id":"1","op":"click","target":"todo-add"}"#).unwrap();
        assert_eq!(req.op.delivery(), DeliveryMode::Semantic);
        assert!(!req.op.is_virtual_input());
    }

    #[test]
    fn click_virtual_roundtrip() {
        let req = Request::new("2", Op::click_virtual("todo-add"));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["delivery"], "virtual");
        assert_eq!(json["op"], "click");
        let back: Request = serde_json::from_value(json).unwrap();
        assert_eq!(back.op.delivery(), DeliveryMode::Virtual);
    }

    #[test]
    fn semantic_delivery_omitted_from_json() {
        let req = Request::new("3", Op::click("todo-add"));
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("delivery").is_none());
    }

    #[test]
    fn debug_redacts_token() {
        let req = Request::new("1", Op::Hello).with_token("super-secret-token");
        let rendered = format!("{req:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            !rendered.contains("super-secret-token"),
            "token leaked in Debug: {rendered}"
        );
    }

    #[test]
    fn keybinding_op_does_not_clobber_request_id() {
        let req = Request::new(
            "rpc-1",
            Op::Keybinding {
                binding: "app.quit".into(),
                chord: Some("cmd-q".into()),
                scope: KeybindingScope::Global,
                confirm: true,
                activate: false,
            },
        );
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["id"], "rpc-1");
        assert_eq!(json["op"], "keybinding");
        assert_eq!(json["binding"], "app.quit");
        assert_eq!(json["scope"], "global");
        assert_eq!(json["confirm"], true);
        assert!(json.get("activate").is_none());
        let back: Request = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "rpc-1");
        match back.op {
            Op::Keybinding {
                binding,
                chord,
                scope,
                confirm,
                activate,
            } => {
                assert_eq!(binding, "app.quit");
                assert_eq!(chord.as_deref(), Some("cmd-q"));
                assert_eq!(scope, KeybindingScope::Global);
                assert!(confirm);
                assert!(!activate);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keybindings_list_alias_roundtrip() {
        let req = Request::new("2", Op::Keybindings);
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"], "keybindings");
        let back: Request = serde_json::from_value(json).unwrap();
        assert!(matches!(back.op, Op::Keybindings));

        let alias: Request =
            serde_json::from_str(r#"{"v":2,"id":"3","op":"keybinding.list"}"#).unwrap();
        assert!(matches!(alias.op, Op::Keybindings));
        assert_eq!(alias.id, "3");
    }

    #[test]
    fn hello_auth_roundtrip() {
        let hello = HelloInfo {
            os: crate::protocol::host_os(),
            protocol: PROTOCOL_VERSION,
            app: "todo".into(),
            platform: PlatformKind::Headless,
            ready: true,
            deliveries: vec![DeliveryMode::Semantic],
            auth: HelloAuth::Required,
        };
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["auth"], "required");
        let back: HelloInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back.auth, HelloAuth::Required);

        let none: HelloInfo =
            serde_json::from_str(r#"{"protocol":1,"app":"x","platform":"headless","ready":true}"#)
                .unwrap();
        assert_eq!(none.auth, HelloAuth::None);
        assert_eq!(HelloAuth::from_token_configured(None), HelloAuth::None);
        assert_eq!(
            HelloAuth::from_token_configured(Some("s")),
            HelloAuth::Required
        );
    }

    #[test]
    fn wait_until_op_roundtrip() {
        let req = Request::new(
            "7",
            Op::wait_until(
                AssertSpec {
                    target: "todo-nav".into(),
                    visible: Some(true),
                    in_viewport: Some(true),
                    ..Default::default()
                },
                1500,
            ),
        );
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"], "wait_until");
        assert_eq!(json["timeout_ms"], 1500);
        assert_eq!(json["target"], "todo-nav");
        assert_eq!(json["visible"], true);
        assert_eq!(json["in_viewport"], true);
        assert!(json.get("exists").is_none());
        let back: Request = serde_json::from_value(json).unwrap();
        match back.op {
            Op::WaitUntil { timeout_ms, spec } => {
                assert_eq!(timeout_ms, 1500);
                assert_eq!(spec.target, "todo-nav");
                assert_eq!(spec.visible, Some(true));
                assert_eq!(spec.in_viewport, Some(true));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn assert_omitted_visible_fields_are_none() {
        let req: Request =
            serde_json::from_str(r#"{"v":2,"id":"1","op":"assert","target":"todo-add"}"#).unwrap();
        match req.op {
            Op::Assert { spec } => {
                assert_eq!(spec.target, "todo-add");
                assert!(spec.visible.is_none());
                assert!(spec.in_viewport.is_none());
            }
            other => panic!("{other:?}"),
        }
    }
}
