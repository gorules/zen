use anyhow::{anyhow, Context};
use pyo3::types::PyDict;
use pyo3::{pyfunction, PyObject, PyResult, Python, ToPyObject};
use pythonize::depythonize;
use serde_json::Value;

use crate::value::PyValue;

#[pyfunction]
pub fn evaluate_expression(
    py: Python,
    expression: String,
    ctx: Option<&PyDict>,
) -> PyResult<PyObject> {
    let context = ctx
        .map(|ctx| depythonize(ctx))
        .transpose()
        .context("Failed to convert context")?
        .unwrap_or(Value::Null);

    let result = zen_expression::evaluate_expression(expression.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyValue(result.to_value()).to_object(py))
}

#[pyfunction]
pub fn evaluate_unary_expression(expression: String, ctx: &PyDict) -> PyResult<bool> {
    let context: Value = depythonize(ctx).context("Failed to convert context")?;

    let result = zen_expression::evaluate_unary_expression(expression.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[pyfunction]
pub fn render_template(py: Python, template: String, ctx: &PyDict) -> PyResult<PyObject> {
    let context: Value = depythonize(ctx).context("Failed to convert context")?;

    let result = zen_tmpl::render(template.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyValue(result.to_value()).to_object(py))
}
