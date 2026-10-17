//! Wire-compat goldens (PRD M1.6): the bytes an existing client sends and
//! parses must not drift. Every op present at `bench-baseline` has a pinned
//! serialized request line here; response shapes are pinned the same way.
//! If a change legitimately alters the wire format, update these strings in
//! the same commit with an explanation — never silently.

use gpui_agent::protocol::{AssertSpec, KeybindingScope, Op, PROTOCOL_VERSION, Request, Response};

fn line(op: Op) -> String {
    serde_json::to_string(&Request::new("1", op)).expect("serialize")
}

#[test]
fn protocol_version_is_two() {
    assert_eq!(PROTOCOL_VERSION, 2, "I2: protocol stays additive at v2");
}

#[test]
fn request_bytes_are_pinned() {
    let cases: [(Op, &str); 19] = [
        (Op::Hello, r#"{"v":2,"id":"1","op":"hello"}"#),
        (Op::Snapshot, r#"{"v":2,"id":"1","op":"snapshot"}"#),
        (
            Op::click("todo-add"),
            r#"{"v":2,"id":"1","op":"click","target":"todo-add"}"#,
        ),
        (
            Op::click_virtual("todo-add"),
            r#"{"v":2,"id":"1","op":"click","target":"todo-add","delivery":"virtual"}"#,
        ),
        (
            Op::type_text("todo-input", "hi"),
            r#"{"v":2,"id":"1","op":"type","target":"todo-input","text":"hi"}"#,
        ),
        (
            Op::SetValue {
                target: "todo-input".into(),
                value: "milk".into(),
            },
            r#"{"v":2,"id":"1","op":"set_value","target":"todo-input","value":"milk"}"#,
        ),
        (
            Op::key("todo-input", "enter"),
            r#"{"v":2,"id":"1","op":"key","target":"todo-input","key":"enter"}"#,
        ),
        (Op::Keybindings, r#"{"v":2,"id":"1","op":"keybindings"}"#),
        (
            Op::Invoke {
                name: "todo.add".into(),
                args: serde_json::json!({"title": "x"}),
            },
            r#"{"v":2,"id":"1","op":"invoke","name":"todo.add","args":{"title":"x"}}"#,
        ),
        (
            Op::Wait { timeout_ms: None },
            r#"{"v":2,"id":"1","op":"wait","timeout_ms":null}"#,
        ),
        (
            Op::Wait {
                timeout_ms: Some(500),
            },
            r#"{"v":2,"id":"1","op":"wait","timeout_ms":500}"#,
        ),
        (Op::Shutdown, r#"{"v":2,"id":"1","op":"shutdown"}"#),
        (
            Op::screenshot("out.png"),
            r#"{"v":2,"id":"1","op":"screenshot","path":"out.png"}"#,
        ),
        (
            Op::screenshot_scrolled("tall.png", "todo-list-scroll", Some(4096)),
            r#"{"v":2,"id":"1","op":"screenshot","path":"tall.png","mode":"scrolled","target":"todo-list-scroll","max_height_px":4096}"#,
        ),
        (
            Op::keybinding("quit", KeybindingScope::Focused),
            r#"{"v":2,"id":"1","op":"keybinding","binding":"quit","scope":"focused"}"#,
        ),
        (
            Op::wait_until(
                AssertSpec {
                    target: "todo-add".into(),
                    ..AssertSpec::default()
                },
                1000,
            ),
            r#"{"v":2,"id":"1","op":"wait_until","timeout_ms":1000,"target":"todo-add"}"#,
        ),
        (
            Op::Assert {
                spec: AssertSpec {
                    target: "todo-nav".into(),
                    visible: Some(true),
                    ..AssertSpec::default()
                },
            },
            r#"{"v":2,"id":"1","op":"assert","target":"todo-nav","visible":true}"#,
        ),
        (
            Op::type_text_virtual("todo-input", "x"),
            r#"{"v":2,"id":"1","op":"type","target":"todo-input","text":"x","delivery":"virtual"}"#,
        ),
        (
            Op::key_virtual("todo-input", "enter"),
            r#"{"v":2,"id":"1","op":"key","target":"todo-input","key":"enter","delivery":"virtual"}"#,
        ),
    ];
    for (op, want) in cases {
        assert_eq!(line(op), want, "request wire bytes changed");
    }
}

#[test]
fn keybinding_with_all_fields_is_pinned() {
    let req = Request::new(
        "7",
        Op::Keybinding {
            binding: "quit".into(),
            chord: Some("cmd-q".into()),
            scope: KeybindingScope::Global,
            confirm: true,
            activate: true,
        },
    );
    assert_eq!(
        serde_json::to_string(&req).unwrap(),
        r#"{"v":2,"id":"7","op":"keybinding","binding":"quit","chord":"cmd-q","scope":"global","confirm":true,"activate":true}"#
    );
}

#[test]
fn token_and_auth_fields_stay_in_position() {
    let req = Request::new("1", Op::Hello).with_token("t").with_auth("a");
    assert_eq!(
        serde_json::to_string(&req).unwrap(),
        r#"{"v":2,"id":"1","token":"t","auth":"a","op":"hello"}"#
    );
}

#[test]
fn response_bytes_are_pinned() {
    assert_eq!(
        serde_json::to_string(&Response::ok("1")).unwrap(),
        r#"{"v":2,"id":"1","ok":true}"#
    );
    assert_eq!(
        serde_json::to_string(&Response::err("1", "nope")).unwrap(),
        r#"{"v":2,"id":"1","ok":false,"error":"nope"}"#
    );
}

#[test]
fn legacy_clients_still_parse() {
    // v1 click: no delivery field, no `v` version bump required on read.
    let req: Request =
        serde_json::from_str(r#"{"v":1,"id":"1","op":"click","target":"todo-add"}"#).unwrap();
    assert!(req.op.delivery().is_semantic());
    // keybinding.list alias stays accepted on the wire.
    let req: Request = serde_json::from_str(r#"{"v":2,"id":"1","op":"keybinding.list"}"#).unwrap();
    assert!(matches!(req.op, Op::Keybindings));
}
