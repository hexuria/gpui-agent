//! Recipes for gpui-agent: a JSON (or `.wants`) file of protocol ops that
//! validates, compiles to a plan, and runs in one session with a receipt.
//!
//! Flow: [`parse_recipe`] → [`validate_recipe`] / [`lint_recipe`] →
//! [`compile_plan`] → [`run_plan`] → [`Receipt`]. Everything before
//! `run_plan` fails with a [`RecipeError`] whose
//! [`code`](RecipeError::code) is stable. The stability contract is in
//! `docs/RECIPES.md`.
//!
//! This crate is **client-side**. It does not add a wire `batch` op and it
//! cannot bypass token / loopback / line-connection-mailbox caps. A recipe
//! is a compiled list of ordinary protocol [`Op`](gpui_agent::Op)s run
//! on one reused TCP session (`AgentClient` from P0). All-Read DAG waves
//! may pipeline (write N lines, then read). Write, Exit, mixed, and
//! screenshot-capture waves stay one request at a time so a failed write
//! never continues the wave.

/// Typed errors with stable codes.
pub mod error;
/// Recipe params: bare names or typed entries.
pub mod param;
/// Recipe → scheduled plan compilation (DAG waves, param binding).
pub mod plan;
/// Deterministic `recipe run` receipt types.
pub mod receipt;
/// Recipe file format: parsing, params, validation.
pub mod recipe;
/// Operation schemas: protocol registry and user schema files.
pub mod registry;
/// Intent → wire-op resolution (`resolve_intent`).
pub mod resolve;
/// Plan execution: waves, screenshots, receipts.
pub mod run;
/// Schema types (`OpSchema`, `Effect`, `SchemaKind`).
pub mod schema;

pub use error::{RecipeError, RecipeErrorCode};
pub use param::{Param, ParamType};
pub use plan::{OrderCheck, PLAN_FORMAT_VERSION, Plan, PlannedStep, compile_plan, order_check};
pub use receipt::{RECEIPT_FORMAT_VERSION, Receipt, ScreenshotReceipt, StepReceipt};
pub use recipe::{
    DEFAULT_SCROLL_UNTIL_STEPS, MAX_KEY_DELAY_MS, MAX_RECIPE_STEPS, MAX_SCROLL_UNTIL_STEPS,
    RECIPE_FORMAT_VERSION, Recipe, RecipeStep, apply_params, lint_recipe, parse_recipe,
    parse_recipe_source, parse_wants, reject_duplicate_keys, validate_recipe,
};
pub use registry::{
    Registry, load_schema_file, protocol_registry, registry_from_schema_paths, todo_registry,
};
pub use resolve::{ResolveResult, TokenFill, invoke_args, resolve_intent};
pub use run::{RunError, RunOptions, ScreenshotCapture, run_plan};
pub use schema::{ArgSchema, Effect, OpSchema, SchemaKind};
