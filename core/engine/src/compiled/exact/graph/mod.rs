mod expression;
mod transform;

use crate::compiled::exact::RowTable;
use crate::compiled::host::decision_table::index::TableIndex;
use crate::compiled::host::{NodeError, NodeResponse, NodeResult};
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use zen_expression::context::EvaluationContext;
use zen_expression::lane::{Programs, Session};
use zen_expression::variable::{ToVariable, Variable};
use zen_types::decision::{DecisionNode, DecisionNodeKind, TransformAttributes};

#[derive(Clone)]
pub(crate) struct Row<'a> {
    pub(crate) id: &'a Arc<str>,
    pub(crate) input: Variable,
    pub(crate) nodes: Option<Variable>,
    pub(crate) programs: Option<&'a Arc<Programs>>,
    pub(crate) trace: bool,
    pub(crate) eval: EvaluationContext,
}

pub(crate) struct Ctx<'a, T> {
    pub(crate) row: Row<'a>,
    pub(crate) trace: Option<T>,
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

    pub(crate) fn with_input(&self, input: Variable, eval: EvaluationContext) -> Self {
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

impl<'a, T: Default + ToVariable> Ctx<'a, T> {
    pub(crate) fn new(row: Row<'a>) -> Self {
        Self {
            trace: row.trace.then(T::default),
            row,
        }
    }

    pub(crate) fn trace(&mut self, mutator: impl FnOnce(&mut T)) {
        if let Some(trace) = &mut self.trace {
            mutator(trace);
        }
    }

    pub(crate) fn error(&self, source: impl Into<Box<dyn std::error::Error>>) -> NodeError {
        NodeError::traced(
            self.row.id.clone(),
            self.trace.as_ref().map(ToVariable::to_variable),
            source,
        )
    }

    pub(crate) fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: self.trace.as_ref().map(ToVariable::to_variable),
        })
    }
}
