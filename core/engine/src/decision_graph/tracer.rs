use crate::nodes::NodeResult;
use ahash::HashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zen_expression::variable::ToVariable;
use zen_types::decision::{DecisionNode, DecisionNodeKind};
use zen_types::variable::Variable;

pub(crate) struct NodeTracer(Option<HashMap<Arc<str>, DecisionGraphTrace>>);

impl NodeTracer {
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(|| HashMap::default()))
    }

    pub fn record_execution(
        &mut self,
        node: &DecisionNode,
        input_trace: Variable,
        result: &NodeResult,
        duration: std::time::Duration,
    ) {
        let Some(traces) = &mut self.0 else {
            return;
        };

        if matches!(node.kind, DecisionNodeKind::SwitchNode { .. }) {
            return;
        }

        let input = match &node.kind {
            DecisionNodeKind::InputNode { .. } => Variable::Null,
            _ => input_trace,
        };

        let mut trace = DecisionGraphTrace {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            order: traces.len() as u32,
            output: Variable::Null,
            trace_data: None,
            performance: Some(Arc::from(format!("{:.1?}", duration))),
        };

        match &result {
            Ok(ok) => {
                trace.trace_data = ok.trace_data.clone();
                if !matches!(node.kind, DecisionNodeKind::OutputNode { .. }) {
                    trace.output = ok.output.clone();
                }
            }
            Err(err) => {
                trace.trace_data = err.trace.clone();
            }
        };

        traces.insert(node.id.clone(), trace);
    }

    pub fn trace_callback(&mut self) -> Option<impl FnMut(DecisionGraphTrace) + '_> {
        let Some(traces) = &mut self.0 else {
            return None;
        };

        Some(|mut trace: DecisionGraphTrace| {
            trace.order = traces.len() as u32;
            traces.insert(trace.id.clone(), trace);
        })
    }

    pub fn into_traces(self) -> Option<HashMap<Arc<str>, DecisionGraphTrace>> {
        self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToVariable)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphTrace {
    pub input: Variable,
    pub output: Variable,
    pub name: Arc<str>,
    pub id: Arc<str>,
    pub performance: Option<Arc<str>>,
    pub trace_data: Option<Variable>,
    pub order: u32,
}
