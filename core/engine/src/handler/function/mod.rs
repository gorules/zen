use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ::serde::{Deserialize, Serialize};
use anyhow::{anyhow, Context};
use rquickjs::prelude::{Async, Func, Opt};
use rquickjs::{async_with, AsyncContext, AsyncRuntime, CatchResultExt, Ctx, IntoJs};
use serde_json::{json, Value};

use crate::handler::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::handler::function::error::{FunctionError, FunctionResult};
use crate::handler::function::function::{Function, HandlerResponse};
use crate::handler::function::module::throw_js_error;
use crate::handler::function::serde::JsValue;
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::loader::{DecisionLoader, NoopLoader};
use crate::model::DecisionNodeKind;

mod error;
pub(crate) mod function;
mod listener;
mod module;
mod serde;

#[derive(Serialize, Deserialize)]
pub struct FunctionResponse {
    performance: String,
    data: Option<HandlerResponse>,
}

#[derive(Serialize, Deserialize)]
struct Output {
    lines: Vec<Value>,
    output: Value,
}

pub struct FunctionHandler<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> {
    trace: bool,
    function: Rc<Function>,

    loader: Arc<L>,
    adapter: Arc<A>,
    max_depth: u8,
}

static MAX_DURATION: Duration = Duration::from_millis(500);

impl<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> FunctionHandler<L, A> {
    pub fn new(
        trace: bool,
        function: Rc<Function>,
        loader: Arc<L>,
        adapter: Arc<A>,
        max_depth: u8,
    ) -> Self {
        Self {
            trace,
            function,
            loader,
            adapter,
            max_depth,
        }
    }

    pub async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::FunctionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let name = request.node.name.as_str();
        let start = std::time::Instant::now();
        let interrupt_handler = Box::new(move || start.elapsed() > MAX_DURATION);
        self.function
            .runtime()
            .set_interrupt_handler(Some(interrupt_handler))
            .await;

        self.attach_evaluate_fn(request).await;
        self.function
            .register_module(name, content.as_str())
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        let response = self
            .function
            .call_handler(name, JsValue(request.input.clone()))
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        self.function.runtime().set_interrupt_handler(None).await;

        Ok(NodeResponse {
            output: response.data,
            trace_data: self.trace.then(|| json!({ "log": response.logs })),
        })
    }

    async fn attach_evaluate_fn(&self, request: &NodeRequest<'_>) {
        async_with!(self.function.context() => |ctx| {
            let max_depth = self.max_depth;
            let trace = self.trace;
            let loader = self.loader.clone();
            // let loader = self
            let adapter = self.adapter.clone();
            let iteration = request.iteration;
            let input = request.input.clone();

           ctx.globals().set("test", Func::from(Async(
                move |ctx: Ctx, key: String, context: JsValue| {
                    let loader = loader.clone();

                    async move {
                        let load_result = loader.load(&key).await;
                        if !load_result.is_ok() {
                            return JsValue(Value::Null);
                        }

                        let r = load_result.unwrap();
                        let mut sub_tree = DecisionGraph::try_new(DecisionGraphConfig {
                            content: r.deref(),
                            max_depth,
                            loader: Arc::new(NoopLoader::default()),
                            adapter: Arc::new(NoopCustomNode::default()),
                            iteration: iteration + 1,
                            trace,
                        }).unwrap();

                        let s = sub_tree.evaluate(&context.0).await;
                        if !s.is_ok() {
                            return JsValue(Value::Null);
                        }

                        return JsValue(serde_json::to_value(s.unwrap()).unwrap())
                    }
                },
            ))).catch(&ctx).unwrap();
        })
        .await;
    }
}
