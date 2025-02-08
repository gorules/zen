use anyhow::anyhow;
use either::Either;
use pyo3::types::PyDict;
use pyo3::{Bound, IntoPyObjectExt, Py, PyAny, PyObject, PyResult, Python};
use pyo3_async_runtimes::TaskLocals;
use pythonize::depythonize;

use zen_engine::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::handler::node::{NodeResponse, NodeResult};

use crate::types::PyNodeRequest;

#[derive(Default)]
pub(crate) struct PyCustomNode {
    callback: Option<Py<PyAny>>,
    task_locals: Option<TaskLocals>,
}

impl PyCustomNode {
    pub fn new(callback: Option<Py<PyAny>>, task_locals: Option<TaskLocals>) -> Self {
        Self {
            callback,
            task_locals,
        }
    }
}

fn extract_custom_node_response(py: Python<'_>, result: PyObject) -> NodeResult {
    let dict = result.extract::<Bound<'_, PyDict>>(py)?;
    let response: NodeResponse = depythonize(&dict)?;
    Ok(response)
}

impl CustomNodeAdapter for PyCustomNode {
    async fn handle(&self, request: CustomNodeRequest) -> NodeResult {
        let Some(callable) = &self.callback else {
            return Err(anyhow!("Custom node handler not provided"));
        };

        let maybe_result: PyResult<_> = Python::with_gil(|py| {
            let req = PyNodeRequest::from_request(py, request)?;
            let result = callable.call1(py, (req,))?;
            let is_coroutine = result.getattr(py, "__await__").is_ok();
            if !is_coroutine {
                return Ok(Either::Left(extract_custom_node_response(py, result)));
            }

            let Some(task_locals) = &self.task_locals else {
                Err(anyhow!("Task locals are required in async context"))?
            };

            let result_future = pyo3_async_runtimes::into_future_with_locals(
                task_locals,
                result.into_bound_py_any(py)?,
            )?;

            Ok(Either::Right(result_future))
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
