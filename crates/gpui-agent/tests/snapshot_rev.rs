//! M3: revision tracking, snapshot cache, `since` diffs, compact format.
//!
//! Wire-compat pins: a v2 `{"op":"snapshot"}` request and response keep
//! byte-identical serialization — new fields only appear when used.

use gpui_agent::dispatch::{DispatchResult, TreeCache};
use gpui_agent::protocol::{
    AssertSpec, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request, Response,
};
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent::{
    AgentHost, HelloAuth, SnapshotFormat, apply_diff, compact_lines, diff_trees,
    handle_request_cached,
};

fn req(op: Op) -> Request {
    Request {
        v: PROTOCOL_VERSION,
        id: "t".into(),
        token: None,
        auth: None,
        op,
    }
}

fn node(id: &str, role: &str, name: &str) -> UiNode {
    UiNode::new(id, role, name)
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

/// Counting host: `TreeCache::rebuild_count` tallies real `snapshot()`
/// calls so tests can prove the cache — not luck — is what stops
/// repeat work.
struct CountingHost {
    rev: Option<u64>,
    nodes: Vec<UiNode>,
}

impl CountingHost {
    fn new(rev: Option<u64>, nodes: Vec<UiNode>) -> Self {
        Self { rev, nodes }
    }
}

impl AgentHost for CountingHost {
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
        tree_with(self.rev, self.nodes.clone())
    }

    fn revision(&self) -> Option<u64> {
        self.rev
    }

    fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
        Ok(DispatchResult::empty())
    }
}

// ---------- wire compat ----------

#[test]
fn snapshot_op_v2_bytes_unchanged() {
    // Old clients send exactly this; new servers must accept it.
    let parsed: Request = serde_json::from_str(r#"{"v":2,"id":"1","op":"snapshot"}"#).unwrap();
    assert!(matches!(
        parsed.op,
        Op::Snapshot {
            since: None,
            format: None
        }
    ));
    // New hosts answering old requests serialize identically.
    let out = serde_json::to_string(&Op::snapshot()).unwrap();
    assert_eq!(out, r#"{"op":"snapshot"}"#);
    let resp = Response::ok("1");
    let json = serde_json::to_string(&resp).unwrap();
    assert!(!json.contains("rev"), "no rev key on v2 response: {json}");
}

#[test]
fn snapshot_since_and_format_serialize() {
    let out = serde_json::to_string(&Op::snapshot_since(7, None)).unwrap();
    assert_eq!(out, r#"{"op":"snapshot","since":7}"#);
    let out = serde_json::to_string(&Op::snapshot_since(7, Some(SnapshotFormat::Compact))).unwrap();
    assert_eq!(out, r#"{"op":"snapshot","since":7,"format":"compact"}"#);
    let parsed: Request =
        serde_json::from_str(r#"{"v":2,"id":"1","op":"snapshot","format":"compact"}"#).unwrap();
    assert!(matches!(
        parsed.op,
        Op::Snapshot {
            since: None,
            format: Some(SnapshotFormat::Compact)
        }
    ));
}

#[test]
fn revision_op_wire() {
    let out = serde_json::to_string(&Op::Revision).unwrap();
    assert_eq!(out, r#"{"op":"revision"}"#);
    let parsed: Request = serde_json::from_str(r#"{"v":2,"id":"1","op":"revision"}"#).unwrap();
    assert!(matches!(parsed.op, Op::Revision));
}

#[test]
fn uitree_rev_omitted_when_none() {
    let t = tree_with(None, vec![]);
    assert!(!serde_json::to_string(&t).unwrap().contains("\"rev\""));
    let t = tree_with(Some(9), vec![]);
    assert!(serde_json::to_string(&t).unwrap().contains("\"rev\":9"));
}

#[test]
fn hello_features_omitted_when_empty() {
    let mut host = CountingHost::new(Some(0), vec![]);
    let resp = handle_request_cached(&mut host, &mut TreeCache::new(), req(Op::Hello), None, None);
    let hello = resp.hello.expect("hello");
    assert!(hello.features.contains(&"rev".to_string()));
    assert!(hello.features.contains(&"snapshot_since".to_string()));
    assert!(hello.features.contains(&"snapshot_compact".to_string()));
    // Untracked host advertises no "rev".
    let mut untracked = CountingHost::new(None, vec![]);
    let resp = handle_request_cached(
        &mut untracked,
        &mut TreeCache::new(),
        req(Op::Hello),
        None,
        None,
    );
    assert!(!resp.hello.unwrap().features.contains(&"rev".to_string()));
}

// ---------- TreeCache ----------

#[test]
fn cache_skips_rebuild_on_same_rev() {
    let mut host = CountingHost::new(Some(1), vec![node("w", "window", "W")]);
    let mut cache = TreeCache::new();
    for _ in 0..5 {
        let tree = cache.snapshot(&host);
        assert_eq!(tree.rev, Some(1));
    }
    assert_eq!(cache.rebuild_count(), 1);
    host.rev = Some(2);
    let _ = cache.snapshot(&host);
    assert_eq!(cache.rebuild_count(), 2);
    let _ = cache.snapshot(&host);
    assert_eq!(cache.rebuild_count(), 2);
}

#[test]
fn none_revision_always_rebuilds() {
    let host = CountingHost::new(None, vec![node("w", "window", "W")]);
    let mut cache = TreeCache::new();
    for _ in 0..3 {
        cache.snapshot(&host);
    }
    assert_eq!(cache.rebuild_count(), 3);
    assert!(cache.tree_at(0).is_none());
}

#[test]
fn ring_holds_last_eight_revisions() {
    let mut host = CountingHost::new(Some(0), vec![node("w", "window", "W")]);
    let mut cache = TreeCache::new();
    for rev in 0..10 {
        host.rev = Some(rev);
        cache.snapshot(&host);
    }
    assert!(cache.tree_at(9).is_some());
    assert!(cache.tree_at(2).is_some());
    assert!(cache.tree_at(1).is_none(), "rev 1 evicted past depth 8");
    assert!(cache.tree_at(0).is_none());
}

#[test]
fn wait_until_unchanged_tree_never_rebuilds() {
    // GATE-P: 50ms of polling an unchanged tree must cost one rebuild.
    let host = CountingHost::new(
        Some(1),
        vec![UiNode::window("w", "W").with_children(vec![node("btn", "button", "Go")])],
    );
    let mut cache = TreeCache::new();
    // Host needs &mut for handle_request_cached; wrap for static borrow.
    let mut host = host;
    let spec = AssertSpec {
        target: "btn".into(),
        name: Some("NoSuch".into()),
        ..AssertSpec::default()
    };
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::WaitUntil {
            timeout_ms: 45,
            spec,
        }),
        None,
        None,
    );
    assert!(!resp.ok, "spec never matches");
    assert_eq!(cache.rebuild_count(), 1, "unchanged rev → one rebuild");
}

#[test]
fn assert_reuses_cached_tree() {
    let mut host = CountingHost::new(Some(3), vec![node("btn", "button", "Go")]);
    let mut cache = TreeCache::new();
    let spec = AssertSpec {
        target: "btn".into(),
        exists: Some(true),
        ..AssertSpec::default()
    };
    for _ in 0..3 {
        let resp = handle_request_cached(
            &mut host,
            &mut cache,
            req(Op::Assert { spec: spec.clone() }),
            None,
            None,
        );
        assert!(resp.ok, "{:?}", resp.error);
    }
    assert_eq!(cache.rebuild_count(), 1);
}

// ---------- revision op + snapshot since through dispatch ----------

#[test]
fn revision_op_reports_host_rev() {
    let mut host = CountingHost::new(Some(42), vec![]);
    let resp = handle_request_cached(
        &mut host,
        &mut TreeCache::new(),
        req(Op::Revision),
        None,
        None,
    );
    assert_eq!(resp.rev, Some(42));
    let mut untracked = CountingHost::new(None, vec![]);
    let resp = handle_request_cached(
        &mut untracked,
        &mut TreeCache::new(),
        req(Op::Revision),
        None,
        None,
    );
    assert_eq!(resp.rev, None);
}

#[test]
fn snapshot_since_same_rev_is_unchanged() {
    let mut host = CountingHost::new(Some(5), vec![node("w", "window", "W")]);
    let mut cache = TreeCache::new();
    let first = handle_request_cached(&mut host, &mut cache, req(Op::snapshot()), None, None);
    assert_eq!(first.rev, Some(5));
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::snapshot_since(5, None)),
        None,
        None,
    );
    let result = resp.result.clone().expect("result");
    assert_eq!(result["unchanged"], true);
    assert!(
        serde_json::to_string(&resp).unwrap().len() <= 1024,
        "GATE-P: unchanged answer must fit 1 KiB"
    );
}

#[test]
fn snapshot_since_cached_rev_returns_diff() {
    let mut host = CountingHost::new(
        Some(1),
        vec![node("w", "window", "W").with_children(vec![node("a", "button", "A")])],
    );
    let mut cache = TreeCache::new();
    let _ = handle_request_cached(&mut host, &mut cache, req(Op::snapshot()), None, None);
    // Mutate: rev bump + added node + changed name.
    host.rev = Some(2);
    host.nodes = vec![
        node("w", "window", "W")
            .with_children(vec![node("a", "button", "A2"), node("b", "button", "B")]),
    ];
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::snapshot_since(1, None)),
        None,
        None,
    );
    let diff = &resp.result.expect("result")["diff"];
    assert!(
        diff["added"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| { a["id"] == "b" && a["parent"] == "w" })
    );
    assert!(
        diff["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| { c["id"] == "a" && c["fields"]["name"] == "A2" })
    );
}

#[test]
fn snapshot_since_evicted_rev_returns_full() {
    let mut host = CountingHost::new(Some(0), vec![node("w", "window", "W")]);
    let mut cache = TreeCache::new();
    let _ = handle_request_cached(&mut host, &mut cache, req(Op::snapshot()), None, None);
    for rev in 1..10 {
        host.rev = Some(rev);
        cache.snapshot(&host);
    }
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::snapshot_since(0, None)),
        None,
        None,
    );
    assert_eq!(resp.result.expect("result")["full"], true);
    assert!(resp.tree.is_some(), "full tree sent when diff base lost");
}

#[test]
fn snapshot_compact_format() {
    let mut host = CountingHost::new(
        Some(1),
        vec![node("w", "window", "W").with_children(vec![
            node("in", "password", "Secret").with_value("hunter2"),
            node("btn", "button", "Go").with_checked(true),
        ])],
    );
    let mut cache = TreeCache::new();
    let resp = handle_request_cached(
        &mut host,
        &mut cache,
        req(Op::snapshot_since(0, Some(SnapshotFormat::Compact))),
        None,
        None,
    );
    let result = resp.result.expect("result");
    let compact = result["compact"].as_str().expect("compact text");
    assert!(compact.contains("w window \"W\""));
    assert!(compact.contains("  btn button \"Go\"  [checked]"));
    assert!(!compact.contains("hunter2"), "password value never emitted");
    assert!(compact.contains("in password \"Secret\""));
}

// ---------- diff correctness ----------

#[test]
fn diff_add_remove_change() {
    let old = tree_with(
        Some(1),
        vec![
            node("w", "window", "W")
                .with_children(vec![node("a", "button", "A"), node("gone", "button", "X")]),
        ],
    );
    let new = tree_with(
        Some(2),
        vec![node("w", "window", "W").with_children(vec![
            node("a", "button", "A2").with_checked(true),
            node("b", "button", "B"),
        ])],
    );
    let diff = diff_trees(&old, &new);
    assert_eq!(diff.removed, vec!["gone"]);
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.added[0].node.id, "b");
    assert_eq!(diff.added[0].parent.as_deref(), Some("w"));
    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].id, "a");
    assert_eq!(diff.changed[0].fields["name"], "A2");
    assert_eq!(diff.changed[0].fields["checked"], true);
}

#[test]
fn diff_move_is_remove_plus_add() {
    let old = tree_with(
        Some(1),
        vec![
            node("w", "window", "W").with_children(vec![node("m", "button", "M")]),
            node("other", "group", "O"),
        ],
    );
    let new = tree_with(
        Some(2),
        vec![
            node("w", "window", "W"),
            node("other", "group", "O").with_children(vec![node("m", "button", "M")]),
        ],
    );
    let diff = diff_trees(&old, &new);
    assert!(diff.removed.contains(&"m".to_string()));
    assert!(
        diff.added
            .iter()
            .any(|a| a.node.id == "m" && a.parent.as_deref() == Some("other"))
    );
}

#[test]
fn diff_empty_when_identical() {
    let t = tree_with(Some(1), vec![node("w", "window", "W")]);
    let diff = diff_trees(&t, &t);
    assert!(diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty());
}

// Deterministic mini property test: apply_diff inverts diff_trees on
// generated trees (LCG — no proptest dep, seed fixed, output stable).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn gen_tree(rng: &mut Lcg, rev: u64) -> UiTree {
    let n = 1 + (rng.next() % 12) as usize;
    let mut roots = Vec::new();
    for i in 0..n {
        let mut top = node(
            &format!("t{i}"),
            ["window", "group", "list"][(rng.next() % 3) as usize],
            &format!("T{i}"),
        );
        let kids = (rng.next() % 6) as usize;
        let mut children = Vec::new();
        for j in 0..kids {
            let mut c = node(
                &format!("t{i}c{j}"),
                ["button", "field", "label"][(rng.next() % 3) as usize],
                &format!("C{j}"),
            );
            c.checked = Some(rng.next() % 2 == 0);
            c.visible = rng.next() % 5 != 0;
            children.push(c);
        }
        top.children = children;
        roots.push(top);
    }
    tree_with(Some(rev), roots)
}

#[test]
fn apply_diff_reconstructs_ids_and_fields() {
    let mut rng = Lcg(0x5eed);
    for case in 0..64 {
        let old = gen_tree(&mut rng, case);
        let mut new = old.clone();
        // Perturb: drop some nodes, tweak some fields.
        for top in &mut new.nodes {
            top.children.retain(|_| rng.next() % 4 != 0);
            for c in &mut top.children {
                if rng.next() % 3 == 0 {
                    c.name = format!("{}*", c.name);
                }
                if rng.next() % 4 == 0 {
                    c.checked = Some(rng.next() % 2 == 0);
                }
            }
        }
        new.nodes.retain(|_| rng.next() % 5 != 0);
        new.rev = Some(case + 1000);
        let diff = diff_trees(&old, &new);
        let applied = apply_diff(&old, &diff);
        // Identity check: same flattened id set; every surviving id's
        // scalar fields match; removed ids are gone.
        let ids_of = |t: &UiTree| -> Vec<(String, serde_json::Value)> {
            let mut v = Vec::new();
            let mut stack: Vec<&UiNode> = t.nodes.iter().collect();
            while let Some(n) = stack.pop() {
                let mut j = serde_json::to_value(n).unwrap();
                j.as_object_mut().map(|o| o.remove("children"));
                v.push((n.id.clone(), j));
                stack.extend(n.children.iter());
            }
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        assert_eq!(ids_of(&applied), ids_of(&new), "case {case}");
    }
}

// ---------- compact_lines ----------

#[test]
fn compact_skips_invisible_and_truncates() {
    let long = "x".repeat(500);
    let mut invisible = node("hidden", "button", "H");
    invisible.visible = false;
    let t = tree_with(
        Some(1),
        vec![
            node("w", "window", "W")
                .with_children(vec![invisible, node("f", "field", "F").with_value(&long)]),
        ],
    );
    let out = compact_lines(&t);
    assert!(!out.contains("hidden"), "invisible nodes skipped:\n{out}");
    let value_len = out
        .lines()
        .find(|l| l.contains("value="))
        .map(|l| l.len())
        .unwrap_or(0);
    assert!(value_len < 260, "200-char cap + wrapper: {value_len}");
    assert!(out.contains('…'), "truncation marker: {out}");
}

#[test]
fn compact_flags_and_indent() {
    let mut disabled = node("d", "button", "D");
    disabled.enabled = false;
    let t = tree_with(
        Some(1),
        vec![node("w", "window", "W").with_children(vec![
            disabled.with_children(vec![node("deep", "label", "L")]),
            node("focused-node", "field", "F").with_focused(true),
        ])],
    );
    let out = compact_lines(&t);
    assert!(out.contains("w window \"W\"\n"));
    assert!(out.contains("  d button \"D\"  [disabled]"));
    assert!(out.contains("    deep label \"L\""));
    assert!(out.contains("  focused-node field \"F\"  [focused]"));
}
