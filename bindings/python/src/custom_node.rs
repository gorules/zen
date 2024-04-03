use anyhow::anyhow;
use pyo3::types::PyDict;
use pyo3::{PyObject, Python};
use pythonize::depythonize;

use zen_engine::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::handler::node::{NodeResponse, NodeResult};

use crate::types::PyNodeRequest;

#[derive(Default)]
pub(crate) struct PyCustomNode(Option<PyObject>);

impl From<PyObject> for PyCustomNode {
    fn from(value: PyObject) -> Self {
        Self(Some(value))
    }
}

impl From<Option<PyObject>> for PyCustomNode {
    fn from(value: Option<PyObject>) -> Self {
        Self(value)
    }
}

impl CustomNodeAdapter for PyCustomNode {
    async fn handle(&self, request: CustomNodeRequest<'_>) -> NodeResult {
        let Some(callable) = &self.0 else {
            return Err(anyhow!("Custom node handler not provided"));
        };

        let content: NodeResponse = Python::with_gil(|py| {
            let req = PyNodeRequest::from_request(py, request)?;
            let result = callable.call1(py, (req,))?;

            let dict = result.extract::<&PyDict>(py)?;
            depythonize(dict)
        })?;

        Ok(content)
    }
}
