use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::compiled::host::definition::NodeHandler;
use crate::compiled::host::function::purity::Purity;
use crate::compiled::host::function::v1::runtime::create_runtime;
use crate::compiled::host::function::v1::script::{Script, Session};
use crate::compiled::host::result::NodeResult;
use crate::compiled::host::{NodeContext, NodeContextBase, NodeContextExt, NodeError};
use serde_json::Value;
use zen_expression::variable::ToVariable;
use zen_expression::Variable;

pub(crate) mod runtime;
pub(super) mod script;

#[derive(Debug, Clone)]
pub struct FunctionV1NodeHandler;

const MAX_DURATION: Duration = Duration::from_millis(500);

impl NodeHandler for FunctionV1NodeHandler {
    type NodeData = Arc<str>;
    type TraceData = FunctionV1Trace;

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let start = Instant::now();
        let runtime = create_runtime().node_context_message(&ctx, "Failed to create JS Runtime")?;
        let interrupt_handler = Box::new(move || start.elapsed() > MAX_DURATION);

        runtime.set_interrupt_handler(Some(interrupt_handler));

        let mut script = Script::new(runtime.clone());
        let result_response = script
            .call(
                ctx.node.deref(),
                &ctx.input_with_nodes(),
                &ctx.extensions.eval,
            )
            .await;

        runtime.set_interrupt_handler(None);

        let response = result_response.node_context(&ctx)?;
        ctx.trace(|t| {
            t.log = response.log;
        });

        ctx.success(response.output)
    }
}

impl FunctionV1NodeHandler {
    pub(crate) fn batch(id: &Arc<str>, source: &Arc<str>, rows: usize) -> Option<Batch> {
        if rows < 2 {
            return None;
        }
        let layout = Purity::layout(source)?;
        Some(Batch {
            id: id.clone(),
            source: source.clone(),
            layout,
            session: None,
            failed: false,
        })
    }
}

pub(crate) struct Batch {
    id: Arc<str>,
    source: Arc<str>,
    layout: Rc<str>,
    session: Option<Session>,
    failed: bool,
}

impl Batch {
    pub(crate) fn call(
        &mut self,
        input: &Variable,
        nodes: Option<&Variable>,
        eval: &zen_expression::context::EvaluationContext,
    ) -> Option<Result<Variable, NodeError>> {
        if self.session.is_none() && !self.failed {
            self.session = Session::open(&self.source, MAX_DURATION);
            self.failed = self.session.is_none();
        }
        let session = self.session.as_ref()?;
        let result = session.call(&self.layout, &NodeContextBase::joined(input, nodes), eval);
        if result.is_err() || !session.settled() {
            self.session = None;
        }
        Some(
            result
                .map(|response| response.output)
                .map_err(|error| NodeError::new(self.id.clone(), error)),
        )
    }
}

#[derive(Debug, Clone, Default, ToVariable)]
pub struct FunctionV1Trace {
    log: Vec<Value>,
}
