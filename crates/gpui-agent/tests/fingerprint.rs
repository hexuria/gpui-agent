//! M5: `UiNode::fingerprint` properties, the `expect_fingerprint`
//! dispatch gate (`stale_target` / `target_gone`), `snapshot
//! fingerprints:true` exposure, and wire compatibility when the
//! optional field is omitted.

use gpui_agent::dispatch::{DispatchResult, TreeCache};
use gpui_agent::protocol::{DeliveryMode, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request};
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent::{AgentHost, HelloAuth, handle_request_cached};

fn req(op: Op) -> Request {
    Request {
        v: PROTOCOL_VERSION,
        id: "t".into(),
        token: None,
        auth: None,
        op,
    }
}

fn tree_with(rev: Option<u64>, nodes: Vec<UiNode>) -> UiTree {
    UiTree {
        app: "t".into(),
        platform: PlatformKind::Headless,
        ready: true,
        nodes,
        rev,
    }
}

// ---------- fingerprint properties ----------

#[test]
fn fingerprint_is_stable_for_identical_nodes() {
    let a = UiNode::new("add", "button", "Add");
    let b = UiNode::new("add", "button", "Add");
    assert_eq!(a.fingerprint(None), b.fingerprint(None));
    assert_eq!(
        a.fingerprint(Some("toolbar")),
        b.fingerprint(Some("toolbar"))
    );
    // A parent changes the fingerprint — the gate binds position.
    assert_ne!(a.fingerprint(None), a.fingerprint(Some("toolbar")));
}

#[test]
fn fingerprint_changes_on_every_relevant_field() {
    let base = UiNode::new("n", "button", "Name");
    let fp = base.fingerprint(Some("p"));

    let mut n = base.clone();
    n.id = "n2".into();
    assert_ne!(fp, n.fingerprint(Some("p")), "id");

    let mut n = base.clone();
    n.role = "link".into();
    assert_ne!(fp, n.fingerprint(Some("p")), "role");

    let mut n = base.clone();
    n.name = "Name2".into();
    assert_ne!(fp, n.fingerprint(Some("p")), "name");

    let mut n = base.clone();
    n.enabled = false;
    assert_ne!(fp, n.fingerprint(Some("p")), "enabled");

    let mut n = base.clone();
    n.visible = false;
    assert_ne!(fp, n.fingerprint(Some("p")), "visible");

    let mut n = base.clone();
    n.checked = Some(true);
    assert_ne!(fp, n.fingerprint(Some("p")), "checked");

    let mut n = base.clone();
    n.checked = Some(false);
    assert_ne!(fp, n.fingerprint(Some("p")), "checked:false vs none");

    let mut n = base.clone();
    n.value = Some("v".into());
    assert_ne!(fp, n.fingerprint(Some("p")), "value");

    let mut n = base.clone();
    n.states = vec!["selected".into()];
    assert_ne!(fp, n.fingerprint(Some("p")), "states");

    // Different parent id → different fingerprint.
    assert_ne!(
        base.fingerprint(Some("p")),
        base.fingerprint(Some("q")),
        "parent_id"
    );
}

#[test]
fn fingerprint_ignores_bounds_and_focus() {
    let mut a = UiNode::new("n", "button", "Name");
    a.focused = true;
    let mut b = UiNode::new("n", "button", "Name");
    b.bounds.x = 99.0;
    b.bounds.w = 640.0;
    assert_eq!(
        a.fingerprint(Some("p")),
        b.fingerprint(Some("p")),
        "bounds/focus are not semantic — a moved or refocused node is the same target"
    );
}

#[test]
fn fingerprint_of_and_map_cover_nested_nodes() {
    let mut parent = UiNode::new("p", "group", "P");
    parent.children = vec![UiNode::new("c", "button", "C")];
    let tree = tree_with(Some(1), vec![parent]);

    assert_eq!(
        tree.fingerprint_of("c"),
        Some(tree.find("c").unwrap().fingerprint(Some("p")))
    );
    assert_eq!(
        tree.fingerprint_of("p"),
        Some(tree.find("p").unwrap().fingerprint(None))
    );
    assert_eq!(tree.fingerprint_of("missing"), None);

    let map = tree.fingerprint_map();
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("c"), tree.fingerprint_of("c").as_ref());
}

// ---------- dispatch gate ----------

/// Host whose tree can be swapped between calls to model a race: the
/// client decides against revision A, then the app mutates to B before
/// the gated op arrives. `dispatched` counts real `dispatch()` hits so
/// tests prove nothing reached the app on a mismatch.
struct RaceHost {
    rev: u64,
    nodes: Vec<UiNode>,
    dispatched: usize,
}

impl RaceHost {
    fn new(nodes: Vec<UiNode>) -> Self {
        Self {
            rev: 1,
            nodes,
            dispatched: 0,
        }
    }

    fn swap(&mut self, rev: u64, nodes: Vec<UiNode>) {
        self.rev = rev;
        self.nodes = nodes;
    }
}

impl AgentHost for RaceHost {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            app: "t".into(),
            platform: PlatformKind::Headless,
            os: "test".into(),
            ready: true,
            deliveries: vec![],
            auth: HelloAuth::None,
            bundle_id: None,
            executable: None,
            features: vec![],
        }
    }

    fn snapshot(&self) -> UiTree {
        tree_with(Some(self.rev), self.nodes.clone())
    }

    fn revision(&self) -> Option<u64> {
        Some(self.rev)
    }

    fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
        self.dispatched += 1;
        Ok(DispatchResult::empty())
    }
}

#[test]
fn gated_click_dispatches_when_fingerprint_matches() {
    let mut host = RaceHost::new(vec![UiNode::new("add", "button", "Add")]);
    let mut cache = TreeCache::default();
    let fp = {
        let tree = cache.snapshot(&host);
        tree.fingerprint_of("add").unwrap()
    };
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::Click {
            target: "add".into(),
            delivery: DeliveryMode::Semantic,
            expect_fingerprint: Some(fp),
        }),
        None,
        None,
    );
    assert!(resp.ok, "matching fingerprint must dispatch: {resp:?}");
    assert_eq!(host.dispatched, 1);
}

#[test]
fn stale_target_blocks_dispatch_and_changes_nothing() {
    let mut host = RaceHost::new(vec![UiNode::new("add", "button", "Add")]);
    let mut cache = TreeCache::default();
    let fp = {
        let tree = cache.snapshot(&host);
        tree.fingerprint_of("add").unwrap()
    };
    // Race: the app renames the button between decide and dispatch.
    host.swap(2, vec![UiNode::new("add", "button", "Save")]);
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::Click {
            target: "add".into(),
            delivery: DeliveryMode::Semantic,
            expect_fingerprint: Some(fp),
        }),
        None,
        None,
    );
    assert!(!resp.ok);
    let err = resp.error.unwrap_or_default();
    assert!(err.starts_with("stale_target"), "got {err}");
    assert_eq!(host.dispatched, 0, "nothing may dispatch on a mismatch");
}

#[test]
fn target_gone_blocks_dispatch() {
    let mut host = RaceHost::new(vec![UiNode::new("add", "button", "Add")]);
    let mut cache = TreeCache::default();
    let fp = {
        let tree = cache.snapshot(&host);
        tree.fingerprint_of("add").unwrap()
    };
    host.swap(2, vec![]);
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::Click {
            target: "add".into(),
            delivery: DeliveryMode::Semantic,
            expect_fingerprint: Some(fp),
        }),
        None,
        None,
    );
    assert!(!resp.ok);
    let err = resp.error.unwrap_or_default();
    assert!(err.starts_with("target_gone"), "got {err}");
    assert_eq!(host.dispatched, 0);
}

#[test]
fn unguarded_ops_skip_the_gate() {
    // expect_fingerprint omitted → v1 behavior, even against a moved node.
    let mut host = RaceHost::new(vec![UiNode::new("add", "button", "Add")]);
    let mut cache = TreeCache::default();
    host.swap(2, vec![UiNode::new("add", "button", "Save")]);
    let resp = handle_request_cached(&mut host, &mut cache, req(Op::click("add")), None, None);
    assert!(resp.ok);
    assert_eq!(host.dispatched, 1);
}

#[test]
fn snapshot_fingerprints_true_returns_the_map() {
    let mut host = RaceHost::new(vec![UiNode::new("add", "button", "Add")]);
    let mut cache = TreeCache::default();
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::Snapshot {
            since: None,
            format: None,
            fingerprints: Some(true),
        }),
        None,
        None,
    );
    assert!(resp.ok);
    let map = resp.fingerprints.expect("fingerprints requested");
    assert_eq!(map.len(), 1);
    assert_eq!(
        map.get("add"),
        host.snapshot().fingerprint_of("add").as_ref()
    );

    // fingerprints:false and omitted → no map, no field on the wire.
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::Snapshot {
            since: None,
            format: None,
            fingerprints: Some(false),
        }),
        None,
        None,
    );
    assert!(resp.fingerprints.is_none());
    let resp = handle_request_cached(&mut host, &mut cache, req(Op::snapshot()), None, None);
    assert!(resp.fingerprints.is_none());
    assert!(
        !serde_json::to_string(&resp)
            .unwrap()
            .contains("fingerprints")
    );
}

#[test]
fn hello_advertises_expect_fingerprint() {
    let mut host = RaceHost::new(vec![]);
    let mut cache = TreeCache::default();
    let resp = handle_request_cached(&mut host, &mut cache, req(Op::Hello), None, None);
    let features = resp.hello.unwrap_or_else(|| panic!("hello")).features;
    assert!(
        features.iter().any(|f| f == "expect_fingerprint"),
        "features: {features:?}"
    );
}

// ---------- wire compat ----------

#[test]
fn mutating_ops_without_fingerprint_keep_v2_bytes() {
    for (op, want) in [
        (Op::click("x"), r#"{"op":"click","target":"x"}"#),
        (
            Op::type_text("x", "hi"),
            r#"{"op":"type","target":"x","text":"hi"}"#,
        ),
        (
            Op::SetValue {
                target: "x".into(),
                value: "v".into(),
                expect_fingerprint: None,
            },
            r#"{"op":"set_value","target":"x","value":"v"}"#,
        ),
        (
            Op::key("x", "Enter"),
            r#"{"op":"key","target":"x","key":"Enter"}"#,
        ),
    ] {
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, want);
        let parsed: Op = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fingerprint_gate(), None);
    }
    // Old wire bytes (no expect_fingerprint) still parse.
    let parsed: Request =
        serde_json::from_str(r#"{"v":2,"id":"1","op":"click","target":"x"}"#).unwrap();
    match parsed.op {
        Op::Click {
            target,
            expect_fingerprint,
            ..
        } => {
            assert_eq!(target, "x");
            assert_eq!(expect_fingerprint, None);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn gated_op_wire_shape_and_gate_accessor() {
    let op = Op::click("x").with_fingerprint(42);
    let json = serde_json::to_string(&op).unwrap();
    assert_eq!(
        json,
        r#"{"op":"click","target":"x","expect_fingerprint":42}"#
    );
    assert_eq!(op.fingerprint_gate(), Some(("x", 42)));
    // Non-mutating ops report no gate.
    assert_eq!(Op::snapshot().fingerprint_gate(), None);
    // with_fingerprint is a no-op on non-targeted ops.
    assert!(matches!(
        Op::snapshot().with_fingerprint(1),
        Op::Snapshot { .. }
    ));
}
