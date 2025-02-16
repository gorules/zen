use anyhow::Context;
use pyo3::prelude::{PyAnyMethods, PyStringMethods};
use pyo3::types::PyString;
use pyo3::{pyclass, pymethods, Bound, FromPyObject, PyAny, PyResult};
use pythonize::depythonize;
use std::sync::Arc;
use zen_engine::model::DecisionContent;

#[pyclass]
#[pyo3(name = "ZenDecisionContent")]
pub struct PyZenDecisionContent(pub Arc<DecisionContent>);

#[pymethods]
impl PyZenDecisionContent {
    #[new]
    pub fn new(data: &str) -> PyResult<Self> {
        let content = serde_json::from_str(data).context("Failed to parse JSON")?;
        Ok(Self(Arc::new(content)))
    }
}

pub struct PyZenDecisionContentJson(pub PyZenDecisionContent);

impl<'py> FromPyObject<'py> for PyZenDecisionContentJson {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(s) = ob.downcast::<PyZenDecisionContent>() {
            let borrow_ref = s.borrow();
            let content = borrow_ref.0.clone();

            return Ok(Self(PyZenDecisionContent(content)));
        }

        if let Ok(b) = ob.downcast::<PyString>() {
            let str = b.to_str()?;
            let content = serde_json::from_str(str).context("Invalid JSON")?;

            return Ok(Self(PyZenDecisionContent(Arc::new(content))));
        }

        let content = depythonize(ob)?;
        Ok(Self(PyZenDecisionContent(Arc::new(content))))
    }
}
