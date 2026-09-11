// Apply this test onto `243ab78` (R1 / pre-R4: 2-arg `authorize_request`,
// PROTOCOL_VERSION = 1, matching `Request.token` is accepted).
//
// It must fail because v1 `authorize_request` **accepts** `Request.token`,
// not because `v` is 2 vs 1. Do not change PROTOCOL_VERSION in the request;
// `Request::new` uses the tree's PROTOCOL_VERSION.
//
// cargo test -p gpui-agent --lib dispatch::tests::v2_raw_token_on_wire_is_rejected -- --exact

#[test]
fn v2_raw_token_on_wire_is_rejected() {
    let req = Request::new("1", Op::Hello).with_token("secret");
    assert_eq!(
        req.v, PROTOCOL_VERSION,
        "this red must not be a version mismatch"
    );
    let err = authorize_request(&req, Some("secret")).unwrap_err();
    let msg = err.error.as_deref().unwrap_or("");
    assert!(
        msg.contains("token must not be sent on the wire"),
        "{err:?}"
    );
    assert!(
        !msg.contains("unsupported protocol version"),
        "must reject the token field, not the version: {msg}"
    );
}
