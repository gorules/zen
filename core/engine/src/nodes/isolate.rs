use crate::nodes::{NodeContext, NodeContextBase, NodeDataType, NodeHandlerExtensions, TraceDataType};
use zen_expression::{Isolate, Variable};

impl NodeHandlerExtensions {
    pub(crate) fn isolate(&self, input: &Variable, nodes: Option<&Variable>) -> Isolate {
        let mut isolate = Isolate::with_environment(input.clone()).with_cache(self.compiled_cache.clone());
        if let Some(nodes) = nodes {
            isolate.set_local(Variable::nodes_key(), nodes.clone());
        }

        isolate
    }
}

impl<NodeData, TraceData> NodeContext<NodeData, TraceData>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    pub fn isolate(&self) -> Isolate {
        self.extensions.isolate(&self.input, self.nodes.as_ref())
    }
}

impl NodeContextBase {
    pub fn isolate(&self) -> Isolate {
        self.extensions.isolate(&self.input, self.nodes.as_ref())
    }
}
