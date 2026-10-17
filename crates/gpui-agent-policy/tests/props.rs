//! M2.5/M5.4 property tests: an abstain never produces a dispatchable
//! op, every choice stays on the offered menu, and a `dangerous` offer
//! is never chosen — over arbitrary trees, goals, and candidate sets.

use gpui_agent::protocol::PlatformKind;
use gpui_agent::tree::{Bounds, UiNode, UiTree};
use gpui_agent_policy::arbitration::{Choice, choose};
use gpui_agent_policy::{ActionSpace, InstinctPolicy, PolicyOutcome, UiGoal, UiPolicy};
use instinct_core::Confidence;
use proptest::prelude::*;

fn word() -> impl Strategy<Value = String> {
    "[a-z]{1,8}".prop_map(String::from)
}

fn leaf_node() -> impl Strategy<Value = UiNode> {
    (
        prop::sample::select(vec!["button", "textbox", "checkbox", "link", "label"]),
        word(),
        proptest::bool::ANY,
        proptest::bool::ANY,
    )
        .prop_map(|(role, name, enabled, visible)| {
            let mut n = UiNode::new(format!("n-{name}"), role, name);
            n.enabled = enabled;
            n.visible = visible;
            n.bounds = Bounds {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            };
            n
        })
}

fn tree() -> impl Strategy<Value = UiTree> {
    prop::collection::vec(leaf_node(), 0..12).prop_map(|children| UiTree {
        app: "prop".to_owned(),
        platform: PlatformKind::Headless,
        ready: true,
        rev: Some(1),
        nodes: vec![{
            let mut w = UiNode::window("w", "App");
            w.enabled = true;
            w.visible = true;
            w.children = children;
            w
        }],
    })
}

fn goal() -> impl Strategy<Value = UiGoal> {
    (
        word(),
        proptest::option::of(word()),
        proptest::option::of(word()),
    )
        .prop_map(|(verb, quote, tail)| {
            let mut g = verb;
            if let Some(q) = quote {
                g.push_str(&format!(" \"{q}\""));
            }
            if let Some(t) = tail {
                g.push(' ');
                g.push_str(&t);
            }
            UiGoal::new(g)
        })
}

proptest! {
    /// M2.5: abstain never yields a dispatchable choice — and a choice
    /// is always on the offered menu and never `dangerous`.
    #[test]
    fn abstain_never_chooses_and_choice_stays_on_menu(t in tree(), g in goal()) {
        let space = ActionSpace::from_tree(&t, &[], &[]);
        let outcome = InstinctPolicy::default()
            .decide(&space, &g, &[])
            .expect("decide does not error on arbitrary trees");
        match outcome {
            PolicyOutcome::Choice(d) => {
                let offered = space
                    .get(&d.action_id)
                    .expect("a choice must name an offered action");
                prop_assert!(
                    !offered.dangerous(),
                    "dangerous offer {offered:?} reached Choice"
                );
            }
            PolicyOutcome::Abstain { .. } => {
                prop_assert!(outcome.as_choice().is_none());
            }
        }
    }

    /// `arbitration::choose` at the raw level: `Chosen` names an offered
    /// id; `Abstained` names nothing.
    #[test]
    fn choose_outcome_is_on_menu_or_nothing(
        candidates in prop::collection::vec((word(), 0i16..=1000), 0..8)
    ) {
        let mut seen = std::collections::BTreeSet::new();
        let candidates: Vec<(String, Confidence)> = candidates
            .into_iter()
            .filter(|(id, _)| seen.insert(id.clone()))
            .map(|(id, c)| (id, Confidence::new(c).expect("0..=1000")))
            .collect();
        match choose(&candidates, None).expect("choose does not error") {
            Choice::Chosen { id, .. } => {
                prop_assert!(candidates.iter().any(|(cid, _)| *cid == id));
            }
            Choice::Abstained { why, .. } => {
                prop_assert!(!why.is_empty());
            }
        }
    }
}
