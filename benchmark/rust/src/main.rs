use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};
use zen_engine::DecisionEngine;
use zen_expression::Variable;

#[derive(Deserialize)]
struct Entry {
    name: String,
    kind: String,
    file: String,
    input: Value,
}

#[derive(Serialize)]
struct BenchResult {
    name: String,
    unit: String,
    value: f64,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let manifest_path = args
        .next()
        .unwrap_or_else(|| "../manifest.json".to_string());
    let out_path = args.next();
    let iters: u32 = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    let manifest_path = Path::new(&manifest_path);
    let base = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("fixtures");
    let manifest: Vec<Entry> =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).expect("read manifest"))
            .expect("parse manifest");

    let engine = DecisionEngine::default().with_loader(Arc::new(FilesystemLoader::new(
        FilesystemLoaderOptions {
            root: base.to_string_lossy().to_string(),
        },
    )));

    let mut results = Vec::new();
    for e in &manifest {
        let input: Variable = e.input.clone().into();
        engine
            .evaluate(e.file.as_str(), input.clone())
            .await
            .unwrap_or_else(|err| panic!("warmup {} failed: {err:?}", e.name));

        let start = Instant::now();
        for _ in 0..iters {
            engine
                .evaluate(e.file.as_str(), input.clone())
                .await
                .unwrap();
        }
        let per = start.elapsed().as_nanos() as f64 / f64::from(iters);

        results.push(BenchResult {
            name: format!("{} ({})", e.name, e.kind),
            unit: "ns/op".to_string(),
            value: per,
        });
    }

    let json = serde_json::to_string_pretty(&results).expect("serialize results");
    match out_path {
        Some(p) => std::fs::write(p, json).expect("write output"),
        None => println!("{json}"),
    }
}
