use anyhow::{anyhow, Context};
use pyo3::types::PyDict;
use pyo3::{pyfunction, PyObject, PyResult, Python, ToPyObject};
use pythonize::depythonize;
use serde_json::Value;

use zen_expression::isolate::Isolate;

use crate::value::PyValue;

#[pyfunction]
pub fn evaluate_expression(
    py: Python,
    expression: String,
    ctx: Option<&PyDict>,
) -> PyResult<PyObject> {
    let isolate = Isolate::default();
    let ctx = ctx
        .map(|ctx| depythonize(ctx))
        .transpose()
        .context("Failed to convert context")?;

    if let Some(env_value) = ctx {
        isolate.inject_env(&env_value);
    }

    let result = isolate
        .run_standard(expression.as_str())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyValue(result).to_object(py))
}

#[pyfunction]
pub fn evaluate_unary_expression(
    py: Python,
    expression: String,
    ctx: &PyDict,
) -> PyResult<PyObject> {
    let env_value: Value = depythonize(ctx).context("Failed to convert context")?;

    let Some(env_object) = env_value.as_object() else {
        return Err(anyhow!("Context must be an object").into());
    };

    if !env_object.contains_key("$") {
        return Err(anyhow!("Context must contain '$' reference.").into());
    }

    let isolate = Isolate::default();
    isolate.inject_env(&env_value);

    let result = isolate
        .run_unary(expression.as_str())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyValue(result).to_object(py))
}
