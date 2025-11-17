use crate::loader::{DynamicLoader, NoopLoader};
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::function::http_handler::DynamicHttpHandler;
use crate::nodes::function::v2::function::{Function, FunctionConfig};
use crate::nodes::function::v2::module::console::ConsoleListener;
use crate::nodes::function::v2::module::http::listener::HttpListener;
use crate::nodes::function::v2::module::zen::ZenListener;
use crate::nodes::validator_cache::ValidatorCache;
use anyhow::Context;
use std::cell::OnceCell;
use std::sync::Arc;

/// This is created on every graph evaluation
#[derive(Debug, Clone)]
pub struct NodeHandlerExtensions {
    pub(crate) function_runtime: Arc<tokio::sync::OnceCell<Function>>,
    pub(crate) validator_cache: Arc<OnceCell<ValidatorCache>>,
    pub(crate) loader: DynamicLoader,
    pub(crate) custom_node: DynamicCustomNode,
    pub(crate) http_handler: DynamicHttpHandler,
}

impl Default for NodeHandlerExtensions {
    fn default() -> Self {
        Self {
            function_runtime: Default::default(),
            validator_cache: Default::default(),

            loader: Arc::new(NoopLoader::default()),
            custom_node: Arc::new(NoopCustomNode::default()),
            http_handler: None,
        }
    }
}

impl NodeHandlerExtensions {
    pub async fn function_runtime(&self) -> anyhow::Result<&Function> {
        self.function_runtime
            .get_or_try_init(|| {
                Function::create(FunctionConfig {
                    listeners: Some(vec![
                        Box::new(ConsoleListener),
                        Box::new(HttpListener {
                            http_handler: self.http_handler.clone(),
                        }),
                        Box::new(ZenListener {
                            loader: self.loader.clone(),
                            custom_node: self.custom_node.clone(),
                            http_handler: self.http_handler.clone(),
                        }),
                    ]),
                })
            })
            .await
            .context("Failed to create function")
    }

    pub fn validator_cache(&self) -> &ValidatorCache {
        self.validator_cache
            .get_or_init(|| ValidatorCache::default())
    }

    pub fn custom_node(&self) -> &DynamicCustomNode {
        &self.custom_node
    }

    pub fn loader(&self) -> &DynamicLoader {
        &self.loader
    }

    pub fn http_handler(&self) -> &DynamicHttpHandler {
        &self.http_handler
    }
}
