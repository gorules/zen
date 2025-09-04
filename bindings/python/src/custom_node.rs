use crate::types::PyNodeRequest;
use anyhow::{anyhow, Context};
use either::Either;
use pyo3::types::PyDict;
use pyo3::{Bound, IntoPyObjectExt, Py, PyAny, PyObject, PyResult, Python};
use pyo3_async_runtimes::TaskLocals;
use pythonize::depythonize;
use std::future::Future;
use std::pin::Pin;
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::{NodeError, NodeResponse, NodeResult};

#[derive(Default, Debug)]
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

fn extract_custom_node_response(py: Python<'_>, result: PyObject) -> anyhow::Result<NodeResponse> {
    let dict = result
        .extract::<Bound<'_, PyDict>>(py)
        .context("Failed to extract response")?;
    let response: NodeResponse = depythonize(&dict).context("Failed to depythonize response")?;
    Ok(response)
}

impl CustomNodeAdapter for PyCustomNode {
    fn handle(&self, request: CustomNodeRequest) -> Pin<Box<dyn Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            let node_id = request.node.id.clone();
            let Some(callable) = &self.callback else {
                return Err(NodeError {
                    node_id,
                    trace: None,
                    source: "Custom node handler not provided".into(),
                });
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

            match maybe_result.map_err(|_| NodeError {
                node_id: node_id.clone(),
                trace: None,
                source: "Failed to run custom node handler".into(),
            })? {
                Either::Left(result) => result.map_err(|err| NodeError {
                    node_id: node_id.clone(),
                    source: err.into(),
                    trace: None,
                }),
                Either::Right(future) => {
                    let result = future.await.map_err(|err| NodeError {
                        node_id: node_id.clone(),
                        trace: None,
                        source: err.into(),
                    })?;
                    Python::with_gil(|py| {
                        extract_custom_node_response(py, result).map_err(|err| NodeError {
                            node_id,
                            source: err.into(),
                            trace: None,
                        })
                    })
                }
            }
        })
    }
}
