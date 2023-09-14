use crate::decision::PyZenDecision;
use crate::engine::PyZenEngine;
use crate::expression::{evaluate_expression, evaluate_unary_expression};
use pyo3::types::PyModule;
use pyo3::{pymodule, wrap_pyfunction, PyResult, Python};

mod decision;
mod engine;
mod expression;
mod loader;
mod value;

#[pymodule]
fn zen(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<PyZenEngine>()?;
    m.add_class::<PyZenDecision>()?;
    m.add_function(wrap_pyfunction!(evaluate_expression, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate_unary_expression, m)?)?;

    Ok(())
}
