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

/// `snapshot` response shape. `Json` (default) is the v2 `UiTree`;
/// `Compact` is the one-line-per-visible-node text for token-lean drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFormat {
    Json,
    Compact,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Hello,
    /// Read the semantic tree. `since` asks for a diff against a revision
    /// the client already holds; `format: "compact"` returns one line per
    /// visible node instead of the JSON tree (M3); `fingerprints: true`
    /// adds an `id → UiNode::fingerprint` map (M5). Omitting all three
    /// keeps the v2 response byte-identical.
    Snapshot {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        since: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<SnapshotFormat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprints: Option<bool>,
    },
    /// Cheap probe for `AgentHost::revision` — used by mailbox `wait_until`
    /// so an unchanged tree costs no rebuild. Additive v2 op.
    Revision,
    /// Poll the host's bounded event stream (M9). `since` returns events
    /// with `seq > N` (`null`/omitted → all retained); `limit` defaults
    /// to 256 and is capped at the ring's 1024 capacity. Result:
    /// `{"events":[{seq,level,target,message}…],"latest":N,"lagged":bool}`.
    /// Hosts without a ring fail closed with "events unsupported".
    Events {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        since: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u32"
        )]
        limit: Option<u32>,
    },
    Click {
        target: String,
        /// Omitted / `semantic` is v1. `virtual` is the in-window event path.
        #[serde(default, skip_serializing_if = "DeliveryMode::is_semantic")]
        delivery: DeliveryMode,
        /// M5 staleness gate: the host recomputes
        /// [`crate::tree::UiNode::fingerprint`] on the dispatch path; mismatch →
        /// `stale_target`, missing node → `target_gone`. Nothing
        /// dispatches on a mismatch.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        expect_fingerprint: Option<u64>,
    },
    Type {
        target: String,
        text: String,
        #[serde(default, skip_serializing_if = "DeliveryMode::is_semantic")]
        delivery: DeliveryMode,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        expect_fingerprint: Option<u64>,
    },
    SetValue {
        target: String,
        value: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        expect_fingerprint: Option<u64>,
    },
    Key {
        target: String,
        key: String,
        #[serde(default, skip_serializing_if = "DeliveryMode::is_semantic")]
        delivery: DeliveryMode,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        expect_fingerprint: Option<u64>,
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
        #[serde(default, deserialize_with = "integers::optional_u64")]
        timeout_ms: Option<u64>,
    },
    /// Poll [`AssertSpec`] until it succeeds or `timeout_ms` elapses.
    ///
    /// Distinct from [`Op::Wait`], which only polls `hello.ready` / first
    /// paint. Use this for toasts, overlays, and post-keybinding UI.
    WaitUntil {
        #[serde(deserialize_with = "integers::required_u64")]
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
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u32"
        )]
        max_height_px: Option<u32>,
        /// Return the PNG to the client over the wire instead of only a
        /// host path: the response carries a `blob` handle the client
        /// reads with [`Op::Blob`]. For hosts whose filesystem the client
        /// cannot see (a device, a browser). Feature `screenshot_inline`.
        #[serde(default, skip_serializing_if = "is_false")]
        inline: bool,
    },
    /// Read one chunk of a blob a previous response announced
    /// (`result.blob = {blob, bytes, chunks}`). Each chunk is base64 and
    /// fits the 1 MiB line cap. The host drops the blob after its last
    /// chunk is read.
    Blob {
        blob: String,
        #[serde(deserialize_with = "integers::required_u32")]
        index: u32,
    },
    /// A pointer press (a tap on touch hosts) at a node or a point, as the
    /// platform delivers it: refused when the target is hidden, disabled,
    /// off screen or covered. Feature `press`.
    Press {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        x: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        y: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        expect_fingerprint: Option<u64>,
    },
    /// Scroll a scrollable node, or whatever is under a point, by a delta
    /// in logical points. Feature `scroll`.
    Scroll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        x: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        y: Option<f64>,
        #[serde(default, deserialize_with = "points::required")]
        dx: f64,
        #[serde(default, deserialize_with = "points::required")]
        dy: f64,
    },
    /// Move the pointer over a node or point. Hosts without a pointer
    /// (phones) do not list the `hover` feature.
    Hover {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        x: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        y: Option<f64>,
    },
    /// Drag node `from` onto node `to`, or drag `target` (or the point
    /// `x`,`y`) by `dx`,`dy`. Feature `drag`.
    Drag {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        x: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        y: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        dx: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        dy: Option<f64>,
    },
    /// A one-finger swipe across a node (or the whole surface when
    /// `target` is omitted). `distance` is in logical points; the host
    /// picks a default. Feature `swipe`.
    Swipe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        direction: SwipeDirection,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        distance: Option<f64>,
    },
    /// Press and hold. Feature `long_press`.
    LongPress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        x: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        y: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "integers::optional_u64"
        )]
        duration_ms: Option<u64>,
    },
    /// Two-finger pinch centred on a node: `scale > 1` spreads, `< 1`
    /// pinches in. Feature `pinch`.
    Pinch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(deserialize_with = "points::required")]
        scale: f64,
    },
    /// Resize the app's content area (not the device) to a size in logical
    /// points or to a host-named preset (`phone`, `tablet`, `desktop`).
    /// Feature `viewport`.
    Viewport {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        width: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "points::optional"
        )]
        height: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
    /// Insert text into an editable node as a paste does (one edit, not
    /// key by key). Feature `paste`.
    Paste {
        target: String,
        text: String,
    },
    /// A device or system action outside the app's own tree. Each action
    /// is its own feature: `system.back`, `system.rotate`, …
    System {
        action: SystemAction,
        /// `rotate`: `portrait` or `landscape`. `open_url`: the URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwipeDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SwipeDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

impl std::str::FromStr for SwipeDirection {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            other => Err(format!(
                "unknown direction `{other}` (want up, down, left or right)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemAction {
    Back,
    Home,
    Rotate,
    KeyboardShow,
    KeyboardHide,
    Background,
    Foreground,
    OpenUrl,
}

impl SystemAction {
    pub const ALL: [Self; 8] = [
        Self::Back,
        Self::Home,
        Self::Rotate,
        Self::KeyboardShow,
        Self::KeyboardHide,
        Self::Background,
        Self::Foreground,
        Self::OpenUrl,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Home => "home",
            Self::Rotate => "rotate",
            Self::KeyboardShow => "keyboard_show",
            Self::KeyboardHide => "keyboard_hide",
            Self::Background => "background",
            Self::Foreground => "foreground",
            Self::OpenUrl => "open_url",
        }
    }

    /// The `hello.features` string a host lists when it implements this.
    pub fn feature(self) -> &'static str {
        match self {
            Self::Back => "system.back",
            Self::Home => "system.home",
            Self::Rotate => "system.rotate",
            Self::KeyboardShow => "system.keyboard_show",
            Self::KeyboardHide => "system.keyboard_hide",
            Self::Background => "system.background",
            Self::Foreground => "system.foreground",
            Self::OpenUrl => "system.open_url",
        }
    }
}

impl std::str::FromStr for SystemAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let wanted = s.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == wanted)
            .ok_or_else(|| {
                format!(
                    "unknown system action `{s}` (want back, home, rotate, keyboard_show, \
                     keyboard_hide, background, foreground or open_url)"
                )
            })
    }
}

/// Float fields of [`Op`] are read through `serde_json::Value`.
///
/// `Request` flattens `Op`, so serde buffers the fields before it reads
/// them. A host whose build turns on serde_json's `arbitrary_precision`
/// (any crate in its dependency graph can) buffers a float as a map, and a
/// plain `f64` field then fails with `invalid type: map, expected f64`.
mod points {
    use serde::{Deserialize, Deserializer, de::Error};

    fn finite<E: Error>(value: &serde_json::Value) -> Result<f64, E> {
        value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| E::custom(format!("expected a finite number, got {value}")))
    }

    pub fn required<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        finite(&serde_json::Value::deserialize(deserializer)?)
    }

    pub fn optional<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Null => Ok(None),
            value => finite(&value).map(Some),
        }
    }
}

mod integers {
    use serde::{Deserialize, Deserializer, de::Error};

    // serde_json 1.x with `arbitrary_precision` (which any crate in the
    // build can switch on) hands a number buffered by the internally tagged
    // `Op` to a field as a one-entry map under this key, holding the digits.
    // The key is serde_json's own, not public API; a protocol test feeds
    // this shape for every integer field so a change to it fails there.
    const PRIVATE_NUMBER: &str = "$serde_json::private::Number";

    fn unsigned<E: Error>(value: serde_json::Value) -> Result<u64, E> {
        match value {
            serde_json::Value::Number(number) => number
                .as_u64()
                .ok_or_else(|| E::custom("expected an unsigned integer")),
            serde_json::Value::Object(mut object) if object.len() == 1 => {
                match object.remove(PRIVATE_NUMBER) {
                    Some(serde_json::Value::String(number)) => number
                        .parse()
                        .map_err(|_| E::custom("expected an unsigned integer")),
                    _ => Err(E::custom("expected an unsigned integer")),
                }
            }
            value => Err(E::custom(format!(
                "expected an unsigned integer, got {value}"
            ))),
        }
    }

    pub fn required_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        unsigned(serde_json::Value::deserialize(deserializer)?)
    }

    pub fn optional_u64<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Null => Ok(None),
            value => unsigned(value).map(Some),
        }
    }

    pub fn required_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        u32::try_from(required_u64(deserializer)?)
            .map_err(|_| D::Error::custom("unsigned integer exceeds u32"))
    }

    pub fn optional_u32<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u32>, D::Error> {
        optional_u64(deserializer)?
            .map(|value| {
                u32::try_from(value).map_err(|_| D::Error::custom("unsigned integer exceeds u32"))
            })
            .transpose()
    }
}

/// Error prefix for an op the answering host does not implement
/// (`unsupported_op: <feature>`). Stable: match the prefix, not the wording.
pub const UNSUPPORTED_OP: &str = "unsupported_op";

pub fn unsupported_op(feature: &str) -> String {
    format!("{UNSUPPORTED_OP}: this host does not list `{feature}` in hello.features")
}

pub fn is_unsupported_op(error: &str) -> bool {
    error.starts_with(UNSUPPORTED_OP)
}

/// What the app is running on, beyond `platform` and `os`. Every field is
/// optional: a host fills what it knows and never invents the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Device or browser model (`Pixel 8`, `iPhone 16 Pro`, `Chrome 131`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `phone`, `tablet`, `desktop` or `browser`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_factor: Option<String>,
    /// Physical pixels per logical point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Content size in logical points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// `portrait` or `landscape`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    /// Insets `[top, right, bottom, left]` in logical points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_area: Option<[f64; 4]>,
    /// `trusted` when input reaches the app as the platform's own does,
    /// `synthetic` when the host can only simulate it (an attached browser tab).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
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
    /// `id → UiNode::fingerprint` map — present only when the request
    /// was `snapshot {fingerprints:true}` (M5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprints: Option<std::collections::BTreeMap<String, u64>>,
    /// Host snapshot revision when the host tracks one (M3). `None` means
    /// "do not cache": every snapshot may differ even at the same rev.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    /// Stable code for a failure ([`crate::error_code`]). The shared
    /// server fills it from `error`; the client fills it for older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Fixed recovery hint for `error_code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Client-side: whether an action reached the app. Hosts never send
    /// it; the client sets it on actions (distinct from `result.delivery`,
    /// which names the `semantic`/`virtual` path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<crate::error_code::DeliveryStatus>,
    /// Client-side: the target as a snapshot saw it after the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<ReadBack>,
}

/// Target state read back after an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadBack {
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The target's state differs from the snapshot taken before the
    /// action (or it left the tree).
    pub changed: bool,
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
    /// macOS bundle id when the host is inside a `.app`, or `GPUI_AGENT_CONNECT_ID`.
    /// Empty on hosts that have neither. Clients ignore this when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// Executable basename (`current_exe`), so `--connect <name>` can match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Feature strings this responder supports (e.g. `rev`,
    /// `snapshot_since`, `snapshot_compact`). Empty/old hosts omit it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// UI framework behind the host (`gpui`, `nsh`). Old hosts omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceInfo>,
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
    pub fn snapshot() -> Self {
        Self::Snapshot {
            since: None,
            format: None,
            fingerprints: None,
        }
    }

    pub fn snapshot_since(rev: u64, format: Option<SnapshotFormat>) -> Self {
        Self::Snapshot {
            since: Some(rev),
            format,
            fingerprints: None,
        }
    }

    pub fn click(target: impl Into<String>) -> Self {
        Self::Click {
            target: target.into(),
            delivery: DeliveryMode::Semantic,
            expect_fingerprint: None,
        }
    }

    pub fn click_virtual(target: impl Into<String>) -> Self {
        Self::Click {
            target: target.into(),
            delivery: DeliveryMode::Virtual,
            expect_fingerprint: None,
        }
    }

    pub fn type_text(target: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Type {
            target: target.into(),
            text: text.into(),
            delivery: DeliveryMode::Semantic,
            expect_fingerprint: None,
        }
    }

    pub fn type_text_virtual(target: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Type {
            target: target.into(),
            text: text.into(),
            delivery: DeliveryMode::Virtual,
            expect_fingerprint: None,
        }
    }

    pub fn key(target: impl Into<String>, key: impl Into<String>) -> Self {
        Self::Key {
            target: target.into(),
            key: key.into(),
            delivery: DeliveryMode::Semantic,
            expect_fingerprint: None,
        }
    }

    pub fn key_virtual(target: impl Into<String>, key: impl Into<String>) -> Self {
        Self::Key {
            target: target.into(),
            key: key.into(),
            delivery: DeliveryMode::Virtual,
            expect_fingerprint: None,
        }
    }

    /// Attach the M5 `expect_fingerprint` gate to a mutating op
    /// (`click`/`press`/`type`/`set_value`/`key`); a no-op for other variants.
    pub fn with_fingerprint(self, fingerprint: u64) -> Self {
        match self {
            Self::Click {
                target, delivery, ..
            } => Self::Click {
                target,
                delivery,
                expect_fingerprint: Some(fingerprint),
            },
            Self::Press { target, x, y, .. } => Self::Press {
                target,
                x,
                y,
                expect_fingerprint: Some(fingerprint),
            },
            Self::Type {
                target,
                text,
                delivery,
                ..
            } => Self::Type {
                target,
                text,
                delivery,
                expect_fingerprint: Some(fingerprint),
            },
            Self::SetValue { target, value, .. } => Self::SetValue {
                target,
                value,
                expect_fingerprint: Some(fingerprint),
            },
            Self::Key {
                target,
                key,
                delivery,
                ..
            } => Self::Key {
                target,
                key,
                delivery,
                expect_fingerprint: Some(fingerprint),
            },
            other => other,
        }
    }

    /// `(target, expected)` when this mutating op carries an M5
    /// `expect_fingerprint` gate; `None` for unguarded ops.
    pub fn fingerprint_gate(&self) -> Option<(&str, u64)> {
        match self {
            Self::Click {
                target,
                expect_fingerprint: Some(fp),
                ..
            }
            | Self::Type {
                target,
                expect_fingerprint: Some(fp),
                ..
            }
            | Self::SetValue {
                target,
                expect_fingerprint: Some(fp),
                ..
            }
            | Self::Key {
                target,
                expect_fingerprint: Some(fp),
                ..
            } => Some((target.as_str(), *fp)),
            Self::Press {
                target: Some(target),
                expect_fingerprint: Some(fp),
                ..
            } => Some((target.as_str(), *fp)),
            _ => None,
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
            inline: false,
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
            inline: false,
        }
    }

    /// The text fields of the ops added after the first protocol 2
    /// release, for callers that rewrite them (recipe `$param`s). Empty
    /// for the original ops, which their callers visit field by field.
    pub fn added_strings_mut(&mut self) -> Vec<&mut String> {
        let mut out = Vec::new();
        match self {
            Self::Press { target, .. }
            | Self::Scroll { target, .. }
            | Self::Hover { target, .. }
            | Self::Swipe { target, .. }
            | Self::LongPress { target, .. }
            | Self::Pinch { target, .. } => out.extend(target.as_mut()),
            Self::Drag {
                from, to, target, ..
            } => {
                out.extend(from.as_mut());
                out.extend(to.as_mut());
                out.extend(target.as_mut());
            }
            Self::Viewport { preset, .. } => out.extend(preset.as_mut()),
            Self::Paste { target, text } => {
                out.push(target);
                out.push(text);
            }
            Self::System { value, .. } => out.extend(value.as_mut()),
            Self::Blob { blob, .. } => out.push(blob),
            _ => {}
        }
        out
    }

    /// The `hello.features` string that gates this op, for ops added after
    /// the first protocol 2 release. `None` for the original ops.
    pub fn feature(&self) -> Option<&'static str> {
        Some(match self {
            Self::Screenshot { inline: true, .. } => "screenshot_inline",
            Self::Press { .. } => "press",
            Self::Scroll { .. } => "scroll",
            Self::Hover { .. } => "hover",
            Self::Drag { .. } => "drag",
            Self::Swipe { .. } => "swipe",
            Self::LongPress { .. } => "long_press",
            Self::Pinch { .. } => "pinch",
            Self::Viewport { .. } => "viewport",
            Self::Paste { .. } => "paste",
            Self::System { action, .. } => action.feature(),
            _ => return None,
        })
    }

    /// The `invoke` spelling hosts built against gpui-agent 0.1 (the nsh
    /// adapter) used for this op, so a new client can still drive them.
    pub fn legacy_invoke(&self) -> Option<Self> {
        fn args(pairs: &[(&str, Option<serde_json::Value>)]) -> serde_json::Value {
            serde_json::Value::Object(
                pairs
                    .iter()
                    .filter_map(|(key, value)| Some(((*key).to_string(), value.clone()?)))
                    .collect(),
            )
        }
        let text = |value: &Option<String>| value.clone().map(serde_json::Value::from);
        let num = |value: &Option<f64>| value.map(serde_json::Value::from);
        let (name, args) = match self {
            // A 0.1 host has no fingerprint gate on `invoke`: a guarded
            // press is refused rather than sent without its guard.
            Self::Press {
                expect_fingerprint: Some(_),
                ..
            } => return None,
            Self::Press { target, x, y, .. } => (
                "press",
                args(&[("target", text(target)), ("x", num(x)), ("y", num(y))]),
            ),
            Self::Scroll {
                target,
                x,
                y,
                dx,
                dy,
            } => (
                "scroll",
                args(&[
                    ("target", text(target)),
                    ("x", num(x)),
                    ("y", num(y)),
                    ("dx", Some((*dx).into())),
                    ("dy", Some((*dy).into())),
                ]),
            ),
            Self::Hover { target, x, y } => (
                "hover",
                args(&[("target", text(target)), ("x", num(x)), ("y", num(y))]),
            ),
            Self::Drag {
                from,
                to,
                target,
                x,
                y,
                dx,
                dy,
            } => (
                "drag",
                args(&[
                    ("from", text(from)),
                    ("to", text(to)),
                    ("target", text(target)),
                    ("x", num(x)),
                    ("y", num(y)),
                    ("dx", num(dx)),
                    ("dy", num(dy)),
                ]),
            ),
            Self::Viewport {
                width,
                height,
                preset,
            } => (
                "viewport",
                args(&[
                    ("width", num(width)),
                    ("height", num(height)),
                    ("preset", text(preset)),
                ]),
            ),
            _ => return None,
        };
        Some(Self::Invoke {
            name: name.into(),
            args,
        })
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

    /// `true` for ops that change the app, so a reply to them carries a
    /// `delivery_status`. Reads (snapshot, assert, wait, …) never do.
    pub fn acts(&self) -> bool {
        matches!(
            self,
            Self::Click { .. }
                | Self::Type { .. }
                | Self::SetValue { .. }
                | Self::Key { .. }
                | Self::Keybinding { .. }
                | Self::Invoke { .. }
                | Self::Shutdown
                | Self::Press { .. }
                | Self::Scroll { .. }
                | Self::Hover { .. }
                | Self::Drag { .. }
                | Self::Swipe { .. }
                | Self::LongPress { .. }
                | Self::Pinch { .. }
                | Self::Viewport { .. }
                | Self::Paste { .. }
                | Self::System { .. }
        )
    }

    /// The node a read-back checks after this action: the target of
    /// `click`, `type`, `set_value`, `key`, `paste`, and of `press` and
    /// `long_press` when they name one.
    pub fn read_back_target(&self) -> Option<&str> {
        match self {
            Self::Click { target, .. }
            | Self::Type { target, .. }
            | Self::SetValue { target, .. }
            | Self::Key { target, .. }
            | Self::Paste { target, .. } => Some(target),
            Self::Press { target, .. } | Self::LongPress { target, .. } => target.as_deref(),
            _ => None,
        }
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
            fingerprints: None,
            rev: None,
            error_code: None,
            hint: None,
            delivery_status: None,
            after: None,
        }
    }

    pub fn err(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::ok(id)
        }
    }

    /// An error response that already carries its `error_code` and `hint`.
    pub fn err_coded(id: impl Into<String>, error: impl Into<String>) -> Self {
        let mut resp = Self::err(id, error);
        resp.fill_error_code();
        resp
    }

    /// Set `error_code` (and `hint`) from `error` on a failed response that
    /// has none. Used by the server before writing and by the client on
    /// replies from hosts that predate the field.
    pub fn fill_error_code(&mut self) {
        self.fill_error_code_with(crate::error_code::error_code_of);
    }

    /// [`fill_error_code`](Self::fill_error_code) for a reply a host sent,
    /// which no transport-only code can describe. A transport-only code the
    /// host sent itself (a 0.3.0 host classifies its own text with the
    /// general table) becomes `host_error`, and its hint goes with it: the
    /// `connect_failed` hint says a retry is safe, which a request that
    /// reached the host cannot promise.
    pub(crate) fn fill_host_reply_code(&mut self) {
        let sent = self.error_code.as_deref();
        if sent.is_some_and(|code| crate::error_code::host_reply_code(code) != code) {
            let code = crate::error_code::HOST_ERROR;
            self.error_code = Some(code.to_string());
            self.hint = crate::error_code::hint_for(code).map(str::to_string);
        }
        self.fill_error_code_with(crate::error_code::host_reply_code_of);
    }

    fn fill_error_code_with(&mut self, classify: fn(&str) -> &'static str) {
        if self.ok || self.error_code.is_some() {
            return;
        }
        let code = classify(self.error.as_deref().unwrap_or(""));
        self.error_code = Some(code.to_string());
        if self.hint.is_none() {
            self.hint = crate::error_code::hint_for(code).map(str::to_string);
        }
    }

    /// The failure's code: `error_code` when sent, else classified from
    /// `error`. `None` on success.
    pub fn code(&self) -> Option<&str> {
        if self.ok {
            return None;
        }
        Some(self.error_code.as_deref().unwrap_or_else(|| {
            crate::error_code::error_code_of(self.error.as_deref().unwrap_or(""))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattened_op_integers_accept_private_number_maps_and_plain_values() {
        let cases = [
            (
                Op::Snapshot {
                    since: None,
                    format: None,
                    fingerprints: None,
                },
                "since",
            ),
            (
                Op::Events {
                    since: None,
                    limit: None,
                },
                "since",
            ),
            (
                Op::Events {
                    since: None,
                    limit: None,
                },
                "limit",
            ),
            (
                Op::Click {
                    target: "x".into(),
                    delivery: DeliveryMode::Semantic,
                    expect_fingerprint: None,
                },
                "expect_fingerprint",
            ),
            (
                Op::Type {
                    target: "x".into(),
                    text: "y".into(),
                    delivery: DeliveryMode::Semantic,
                    expect_fingerprint: None,
                },
                "expect_fingerprint",
            ),
            (
                Op::SetValue {
                    target: "x".into(),
                    value: "y".into(),
                    expect_fingerprint: None,
                },
                "expect_fingerprint",
            ),
            (
                Op::Key {
                    target: "x".into(),
                    key: "Enter".into(),
                    delivery: DeliveryMode::Semantic,
                    expect_fingerprint: None,
                },
                "expect_fingerprint",
            ),
            (
                Op::Press {
                    target: Some("x".into()),
                    x: None,
                    y: None,
                    expect_fingerprint: None,
                },
                "expect_fingerprint",
            ),
            (Op::Wait { timeout_ms: None }, "timeout_ms"),
            (
                Op::wait_until(
                    AssertSpec {
                        target: "x".into(),
                        ..AssertSpec::default()
                    },
                    0,
                ),
                "timeout_ms",
            ),
            (
                Op::Screenshot {
                    path: None,
                    mode: ScreenshotMode::Viewport,
                    target: None,
                    max_height_px: None,
                    inline: false,
                },
                "max_height_px",
            ),
            (
                Op::Blob {
                    blob: "b".into(),
                    index: 0,
                },
                "index",
            ),
            (
                Op::LongPress {
                    target: None,
                    x: None,
                    y: None,
                    duration_ms: None,
                },
                "duration_ms",
            ),
        ];

        for (op, field) in cases {
            let request = Request::new("1", op);
            for number in [
                serde_json::json!({"$serde_json::private::Number": "123"}),
                serde_json::json!(123),
            ] {
                let mut value = serde_json::to_value(&request).unwrap();
                value[field] = number;
                let decoded: Request = serde_json::from_value(value).unwrap();
                assert_eq!(serde_json::to_value(decoded).unwrap()[field], 123);
            }
        }
    }

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
                inline: _,
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
            framework: None,
            device: None,
            app: "todo".into(),
            platform: PlatformKind::Headless,
            ready: true,
            deliveries: vec![DeliveryMode::Semantic],
            auth: HelloAuth::Required,
            bundle_id: None,
            executable: None,
            features: vec![],
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
