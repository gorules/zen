use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::loader::DecisionLoader;
use crate::model::DecisionNodeKind;
use anyhow::{anyhow, Context};
use async_recursion::async_recursion;
use std::ops::Deref;
use std::sync::Arc;

pub struct DecisionHandler<T: DecisionLoader> {
    trace: bool,
    loader: Arc<T>,
    max_depth: u8,
}

impl<T: DecisionLoader> DecisionHandler<T> {
    pub fn new(trace: bool, max_depth: u8, loader: Arc<T>) -> Self {
        Self {
            trace,
            loader,
            max_depth,
        }
    }

    #[async_recursion(?Send)]
    pub async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::DecisionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let sub_decision = self.loader.load(&content.key).await?;
        let sub_tree = DecisionGraph::new(DecisionGraphConfig {
            content: sub_decision.deref(),
            max_depth: self.max_depth,
            loader: self.loader.clone(),
            iteration: request.iteration + 1,
            trace: self.trace,
        });

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
