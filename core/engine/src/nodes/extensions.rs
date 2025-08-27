use crate::handler::custom_node_adapter::DynamicCustomNode;
use crate::loader::DynamicLoader;
use crate::nodes::function::v2::function::{Function, FunctionConfig};
use crate::nodes::function::v2::module::console::ConsoleListener;
use crate::nodes::function::v2::module::zen::ZenListener;
use anyhow::Context;
use std::cell::OnceCell;
use std::sync::Arc;

/// This is created on every graph evaluation
#[derive(Clone)]
pub struct NodeHandlerExtensions {
    tokio_runtime: Arc<OnceCell<tokio::runtime::Runtime>>,
    function_runtime: Arc<OnceCell<Function>>,

    decision_loader: Arc<DynamicLoader>,
    custom_node_adapter: Arc<DynamicCustomNode>,
}

impl NodeHandlerExtensions {
    pub fn tokio_runtime(&self) -> anyhow::Result<&tokio::runtime::Runtime> {
        if let Some(tokio_runtime) = self.tokio_runtime.get() {
            return Ok(tokio_runtime);
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("Failed to build tokio runtime")?;

        let _ = self.tokio_runtime.set(runtime);
        self.tokio_runtime
            .get()
            .context("Tokio runtime is not initialized")
    }

    pub fn function_runtime(&self) -> anyhow::Result<&Function> {
        if let Some(function_runtime) = self.function_runtime.get() {
            return Ok(function_runtime);
        }

        let tokio_runtime = self.tokio_runtime()?;
        let function_runtime = tokio_runtime
            .block_on(Function::create(FunctionConfig {
                listeners: Some(vec![
                    Box::new(ConsoleListener),
                    Box::new(ZenListener {
                        loader: self.decision_loader.clone(),
                        adapter: self.custom_node_adapter.clone(),
                    }),
                ]),
            }))
            .context("Failed to create async function")?;

        let _ = self.function_runtime.set(function_runtime);
        self.function_runtime
            .get()
            .context("Tokio runtime is not initialized")
    }
}
