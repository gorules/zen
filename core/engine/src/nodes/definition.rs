use crate::nodes::context::NodeContext;
use crate::nodes::result::NodeResult;
use crate::nodes::NodeError;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use zen_types::decision::TransformAttributes;
use zen_types::variable::ToVariable;

pub trait NodeDataType: Clone + Debug + Serialize + for<'de> Deserialize<'de> {}
impl<T> NodeDataType for T where T: Clone + Debug + Serialize + for<'de> Deserialize<'de> {}

pub trait TraceDataType: Clone + Debug + Default + ToVariable {}
impl<T> TraceDataType for T where T: Clone + Debug + Default + ToVariable {}

pub trait NodeHandler: Clone {
    type NodeData: NodeDataType;
    type TraceData: TraceDataType;

    #[allow(unused_variables)]
    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        None
    }

    #[allow(unused_variables)]
    fn after_transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> impl std::future::Future<Output = Result<(), NodeError>> {
        Box::pin(async { Ok(()) })
    }

    fn handle(
        &self,
        ctx: NodeContext<Self::NodeData, Self::TraceData>,
    ) -> impl std::future::Future<Output = NodeResult>;
}
