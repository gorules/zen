use crate::handler::node::{NodeRequest, NodeResult};
use crate::model::{DecisionNode, DecisionNodeKind};
use anyhow::anyhow;
use json_dotpath::DotPaths;
use serde::Serialize;
use serde_json::Value;
use zen_expression::variable::Variable;
use zen_tmpl::TemplateRenderError;

pub trait CustomNodeAdapter {
    fn handle(
        &self,
        request: CustomNodeRequest<'_>,
    ) -> impl std::future::Future<Output = NodeResult>;
}

#[derive(Default, Debug)]
pub struct NoopCustomNode;

impl CustomNodeAdapter for NoopCustomNode {
    async fn handle(&self, _: CustomNodeRequest<'_>) -> NodeResult {
        Err(anyhow!("Custom node handler not provided"))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomNodeRequest<'a> {
    pub input: Variable,
    pub node: CustomDecisionNode<'a>,
}

impl<'a> TryFrom<&'a NodeRequest<'a>> for CustomNodeRequest<'a> {
    type Error = ();

    fn try_from(value: &'a NodeRequest<'a>) -> Result<Self, Self::Error> {
        Ok(Self {
            input: value.input.clone(),
            node: value.node.try_into()?,
        })
    }
}

impl<'a> CustomNodeRequest<'a> {
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
pub struct CustomDecisionNode<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub config: &'a Value,
}

impl<'a> TryFrom<&'a DecisionNode> for CustomDecisionNode<'a> {
    type Error = ();

    fn try_from(value: &'a DecisionNode) -> Result<Self, Self::Error> {
        let DecisionNodeKind::CustomNode { content } = &value.kind else {
            return Err(());
        };

        Ok(Self {
            id: &value.id,
            name: &value.name,
            kind: &content.kind,
            config: &content.config,
        })
    }
}
