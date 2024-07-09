use crate::handler::custom_node_adapter::CustomNodeAdapter;
use crate::handler::function::function::Function;
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::loader::DecisionLoader;
use crate::model::DecisionNodeKind;
use anyhow::{anyhow, Context};
use async_recursion::async_recursion;
use rquickjs::Runtime;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

pub struct DecisionHandler<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> {
    trace: bool,
    loader: Arc<L>,
    adapter: Arc<A>,
    max_depth: u8,
    js_function: Option<Rc<Function>>,
}

impl<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> DecisionHandler<L, A> {
    pub fn new(
        trace: bool,
        max_depth: u8,
        loader: Arc<L>,
        adapter: Arc<A>,
        js_function: Option<Rc<Function>>,
    ) -> Self {
        Self {
            trace,
            loader,
            adapter,
            max_depth,
            js_function,
        }
    }

    #[async_recursion(?Send)]
    pub async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::DecisionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let sub_decision = self.loader.load(&content.key).await?;
        let mut sub_tree = DecisionGraph::try_new(DecisionGraphConfig {
            content: sub_decision.deref(),
            max_depth: self.max_depth,
            loader: self.loader.clone(),
            adapter: self.adapter.clone(),
            iteration: request.iteration + 1,
            trace: self.trace,
        })?
        .with_function(self.js_function.clone());

        let result = sub_tree
            .evaluate(&request.input)
            .await
            .map_err(|e| e.source)?;

        Ok(NodeResponse {
            output: result.result,
            trace_data: self
                .trace
                .then(|| serde_json::to_value(result.trace).context("Failed to parse trace data"))
                .transpose()?,
        })
    }
}
