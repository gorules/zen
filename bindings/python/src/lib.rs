use crate::decision::PyZenDecision;
use crate::engine::PyZenEngine;
use pyo3::types::PyModule;
use pyo3::{pymodule, PyResult, Python};

mod decision;
mod engine;
mod loader;
mod value;

#[pymodule]
fn zen(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<PyZenEngine>()?;
    m.add_class::<PyZenDecision>()?;
    Ok(())
}
