use std::collections::HashMap;
use std::rc::Rc;

use pyo3::prelude::{PyDictMethods, PyListMethods};
use pyo3::types::{PyDict, PyList};
use pyo3::{Bound, IntoPyObjectExt, Py, PyAny, PyResult, Python};
use pythonize::pythonize;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use zen_engine::{DecisionGraphResponse, EvaluationTrace};
use zen_expression::variable::ToVariable;
use zen_expression::Variable;

use crate::value::value_to_object;

pub struct VariableConverter<'py> {
    py: Python<'py>,
    seen: HashMap<usize, Bound<'py, PyAny>>,
}

impl<'py> VariableConverter<'py> {
    pub fn new(py: Python<'py>) -> Self {
        Self {
            py,
            seen: HashMap::new(),
        }
    }

    pub fn convert(&mut self, var: &Variable) -> PyResult<Bound<'py, PyAny>> {
        let addr = match var {
            Variable::Array(a) => Some(Rc::as_ptr(a) as *const () as usize),
            Variable::Object(o) => Some(Rc::as_ptr(o) as *const () as usize),
            _ => None,
        };

        if let Some(addr) = addr {
            if let Some(existing) = self.seen.get(&addr) {
                return Ok(existing.clone());
            }
        }

        let built = self.build(var)?;
        if let Some(addr) = addr {
            self.seen.insert(addr, built.clone());
        }

        Ok(built)
    }

    fn build(&mut self, var: &Variable) -> PyResult<Bound<'py, PyAny>> {
        match var {
            Variable::Null => self.py.None().into_bound_py_any(self.py),
            Variable::Bool(b) => b.into_bound_py_any(self.py),
            Variable::Number(n) => self.number(*n),
            Variable::String(s) => s.into_bound_py_any(self.py),
            Variable::Array(a) => {
                let list = PyList::empty(self.py);
                let borrowed = a.borrow();
                for item in borrowed.iter() {
                    list.append(self.convert(item)?)?;
                }

                list.into_bound_py_any(self.py)
            }
            Variable::Object(o) => {
                let dict = PyDict::new(self.py);
                let borrowed = o.borrow();
                for (key, value) in borrowed.iter() {
                    dict.set_item(key.as_ref(), self.convert(value)?)?;
                }

                dict.into_bound_py_any(self.py)
            }
            Variable::Dynamic(d) => Ok(pythonize(self.py, &d.to_value())?),
        }
    }

    fn number(&self, n: Decimal) -> PyResult<Bound<'py, PyAny>> {
        let normalized = n.normalize();
        if normalized.fract().is_zero() {
            if let Some(i) = normalized.to_i64() {
                return i.into_bound_py_any(self.py);
            }

            if let Some(u) = normalized.to_u64() {
                return u.into_bound_py_any(self.py);
            }
        }

        normalized
            .to_string()
            .parse::<f64>()
            .unwrap_or(f64::NAN)
            .into_bound_py_any(self.py)
    }
}

enum PNode {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(Box<str>),
    Arr(Vec<u32>),
    Obj(Vec<(Box<str>, u32)>),
    Json(Value),
}

enum PortableTrace {
    None,
    Arena(u32),
    Json(Value),
}

pub struct PortableResponse {
    performance: String,
    nodes: Vec<PNode>,
    result_root: u32,
    trace: PortableTrace,
}

impl PortableResponse {
    pub fn build(response: DecisionGraphResponse) -> Self {
        let mut nodes = Vec::new();
        let mut memo = HashMap::new();

        let result_root = Self::add(&mut nodes, &mut memo, &response.result);

        let trace = match response.trace {
            None => PortableTrace::None,
            Some(EvaluationTrace::Graph(graph)) => {
                PortableTrace::Arena(Self::add(&mut nodes, &mut memo, &graph.to_variable()))
            }
            Some(EvaluationTrace::Policy(policy)) => {
                PortableTrace::Json(serde_json::to_value(&policy).unwrap_or_default())
            }
        };

        Self {
            performance: response.performance,
            nodes,
            result_root,
            trace,
        }
    }

    fn add(nodes: &mut Vec<PNode>, memo: &mut HashMap<usize, u32>, var: &Variable) -> u32 {
        let addr = match var {
            Variable::Array(a) => Some(Rc::as_ptr(a) as *const () as usize),
            Variable::Object(o) => Some(Rc::as_ptr(o) as *const () as usize),
            _ => None,
        };

        if let Some(addr) = addr {
            if let Some(id) = memo.get(&addr) {
                return *id;
            }
        }

        let node = match var {
            Variable::Null => PNode::Null,
            Variable::Bool(b) => PNode::Bool(*b),
            Variable::Number(n) => {
                let normalized = n.normalize();
                if normalized.fract().is_zero() {
                    if let Some(i) = normalized.to_i64() {
                        PNode::Int(i)
                    } else if let Some(u) = normalized.to_u64() {
                        PNode::Uint(u)
                    } else {
                        PNode::Float(normalized.to_string().parse::<f64>().unwrap_or(f64::NAN))
                    }
                } else {
                    PNode::Float(normalized.to_string().parse::<f64>().unwrap_or(f64::NAN))
                }
            }
            Variable::String(s) => PNode::Str(Box::from(s.as_ref())),
            Variable::Array(a) => {
                let borrowed = a.borrow();
                let mut children = Vec::with_capacity(borrowed.len());
                for item in borrowed.iter() {
                    children.push(Self::add(nodes, memo, item));
                }

                PNode::Arr(children)
            }
            Variable::Object(o) => {
                let borrowed = o.borrow();
                let mut entries = Vec::with_capacity(borrowed.len());
                for (key, value) in borrowed.iter() {
                    let child = Self::add(nodes, memo, value);
                    entries.push((Box::from(key.as_ref()), child));
                }

                PNode::Obj(entries)
            }
            Variable::Dynamic(d) => PNode::Json(d.to_value()),
        };

        let id = nodes.len() as u32;
        nodes.push(node);
        if let Some(addr) = addr {
            memo.insert(addr, id);
        }

        id
    }

    fn materialize<'py>(
        &self,
        py: Python<'py>,
        id: u32,
        cache: &mut [Option<Bound<'py, PyAny>>],
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(existing) = &cache[id as usize] {
            return Ok(existing.clone());
        }

        let built = match &self.nodes[id as usize] {
            PNode::Null => py.None().into_bound_py_any(py)?,
            PNode::Bool(b) => b.into_bound_py_any(py)?,
            PNode::Int(i) => i.into_bound_py_any(py)?,
            PNode::Uint(u) => u.into_bound_py_any(py)?,
            PNode::Float(f) => f.into_bound_py_any(py)?,
            PNode::Str(s) => s.into_bound_py_any(py)?,
            PNode::Arr(children) => {
                let list = PyList::empty(py);
                for child in children {
                    list.append(self.materialize(py, *child, cache)?)?;
                }

                list.into_bound_py_any(py)?
            }
            PNode::Obj(entries) => {
                let dict = PyDict::new(py);
                for (key, child) in entries {
                    dict.set_item(key.as_ref(), self.materialize(py, *child, cache)?)?;
                }

                dict.into_bound_py_any(py)?
            }
            PNode::Json(value) => value_to_object(py, value)?,
        };

        cache[id as usize] = Some(built.clone());
        Ok(built)
    }

    pub fn into_py(self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let mut cache = vec![None; self.nodes.len()];
        let dict = PyDict::new(py);

        dict.set_item("performance", &self.performance)?;
        dict.set_item(
            "result",
            self.materialize(py, self.result_root, &mut cache)?,
        )?;

        match &self.trace {
            PortableTrace::None => {}
            PortableTrace::Arena(root) => {
                dict.set_item("trace", self.materialize(py, *root, &mut cache)?)?;
            }
            PortableTrace::Json(value) => {
                dict.set_item("trace", value_to_object(py, value)?)?;
            }
        }

        Ok(dict.into_any().unbind())
    }
}

pub fn response_to_py(py: Python<'_>, response: DecisionGraphResponse) -> PyResult<Py<PyAny>> {
    let mut converter = VariableConverter::new(py);
    let dict = PyDict::new(py);

    dict.set_item("performance", response.performance)?;
    dict.set_item("result", converter.convert(&response.result)?)?;

    if let Some(trace) = response.trace {
        let trace_obj = match trace {
            EvaluationTrace::Graph(graph) => converter.convert(&graph.to_variable())?,
            EvaluationTrace::Policy(policy) => {
                let value = serde_json::to_value(&policy).unwrap_or_default();
                value_to_object(py, &value)?
            }
        };

        dict.set_item("trace", trace_obj)?;
    }

    Ok(dict.into_any().unbind())
}
