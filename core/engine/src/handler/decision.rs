use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::loader::DecisionLoader;
use crate::model::custom_node_adapter::CustomNodeAdapter;
use crate::model::DecisionNodeKind;
use anyhow::{anyhow, Context};
use async_recursion::async_recursion;
use rquickjs::Runtime;
use std::ops::Deref;
use std::sync::Arc;

pub struct DecisionHandler<L: DecisionLoader, A: CustomNodeAdapter> {
    trace: bool,
    loader: Arc<L>,
    adapter: Arc<A>,
    max_depth: u8,
    js_runtime: Option<Runtime>,
}

impl<L: DecisionLoader, A: CustomNodeAdapter> DecisionHandler<L, A> {
    pub fn new(
        trace: bool,
        max_depth: u8,
        loader: Arc<L>,
        adapter: Arc<A>,
        js_runtime: Option<Runtime>,
    ) -> Self {
        Self {
            trace,
            loader,
            adapter,
            max_depth,
            js_runtime,
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
        .with_runtime(self.js_runtime.clone());

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
