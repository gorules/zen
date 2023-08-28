use anyhow::anyhow;
use async_trait::async_trait;
use pyo3::{PyObject, Python};
use std::sync::Arc;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResult};
use zen_engine::model::DecisionContent;

#[derive(Default)]
pub(crate) struct PyDecisionLoader(Option<PyObject>);

impl From<PyObject> for PyDecisionLoader {
    fn from(value: PyObject) -> Self {
        Self(Some(value))
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

#[async_trait]
impl DecisionLoader for PyDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResult<Arc<DecisionContent>> {
        self.load_element(key).map_err(|e| {
            LoaderError::Internal {
                source: e,
                key: key.to_string(),
            }
            .into()
        })
    }
}
