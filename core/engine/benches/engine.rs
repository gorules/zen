use criterion::async_executor::FuturesExecutor;
use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use futures::executor::block_on;
use serde_json::{json, Value};
use std::path::Path;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};
use zen_engine::DecisionEngine;

fn create_graph() -> DecisionEngine<FilesystemLoader> {
    let cargo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let loader = FilesystemLoader::new(FilesystemLoaderOptions {
        keep_in_memory: true,
        root: cargo_root
            .join("../../")
            .join("test-data")
            .to_str()
            .unwrap(),
    });

    DecisionEngine::new(loader)
}

fn bench_decision(b: &mut Bencher, key: &str, context: Value) {
    let graph = create_graph();
    let decision = block_on(graph.get_decision(key)).unwrap();

    b.to_async(FuturesExecutor).iter(|| async {
        criterion::black_box(decision.evaluate(&context).await.unwrap());
    });
}

fn bench_loader(b: &mut Bencher, key: &str, context: Value) {
    let graph = create_graph();

    b.to_async(FuturesExecutor).iter(|| async {
        criterion::black_box(graph.evaluate(key, &context).await.unwrap());
    });
}

fn bench_functions(c: &mut Criterion) {
    c.bench_function("loader/table", |b| {
        bench_loader(b, "table.json", json!({ "input": 15 }));
    });

    c.bench_function("decision/table", |b| {
        bench_decision(b, "table.json", json!({ "input": 15 }));
    });
}

criterion_group!(benches, bench_functions);
criterion_main!(benches);
