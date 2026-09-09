use std::hint::black_box;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use gpui_agent::client::AgentClient;
use gpui_agent::dispatch::handle_request;
use gpui_agent::host::AgentHost;
use gpui_agent::mailbox::{AgentMailbox, MAX_MAILBOX_DEPTH};
use gpui_agent::ndjson::write_json_line;
use gpui_agent::protocol::{HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request};
use gpui_agent::server::{AgentServer, ServerLimits};
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent::{DeliveryMode, DispatchResult};

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
        bench_tree(32)
    }

    fn dispatch(&mut self, _op: &Op) -> Result<DispatchResult, String> {
        Ok(DispatchResult::empty())
    }
}

fn bench_tree(items: usize) -> UiTree {
    let children: Vec<UiNode> = (0..items)
        .map(|i| {
            UiNode::new(format!("row-{i}"), "listitem", format!("Item {i}"))
                .with_checked(i % 2 == 0)
                .with_children(vec![
                    UiNode::new(format!("row-{i}-toggle"), "checkbox", format!("Item {i}"))
                        .with_checked(i % 2 == 0),
                    UiNode::new(format!("row-{i}-del"), "button", "Delete"),
                ])
        })
        .collect();
    UiTree {
        app: "bench".into(),
        platform: PlatformKind::Headless,
        ready: true,
        nodes: vec![UiNode::new("root", "window", "Bench").with_children(children)],
    }
}

/// Historical flatten: allocate an intermediate vec per child.
fn naive_flatten<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        let mut extra = Vec::new();
        naive_flatten(child, &mut extra);
        out.extend(extra);
    }
}

fn flatten_trees(c: &mut Criterion) {
    let tree = bench_tree(100);
    c.bench_function("tree_flatten_capacity", |b| {
        b.iter(|| black_box(tree.flatten().len()));
    });
    c.bench_function("tree_flatten_into_reuse", |b| {
        let mut out = Vec::new();
        b.iter(|| {
            tree.flatten_into(&mut out);
            black_box(out.len())
        });
    });
    c.bench_function("tree_flatten_naive_intermediate_vecs", |b| {
        b.iter(|| {
            let mut out = Vec::new();
            for node in &tree.nodes {
                naive_flatten(node, &mut out);
            }
            black_box(out.len())
        });
    });
}

fn snapshot_serialize(c: &mut Criterion) {
    let tree = bench_tree(100);
    c.bench_function("snapshot_to_string", |b| {
        b.iter(|| serde_json::to_string(black_box(&tree)).unwrap());
    });
    c.bench_function("snapshot_write_json_line_reuse", |b| {
        let mut buf = Vec::with_capacity(32 * 1024);
        let mut out = Vec::with_capacity(32 * 1024);
        b.iter(|| {
            out.clear();
            write_json_line(&mut out, &mut buf, black_box(&tree)).unwrap();
        });
    });
    c.bench_function("snapshot_to_string_then_writeln", |b| {
        let mut out = Vec::with_capacity(32 * 1024);
        b.iter(|| {
            out.clear();
            let line = serde_json::to_string(black_box(&tree)).unwrap();
            writeln!(&mut out, "{line}").unwrap();
        });
    });
}

fn mailbox_pressure(c: &mut Criterion) {
    c.bench_function("mailbox_push_take_full", |b| {
        let mailbox = AgentMailbox::new();
        b.iter(|| {
            for i in 0..MAX_MAILBOX_DEPTH {
                let _rx = mailbox.push(Request::new(i.to_string(), Op::Hello));
            }
            let taken = mailbox.take();
            black_box(taken.len());
        });
    });
}

fn spawn_empty_server() -> std::net::SocketAddr {
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
    addr
}

fn session_vs_reconnect(c: &mut Criterion) {
    let addr = spawn_empty_server();
    let mut warmup = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    warmup.expect_ok(Op::Hello).unwrap();
    warmup.close_session();

    c.bench_function("rpc_session_reuse_32_hellos", |b| {
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        b.iter(|| {
            for _ in 0..32 {
                client.expect_ok(Op::Hello).unwrap();
            }
            black_box(client.has_session());
        });
    });
    c.bench_function("rpc_pipeline_32_hellos", |b| {
        let hello = Op::Hello;
        let ops: Vec<&Op> = (0..32).map(|_| &hello).collect();
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        b.iter(|| {
            let resps = client.rpc_pipeline(&ops).unwrap();
            black_box(resps.len());
            black_box(client.has_session());
        });
    });
    c.bench_function("rpc_once_reconnect_32_hellos", |b| {
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        b.iter(|| {
            for _ in 0..32 {
                client.rpc_once(Op::Hello).unwrap();
            }
        });
    });
    c.bench_function("handle_request_inprocess_32_hellos", |b| {
        let mut host = EmptyHost;
        b.iter(|| {
            for i in 0..32 {
                let resp = handle_request(&mut host, Request::new(i.to_string(), Op::Hello), None);
                black_box(resp.ok);
            }
        });
    });
}

criterion_group!(
    benches,
    flatten_trees,
    snapshot_serialize,
    mailbox_pressure,
    session_vs_reconnect
);
criterion_main!(benches);
