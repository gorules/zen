use anyhow::{anyhow, Context};
use json_dotpath::DotPaths;
use pyo3::{pyclass, pymethods, PyObject, PyResult, Python, ToPyObject};
use serde::Serialize;
use serde_json::Value;

use zen_engine::handler::node::{NodeRequest, NodeResponse};
use zen_engine::model::{DecisionNode, DecisionNodeKind};

use crate::value::{value_to_object, PyValue};

#[derive(Serialize)]
struct CustomDecisionNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config: Value,
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
            kind: content.kind,
            config: content.config,
        });
    }
}

#[pyclass]
pub struct PyNodeRequest {
    inner_node: CustomDecisionNode,
    inner_input: Value,

    #[pyo3(get)]
    pub input: PyObject,
    #[pyo3(get)]
    pub node: PyObject,
}

impl PyNodeRequest {
    pub fn from_request(py: Python, value: &NodeRequest<'_>) -> pythonize::Result<PyNodeRequest> {
        let inner_node = value.node.clone().try_into().unwrap();
        let node_val = serde_json::to_value(&inner_node).unwrap();

        Ok(Self {
            input: value_to_object(py, &value.input),
            node: value_to_object(py, &node_val),

            inner_input: value.input.clone(),
            inner_node,
        })
    }
}

#[pymethods]
impl PyNodeRequest {
    fn get_field(&self, py: Python, path: String) -> PyResult<PyObject> {
        let node_config = &self.inner_node.config;

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
        let node_config = &self.inner_node.config;

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
