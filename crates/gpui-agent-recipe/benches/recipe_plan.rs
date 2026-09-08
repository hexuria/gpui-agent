use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use gpui_agent_recipe::{Recipe, compile_plan, parse_wants, todo_registry};

fn compile_linear(c: &mut Criterion) {
    let mut body = String::from("wait\n");
    for i in 0..40 {
        body.push_str(&format!("invoke todo.add title=item-{i}\n"));
    }
    let recipe = parse_wants(&body, "many").unwrap();
    let registry = todo_registry();
    c.bench_function("compile_40_invoke_adds", |b| {
        b.iter(|| compile_plan(black_box(&recipe), &BTreeMap::new(), &registry).unwrap());
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
}

criterion_group!(benches, compile_linear, compile_dag);
criterion_main!(benches);
