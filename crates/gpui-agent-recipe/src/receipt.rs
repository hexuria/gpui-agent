use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub ok: bool,
    pub recipe: String,
    pub fingerprint: String,
    pub session_reused: bool,
    pub steps: Vec<StepReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<ScreenshotReceipt>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReceipt {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotReceipt>,
}

/// Path the host was asked to write. `ok` is false on
/// `screenshot_unavailable` (no fake PNG is created).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotReceipt {
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
