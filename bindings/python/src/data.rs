use std::sync::Arc;

use anyhow::anyhow;
use pyo3::types::PyBytes;
use pyo3::{pyclass, pymethods, Py, PyResult, Python};
use zen_engine::data::impact::ImpactAnalysis;
use zen_engine::EvaluationOptions;

use crate::engine::PyZenEngine;
use crate::mt::{block_on, worker_pool};
use zen_engine::Variable;
use zen_expression::variable::VariableMap;

/// Dotted top-level keys compose into nested objects — `customer.firstName`
/// becomes `{customer: {firstName: …}}`, matching how zen expressions resolve
/// member access. Spark's `df.toJSON()` emits flat dotted keys for dotted
/// column names; without this they would silently never match a rule.
pub(crate) fn normalize_dotted(variable: Variable) -> Variable {
    let Variable::Object(object) = &variable else {
        return variable;
    };
    if !object.borrow().keys().any(|key| key.contains('.')) {
        return variable;
    }

    let mut out = VariableMap::new();
    for (key, value) in object.borrow().iter() {
        let segments: Vec<String> = key.as_str().split('.').map(str::to_string).collect();
        insert_path(&mut out, &segments, value.clone());
    }
    Variable::from_object(out)
}

pub(crate) fn insert_path(map: &mut VariableMap, segments: &[String], value: Variable) {
    let (first, rest) = match segments.split_first() {
        Some(parts) => parts,
        None => return,
    };
    if rest.is_empty() {
        map.insert(first.as_str().into(), value);
        return;
    }

    let key = first.as_str().into();
    if !matches!(map.get(&key), Some(Variable::Object(_))) {
        map.insert(key.clone(), Variable::from_object(VariableMap::new()));
    }
    if let Some(Variable::Object(child)) = map.get(&key) {
        let child = child.clone();
        insert_path(&mut child.borrow_mut(), rest, value);
    }
}

#[pyclass]
#[pyo3(name = "ZenImpactAnalysis")]
pub struct PyZenImpactAnalysis {
    inner: Arc<ImpactAnalysis>,
}

#[pymethods]
impl PyZenImpactAnalysis {
    #[new]
    pub fn new(candidate: &PyZenEngine, baseline: &PyZenEngine) -> Self {
        Self {
            inner: Arc::new(ImpactAnalysis::new(
                candidate.engine.clone(),
                baseline.engine.clone(),
            )),
        }
    }

    /// One entry point for both feeds: a list of JSON documents (bytes or
    /// str), or any object implementing the Arrow PyCapsule protocol
    /// (`__arrow_c_stream__` — a pyarrow RecordBatch, Table, polars frame…).
    /// Inputs are parsed once and shared by both arms; dotted top-level keys
    /// and dotted column names compose into nested objects. Returns the
    /// serialized batch — `{rows, summary}`, with `before`/`after` present
    /// only for changed or failed records and summary counts that merge
    /// additively across batches. The GIL is released for the batch.
    #[pyo3(signature = (candidate_key, baseline_key, data, max_depth=None))]
    pub fn run_batch(
        &self,
        py: Python,
        candidate_key: String,
        baseline_key: String,
        data: pyo3::Bound<'_, pyo3::PyAny>,
        max_depth: Option<u8>,
    ) -> PyResult<Py<PyBytes>> {
        use pyo3::prelude::PyAnyMethods;

        enum Feed {
            Lines(Vec<Vec<u8>>),
            #[cfg(feature = "arrow")]
            Batches(Vec<arrow_array::RecordBatch>),
        }

        let feed = if let Ok(list) = data.downcast::<pyo3::types::PyList>() {
            use pyo3::prelude::PyListMethods;
            let mut lines = Vec::with_capacity(list.len());
            for item in list.iter() {
                let payload = item
                    .extract::<Vec<u8>>()
                    .or_else(|_| item.extract::<String>().map(String::into_bytes))
                    .map_err(|_| {
                        pyo3::exceptions::PyTypeError::new_err(
                            "list items must be JSON documents as bytes or str",
                        )
                    })?;
                lines.push(payload);
            }
            Feed::Lines(lines)
        } else if data.hasattr("__arrow_c_stream__")? {
            #[cfg(not(feature = "arrow"))]
            {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "Arrow input requires a build with the 'arrow' feature",
                ));
            }
            #[cfg(feature = "arrow")]
            {
                use pyo3::types::PyCapsuleMethods;
                let capsule_any = data.call_method0("__arrow_c_stream__")?;
                let capsule = capsule_any.downcast::<pyo3::types::PyCapsule>()?;
                let pointer =
                    capsule.pointer() as *mut arrow_array::ffi_stream::FFI_ArrowArrayStream;
                let reader =
                    unsafe { arrow_array::ffi_stream::ArrowArrayStreamReader::from_raw(pointer) }
                        .map_err(|e| anyhow!("arrow stream: {e}"))?;
                let mut batches = Vec::new();
                for batch in reader {
                    batches.push(batch.map_err(|e| anyhow!("arrow batch: {e}"))?);
                }
                Feed::Batches(batches)
            }
        } else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "expected a list of JSON documents or an Arrow stream object",
            ));
        };

        let analysis = self.inner.clone();
        let options = EvaluationOptions {
            trace: false,
            max_depth: max_depth.unwrap_or(10),
        };

        let body = py.allow_threads(move || {
            block_on(
                worker_pool().spawn_pinned(move || async move {
                    let variables = match feed {
                        Feed::Lines(lines) => {
                            let mut variables = Vec::with_capacity(lines.len());
                            for payload in &lines {
                                let parsed =
                                    serde_json::from_slice::<zen_engine::Variable>(payload)
                                        .map_err(|e| anyhow!("input is not JSON: {e}"))?;
                                variables.push(normalize_dotted(parsed));
                            }
                            variables
                        }
                        #[cfg(feature = "arrow")]
                        Feed::Batches(batches) => {
                            let mut variables = Vec::new();
                            for batch in &batches {
                                variables.extend(crate::columnar::inputs_from_batch(batch)?);
                            }
                            variables
                        }
                    };

                    let comparison = analysis
                        .compare(&candidate_key, &baseline_key)
                        .await
                        .map_err(|e| anyhow!(e.to_string()))?;
                    let batch = comparison.run_batch(variables, options).await;

                    serde_json::to_vec(&batch).map_err(|e| anyhow!(e))
                }),
            )
            .map_err(|_| anyhow!("evaluation worker panicked"))?
        })?;

        Ok(PyBytes::new(py, &body).into())
    }
}
