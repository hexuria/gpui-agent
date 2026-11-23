use gpui_agent::DeliveryStatus;
use serde::{Deserialize, Serialize};

/// Version of the [`Receipt`] JSON shape (`v`).
pub const RECEIPT_FORMAT_VERSION: u32 = 1;

fn receipt_v1() -> u32 {
    RECEIPT_FORMAT_VERSION
}

/// Whole-recipe run outcome — deterministic payload for `recipe run`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Receipt {
    /// [`RECEIPT_FORMAT_VERSION`].
    #[serde(default = "receipt_v1")]
    pub v: u32,
    /// Every step succeeded.
    pub ok: bool,
    /// Recipe name from the plan.
    pub recipe: String,
    /// Plan fingerprint for cache correlation.
    pub fingerprint: String,
    /// True when the client held a live session at run end — not proof
    /// one connection served every step (it may have reconnected).
    pub session_reused: bool,
    /// Per-step results in plan order.
    pub steps: Vec<StepReceipt>,
    /// Screenshots captured by `--screenshot-dir` / flagged steps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<ScreenshotReceipt>,
    /// Wall-clock run time.
    pub elapsed_ms: u64,
}

/// One executed step: result, error envelope, and timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StepReceipt {
    /// Step id from the recipe.
    pub id: String,
    /// The step's op reported `ok`.
    pub ok: bool,
    /// Host error text when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Stable code of a failed step ([`gpui_agent::error_code`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Whether a step that acts reached the app; absent on reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<DeliveryStatus>,
    /// Step wall-clock time.
    pub elapsed_ms: u64,
    /// The op's result payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Captured screenshot info when the step requested one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotReceipt>,
}

/// Path the host was asked to write. `ok` is false on
/// `screenshot_unavailable` (no fake PNG is created).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScreenshotReceipt {
    /// Requested output path.
    pub path: String,
    /// The PNG was written.
    pub ok: bool,
    /// Host error text when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
