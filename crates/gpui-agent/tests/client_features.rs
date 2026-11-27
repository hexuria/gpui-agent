//! Client feature coverage against a live loopback server.
#![expect(
    clippy::unwrap_used,
    reason = "a failed setup/assert aborts the test anyway"
)]

use std::{
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::Duration,
};

use gpui_agent::{
    AgentClient, HelloAuth, HelloInfo, PROTOCOL_VERSION, PlatformKind, Response, ScreenshotMode,
    challenge_for_nonce, hmac_hex, is_unsupported_op,
    protocol::{AssertSpec, Op},
};

fn read_request(reader: &mut BufReader<TcpStream>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn accept_with_timeout(listener: &TcpListener) -> TcpStream {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("restore blocking mode on accepted stream");
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("accept timed out: {error}"),
        }
    }
}

fn respond(writer: &mut TcpStream, request: &serde_json::Value, hello: Option<HelloInfo>) {
    let mut response = Response::ok(request["id"].as_str().unwrap());
    response.hello = hello;
    serde_json::to_writer(&mut *writer, &response).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
}

fn send_challenge(writer: &mut TcpStream, nonce: &[u8; 32]) {
    serde_json::to_writer(&mut *writer, &challenge_for_nonce(nonce)).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
}

fn hello(features: &[&str]) -> HelloInfo {
    HelloInfo {
        protocol: PROTOCOL_VERSION,
        app: "fake".into(),
        platform: PlatformKind::Headless,
        os: "test".into(),
        ready: true,
        deliveries: Vec::new(),
        auth: HelloAuth::None,
        bundle_id: None,
        executable: None,
        features: features.iter().map(|feature| (*feature).into()).collect(),
        framework: None,
        device: None,
    }
}

fn press() -> Op {
    Op::Press {
        target: Some("button".into()),
        x: None,
        y: None,
        within: None,
        expect_fingerprint: None,
    }
}

#[test]
fn screenshot_bytes_refuses_hosts_without_inline_screenshot_support() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut writer, _) = listener.accept().unwrap();
        writer
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "hello");
        respond(&mut writer, &request, Some(hello(&[])));

        let mut line = String::new();
        let error = reader.read_line(&mut line).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ));
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    let error = client
        .screenshot_bytes(ScreenshotMode::Viewport, None, None)
        .unwrap_err();
    assert!(is_unsupported_op(&error), "{error}");
    server.join().unwrap();
}

#[test]
fn gated_call_renegotiates_after_the_host_restarts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut old_writer, _) = listener.accept().unwrap();
        let mut old_reader = BufReader::new(old_writer.try_clone().unwrap());
        let request = read_request(&mut old_reader);
        assert_eq!(request["op"], "hello");
        respond(
            &mut old_writer,
            &request,
            Some(hello(&["press", "screenshot_inline"])),
        );
        let request = read_request(&mut old_reader);
        assert_eq!(request["op"], "press");
        respond(&mut old_writer, &request, None);
        drop(old_reader);
        drop(old_writer);
        closed_tx.send(()).unwrap();

        let (mut new_writer, _) = listener.accept().unwrap();
        new_writer
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut new_reader = BufReader::new(new_writer.try_clone().unwrap());
        let request = read_request(&mut new_reader);
        assert_eq!(request["op"], "hello");
        respond(
            &mut new_writer,
            &request,
            Some(hello(&["screenshot_inline"])),
        );

        let mut line = String::new();
        let error = new_reader.read_line(&mut line).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ));
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
    assert!(client.gated(press()).unwrap().ok);
    closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    client.close_session();

    let error = client.gated(press()).unwrap_err();
    assert!(is_unsupported_op(&error), "{error}");
    server.join().unwrap();
}

#[test]
fn gated_call_refreshes_cached_rejections_after_the_host_restarts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut old_writer = accept_with_timeout(&listener);
        let mut old_reader = BufReader::new(old_writer.try_clone().unwrap());
        let request = read_request(&mut old_reader);
        assert_eq!(request["op"], "hello");
        respond(
            &mut old_writer,
            &request,
            Some(hello(&["screenshot_inline"])),
        );
        let request = read_request(&mut old_reader);
        assert_eq!(request["op"], "screenshot");
        assert!(request["inline"].as_bool().unwrap());
        respond(&mut old_writer, &request, None);
        old_writer.shutdown(Shutdown::Both).unwrap();
        drop(old_reader);
        drop(old_writer);
        closed_tx.send(()).unwrap();

        let mut new_writer = accept_with_timeout(&listener);
        let mut new_reader = BufReader::new(new_writer.try_clone().unwrap());
        let request = read_request(&mut new_reader);
        assert_eq!(request["op"], "hello");
        respond(
            &mut new_writer,
            &request,
            Some(hello(&["screenshot_inline", "press"])),
        );
        let request = read_request(&mut new_reader);
        assert_eq!(request["op"], "press");
        respond(&mut new_writer, &request, None);
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
    let screenshot = Op::Screenshot {
        path: None,
        mode: ScreenshotMode::Viewport,
        target: None,
        max_height_px: None,
        inline: true,
    };
    assert!(client.gated(screenshot).unwrap().ok);
    closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(client.gated(press()).unwrap().ok);
    server.join().unwrap();
}

#[test]
fn press_within_is_refused_without_the_capability_and_sent_with_it() {
    use gpui_agent::protocol::PointOffset;

    let within_press = || Op::Press {
        target: Some("button".into()),
        x: None,
        y: None,
        within: Some(PointOffset { x: 12.0, y: 8.0 }),
        expect_fingerprint: None,
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut writer = accept_with_timeout(&listener);
        writer
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "hello");
        respond(&mut writer, &request, Some(hello(&["press"])));
        let mut line = String::new();
        let error = reader.read_line(&mut line).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ));
    });
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    let error = client.gated(within_press()).unwrap_err();
    assert!(is_unsupported_op(&error), "{error}");
    assert!(error.contains("press_within"), "{error}");
    server.join().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut writer = accept_with_timeout(&listener);
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "hello");
        respond(
            &mut writer,
            &request,
            Some(hello(&["press", "press_within"])),
        );
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "press");
        assert_eq!(request["within"]["x"], 12.0);
        assert_eq!(request["within"]["y"], 8.0);
        respond(&mut writer, &request, None);
    });
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    assert!(client.gated(within_press()).unwrap().ok);
    server.join().unwrap();
}

#[test]
fn pipeline_wait_uses_the_longest_operation_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut writer = accept_with_timeout(&listener);
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let wait_request = read_request(&mut reader);
        assert_eq!(wait_request["op"], "wait_until");
        let other_request = read_request(&mut reader);
        assert_eq!(other_request["op"], "snapshot");
        thread::sleep(Duration::from_millis(250));
        respond(&mut writer, &wait_request, None);
        respond(&mut writer, &other_request, None);
    });

    let wait = Op::WaitUntil {
        timeout_ms: 1_000,
        spec: AssertSpec {
            target: "button".into(),
            exists: Some(true),
            ..AssertSpec::default()
        },
    };
    let snapshot = Op::snapshot();
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_millis(100));
    let responses = client.rpc_pipeline(&[&wait, &snapshot]).unwrap();
    assert_eq!(responses.len(), 2);
    assert!(responses.iter().all(|response| response.ok));
    server.join().unwrap();
}

#[test]
fn zero_timeout_wait_still_attempts_the_host() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut writer = accept_with_timeout(&listener);
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "wait");
        assert_eq!(request["timeout_ms"], 0);
        respond(&mut writer, &request, None);
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::ZERO);
    assert!(
        client
            .expect_ok(Op::Wait {
                timeout_ms: Some(0),
            })
            .unwrap()
            .ok
    );
    server.join().unwrap();
}

#[test]
fn zero_timeout_wait_reconnects_after_cached_session_closes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut first_writer = accept_with_timeout(&listener);
        let mut first_reader = BufReader::new(first_writer.try_clone().unwrap());
        let request = read_request(&mut first_reader);
        assert_eq!(request["op"], "hello");
        respond(&mut first_writer, &request, None);
        first_writer.shutdown(Shutdown::Both).unwrap();
        drop(first_reader);
        drop(first_writer);
        closed_tx.send(()).unwrap();

        let mut second_writer = accept_with_timeout(&listener);
        let mut second_reader = BufReader::new(second_writer.try_clone().unwrap());
        let request = read_request(&mut second_reader);
        assert_eq!(request["op"], "hello");
        respond(
            &mut second_writer,
            &request,
            Some(hello(&["secret_redaction"])),
        );

        let request = read_request(&mut second_reader);
        assert_eq!(request["op"], "wait");
        assert_eq!(request["timeout_ms"], 0);
        respond(&mut second_writer, &request, None);
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::ZERO);
    assert!(client.expect_ok(Op::Hello).unwrap().ok);
    closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        client
            .expect_ok(Op::Wait {
                timeout_ms: Some(0),
            })
            .unwrap()
            .ok
    );
    server.join().unwrap();
}

#[test]
fn long_wait_does_not_extend_connection_retry_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let wait = || Op::WaitUntil {
        timeout_ms: 20_000,
        spec: AssertSpec {
            target: "button".into(),
            exists: Some(true),
            ..AssertSpec::default()
        },
    };
    let timeout = Duration::from_millis(200);
    let snapshot = Op::snapshot();
    let mut pipeline_client = AgentClient::connect(addr).with_timeout(timeout);
    let started = std::time::Instant::now();
    let error = pipeline_client
        .rpc_pipeline(&[&wait(), &snapshot])
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "pipeline connection retries took {:?}: {error}",
        started.elapsed()
    );

    let mut client = AgentClient::connect(addr).with_timeout(timeout);
    let started = std::time::Instant::now();
    let error = client.rpc(wait()).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "single-op connection retries took {:?}: {error}",
        started.elapsed()
    );
}

#[test]
fn gated_no_reply_is_reported_once_within_the_shared_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut writer, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let nonce = [7u8; 32];
        thread::sleep(Duration::from_millis(220));
        send_challenge(&mut writer, &nonce);
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "hello");
        let expected_auth = hmac_hex("secret", &nonce).unwrap();
        assert_eq!(request["auth"], expected_auth);
        respond(&mut writer, &request, Some(hello(&["press"])));

        let request = read_request(&mut reader);
        assert_eq!(request["op"], "press");
        assert_eq!(request["auth"], expected_auth);
        thread::sleep(Duration::from_millis(700));
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ),
            "the client opened another session after sending press"
        );
    });

    let mut client = AgentClient::connect(addr)
        .with_token("secret")
        .with_timeout(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let error = client.gated(press()).unwrap_err();
    let elapsed = started.elapsed();
    assert!(error.contains("no reply"), "{error}");
    assert!(
        elapsed < Duration::from_millis(450),
        "gated call exceeded its shared timeout: {elapsed:?}"
    );
    server.join().unwrap();
}

#[test]
fn gated_call_does_not_retry_after_the_request_was_written() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut writer, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "hello");
        respond(&mut writer, &request, Some(hello(&["press"])));
        let request = read_request(&mut reader);
        assert_eq!(request["op"], "press");
        drop(reader);
        drop(writer);

        thread::sleep(Duration::from_millis(200));
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ),
            "the client retried after the host had received press"
        );
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(1));
    let error = client.gated(press()).unwrap_err();
    assert!(error.contains("connection closed"), "{error}");
    server.join().unwrap();
}

#[test]
fn gated_call_reconnects_when_the_peer_closed_between_calls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut old_writer, _) = listener.accept().unwrap();
        let mut old_reader = BufReader::new(old_writer.try_clone().unwrap());
        let request = read_request(&mut old_reader);
        assert_eq!(request["op"], "hello");
        respond(&mut old_writer, &request, Some(hello(&["press"])));
        let request = read_request(&mut old_reader);
        assert_eq!(request["op"], "press");
        respond(&mut old_writer, &request, None);
        old_writer.shutdown(Shutdown::Both).unwrap();
        drop(old_reader);
        drop(old_writer);
        closed_tx.send(()).unwrap();

        let (mut new_writer, _) = listener.accept().unwrap();
        let mut new_reader = BufReader::new(new_writer.try_clone().unwrap());
        let request = read_request(&mut new_reader);
        assert_eq!(request["op"], "hello");
        respond(&mut new_writer, &request, Some(hello(&["press"])));
        let request = read_request(&mut new_reader);
        assert_eq!(request["op"], "press");
        respond(&mut new_writer, &request, None);

        new_reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut line = String::new();
        let error = new_reader.read_line(&mut line).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ),
            "the second call was sent more than once"
        );
    });

    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    assert!(client.gated(press()).unwrap().ok);
    closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(client.gated(press()).unwrap().ok);
    server.join().unwrap();
}
