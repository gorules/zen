use std::sync::Arc;

use crate::content::PyZenDecisionContentJson;
use crate::custom_node::PyCustomNode;
use crate::decision::PyZenDecision;
use crate::loader::PyDecisionLoader;
use crate::mt::{block_on, worker_pool};
use crate::value::PyValue;
use crate::variable::PyVariable;
use anyhow::{anyhow, Context};
use pyo3::prelude::{PyAnyMethods, PyDictMethods};
use pyo3::types::PyDict;
use pyo3::{pyclass, pymethods, Bound, FromPyObject, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pyo3_async_runtimes::tokio::get_current_locals;
use pyo3_async_runtimes::{tokio, TaskLocals};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zen_engine::{DecisionEngine, EvaluationOptions};

#[pyclass]
#[pyo3(name = "ZenEngine")]
pub struct PyZenEngine {
    engine: Arc<DecisionEngine<PyDecisionLoader, PyCustomNode>>,
}

#[derive(Serialize, Deserialize)]
pub struct PyZenEvaluateOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
}

impl<'py> FromPyObject<'py> for PyZenEvaluateOptions {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        let dict = ob.downcast::<PyDict>()?;

        let trace = dict
            .get_item("trace")?
            .map(|v| v.extract::<bool>())
            .transpose()?;

        let max_depth = dict
            .get_item("max_depth")?
            .map(|v| v.extract::<u8>())
            .transpose()?;

        Ok(PyZenEvaluateOptions { trace, max_depth })
    }
}

impl Default for PyZenEvaluateOptions {
    fn default() -> Self {
        Self {
            trace: None,
            max_depth: None,
        }
    }
}

impl Default for PyZenEngine {
    fn default() -> Self {
        Self {
            engine: Arc::new(DecisionEngine::new(
                Arc::new(PyDecisionLoader::default()),
                Arc::new(PyCustomNode::default()),
            )),
        }
    }
}

#[pymethods]
impl PyZenEngine {
    #[new]
    #[pyo3(signature = (maybe_options=None))]
    pub fn new(py: Python, maybe_options: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let Some(options) = maybe_options else {
            return Ok(Default::default());
        };

        let loader = match options.get_item("loader")? {
            Some(loader) => Some(loader.into_py_any(py)?),
            None => None,
        };

        let custom_node = match options.get_item("customHandler")? {
            Some(custom_node) => Some(custom_node.into_py_any(py)?),
            None => None,
        };

        let make_locals = || {
            TaskLocals::with_running_loop(py)
                .ok()
                .map(|s| s.copy_context(py).ok())
                .flatten()
        };

        Ok(Self {
            engine: Arc::new(DecisionEngine::new(
                Arc::new(PyDecisionLoader::new(loader, make_locals())),
                Arc::new(PyCustomNode::new(custom_node, make_locals())),
            )),
        })
    }

    #[pyo3(signature = (key, ctx, opts=None))]
    pub fn evaluate(
        &self,
        py: Python,
        key: &str,
        ctx: PyVariable,
        opts: Option<PyZenEvaluateOptions>,
    ) -> PyResult<Py<PyAny>> {
        let options = opts.unwrap_or_default();
        let result = block_on(self.engine.evaluate_with_opts(
            key,
            ctx.into_inner(),
            EvaluationOptions {
                max_depth: options.max_depth,
                trace: options.trace,
            },
        ))
        .map_err(|e| {
            anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
        })?;

        let value = serde_json::to_value(&result).context("Failed to serialize result")?;
        PyValue(value).into_py_any(py)
    }

    #[pyo3(signature = (key, ctx, opts=None))]
    pub fn async_evaluate<'py>(
        &'py self,
        py: Python<'py>,
        key: String,
        ctx: PyValue,
        opts: Option<PyZenEvaluateOptions>,
    ) -> PyResult<Py<PyAny>> {
        let context: Value = ctx.0;
        let options: PyZenEvaluateOptions = opts.unwrap_or_default();

        let engine = self.engine.clone();
        let result = tokio::future_into_py_with_locals(py, get_current_locals(py)?, async move {
            let value = worker_pool()
                .spawn_pinned(move || async move {
                    engine
                        .evaluate_with_opts(
                            key,
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

    pub fn create_decision(&self, content: PyZenDecisionContentJson) -> PyResult<PyZenDecision> {
        let decision = self.engine.create_decision(content.0 .0);
        Ok(PyZenDecision::from(decision))
    }

    pub fn get_decision<'py>(&'py self, _py: Python<'py>, key: &str) -> PyResult<PyZenDecision> {
        let decision = block_on(self.engine.get_decision(key))
            .context("Failed to find decision with given key")?;

        Ok(PyZenDecision::from(decision))
    }
}
