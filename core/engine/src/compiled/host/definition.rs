use crate::compiled::host::context::NodeContext;
use crate::compiled::host::result::NodeResult;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use zen_types::variable::ToVariable;

pub trait NodeDataType: Clone + Debug + Serialize + for<'de> Deserialize<'de> {}
impl<T> NodeDataType for T where T: Clone + Debug + Serialize + for<'de> Deserialize<'de> {}

pub trait TraceDataType: Clone + Debug + Default + ToVariable {}
impl<T> TraceDataType for T where T: Clone + Debug + Default + ToVariable {}

pub trait NodeHandler: Clone {
    type NodeData: NodeDataType;
    type TraceData: TraceDataType;

    fn handle(
        &self,
        ctx: NodeContext<Self::NodeData, Self::TraceData>,
    ) -> impl std::future::Future<Output = NodeResult>;
}
