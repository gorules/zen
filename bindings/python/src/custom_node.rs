use anyhow::anyhow;
use pyo3::types::PyDict;
use pyo3::{PyObject, PyResult, Python};
use pyo3_asyncio::tokio::into_future;
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

fn extract_custom_node_response(result: PyObject, py: Python<'_>) -> NodeResult {
    let dict = result.extract::<&PyDict>(py)?;
    let response: NodeResponse = depythonize(dict)?;
    Ok(response)
}

impl CustomNodeAdapter for PyCustomNode {
    async fn handle(&self, request: CustomNodeRequest<'_>) -> NodeResult {
        let Some(callable) = &self.0 else {
            return Err(anyhow!("Custom node handler not provided"));
        };

        let (future, result) = Python::with_gil(|py| -> PyResult<_> {
            let req = PyNodeRequest::from_request(py, request)?;
            let result = callable.call1(py, (req,))?;
            let is_coroutine = result.getattr(py, "__await__").is_ok();
            if is_coroutine {
                return Ok((Some(into_future(result.as_ref(py))), None));
            }
            Ok((None, Some(extract_custom_node_response(result, py))))
        })?;

        if let Some(result) = result {
            return result;
        }

        let result = future
            .ok_or_else(|| anyhow!("Future or result must be present"))??
            .await?;

        let content = Python::with_gil(|py| -> PyResult<_> {
            Ok(extract_custom_node_response(result, py))
        })??;

        Ok(content)
    }
}
