use std::sync::Arc;

use anyhow::{anyhow, Context};
use pyo3::types::PyDict;
use pyo3::{pyclass, pymethods, Bound, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pyo3_async_runtimes::tokio;
use pyo3_async_runtimes::tokio::get_current_locals;
use pyo3_async_runtimes::tokio::re_exports::runtime::Runtime;
use pythonize::depythonize;
use serde_json::Value;
use zen_engine::{Decision, EvaluationOptions};

use crate::custom_node::PyCustomNode;
use crate::engine::PyZenEvaluateOptions;
use crate::loader::PyDecisionLoader;
use crate::mt::worker_pool;
use crate::value::PyValue;

#[pyclass]
#[pyo3(name = "ZenDecision")]
pub struct PyZenDecision(pub(crate) Arc<Decision<PyDecisionLoader, PyCustomNode>>);

impl From<Decision<PyDecisionLoader, PyCustomNode>> for PyZenDecision {
    fn from(value: Decision<PyDecisionLoader, PyCustomNode>) -> Self {
        Self(value.into())
    }
}

#[pymethods]
impl PyZenDecision {
    #[pyo3(signature = (ctx, opts=None))]
    pub fn evaluate(
        &self,
        py: Python,
        ctx: &Bound<'_, PyDict>,
        opts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let context: Value = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let decision = self.0.clone();

        let rt = Runtime::new()?;
        let result = rt
            .block_on(decision.evaluate_with_opts(
                context.into(),
                EvaluationOptions {
                    max_depth: options.max_depth,
                    trace: options.trace,
                },
            ))
            .map_err(|e| {
                anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
            })?;

        let value = serde_json::to_value(&result).context("Fail")?;
        PyValue(value).into_py_any(py)
    }

    #[pyo3(signature = (ctx, opts=None))]
    pub fn async_evaluate<'py>(
        &'py self,
        py: Python<'py>,
        ctx: &Bound<'_, PyDict>,
        opts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let context: Value = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let decision = self.0.clone();
        let result = tokio::future_into_py_with_locals(py, get_current_locals(py)?, async move {
            let value = worker_pool()
                .spawn_pinned(move || async move {
                    decision
                        .evaluate_with_opts(
                            context.into(),
                            EvaluationOptions {
                                max_depth: options.max_depth,
                                trace: options.trace,
                            },
                        )
                        .await
                        .map(serde_json::to_value)
                })
                .await
                .context("Failed to join threads")?
                .map_err(|e| {
                    anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
                })?
                .context("Failed to serialize result")?;

            Python::with_gil(|py| PyValue(value).into_py_any(py))
        })?;

        Ok(result.unbind())
    }

    pub fn validate(&self) -> PyResult<()> {
        let decision = self.0.clone();
        decision
            .validate()
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        Ok(())
    }
}
