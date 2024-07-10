use std::future::Future;
use std::sync::Arc;

use anyhow::anyhow;
use pyo3::{PyObject, Python};

use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;

#[derive(Default)]
pub(crate) struct PyDecisionLoader(Option<PyObject>);

impl From<PyObject> for PyDecisionLoader {
    fn from(value: PyObject) -> Self {
        Self(Some(value))
    }
}

impl From<Option<PyObject>> for PyDecisionLoader {
    fn from(value: Option<PyObject>) -> Self {
        Self(value)
    }
}

impl PyDecisionLoader {
    fn load_element(&self, key: &str) -> Result<Arc<DecisionContent>, anyhow::Error> {
        let Some(object) = &self.0 else {
            return Err(anyhow!("Loader is not defined"));
        };

        let content = Python::with_gil(|py| {
            let result = object.call1(py, (key,))?;
            result.extract::<String>(py)
        })?;

        Ok(serde_json::from_str::<DecisionContent>(&content)?.into())
    }
}

impl DecisionLoader for PyDecisionLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            self.load_element(key).map_err(|e| {
                LoaderError::Internal {
                    source: e,
                    key: key.to_string(),
                }
                .into()
            })
        }
    }
}
