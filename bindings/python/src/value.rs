use pyo3::prelude::{PyDictMethods, PyListMethods};
use pyo3::types::{PyDict, PyList};
use pyo3::{Bound, IntoPyObject, IntoPyObjectExt, PyAny, PyErr, PyResult, Python};
use serde_json::Value;

#[repr(transparent)]
#[derive(Clone, Debug)]
pub struct PyValue(pub Value);

pub fn value_to_object<'py>(py: Python<'py>, val: &Value) -> PyResult<Bound<'py, PyAny>> {
    match val {
        Value::Null => py.None().into_bound_py_any(py),
        Value::Bool(b) => b.into_bound_py_any(py),
        Value::Number(n) => {
            let oi64 = n.as_i64().map(|i| i.into_bound_py_any(py));
            let ou64 = n.as_u64().map(|i| i.into_bound_py_any(py));
            let of64 = n.as_f64().map(|i| i.into_bound_py_any(py));
            oi64.or(ou64).or(of64).expect("number too large")
        }
        Value::String(s) => s.into_bound_py_any(py),
        Value::Array(v) => {
            let list = PyList::empty(py);
            for item in v {
                list.append(value_to_object(py, item)?)?;
            }

            list.into_bound_py_any(py)
        }
        Value::Object(m) => {
            let dict = PyDict::new(py);
            for (key, value) in m {
                dict.set_item(key, value_to_object(py, value)?)?;
            }

            dict.into_bound_py_any(py)
        }
    }
}

impl<'py> IntoPyObject<'py> for PyValue {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        value_to_object(py, &self.0)
    }
}
