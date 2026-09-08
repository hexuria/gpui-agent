use std::collections::{BTreeMap, HashMap};
use std::hint::black_box;
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use gpui_agent::client::AgentClient;
use gpui_agent::dispatch::handle_request;
use gpui_agent::host::AgentHost;
use gpui_agent::mailbox::{AgentMailbox, MAX_MAILBOX_DEPTH};
use gpui_agent::ndjson::{read_limited_line_into, write_json_line};
use gpui_agent::protocol::{HelloInfo, Op, PROTOCOL_VERSION, PlatformKind, Request, Response};
use gpui_agent::server::{AgentServer, MAX_LINE_BYTES, ServerLimits};
use gpui_agent::tree::{Bounds, UiNode, UiTree};
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

fn naive_flatten<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        let mut extra = Vec::new();
        naive_flatten(child, &mut extra);
        out.extend(extra);
    }
}

fn flatten_no_count<'a>(tree: &'a UiTree) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    tree.visit(&mut |node| out.push(node));
    out
}

fn protocol_parse(c: &mut Criterion) {
    let hello = r#"{"v":1,"id":"1","op":"hello"}"#;
    let click = r#"{"v":1,"id":"2","op":"click","target":"todo-add","delivery":"virtual"}"#;
    let snap = r#"{"v":1,"id":"3","op":"snapshot","token":"secret"}"#;
    c.bench_function("protocol_parse_hello", |b| {
        b.iter(|| serde_json::from_str::<Request>(black_box(hello)).unwrap());
    });
    c.bench_function("protocol_parse_click_virtual", |b| {
        b.iter(|| serde_json::from_str::<Request>(black_box(click)).unwrap());
    });
    c.bench_function("protocol_parse_snapshot_token", |b| {
        b.iter(|| serde_json::from_str::<Request>(black_box(snap)).unwrap());
    });

    let tree = bench_tree(100);
    let snapshot_json = serde_json::to_string(&tree).unwrap();
    c.bench_function("protocol_parse_snapshot_tree_100", |b| {
        b.iter(|| serde_json::from_str::<UiTree>(black_box(&snapshot_json)).unwrap());
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
    c.bench_function("tree_flatten_no_precount", |b| {
        b.iter(|| black_box(flatten_no_count(&tree).len()));
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

fn find_and_bounds(c: &mut Criterion) {
    let tree = bench_tree(100);
    let last = format!("row-99-del");
    c.bench_function("tree_find_dfs_last_of_100_rows", |b| {
        b.iter(|| black_box(tree.find(black_box(&last)).is_some()));
    });

    let mut index: HashMap<String, &UiNode> = HashMap::new();
    tree.visit(&mut |node| {
        index.insert(node.id.clone(), node);
    });
    c.bench_function("tree_find_hashmap_last_of_100_rows", |b| {
        b.iter(|| black_box(index.get(black_box(last.as_str())).is_some()));
    });

    let mut map = HashMap::new();
    let mut btree = BTreeMap::new();
    let mut linear = Vec::new();
    tree.visit(&mut |node| {
        let bounds = Bounds {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
        };
        map.insert(node.id.clone(), bounds);
        btree.insert(node.id.clone(), bounds);
        linear.push((node.id.clone(), bounds));
    });
    let mut hashed = tree.clone();
    c.bench_function("tree_apply_bounds_hashmap", |b| {
        b.iter(|| {
            hashed.apply_bounds_map(black_box(&map));
        });
    });
    c.bench_function("tree_apply_bounds_btreemap_rebuild", |b| {
        b.iter(|| {
            apply_bounds_btree(&mut hashed, black_box(&btree));
        });
    });
    c.bench_function("tree_apply_bounds_linear_scan", |b| {
        b.iter(|| {
            apply_bounds_linear(&mut hashed, black_box(&linear));
        });
    });
}

fn apply_bounds_btree(tree: &mut UiTree, map: &BTreeMap<String, Bounds>) {
    fn walk(node: &mut UiNode, map: &BTreeMap<String, Bounds>) {
        if let Some(bounds) = map.get(&node.id) {
            node.bounds = *bounds;
        }
        for child in &mut node.children {
            walk(child, map);
        }
    }
    for node in &mut tree.nodes {
        walk(node, map);
    }
}

fn apply_bounds_linear(tree: &mut UiTree, pairs: &[(String, Bounds)]) {
    fn walk(node: &mut UiNode, pairs: &[(String, Bounds)]) {
        if let Some((_, bounds)) = pairs.iter().find(|(id, _)| id == &node.id) {
            node.bounds = *bounds;
        }
        for child in &mut node.children {
            walk(child, pairs);
        }
    }
    for node in &mut tree.nodes {
        walk(node, pairs);
    }
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

fn pipeline_hellos_on(writer: &mut TcpStream, reader: &mut BufReader<TcpStream>, n: usize) {
    let mut encode = Vec::with_capacity(256);
    let mut out = Vec::with_capacity(4096);
    for i in 0..n {
        write_json_line(
            &mut out,
            &mut encode,
            &Request::new(i.to_string(), Op::Hello),
        )
        .unwrap();
    }
    writer.write_all(&out).unwrap();
    writer.flush().unwrap();
    let mut line = Vec::new();
    for _ in 0..n {
        line.clear();
        assert!(read_limited_line_into(reader, &mut line, MAX_LINE_BYTES).unwrap());
        let resp: Response = serde_json::from_slice(&line).unwrap();
        assert!(resp.ok);
    }
}

fn open_pipeline_session(addr: std::net::SocketAddr) -> (TcpStream, BufReader<TcpStream>) {
    let stream = TcpStream::connect(addr).unwrap();
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let writer = stream.try_clone().unwrap();
    (writer, BufReader::new(stream))
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
    c.bench_function("rpc_once_reconnect_32_hellos", |b| {
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
        b.iter(|| {
            for _ in 0..32 {
                client.rpc_once(Op::Hello).unwrap();
            }
        });
    });
    c.bench_function("rpc_pipeline_32_hellos", |b| {
        let (mut writer, mut reader) = open_pipeline_session(addr);
        b.iter(|| pipeline_hellos_on(&mut writer, &mut reader, 32));
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

fn threads_tree_walk(c: &mut Criterion) {
    let small = bench_tree(100);
    let large = bench_tree(2_000);
    c.bench_function("tree_node_count_100_seq", |b| {
        b.iter(|| black_box(small.node_count()));
    });
    c.bench_function("tree_node_count_100_scoped_threads", |b| {
        b.iter(|| {
            let total: usize = std::thread::scope(|scope| {
                let handles: Vec<_> = small
                    .nodes
                    .iter()
                    .map(|node| scope.spawn(|| node.node_count()))
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            });
            black_box(total)
        });
    });
    c.bench_function("tree_node_count_2000_seq", |b| {
        b.iter(|| black_box(large.node_count()));
    });
    c.bench_function("tree_node_count_2000_scoped_threads_on_rows", |b| {
        b.iter(|| {
            let total: usize = std::thread::scope(|scope| {
                let rows = &large.nodes[0].children;
                let handles: Vec<_> = rows
                    .iter()
                    .map(|node| scope.spawn(|| node.node_count()))
                    .collect();
                1 + handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .sum::<usize>()
            });
            black_box(total)
        });
    });
}

criterion_group!(
    benches,
    protocol_parse,
    snapshot_serialize,
    flatten_trees,
    find_and_bounds,
    mailbox_pressure,
    session_vs_reconnect,
    threads_tree_walk
);
criterion_main!(benches);
