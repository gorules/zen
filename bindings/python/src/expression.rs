use crate::variable::PyVariable;
use anyhow::anyhow;
use either::Either;
use pyo3::{pyclass, pyfunction, pymethods, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pythonize::pythonize;
use zen_expression::expression::{Standard, Unary};
use zen_expression::{Expression, Variable};

#[pyfunction]
pub fn compile_expression(expression: &str) -> PyResult<PyExpression> {
    let expr = zen_expression::compile_expression(expression)
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyExpression {
        expression: Either::Left(expr),
    })
}

#[pyfunction]
pub fn compile_unary_expression(expression: &str) -> PyResult<PyExpression> {
    let expr = zen_expression::compile_unary_expression(expression)
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyExpression {
        expression: Either::Right(expr),
    })
}

#[pyfunction]
#[pyo3(signature = (expression, ctx=None))]
pub fn evaluate_expression(
    py: Python,
    expression: &str,
    ctx: Option<PyVariable>,
) -> PyResult<Py<PyAny>> {
    let context = ctx.map(|c| c.into_inner()).unwrap_or(Variable::Null);

    let result = zen_expression::evaluate_expression(expression, context)
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    PyVariable(result).into_py_any(py)
}

#[pyfunction]
pub fn evaluate_unary_expression(expression: &str, ctx: PyVariable) -> PyResult<bool> {
    let result = zen_expression::evaluate_unary_expression(expression, ctx.into_inner())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[pyfunction]
pub fn render_template(py: Python, template: &str, ctx: PyVariable) -> PyResult<Py<PyAny>> {
    let result = zen_tmpl::render(template, ctx.into_inner())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    PyVariable(result).into_py_any(py)
}

#[pyclass]
pub struct PyExpression {
    expression: Either<Expression<Standard>, Expression<Unary>>,
}
#[pymethods]
impl PyExpression {
    #[pyo3(signature = (ctx=None))]
    pub fn evaluate(&self, py: Python, ctx: Option<PyVariable>) -> PyResult<Py<PyAny>> {
        let context = ctx.map(|c| c.into_inner()).unwrap_or(Variable::Null);
        let maybe_result = match &self.expression {
            Either::Left(standard) => standard.evaluate(context),
            Either::Right(unary) => unary.evaluate(context).map(Variable::Bool),
        };

        let result = maybe_result
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        PyVariable(result).into_py_any(py)
    }
}

#[pyfunction]
pub fn validate_expression(py: Python, expression: &str) -> PyResult<Option<Py<PyAny>>> {
    let Err(error) = zen_expression::validate::validate_expression(expression) else {
        return Ok(None);
    };

    Ok(Some(pythonize(py, &error)?.unbind()))
}

#[pyfunction]
pub fn validate_unary_expression(py: Python, expression: &str) -> PyResult<Option<Py<PyAny>>> {
    let Err(error) = zen_expression::validate::validate_expression(expression) else {
        return Ok(None);
    };

    Ok(Some(pythonize(py, &error)?.unbind()))
}
