use crate::nodes::NodeResult;
use ahash::HashMap;
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use std::sync::Arc;
use zen_types::decision::SwitchStatement;
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
    #[serde(default = "null_variable")]
    pub input: Variable,
    #[serde(default = "null_variable")]
    pub output: Variable,
    pub name: Arc<str>,
    pub id: Arc<str>,
    #[serde(default)]
    pub performance: Option<Arc<str>>,
    #[serde(default)]
    pub trace_data: Option<Variable>,
    #[serde(default)]
    pub order: u32,
}

fn null_variable() -> Variable {
    Variable::Null
}

#[derive(Debug, Clone, ToVariable)]
pub struct ExpressionNodeTraceItem {
    pub(crate) result: Variable,
}

#[derive(Debug, Clone, Serialize, ToVariable)]
pub struct DecisionTableRowTrace {
    pub(crate) index: usize,
    pub(crate) reference_map: HashMap<Rc<str>, Variable>,
    pub(crate) rule: HashMap<Rc<str>, Rc<str>>,
}

#[derive(Debug, Clone, Serialize, ToVariable)]
#[serde(untagged)]
pub enum DecisionTableNodeTrace {
    FirstHit(DecisionTableRowTrace),
    Collect(Vec<DecisionTableRowTrace>),
}

impl Default for DecisionTableNodeTrace {
    fn default() -> Self {
        DecisionTableNodeTrace::Collect(Default::default())
    }
}

#[derive(ToVariable)]
pub(crate) struct SwitchStatementTrace {
    pub(crate) statements: Vec<SwitchStatementTraceRow>,
}

#[derive(ToVariable, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwitchStatementTraceRow {
    pub id: Arc<str>,
}

impl From<SwitchStatement> for SwitchStatementTraceRow {
    fn from(value: SwitchStatement) -> Self {
        Self { id: value.id }
    }
}
