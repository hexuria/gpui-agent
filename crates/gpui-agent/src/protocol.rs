use serde::{Deserialize, Serialize};

use crate::tree::UiTree;

/// Bump when introducing a breaking change to the wire format.
pub const PROTOCOL_VERSION: u32 = 1;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(flatten)]
    pub op: Op,
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
    Shutdown,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloInfo {
    pub protocol: u32,
    pub app: String,
    pub platform: PlatformKind,
    pub ready: bool,
    /// Input delivery modes this host actually implements.
    /// Headless typically lists only `semantic`. Desktop GPUI lists both.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<DeliveryMode>,
}

impl Request {
    pub fn new(id: impl Into<String>, op: Op) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: id.into(),
            token: None,
            op,
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
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
}
