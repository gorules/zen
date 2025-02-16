use anyhow::{anyhow, Context};
use json_dotpath::DotPaths;
use pyo3::{pyclass, pymethods, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use crate::value::{value_to_object, PyValue};
use crate::variable::PyVariable;
use zen_engine::handler::custom_node_adapter::{
    CustomDecisionNode as BaseCustomDecisionNode, CustomNodeRequest,
};
use zen_engine::handler::node::NodeResponse;
use zen_expression::Variable;

#[derive(Serialize)]
struct CustomDecisionNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config: Arc<Value>,
}

impl From<BaseCustomDecisionNode> for CustomDecisionNode {
    fn from(value: BaseCustomDecisionNode) -> Self {
        Self {
            id: value.id,
            name: value.name,
            kind: value.kind,
            config: value.config.clone(),
        }
    }
}

#[pyclass]
pub struct PyNodeRequest {
    inner_node: CustomDecisionNode,
    inner_input: Value,

    #[pyo3(get)]
    pub input: Py<PyAny>,
    #[pyo3(get)]
    pub node: Py<PyAny>,
}

impl PyNodeRequest {
    pub fn from_request(py: Python, value: CustomNodeRequest) -> pythonize::Result<PyNodeRequest> {
        let inner_node = value.node.into();
        let node_val = serde_json::to_value(&inner_node).unwrap();

        Ok(Self {
            input: value_to_object(py, &value.input.to_value())?.unbind(),
            node: value_to_object(py, &node_val)?.unbind(),

            inner_input: value.input.to_value(),
            inner_node,
        })
    }
}

#[pymethods]
impl PyNodeRequest {
    fn get_field(&self, py: Python, path: &str) -> PyResult<Py<PyAny>> {
        let node_config = &self.inner_node.config;

        let selected_value: Value = node_config
            .dot_get(path)
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;
        let Value::String(template) = selected_value else {
            return PyValue(selected_value).into_py_any(py);
        };

        let template_value = zen_tmpl::render(template.as_str(), Variable::from(&self.inner_input))
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        PyVariable(template_value).into_py_any(py)
    }

    fn get_field_raw(&self, py: Python, path: &str) -> PyResult<Py<PyAny>> {
        let node_config = &self.inner_node.config;

        let selected_value: Value = node_config
            .dot_get(path)
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;

        PyValue(selected_value).into_py_any(py)
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
            output: value.output.to_value(),
            trace_data: value.trace_data,
        }
    }
}

impl From<PyNodeResponse> for NodeResponse {
    fn from(value: PyNodeResponse) -> Self {
        Self {
            output: value.output.into(),
            trace_data: value.trace_data,
        }
    }
}
