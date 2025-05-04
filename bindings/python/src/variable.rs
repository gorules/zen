use anyhow::Context;
use pyo3::prelude::{PyAnyMethods, PyBytesMethods, PyDictMethods, PyListMethods, PyStringMethods};
use pyo3::types::{PyBytes, PyDict, PyList, PyString};
use pyo3::{Bound, FromPyObject, IntoPyObject, IntoPyObjectExt, PyAny, PyErr, PyResult, Python};
use pythonize::{depythonize, pythonize};
use rust_decimal::prelude::ToPrimitive;
use zen_expression::Variable;

#[repr(transparent)]
#[derive(Clone, Debug)]
pub struct PyVariable(pub Variable);

impl PyVariable {
    pub fn into_inner(self) -> Variable {
        self.0
    }
}

pub fn variable_to_object<'py>(py: Python<'py>, val: &Variable) -> PyResult<Bound<'py, PyAny>> {
    match val {
        Variable::Null => py.None().into_bound_py_any(py),
        Variable::Bool(b) => b.into_bound_py_any(py),
        Variable::Number(n) => {
            let of64 = n.to_f64().map(|i| i.into_bound_py_any(py));
            let oi64 = n.to_i64().map(|i| i.into_bound_py_any(py));
            let ou64 = n.to_u64().map(|i| i.into_bound_py_any(py));
            of64.or(oi64).or(ou64).expect("number too large")
        }
        Variable::String(s) => s.into_bound_py_any(py),
        Variable::Array(v) => {
            let list = PyList::empty(py);
            let b = v.borrow();
            for item in b.iter() {
                list.append(variable_to_object(py, item)?)?;
            }

            list.into_bound_py_any(py)
        }
        Variable::Object(m) => {
            let dict = PyDict::new(py);
            let b = m.borrow();
            for (key, value) in b.iter() {
                dict.set_item(String::from(key.as_ref()), variable_to_object(py, value)?)?;
            }

            dict.into_bound_py_any(py)
        }
        Variable::Dynamic(d) => Ok(pythonize(py, &d.to_value())?),
    }
}

impl<'py> IntoPyObject<'py> for PyVariable {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        variable_to_object(py, &self.0)
    }
}

impl<'py> FromPyObject<'py> for PyVariable {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(s) = ob.downcast::<PyString>() {
            let str_slice = s.to_str()?;

            let var = serde_json::from_str(str_slice).context("Invalid JSON")?;
            return Ok(PyVariable(var));
        }

        if let Ok(b) = ob.downcast::<PyBytes>() {
            let bytes = b.as_bytes();

            let var = serde_json::from_slice(bytes).context("Invalid JSON")?;
            return Ok(PyVariable(var));
        }

        let var = depythonize(ob)?;
        Ok(PyVariable(var))
    }
}
