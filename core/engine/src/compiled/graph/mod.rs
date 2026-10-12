mod batched;
pub(super) mod bound;
mod plan;
pub(crate) mod call;
pub(crate) mod traced;
mod state;
mod step;
pub use columnar::{ColumnarOutput, OutputColumn, RecordsView};
mod schedule;
pub(super) mod table;
pub(super) mod columnar;
mod data;
mod schema;

use crate::decision_graph::graph::DecisionGraphResponse;
use crate::decision_graph::walker::StableDiDecisionGraph;
use crate::model::GraphContent;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use schedule::Segment;
pub(crate) use plan::census::Census;
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use zen_expression::lane::Columns;
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;
pub(crate) use state::{Nesting, State};
use state::Host;
use state::NodeColumn;
use step::{Body, Condition, Hit, Kind, Step, Transform};

pub struct CompiledGraph {
    graph: StableDiDecisionGraph,
    steps: Vec<Option<Step>>,
    root: Arc<Segment>,
    dictionaries: bool,
    plan: OnceLock<plan::Plan>,
}

pub struct CompiledPlan(Result<CompiledGraph, Refusal>);

pub(crate) enum Refusal {
    Invalid(Arc<str>, Invalid),
}

#[derive(Clone)]
pub(crate) enum Invalid {
    Build(DecisionGraphValidationError),
    Validation(DecisionGraphValidationError),
}

impl Invalid {
    pub(crate) fn evaluation(&self) -> Box<EvaluationError> {
        match self {
            Invalid::Build(error) | Invalid::Validation(error) => Box::new(EvaluationError::InvalidGraph(error.clone())),
        }
    }

    pub(crate) fn source(&self) -> Box<dyn std::error::Error> {
        match self {
            Invalid::Build(error) => Box::new(error.clone()),
            Invalid::Validation(_) => self.evaluation().to_string().into(),
        }
    }
}

impl Debug for CompiledPlan {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Ok(_) => write!(f, "CompiledPlan(Ok)"),
            Err(Refusal::Invalid(reason, _)) => write!(f, "CompiledPlan(Err({reason}))"),
        }
    }
}

impl PartialEq for CompiledPlan {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl CompiledPlan {
    pub fn compile(content: &GraphContent) -> Self {
        Self(CompiledGraph::compile(content, true))
    }

    pub(crate) fn hosted(content: &GraphContent) -> Self {
        Self(CompiledGraph::compile(content, false))
    }

    pub fn verdict(&self) -> Result<(), &str> {
        match &self.0 {
            Ok(_) => Ok(()),
            Err(Refusal::Invalid(reason, _)) => Err(reason.as_ref()),
        }
    }

    pub(crate) fn graph(&self) -> Result<&CompiledGraph, &Invalid> {
        match &self.0 {
            Ok(graph) => Ok(graph),
            Err(Refusal::Invalid(_, invalid)) => Err(invalid),
        }
    }
}


impl CompiledGraph {
    thread_local! {
        static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
    }

    pub(crate) fn census() -> Vec<(String, u64, u64, u128)> {
        plan::census::Census::take()
    }

    fn plan(&self) -> &plan::Plan {
        self.plan.get_or_init(|| plan::Plan::analyze(self))
    }

    pub(crate) fn plan_verdict(&self, columns: &Columns) -> Result<(), String> {
        self.plan().verdict(self, columns)
    }

    pub(crate) async fn evaluate(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        inputs: &[Variable],
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let start = Instant::now();
        let state = self
            .run(Host { content, extensions, max_depth }, inputs, Nesting::default())
            .await;
        let performance = format!("{:.1?}", start.elapsed());
        state
            .errors
            .into_iter()
            .zip(state.results)
            .map(|(error, result)| match error {
                Some(error) => Err(error),
                None => Ok(DecisionGraphResponse {
                    performance: performance.clone(),
                    result: result.unwrap_or_else(Variable::empty_object),
                    trace: None,
                }),
            })
            .collect()
    }

    pub(crate) async fn evaluate_one(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        input: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let start = Instant::now();
        let inputs = [input];
        let state = self
            .run(Host { content, extensions, max_depth }, &inputs, Nesting::default())
            .await;
        if let Some(error) = state.errors.into_iter().next().flatten() {
            return Err(error);
        }
        Ok(DecisionGraphResponse {
            performance: format!("{:.1?}", start.elapsed()),
            result: state.results.into_iter().next().flatten().unwrap_or_else(Variable::empty_object),
            trace: None,
        })
    }
}
