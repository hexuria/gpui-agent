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
    let cases: [(Op, &str); 20] = [
        (Op::Hello, r#"{"v":2,"id":"1","op":"hello"}"#),
        (Op::snapshot(), r#"{"v":2,"id":"1","op":"snapshot"}"#),
        (Op::Revision, r#"{"v":2,"id":"1","op":"revision"}"#),
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
                expect_fingerprint: None,
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

/// `error_code`, `hint`, `delivery_status` and `after` are additive: a
/// reply without them parses, and a coded reply adds only the two fields.
#[test]
fn error_code_fields_are_additive() {
    let old: Response =
        serde_json::from_str(r#"{"v":2,"id":"1","ok":false,"error":"node `a` not found"}"#)
            .unwrap();
    assert!(old.error_code.is_none() && old.hint.is_none());
    assert_eq!(old.code(), Some("not_found"));
    assert_eq!(
        serde_json::to_string(&Response::err_coded("1", "stale_target: `a` changed")).unwrap(),
        r#"{"v":2,"id":"1","ok":false,"error":"stale_target: `a` changed","error_code":"stale_target","hint":"take a new snapshot and retry"}"#
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

#[test]
fn added_ops_keep_their_wire_names_and_omit_unset_fields() {
    use gpui_agent::{Op, Request, SwipeDirection, SystemAction};
    let line = |op: Op| serde_json::to_string(&Request::new("1", op)).unwrap();
    assert_eq!(
        line(Op::Press {
            target: Some("save".into()),
            x: None,
            y: None,
            within: None,
            expect_fingerprint: None,
        }),
        r#"{"v":2,"id":"1","op":"press","target":"save"}"#
    );
    assert_eq!(
        line(Op::Swipe {
            target: None,
            direction: SwipeDirection::Up,
            distance: None,
        }),
        r#"{"v":2,"id":"1","op":"swipe","direction":"up"}"#
    );
    assert_eq!(
        line(Op::System {
            action: SystemAction::KeyboardHide,
            value: None,
        }),
        r#"{"v":2,"id":"1","op":"system","action":"keyboard_hide"}"#
    );
    // A screenshot without `inline` is byte-identical to the 0.1 request.
    assert_eq!(
        line(Op::screenshot("a.png")),
        r#"{"v":2,"id":"1","op":"screenshot","path":"a.png"}"#
    );
}

#[test]
fn hello_from_a_0_1_host_still_parses_and_new_fields_are_omitted_when_unset() {
    let old = r#"{"protocol":2,"app":"x","platform":"mobile","os":"android","ready":true,"auth":"required"}"#;
    let hello: gpui_agent::HelloInfo = serde_json::from_str(old).unwrap();
    assert!(hello.framework.is_none() && hello.device.is_none() && hello.features.is_empty());
    let again = serde_json::to_string(&hello).unwrap();
    assert!(!again.contains("framework") && !again.contains("device"));
}

#[test]
fn gesture_ops_fall_back_to_the_invoke_names_0_1_hosts_knew() {
    use gpui_agent::Op;
    let legacy = Op::Scroll {
        target: Some("list".into()),
        x: None,
        y: None,
        dx: 0.0,
        dy: 120.0,
    }
    .legacy_invoke()
    .unwrap();
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::json!({"op":"invoke","name":"scroll","args":{"target":"list","dx":0.0,"dy":120.0}})
    );
    // Ops no 0.1 host had are never rewritten.
    assert!(
        Op::Paste {
            target: "a".into(),
            text: "b".into()
        }
        .legacy_invoke()
        .is_none()
    );
}

#[test]
fn float_fields_parse_from_integers_floats_and_null() {
    use gpui_agent::{Op, Request};
    let req: Request = serde_json::from_str(
        r#"{"v":2,"id":"1","op":"scroll","target":"list","dx":0,"dy":-120.5,"x":null}"#,
    )
    .unwrap();
    match req.op {
        Op::Scroll { dx, dy, x, y, .. } => {
            assert_eq!((dx, dy, x, y), (0.0, -120.5, None, None));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        serde_json::from_str::<Request>(r#"{"v":2,"id":"1","op":"pinch","scale":"big"}"#).is_err()
    );
}
