use crate::handler::node::{NodeError, NodeRequest, NodeResult};
use crate::model::{DecisionNode, DecisionNodeKind};
use json_dotpath::DotPaths;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Debug;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use zen_expression::variable::Variable;
use zen_tmpl::TemplateRenderError;

pub trait CustomNodeAdapter: Debug {
    fn handle(
        &self,
        request: CustomNodeRequest,
    ) -> Pin<Box<dyn Future<Output = NodeResult> + Send>>;
}

#[derive(Default, Debug)]
pub struct NoopCustomNode;

impl CustomNodeAdapter for NoopCustomNode {
    fn handle(
        &self,
        request: CustomNodeRequest,
    ) -> Pin<Box<dyn Future<Output = NodeResult> + Send>> {
        Box::pin(async move { Err(NodeError("Custom node handler not provided".to_string())) })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomNodeRequest {
    pub input: Variable,
    pub node: CustomDecisionNode,
}

impl TryFrom<NodeRequest> for CustomNodeRequest {
    type Error = ();

    fn try_from(value: NodeRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            input: value.input.clone(),
            node: value.node.deref().try_into()?,
        })
    }
}

impl CustomNodeRequest {
    pub fn get_field(&self, path: &str) -> Result<Option<Variable>, TemplateRenderError> {
        let Some(selected_value) = self.get_field_raw(path) else {
            return Ok(None);
        };

        let Variable::String(template) = selected_value else {
            return Ok(Some(selected_value));
        };

        let template_value = zen_tmpl::render(template.as_ref(), self.input.clone())?;
        Ok(Some(template_value))
    }

    fn get_field_raw(&self, path: &str) -> Option<Variable> {
        self.node.config.dot_get(path).ok().flatten()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomDecisionNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config: Arc<Value>,
}

impl TryFrom<&DecisionNode> for CustomDecisionNode {
    type Error = ();

    fn try_from(value: &DecisionNode) -> Result<Self, Self::Error> {
        let DecisionNodeKind::CustomNode { content } = &value.kind else {
            return Err(());
        };

        Ok(Self {
            id: value.id.clone(),
            name: value.name.clone(),
            kind: content.kind.clone(),
            config: content.config.clone(),
        })
    }
}
