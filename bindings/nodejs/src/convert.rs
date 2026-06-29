use std::collections::HashMap;
use std::os::raw::c_char;
use std::ptr;
use std::rc::Rc;

use napi::bindgen_prelude::ToNapiValue;
use napi::sys;
use napi::sys::{napi_env, napi_value};
use serde_json::Value;
use zen_engine::{DecisionGraphResponse, EvaluationTrace, EvaluationTraceKind};
use zen_expression::variable::ToVariable;
use zen_expression::Variable;

enum PNode {
    Null,
    Bool(bool),
    Num(f64),
    Str(Box<str>),
    Arr(Vec<u32>),
    Obj(Vec<(Box<str>, u32)>),
    Json(Value),
}

pub struct PortableArena {
    nodes: Vec<PNode>,
}

impl PortableArena {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn add(&mut self, var: &Variable, memo: &mut HashMap<usize, u32>) -> u32 {
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
                PNode::Num(n.normalize().to_string().parse::<f64>().unwrap_or(f64::NAN))
            }
            Variable::String(s) => PNode::Str(Box::from(s.as_ref())),
            Variable::Array(a) => {
                let borrowed = a.borrow();
                let mut children = Vec::with_capacity(borrowed.len());
                for item in borrowed.iter() {
                    children.push(self.add(item, memo));
                }

                PNode::Arr(children)
            }
            Variable::Object(o) => {
                let borrowed = o.borrow();
                let mut entries = Vec::with_capacity(borrowed.len());
                for (key, value) in borrowed.iter() {
                    let child = self.add(value, memo);
                    entries.push((Box::from(key.as_ref()), child));
                }

                PNode::Obj(entries)
            }
            Variable::Dynamic(d) => PNode::Json(d.to_value()),
        };

        let id = self.nodes.len() as u32;
        self.nodes.push(node);
        if let Some(addr) = addr {
            memo.insert(addr, id);
        }

        id
    }
}

pub enum TraceField {
    None,
    Arena(u32),
    Json(Value),
}

pub struct NodeEvalResponse {
    pub performance: String,
    pub arena: PortableArena,
    pub result_root: u32,
    pub trace: TraceField,
}

impl NodeEvalResponse {
    pub fn build(response: DecisionGraphResponse, mode: EvaluationTraceKind) -> Self {
        let mut arena = PortableArena::new();
        let mut memo = HashMap::new();

        let result_root = arena.add(&response.result, &mut memo);

        let trace = match response.trace {
            None => TraceField::None,
            Some(EvaluationTrace::Graph(graph)) => {
                let variable = graph.to_variable();
                match mode {
                    EvaluationTraceKind::Default => {
                        TraceField::Arena(arena.add(&variable, &mut memo))
                    }
                    other => TraceField::Json(other.serialize_trace(&variable)),
                }
            }
            Some(EvaluationTrace::Policy(policy)) => {
                let value = match mode {
                    EvaluationTraceKind::String | EvaluationTraceKind::ReferenceString => {
                        Value::String(serde_json::to_string(&policy).unwrap_or_default())
                    }
                    _ => serde_json::to_value(&policy).unwrap_or_default(),
                };

                TraceField::Json(value)
            }
        };

        Self {
            performance: response.performance,
            arena,
            result_root,
            trace,
        }
    }
}

struct Materializer<'a> {
    env: napi_env,
    nodes: &'a [PNode],
    cache: Vec<napi_value>,
    keys: HashMap<&'a str, napi_value>,
}

impl<'a> Materializer<'a> {
    unsafe fn string(&self, s: &str) -> napi_value {
        let mut out = ptr::null_mut();
        sys::napi_create_string_utf8(
            self.env,
            s.as_ptr() as *const c_char,
            s.len() as isize,
            &mut out,
        );
        out
    }

    unsafe fn intern_key(&mut self, key: &'a str) -> napi_value {
        if let Some(existing) = self.keys.get(key) {
            return *existing;
        }

        let created = self.string(key);
        self.keys.insert(key, created);
        created
    }

    unsafe fn value(&mut self, id: u32) -> napi::Result<napi_value> {
        let cached = self.cache[id as usize];
        if !cached.is_null() {
            return Ok(cached);
        }

        let built = self.build(id)?;
        self.cache[id as usize] = built;
        Ok(built)
    }

    unsafe fn build(&mut self, id: u32) -> napi::Result<napi_value> {
        let nodes = self.nodes;
        match &nodes[id as usize] {
            PNode::Null => {
                let mut out = ptr::null_mut();
                sys::napi_get_null(self.env, &mut out);
                Ok(out)
            }
            PNode::Bool(b) => {
                let mut out = ptr::null_mut();
                sys::napi_get_boolean(self.env, *b, &mut out);
                Ok(out)
            }
            PNode::Num(n) => {
                let mut out = ptr::null_mut();
                sys::napi_create_double(self.env, *n, &mut out);
                Ok(out)
            }
            PNode::Str(s) => Ok(self.string(s)),
            PNode::Arr(children) => {
                let mut arr = ptr::null_mut();
                sys::napi_create_array_with_length(self.env, children.len(), &mut arr);
                for (index, child) in children.iter().enumerate() {
                    let value = self.value(*child)?;
                    sys::napi_set_element(self.env, arr, index as u32, value);
                }

                Ok(arr)
            }
            PNode::Obj(entries) => {
                let mut obj = ptr::null_mut();
                sys::napi_create_object(self.env, &mut obj);
                for (key, child) in entries.iter() {
                    let key_value = self.intern_key(key);
                    let value = self.value(*child)?;
                    sys::napi_set_property(self.env, obj, key_value, value);
                }

                Ok(obj)
            }
            PNode::Json(value) => ToNapiValue::to_napi_value(self.env, value.clone()),
        }
    }
}

impl ToNapiValue for NodeEvalResponse {
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        let mut materializer = Materializer {
            env,
            nodes: &val.arena.nodes,
            cache: vec![ptr::null_mut(); val.arena.nodes.len()],
            keys: HashMap::new(),
        };

        let mut obj = ptr::null_mut();
        sys::napi_create_object(env, &mut obj);

        let performance = materializer.string(&val.performance);
        let performance_key = materializer.string("performance");
        sys::napi_set_property(env, obj, performance_key, performance);

        let result = materializer.value(val.result_root)?;
        let result_key = materializer.string("result");
        sys::napi_set_property(env, obj, result_key, result);

        let trace = match &val.trace {
            TraceField::None => None,
            TraceField::Arena(root) => Some(materializer.value(*root)?),
            TraceField::Json(value) => Some(ToNapiValue::to_napi_value(env, value.clone())?),
        };

        if let Some(trace) = trace {
            let trace_key = materializer.string("trace");
            sys::napi_set_property(env, obj, trace_key, trace);
        }

        Ok(obj)
    }
}
