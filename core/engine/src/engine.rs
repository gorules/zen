use crate::decision::Decision;
use crate::handler::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::handler::graph::DecisionGraphResponse;
use crate::loader::{ClosureLoader, DecisionLoader, LoaderResponse, LoaderResult, NoopLoader};
use crate::model::DecisionContent;
use crate::EvaluationError;
use serde_json::Value;
use std::future::Future;
use std::sync::Arc;
use strum::{EnumString, IntoStaticStr};
use zen_expression::variable::Variable;

/// Structure used for generating and evaluating JDM decisions
#[derive(Debug, Clone)]
pub struct DecisionEngine<Loader, CustomNode>
where
    Loader: DecisionLoader + 'static,
    CustomNode: CustomNodeAdapter + 'static,
{
    loader: Arc<Loader>,
    adapter: Arc<CustomNode>,
}

#[derive(Debug, Default)]
pub struct EvaluationOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
}

#[derive(Debug, Default)]
pub struct EvaluationSerializedOptions {
    pub trace: EvaluationTraceKind,
    pub max_depth: Option<u8>,
}

#[derive(Debug, Default, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum EvaluationTraceKind {
    #[default]
    None,
    Default,
    String,
    Reference,
    ReferenceString,
}

impl EvaluationTraceKind {
    pub fn serialize_trace(&self, trace: &Variable) -> Value {
        match self {
            EvaluationTraceKind::None => Value::Null,
            EvaluationTraceKind::Default => serde_json::to_value(&trace).unwrap_or_default(),
            EvaluationTraceKind::String => {
                Value::String(serde_json::to_string(&trace).unwrap_or_default())
            }
            EvaluationTraceKind::Reference => {
                serde_json::to_value(&trace.serialize_ref()).unwrap_or_default()
            }
            EvaluationTraceKind::ReferenceString => {
                Value::String(serde_json::to_string(&trace.serialize_ref()).unwrap_or_default())
            }
        }
    }
}

impl Default for DecisionEngine<NoopLoader, NoopCustomNode> {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
        }
    }
}

impl<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> DecisionEngine<L, A> {
    pub fn new(loader: Arc<L>, adapter: Arc<A>) -> Self {
        Self { loader, adapter }
    }

    pub fn with_adapter<CustomNode>(self, adapter: Arc<CustomNode>) -> DecisionEngine<L, CustomNode>
    where
        CustomNode: CustomNodeAdapter,
    {
        DecisionEngine {
            loader: self.loader,
            adapter,
        }
    }

    pub fn with_loader<Loader>(self, loader: Arc<Loader>) -> DecisionEngine<Loader, A>
    where
        Loader: DecisionLoader,
    {
        DecisionEngine {
            loader,
            adapter: self.adapter,
        }
    }

    pub fn with_closure_loader<F, O>(self, loader: F) -> DecisionEngine<ClosureLoader<F>, A>
    where
        F: Fn(String) -> O + Sync + Send,
        O: Future<Output = LoaderResponse> + Send,
    {
        DecisionEngine {
            loader: Arc::new(ClosureLoader::new(loader)),
            adapter: self.adapter,
        }
    }

    /// Evaluates a decision through loader using a key
    pub async fn evaluate<K>(
        &self,
        key: K,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        self.evaluate_with_opts(key, context, Default::default())
            .await
    }

    /// Evaluates a decision through loader using a key with advanced options
    pub async fn evaluate_with_opts<K>(
        &self,
        key: K,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        let content = self.loader.load(key.as_ref()).await?;
        let decision = self.create_decision(content);
        decision.evaluate_with_opts(context, options).await
    }

    pub async fn evaluate_serialized<K>(
        &self,
        key: K,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value>
    where
        K: AsRef<str>,
    {
        let content = self
            .loader
            .load(key.as_ref())
            .await
            .map_err(|err| Value::String(err.to_string()))?;

        let decision = self.create_decision(content);
        decision.evaluate_serialized(context, options).await
    }

    /// Creates a decision from DecisionContent, exists for easier binding creation
    pub fn create_decision(&self, content: Arc<DecisionContent>) -> Decision<L, A> {
        Decision::from(content)
            .with_loader(self.loader.clone())
            .with_adapter(self.adapter.clone())
    }

    /// Retrieves a decision based on the loader
    pub async fn get_decision(&self, key: &str) -> LoaderResult<Decision<L, A>> {
        let content = self.loader.load(key).await?;
        Ok(self.create_decision(content))
    }

    pub fn loader(&self) -> &L {
        self.loader.as_ref()
    }

    pub fn adapter(&self) -> &A {
        self.adapter.as_ref()
    }
}
