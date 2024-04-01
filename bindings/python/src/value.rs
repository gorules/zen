use pyo3::{PyObject, Python, ToPyObject};
use serde_json::Value;
use std::collections::HashMap;

#[repr(transparent)]
#[derive(Clone, Debug)]
pub struct PyValue(pub Value);

pub fn value_to_object(py: Python<'_>, val: &Value) -> PyObject {
    match val {
        Value::Null => py.None(),
        Value::Bool(b) => b.to_object(py),
        Value::Number(n) => {
            let oi64 = n.as_i64().map(|i| i.to_object(py));
            let ou64 = n.as_u64().map(|i| i.to_object(py));
            let of64 = n.as_f64().map(|i| i.to_object(py));
            oi64.or(ou64).or(of64).expect("number too large")
        }
        Value::String(s) => s.to_object(py),
        Value::Array(v) => {
            let inner: Vec<_> = v.iter().map(|x| value_to_object(py, x)).collect();
            inner.to_object(py)
        }
        Value::Object(m) => {
            let inner: HashMap<_, _> = m.iter().map(|(k, v)| (k, value_to_object(py, v))).collect();
            inner.to_object(py)
        }
    }
}

impl ToPyObject for PyValue {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        value_to_object(py, &self.0)
    }
}
