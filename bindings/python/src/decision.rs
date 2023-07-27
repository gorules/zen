use crate::engine::PyZenEvaluateOptions;
use crate::loader::PyDecisionLoader;
use crate::value::PyValue;
use anyhow::{anyhow, Context};
use pyo3::types::PyDict;
use pyo3::{pyclass, pymethods, PyObject, PyResult, Python, ToPyObject};
use pythonize::depythonize;
use std::sync::Arc;
use zen_engine::{Decision, EvaluationOptions};

#[pyclass]
#[pyo3(name = "ZenDecision")]
pub struct PyZenDecision(pub(crate) Arc<Decision<PyDecisionLoader>>);

impl From<Decision<PyDecisionLoader>> for PyZenDecision {
    fn from(value: Decision<PyDecisionLoader>) -> Self {
        Self(value.into())
    }
}

#[pymethods]
impl PyZenDecision {
    pub fn evaluate(&self, py: Python, ctx: &PyDict, opts: Option<&PyDict>) -> PyResult<PyObject> {
        let context = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let decision = self.0.clone();
        let result = futures::executor::block_on(decision.evaluate_with_opts(
            &context,
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
}
