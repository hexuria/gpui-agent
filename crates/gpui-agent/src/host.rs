use crate::DispatchResult;
use crate::protocol::{HelloInfo, Op};
use crate::tree::UiTree;

/// A host is the platform-specific half of the control plane.
///
/// Desktop GPUI Kit apps, the headless test binary, and (later) a WASM or
/// mobile shell all implement this trait and speak the same protocol.
pub trait AgentHost: Send {
    fn hello(&self) -> HelloInfo;
    fn snapshot(&self) -> UiTree;
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String>;
}
