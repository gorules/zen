use std::future::Future;
use std::sync::Arc;

use crate::content::PyZenDecisionContentJson;
use anyhow::anyhow;
use either::Either;
use pyo3::{IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pyo3_async_runtimes::TaskLocals;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;

#[derive(Default)]
pub(crate) struct PyDecisionLoader {
    callback: Option<Py<PyAny>>,
    task_locals: Option<TaskLocals>,
}

impl PyDecisionLoader {
    pub fn new(callback: Option<Py<PyAny>>, task_locals: Option<TaskLocals>) -> Self {
        Self {
            callback,
            task_locals,
        }
    }
}

impl PyDecisionLoader {
    async fn load_element(&self, key: &str) -> Result<Arc<DecisionContent>, anyhow::Error> {
        let Some(callable) = &self.callback else {
            return Err(anyhow!("Loader is not defined"));
        };

        let maybe_result: PyResult<_> = Python::with_gil(|py| {
            let result = callable.call1(py, (key,))?;
            let is_coroutine = result.getattr(py, "__await__").is_ok();
            if !is_coroutine {
                return Ok(Either::Left(
                    result.extract::<PyZenDecisionContentJson>(py)?,
                ));
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
            Either::Left(result) => Ok(result.0 .0),
            Either::Right(future) => {
                let result = future.await?;
                let content =
                    Python::with_gil(|py| result.extract::<PyZenDecisionContentJson>(py))?;
                Ok(content.0 .0)
            }
        }
    }
}

impl DecisionLoader for PyDecisionLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            self.load_element(key).await.map_err(|e| {
                LoaderError::Internal {
                    source: e,
                    key: key.to_string(),
                }
                .into()
            })
        }
    }
}
