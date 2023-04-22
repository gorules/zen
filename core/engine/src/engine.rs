use crate::decision::Decision;
use crate::handler::tree::GraphResponse;
use crate::loader::{ClosureLoader, DecisionLoader, LoaderResponse, LoaderResult, NoopLoader};
use crate::model::DecisionContent;

use serde_json::Value;
use std::future::Future;

use crate::EvaluationError;
use std::sync::Arc;

/// Structure used for generating and evaluating JDM decisions
#[derive(Debug, Clone)]
pub struct DecisionEngine<L>
where
    L: DecisionLoader,
{
    loader: Arc<L>,
}

#[derive(Debug, Default)]
pub struct EvaluationOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
}

impl Default for DecisionEngine<NoopLoader> {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader::default()),
        }
    }
}

impl<F, O> DecisionEngine<ClosureLoader<F>>
where
    F: Fn(&str) -> O + Sync + Send,
    O: Future<Output = LoaderResponse> + Send,
{
    pub fn async_loader(loader: F) -> Self {
        Self {
            loader: Arc::new(ClosureLoader::new(loader)),
        }
    }
}

impl<L: DecisionLoader> DecisionEngine<L> {
    pub fn new<Loader>(loader: Loader) -> Self
    where
        Loader: Into<Arc<L>>,
    {
        Self {
            loader: loader.into(),
        }
    }

    pub fn new_arc(loader: Arc<L>) -> Self {
        Self { loader }
    }

    /// Evaluates a decision through loader using a key
    pub async fn evaluate<K>(
        &self,
        key: K,
        context: &Value,
    ) -> Result<GraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        self.evaluate_with_opts(key, context, Default::default())
            .await
    }

    /// Evaluates a decision through loader using a key with advanced options
    pub async fn evaluate_with_opts<K>(
        &self,
        key: K,
        context: &Value,
        options: EvaluationOptions,
    ) -> Result<GraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        let content = self.loader.load(key.as_ref()).await?;
        let decision = self.create_decision(content);
        decision.evaluate_with_opts(context, options).await
    }

    /// Creates a decision from DecisionContent, exists for easier binding creation
    pub fn create_decision(&self, content: Arc<DecisionContent>) -> Decision<L> {
        Decision::from(content).with_loader(self.loader.clone())
    }

    /// Retrieves a decision based on the loader
    pub async fn get_decision(&self, key: &str) -> LoaderResult<Decision<L>> {
        let content = self.loader.load(key).await?;
        Ok(self.create_decision(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{FilesystemLoader, FilesystemLoaderOptions, MemoryLoader};
    use crate::model::DecisionContent;
    use serde_json::json;
    use std::ops::Deref;
    use std::path::Path;

    #[test]
    fn it_supports_memory_loader() {
        let mem_loader = MemoryLoader::default();

        mem_loader.add(
            "table",
            serde_json::from_str::<DecisionContent>(include_str!("../../../test-data/table.json"))
                .unwrap(),
        );

        mem_loader.add(
            "function",
            serde_json::from_str::<DecisionContent>(include_str!(
                "../../../test-data/function.json"
            ))
            .unwrap(),
        );

        let graph = DecisionEngine::new(mem_loader);
        let res1 = tokio_test::block_on(graph.evaluate("table", &json!({ "input": 12 })));
        let res2 = tokio_test::block_on(graph.evaluate("aaa", &json!({ "input": 12 })));

        assert_eq!(res1.unwrap().result, json!({"output": 10}));
        assert!(res2.is_err());
    }

    #[test]
    fn it_supports_filesystem_loader() {
        let cargo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let test_data_root = cargo_root.join("../../").join("test-data");
        let fs_loader = FilesystemLoader::new(FilesystemLoaderOptions {
            keep_in_memory: true,
            root: test_data_root.to_str().unwrap(),
        });

        let graph = DecisionEngine::new(fs_loader);
        let res1 = tokio_test::block_on(graph.evaluate("table.json", &json!({ "input": 12 })));
        let res2 = tokio_test::block_on(graph.evaluate("aaa", &json!({ "input": 12 })));

        assert_eq!(res1.unwrap().result, json!({"output": 10}));
        assert!(res2.is_err());
    }

    #[test]
    fn it_supports_closure_loader() {
        let graph = DecisionEngine::async_loader(|_| async {
            let content = serde_json::from_str::<DecisionContent>(include_str!(
                "../../../test-data/table.json"
            ))
            .unwrap();

            Ok(Arc::new(content))
        });

        let res1 = tokio_test::block_on(graph.evaluate("sample", &json!({ "input": 12 })));
        let res2 = tokio_test::block_on(graph.evaluate("1", &json!({ "input": 4 })));

        assert_eq!(res1.unwrap().result, json!({"output": 10}));
        assert_eq!(res2.unwrap().result, json!({"output": 0}))
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn it_throws_correct_error_type() {
        let cargo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let test_data_root = cargo_root.join("../../").join("test-data");
        let fs_loader = FilesystemLoader::new(FilesystemLoaderOptions {
            keep_in_memory: true,
            root: test_data_root.to_str().unwrap(),
        });

        let graph = DecisionEngine::new(fs_loader);

        let infinite_fn =
            tokio_test::block_on(graph.evaluate("infinite-function.json", &json!({})));

        match infinite_fn.unwrap_err().deref() {
            EvaluationError::NodeError(e) => {
                assert_eq!(e.node_id, "e0fd96d0-44dc-4f0e-b825-06e56b442d78");
                assert_eq!(e.source.to_string(), "Timeout exceeded");
            }
            _ => assert!(false, "Wrong error type"),
        }
    }
}
