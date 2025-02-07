use anyhow::{anyhow, Context};
use pyo3::types::PyDict;
use pyo3::{pyfunction, Bound, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pythonize::depythonize;
use serde_json::Value;

use crate::value::PyValue;

#[pyfunction]
#[pyo3(signature = (expression, ctx=None))]
pub fn evaluate_expression(
    py: Python,
    expression: String,
    ctx: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let context = ctx
        .map(|ctx| depythonize(ctx))
        .transpose()
        .context("Failed to convert context")?
        .unwrap_or(Value::Null);

    let result = zen_expression::evaluate_expression(expression.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    PyValue(result.to_value()).into_py_any(py)
}

#[pyfunction]
pub fn evaluate_unary_expression(expression: String, ctx: &Bound<'_, PyDict>) -> PyResult<bool> {
    let context: Value = depythonize(ctx).context("Failed to convert context")?;

    let result = zen_expression::evaluate_unary_expression(expression.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[pyfunction]
pub fn render_template(
    py: Python,
    template: String,
    ctx: &Bound<'_, PyDict>,
) -> PyResult<Py<PyAny>> {
    let context: Value = depythonize(ctx).context("Failed to convert context")?;

    let result = zen_tmpl::render(template.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    PyValue(result.to_value()).into_py_any(py)
}
