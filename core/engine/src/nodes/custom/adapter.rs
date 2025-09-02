use crate::nodes::result::{NodeError, NodeResult};
use json_dotpath::DotPaths;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use zen_expression::variable::Variable;
use zen_tmpl::TemplateRenderError;

pub trait CustomNodeAdapter: Debug + Send {
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
        Box::pin(async move {
            Err(NodeError {
                trace: None,
                node_id: request.node.id.clone(),
                source: "Custom node handler not provided".to_string().into(),
            })
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomNodeRequest {
    pub input: Variable,
    pub node: CustomDecisionNode,
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
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub kind: Arc<str>,
    pub config: Arc<Value>,
}

pub type DynamicCustomNode = Arc<dyn CustomNodeAdapter + Send + Sync>;
