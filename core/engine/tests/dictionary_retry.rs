use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use zen_engine::loader::{
    CachedLoader, DecisionLoader, DynamicLoader, LoaderError, LoaderResponse, MemoryLoader,
};
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, DecisionGraphResponse, EvaluationError};

#[derive(Debug)]
struct Flaky {
    inner: MemoryLoader,
    fails: AtomicUsize,
}

impl DecisionLoader for Flaky {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            if key == "dicts" && self.fails.load(Ordering::SeqCst) > 0 {
                self.fails.fetch_sub(1, Ordering::SeqCst);
                return Err(LoaderError::NotFound(key.to_string()));
            }
            self.inner.load(key).await
        })
    }
}

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn flaky(fails: usize) -> Arc<Flaky> {
    let inner = MemoryLoader::default();
    inner.add(
        "dicts",
        document(json!({
            "imports": [],
            "blocks": [{ "id": "dict1", "type": "dictionary", "props": { "data": { "name": "customerTier",
                "entries": [{ "id": "e0", "value": "VIP", "label": "v" }, { "id": "e1", "value": "STD", "label": "s" }] } } }]
        })),
    );
    let schema =
        json!({ "type": "object", "properties": { "tier": { "$dictionary": "customerTier" } } });
    inner.add(
        "g",
        document(json!({
            "imports": ["dicts"],
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode", "content": { "schema": schema.to_string() } },
                { "id": "out", "name": "out", "type": "outputNode", "content": {} }
            ],
            "edges": [{ "id": "e", "sourceId": "in", "targetId": "out", "sourceHandle": null }]
        })),
    );
    Arc::new(Flaky {
        inner,
        fails: AtomicUsize::new(fails),
    })
}

fn message(result: &Result<DecisionGraphResponse, Box<EvaluationError>>) -> String {
    match result {
        Ok(response) => format!("ok {}", serde_json::to_string(&response.result).unwrap()),
        Err(error) => format!("error {error}"),
    }
}

#[tokio::test]
async fn a_failed_dictionary_load_is_retried_by_the_cached_loader() {
    let loader: Arc<dyn DecisionLoader> = flaky(1);
    let engine = DecisionEngine::default()
        .compiled_engine()
        .with_loader(Arc::new(CachedLoader::from(loader)));
    let first = message(&engine.evaluate("g", json!({ "tier": "GOLD" }).into()).await);
    let second = message(&engine.evaluate("g", json!({ "tier": "GOLD" }).into()).await);
    let third = message(&engine.evaluate("g", json!({ "tier": "VIP" }).into()).await);
    for failed in [&first, &second] {
        assert!(
            failed.starts_with("error") && !failed.contains("dicts"),
            "{failed}"
        );
    }
    assert!(third.starts_with("ok"), "{third}");
}

#[tokio::test]
async fn resolve_schemas_keeps_failing_until_it_succeeds() {
    let loader = flaky(2);
    let DecisionContent::Graph(graph) = loader.inner.load("g").await.unwrap().as_ref().clone()
    else {
        panic!("graph");
    };
    let dynamic: DynamicLoader = loader;
    let mut graph = (*graph).clone();
    assert!(graph.resolve_schemas(&dynamic).await.is_err());
    assert!(graph.resolve_schemas(&dynamic).await.is_err());
    assert!(graph.resolve_schemas(&dynamic).await.is_ok());
    assert!(graph.resolve_schemas(&dynamic).await.is_ok());
}
