use crate::decision::PyZenDecision;
use crate::engine::PyZenEngine;
use crate::expression::{evaluate_expression, evaluate_unary_expression, render_template};
use pyo3::types::PyModule;
use pyo3::{pymodule, wrap_pyfunction, PyResult, Python};

mod custom_node;
mod decision;
mod engine;
mod expression;
mod loader;
mod types;
mod value;

#[pymodule]
fn zen(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<PyZenEngine>()?;
    m.add_class::<PyZenDecision>()?;
    m.add_function(wrap_pyfunction!(evaluate_expression, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate_unary_expression, m)?)?;
    m.add_function(wrap_pyfunction!(render_template, m)?)?;

    Ok(())
}
