use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use gpui_agent::client::AgentClient;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::{HelloInfo, Op, PROTOCOL_VERSION, PlatformKind};
use gpui_agent::server::{AgentServer, ServerLimits};
use gpui_agent::tree::UiTree;
use gpui_agent::{DeliveryMode, DispatchResult};
use gpui_agent_recipe::{Recipe, compile_plan, parse_wants, run_plan, todo_registry};

fn wants_body(n: usize) -> String {
    let mut body = String::from("wait\n");
    for i in 0..n {
        body.push_str(&format!("invoke todo.add title=item-{i}\n"));
    }
    body
}

fn json_recipe(n: usize) -> String {
    let mut steps = Vec::with_capacity(n + 1);
    steps.push(serde_json::json!({"id": "wait", "op": "wait"}));
    for i in 0..n {
        let needs = if i == 0 {
            vec!["wait".to_string()]
        } else {
            vec![format!("add-{i}")]
        };
        let id = format!("add-{}", i + 1);
        steps.push(serde_json::json!({
            "id": id,
            "op": "invoke",
            "name": "todo.add",
            "args": {"title": format!("item-{i}")},
            "needs": needs
        }));
    }
    serde_json::json!({"name": "many", "steps": steps}).to_string()
}

fn wide_dag_json(n: usize) -> String {
    let mut steps = vec![serde_json::json!({"id": "root", "op": "hello"})];
    let mut leaves = Vec::new();
    for i in 0..n {
        let id = format!("leaf-{i}");
        leaves.push(id.clone());
        steps.push(serde_json::json!({
            "id": id,
            "op": "snapshot",
            "needs": ["root"]
        }));
    }
    steps.push(serde_json::json!({
        "id": "join",
        "op": "assert",
        "target": "todo-window",
        "needs": leaves
    }));
    serde_json::json!({"name": "wide", "steps": steps}).to_string()
}

fn compile_linear(c: &mut Criterion) {
    let body = wants_body(40);
    let recipe = parse_wants(&body, "many").unwrap();
    let registry = todo_registry();
    c.bench_function("compile_40_invoke_adds", |b| {
        b.iter(|| compile_plan(black_box(&recipe), &BTreeMap::new(), &registry).unwrap());
    });

    let body256 = wants_body(255);
    let recipe256 = parse_wants(&body256, "cap").unwrap();
    c.bench_function("compile_255_invoke_adds", |b| {
        b.iter(|| compile_plan(black_box(&recipe256), &BTreeMap::new(), &registry).unwrap());
    });
}

fn parse_paths(c: &mut Criterion) {
    let wants = wants_body(40);
    let json = json_recipe(40);
    c.bench_function("parse_wants_40_invoke_adds", |b| {
        b.iter(|| parse_wants(black_box(&wants), "many").unwrap());
    });
    c.bench_function("parse_json_40_invoke_adds", |b| {
        b.iter(|| Recipe::from_json(black_box(&json)).unwrap());
    });
    let mut set = BTreeMap::new();
    set.insert("title".into(), "Buy milk".into());
    let parameterized = r#"
set-value todo-input $title
click todo-add
assert todo-item-1 name=$title
"#;
    let recipe = parse_wants(parameterized, "p").unwrap();
    c.bench_function("apply_params_three_steps", |b| {
        b.iter(|| gpui_agent_recipe::apply_params(black_box(&recipe), black_box(&set)).unwrap());
    });
}

fn compile_dag(c: &mut Criterion) {
    let recipe = Recipe::from_json(
        r#"{
        "name": "dag",
        "steps": [
            {"id": "a", "op": "hello"},
            {"id": "b", "op": "snapshot"},
            {"id": "c", "op": "assert", "target": "todo-window", "needs": ["a", "b"]},
            {"id": "d", "op": "invoke", "name": "todo.list", "needs": ["c"]}
        ]
    }"#,
    )
    .unwrap();
    let registry = todo_registry();
    c.bench_function("compile_small_dag", |b| {
        b.iter(|| compile_plan(black_box(&recipe), &BTreeMap::new(), &registry).unwrap());
    });

    let wide = Recipe::from_json(&wide_dag_json(32)).unwrap();
    c.bench_function("compile_wide_dag_32_leaves", |b| {
        b.iter(|| compile_plan(black_box(&wide), &BTreeMap::new(), &registry).unwrap());
    });
}

fn cli_recipe_path(c: &mut Criterion) {
    let wants = wants_body(40);
    let registry = todo_registry();
    c.bench_function("cli_validate_plan_wants_40", |b| {
        b.iter(|| {
            let recipe = parse_wants(black_box(&wants), "many").unwrap();
            gpui_agent_recipe::validate_recipe(&recipe, &registry).unwrap();
            compile_plan(&recipe, &BTreeMap::new(), &registry).unwrap()
        });
    });
}

struct EmptyHost;

impl AgentHost for EmptyHost {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            app: "bench".into(),
            platform: PlatformKind::Headless,
            ready: true,
            deliveries: vec![DeliveryMode::Semantic],
            auth: gpui_agent::HelloAuth::None,
        }
    }

    fn snapshot(&self) -> UiTree {
        UiTree {
            app: "bench".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![],
        }
    }

    fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
        Ok(DispatchResult::empty())
    }
}

fn recipe_run_session(c: &mut Criterion) {
    let host = Arc::new(Mutex::new(EmptyHost));
    let server = AgentServer::bind_with_limits(
        "127.0.0.1:0".parse().unwrap(),
        None,
        ServerLimits {
            idle_timeout: Duration::from_secs(5),
            ..ServerLimits::default()
        },
    )
    .unwrap();
    let addr = server.local_addr().unwrap();
    std::thread::spawn(move || server.serve_host(host));

    let mut hellos = String::new();
    for _ in 0..32 {
        hellos.push_str("hello\n");
    }
    let recipe = parse_wants(&hellos, "hellos").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();

    c.bench_function("recipe_run_32_hellos_one_session", |b| {
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        b.iter(|| {
            let receipt = run_plan(black_box(&mut client), black_box(&plan), false).unwrap();
            black_box(receipt.ok);
        });
    });

    let mut hello_steps = vec![serde_json::json!({"id": "root", "op": "wait"})];
    for i in 0..32 {
        hello_steps.push(serde_json::json!({
            "id": format!("h{i}"),
            "op": "hello",
            "needs": ["root"]
        }));
    }
    let hello_wide = Recipe::from_json(
        &serde_json::json!({"name": "wide-hello", "steps": hello_steps}).to_string(),
    )
    .unwrap();
    let hello_plan = compile_plan(&hello_wide, &BTreeMap::new(), &todo_registry()).unwrap();
    c.bench_function("recipe_run_32_hello_wave_sequential", |b| {
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        b.iter(|| {
            let receipt = run_plan(black_box(&mut client), black_box(&hello_plan), false).unwrap();
            black_box(receipt.ok);
        });
    });
}

criterion_group!(
    benches,
    compile_linear,
    parse_paths,
    compile_dag,
    cli_recipe_path,
    recipe_run_session
);
criterion_main!(benches);
