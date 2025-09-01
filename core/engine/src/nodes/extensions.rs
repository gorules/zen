use crate::loader::{DynamicLoader, NoopLoader};
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::function::v2::function::{Function, FunctionConfig};
use crate::nodes::function::v2::module::console::ConsoleListener;
use crate::nodes::function::v2::module::zen::ZenListener;
use crate::nodes::validator_cache::ValidatorCache;
use std::cell::OnceCell;
use std::sync::{Arc, OnceLock};

/// This is created on every graph evaluation
#[derive(Clone)]
pub struct NodeHandlerExtensions {
    pub(crate) function_runtime: Arc<OnceCell<Function>>,
    pub(crate) tokio_runtime: Arc<OnceLock<tokio::runtime::Runtime>>,
    pub(crate) validator_cache: Arc<OnceCell<ValidatorCache>>,
    pub(crate) loader: DynamicLoader,
    pub(crate) custom_node: DynamicCustomNode,
}

impl Default for NodeHandlerExtensions {
    fn default() -> Self {
        Self {
            tokio_runtime: Default::default(),
            function_runtime: Default::default(),
            validator_cache: Default::default(),

            loader: Arc::new(NoopLoader::default()),
            custom_node: Arc::new(NoopCustomNode::default()),
        }
    }
}

impl NodeHandlerExtensions {
    pub fn tokio_runtime(&self) -> &tokio::runtime::Runtime {
        self.tokio_runtime.get_or_init(|| {
            println!("Creating tokio runtime");

            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime")
        })
    }

    pub fn function_runtime(&self) -> &Function {
        self.function_runtime.get_or_init(|| {
            let tokio_runtime = self.tokio_runtime();

            tokio_runtime
                .block_on(Function::create(FunctionConfig {
                    listeners: Some(vec![
                        Box::new(ConsoleListener),
                        Box::new(ZenListener {
                            extensions: self.clone(),
                        }),
                    ]),
                }))
                .expect("Failed to create async function")
        })
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
}
