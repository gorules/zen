use anyhow::anyhow;
use either::Either;
use pyo3::types::PyDict;
use pyo3::{PyObject, PyResult, Python};
use pyo3_asyncio::tokio;
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

fn extract_custom_node_response(py: Python<'_>, result: PyObject) -> NodeResult {
    let dict = result.extract::<&PyDict>(py)?;
    let response: NodeResponse = depythonize(dict)?;
    Ok(response)
}

impl CustomNodeAdapter for PyCustomNode {
    async fn handle(&self, request: CustomNodeRequest<'_>) -> NodeResult {
        let Some(callable) = &self.0 else {
            return Err(anyhow!("Custom node handler not provided"));
        };

        let maybe_result: PyResult<_> = Python::with_gil(|py| {
            let req = PyNodeRequest::from_request(py, request)?;
            let result = callable.call1(py, (req,))?;
            let is_coroutine = result.getattr(py, "__await__").is_ok();
            if !is_coroutine {
                return Ok(Either::Left(extract_custom_node_response(py, result)));
            }

            let result_future = tokio::into_future(result.as_ref(py))?;
            return Ok(Either::Right(result_future));
        });

        match maybe_result? {
            Either::Left(result) => result,
            Either::Right(future) => {
                let result = future.await?;
                Python::with_gil(|py| extract_custom_node_response(py, result))
            }
        }
    }
}
