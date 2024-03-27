use crate::handler::node::{NodeRequest, NodeResult};
use anyhow::anyhow;

pub trait CustomNodeAdapter {
    fn handle(
        &self,
        request: &NodeRequest<'_>,
    ) -> impl std::future::Future<Output = NodeResult> + Send;
}

#[derive(Default)]
pub struct NoopCustomNode;

impl CustomNodeAdapter for NoopCustomNode {
    async fn handle(&self, _: &NodeRequest<'_>) -> NodeResult {
        Err(anyhow!("Custom node handler not provided"))
    }
}
