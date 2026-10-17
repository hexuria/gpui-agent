//! M8.4: `gpui-agent run --goal … --escalate jev` (feature `jev`)
//! against a headless `TodoStore` and a std-only mock System One
//! server — no network, no SDK mock feature. The mock reads the
//! request's own criteria keys and answers probabilities that cover
//! exactly the offered set, so validation runs the real path.
#![cfg(feature = "jev")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gpui_agent::AgentHost;
use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use serde_json::{Value, json};
use todo_core::TodoStore;

struct Todo {
    addr: String,
    shutdown: Arc<AtomicBool>,
    store: Arc<Mutex<TodoStore>>,
}

impl Todo {
    fn spawn() -> Self {
        let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
        let (addr, shutdown) = spawn_host(
            "127.0.0.1:0".parse().unwrap(),
            Some("m8-token".into()),
            store.clone(),
        )
        .expect("bind");
        Self {
            addr: addr.to_string(),
            shutdown,
            store,
        }
    }

    fn seed(&self, titles: &[&str]) {
        let mut store = self.store.lock().unwrap();
        for title in titles {
            store
                .dispatch(&gpui_agent::Op::Invoke {
                    name: "todo.add".into(),
                    args: json!({ "title": title }),
                })
                .expect("seed add");
        }
    }

    fn titles(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .items()
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }
}

impl Drop for Todo {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// A minimal System One server: reads each POST body, hands it to the
/// responder, writes back its JSON. Records every request body.
struct MockJev {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockJev {
    /// `respond` maps a `POST /v1/systemone` request body to its
    /// response body. `answer(request)` builds the strict valid reply:
    /// one choice answer per question head, probabilities covering
    /// exactly the head's criteria keys, `picks[head]` as argmax.
    fn serve(respond: impl Fn(&Value) -> Value + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let requests = requests.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut sock, _)) => {
                            sock.set_nonblocking(false).ok();
                            let body = read_http_body(&mut sock);
                            let req: Value =
                                serde_json::from_str(&body).unwrap_or(json!({"_bad": true}));
                            requests.lock().unwrap().push(req.clone());
                            let body = serde_json::to_string(&respond(&req))
                                .unwrap_or_else(|_| "{}".into());
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = sock.write_all(resp.as_bytes());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            url: format!("http://127.0.0.1:{port}"),
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// Valid answers: `picks["operation"]` / `picks["<kind>_target"]`
    /// win their head; every other head argmaxes its first key.
    fn answering(picks: &[(&str, &str)]) -> impl Fn(&Value) -> Value {
        let picks: Vec<(String, String)> = picks
            .iter()
            .map(|(h, p)| (h.to_string(), p.to_string()))
            .collect();
        move |req: &Value| {
            let mut answers = serde_json::Map::new();
            if let Some(questions) = req["questions"].as_object() {
                for (head, q) in questions {
                    let keys: Vec<String> = q["criteria"]
                        .as_object()
                        .map(|c| c.keys().cloned().collect())
                        .unwrap_or_default();
                    if keys.is_empty() {
                        continue;
                    }
                    let pick = picks
                        .iter()
                        .find(|(h, _)| h == head)
                        .map(|(_, p)| p.clone())
                        .unwrap_or_else(|| keys[0].clone());
                    let mut probs = serde_json::Map::new();
                    for k in &keys {
                        probs.insert(
                            k.clone(),
                            json!(if *k == pick {
                                0.9
                            } else {
                                0.1 / (keys.len() - 1).max(1) as f64
                            }),
                        );
                    }
                    answers.insert(
                        head.clone(),
                        json!({"type":"choice","choice":pick,"confidence":0.9,"probabilities":probs}),
                    );
                }
            }
            json!({"model":"mock-jev","usage":{"input_tokens":1,"output_tokens":1},"answers":answers})
        }
    }

    /// Like `answering` but one criteria key is dropped from the
    /// probabilities — must fail coverage validation.
    fn answering_uncovered() -> impl Fn(&Value) -> Value {
        |req: &Value| {
            let mut answers = serde_json::Map::new();
            if let Some(questions) = req["questions"].as_object() {
                for (head, q) in questions {
                    let keys: Vec<String> = q["criteria"]
                        .as_object()
                        .map(|c| c.keys().cloned().collect())
                        .unwrap_or_default();
                    if keys.is_empty() {
                        continue;
                    }
                    let mut probs = serde_json::Map::new();
                    for (i, k) in keys.iter().enumerate() {
                        if i + 1 == keys.len() && keys.len() > 1 {
                            continue; // drop the last key — coverage violation
                        }
                        probs.insert(k.clone(), json!(1.0 / (keys.len() - 1).max(1) as f64));
                    }
                    answers.insert(
                        head.clone(),
                        json!({"type":"choice","choice":&keys[0],"confidence":0.9,"probabilities":probs}),
                    );
                }
            }
            json!({"model":"mock-jev","usage":{"input_tokens":1,"output_tokens":1},"answers":answers})
        }
    }

    /// Probabilities sum to ~0.5 — must fail the sum check.
    fn answering_bad_sum() -> impl Fn(&Value) -> Value {
        |req: &Value| {
            let mut answers = serde_json::Map::new();
            if let Some(questions) = req["questions"].as_object() {
                for (head, q) in questions {
                    let keys: Vec<String> = q["criteria"]
                        .as_object()
                        .map(|c| c.keys().cloned().collect())
                        .unwrap_or_default();
                    if keys.is_empty() {
                        continue;
                    }
                    let mut probs = serde_json::Map::new();
                    for k in &keys {
                        probs.insert(k.clone(), json!(0.5 / keys.len() as f64));
                    }
                    answers.insert(
                        head.clone(),
                        json!({"type":"choice","choice":&keys[0],"confidence":0.5,"probabilities":probs}),
                    );
                }
            }
            json!({"model":"mock-jev","usage":{"input_tokens":1,"output_tokens":1},"answers":answers})
        }
    }

    /// The named choice is not the argmax — must fail argmax check.
    fn answering_not_argmax() -> impl Fn(&Value) -> Value {
        |req: &Value| {
            let mut answers = serde_json::Map::new();
            if let Some(questions) = req["questions"].as_object() {
                for (head, q) in questions {
                    let keys: Vec<String> = q["criteria"]
                        .as_object()
                        .map(|c| c.keys().cloned().collect())
                        .unwrap_or_default();
                    if keys.len() < 2 {
                        continue;
                    }
                    let mut probs = serde_json::Map::new();
                    for k in &keys {
                        probs.insert(
                            k.clone(),
                            json!(if *k == keys[0] {
                                0.4
                            } else if *k == keys[1] {
                                0.6
                            } else {
                                0.0
                            }),
                        );
                    }
                    // choice = keys[0] (0.4) while keys[1] (0.6) is higher
                    answers.insert(
                        head.clone(),
                        json!({"type":"choice","choice":&keys[0],"confidence":0.4,"probabilities":probs}),
                    );
                }
            }
            json!({"model":"mock-jev","usage":{"input_tokens":1,"output_tokens":1},"answers":answers})
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockJev {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Read one HTTP/1.1 request's body (Content-Length framed).
fn read_http_body(sock: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = sock.read(&mut tmp).unwrap_or(0);
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let len = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= pos + 4 + len {
                return String::from_utf8_lossy(&buf[pos + 4..pos + 4 + len]).to_string();
            }
        }
        if n == 0 {
            return String::from_utf8_lossy(&buf).to_string();
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn run_goal_jev(
    todo: &Todo,
    goal: &str,
    mock: Option<&MockJev>,
    with_key: bool,
) -> (i32, Value, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_gpui-agent"));
    cmd.args([
        "--addr",
        &todo.addr,
        "--token",
        "m8-token",
        "run",
        "--goal",
        goal,
        "--json",
        "--escalate",
        "jev",
    ]);
    if with_key {
        cmd.env("TYPESAFE_API_KEY", "test-key");
        cmd.env("TYPESAFE_MODEL", "jev-test-model");
    }
    if let Some(m) = mock {
        cmd.env("TYPESAFE_BASE_URL", &m.url);
    }
    let out = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_remove("GPUI_AGENT_TOKEN")
        .output()
        .expect("spawn gpui-agent run");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let receipt: Value = serde_json::from_str(stdout.trim()).unwrap_or(json!({"_nojson": true}));
    (code, receipt, stderr)
}

/// A clause the local policy abstains on: the named row does not exist.
const ABSTAIN_GOAL: &str = r#"Delete "Walk dog""#;

#[test]
fn jev_pick_from_the_menu_runs() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let mock = MockJev::serve(MockJev::answering(&[
        ("operation", "CLICK"),
        ("click_target", "nav-settings"),
    ]));
    let (code, receipt, err) = run_goal_jev(&todo, ABSTAIN_GOAL, Some(&mock), true);
    assert_eq!(code, 2, "{receipt} {err}");
    // The escalated CLICK dispatches repeatedly until a bounded stop —
    // the no-effect tripwire or step cap ends the run.
    let outcome = receipt["outcome"].as_str().unwrap_or("");
    assert!(outcome == "cap_hit" || outcome == "blocked", "{receipt}");
    let steps = receipt["steps"].as_array().expect("steps");
    assert_eq!(steps[0]["id"], "CLICK:nav-settings", "{receipt}");
    assert_eq!(steps[0]["confidence_millis"], 0, "escalated pick");
    // The System One request carried operation + target heads, plus
    // the env-selected model.
    let reqs = mock.requests();
    assert!(!reqs.is_empty(), "mock saw no calls");
    assert_eq!(reqs[0]["model"], json!("jev-test-model"), "{reqs:?}");
    let questions = reqs[0]["questions"].as_object().expect("questions");
    assert!(questions.contains_key("operation"), "{reqs:?}");
    assert!(questions.contains_key("click_target"), "{reqs:?}");
    let op_criteria: Vec<&String> = questions["operation"]["criteria"]
        .as_object()
        .expect("criteria")
        .keys()
        .collect();
    for kind in [
        "CLICK",
        "TYPE",
        "SET_VALUE",
        "KEY",
        "KEYBINDING",
        "DONE",
        "BLOCKED",
        "WAIT",
    ] {
        assert!(op_criteria.contains(&&kind.to_string()), "{op_criteria:?}");
    }
}

#[test]
fn jev_off_menu_answer_blocks() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let mock = MockJev::serve(MockJev::answering(&[("operation", "GHOST")]));
    let (code, receipt, _) = run_goal_jev(&todo, ABSTAIN_GOAL, Some(&mock), true);
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "blocked", "{receipt}");
    assert!(
        receipt["reason"]
            .as_str()
            .unwrap_or("")
            .contains("escalation:"),
        "{receipt}"
    );
    assert_eq!(todo.titles(), ["Buy milk"]);
}

#[test]
fn jev_uncovered_set_blocks() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let mock = MockJev::serve(MockJev::answering_uncovered());
    let (code, receipt, _) = run_goal_jev(&todo, ABSTAIN_GOAL, Some(&mock), true);
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "blocked", "{receipt}");
    assert!(
        receipt["reason"]
            .as_str()
            .unwrap_or("")
            .contains("do not cover"),
        "{receipt}"
    );
}

#[test]
fn jev_bad_probability_sum_blocks() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let mock = MockJev::serve(MockJev::answering_bad_sum());
    let (code, receipt, _) = run_goal_jev(&todo, ABSTAIN_GOAL, Some(&mock), true);
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "blocked", "{receipt}");
    assert!(
        receipt["reason"].as_str().unwrap_or("").contains("sum"),
        "{receipt}"
    );
}

#[test]
fn jev_non_argmax_blocks() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let mock = MockJev::serve(MockJev::answering_not_argmax());
    let (code, receipt, _) = run_goal_jev(&todo, ABSTAIN_GOAL, Some(&mock), true);
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "blocked", "{receipt}");
    assert!(
        receipt["reason"].as_str().unwrap_or("").contains("argmax"),
        "{receipt}"
    );
}

#[test]
fn jev_never_called_when_instinct_chooses() {
    let todo = Todo::spawn();
    let mock = MockJev::serve(MockJev::answering(&[("operation", "DONE")]));
    let (code, receipt, _) = run_goal_jev(&todo, r#"Add a todo "Buy milk""#, Some(&mock), true);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["outcome"], "done", "{receipt}");
    assert!(
        mock.requests().is_empty(),
        "jev must not run on local choice"
    );
}

#[test]
fn missing_typesafe_key_is_a_clear_error() {
    let todo = Todo::spawn();
    let (code, receipt, stderr) = run_goal_jev(&todo, ABSTAIN_GOAL, None, false);
    assert_eq!(code, 1, "{receipt} {stderr}");
    assert!(
        stderr.contains("TYPESAFE_API_KEY"),
        "clear config error: {stderr}"
    );
}
