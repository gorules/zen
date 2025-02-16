use std::sync::Arc;

use crate::custom_node::PyCustomNode;
use crate::engine::PyZenEvaluateOptions;
use crate::loader::PyDecisionLoader;
use crate::mt::worker_pool;
use crate::value::PyValue;
use crate::variable::PyVariable;
use anyhow::{anyhow, Context};
use pyo3::{pyclass, pymethods, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pyo3_async_runtimes::tokio;
use pyo3_async_runtimes::tokio::get_current_locals;
use pyo3_async_runtimes::tokio::re_exports::runtime::Runtime;
use serde_json::Value;
use zen_engine::{Decision, EvaluationOptions};

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
        ctx: PyVariable,
        opts: Option<PyZenEvaluateOptions>,
    ) -> PyResult<Py<PyAny>> {
        let options = opts.unwrap_or_default();
        let decision = self.0.clone();

        let rt = Runtime::new()?;
        let result = rt
            .block_on(decision.evaluate_with_opts(
                ctx.into_inner(),
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
        ctx: PyValue,
        opts: Option<PyZenEvaluateOptions>,
    ) -> PyResult<Py<PyAny>> {
        let context: Value = ctx.0;
        let options = opts.unwrap_or_default();

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
