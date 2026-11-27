//! Asserting exact node states from recipes.

use std::collections::BTreeMap;

use gpui_agent::protocol::Op;
use gpui_agent_recipe::plan::compile_plan;
use gpui_agent_recipe::recipe::{Recipe, parse_wants};
use gpui_agent_recipe::todo_registry;

fn assert_spec(recipe: &Recipe) -> gpui_agent::protocol::AssertSpec {
    let plan = compile_plan(recipe, &BTreeMap::new(), &todo_registry()).expect("plan");
    let Op::Assert { spec } = &plan.steps[0].op else {
        panic!("not assert");
    };
    spec.clone()
}

#[test]
fn json_and_wants_parse_state_and_states() {
    let json = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"assert","target":"field","state":"masked","states":["selection: 0-9","field: amount"]}]}"#,
    )
    .unwrap();
    let spec = assert_spec(&json);
    assert_eq!(spec.state.as_deref(), Some("masked"));
    assert_eq!(spec.states, ["selection: 0-9", "field: amount"]);

    let wants = parse_wants(r#"assert field state="selection: 0-9""#, "s").unwrap();
    let spec = assert_spec(&wants);
    assert_eq!(spec.state.as_deref(), Some("selection: 0-9"));
}

#[test]
fn wait_until_uses_the_same_state_fields() {
    let recipe = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"wait_until","target":"field","states":["masked"],"timeout_ms":100}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let Op::WaitUntil { spec, .. } = &plan.steps[0].op else {
        panic!("not wait_until");
    };
    assert_eq!(spec.states, ["masked"]);
}

#[test]
fn absent_state_fields_do_not_change_the_plan_fingerprint() {
    let plain = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"assert","target":"field","name":"Amount"}]}"#,
    )
    .unwrap();
    let empty = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"assert","target":"field","name":"Amount","states":[]}]}"#,
    )
    .unwrap();
    let a = compile_plan(&plain, &BTreeMap::new(), &todo_registry()).unwrap();
    let b = compile_plan(&empty, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
}

fn spawn_feature_host(
    features: &[&str],
    requests: usize,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use gpui_agent::protocol::{HelloAuth, HelloInfo, PROTOCOL_VERSION, PlatformKind, Response};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let thread_seen = Arc::clone(&seen);
    let features = features
        .iter()
        .map(|feature| (*feature).to_string())
        .collect::<Vec<_>>();
    std::thread::spawn(move || {
        let (mut writer, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(writer.try_clone().expect("clone"));
        for _ in 0..requests {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("request") == 0 {
                break;
            }
            let request: serde_json::Value = serde_json::from_str(&line).expect("json");
            thread_seen
                .lock()
                .expect("seen")
                .push(request["op"].as_str().unwrap_or("").to_string());
            let mut response = Response::ok(request["id"].as_str().expect("id"));
            if request["op"] == "hello" {
                response.hello = Some(HelloInfo {
                    protocol: PROTOCOL_VERSION,
                    app: "states".into(),
                    platform: PlatformKind::Headless,
                    os: "test".into(),
                    ready: true,
                    deliveries: Vec::new(),
                    auth: HelloAuth::None,
                    bundle_id: None,
                    executable: None,
                    features: features.clone(),
                    framework: None,
                    device: None,
                });
            }
            serde_json::to_writer(&mut writer, &response).expect("write");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
        }
    });
    (addr, seen)
}

#[test]
fn state_assertions_are_preflighted_against_assert_states_capability() {
    use std::time::Duration;

    use gpui_agent::AgentClient;
    use gpui_agent_recipe::{RunOptions, run_plan};

    let recipe = Recipe::from_json(
        r#"{"name":"states","steps":[{"id":"check","op":"assert","target":"field","state":"masked"}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();

    let (addr, seen) = spawn_feature_host(&[], 1);
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    let error = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    assert!(error.to_string().contains("assert_states"), "{error}");
    assert_eq!(seen.lock().expect("seen").as_slice(), ["hello"]);

    let (addr, seen) = spawn_feature_host(&["assert_states"], 2);
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("capable host");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(seen.lock().expect("seen").as_slice(), ["hello", "assert"]);
}
