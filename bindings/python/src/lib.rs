use crate::content::PyZenDecisionContent;
use crate::decision::PyZenDecision;
use crate::engine::PyZenEngine;
use crate::expression::{
    compile_expression, compile_unary_expression, evaluate_expression, evaluate_unary_expression,
    render_template, validate_expression, validate_unary_expression, PyExpression,
};
use pyo3::prelude::PyModuleMethods;
use pyo3::types::PyModule;
use pyo3::{pymodule, wrap_pyfunction, Bound, PyResult, Python};

mod content;
mod custom_node;
mod decision;
mod engine;
mod expression;
mod loader;
mod mt;
mod types;
mod value;
mod variable;

#[pymodule]
fn zen(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyZenEngine>()?;
    m.add_class::<PyZenDecision>()?;
    m.add_class::<PyExpression>()?;
    m.add_class::<PyZenDecisionContent>()?;
    m.add_function(wrap_pyfunction!(evaluate_expression, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate_unary_expression, m)?)?;
    m.add_function(wrap_pyfunction!(render_template, m)?)?;
    m.add_function(wrap_pyfunction!(compile_expression, m)?)?;
    m.add_function(wrap_pyfunction!(compile_unary_expression, m)?)?;
    m.add_function(wrap_pyfunction!(validate_expression, m)?)?;
    m.add_function(wrap_pyfunction!(validate_unary_expression, m)?)?;

    Ok(())
}
