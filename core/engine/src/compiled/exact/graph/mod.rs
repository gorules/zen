mod expression;
mod transform;

use crate::compiled::exact::RowTable;
use crate::compiled::host::decision_table::index::TableIndex;
use crate::compiled::host::{NodeError, NodeResponse, NodeResult};
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use zen_expression::clock::EvalContext;
use zen_expression::lane::{Programs, Session};
use zen_expression::variable::Variable;
use zen_types::decision::{DecisionNode, DecisionNodeKind, TransformAttributes};

pub(crate) type Sink = std::cell::RefCell<crate::compiled::trace::collect::Fact>;

#[derive(Clone)]
pub(crate) struct Row<'a> {
    pub(crate) id: &'a Arc<str>,
    pub(crate) input: Variable,
    pub(crate) nodes: Option<Variable>,
    pub(crate) programs: Option<&'a Arc<Programs>>,
    pub(crate) sink: Option<&'a Sink>,
    pub(crate) eval: EvalContext,
}

impl<'a> Row<'a> {
    pub(crate) fn session(&self) -> Session {
        let mut session = Session::with_environment(self.input.clone())
            .with_programs(self.programs.cloned())
            .with_eval(self.eval.clone());
        if let Some(nodes) = &self.nodes {
            session.set_local(Variable::nodes_key(), nodes.clone());
        }

        session
    }

    pub(crate) fn with_input(&self, input: Variable, eval: EvalContext) -> Self {
        Self {
            input,
            eval,
            ..self.clone()
        }
    }

    pub(crate) async fn node(
        self,
        node: &'a DecisionNode,
        index: Option<&'a TableIndex>,
        transforms: bool,
    ) -> NodeResult {
        let plain = TransformAttributes::default();
        match &node.kind {
            DecisionNodeKind::ExpressionNode { content } => {
                let attributes = match transforms {
                    true => &content.transform_attributes,
                    false => &plain,
                };
                self.transform(attributes, |input, _, eval| {
                    let row = self.with_input(input, eval);
                    async move { row.expression(content) }
                })
                .await
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                let attributes = match transforms {
                    true => &content.transform_attributes,
                    false => &plain,
                };
                self.transform(attributes, |input, _, eval| {
                    let row = self.with_input(input, eval);
                    async move { RowTable::graph(row, content, index) }
                })
                .await
            }
            _ => Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            }),
        }
    }

    pub(crate) fn now(
        self,
        node: &'a DecisionNode,
        index: Option<&'a TableIndex>,
        transforms: bool,
    ) -> NodeResult {
        let id = self.id.clone();
        let mut future = std::pin::pin!(self.node(node, index, transforms));
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result,
            Poll::Pending => Err(NodeError::new(id, "node did not complete")),
        }
    }
}

impl Row<'_> {
    pub(crate) fn traced(&self) -> bool {
        self.sink.is_some()
    }

    pub(crate) fn push(&self, fact: crate::compiled::trace::collect::Fact) {
        if let Some(sink) = self.sink {
            *sink.borrow_mut() = fact;
        }
    }

    pub(crate) fn take(&self) -> crate::compiled::trace::collect::Fact {
        match self.sink {
            Some(sink) => std::mem::replace(
                &mut *sink.borrow_mut(),
                crate::compiled::trace::collect::Fact::None,
            ),
            None => crate::compiled::trace::collect::Fact::None,
        }
    }

    pub(crate) fn error(&self, source: impl Into<Box<dyn std::error::Error>>) -> NodeError {
        NodeError::new(self.id.clone(), source)
    }

    pub(crate) fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: None,
        })
    }
}
