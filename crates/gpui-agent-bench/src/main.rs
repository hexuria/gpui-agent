//! Benchmark harness for the milestone arms (PRD §5).
//!
//! `resolve` runs the intent corpus through `resolve_intent` (R0 now,
//! R1 after M2). `local` spawns a fresh `todo-headless` per attempt,
//! seeds it, runs a goal command, and judges final app state (A2/A3).
//! `report` aggregates JSONL into the RESULTS.md table section.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::Op;
use gpui_agent_recipe::resolve_intent;
use serde::Deserialize;
use serde_json::{Value, json};

const CAP_WALL_MS: u64 = 120_000;

#[derive(Parser)]
#[command(
    name = "gpui-agent-bench",
    about = "gpui-agent milestone benchmark arms"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// R0/R1: run the labeled intent corpus through resolve_intent.
    Resolve {
        /// bench/intents.toml
        #[arg(long)]
        corpus: PathBuf,
        /// Arm label for the JSONL rows (r0 = keyword resolver).
        #[arg(long, default_value = "r0")]
        arm: String,
        /// Attempts per corpus row (timing n).
        #[arg(long, default_value_t = 20)]
        attempts: u32,
        /// Output JSONL path.
        #[arg(long)]
        out: PathBuf,
    },
    /// A2/A3: spawn todo-headless, seed, run a goal command, judge state.
    Local {
        /// bench/tasks.toml
        #[arg(long)]
        tasks: PathBuf,
        /// Arm label (a2 = local policy, a3 = policy+escalation).
        #[arg(long, default_value = "a2")]
        arm: String,
        /// Shell command run once per attempt with GPUI_AGENT_ADDR /
        /// GPUI_AGENT_TOKEN exported (e.g. `gpui-agent run --goal ...`).
        #[arg(long)]
        goal_cmd: String,
        /// Attempts per task.
        #[arg(long, default_value_t = 1)]
        attempts: u32,
        /// Only run these task ids (comma separated).
        #[arg(long)]
        only: Option<String>,
        /// Path to the todo-headless binary (autodetected under target/ otherwise).
        #[arg(long)]
        todo_headless: Option<PathBuf>,
        /// Output JSONL path.
        #[arg(long)]
        out: PathBuf,
    },
    /// Aggregate JSONL results into the RESULTS.md report section.
    Report {
        /// Directory scanned recursively for *.jsonl.
        #[arg(long)]
        results: PathBuf,
        /// Markdown file updated between bench:report markers.
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Deserialize)]
struct Corpus {
    row: Vec<CorpusRow>,
}

#[derive(Deserialize)]
struct CorpusRow {
    intent: String,
    expect: String,
}

#[derive(Deserialize)]
struct Tasks {
    task: Vec<TaskSpec>,
}

#[derive(Deserialize)]
struct TaskSpec {
    id: String,
    goal: String,
    kind: String,
    #[serde(default)]
    seed: Vec<String>,
    /// Bulk seed: `prefix` + start..=end (T12/T13: "Item " 1..=200).
    seed_range: Option<SeedRange>,
    #[serde(default)]
    expect: Expect,
}

#[derive(Deserialize)]
struct SeedRange {
    prefix: String,
    start: u64,
    end: u64,
}

#[derive(Default, Deserialize)]
struct Expect {
    #[serde(default)]
    added: Vec<String>,
    #[serde(default)]
    absent: Vec<String>,
    #[serde(default)]
    present: Vec<String>,
    #[serde(default)]
    done: Vec<String>,
    #[serde(default)]
    not_done: Vec<String>,
    total: Option<usize>,
    #[serde(default)]
    visible: Vec<String>,
    #[serde(default)]
    not_visible: Vec<String>,
    #[serde(default)]
    checked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Item {
    title: String,
    done: bool,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Cmd::Resolve {
            corpus,
            arm,
            attempts,
            out,
        } => cmd_resolve(&corpus, &arm, attempts, &out),
        Cmd::Local {
            tasks,
            arm,
            goal_cmd,
            attempts,
            only,
            todo_headless,
            out,
        } => cmd_local(&tasks, &arm, &goal_cmd, attempts, only, todo_headless, &out),
        Cmd::Report { results, out } => cmd_report(&results, &out),
    };
    std::process::exit(code);
}

// ---- shared row emit -----------------------------------------------------

fn base_row(arm: &str, task: &str, attempt: u32) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("task".into(), json!(task));
    m.insert("arm".into(), json!(arm));
    m.insert("attempt".into(), json!(attempt));
    m.insert("pass".into(), json!(false));
    m.insert("wrong_action".into(), json!(false));
    m.insert("refused".into(), json!(false));
    m.insert("cap_hit".into(), json!(false));
    m.insert("wall_ms".into(), Value::Null);
    m.insert("protocol_ops".into(), Value::Null);
    m.insert("bytes_to_model".into(), Value::Null);
    m.insert("model_calls".into(), Value::Null);
    m.insert("input_tokens".into(), Value::Null);
    m.insert("output_tokens".into(), Value::Null);
    m.insert("cache_read_tokens".into(), Value::Null);
    m.insert("cache_write_tokens".into(), Value::Null);
    m.insert("usd".into(), Value::Null);
    m.insert("git_sha".into(), json!(git_sha()));
    m.insert("host_os".into(), json!(std::env::consts::OS));
    m
}

fn emit(out: &mut impl Write, row: &serde_json::Map<String, Value>) {
    let line = serde_json::to_string(row).expect("row serializes");
    out.write_all(line.as_bytes()).expect("write");
    out.write_all(b"\n").expect("write");
}

fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

// ---- resolve (R0/R1) ------------------------------------------------------

fn cmd_resolve(corpus: &Path, arm: &str, attempts: u32, out: &Path) -> i32 {
    if arm != "r0" {
        eprintln!(
            "arm {arm:?} unavailable: the instinct-backed resolver lands in M2; run --arm r0"
        );
        return 2;
    }
    let text = match fs::read_to_string(corpus) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {}: {e}", corpus.display());
            return 2;
        }
    };
    let corpus: Corpus = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("parse {}: {e}", corpus.display());
            return 2;
        }
    };
    let registry = gpui_agent_recipe::registry::todo_registry();
    let mut file = match fs::File::create(out) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("create {}: {e}", out.display());
            return 2;
        }
    };
    for row in &corpus.row {
        for attempt in 1..=attempts {
            let started = Instant::now();
            let resolved = resolve_intent(&row.intent, &registry);
            let wall_us = started.elapsed().as_micros() as f64;
            let mut rec = base_row(arm, &row.intent, attempt);
            rec.insert("wall_ms".into(), json!(wall_us / 1000.0));
            rec.insert("wall_us".into(), json!(wall_us));
            rec.insert("expect".into(), json!(row.expect));
            match resolved {
                Ok(r) => {
                    rec.insert("got".into(), json!(r.schema));
                    let abstain_row = row.expect == "abstain";
                    rec.insert("pass".into(), json!(!abstain_row && r.schema == row.expect));
                    rec.insert(
                        "wrong_action".into(),
                        json!(abstain_row || r.schema != row.expect),
                    );
                }
                Err(e) => {
                    rec.insert("got".into(), Value::Null);
                    rec.insert("refused".into(), json!(true));
                    rec.insert("pass".into(), json!(row.expect == "abstain"));
                    rec.insert("detail".into(), json!(e));
                }
            }
            emit(&mut file, &rec);
        }
    }
    println!("wrote {}", out.display());
    0
}

// ---- local (A2/A3) --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn cmd_local(
    tasks: &Path,
    arm: &str,
    goal_cmd: &str,
    attempts: u32,
    only: Option<String>,
    todo_headless: Option<PathBuf>,
    out: &Path,
) -> i32 {
    let text = match fs::read_to_string(tasks) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {}: {e}", tasks.display());
            return 2;
        }
    };
    let tasks: Tasks = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("parse {}: {e}", tasks.display());
            return 2;
        }
    };
    let host_bin = match todo_headless.or_else(find_todo_headless) {
        Some(p) => p,
        None => {
            eprintln!(
                "todo-headless binary not found; build it first (cargo build -p todo-headless) or pass --todo-headless"
            );
            return 2;
        }
    };
    let only: Option<Vec<String>> =
        only.map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let mut file = match fs::File::create(out) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("create {}: {e}", out.display());
            return 2;
        }
    };
    for task in &tasks.task {
        if let Some(only) = &only {
            if !only.iter().any(|o| o == &task.id) {
                continue;
            }
        }
        for attempt in 1..=attempts {
            let rec = run_local_attempt(task, arm, attempt, goal_cmd, &host_bin);
            emit(&mut file, &rec);
            let status = if rec["pass"].as_bool() == Some(true) {
                "pass"
            } else {
                "FAIL"
            };
            eprintln!("{} {} attempt {attempt}: {status}", arm, task.id);
        }
    }
    println!("wrote {}", out.display());
    0
}

fn find_todo_headless() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "todo-headless.exe"
    } else {
        "todo-headless"
    };
    for dir in ["target/debug", "target/release"] {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(17_421)
}

fn bench_token(port: u16, attempt: u32) -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if f.read_exact(&mut buf).is_ok() {
                return buf.iter().map(|b| format!("{b:02x}")).collect();
            }
        }
    }
    // Fallback: hashed time + pid + port (local-only bench secret).
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    (
        std::time::SystemTime::now(),
        std::process::id(),
        port,
        attempt,
    )
        .hash(&mut h);
    format!("{:016x}", h.finish())
}

fn seed_titles(task: &TaskSpec) -> Vec<String> {
    let mut titles = task.seed.clone();
    if let Some(r) = &task.seed_range {
        for n in r.start..=r.end {
            titles.push(format!("{}{n}", r.prefix));
        }
    }
    titles
}

fn run_local_attempt(
    task: &TaskSpec,
    arm: &str,
    attempt: u32,
    goal_cmd: &str,
    host_bin: &Path,
) -> serde_json::Map<String, Value> {
    let mut rec = base_row(arm, &task.id, attempt);
    rec.insert("goal".into(), json!(task.goal));
    let port = free_port();
    let token = bench_token(port, attempt);
    let addr = format!("127.0.0.1:{port}");

    let mut child = match Command::new(host_bin)
        .arg("serve")
        .env("GPUI_AGENT", "1")
        .env("GPUI_AGENT_ADDR", &addr)
        .env("GPUI_AGENT_TOKEN", &token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            rec.insert(
                "detail".into(),
                json!(format!("spawn {}: {e}", host_bin.display())),
            );
            return rec;
        }
    };

    let outcome = (|| {
        let mut client = AgentClient::connect(addr.parse().expect("addr"))
            .with_token(token.clone())
            .with_timeout(Duration::from_secs(3));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match client.expect_ok(Op::Hello) {
                Ok(_) => break,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(e) => return format!("host did not answer hello: {e}"),
            }
            if child.try_wait().ok().flatten().is_some() {
                return "todo-headless exited during startup".into();
            }
        }

        let mut harness_ops = 0u64;
        let seeded = seed_titles(task);
        for title in &seeded {
            harness_ops += 1;
            if let Err(e) = client.expect_ok(Op::Invoke {
                name: "todo.add".into(),
                args: json!({ "title": title }),
            }) {
                return format!("seed add {title:?}: {e}");
            }
        }

        let started = Instant::now();
        let (goal_status, cap_hit) = run_goal(goal_cmd, &addr, &token, CAP_WALL_MS);
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        rec.insert("wall_ms".into(), json!(wall_ms));
        rec.insert("cap_hit".into(), json!(cap_hit));
        rec.insert("goal_exit".into(), json!(goal_status));

        harness_ops += 1;
        let items = match client
            .expect_ok(Op::Invoke {
                name: "todo.list".into(),
                args: json!({}),
            })
            .and_then(|resp| parse_items(resp.result))
        {
            Ok(items) => items,
            Err(e) => return format!("todo.list: {e}"),
        };
        let expect_state = judge(task, &seeded, &items, &goal_status, cap_hit);
        rec.insert("pass".into(), json!(expect_state.pass));
        rec.insert("wrong_action".into(), json!(expect_state.wrong));
        rec.insert("refused".into(), json!(expect_state.refused));
        if let Some(d) = expect_state.detail {
            rec.insert("detail".into(), json!(d));
        }
        if needs_tree_checks(&task.expect) {
            harness_ops += 1;
            match client.expect_ok(Op::Snapshot) {
                Ok(resp) => {
                    let tree_ok = judge_tree(&task.expect, resp.result);
                    if !tree_ok.0 {
                        rec.insert("pass".into(), json!(false));
                        rec.insert("detail".into(), json!(tree_ok.1));
                    }
                }
                Err(e) => {
                    rec.insert("pass".into(), json!(false));
                    rec.insert("detail".into(), json!(format!("snapshot: {e}")));
                }
            }
        }
        if !expect_state.pass && task.kind == "refuse" && !expect_state.wrong {
            rec.insert("refused".into(), json!(true));
        }
        rec.insert("harness_ops".into(), json!(harness_ops));
        String::new()
    })();

    if !outcome.is_empty() {
        rec.insert("detail".into(), json!(outcome));
    }
    let _ = child.kill();
    let _ = child.wait();
    rec
}

fn run_goal(goal_cmd: &str, addr: &str, token: &str, cap_ms: u64) -> (i32, bool) {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let mut child = match Command::new(shell)
        .arg(flag)
        .arg(goal_cmd)
        .env("GPUI_AGENT_ADDR", addr)
        .env("GPUI_AGENT_TOKEN", token)
        .env("GPUI_AGENT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (-1, false),
    };
    let deadline = Instant::now() + Duration::from_millis(cap_ms);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return (s.code().unwrap_or(-1), false),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return (-1, true);
            }
            Err(_) => return (-1, false),
        }
    }
}

fn parse_items(result: Option<Value>) -> Result<Vec<Item>, String> {
    let v = result.ok_or("todo.list returned no result")?;
    let arr = v.as_array().ok_or("todo.list result is not an array")?;
    let mut items = Vec::with_capacity(arr.len());
    for it in arr {
        let title = it
            .get("title")
            .and_then(Value::as_str)
            .ok_or("item without title")?;
        let done = it.get("done").and_then(Value::as_bool).unwrap_or(false);
        items.push(Item {
            title: title.to_string(),
            done,
        });
    }
    Ok(items)
}

struct Judge {
    pass: bool,
    wrong: bool,
    refused: bool,
    detail: Option<String>,
}

fn judge(
    task: &TaskSpec,
    seeded: &[String],
    items: &[Item],
    _goal_status: &i32,
    cap_hit: bool,
) -> Judge {
    let e = &task.expect;
    let count = |t: &str| items.iter().filter(|i| i.title == t).count();
    let seed_count = |t: &str| seeded.iter().filter(|s| s.as_str() == t).count();
    let done_count = |t: &str| items.iter().filter(|i| i.title == t && i.done).count();

    if task.kind == "refuse" {
        let mut seed_items: Vec<Item> = seeded
            .iter()
            .map(|t| Item {
                title: t.clone(),
                done: false,
            })
            .collect();
        let mut got: Vec<Item> = items.to_vec();
        seed_items.sort();
        got.sort();
        let changed = seed_items != got;
        return Judge {
            pass: !changed,
            wrong: changed,
            refused: !changed,
            detail: changed.then(|| format!("state changed on refuse task: {got:?}")),
        };
    }

    let mut misses = Vec::new();
    for t in &e.added {
        if count(t) <= seed_count(t) {
            misses.push(format!("missing added {t:?}"));
        }
    }
    for t in &e.absent {
        if count(t) != 0 {
            misses.push(format!("still present {t:?}"));
        }
    }
    for t in &e.present {
        if count(t) == 0 {
            misses.push(format!("missing {t:?}"));
        }
    }
    for t in &e.done {
        if done_count(t) != count(t) || count(t) == 0 {
            misses.push(format!("not done {t:?}"));
        }
    }
    for t in &e.not_done {
        if count(t) == 0 || done_count(t) != 0 {
            misses.push(format!("should exist undone {t:?}"));
        }
    }
    if let Some(total) = e.total {
        if items.len() != total {
            misses.push(format!("item count {} != {total}", items.len()));
        }
    }

    // A state change outside expect = wrong action: any item whose
    // title/done differs from seed beyond what the expect clauses asked for.
    let wrong = unexpected_change(seeded, items, e);
    Judge {
        pass: misses.is_empty() && !cap_hit,
        wrong,
        refused: false,
        detail: if misses.is_empty() {
            None
        } else {
            Some(misses.join("; "))
        },
    }
}

fn unexpected_change(seeded: &[String], items: &[Item], e: &Expect) -> bool {
    // Multiset of (title, done) expected after a correct run.
    let mut expect_counts: BTreeMap<(String, bool), i64> = BTreeMap::new();
    for t in seeded {
        *expect_counts.entry((t.clone(), false)).or_default() += 1;
    }
    for t in &e.added {
        *expect_counts.entry((t.clone(), false)).or_default() += 1;
    }
    for t in &e.absent {
        expect_counts.retain(|(title, _), _| title != t);
    }
    for t in &e.done {
        if let Some((k, v)) = expect_counts
            .iter()
            .find(|((title, done), _)| title == t && !*done)
            .map(|(k, v)| (k.clone(), *v))
        {
            expect_counts.insert(k.clone(), v - 1);
            *expect_counts.entry((k.0, true)).or_default() += 1;
        }
    }
    let mut got_counts: BTreeMap<(String, bool), i64> = BTreeMap::new();
    for i in items {
        *got_counts.entry((i.title.clone(), i.done)).or_default() += 1;
    }
    // Items outside the expected multiset = wrong action.
    let mut surplus: i64 = 0;
    for (k, v) in &got_counts {
        let want = expect_counts.get(k).copied().unwrap_or(0);
        surplus += (v - want).max(0);
    }
    surplus > 0
}

impl Ord for Item {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.title
            .cmp(&other.title)
            .then(self.done.cmp(&other.done))
    }
}
impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Eq for Item {}

fn needs_tree_checks(e: &Expect) -> bool {
    !e.visible.is_empty() || !e.not_visible.is_empty() || !e.checked.is_empty()
}

fn judge_tree(e: &Expect, result: Option<Value>) -> (bool, String) {
    let Some(v) = result else {
        return (false, "snapshot returned no tree".into());
    };
    // Response.result for snapshot carries the UiTree: { nodes: [...] }.
    let nodes = v
        .get("nodes")
        .or_else(|| v.get("tree").and_then(|t| t.get("nodes")))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let find = |id: &str| {
        nodes
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(id))
    };
    for id in &e.visible {
        match find(id) {
            Some(n) if n.get("visible").and_then(Value::as_bool) == Some(true) => {}
            _ => return (false, format!("{id} not visible")),
        }
    }
    for id in &e.not_visible {
        if let Some(n) = find(id) {
            if n.get("visible").and_then(Value::as_bool) == Some(true) {
                return (false, format!("{id} still visible"));
            }
        }
    }
    for id in &e.checked {
        match find(id) {
            Some(n) if n.get("checked").and_then(Value::as_bool) == Some(true) => {}
            _ => return (false, format!("{id} not checked")),
        }
    }
    (true, String::new())
}

// ---- report ---------------------------------------------------------------

fn cmd_report(results: &Path, out: &Path) -> i32 {
    let mut rows = Vec::new();
    collect_jsonl(results, &mut rows);
    if rows.is_empty() {
        eprintln!("no jsonl rows under {}", results.display());
        return 2;
    }
    // group: arm -> task -> attempts
    let mut groups: BTreeMap<(String, String), Vec<&serde_json::Map<String, Value>>> =
        BTreeMap::new();
    for r in &rows {
        let arm = r["arm"].as_str().unwrap_or("?").to_string();
        let task = r["task"].as_str().unwrap_or("?").to_string();
        groups.entry((arm, task)).or_default().push(r);
    }
    let mut md = String::from("<!-- bench:report:start -->\n");
    md.push_str("_Generated by `gpui-agent-bench report`; do not edit by hand._\n\n");
    md.push_str("| arm | task | pass | wrong | refused | cap | wall_ms p50 | wall_ms p90 | in_tok p50 | usd/passed |\n");
    md.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    let mut arms: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for ((arm, task), rs) in &groups {
        let n = rs.len();
        let k = rs
            .iter()
            .filter(|r| r["pass"].as_bool() == Some(true))
            .count();
        let wrong = rs
            .iter()
            .filter(|r| r["wrong_action"].as_bool() == Some(true))
            .count();
        let refused = rs
            .iter()
            .filter(|r| r["refused"].as_bool() == Some(true))
            .count();
        let cap = rs
            .iter()
            .filter(|r| r["cap_hit"].as_bool() == Some(true))
            .count();
        let walls: Vec<f64> = rs.iter().filter_map(|r| r["wall_ms"].as_f64()).collect();
        let toks: Vec<f64> = rs
            .iter()
            .filter_map(|r| r["input_tokens"].as_f64())
            .collect();
        let usds: Vec<f64> = rs
            .iter()
            .filter(|r| r["pass"].as_bool() == Some(true))
            .filter_map(|r| r["usd"].as_f64())
            .collect();
        let usd_per = if usds.is_empty() {
            "—".into()
        } else {
            format!("${:.4}", usds.iter().sum::<f64>() / usds.len() as f64)
        };
        md.push_str(&format!(
            "| {arm} | {} | {k}/{n} | {wrong} | {refused} | {cap} | {} | {} | {} | {usd_per} |\n",
            short(task),
            fmt_stat(percentile(&walls, 0.5)),
            fmt_stat(percentile(&walls, 0.9)),
            fmt_stat(percentile(&toks, 0.5)),
        ));
        let e = arms.entry(arm.clone()).or_default();
        e.0 += k as u64;
        e.1 += n as u64;
        e.2 += wrong as u64;
    }
    md.push_str("\n### Arm summary\n\n| arm | pass | wrong_actions |\n|---|---|---|\n");
    for (arm, (k, n, w)) in &arms {
        md.push_str(&format!("| {arm} | {k}/{n} | {w} |\n"));
    }
    md.push_str("<!-- bench:report:end -->\n");

    let final_text = if out.exists() {
        let existing = fs::read_to_string(out).expect("read out");
        match (
            existing.find("<!-- bench:report:start -->"),
            existing.find("<!-- bench:report:end -->"),
        ) {
            (Some(s), Some(e)) => format!(
                "{}{}{}",
                &existing[..s],
                md,
                &existing[e + "<!-- bench:report:end -->\n".len()..]
            ),
            _ => format!("{existing}\n{md}"),
        }
    } else {
        format!("# Benchmark results\n\n{md}")
    };
    fs::write(out, final_text).expect("write out");
    println!("wrote {}", out.display());
    0
}

fn collect_jsonl(dir: &Path, rows: &mut Vec<serde_json::Map<String, Value>>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = fs::read_dir(&d) else { continue };
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let Ok(f) = fs::File::open(&p) else { continue };
                for line in BufReader::new(f).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&line) {
                        rows.push(m);
                    }
                }
            }
        }
    }
}

fn percentile(sorted_source: &[f64], p: f64) -> Option<f64> {
    if sorted_source.is_empty() {
        return None;
    }
    let mut v = sorted_source.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len() as f64) * p).ceil() as usize - 1;
    Some(v[idx.clamp(0, v.len() - 1)])
}

fn fmt_stat(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "—".into())
}

fn short(task: &str) -> String {
    task.chars().take(40).collect()
}
