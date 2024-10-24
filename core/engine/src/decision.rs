use crate::engine::EvaluationOptions;
use crate::handler::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::loader::{CachedLoader, DecisionLoader, NoopLoader};
use crate::model::DecisionContent;
use crate::{DecisionGraphValidationError, EvaluationError};
use jsonschema::Validator;
use serde_json::Value;
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::RwLock;
use zen_expression::variable::Variable;

type SharedValidator = Arc<RwLock<Option<Arc<Validator>>>>;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision<Loader, CustomNode>
where
    Loader: DecisionLoader + 'static,
    CustomNode: CustomNodeAdapter + 'static,
{
    content: Arc<DecisionContent>,
    loader: Arc<Loader>,
    adapter: Arc<CustomNode>,

    input_validator: SharedValidator,
    output_validator: SharedValidator,
}

impl From<DecisionContent> for Decision<NoopLoader, NoopCustomNode> {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: NoopLoader::default().into(),
            adapter: NoopCustomNode::default().into(),

            input_validator: Arc::new(RwLock::new(None)),
            output_validator: Arc::new(RwLock::new(None)),
        }
    }
}

impl From<Arc<DecisionContent>> for Decision<NoopLoader, NoopCustomNode> {
    fn from(value: Arc<DecisionContent>) -> Self {
        Self {
            content: value,
            loader: NoopLoader::default().into(),
            adapter: NoopCustomNode::default().into(),

            input_validator: Arc::new(RwLock::new(None)),
            output_validator: Arc::new(RwLock::new(None)),
        }
    }
}

impl<L, A> Decision<L, A>
where
    L: DecisionLoader + 'static,
    A: CustomNodeAdapter + 'static,
{
    pub fn with_loader<Loader>(self, loader: Arc<Loader>) -> Decision<Loader, A>
    where
        Loader: DecisionLoader,
    {
        Decision {
            loader,
            adapter: self.adapter,
            content: self.content,

            input_validator: self.input_validator,
            output_validator: self.output_validator,
        }
    }

    pub fn with_adapter<Adapter>(self, adapter: Arc<Adapter>) -> Decision<L, Adapter>
    where
        Adapter: CustomNodeAdapter,
    {
        Decision {
            loader: self.loader,
            adapter,
            content: self.content,

            input_validator: self.input_validator,
            output_validator: self.output_validator,
        }
    }

    /// Evaluates a decision using an in-memory reference stored in struct
    pub async fn evaluate(
        &self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        self.evaluate_with_opts(context, Default::default()).await
    }

    /// Evaluates a decision using in-memory reference with advanced options
    pub async fn evaluate_with_opts(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        if let Some(input_schema) = &self.content.settings.validation.input_schema {
            let input_validator =
                get_validator(self.input_validator.clone(), &input_schema).await?;

            let context_json = context.to_value();
            input_validator.validate(&context_json)?;
        }

        let mut decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: options.max_depth.unwrap_or(5),
            trace: options.trace.unwrap_or_default(),
            loader: Arc::new(CachedLoader::from(self.loader.clone())),
            adapter: self.adapter.clone(),
            iteration: 0,
        })?;

        let response = decision_graph.evaluate(context).await?;
        if let Some(output_schema) = &self.content.settings.validation.output_schema {
            let output_validator =
                get_validator(self.output_validator.clone(), &output_schema).await?;

            let output_json = response.result.to_value();
            output_validator.validate(&output_json)?;
        }

        Ok(response)
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: 1,
            trace: false,
            loader: Arc::new(CachedLoader::from(self.loader.clone())),
            adapter: self.adapter.clone(),
            iteration: 0,
        })?;

        decision_graph.validate()
    }
}

async fn get_validator(
    shared: SharedValidator,
    schema: &Value,
) -> Result<Arc<Validator>, Box<EvaluationError>> {
    if let Some(validator) = shared.read().await.deref() {
        return Ok(validator.clone());
    }

    let mut w_shared = shared.write().await;
    let validator = Arc::new(jsonschema::draft7::new(&schema)?);
    w_shared.replace(validator.clone());

    Ok(validator)
}
