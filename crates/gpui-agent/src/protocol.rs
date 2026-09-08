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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Hello,
    Snapshot,
    Click {
        target: String,
    },
    Type {
        target: String,
        text: String,
    },
    SetValue {
        target: String,
        value: String,
    },
    Key {
        target: String,
        key: String,
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
