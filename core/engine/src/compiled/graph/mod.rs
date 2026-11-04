mod batched;
pub(crate) mod call;
mod plan;
mod state;
mod step;
pub(crate) mod tap;
pub(crate) mod traced;
pub(crate) mod traces;
pub use columnar::ColumnarOutput;
pub(super) mod columnar;
mod data;
pub(crate) mod errors;
mod schedule;
mod schema;

use crate::compiled::content::Content;
use crate::compiled::host::graph::walker::StableDiDecisionGraph;
use crate::compiled::host::NodeHandlerExtensions;
use crate::decision_graph::graph::DecisionGraphResponse;
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
    template: OnceLock<Arc<crate::compiled::trace::collect::Template>>,
    census: usize,
}

pub struct CompiledPlan(Result<CompiledGraph, Invalid>);

pub(crate) enum Invalid {
    Build(DecisionGraphValidationError),
    Validation(DecisionGraphValidationError),
}

impl Invalid {
    fn duplicate(error: &DecisionGraphValidationError) -> DecisionGraphValidationError {
        match error {
            DecisionGraphValidationError::InvalidInputCount(count) => {
                DecisionGraphValidationError::InvalidInputCount(*count)
            }
            DecisionGraphValidationError::InvalidOutputCount(count) => {
                DecisionGraphValidationError::InvalidOutputCount(*count)
            }
            DecisionGraphValidationError::CyclicGraph => DecisionGraphValidationError::CyclicGraph,
            DecisionGraphValidationError::MissingNode(node) => {
                DecisionGraphValidationError::MissingNode(node.clone())
            }
        }
    }

    pub(crate) fn evaluation(&self) -> Box<EvaluationError> {
        match self {
            Invalid::Build(error) | Invalid::Validation(error) => {
                Box::new(EvaluationError::InvalidGraph(Self::duplicate(error)))
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
            Invalid::Build(error) => Box::new(Self::duplicate(error)),
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
    pub fn compile(content: &Content) -> Self {
        Self(CompiledGraph::compile(content, true))
    }

    pub(crate) fn hosted(content: &Content) -> Self {
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

    pub(crate) fn template(&self) -> &Arc<crate::compiled::trace::collect::Template> {
        self.template.get_or_init(|| {
            Arc::new(crate::compiled::trace::collect::Template::new(
                self.steps
                    .iter()
                    .map(|step| step.as_ref().map(|step| step.node.clone())),
            ))
        })
    }

    pub(crate) fn plan_verdict(&self, columns: &Columns) -> Result<(), String> {
        self.plan()
            .verdict(self, columns)
            .map_err(|refusal| refusal.to_string())
    }

    pub(crate) async fn evaluate(
        &self,
        content: &Arc<Content>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        inputs: &[Variable],
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        self.resumed(content, extensions, max_depth, inputs, None)
            .await
    }

    pub(in crate::compiled::graph) async fn resumed(
        &self,
        content: &Arc<Content>,
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

    pub(crate) async fn traced_batch(
        &self,
        content: &Arc<Content>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        inputs: &[Variable],
        requests: &[Variable],
    ) -> (
        Vec<Result<Variable, Box<EvaluationError>>>,
        traces::Traces<'static>,
    ) {
        let nesting = Nesting {
            traced: true,
            ..Nesting::default()
        };
        let mut state = self
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
        let mut traces = state
            .traces
            .take()
            .unwrap_or_else(|| traces::Traces::new(self.template().clone(), 0, inputs.len()));
        let errors = state.errors.into_vec();
        let mut results = Vec::with_capacity(inputs.len());
        for (row, (error, result)) in errors.into_iter().zip(state.results).enumerate() {
            match error {
                None => results.push(Ok(result.unwrap_or_else(Variable::empty_object))),
                Some(error) => {
                    let Some(request) = requests.get(row) else {
                        results.push(Err(error));
                        continue;
                    };
                    let fresh = NodeHandlerExtensions {
                        function_runtime: Default::default(),
                        ..extensions.clone()
                    };
                    let traced = traced::Traced {
                        graph: self,
                        content,
                        extensions: &fresh,
                        max_depth,
                        iteration: 0,
                        parent: None,
                    };
                    let run = match zen_expression::clock::current() {
                        Some(clock) if clock.is_batch() => {
                            Box::pin(crate::compiled::pinning::with(
                                clock.fresh_stream(row),
                                traced.run(request.deep_clone()),
                            ))
                            .await
                        }
                        _ => Box::pin(traced.run(request.deep_clone())).await,
                    };
                    if let Some(slot) = traces.replaced.get_mut(row) {
                        *slot = Some(run.level);
                    }
                    results.push(Err(error));
                }
            }
        }
        (results, traces)
    }

    pub(crate) async fn evaluate_one(
        &self,
        content: &Arc<Content>,
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
