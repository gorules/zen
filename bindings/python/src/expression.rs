use crate::variable::PyVariable;
use anyhow::{anyhow, Context};
use bumpalo::Bump;
use either::Either;
use pyo3::types::PyDict;
use pyo3::{pyclass, pyfunction, pymethods, Bound, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pythonize::depythonize;
use serde_json::Value;
use zen_expression::compiler::Compiler;
use zen_expression::lexer::Lexer;
use zen_expression::parser::Parser;
use zen_expression::IsolateError;

use crate::value::PyValue;
use zen_expression::expression::{Standard, Unary};
use zen_expression::{Expression, Variable};

#[pyfunction]
pub fn compile_expression(expression: String) -> PyResult<PyExpression> {
    let expr = zen_expression::compile_expression(expression.as_str())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyExpression {
        expression: Either::Left(expr),
    })
}

#[pyfunction]
pub fn compile_unary_expression(expression: String) -> PyResult<PyExpression> {
    let expr = zen_expression::compile_unary_expression(expression.as_str())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(PyExpression {
        expression: Either::Right(expr),
    })
}

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
        .unwrap_or(Variable::Null);

    let result = zen_expression::evaluate_expression(expression.as_str(), context)
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    PyVariable(result).into_py_any(py)
}

#[pyfunction]
pub fn evaluate_unary_expression(expression: String, ctx: &Bound<'_, PyDict>) -> PyResult<bool> {
    let context: Variable = depythonize(ctx).context("Failed to convert context")?;

    let result = zen_expression::evaluate_unary_expression(expression.as_str(), context)
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[pyfunction]
pub fn render_template(
    py: Python,
    template: String,
    ctx: &Bound<'_, PyDict>,
) -> PyResult<Py<PyAny>> {
    let context: Variable = depythonize(ctx)
        .context("Failed to convert context")
        .unwrap_or(Variable::Null);

    let result = zen_tmpl::render(template.as_str(), context)
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
    pub fn evaluate(&self, py: Python, ctx: Option<&Bound<'_, PyDict>>) -> PyResult<Py<PyAny>> {
        let context = ctx
            .map(|ctx| depythonize(ctx))
            .transpose()
            .context("Failed to convert context")?
            .unwrap_or(Variable::Null);

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
pub fn validate_expression(py: Python, expression: String) -> PyResult<PyObject> {
    let Some(err) = get_error(expression.as_str()) else {
        return Ok(py.None());
    };
    return Ok(PyValue(err).to_object(py));
}

#[pyfunction]
pub fn validate_unary_expression(py: Python, expression: String) -> PyResult<PyObject> {
    let Some(err) = get_unary_error(expression.as_str()) else {
        return Ok(py.None());
    };
    return Ok(PyValue(err).to_object(py));
}

fn get_unary_error(expression: &str) -> Option<Value> {
    let mut lexer = Lexer::new();
    let tokens = match lexer.tokenize(expression) {
        Err(e) => {
            return serde_json::to_value(IsolateError::LexerError { source: e })
                .ok()
                .into()
        }
        Ok(tokens) => tokens,
    };

    let bump = Bump::new();
    let parser = match Parser::try_new(tokens, &bump) {
        Err(e) => {
            return serde_json::to_value(IsolateError::ParserError { source: e })
                .ok()
                .into()
        }
        Ok(p) => p.unary(),
    };

    let parser_result = parser.parse();
    match parser_result.error() {
        Err(e) => {
            return serde_json::to_value(IsolateError::ParserError { source: e })
                .ok()
                .into()
        }
        Ok(n) => n,
    };

    let mut compiler = Compiler::new();
    if let Err(e) = compiler.compile(parser_result.root) {
        return serde_json::to_value(IsolateError::CompilerError { source: e })
            .ok()
            .into();
    }

    None
}

fn get_error(expression: &str) -> Option<Value> {
    let mut lexer = Lexer::new();
    let tokens = match lexer.tokenize(expression) {
        Err(e) => {
            return serde_json::to_value(IsolateError::LexerError { source: e })
                .ok()
                .into()
        }
        Ok(tokens) => tokens,
    };

    let bump = Bump::new();
    let parser = match Parser::try_new(tokens, &bump) {
        Err(e) => {
            return serde_json::to_value(IsolateError::ParserError { source: e })
                .ok()
                .into()
        }
        Ok(p) => p.standard(),
    };

    let parser_result = parser.parse();
    match parser_result.error() {
        Err(e) => {
            return serde_json::to_value(IsolateError::ParserError { source: e })
                .ok()
                .into()
        }
        Ok(n) => n,
    };

    let mut compiler = Compiler::new();
    if let Err(e) = compiler.compile(parser_result.root) {
        return serde_json::to_value(IsolateError::CompilerError { source: e })
            .ok()
            .into();
    }

    None
}
