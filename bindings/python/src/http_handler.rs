use anyhow::{anyhow, Context};
use either::Either;
use pyo3::types::PyDict;
use pyo3::{Bound, IntoPyObjectExt, Py, PyAny, PyObject, Python};
use pyo3_async_runtimes::TaskLocals;
use pythonize::{depythonize, pythonize};
use std::future::Future;
use std::pin::Pin;
use zen_engine::nodes::http_handler::{HttpHandler, HttpHandlerRequest, HttpHandlerResponse};

#[derive(Debug)]
pub(crate) struct PyHttpHandler {
    callback: Py<PyAny>,
    task_locals: Option<TaskLocals>,
}

impl PyHttpHandler {
    pub fn new(callback: Py<PyAny>, task_locals: Option<TaskLocals>) -> Self {
        Self {
            callback,
            task_locals,
        }
    }
}

fn extract_http_response(py: Python<'_>, result: PyObject) -> anyhow::Result<HttpHandlerResponse> {
    let dict = result
        .extract::<Bound<'_, PyDict>>(py)
        .context("Failed to extract response")?;
    let response: HttpHandlerResponse =
        depythonize(&dict).context("Failed to depythonize response")?;
    Ok(response)
}

impl HttpHandler for PyHttpHandler {
    fn handle(
        &self,
        request: HttpHandlerRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpHandlerResponse, String>> + Send + '_>> {
        Box::pin(async move {
            let maybe_result: anyhow::Result<_> = Python::with_gil(|py| {
                let request_obj = pythonize(py, &request).context("Failed to convert request")?;
                let result = self.callback.call1(py, (request_obj,))?;
                let is_coroutine = result.getattr(py, "__await__").is_ok();
                if !is_coroutine {
                    return Ok(Either::Left(extract_http_response(py, result)));
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

            match maybe_result.map_err(|err| err.to_string())? {
                Either::Left(result) => result.map_err(|err| err.to_string()),
                Either::Right(future) => {
                    let result = future.await.map_err(|err| err.to_string())?;
                    Python::with_gil(|py| extract_http_response(py, result))
                        .map_err(|err| err.to_string())
                }
            }
        })
    }
}
