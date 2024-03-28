use anyhow::{anyhow, Context};
use json_dotpath::DotPaths;
use pyo3::{pyclass, pymethods, PyObject, PyResult, Python, ToPyObject};
use pythonize::pythonize;
use serde::Serialize;
use serde_json::Value;

use crate::value::PyValue;
use zen_engine::handler::node::{NodeRequest, NodeResponse};
use zen_engine::model::{CustomNodeContent, DecisionNode, DecisionNodeKind};

#[derive(Serialize)]
struct CustomDecisionNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub content: CustomNodeContent,
}

impl TryFrom<DecisionNode> for CustomDecisionNode {
    type Error = ();

    fn try_from(value: DecisionNode) -> Result<Self, Self::Error> {
        let DecisionNodeKind::CustomNode { content } = value.kind else {
            return Err(());
        };

        return Ok(Self {
            id: value.id,
            name: value.name,
            kind: String::from("customNode"),
            content,
        });
    }
}

#[pyclass]
pub struct PyNodeRequest {
    #[pyo3(get)]
    pub iteration: u8,
    #[pyo3(get)]
    pub input: PyObject,
    #[pyo3(get)]
    pub node: PyObject,

    inner_node: CustomDecisionNode,
    inner_input: Value,
}

impl PyNodeRequest {
    pub fn from_request(py: Python, value: &NodeRequest<'_>) -> pythonize::Result<PyNodeRequest> {
        Ok(Self {
            iteration: value.iteration,
            input: pythonize(py, &value.input)?,
            node: pythonize(py, &value.node)?,

            inner_input: value.input.clone(),
            inner_node: value.node.clone().try_into().unwrap(),
        })
    }
}

#[pymethods]
impl PyNodeRequest {
    fn get_field(&self, py: Python, path: String) -> PyResult<PyObject> {
        let node_config = &self.inner_node.content.config;

        let selected_value: Value = node_config
            .dot_get(path.as_str())
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;
        let Value::String(template) = selected_value else {
            return Ok(PyValue(selected_value).to_object(py));
        };

        let template_value = zen_template::render(template.as_str(), &self.inner_input)
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        Ok(PyValue(template_value).to_object(py))
    }

    fn get_field_raw(&self, py: Python, path: String) -> PyResult<PyObject> {
        let node_config = &self.inner_node.content.config;

        let selected_value: Value = node_config
            .dot_get(path.as_str())
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;

        Ok(PyValue(selected_value).to_object(py))
    }
}

#[pyclass]
#[derive(Clone)]
pub struct PyNodeResponse {
    pub output: Value,
    pub trace_data: Option<Value>,
}

impl From<NodeResponse> for PyNodeResponse {
    fn from(value: NodeResponse) -> Self {
        Self {
            output: value.output,
            trace_data: value.trace_data,
        }
    }
}

impl From<PyNodeResponse> for NodeResponse {
    fn from(value: PyNodeResponse) -> Self {
        Self {
            output: value.output,
            trace_data: value.trace_data,
        }
    }
}
