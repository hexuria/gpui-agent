//! Finite action space derived from a [`UiTree`] snapshot.
//!
//! Port of `aui-core/src/action_space.rs` (InteractionManifold → ActionSpace)
//! with gpui-agent's role vocabulary and op ids. Invariants preserved:
//! every observation yields a dynamic finite space; policies may only
//! choose offered operations/target ids; hard-invalid nodes (hidden,
//! disabled — inside an invisible or disabled ancestor too) are filtered
//! *before* scoring, never penalized.
//!
//! Op mapping (PRD M4.1): clickable roles → `CLICK:<id>`; textbox →
//! `TYPE:<id>`, `SET_VALUE:<id>` and `KEY:<id>:Enter` (submit); each
//! keybinding row → `KEYBINDING:<action>`; each invoke schema →
//! `INVOKE:<name>`; controls `DONE`, `BLOCKED`, `WAIT` are always offered.

use std::collections::BTreeMap;
use std::fmt;

use gpui_agent::keybinding::{KeybindingInfo, binding_is_dangerous};
use gpui_agent::tree::{UiNode, UiTree, role};

use crate::evidence::OperationKind;

/// Agent-level operation. Distinct from the protocol `Op` it dispatches
/// as — the action space offers, the runner materializes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ActionKind {
    Click,
    Type,
    SetValue,
    /// `KEY:<id>:Enter` — submit on a textbox.
    Key,
    Keybinding,
    Invoke,
    Done,
    Blocked,
    Wait,
}

impl ActionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Click => "CLICK",
            Self::Type => "TYPE",
            Self::SetValue => "SET_VALUE",
            Self::Key => "KEY",
            Self::Keybinding => "KEYBINDING",
            Self::Invoke => "INVOKE",
            Self::Done => "DONE",
            Self::Blocked => "BLOCKED",
            Self::Wait => "WAIT",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "CLICK" | "click" => Self::Click,
            "TYPE" | "type" | "TYPE_TEXT" | "type_text" => Self::Type,
            "SET_VALUE" | "set_value" => Self::SetValue,
            "KEY" | "key" => Self::Key,
            "KEYBINDING" | "keybinding" => Self::Keybinding,
            "INVOKE" | "invoke" => Self::Invoke,
            "DONE" | "done" => Self::Done,
            "BLOCKED" | "blocked" => Self::Blocked,
            "WAIT" | "wait" => Self::Wait,
            _ => return None,
        })
    }

    /// Controls name the app state itself; every other kind binds an
    /// entity (node, binding, schema) picked by the target head.
    pub const fn is_control(self) -> bool {
        matches!(self, Self::Done | Self::Blocked | Self::Wait)
    }

    /// The evidence [`OperationKind`] this kind scores under.
    pub const fn operation_kind(self) -> OperationKind {
        match self {
            Self::Click => OperationKind::Click,
            Self::Type => OperationKind::TypeText,
            Self::SetValue => OperationKind::SetValue,
            Self::Key => OperationKind::Key,
            Self::Keybinding => OperationKind::Keybinding,
            Self::Invoke => OperationKind::Invoke,
            Self::Done => OperationKind::Done,
            Self::Blocked => OperationKind::Blocked,
            Self::Wait => OperationKind::Wait,
        }
    }
}

impl fmt::Display for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable id of one offered action inside an [`ActionSpace`].
///
/// Node-bound forms: `{KIND}:{node_id}` (e.g. `CLICK:settings`),
/// `KEY:{node_id}:Enter`. Entity-bound: `KEYBINDING:{action_id}`,
/// `INVOKE:{schema_name}`. Controls use the kind name alone.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionId(String);

impl ActionId {
    pub fn try_new(raw: impl AsRef<str>) -> Result<Self, ActionSpaceError> {
        let raw = raw.as_ref();
        if raw.is_empty() {
            return Err(ActionSpaceError::EmptyId);
        }
        if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(ActionSpaceError::InvalidId);
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn for_target(kind: ActionKind, node_id: &str) -> Self {
        Self(format!("{}:{}", kind.as_str(), node_id))
    }

    fn for_key(node_id: &str, key: &str) -> Self {
        Self(format!("KEY:{node_id}:{key}"))
    }

    fn for_entity(kind: ActionKind, name: &str) -> Self {
        Self(format!("{}:{}", kind.as_str(), name))
    }

    fn for_control(kind: ActionKind) -> Self {
        Self(kind.as_str().to_owned())
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ActionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A host invoke schema the action space may offer (`INVOKE:<name>`).
/// Constructed from the recipe `Registry` by the caller so this crate
/// stays recipe-free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferedInvoke {
    pub name: String,
    /// Evidence label words — schema description + keywords.
    pub description: String,
    /// `effects` contains `Effect::Exit` (process-ending).
    pub dangerous: bool,
}

/// One offered action: a node target, a named entity (binding/schema),
/// or a terminal control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferedAction {
    id: ActionId,
    kind: ActionKind,
    /// Node id for node-bound kinds; the entity name for
    /// `Keybinding`/`Invoke`; `None` for controls.
    target: Option<String>,
    label: String,
    role: Option<String>,
    /// Destructive actions are listed but never auto-chosen — choosing
    /// one abstains `needs_confirmation` (PRD M4.1).
    dangerous: bool,
    /// `UiNode::fingerprint` of the target at build time (M5). `Some`
    /// only for node-bound kinds; `act` sends it as
    /// `expect_fingerprint` on every mutating dispatch.
    fingerprint: Option<u64>,
}

impl OfferedAction {
    pub fn id(&self) -> &ActionId {
        &self.id
    }
    pub fn kind(&self) -> ActionKind {
        self.kind
    }
    /// The dispatch target: node id, binding id, or schema name.
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }
    pub fn dangerous(&self) -> bool {
        self.dangerous
    }
    /// Build-time `UiNode::fingerprint` for node-bound actions (M5).
    pub fn fingerprint(&self) -> Option<u64> {
        self.fingerprint
    }
}

/// Finite set of operations/targets a policy may choose from for one
/// observation, in deterministic [`ActionId`] order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionSpace {
    rev: Option<u64>,
    actions: BTreeMap<ActionId, OfferedAction>,
    /// Tokenized `name`/`value` of every viable node — quoted-literal
    /// placement evidence for [`crate::evidence::GoalEvidence`].
    texts: Vec<NodeText>,
}

/// Tokenized label, value, and role of one viable node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeText {
    pub label: Vec<String>,
    pub value: Vec<String>,
    pub role: String,
}

/// Construction errors — id validity only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionSpaceError {
    EmptyId,
    InvalidId,
}

impl fmt::Display for ActionSpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => f.write_str("action id is empty"),
            Self::InvalidId => f.write_str("action id contains whitespace or control chars"),
        }
    }
}

impl std::error::Error for ActionSpaceError {}

impl ActionSpace {
    /// Build the offered set from a snapshot plus the app's keybinding
    /// catalog and invoke schemas.
    ///
    /// Hidden or disabled nodes — and every node under a hidden or
    /// disabled ancestor — are skipped before scoring.
    pub fn from_tree(
        tree: &UiTree,
        keybindings: &[KeybindingInfo],
        invokes: &[OfferedInvoke],
    ) -> Self {
        let mut actions = BTreeMap::new();
        let mut texts = Vec::new();
        for node in &tree.nodes {
            collect_node(node, None, true, &mut actions, &mut texts);
        }
        for kb in keybindings {
            // Identifier separators split into words so a goal naming
            // "quit" matches `app.quit` as lexical evidence.
            let label = format!("{} {}", explode_ident(&kb.id), kb.chord);
            offer(
                &mut actions,
                OfferedAction {
                    id: ActionId::for_entity(ActionKind::Keybinding, &kb.id),
                    kind: ActionKind::Keybinding,
                    target: Some(kb.id.clone()),
                    label,
                    role: Some("keybinding".to_owned()),
                    dangerous: binding_is_dangerous(kb),
                    fingerprint: None,
                },
            );
        }
        for invoke in invokes {
            let label = format!("{} {}", explode_ident(&invoke.name), invoke.description);
            offer(
                &mut actions,
                OfferedAction {
                    id: ActionId::for_entity(ActionKind::Invoke, &invoke.name),
                    kind: ActionKind::Invoke,
                    target: Some(invoke.name.clone()),
                    label,
                    role: Some("invoke".to_owned()),
                    dangerous: invoke.dangerous,
                    fingerprint: None,
                },
            );
        }
        for kind in [ActionKind::Done, ActionKind::Blocked, ActionKind::Wait] {
            let label = match kind {
                ActionKind::Done => "Every requirement is satisfied".to_owned(),
                ActionKind::Blocked => "No supported operation can progress".to_owned(),
                ActionKind::Wait => "Wait briefly for the app to settle".to_owned(),
                _ => kind.as_str().to_owned(),
            };
            offer(
                &mut actions,
                OfferedAction {
                    id: ActionId::for_control(kind),
                    kind,
                    target: None,
                    label,
                    role: None,
                    dangerous: false,
                    fingerprint: None,
                },
            );
        }
        // Sorted so `PartialEq` is invariant under sibling order.
        texts.sort();
        Self {
            rev: tree.rev,
            actions,
            texts,
        }
    }

    /// A quoted clause literal is "placed" once its token run appears
    /// inside some visible node's label or value — e.g. "Buy milk"
    /// lands in the draft field's value after TYPE. Until then the
    /// clause still carries an unplaced payload.
    pub fn literal_placed(&self, literal_tokens: &[String]) -> bool {
        !literal_tokens.is_empty()
            && self.texts.iter().any(|text| {
                contains_run(&text.label, literal_tokens)
                    || contains_run(&text.value, literal_tokens)
            })
    }

    /// Stronger than [`literal_placed`]: the literal appears inside a
    /// node's *label* — it names an entity that exists on screen
    /// ("Buy milk" the list item), not merely a field's draft value.
    /// Completion evidence for payload clauses: the add landed.
    pub fn literal_in_label(&self, literal_tokens: &[String]) -> bool {
        !literal_tokens.is_empty()
            && self
                .texts
                .iter()
                .any(|text| contains_run(&text.label, literal_tokens))
    }

    /// A `page`-role node already displays `word`: the "in Settings"
    /// locative names the page the tree is on, so its navigation
    /// evidence is spent — `nav-settings` stops earning the boost
    /// once Settings is shown.
    pub fn location_shown(&self, word: &str) -> bool {
        let needle = crate::evidence::tokenize(word);
        !needle.is_empty()
            && self
                .texts
                .iter()
                .any(|text| text.role == "page" && contains_run(&text.label, &needle))
    }

    /// Snapshot revision the space was built from (`None` when the host
    /// does not report one).
    pub fn rev(&self) -> Option<u64> {
        self.rev
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn get(&self, id: &ActionId) -> Option<&OfferedAction> {
        self.actions.get(id)
    }

    pub fn get_str(&self, id: &str) -> Option<&OfferedAction> {
        let id = ActionId::try_new(id).ok()?;
        self.actions.get(&id)
    }

    pub fn actions(&self) -> impl Iterator<Item = &OfferedAction> {
        self.actions.values()
    }

    pub fn ids(&self) -> impl Iterator<Item = &ActionId> {
        self.actions.keys()
    }

    /// Target-bound actions of one kind, in id order.
    pub fn targets_of(&self, kind: ActionKind) -> impl Iterator<Item = &OfferedAction> {
        self.actions
            .values()
            .filter(move |a| a.kind == kind && a.target.is_some())
    }

    pub fn contains_kind(&self, kind: ActionKind) -> bool {
        self.actions.values().any(|a| a.kind == kind)
    }
}

fn offer(actions: &mut BTreeMap<ActionId, OfferedAction>, action: OfferedAction) {
    actions.entry(action.id.clone()).or_insert(action);
}

/// `app.quit` / `todo-add` / `todo_add` → `app quit` / `todo add` —
/// identifier separators become token boundaries in evidence text.
fn explode_ident(name: &str) -> String {
    name.chars()
        .map(|c| {
            if matches!(c, '.' | '-' | '_' | '/' | ':') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// `hay` holds `needle` as a contiguous token run.
fn contains_run(hay: &[String], needle: &[String]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn collect_node(
    node: &UiNode,
    parent: Option<&UiNode>,
    ancestors_viable: bool,
    actions: &mut BTreeMap<ActionId, OfferedAction>,
    texts: &mut Vec<NodeText>,
) {
    // Filtered before scoring — never penalized.
    let viable = ancestors_viable && node.enabled && node.visible;
    if viable {
        texts.push(NodeText {
            label: crate::evidence::tokenize(&node.name),
            value: node
                .value
                .as_deref()
                .map(crate::evidence::tokenize)
                .unwrap_or_default(),
            role: node.role.clone(),
        });
        let parent_role = parent.map(|p| p.role.as_str());
        let parent_id = parent.map(|p| p.id.as_str());
        let fingerprint = Some(node.fingerprint(parent_id));
        let clickable = matches!(
            node.role.as_str(),
            "button" | "link" | "tab" | "checkbox" | "switch"
        ) || (node.role == role::LIST_ITEM
            && parent_role == Some(role::NAVIGATION));
        if clickable {
            offer(
                actions,
                OfferedAction {
                    id: ActionId::for_target(ActionKind::Click, &node.id),
                    kind: ActionKind::Click,
                    target: Some(node.id.clone()),
                    label: node.name.clone(),
                    role: Some(node.role.clone()),
                    dangerous: false,
                    fingerprint,
                },
            );
        }
        if node.role == role::TEXTBOX {
            for kind in [ActionKind::Type, ActionKind::SetValue] {
                offer(
                    actions,
                    OfferedAction {
                        id: ActionId::for_target(kind, &node.id),
                        kind,
                        target: Some(node.id.clone()),
                        label: node.name.clone(),
                        role: Some(node.role.clone()),
                        dangerous: false,
                        fingerprint,
                    },
                );
            }
            // Textbox submit: Enter is the canonical submit key. The
            // tree carries no per-field submit flag, so it is offered
            // on every textbox — arbitration gates it.
            offer(
                actions,
                OfferedAction {
                    id: ActionId::for_key(&node.id, "Enter"),
                    kind: ActionKind::Key,
                    target: Some(node.id.clone()),
                    label: format!("{} submit", node.name),
                    role: Some(node.role.clone()),
                    dangerous: false,
                    fingerprint,
                },
            );
        }
    }
    for child in &node.children {
        collect_node(child, Some(node), viable, actions, texts);
    }
}
