use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tokio::runtime::Runtime;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};
use zen_engine::nodes::custom::NoopCustomNode;
use zen_engine::DecisionEngine;
use zen_expression::variable::Variable;

fn create_graph() -> DecisionEngine {
    let cargo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let loader = FilesystemLoader::new(FilesystemLoaderOptions {
        keep_in_memory: true,
        root: cargo_root
            .join("../../")
            .join("test-data")
            .to_str()
            .unwrap(),
    });

    DecisionEngine::new(Arc::new(loader), Arc::new(NoopCustomNode::default()))
}

fn bench_decision(b: &mut Bencher, key: &str, context: Variable) {
    let rt = Runtime::new().unwrap();
    let graph = create_graph();

    let decision = rt.block_on(graph.get_decision(key)).unwrap();
    b.to_async(&rt).iter(|| async {
        criterion::black_box(decision.evaluate(context.clone()).await.unwrap());
    });
}
fn bench_decision_8k(b: &mut Bencher, key: &str, context: Variable) {
    let rt = Runtime::new().unwrap();
    let graph = create_graph();

    let decision = rt.block_on(graph.get_decision(key)).unwrap();
    b.to_async(&rt).iter(|| async {
        criterion::black_box(decision.evaluate(context.clone()).await.unwrap());
    });
}

fn bench_decision_8k_precompiled(b: &mut Bencher, key: &str, context: Variable) {
    let rt = Runtime::new().unwrap();
    let graph = create_graph();

    let mut decision = rt.block_on(graph.get_decision(key)).unwrap();
    decision.compile();
    b.to_async(&rt).iter(|| async {
        criterion::black_box(decision.evaluate(context.clone()).await.unwrap());
    });
}

fn bench_loader(b: &mut Bencher, key: &str, context: Variable) {
    let rt = Runtime::new().unwrap();
    let graph = create_graph();

    b.to_async(&rt).iter(|| async {
        criterion::black_box(graph.evaluate(key, context.clone()).await.unwrap());
    });
}

fn bench_functions(c: &mut Criterion) {
    c.bench_function("loader/table", |b| {
        bench_loader(b, "table.json", json!({ "input": 15 }).into());
    });

    c.bench_function("decision/table", |b| {
        bench_decision(b, "table.json", json!({ "input": 15 }).into());
    });
}

fn precompile_functions(c: &mut Criterion) {
    let mut group = c.benchmark_group("decision/8k");
    group.sample_size(50);

    group.bench_function("uncompiled", |b| {
        bench_decision_8k(b, "8k.json", json!({ "input": 15 }).into());
    });

    group.bench_function("precompiled", |b| {
        bench_decision_8k_precompiled(b, "8k.json", json!({ "input": 15 }).into());
    });

    group.finish();
}

criterion_group!(benches, bench_functions);
criterion_group!(precompiled, precompile_functions);
criterion_main!(benches, precompiled);
