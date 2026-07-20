use std::sync::Arc;

use crate::content::PyZenDecisionContentJson;
use crate::custom_node::PyCustomNode;
use crate::decision::PyZenDecision;
use crate::loader::PyDecisionLoader;
use crate::mt::{block_on, worker_pool};
use crate::value::PyValue;
use crate::variable::PyVariable;
use anyhow::{anyhow, Context};
use pyo3::prelude::{PyAnyMethods, PyDictMethods, PyListMethods};
use pyo3::types::{PyDict, PyList};
use pyo3::{pyclass, pymethods, Bound, FromPyObject, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pyo3_async_runtimes::tokio::get_current_locals;
use pyo3_async_runtimes::{tokio, TaskLocals};
use pythonize::{depythonize, pythonize};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zen_engine::loader::{DynamicLoader, LoaderConfig};
use zen_engine::{DecisionEngine, EvaluationOptions};

#[pyclass]
#[pyo3(name = "ZenEngine")]
pub struct PyZenEngine {
    engine: Arc<DecisionEngine>,
}

#[derive(Serialize, Deserialize)]
pub struct PyZenEvaluateOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
}

impl From<PyZenEvaluateOptions> for EvaluationOptions {
    fn from(value: PyZenEvaluateOptions) -> Self {
        Self {
            max_depth: value.max_depth.unwrap_or(5),
            trace: value.trace.unwrap_or_default(),
        }
    }
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

pub struct PyBatchRequest {
    key: String,
    context: Value,
}

impl<'py> FromPyObject<'py> for PyBatchRequest {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        let dict = ob.downcast::<PyDict>()?;

        let key: String = dict
            .get_item("key")?
            .ok_or_else(|| anyhow!("batch request requires a 'key'"))?
            .extract()?;

        let context = dict
            .get_item("context")?
            .ok_or_else(|| anyhow!("batch request requires a 'context'"))?
            .extract::<PyValue>()?;

        Ok(Self {
            key,
            context: context.0,
        })
    }
}

impl PyZenEngine {
    fn config_loader(config: &Bound<'_, PyDict>) -> PyResult<DynamicLoader> {
        let loader_type: Option<String> =
            config.get_item("type")?.map(|v| v.extract()).transpose()?;

        let loader_config = match loader_type.as_deref() {
            Some("zip") => {
                let bytes = config
                    .get_item("bytes")?
                    .ok_or_else(|| anyhow!("zip loader requires a 'bytes' value"))?;
                LoaderConfig::Zip {
                    bytes: bytes.extract()?,
                }
            }
            _ => depythonize(config.as_any())?,
        };

        Ok(loader_config.into_loader()?)
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

        let loader: DynamicLoader = match options.get_item("loader")? {
            Some(loader) => match loader.downcast::<PyDict>() {
                Ok(config) => Self::config_loader(config)?,
                Err(_) => Arc::new(PyDecisionLoader::new(
                    Some(loader.into_py_any(py)?),
                    make_locals(),
                )),
            },
            None => Arc::new(PyDecisionLoader::default()),
        };

        let engine = DecisionEngine::new(
            loader,
            Arc::new(PyCustomNode::new(custom_node, make_locals())),
        );
        engine.compile();

        Ok(Self {
            engine: Arc::new(engine),
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
            options.into(),
        ))
        .map_err(|e| {
            anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
        })?;

        crate::convert::response_to_py(py, result)
    }

    #[pyo3(signature = (requests, opts=None))]
    pub fn evaluate_batch(
        &self,
        py: Python,
        requests: Vec<PyBatchRequest>,
        opts: Option<PyZenEvaluateOptions>,
    ) -> PyResult<Py<PyAny>> {
        let options: EvaluationOptions = opts.unwrap_or_default().into();

        let handles: Vec<_> = requests
            .into_iter()
            .map(|request| {
                let engine = self.engine.clone();
                worker_pool().spawn_pinned(move || async move {
                    engine
                        .evaluate_with_opts(request.key, request.context.into(), options)
                        .await
                        .map(crate::convert::PortableResponse::build)
                        .map_err(|e| {
                            serde_json::to_value(e.as_ref())
                                .unwrap_or_else(|_| Value::String(e.to_string()))
                        })
                })
            })
            .collect();

        let results = py.allow_threads(|| {
            block_on(async move {
                let mut out = Vec::with_capacity(handles.len());
                for handle in handles {
                    out.push(handle.await);
                }
                out
            })
        });

        let list = PyList::empty(py);
        for result in results {
            let item = PyDict::new(py);
            match result {
                Ok(Ok(response)) => {
                    item.set_item("success", true)?;
                    item.set_item("data", response.into_py(py)?)?;
                }
                Ok(Err(error)) => {
                    item.set_item("success", false)?;
                    item.set_item("error", pythonize(py, &error)?)?;
                }
                Err(_) => {
                    item.set_item("success", false)?;
                    item.set_item("error", "evaluation worker panicked")?;
                }
            }
            list.append(item)?;
        }

        Ok(list.into_py_any(py)?)
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
            let response = worker_pool()
                .spawn_pinned(move || async move {
                    engine
                        .evaluate_with_opts(key, context.into(), options.into())
                        .await
                        .map(crate::convert::PortableResponse::build)
                        .map_err(|e| {
                            anyhow!(
                                serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string())
                            )
                        })
                })
                .await
                .context("Failed to join threads")??;

            Python::with_gil(|py| response.into_py(py))
        })?;

        Ok(result.unbind())
    }

    pub fn create_decision(&self, content: PyZenDecisionContentJson) -> PyResult<PyZenDecision> {
        let decision = self
            .engine
            .create_decision(content.0 .0)
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(PyZenDecision::from(decision))
    }

    pub fn get_decision<'py>(&'py self, _py: Python<'py>, key: &str) -> PyResult<PyZenDecision> {
        let decision = block_on(self.engine.get_decision(key))
            .context("Failed to find decision with given key")?
            .map_err(|e| anyhow!(e.to_string()))?;

        Ok(PyZenDecision::from(decision))
    }
}
