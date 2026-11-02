mod batched;
pub(crate) mod call;
mod plan;
mod state;
mod step;
pub(crate) mod traced;
pub use columnar::ColumnarOutput;
pub(super) mod columnar;
mod data;
pub(crate) mod errors;
mod schedule;
mod schema;

use crate::decision_graph::graph::DecisionGraphResponse;
use crate::decision_graph::walker::StableDiDecisionGraph;
use crate::model::GraphContent;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use schedule::Segment;
use state::Completed;
use state::Host;
use state::NodeColumn;
pub(crate) use state::{Nesting, State};
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use step::{Body, Condition, Hit, Kind, Step, Transform};
use zen_expression::lane::Columns;
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;

pub struct CompiledGraph {
    graph: StableDiDecisionGraph,
    steps: Vec<Option<Step>>,
    root: Arc<Segment>,
    dictionaries: bool,
    plan: OnceLock<plan::Plan>,
    census: usize,
}

pub struct CompiledPlan(Result<CompiledGraph, Invalid>);

#[derive(Clone)]
pub(crate) enum Invalid {
    Build(DecisionGraphValidationError),
    Validation(DecisionGraphValidationError),
}

impl Invalid {
    pub(crate) fn evaluation(&self) -> Box<EvaluationError> {
        match self {
            Invalid::Build(error) | Invalid::Validation(error) => {
                Box::new(EvaluationError::InvalidGraph(error.clone()))
            }
        }
    }

    fn reason(&self) -> String {
        match self {
            Invalid::Build(error) => error.to_string(),
            Invalid::Validation(_) => "graph fails validation".into(),
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
            Err(invalid) => write!(f, "CompiledPlan(Err({}))", invalid.reason()),
        }
    }
}

impl CompiledPlan {
    pub fn compile(content: &GraphContent) -> Self {
        Self(CompiledGraph::compile(content, true))
    }

    pub(crate) fn hosted(content: &GraphContent) -> Self {
        Self(CompiledGraph::compile(content, false))
    }

    pub fn verdict(&self) -> Result<(), String> {
        self.0.as_ref().map(|_| ()).map_err(Invalid::reason)
    }

    pub(crate) fn graph(&self) -> Result<&CompiledGraph, &Invalid> {
        self.0.as_ref()
    }
}

impl CompiledGraph {
    thread_local! {
        static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
    }

    pub(crate) fn census() -> Vec<(String, u64, u64, u128)> {
        crate::compiled::census::Census::take()
    }

    pub(crate) fn degraded(&self) -> Vec<(Arc<str>, Arc<str>)> {
        let mut out = Vec::new();
        for step in self.steps.iter().flatten() {
            match &step.kind {
                Kind::Host(Some(why)) => out.push((step.node.id.clone(), why.clone())),
                Kind::Switch { conditions, .. } => out.extend(conditions.iter().filter_map(
                    |(_, condition)| match condition {
                        Condition::Invalid(why) => Some((step.node.id.clone(), why.clone())),
                        _ => None,
                    },
                )),
                _ => {}
            }
        }
        out
    }

    fn plan(&self) -> &plan::Plan {
        self.plan.get_or_init(|| plan::Plan::analyze(self))
    }

    pub(crate) fn plan_verdict(&self, columns: &Columns) -> Result<(), String> {
        self.plan()
            .verdict(self, columns)
            .map_err(|refusal| refusal.to_string())
    }

    pub(crate) async fn evaluate(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        inputs: &[Variable],
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        self.resumed(content, extensions, max_depth, inputs, None)
            .await
    }

    pub(in crate::compiled::graph) async fn resumed(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        inputs: &[Variable],
        completed: Option<&Completed>,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let start = Instant::now();
        let nesting = Nesting {
            completed,
            ..Nesting::default()
        };
        let state = self
            .run(
                Host {
                    content,
                    extensions,
                    max_depth,
                },
                inputs,
                nesting,
            )
            .await;
        let performance = format!("{:.1?}", start.elapsed());
        state
            .errors
            .into_vec()
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
            .run(
                Host {
                    content,
                    extensions,
                    max_depth,
                },
                &inputs,
                Nesting::default(),
            )
            .await;
        if let Some(error) = state.errors.into_vec().into_iter().next().flatten() {
            return Err(error);
        }
        Ok(DecisionGraphResponse {
            performance: format!("{:.1?}", start.elapsed()),
            result: state
                .results
                .into_iter()
                .next()
                .flatten()
                .unwrap_or_else(Variable::empty_object),
            trace: None,
        })
    }
}
