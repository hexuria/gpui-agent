//! M4.6: action-space goldens, T08/T09/T10/T14 semantics,
//! determinism/permutation, and text-resolver pins.
use gpui_agent::keybinding::KeybindingInfo;
use gpui_agent::protocol::{KeybindingScope, PlatformKind};
use gpui_agent::tree::{Bounds, UiNode, UiTree};
use gpui_agent_policy::{
    ActionSpace, DeterministicTextResolver, InstinctPolicy, OfferedInvoke, PolicyOutcome,
    TextContext, TextError, TextResolver, UiGoal, UiPolicy,
};

fn tree(nodes: Vec<UiNode>) -> UiTree {
    UiTree {
        app: "todo".to_owned(),
        platform: PlatformKind::Headless,
        ready: true,
        nodes,
        rev: Some(1),
    }
}

fn boxed(mut n: UiNode) -> UiNode {
    n.bounds = Bounds {
        x: 0.0,
        y: 0.0,
        w: 40.0,
        h: 20.0,
    };
    n
}

/// Mirrors `todo-core`'s row shape: `listitem(todo-item-N, title)`
/// holding `checkbox(todo-toggle-N, title)` + `button(todo-delete-N,
/// "Delete {title}")`.
fn todo_row(n: usize, title: &str) -> UiNode {
    UiNode::listitem(format!("todo-item-{n}"), title)
        .with_checked(false)
        .with_children(vec![
            boxed(UiNode::checkbox(format!("todo-toggle-{n}"), title)),
            boxed(UiNode::button(
                format!("todo-delete-{n}"),
                format!("Delete {title}"),
            )),
        ])
}

fn todo_tree(n: usize) -> UiTree {
    let rows: Vec<UiNode> = (1..=n).map(|i| todo_row(i, &format!("task {i}"))).collect();
    tree(vec![
        UiNode::window("todo-window", "Agent Todo").with_children(vec![
            boxed(UiNode::button("nav-toggle-sidebar", "Hide sidebar")),
            UiNode::navigation("todo-nav", "Todo").with_children(vec![
                boxed(UiNode::button("nav-todos", "Todos")),
                boxed(UiNode::button("nav-settings", "Settings")),
            ]),
            UiNode::page("page-todos", "Todos").with_children(vec![
                boxed(UiNode::textbox("todo-input", "What needs doing?")),
                boxed(UiNode::button("todo-add", "Add")),
                UiNode::scroll("todo-list-scroll", "Todo list")
                    .with_child(UiNode::list("todo-list", "Todos").with_children(rows)),
            ]),
        ]),
    ])
}

fn space(tree: &UiTree) -> ActionSpace {
    ActionSpace::from_tree(tree, &[], &[])
}

fn decide(space: &ActionSpace, goal: &str) -> PolicyOutcome {
    InstinctPolicy::default()
        .decide(space, &UiGoal::new(goal), &[])
        .expect("decide")
}

#[test]
fn action_space_maps_todo_tree() {
    let space = space(&todo_tree(3));
    let ids: Vec<String> = space.ids().map(|id| id.as_str().to_owned()).collect();
    // nav listitems → CLICK; textbox → TYPE + SET_VALUE + KEY:Enter;
    // button → CLICK; each row: checkbox + delete button.
    for expected in [
        "CLICK:nav-toggle-sidebar",
        "CLICK:nav-todos",
        "CLICK:nav-settings",
        "CLICK:todo-add",
        "TYPE:todo-input",
        "SET_VALUE:todo-input",
        "KEY:todo-input:Enter",
        "CLICK:todo-toggle-1",
        "CLICK:todo-delete-1",
        "CLICK:todo-toggle-3",
        "CLICK:todo-delete-3",
        "DONE",
        "BLOCKED",
        "WAIT",
    ] {
        assert!(ids.contains(&expected.to_owned()), "missing {expected}");
    }
    // Rows are listitems under `list`, not `navigation` — no CLICK.
    assert!(!ids.contains(&"CLICK:todo-item-1".to_owned()));
    // scroll/list containers offer nothing.
    assert!(!ids.iter().any(|id| id.contains("todo-list")));
}

#[test]
fn action_space_200_rows_bounded() {
    let space = space(&todo_tree(200));
    // 3 controls + 4 nav/add clicks + 3 input ops + 2*200 row ops.
    assert_eq!(space.len(), 3 + 4 + 3 + 400);
    let out = decide(&space, "Delete task 150");
    match out {
        PolicyOutcome::Choice(d) => {
            assert_eq!(d.action_id.as_str(), "CLICK:todo-delete-150");
        }
        PolicyOutcome::Abstain { reason, .. } => panic!("abstained: {reason}"),
    }
}

#[test]
fn hidden_and_disabled_filtered_before_scoring() {
    let t = tree(vec![UiNode::window("win", "W").with_children(vec![
        UiNode::page("page", "P").with_children(vec![
            boxed(UiNode::button("hidden-btn", "Ghost").with_visible(false)),
            boxed(UiNode::button("disabled-btn", "Grey").with_enabled(false)),
            UiNode::page("closed", "Closed")
                .with_visible_deep(false)
                .with_child(boxed(UiNode::button("inner", "Inner"))),
            boxed(UiNode::button("real", "Real")),
        ]),
    ])]);
    let space = space(&t);
    for gone in ["CLICK:hidden-btn", "CLICK:disabled-btn", "CLICK:inner"] {
        assert!(space.get_str(gone).is_none(), "{gone} must not be offered");
    }
    assert!(space.get_str("CLICK:real").is_some());
}

#[test]
fn keybindings_and_invokes_offered_with_danger_marks() {
    let kbs = vec![
        KeybindingInfo::new("todo.new", "ctrl+n", KeybindingScope::Global, false),
        KeybindingInfo::new("app.quit", "cmd+q", KeybindingScope::Global, true),
    ];
    let invokes = vec![
        OfferedInvoke {
            name: "todo.add".to_owned(),
            description: "Add a todo".to_owned(),
            dangerous: false,
        },
        OfferedInvoke {
            name: "app.exit".to_owned(),
            description: "Quit the app".to_owned(),
            dangerous: true,
        },
    ];
    let space = ActionSpace::from_tree(&todo_tree(1), &kbs, &invokes);
    let quit = space.get_str("KEYBINDING:app.quit").expect("quit offered");
    assert!(quit.dangerous());
    assert!(!space.get_str("KEYBINDING:todo.new").unwrap().dangerous());
    let exit = space.get_str("INVOKE:app.exit").expect("exit offered");
    assert!(exit.dangerous());
}

// --- T-case semantics ---------------------------------------------------

/// T08: an intent with no matching target must abstain, never guess.
#[test]
fn t08_unknown_target_abstains() {
    let space = space(&todo_tree(2));
    match decide(&space, "click the settings gear") {
        PolicyOutcome::Abstain { reason, .. } => {
            assert!(!reason.is_empty());
        }
        PolicyOutcome::Choice(d) => {
            panic!("must abstain, chose {} ({})", d.action_id, d.target_label)
        }
    }
}

/// T09: two identical-named targets — ambiguous, abstain.
#[test]
fn t09_duplicate_targets_abstain() {
    let t = tree(vec![UiNode::window("win", "W").with_child(
        UiNode::page("page", "P").with_children(vec![
            boxed(UiNode::button("btn-a", "OK")),
            boxed(UiNode::button("btn-b", "OK")),
        ]),
    )]);
    match decide(&space(&t), "click OK") {
        PolicyOutcome::Abstain { reason, .. } => assert!(!reason.is_empty()),
        PolicyOutcome::Choice(d) => {
            panic!("must abstain on twins, chose {}", d.action_id)
        }
    }
}

/// T10: exact label beats near-miss sibling.
#[test]
fn t10_exact_target_wins() {
    let t = tree(vec![UiNode::window("win", "W").with_child(
        UiNode::page("page", "P").with_children(vec![
            boxed(UiNode::button("btn-save", "Save")),
            boxed(UiNode::button("btn-save-all", "Save All")),
        ]),
    )]);
    match decide(&space(&t), "click Save") {
        PolicyOutcome::Choice(d) => assert_eq!(d.action_id.as_str(), "CLICK:btn-save"),
        PolicyOutcome::Abstain { reason, .. } => panic!("abstained: {reason}"),
    }
}

/// T14: destructive action is never auto-chosen; the policy abstains
/// `needs_confirmation` instead.
#[test]
fn t14_dangerous_needs_confirmation() {
    let kbs = vec![KeybindingInfo::new(
        "app.quit",
        "cmd+q",
        KeybindingScope::Global,
        true,
    )];
    let space = ActionSpace::from_tree(&todo_tree(1), &kbs, &[]);
    // "keybinding" names the operation; "quit" picks the app.quit
    // target — which is destructive, so the policy must abstain
    // `needs_confirmation` instead of choosing it. (A "press" verb
    // would also name CLICK and abstain at the operation head —
    // likewise fail-closed.)
    match decide(&space, "app quit keybinding") {
        PolicyOutcome::Abstain { reason, .. } => {
            assert_eq!(reason, "needs_confirmation", "reason was {reason}");
        }
        PolicyOutcome::Choice(d) => {
            let action = space.get(&d.action_id).expect("offered");
            assert!(!action.dangerous(), "auto-chose dangerous {}", d.action_id);
        }
    }
}

/// Permutation: child order must not change the offered set or the
/// decision.
#[test]
fn permutation_invariant() {
    let mut children: Vec<UiNode> = (1..=6).map(|i| todo_row(i, &format!("task {i}"))).collect();
    let space_a = ActionSpace::from_tree(
        &tree(vec![UiNode::window("win", "W").with_child(
            UiNode::page("page", "P").with_children(children.clone()),
        )]),
        &[],
        &[],
    );
    children.reverse();
    let space_b = ActionSpace::from_tree(
        &tree(vec![UiNode::window("win", "W").with_child(
            UiNode::page("page", "P").with_children(children),
        )]),
        &[],
        &[],
    );
    assert_eq!(space_a, space_b);
    let a = decide(&space_a, "delete task 3");
    let b = decide(&space_b, "delete task 3");
    assert_eq!(a, b);
}

#[test]
fn decide_repeated_same_input_is_deterministic() {
    let space = space(&todo_tree(5));
    let first = decide(&space, "type task into New todo");
    let second = decide(&space, "type task into New todo");
    assert_eq!(first, second);
}

// --- Text resolver -------------------------------------------------------

#[test]
fn text_exact_quoted_literal() {
    let mut resolver = DeterministicTextResolver;
    let ctx = TextContext {
        goal: UiGoal::new(r#"type "buy milk" into New todo"#),
        field_label: "New todo".to_owned(),
        field_role: "textbox".to_owned(),
        typed: Vec::new(),
        context_fingerprint: 7,
    };
    let r = resolver.resolve(&ctx).expect("resolves");
    assert_eq!(r.text, "buy milk");
}

#[test]
fn text_no_quote_is_missing() {
    let mut resolver = DeterministicTextResolver;
    let ctx = TextContext {
        goal: UiGoal::new("click Save"),
        field_label: "Save".to_owned(),
        field_role: "button".to_owned(),
        typed: Vec::new(),
        context_fingerprint: 0,
    };
    assert!(matches!(resolver.resolve(&ctx), Err(TextError::Missing)));
}

#[test]
fn text_already_typed_abstains_not_retypes() {
    let mut resolver = DeterministicTextResolver;
    let ctx = TextContext {
        goal: UiGoal::new(r#"type "milk" into New todo"#),
        field_label: "New todo".to_owned(),
        field_role: "textbox".to_owned(),
        typed: vec!["milk".to_owned()],
        context_fingerprint: 0,
    };
    assert!(matches!(resolver.resolve(&ctx), Err(TextError::Abstain(_))));
}

#[test]
fn type_into_pattern_unquoted() {
    let mut resolver = DeterministicTextResolver;
    let ctx = TextContext {
        goal: UiGoal::new("type milk into New todo"),
        field_label: "New todo".to_owned(),
        field_role: "textbox".to_owned(),
        typed: Vec::new(),
        context_fingerprint: 0,
    };
    let r = resolver.resolve(&ctx).expect("pattern resolves");
    assert_eq!(r.text, "milk");
}
