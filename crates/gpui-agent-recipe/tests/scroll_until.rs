//! Recipe-only bounded `scroll_until` runs against a fake scroll host.
#![expect(
    clippy::unwrap_used,
    reason = "a failed setup/assert aborts the test anyway"
)]

use std::collections::BTreeMap;

use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::{
    DeliveryMode, HelloAuth, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind,
};
use gpui_agent::testing::TestHost;
use gpui_agent::tree::{Bounds, UiNode, UiTree};
use gpui_agent_recipe::{
    Recipe, RecipeErrorCode, RunError, RunOptions, compile_plan, parse_wants, run_plan,
    todo_registry,
};

#[derive(Default)]
struct ScrollHost {
    scrolls: usize,
    reveal_at: Option<usize>,
}

impl AgentHost for ScrollHost {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            app: "scroll".into(),
            platform: PlatformKind::Headless,
            os: gpui_agent::protocol::host_os(),
            ready: true,
            deliveries: vec![DeliveryMode::Semantic],
            auth: HelloAuth::None,
            bundle_id: None,
            executable: None,
            features: vec!["scroll".into()],
            framework: None,
            device: None,
        }
    }

    fn snapshot(&self) -> UiTree {
        let mut list = UiNode::new("list", "scroll", "List");
        list.children
            .push(UiNode::new("row-0", "listitem", "Row 0"));
        if self.reveal_at.is_some_and(|at| self.scrolls >= at) {
            list.children
                .push(UiNode::new("row-target", "listitem", "Target row"));
        }
        UiTree {
            app: "scroll".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![list],
            rev: Some(u64::try_from(self.scrolls).unwrap_or(u64::MAX)),
        }
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if matches!(op, Op::Scroll { .. }) {
            self.scrolls += 1;
        }
        Ok(DispatchResult::empty())
    }
}

fn host(reveal_at: Option<usize>) -> TestHost<ScrollHost> {
    TestHost::spawn(
        ScrollHost {
            scrolls: 0,
            reveal_at,
        },
        None,
    )
    .unwrap()
}

fn json_recipe(max_steps: u64) -> Recipe {
    Recipe::from_json(&format!(
        r#"{{"name":"scroll","steps":[{{"id":"s1","op":"scroll_until","target":"list","until":"row-target","dy":120,"max_steps":{max_steps}}}]}}"#
    ))
    .unwrap()
}

fn run(
    host: &TestHost<ScrollHost>,
    recipe: &Recipe,
) -> Result<gpui_agent_recipe::Receipt, RunError> {
    let plan = compile_plan(recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    run_plan(&mut client, &plan, &RunOptions::new())
}

#[test]
fn found_after_two_scroll_steps() {
    let host = host(Some(2));
    let receipt = run(&host, &json_recipe(5)).expect("run");
    assert_eq!(receipt.steps[0].result.as_ref().unwrap()["steps"], 2);
    assert_eq!(host.host().lock().unwrap().scrolls, 2);
}

#[test]
fn never_found_is_a_bounded_failure_with_revision() {
    let host = host(None);
    let Err(RunError::Step { error, .. }) = run(&host, &json_recipe(2)) else {
        panic!("must be bounded");
    };
    assert!(error.contains("after 2 scroll steps"), "{error}");
    assert!(error.contains("last snapshot revision 2"), "{error}");
    assert_eq!(host.host().lock().unwrap().scrolls, 2);
}

#[test]
fn already_visible_scrolls_zero_times() {
    let host = host(Some(0));
    let receipt = run(&host, &json_recipe(5)).expect("run");
    assert_eq!(receipt.steps[0].result.as_ref().unwrap()["steps"], 0);
    assert_eq!(host.host().lock().unwrap().scrolls, 0);
}

#[test]
fn wants_and_json_compile_to_the_same_plan() {
    let json = compile_plan(&json_recipe(7), &BTreeMap::new(), &todo_registry()).unwrap();
    let wants_recipe = parse_wants(
        "scroll_until list row-target --dy 120 --max-steps 7",
        "scroll",
    )
    .unwrap();
    let wants = compile_plan(&wants_recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(json.fingerprint, wants.fingerprint);
    assert_eq!(json.steps[0].until.as_deref(), Some("row-target"));
    assert_eq!(json.steps[0].max_steps, Some(7));
}

#[test]
fn max_steps_over_fifty_is_refused_at_parse() {
    let err = Recipe::from_json(
        r#"{"name":"x","steps":[{"id":"s","op":"scroll_until","target":"list","until":"row","dy":1,"max_steps":51}]}"#,
    )
    .unwrap_err();
    assert_eq!(err.code, RecipeErrorCode::Parse);
    assert!(err.to_string().contains("1 to 50"), "{err}");
    let err = parse_wants("scroll_until list row --dy 1 --max-steps 51", "x")
        .unwrap_err()
        .to_string();
    assert!(err.contains("1 to 50"), "{err}");
}

#[test]
fn wants_scroll_maps_to_the_json_scroll_op() {
    let recipe = parse_wants("scroll list --dy 120 --dx -4", "s").unwrap();
    match &recipe.steps[0].op {
        Op::Scroll { target, dx, dy, .. } => {
            assert_eq!(target.as_deref(), Some("list"));
            assert_eq!(*dx, -4.0);
            assert_eq!(*dy, 120.0);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each fixture switch is independent"
)]
struct GeometryHost {
    scrolls: usize,
    target_y: Option<f32>,
    row10: bool,
    outside_target: bool,
    hidden_target: bool,
    container_named_target: bool,
    zero_target_bounds: bool,
}

impl GeometryHost {
    fn bounded(id: &str, y: f32) -> UiNode {
        UiNode::new(id, "listitem", id).with_bounds(Bounds {
            x: 0.0,
            y,
            w: 100.0,
            h: 20.0,
        })
    }
}

impl AgentHost for GeometryHost {
    fn hello(&self) -> HelloInfo {
        ScrollHost::default().hello()
    }

    fn snapshot(&self) -> UiTree {
        let mut list = UiNode::new("list", "scroll", "List").with_bounds(Bounds {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        });
        if self.container_named_target {
            list.name = "row-target".into();
        }
        list.children.push(Self::bounded("row-0", 0.0));
        if self.row10 {
            list.children.push(Self::bounded("row-10", 10.0));
        }
        if let Some(y) = self.target_y {
            let scrolls = u16::try_from(self.scrolls).unwrap_or(u16::MAX);
            let y = y - (f32::from(scrolls) * 100.0);
            let mut row = Self::bounded("row-target", y);
            if self.zero_target_bounds {
                row.bounds = Bounds::default();
            }
            list.children.push(row);
        }
        if self.hidden_target {
            let mut parent = UiNode::new("hidden-parent", "group", "Hidden");
            parent.visible = false;
            parent.children.push(Self::bounded("row-target", 10.0));
            list.children.push(parent);
        }
        let mut roots = vec![list];
        if self.outside_target {
            roots.push(Self::bounded("row-target", 10.0));
        }
        UiTree {
            app: "geometry".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: roots,
            rev: Some(u64::try_from(self.scrolls).unwrap_or(u64::MAX)),
        }
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if matches!(op, Op::Scroll { .. }) {
            self.scrolls += 1;
        }
        Ok(DispatchResult::empty())
    }
}

fn geometry(host: GeometryHost) -> TestHost<GeometryHost> {
    TestHost::spawn(host, None).unwrap()
}

fn run_geometry(host: &TestHost<GeometryHost>) -> Result<gpui_agent_recipe::Receipt, RunError> {
    let plan = compile_plan(&json_recipe(2), &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    run_plan(&mut client, &plan, &RunOptions::new())
}

#[test]
fn offscreen_descendant_does_not_match_until_inside_container_bounds() {
    let host = geometry(GeometryHost {
        target_y: Some(150.0),
        ..GeometryHost::default()
    });
    let receipt = run_geometry(&host).expect("run");
    assert_eq!(receipt.steps[0].result.as_ref().unwrap()["steps"], 1);
    assert_eq!(host.host().lock().unwrap().scrolls, 1);
}

#[test]
fn matching_requires_a_descendant_inside_the_container() {
    for host in [
        geometry(GeometryHost {
            outside_target: true,
            ..GeometryHost::default()
        }),
        geometry(GeometryHost {
            hidden_target: true,
            ..GeometryHost::default()
        }),
        geometry(GeometryHost {
            container_named_target: true,
            ..GeometryHost::default()
        }),
    ] {
        assert!(
            run_geometry(&host).is_err(),
            "a match outside the visible descendants passed"
        );
    }
}

#[test]
fn matching_is_anchored_row1_does_not_match_row10() {
    let host = geometry(GeometryHost {
        row10: true,
        ..GeometryHost::default()
    });
    let recipe = Recipe::from_json(
        r#"{"name":"scroll","steps":[{"id":"s1","op":"scroll_until","target":"list","until":"row-1","dy":120,"max_steps":1}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    assert!(
        run_plan(&mut client, &plan, &RunOptions::new()).is_err(),
        "row-10 must not satisfy row-1"
    );
}

#[test]
fn serialized_scroll_until_round_trips() {
    let recipe = json_recipe(7);
    let written = serde_json::to_string(&recipe).unwrap();
    assert!(written.contains("\"op\":\"scroll_until\""), "{written}");
    let back = Recipe::from_json(&written).unwrap();
    assert_eq!(back.steps[0].until.as_deref(), Some("row-target"));
}

#[test]
fn container_that_does_not_move_stops_after_two_scrolls() {
    let host = host(None);
    let Err(RunError::Step { error, .. }) = run(&host, &json_recipe(5)) else {
        panic!("must stop");
    };
    assert!(error.contains("container did not move"), "{error}");
    assert!(error.contains("after 2 scroll steps"), "{error}");
    assert_eq!(host.host().lock().unwrap().scrolls, 2);
}

#[test]
fn zero_scroll_delta_is_refused_before_running() {
    let recipe = Recipe::from_json(
        r#"{"name":"zero","steps":[{"id":"s1","op":"scroll_until","target":"list","until":"row-target"}]}"#,
    )
    .unwrap();
    let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
    assert!(err.to_string().contains("non-zero dx or dy"), "{err}");
}

#[test]
fn repeated_scroll_flags_are_refused() {
    for text in [
        "scroll_until list row --dy 1 --dy 2",
        "scroll_until list row --dy 1 --dx 2 --dx 3",
        "scroll_until list row --dy 1 --max-steps 2 --max-steps 3",
    ] {
        assert!(parse_wants(text, "x").is_err(), "{text}");
    }
}

#[test]
fn success_reports_node_and_a_later_step_can_use_it() {
    #[derive(Default)]
    struct PressingScrollHost {
        scrolls: usize,
        pressed: Vec<String>,
    }
    impl AgentHost for PressingScrollHost {
        fn hello(&self) -> HelloInfo {
            let mut hello = ScrollHost::default().hello();
            hello.features.push("press".into());
            hello
        }
        fn snapshot(&self) -> UiTree {
            let mut list = UiNode::new("list", "scroll", "List");
            if self.scrolls > 0 {
                list.children
                    .push(UiNode::new("row-target", "listitem", "Target"));
            }
            UiTree {
                app: "press-scroll".into(),
                platform: PlatformKind::Headless,
                ready: true,
                nodes: vec![list],
                rev: Some(u64::try_from(self.scrolls).unwrap_or(u64::MAX)),
            }
        }
        fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
            match op {
                Op::Scroll { .. } => self.scrolls += 1,
                Op::Press { target, .. } => self.pressed.push(target.clone().unwrap_or_default()),
                _ => {}
            }
            Ok(DispatchResult::empty())
        }
    }
    let host = TestHost::spawn(PressingScrollHost::default(), None).unwrap();
    let state = host.host();
    let recipe = Recipe::from_json(
        r#"{"name":"use-node","steps":[
            {"id":"find","op":"scroll_until","target":"list","until":"row-target","dy":120,"max_steps":3},
            {"id":"press","op":"press","target":"$find.node","needs":["find"]}
        ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(
        receipt.steps[0].result.as_ref().unwrap()["node"],
        "row-target"
    );
    assert_eq!(state.lock().unwrap().pressed, vec!["row-target"]);
}

#[test]
fn param_values_in_until_are_regex_literals() {
    let recipe = Recipe::from_json(
        r#"{"name":"escape","params":["row"],"steps":[{"id":"s1","op":"scroll_until","target":"list","until":"$row","dy":1,"max_steps":1}]}"#,
    )
    .unwrap();
    let mut set = BTreeMap::new();
    set.insert("row".into(), "Fee (1)".into());
    let plan = compile_plan(&recipe, &set, &todo_registry()).unwrap();
    assert_eq!(plan.steps[0].until.as_deref(), Some("Fee \\(1\\)"));
}

struct PipelineNodeHost {
    scrolls: usize,
}

impl AgentHost for PipelineNodeHost {
    fn hello(&self) -> HelloInfo {
        ScrollHost::default().hello()
    }
    fn snapshot(&self) -> UiTree {
        let mut list = UiNode::new("list", "scroll", "List");
        if self.scrolls > 0 {
            list.children
                .push(UiNode::new("row-target", "listitem", "Target"));
        }
        UiTree {
            app: "pipeline-node".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![list],
            rev: Some(u64::try_from(self.scrolls).unwrap_or(u64::MAX)),
        }
    }
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if matches!(op, Op::Scroll { .. }) {
            self.scrolls += 1;
        }
        Ok(DispatchResult::empty())
    }
}

#[test]
fn node_references_are_substituted_in_pipelined_read_waves() {
    let host = TestHost::spawn(PipelineNodeHost { scrolls: 0 }, None).unwrap();
    let recipe = Recipe::from_json(
        r#"{"name":"pipe-node","steps":[
            {"id":"find","op":"scroll_until","target":"list","until":"row-target","dy":120,"max_steps":2},
            {"id":"check","op":"assert","target":"$find.node","needs":["find"]},
            {"id":"wait","op":"wait_until","target":"$find.node","timeout_ms":50,"needs":["find"]}
        ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("pipelined run");
    assert!(receipt.ok, "{receipt:?}");
}

#[test]
fn zero_sized_child_bounds_do_not_count_inside_a_geometric_container() {
    let host = geometry(GeometryHost {
        target_y: Some(0.0),
        zero_target_bounds: true,
        ..GeometryHost::default()
    });
    let Err(RunError::Step { error, .. }) = run_geometry(&host) else {
        panic!("zero-sized target must not be found");
    };
    assert!(error.contains("not_found"), "{error}");
}

#[test]
fn scroll_until_not_found_has_its_own_error_code() {
    let host = host(None);
    let Err(RunError::Step { error_code, .. }) = run(&host, &json_recipe(1)) else {
        panic!("must not find");
    };
    assert_eq!(error_code, "scroll_until_not_found");
}

#[test]
fn node_reference_rules_are_checked_at_compile() {
    for (json, wanted) in [
        (
            r#"{"name":"x","steps":[{"id":"find","op":"scroll_until","target":"list","until":"row","dy":1},{"id":"use","op":"press","target":"$missing.node"}]}"#,
            "missing",
        ),
        (
            r#"{"name":"x","steps":[{"id":"plain","op":"snapshot"},{"id":"use","op":"press","target":"$plain.node"}]}"#,
            "scroll_until",
        ),
    ] {
        let recipe = Recipe::from_json(json).unwrap();
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
        assert!(err.to_string().contains(wanted), "{err}");
    }
}

#[test]
fn wants_node_reference_implies_the_dependency() {
    let recipe = parse_wants(
        "scroll_until list row-target --dy 1\nscroll_until other $s1.node --dy 1",
        "x",
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let second = plan.steps.iter().find(|step| step.id == "s2").unwrap();
    assert!(second.needs.iter().any(|need| need == "s1"));
}

struct UntilReferenceHost {
    scrolls: usize,
}

impl AgentHost for UntilReferenceHost {
    fn hello(&self) -> HelloInfo {
        ScrollHost::default().hello()
    }
    fn snapshot(&self) -> UiTree {
        let mut list = UiNode::new("list", "scroll", "List");
        if self.scrolls > 0 {
            list.children
                .push(UiNode::new("row-target", "listitem", "Target"));
        }
        let mut other = UiNode::new("other", "scroll", "Other");
        other
            .children
            .push(UiNode::new("row-target", "listitem", "Target"));
        UiTree {
            app: "until-reference".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![list, other],
            rev: Some(u64::try_from(self.scrolls).unwrap_or(u64::MAX)),
        }
    }
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if matches!(op, Op::Scroll { .. }) {
            self.scrolls += 1;
        }
        Ok(DispatchResult::empty())
    }
}

#[test]
fn node_references_inside_until_are_substituted_before_matching() {
    let host = TestHost::spawn(UntilReferenceHost { scrolls: 0 }, None).unwrap();
    let state = host.host();
    let recipe = Recipe::from_json(
        r#"{"name":"until-ref","steps":[
            {"id":"find","op":"scroll_until","target":"list","until":"row-target","dy":120,"max_steps":2},
            {"id":"reuse","op":"scroll_until","target":"other","until":"$find.node","dy":120,"max_steps":1}
        ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("until reference");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.steps[1].result.as_ref().unwrap()["steps"], 0);
    assert_eq!(state.lock().unwrap().scrolls, 1);
}
