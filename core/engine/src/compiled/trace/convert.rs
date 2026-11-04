use super::value::{Obj, Val};
use super::{bad, TraceError, R};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::rc::Rc;
use zen_types::rcvalue::RcValue;
use zen_types::variable::Variable;

pub fn from_variable(v: &Variable) -> Val {
    match v {
        Variable::Null => Val::Null,
        Variable::Bool(b) => Val::Bool(*b),
        Variable::Number(n) => Val::Num(*n),
        Variable::String(s) => Val::Str(Rc::from(&**s)),
        Variable::Array(a) => Val::arr(a.borrow().iter().map(from_variable).collect()),
        Variable::Object(o) => {
            let m = o.borrow();
            let mut out = Obj::with_capacity(m.len());
            if m.shape().is_some() {
                for (k, x) in m.iter() {
                    out.insert(Rc::from(&**k), from_variable(x));
                }
            } else {
                let mut entries: Vec<(&str, &Variable)> =
                    m.iter().map(|(k, x)| (&**k, x)).collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (k, x) in entries {
                    out.insert(Rc::from(k), from_variable(x));
                }
            }
            Val::obj(out)
        }
        Variable::Dynamic(d) => from_json(&d.to_value()),
    }
}

#[derive(Default)]
pub struct Shared {
    memo: super::value::FastMap<usize, Val>,
    start: super::value::FastMap<usize, Val>,
    shapes: super::value::FastMap<usize, Option<Rc<zen_types::variable::Shape>>>,
}

impl Shared {
    pub fn val(&mut self, v: &Variable) -> Val {
        let key = match v {
            Variable::Array(a) => Rc::as_ptr(a) as *const u8 as usize,
            Variable::Object(o) => Rc::as_ptr(o) as *const u8 as usize,
            _ => return from_variable(v),
        };
        if let Some(x) = self.memo.get(&key) {
            return x.clone();
        }
        if let Some(old) = self.start.get(&key).cloned() {
            if self.same(key, v, &old) {
                self.memo.insert(key, old.clone());
                return old;
            }
        }
        let out = match v {
            Variable::Array(a) => Val::arr(a.borrow().iter().map(|x| self.val(x)).collect()),
            Variable::Object(o) => {
                let m = o.borrow();
                let mut out = Obj::with_capacity(m.len());
                self.shapes.entry(key).or_insert_with(|| m.shape().cloned());
                if m.shape().is_some() {
                    for (k, x) in m.iter() {
                        out.insert(Rc::from(&**k), self.val(x));
                    }
                } else {
                    let mut entries: Vec<(&str, &Variable)> =
                        m.iter().map(|(k, x)| (&**k, x)).collect();
                    entries.sort_by(|a, b| a.0.cmp(b.0));
                    for (k, x) in entries {
                        out.insert(Rc::from(k), self.val(x));
                    }
                }
                Val::obj(out)
            }
            _ => unreachable!(),
        };
        self.memo.insert(key, out.clone());
        out
    }

    fn same(&mut self, key: usize, v: &Variable, old: &Val) -> bool {
        match (v, old) {
            (Variable::Array(a), Val::Arr(o)) => {
                let a = a.borrow();
                a.len() == o.len() && a.iter().zip(o.iter()).all(|(x, y)| self.child(x, y))
            }
            (Variable::Object(m), Val::Obj(o)) => {
                let m = m.borrow();
                let shape_same = match (m.shape(), self.shapes.get(&key)) {
                    (None, Some(None)) => true,
                    (Some(a), Some(Some(b))) => Rc::ptr_eq(a, b),
                    _ => false,
                };
                shape_same
                    && m.len() == o.len()
                    && m.iter().all(|(k, x)| match o.get(k) {
                        Some(y) => self.child(x, y),
                        None => false,
                    })
            }
            _ => false,
        }
    }

    fn child(&mut self, x: &Variable, y: &Val) -> bool {
        match (x, y) {
            (Variable::Null, Val::Null) => true,
            (Variable::Bool(a), Val::Bool(b)) => a == b,
            (Variable::Number(a), Val::Num(b)) => a.serialize() == b.serialize(),
            (Variable::String(a), Val::Str(b)) => **a == **b,
            (Variable::Array(_), Val::Arr(_)) | (Variable::Object(_), Val::Obj(_)) => {
                self.val(x).ptr() == y.ptr()
            }
            _ => false,
        }
    }
}

fn from_json(v: &serde_json::Value) -> Val {
    match v {
        serde_json::Value::Null => Val::Null,
        serde_json::Value::Bool(b) => Val::Bool(*b),
        serde_json::Value::Number(n) => {
            let t = n.to_string();
            match super::value::parse_number(&t) {
                Some(d) => Val::Num(d),
                None => Val::Num(n.as_f64().and_then(Decimal::from_f64).unwrap_or_default()),
            }
        }
        serde_json::Value::String(s) => Val::str(s),
        serde_json::Value::Array(a) => Val::arr(a.iter().map(from_json).collect()),
        serde_json::Value::Object(o) => {
            let mut out = Obj::with_capacity(o.len());
            for (k, x) in o.iter() {
                out.insert(Rc::from(k.as_str()), from_json(x));
            }
            Val::obj(out)
        }
    }
}

pub fn to_variable(v: &Val) -> Variable {
    match v {
        Val::Null => Variable::Null,
        Val::Bool(b) => Variable::Bool(*b),
        Val::Num(n) => Variable::Number(*n),
        Val::Str(s) => Variable::String(zen_types::symbol::Symbol::from(&**s)),
        Val::Arr(a) => Variable::from_array(a.iter().map(to_variable).collect()),
        Val::Obj(o) => {
            let mut m = zen_types::variable::VariableMap::with_capacity(o.len());
            for (k, x) in o.iter() {
                m.insert(zen_types::symbol::Symbol::from(&**k), to_variable(x));
            }
            Variable::from_object(m)
        }
    }
}

pub fn from_reference(v: &Variable) -> R<Val> {
    let rc = RcValue::from(v);
    let var = Variable::deserialize_ref(rc).map_err(|e| bad(format!("reference trace: {e}")))?;
    Ok(from_variable(&var))
}

pub fn from_reference_string(s: &str) -> R<Val> {
    let doc = super::value::parse(s).map_err(|e| TraceError::Json(e.to_string()))?;
    let root = doc
        .as_obj()
        .ok_or_else(|| bad("reference trace: expected root object"))?;
    let refs: Vec<Val> = match root.get("$refs") {
        Some(Val::Arr(a)) => a.to_vec(),
        _ => Vec::new(),
    };
    let mut r = Refs {
        refs: &refs,
        done: vec![None; refs.len()],
        busy: vec![false; refs.len()],
    };
    let top = root
        .get("$root")
        .ok_or_else(|| bad("reference trace: missing $root"))?;
    r.value(top)
}

struct Refs<'a> {
    refs: &'a [Val],
    done: Vec<Option<Val>>,
    busy: Vec<bool>,
}

fn ref_id(s: &str) -> Option<usize> {
    s.strip_prefix('@')?.parse().ok()
}

fn unescape_at(s: &Rc<str>) -> Rc<str> {
    if s.starts_with("@@") {
        Rc::from(&s[1..])
    } else {
        s.clone()
    }
}

impl Refs<'_> {
    fn resolve(&mut self, i: usize) -> R<Val> {
        if let Some(v) = &self.done[i] {
            return Ok(v.clone());
        }
        if self.busy[i] {
            return Err(bad(format!("reference trace: cycle through @{i}")));
        }
        self.busy[i] = true;
        let v = self.value(&self.refs[i].clone())?;
        self.done[i] = Some(v.clone());
        Ok(v)
    }

    fn value(&mut self, v: &Val) -> R<Val> {
        match v {
            Val::Str(s) => match ref_id(s) {
                Some(i) if i < self.refs.len() => self.resolve(i),
                Some(i) => Err(bad(format!("reference trace: invalid reference @{i}"))),
                None => Ok(Val::Str(unescape_at(s))),
            },
            Val::Arr(a) => Ok(Val::arr(a.iter().map(|x| self.value(x)).collect::<R<_>>()?)),
            Val::Obj(o) => {
                let mut out = Obj::with_capacity(o.len());
                for (k, x) in o.iter() {
                    let key = match ref_id(k) {
                        Some(i) if i < self.refs.len() => match self.resolve(i)? {
                            Val::Str(s) => s,
                            _ => {
                                return Err(bad(
                                    "reference trace: a key reference must be a string",
                                ))
                            }
                        },
                        Some(i) => {
                            return Err(bad(format!("reference trace: invalid reference @{i}")))
                        }
                        None => unescape_at(k),
                    };
                    let val = self.value(x)?;
                    out.insert(key, val);
                }
                Ok(Val::obj(out))
            }
            _ => Ok(v.clone()),
        }
    }
}
