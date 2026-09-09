//! Experimental recipes + TMP-inspired mapping for gpui-agent.
//!
//! This crate is **client-side**. It does not add a wire `batch` op and it
//! cannot bypass token / loopback / line-connection-mailbox caps. A recipe
//! is a compiled list of ordinary protocol [`Op`](gpui_agent::Op)s run
//! sequentially on one reused TCP session (`AgentClient` from P0).
//! Independent DAG waves are documented on the plan; execution is still
//! one request at a time so a failed step never continues the wave.

pub mod plan;
pub mod receipt;
pub mod recipe;
pub mod registry;
pub mod resolve;
pub mod run;
pub mod schema;

pub use plan::{OrderCheck, Plan, PlannedStep, compile_plan, order_check};
pub use receipt::{Receipt, ScreenshotReceipt, StepReceipt};
pub use recipe::{
    MAX_RECIPE_STEPS, RECIPE_FORMAT_VERSION, Recipe, RecipeStep, apply_params, parse_recipe,
    parse_recipe_source, parse_wants, validate_recipe,
};
pub use registry::{Registry, todo_registry};
pub use resolve::{ResolveResult, TokenFill, resolve_intent};
pub use run::{RunError, ScreenshotCapture, run_plan, run_plan_with_screenshots};
pub use schema::{ArgSchema, Effect, OpSchema, SchemaKind};
