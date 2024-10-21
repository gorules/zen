use crate::handler::node::{NodeRequest, NodeResult};
use crate::model::{DecisionNode, DecisionNodeKind};
use anyhow::anyhow;
use json_dotpath::DotPaths;
use serde::Serialize;
use serde_json::Value;
use std::ops::Deref;
use std::sync::Arc;
use zen_expression::variable::Variable;
use zen_tmpl::TemplateRenderError;

pub trait CustomNodeAdapter {
    fn handle(&self, request: CustomNodeRequest) -> impl std::future::Future<Output = NodeResult>;
}

#[derive(Default, Debug)]
pub struct NoopCustomNode;

impl CustomNodeAdapter for NoopCustomNode {
    async fn handle(&self, _: CustomNodeRequest) -> NodeResult {
        Err(anyhow!("Custom node handler not provided"))
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
