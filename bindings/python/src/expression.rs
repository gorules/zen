use anyhow::{anyhow, Context};
use bumpalo::Bump;
use pyo3::types::PyDict;
use pyo3::{pyfunction, PyObject, PyResult, Python, ToPyObject};
use pythonize::depythonize;
use serde_json::Value;
use zen_expression::compiler::Compiler;
use zen_expression::lexer::Lexer;
use zen_expression::parser::Parser;
use zen_expression::IsolateError;

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
