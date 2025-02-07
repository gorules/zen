use std::sync::Arc;

use anyhow::{anyhow, Context};
use pyo3::prelude::PyDictMethods;
use pyo3::types::PyDict;
use pyo3::{pyclass, pymethods, Bound, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pyo3_async_runtimes::tokio;
use pythonize::depythonize;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationOptions};

use crate::custom_node::PyCustomNode;
use crate::decision::PyZenDecision;
use crate::loader::PyDecisionLoader;
use crate::value::PyValue;

#[pyclass]
#[pyo3(name = "ZenEngine")]
pub struct PyZenEngine {
    graph: Arc<DecisionEngine<PyDecisionLoader, PyCustomNode>>,
}

#[derive(Serialize, Deserialize)]
pub struct PyZenEvaluateOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
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
            graph: DecisionEngine::new(
                Arc::new(PyDecisionLoader::default()),
                Arc::new(PyCustomNode::default()),
            )
            .into(),
        }
    }
}

#[pymethods]
impl PyZenEngine {
    #[new]
    #[pyo3(signature = (maybe_options=None))]
    pub fn new(maybe_options: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let Some(options) = maybe_options else {
            return Ok(Default::default());
        };

        let loader = match options.get_item("loader")? {
            Some(loader) => Some(Python::with_gil(|py| loader.into_py_any(py))?),
            None => None,
        };

        let custom_node = match options.get_item("customHandler")? {
            Some(custom_node) => Some(Python::with_gil(|py| custom_node.into_py_any(py))?),
            None => None,
        };

        Ok(Self {
            graph: DecisionEngine::new(
                Arc::new(PyDecisionLoader::from(loader)),
                Arc::new(PyCustomNode::from(custom_node)),
            )
            .into(),
        })
    }

    #[pyo3(signature = (key, ctx, opts=None))]
    pub fn evaluate(
        &self,
        py: Python,
        key: String,
        ctx: &Bound<'_, PyDict>,
        opts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let context: Value = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let graph = self.graph.clone();
        let result = futures::executor::block_on(graph.evaluate_with_opts(
            key,
            context.into(),
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
        ctx: &Bound<'_, PyDict>,
        opts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let context: Value = depythonize(ctx).context("Failed to convert dict")?;
        let options: PyZenEvaluateOptions = if let Some(op) = opts {
            depythonize(op).context("Failed to convert dict")?
        } else {
            Default::default()
        };

        let graph = self.graph.clone();
        let result = tokio::future_into_py(py, async move {
            let result = futures::executor::block_on(graph.evaluate_with_opts(
                key,
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

            Python::with_gil(|py| PyValue(value).into_py_any(py))
        })?;

        Ok(result.unbind())
    }

    pub fn create_decision(&self, content: String) -> PyResult<PyZenDecision> {
        let decision_content: DecisionContent =
            serde_json::from_str(&content).context("Failed to serialize decision content")?;

        let decision = self.graph.create_decision(decision_content.into());
        Ok(PyZenDecision::from(decision))
    }

    pub fn get_decision<'py>(&'py self, _py: Python<'py>, key: String) -> PyResult<PyZenDecision> {
        let decision = futures::executor::block_on(self.graph.get_decision(&key))
            .context("Failed to find decision with given key")?;

        Ok(PyZenDecision::from(decision))
    }
}
