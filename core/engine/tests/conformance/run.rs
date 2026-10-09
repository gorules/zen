use crate::conformance::{Case, Suite};
use crate::support::golden::{Canon, Outcome, Reference};
use serde_json::Value;
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::{NodeError, NodeResponse, NodeResult};
use zen_engine::policy::{EvaluateRequest, EvaluationResult, PolicyWorkspace, Severity};
use zen_engine::{Decision, DecisionEngine, DecisionGraphResponse, EvaluationError};
use zen_expression::Variable;

#[derive(Debug)]
pub struct SpecNodes;

impl SpecNodes {
    pub fn respond(request: &CustomNodeRequest) -> Result<Variable, String> {
        let config = &request.node.config;
        match request.node.kind.as_ref() {
            "echo" => Ok(request.input.clone()),
            "config" => Ok(Variable::from(config.as_ref().clone())),
            "render" => {
                let out = Variable::empty_object();
                for key in config.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default() {
                    let value = request.get_field(&key).map_err(|e| e.to_string())?.unwrap_or(Variable::Null);
                    out.dot_insert(&key, value);
                }
                Ok(out)
            }
            "fail" => Err(config.get("message").and_then(|m| m.as_str()).unwrap_or("custom node failed").to_string()),
            other => Err(format!("unknown spec custom node kind {other:?}")),
        }
    }
}

impl CustomNodeAdapter for SpecNodes {
    fn handle(&self, request: CustomNodeRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            Self::respond(&request)
                .map(|output| NodeResponse { output, trace_data: None })
                .map_err(|message| NodeError { trace: None, node_id: request.node.id.clone(), source: message.into() })
        })
    }
}

pub struct GraphRun {
    pub walker: Decision,
    pub compiled: Decision,
    pub engine: DecisionEngine,
}

impl GraphRun {
    pub fn new(suite: &Suite) -> Result<GraphRun, String> {
        let content = |value: &Value| -> Result<DecisionContent, String> {
            serde_json::from_value::<DecisionContent>(value.clone()).map_err(|e| format!("{}: invalid content: {e}", suite.name))
        };
        let (plain, built) = (Arc::new(MemoryLoader::default()), Arc::new(MemoryLoader::default()));
        for (key, doc) in &suite.documents {
            plain.add(key.as_str(), content(doc)?);
            built.add(key.as_str(), Self::compiled(content(doc)?));
        }
        let DecisionContent::Graph(graph) = content(&suite.content)? else {
            return Err(format!("{}: graph suite content is not a graph", suite.name));
        };
        let shared = Arc::new(MemoryLoader::default());
        for (key, doc) in &suite.documents {
            shared.add(key.as_str(), content(doc)?);
        }
        shared.add(Self::MAIN, content(&suite.content)?);
        let engine = DecisionEngine::default().with_loader(shared).with_adapter(Arc::new(SpecNodes));
        engine.compile();
        let walker = Decision::from(graph.clone()).with_loader(plain).with_adapter(Arc::new(SpecNodes));
        let mut compiled = Decision::from(graph).with_loader(built).with_adapter(Arc::new(SpecNodes));
        compiled.compile();
        Ok(GraphRun { walker, compiled, engine })
    }

    pub const MAIN: &'static str = "spec-main";

    pub fn compiled(content: DecisionContent) -> DecisionContent {
        match content {
            DecisionContent::Graph(mut graph) => {
                Arc::make_mut(&mut graph).compile();
                DecisionContent::Graph(graph)
            }
            other => other,
        }
    }

    pub fn outcome(result: Result<DecisionGraphResponse, Box<EvaluationError>>) -> Result<Variable, String> {
        result.map(|r| r.result).map_err(|e| Self::error(&e))
    }

    pub fn error(error: &EvaluationError) -> String {
        serde_json::to_value(error).map(|v| v.to_string()).unwrap_or_else(|_| error.to_string())
    }

    pub async fn walker(&self, case: &Case) -> Result<Variable, String> {
        Self::outcome(self.walker.evaluate(case.input.depth_clone(64)).await)
    }
}

pub struct PolicyRun {
    pub workspace: PolicyWorkspace,
    pub blocking: Vec<String>,
    pub gate: bool,
    pub engine: DecisionEngine,
}

impl PolicyRun {
    pub const MAIN: &'static str = "main";

    pub fn new(suite: &Suite) -> Result<PolicyRun, String> {
        let mut workspace = PolicyWorkspace::new();
        let loader = Arc::new(MemoryLoader::default());
        let policy = |value: &Value| serde_json::from_value(value.clone()).map_err(|e| format!("{}: invalid policy: {e}", suite.name));
        let document = |value: &Value| serde_json::from_value::<DecisionContent>(value.clone()).map_err(|e| format!("{}: invalid document: {e}", suite.name));
        workspace.set_policy(Self::MAIN, policy(&suite.content)?);
        loader.add(Self::MAIN, document(&suite.content)?);
        for (key, doc) in &suite.documents {
            match doc.get("nodes") {
                Some(_) => workspace.set_document(key.as_str(), document(doc)?),
                None => workspace.set_policy(key.as_str(), policy(doc)?),
            }
            loader.add(key.as_str(), document(doc)?);
        }
        let mut blocking: Vec<String> = workspace
            .evaluation_diagnostics(Self::MAIN)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| serde_json::to_value(d.code).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default())
            .collect();
        blocking.sort();
        blocking.dedup();
        let engine = DecisionEngine::default().with_loader(loader);
        let gate = suite.diagnostics.is_none();
        Ok(PolicyRun { workspace, blocking, gate, engine })
    }

    pub fn gated(&self, result: &Result<EvaluationResult, zen_engine::policy::EvaluationError>) -> Result<Variable, String> {
        match self.blocking.is_empty() || !self.gate {
            true => Self::outcome(result),
            false => Err(format!("CompilationErrors {:?}", self.blocking)),
        }
    }

    pub fn driver(&self, case: &Case) -> Result<Variable, String> {
        self.gated(&crate::support::golden::Live::policy(&self.workspace, &Self::request(case)))
    }

    pub fn compiled(&self, case: &Case) -> Result<Variable, String> {
        self.gated(&self.workspace.evaluate(&Self::request(case)))
    }

    pub fn engine(&self, case: &Case) -> Result<Variable, String> {
        let result = Guard::block(self.engine.evaluate(Self::MAIN, case.input.depth_clone(64)));
        match (&result, self.blocking.is_empty()) {
            (Err(_), false) => Err(format!("CompilationErrors {:?}", self.blocking)),
            (Err(error), true) => match &**error {
                EvaluationError::Policy(error) => Err(Reference::debug(error)),
                other => Err(GraphRun::error(other)),
            },
            (Ok(response), _) => Ok(response.result.clone()),
        }
    }

    pub fn request(case: &Case) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from(Self::MAIN),
            input: case.input.depth_clone(64),
            goals: case.goals.iter().map(|g| Arc::from(g.as_str())).collect(),
            trace: false,
        }
    }

    pub fn outcome(result: &Result<EvaluationResult, zen_engine::policy::EvaluationError>) -> Result<Variable, String> {
        result.as_ref().map(|r| r.output.clone()).map_err(Reference::debug)
    }
}

pub struct Guard;

impl Guard {
    pub fn run<T>(label: String, failures: &mut Vec<String>, work: impl FnOnce() -> T) -> Option<T> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
            Ok(value) => Some(value),
            Err(payload) => {
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                failures.push(format!("{label} | PANIC: {message}"));
                None
            }
        }
    }

    pub fn block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}

impl GraphRun {
    pub fn recorded(outcome: &Outcome) -> Result<Variable, String> {
        match outcome {
            Outcome::Ok(value) => Ok(Canon::restore(value)),
            Outcome::Err { value, .. } => Err(value.to_string()),
        }
    }
}

impl PolicyRun {
    pub fn recorded(&self, outcome: &Outcome) -> Result<Variable, String> {
        match (self.blocking.is_empty() || !self.gate, outcome) {
            (false, _) => Err(format!("CompilationErrors {:?}", self.blocking)),
            (true, Outcome::Ok(value)) => Ok(Canon::restore(value)),
            (true, Outcome::Err { debug, .. }) => Err(debug.clone()),
        }
    }
}
