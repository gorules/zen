use std::sync::Arc;

use anyhow::{anyhow, Context};
use pyo3::types::PyDict;
use pyo3::{pyclass, pymethods, PyAny, PyObject, PyResult, Python, ToPyObject};
use pyo3_asyncio::tokio;
use pythonize::depythonize;
use serde_json::Value;
use zen_engine::{Decision, EvaluationOptions};

use crate::custom_node::PyCustomNode;
use crate::engine::PyZenEvaluateOptions;
use crate::loader::PyDecisionLoader;
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
    pub fn evaluate(&self, py: Python, ctx: &PyDict, opts: Option<&PyDict>) -> PyResult<PyObject> {
        let context: Value = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let decision = self.0.clone();
        let result = futures::executor::block_on(decision.evaluate_with_opts(
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
        Ok(PyValue(value).to_object(py))
    }

    pub fn async_evaluate<'py>(
        &'py self,
        py: Python<'py>,
        ctx: &PyDict,
        opts: Option<&PyDict>,
    ) -> PyResult<&PyAny> {
        let context: Value = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let decision = self.0.clone();
        tokio::future_into_py(py, async move {
            let result = futures::executor::block_on(decision.evaluate_with_opts(
                context.into(),
                EvaluationOptions {
                    max_depth: options.max_depth,
                    trace: options.trace,
                },
            ))
            .map_err(|e| {
                anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
            })?;

            let value = serde_json::to_value(result).context("Failed to serialize result")?;

            Python::with_gil(|py| Ok(PyValue(value).to_object(py)))
        })
    }

    pub fn validate(&self) -> PyResult<()> {
        let decision = self.0.clone();
        decision
            .validate()
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        Ok(())
    }
}
