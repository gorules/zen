use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::nodes::definition::NodeHandler;
use crate::nodes::function::v2::error::{FunctionError, FunctionResult};
use crate::nodes::function::v2::function::Function;
use crate::nodes::function::v2::module::console::Log;
use crate::nodes::function::v2::serde::{JsValue, JsValueWithNodes};
use crate::nodes::result::NodeResult;
use crate::nodes::{NodeContext, NodeContextConfig, NodeError, NodeHandlerExtensions};
use rquickjs::prelude::Func;
use rquickjs::{async_with, CatchResultExt, Object};
use serde_json::json;
use zen_expression::variable::ToVariable;
use zen_types::decision::FunctionContent;
use zen_types::variable::Variable;

pub(crate) mod error;
pub(crate) mod function;
pub(crate) mod isolation;
pub(crate) mod listener;
pub(crate) mod module;
pub(crate) mod serde;
pub(crate) mod strip;

#[derive(Debug, Clone)]
pub struct FunctionV2NodeHandler;

impl NodeHandler for FunctionV2NodeHandler {
    type NodeData = FunctionContent;
    type TraceData = FunctionV2Trace;

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let start = Instant::now();

        let function = ctx.function_runtime().await?;
        let source = ctx
            .extensions
            .stripped_functions
            .as_ref()
            .and_then(|stripped| stripped.get(ctx.node.source.as_ref()).cloned())
            .unwrap_or_else(|| strip::TypeStripper::strip(ctx.node.source.deref()));
        let module_name = function.suggest_module_name(ctx.id.deref(), source.as_ref());

        let max_duration = Duration::from_millis(ctx.config.function_timeout_millis);
        let interrupt_handler = Box::new(move || start.elapsed() > max_duration);

        function
            .runtime()
            .set_interrupt_handler(Some(interrupt_handler))
            .await;

        let function_context = FunctionContext {
            start,
            context: &ctx,
            function: &function,
        };

        self.attach_globals(function, &ctx)
            .await
            .function_context(&function_context)
            .await?;

        function
            .register_module(&module_name, source.as_ref())
            .await
            .function_context(&function_context)
            .await?;

        let response_result = function
            .call_handler(&module_name, JsValueWithNodes(JsValue(ctx.input.clone())))
            .await;

        function.runtime().set_interrupt_handler(None).await;

        let response = response_result.function_context(&function_context).await?;
        ctx.trace(|t| {
            t.log = response.logs.clone();
        });

        ctx.success(response.data)
    }
}

impl FunctionV2NodeHandler {
    async fn attach_globals(
        &self,
        function: &Function,
        node_ctx: &NodeContext<FunctionContent, FunctionV2Trace>,
    ) -> FunctionResult {
        async_with!(function.context() => |ctx| {
            let config = Object::new(ctx.clone()).catch(&ctx)?;

            config.prop("iteration", node_ctx.iteration).catch(&ctx)?;
            config.prop("maxDepth", node_ctx.config.max_depth).catch(&ctx)?;
            config.prop("trace", node_ctx.config.trace).catch(&ctx)?;

            ctx.globals().set("config", config).catch(&ctx)?;

            let nodes_data = node_ctx.nodes.clone().unwrap_or_default();

            ctx.globals()
                .set(
                    "__getNodesData",
                    Func::from(move || JsValue(nodes_data.clone())),
                )
                .catch(&ctx)?;

            Ok(())
        })
        .await
    }
}

impl FunctionV2NodeHandler {
    pub(crate) async fn batch(
        id: &Arc<str>,
        content: &FunctionContent,
        extensions: &NodeHandlerExtensions,
        config: &NodeContextConfig,
        rows: Vec<(Variable, Option<Variable>)>,
    ) -> Vec<Result<Variable, NodeError>> {
        let source = extensions
            .stripped_functions
            .as_ref()
            .and_then(|stripped| stripped.get(content.source.as_ref()).cloned())
            .unwrap_or_else(|| strip::TypeStripper::strip(content.source.deref()));
        if isolation::Isolation::shareable(source.as_ref()) || rows.len() < 2 {
            return Self::run(id, &source, extensions, config, rows).await;
        }
        let mut results = Vec::with_capacity(rows.len());
        for row in rows {
            let fresh = NodeHandlerExtensions {
                function_runtime: Default::default(),
                ..extensions.clone()
            };
            results.extend(Self::run(id, &source, &fresh, config, vec![row]).await);
        }
        results
    }

    async fn run(
        id: &Arc<str>,
        source: &Arc<str>,
        extensions: &NodeHandlerExtensions,
        config: &NodeContextConfig,
        rows: Vec<(Variable, Option<Variable>)>,
    ) -> Vec<Result<Variable, NodeError>> {
        let failed = |source: Box<dyn std::error::Error>| NodeError {
            node_id: id.clone(),
            trace: None,
            source,
        };
        let count = rows.len();
        let function = match extensions.function_runtime().await {
            Ok(function) => function,
            Err(error) => {
                let message = error.to_string();
                return (0..count).map(|_| Err(failed(message.clone().into()))).collect();
            }
        };
        let module_name = match function.shared_module(id.deref(), source.as_ref()).await {
            Ok(name) => name,
            Err(error) => {
                let message = error.to_string();
                return (0..count).map(|_| Err(failed(FunctionError::Caught(message.clone()).into()))).collect();
            }
        };
        let base = Instant::now();
        let started = Arc::new(AtomicU64::new(0));
        let limit = Duration::from_millis(config.function_timeout_millis);
        let watched = started.clone();
        function
            .runtime()
            .set_interrupt_handler(Some(Box::new(move || {
                base.elapsed().saturating_sub(Duration::from_nanos(watched.load(Ordering::Relaxed))) > limit
            })))
            .await;
        let tick = || started.store(base.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let results = function
            .call_rows(&module_name, (0, config.max_depth, config.trace), rows, &tick)
            .await;
        function.runtime().set_interrupt_handler(None).await;
        results
            .into_iter()
            .map(|result| result.map_err(|error| failed(error.into())))
            .collect()
    }
}

#[derive(Debug, Clone, Default, ToVariable)]
#[serde(rename_all = "camelCase")]
pub struct FunctionV2Trace {
    pub log: Vec<Log>,
}

struct FunctionContext<'a> {
    context: &'a NodeContext<FunctionContent, FunctionV2Trace>,
    function: &'a Function,
    start: Instant,
}

trait FunctionErrorExt<T> {
    async fn function_context(self, ctx: &FunctionContext) -> Result<T, NodeError>;
}

impl<T> FunctionErrorExt<T> for Result<T, FunctionError> {
    async fn function_context(self, c: &FunctionContext<'_>) -> Result<T, NodeError> {
        match self {
            Ok(ok) => Ok(ok),
            Err(err) => {
                let log = c.function.extract_logs().await;
                c.context.trace(|t| {
                    t.log = log;
                    t.log.push(Log {
                        lines: vec![json!(err.to_string()).to_string()],
                        ms_since_run: c.start.elapsed().as_millis() as usize,
                    });
                });

                Err(c.context.make_error(err))
            }
        }
    }
}
