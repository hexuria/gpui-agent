//! Shared instinct-feature wiring between `run` and MCP `locate`/`act`:
//! the recipe-registry → offered-invoke map lives here so both callers
//! describe the same action space to the policy.

use gpui_agent_policy::OfferedInvoke;
use gpui_agent_recipe::{Effect, Registry, SchemaKind};

/// Invoke schemas become the space's `INVOKE:<name>` offers;
/// `description + keywords` is the policy's evidence label, and
/// `Effect::Exit` marks the offer dangerous (listed, never chosen).
pub fn offered_invokes(registry: &Registry) -> Vec<OfferedInvoke> {
    registry
        .iter()
        .filter(|s| s.kind == SchemaKind::Invoke)
        .map(|s| OfferedInvoke {
            name: s.name.clone(),
            description: format!("{} {}", s.description, s.keywords.join(" ")),
            dangerous: s.effects.contains(&Effect::Exit),
        })
        .collect()
}
