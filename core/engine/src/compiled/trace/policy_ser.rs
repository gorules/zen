use super::collect::{PolicyExecution, PolicyFact, PolicyLevel};
use super::convert::from_variable;
use super::core::{dot_to_ptr, escape, pack};
use super::policy::Absent;
use super::value::{write_num, Val};
use rust_decimal::Decimal;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use std::sync::Arc;
use zen_types::variable::{Variable, VariableMap};

const TOKEN: &str = "$serde_json::private::RawValue";

fn raw<S: Serializer>() -> bool {
    std::any::type_name::<S>().starts_with("&mut serde_json::ser::Serializer<")
}

fn put_raw<S: Serializer>(text: &str, serializer: S) -> Result<S::Ok, S::Error> {
    let mut s = serializer.serialize_struct(TOKEN, 1)?;
    s.serialize_field(TOKEN, text)?;
    s.end()
}

struct Num<'a>(&'a Decimal);

thread_local! {
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static TEXT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static PACKED: std::cell::RefCell<Vec<(Variable, std::rc::Rc<Packed>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl Serialize for Num<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !raw::<S>() {
            return Variable::Number(*self.0).serialize(serializer);
        }
        if self.0.scale() == 0 {
            if let Ok(n) = i64::try_from(self.0.mantissa()) {
                return serializer.serialize_i64(n);
            }
        }
        let mut text = TEXT.with(|t| std::mem::take(&mut *t.borrow_mut()));
        text.clear();
        write_num(self.0, &mut text);
        let out = put_raw(&text, serializer);
        TEXT.with(|t| *t.borrow_mut() = text);
        out
    }
}

enum Packed {
    Unpacked,
    Ints(super::core::PackedInts),
    Nums(super::core::PackedNums),
    Plain(Vec<i64>),
    Val(Val),
}

impl Serialize for Packed {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use super::core::{PackedInts, PackedNums};
        struct Runs<'a>(&'a [(i64, usize, Option<i64>)]);
        impl Serialize for Runs<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
                for &(v, n, d) in self.0 {
                    match (d, n) {
                        (Some(d), n) => seq.serialize_element(&(v, n, d))?,
                        (None, 1) => seq.serialize_element(&[v])?,
                        (None, n) => seq.serialize_element(&(v, n))?,
                    }
                }
                seq.end()
            }
        }
        match self {
            Packed::Ints(PackedInts::Runs(runs)) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$runs", &Runs(runs))?;
                map.end()
            }
            Packed::Ints(PackedInts::Pick(values, at)) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("$pick", values)?;
                map.serialize_entry("at", at)?;
                map.end()
            }
            Packed::Nums(PackedNums::Runs(runs)) => {
                struct Steps<'a>(&'a [(Decimal, usize, Option<Decimal>)]);
                impl Serialize for Steps<'_> {
                    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
                        for (v, n, d) in self.0 {
                            match (d, n) {
                                (Some(d), n) => seq.serialize_element(&(Num(v), n, Num(d)))?,
                                (None, 1) => seq.serialize_element(&[Num(v)])?,
                                (None, n) => seq.serialize_element(&(Num(v), n))?,
                            }
                        }
                        seq.end()
                    }
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$runs", &Steps(runs))?;
                map.end()
            }
            Packed::Nums(PackedNums::Pick(values, at)) => {
                struct Nums<'a>(&'a [Decimal]);
                impl Serialize for Nums<'_> {
                    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
                        for v in self.0 {
                            seq.serialize_element(&Num(v))?;
                        }
                        seq.end()
                    }
                }
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("$pick", &Nums(values))?;
                map.serialize_entry("at", at)?;
                map.end()
            }
            Packed::Plain(ints) => ints.serialize(serializer),
            Packed::Unpacked => serializer.serialize_unit(),
            Packed::Val(val) => ValSer(val).serialize(serializer),
        }
    }
}

fn cached(v: &Variable, len: usize) -> Option<std::rc::Rc<Packed>> {
    PACKED.with(|p| {
        p.borrow()
            .iter()
            .find(|(x, _)| match (x, v) {
                (Variable::Array(a), Variable::Array(b)) => {
                    std::rc::Rc::ptr_eq(a, b)
                        || (a.borrow().len() == len && super::native::veq(x, v))
                }
                _ => false,
            })
            .map(|(_, val)| val.clone())
    })
}

fn remember(v: &Variable, packed: Packed) -> std::rc::Rc<Packed> {
    let packed = std::rc::Rc::new(packed);
    PACKED.with(|p| p.borrow_mut().push((v.clone(), packed.clone())));
    packed
}

fn packed(v: &Variable, items: &[Variable]) -> Option<std::rc::Rc<Packed>> {
    if let Some(found) = cached(v, items.len()) {
        return Some(found);
    }
    if let Some(ints) = ints(items) {
        return Some(match super::core::pack_ints(&ints) {
            Some(packed) => remember(v, Packed::Ints(packed)),
            None => remember(v, Packed::Plain(ints)),
        });
    }
    if !items.iter().all(scalar) || !worth(items) {
        return None;
    }
    if items.iter().all(|x| matches!(x, Variable::Number(_))) {
        let num = |i: usize| match &items[i] {
            Variable::Number(d) => *d,
            _ => Decimal::ZERO,
        };
        return Some(match super::core::pack_decimals(items.len(), num) {
            Some(packed) => remember(v, Packed::Nums(packed)),
            None => remember(v, Packed::Unpacked),
        });
    }
    let packed = Packed::Val(pack(&Val::arr(items.iter().map(from_variable).collect())));
    Some(remember(v, packed))
}

fn ints(items: &[Variable]) -> Option<Vec<i64>> {
    let mut out = Vec::with_capacity(items.len());
    for x in items {
        match x {
            Variable::Number(d) if d.scale() == 0 => out.push(i64::try_from(d.mantissa()).ok()?),
            _ => return None,
        }
    }
    Some(out)
}

struct Ptr<'a>(&'a str);

impl Serialize for Ptr<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let plain = self
            .0
            .bytes()
            .all(|b| b >= 0x20 && b != b'"' && b != b'\\' && b != b'~' && b != b'/' && b < 0x7f);
        if !plain {
            return serializer.serialize_str(&dot_to_ptr(self.0));
        }
        let mut text = String::with_capacity(self.0.len() + 2);
        if !self.0.is_empty() {
            for segment in self.0.split('.') {
                text.push('/');
                text.push_str(segment);
            }
        }
        serializer.serialize_str(&text)
    }
}

pub(crate) struct ValSer<'a>(pub(crate) &'a Val);

impl Serialize for ValSer<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Val::Null => serializer.serialize_unit(),
            Val::Bool(b) => serializer.serialize_bool(*b),
            Val::Num(d) => Num(d).serialize(serializer),
            Val::Str(s) => serializer.serialize_str(s),
            Val::Arr(a) => {
                let mut seq = serializer.serialize_seq(Some(a.len()))?;
                for x in a.iter() {
                    seq.serialize_element(&ValSer(x))?;
                }
                seq.end()
            }
            Val::Obj(o) => {
                let mut map = serializer.serialize_map(Some(o.len()))?;
                for (k, x) in o.iter() {
                    map.serialize_entry(&**k, &ValSer(x))?;
                }
                map.end()
            }
        }
    }
}

fn entries(m: &VariableMap) -> Vec<(&str, &Variable)> {
    let mut out: Vec<(&str, &Variable)> = m.iter().map(|(k, v)| (&**k, v)).collect();
    if m.shape().is_none() {
        out.sort_unstable_by(|a, b| a.0.cmp(b.0));
    }
    out
}

struct Plain<'a>(&'a Variable);

impl Serialize for Plain<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Variable::Number(d) => Num(d).serialize(serializer),
            Variable::Array(a) => {
                let a = a.borrow();
                let mut seq = serializer.serialize_seq(Some(a.len()))?;
                for x in a.iter() {
                    seq.serialize_element(&Plain(x))?;
                }
                seq.end()
            }
            Variable::Object(o) => {
                let o = o.borrow();
                let mut map = serializer.serialize_map(Some(o.len()))?;
                match o.shape() {
                    Some(_) => {
                        for (k, x) in o.iter() {
                            map.serialize_entry(&**k, &Plain(x))?;
                        }
                    }
                    None => {
                        for (k, x) in entries(&o) {
                            map.serialize_entry(k, &Plain(x))?;
                        }
                    }
                }
                map.end()
            }
            Variable::Dynamic(_) => ValSer(&from_variable(self.0)).serialize(serializer),
            other => other.serialize(serializer),
        }
    }
}

fn scalar(v: &Variable) -> bool {
    matches!(
        v,
        Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::String(_)
    )
}

fn mark(v: &Variable) -> u64 {
    match v {
        Variable::Null => 1,
        Variable::Bool(b) => 2 + *b as u64,
        Variable::Number(d) => {
            (d.mantissa() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ d.scale() as u64
        }
        Variable::String(s) => s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
        }),
        _ => 0,
    }
}

fn worth(items: &[Variable]) -> bool {
    let n = items.len();
    let mut structured = 0usize;
    for i in 1..n {
        let same = match (&items[i - 1], &items[i]) {
            (Variable::Number(a), Variable::Number(b)) => a.serialize() == b.serialize(),
            (Variable::String(a), Variable::String(b)) => a == b,
            (Variable::Bool(a), Variable::Bool(b)) => a == b,
            (Variable::Null, Variable::Null) => true,
            _ => false,
        };
        if same {
            structured += 1;
        }
    }
    if structured * 5 >= n {
        return true;
    }
    let mut bits = [0u64; 64];
    let mut distinct = 0usize;
    for item in items {
        let h = mark(item) as usize & 4095;
        let (word, bit) = (h / 64, 1u64 << (h % 64));
        if bits[word] & bit == 0 {
            bits[word] |= bit;
            distinct += 1;
        }
    }
    distinct * 2 <= n
}

struct Value<'a>(&'a Variable);

impl Serialize for Value<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Variable::Array(a) => {
                let a = a.borrow();
                if a.len() >= 4 {
                    if let Some(packed) = packed(self.0, &a) {
                        if !matches!(*packed, Packed::Unpacked) {
                            return packed.serialize(serializer);
                        }
                    }
                }
                let mut seq = serializer.serialize_seq(Some(a.len()))?;
                for x in a.iter() {
                    seq.serialize_element(&Value(x))?;
                }
                seq.end()
            }
            Variable::Object(o) => {
                let o = o.borrow();
                if o.iter().any(|(k, _)| k.starts_with('$')) {
                    let mut map = serializer.serialize_map(Some(1))?;
                    map.serialize_entry("$literal", &Plain(self.0))?;
                    return map.end();
                }
                let mut map = serializer.serialize_map(Some(o.len()))?;
                match o.shape() {
                    Some(_) => {
                        for (k, x) in o.iter() {
                            map.serialize_entry(&**k, &Value(x))?;
                        }
                    }
                    None => {
                        for (k, x) in entries(&o) {
                            map.serialize_entry(k, &Value(x))?;
                        }
                    }
                }
                map.end()
            }
            Variable::Number(d) => Num(d).serialize(serializer),
            Variable::Dynamic(_) => {
                ValSer(&pack(&escape(&from_variable(self.0)))).serialize(serializer)
            }
            other => other.serialize(serializer),
        }
    }
}

struct Bools<'a>(&'a [(std::sync::Arc<str>, bool)]);

impl Serialize for Bools<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.len() >= 4 {
            let list = Val::arr(self.0.iter().map(|(_, r)| Val::Bool(*r)).collect());
            return ValSer(&pack(&list)).serialize(serializer);
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (_, r) in self.0 {
            seq.serialize_element(r)?;
        }
        seq.end()
    }
}

fn default_id(block: &str, i: usize, id: &str) -> bool {
    let Some(n) = id
        .strip_prefix(block)
        .and_then(|rest| rest.strip_prefix(':'))
    else {
        return false;
    };
    let canonical = n.bytes().all(|b| b.is_ascii_digit()) && (n == "0" || !n.starts_with('0'));
    canonical && n.parse::<usize>() == Ok(i)
}

fn ids(block: &str, list: &[(std::sync::Arc<str>, bool)]) -> Option<Val> {
    let default = list
        .iter()
        .enumerate()
        .all(|(i, (id, _))| default_id(block, i, id));
    (!default).then(|| pack(&Val::arr(list.iter().map(|(id, _)| Val::str(id)).collect())))
}

fn evaluations_of(evaluations: &[Vec<(std::sync::Arc<str>, Variable)>]) -> Variable {
    Variable::from_array(
        evaluations
            .iter()
            .map(|fields| {
                let mut map = VariableMap::with_capacity(fields.len());
                for (k, v) in fields {
                    map.insert((&**k).into(), v.clone());
                }
                Variable::from_object(map)
            })
            .collect(),
    )
}

pub(crate) enum Frozen {
    Null,
    Bool(bool),
    Num(Decimal),
    Str(Box<str>),
    Json(Arc<str>),
}

impl Serialize for Frozen {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Frozen::Null => serializer.serialize_unit(),
            Frozen::Bool(b) => serializer.serialize_bool(*b),
            Frozen::Num(d) => Num(d).serialize(serializer),
            Frozen::Str(text) => serializer.serialize_str(text),
            Frozen::Json(text) if raw::<S>() => put_raw(text, serializer),
            Frozen::Json(text) => super::JsonText(text).serialize(serializer),
        }
    }
}

fn same_list(a: &[Variable], b: &[Variable]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Variable::Number(p), Variable::Number(q)) => p.serialize() == q.serialize(),
            (Variable::Bool(p), Variable::Bool(q)) => p == q,
            (Variable::Null, Variable::Null) => true,
            (Variable::String(p), Variable::String(q)) => p == q,
            _ => super::native::veq(x, y),
        })
}

#[derive(Default)]
struct Freezer {
    seen: Vec<(usize, Variable, Arc<str>)>,
}

impl Freezer {
    fn freeze(&mut self, v: &Variable) -> Frozen {
        let at = match v {
            Variable::Null => return Frozen::Null,
            Variable::Bool(b) => return Frozen::Bool(*b),
            Variable::Number(d) => return Frozen::Num(*d),
            Variable::String(s) => return Frozen::Str(Box::from(&**s)),
            Variable::Array(a) => std::rc::Rc::as_ptr(a) as *const u8 as usize,
            Variable::Object(o) => std::rc::Rc::as_ptr(o) as *const u8 as usize,
            Variable::Dynamic(_) => 0,
        };
        if at != 0 {
            if let Some((_, _, text)) = self.seen.iter().find(|(p, _, _)| *p == at) {
                return Frozen::Json(text.clone());
            }
        }
        if let Variable::Array(a) = v {
            let len = a.borrow().len();
            let same = self.seen.iter().find(|(_, x, _)| match x {
                Variable::Array(b) => {
                    b.borrow().len() == len && same_list(&b.borrow(), &a.borrow())
                }
                _ => false,
            });
            if let Some((_, _, text)) = same {
                let text = text.clone();
                self.seen.push((at, v.clone(), text.clone()));
                return Frozen::Json(text);
            }
        }
        let text = SCRATCH.with(|scratch| {
            let mut buffer = std::mem::take(&mut *scratch.borrow_mut());
            buffer.clear();
            let text: Arc<str> = match serde_json::to_writer(&mut buffer, &Value(v)) {
                Ok(()) => Arc::from(std::str::from_utf8(&buffer).unwrap_or("null")),
                Err(_) => Arc::from("null"),
            };
            if buffer.capacity() <= 1 << 16 {
                *scratch.borrow_mut() = buffer;
            }
            text
        });
        if at != 0 {
            self.seen.push((at, v.clone(), text.clone()));
        }
        Frozen::Json(text)
    }
}

enum Fact {
    Expression {
        property: Arc<str>,
        value: Frozen,
    },
    Match {
        arms: Vec<(Arc<str>, bool)>,
        matched: Option<Arc<str>>,
        value: Frozen,
    },
    Assertion {
        conditions: Vec<(Arc<str>, bool)>,
        result: bool,
    },
    Whole(Frozen),
    Table {
        rows: Vec<u32>,
        input_pass: String,
        evaluations: Option<Frozen>,
    },
}

struct Exec {
    block: Arc<str>,
    policy: Option<Arc<str>>,
    member: Option<Arc<str>>,
    fact: Fact,
}

impl Exec {
    fn of(e: PolicyExecution, freezer: &mut Freezer) -> Exec {
        let fact = match e.fact {
            PolicyFact::Expression { property, value } => Fact::Expression {
                property,
                value: freezer.freeze(&value),
            },
            PolicyFact::Match {
                arms,
                matched,
                value,
            } => Fact::Match {
                arms,
                matched,
                value: freezer.freeze(&value),
            },
            PolicyFact::Assertion { conditions, result } => Fact::Assertion { conditions, result },
            PolicyFact::Table {
                rows,
                evaluations,
                input_pass: None,
            } => {
                let mut whole = VariableMap::with_capacity(3);
                whole.insert("kind".into(), Variable::String("decisionTable".into()));
                whole.insert(
                    "matchedRows".into(),
                    Variable::from_array(
                        rows.iter()
                            .map(|r| Variable::Number(Decimal::from(*r)))
                            .collect(),
                    ),
                );
                whole.insert("evaluations".into(), evaluations_of(&evaluations));
                Fact::Whole(freezer.freeze(&Variable::from_object(whole)))
            }
            PolicyFact::Table {
                rows,
                evaluations,
                input_pass: Some(input_pass),
            } => {
                let default = rows.is_empty()
                    || (rows.len() == 1 && evaluations.first().is_some_and(|e| e.is_empty()));
                let evaluations = (evaluations.len() != rows.len() || !default)
                    .then(|| freezer.freeze(&evaluations_of(&evaluations)));
                Fact::Table {
                    rows,
                    input_pass,
                    evaluations,
                }
            }
        };
        Exec {
            block: e.block,
            policy: e.policy,
            member: e.member,
            fact,
        }
    }

    fn kind(&self) -> &'static str {
        match self.fact {
            Fact::Expression { .. } => "expression",
            Fact::Match { .. } => "match",
            Fact::Assertion { .. } => "assertion",
            Fact::Whole(_) | Fact::Table { .. } => "decisionTable",
        }
    }
}

struct Details<'a>(&'a Exec, bool);

impl Serialize for Details<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut map = serializer.serialize_map(None)?;
        match &e.fact {
            Fact::Expression { property, value } => {
                map.serialize_entry("property", &**property)?;
                map.serialize_entry("value", value)?;
            }
            Fact::Match {
                arms,
                matched,
                value,
            } => {
                if let Some(ids) = ids(&e.block, arms) {
                    map.serialize_entry("ids", &ValSer(&ids))?;
                }
                map.serialize_entry("value", value)?;
                let first = arms.iter().find(|(_, r)| *r).map(|(id, _)| id);
                if first.map(|f| &**f) != matched.as_deref() {
                    map.serialize_entry("matchedArm", &matched.as_deref())?;
                }
            }
            Fact::Assertion { conditions, result } => {
                if let Some(ids) = ids(&e.block, conditions) {
                    map.serialize_entry("ids", &ValSer(&ids))?;
                }
                map.serialize_entry("value", result)?;
            }
            Fact::Whole(trace) => map.serialize_entry("trace", trace)?,
            Fact::Table {
                input_pass,
                evaluations,
                ..
            } => {
                if !self.1 {
                    let mut extras = super::value::Obj::with_capacity(1);
                    extras.insert(std::rc::Rc::from("inputPass"), Val::str(input_pass));
                    map.serialize_entry("extras", &ValSer(&Val::obj(extras)))?;
                }
                if let Some(evaluations) = evaluations {
                    map.serialize_entry("evaluations", evaluations)?;
                }
            }
        }
        map.end()
    }
}

fn has_details(e: &Exec, shaped: bool) -> bool {
    match &e.fact {
        Fact::Table { evaluations, .. } => !shaped || evaluations.is_some(),
        _ => true,
    }
}

struct Record<'a>(&'a Exec, bool);

impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("block", &*e.block)?;
        map.serialize_entry("kind", e.kind())?;
        if let Some(member) = &e.member {
            map.serialize_entry("member", &Ptr(member))?;
        }
        if let Some(policy) = &e.policy {
            map.serialize_entry("policy", &**policy)?;
        }
        let mut shaped = false;
        match &e.fact {
            Fact::Match { arms, .. } => map.serialize_entry("arms", &Bools(arms))?,
            Fact::Assertion { conditions, .. } => {
                map.serialize_entry("conditions", &Bools(conditions))?
            }
            Fact::Table {
                rows, input_pass, ..
            } => {
                let list = Val::arr(rows.iter().map(|r| Val::int(*r as i64)).collect());
                map.serialize_entry("rows", &ValSer(&pack(&list)))?;
                if let Some(shape) = super::policy::infer_shape(Some(&Val::str(input_pass))) {
                    shaped = true;
                    map.serialize_entry("ruleCount", &shape.rules)?;
                    map.serialize_entry("inputCount", &shape.inputs)?;
                    if !shape.fail.is_empty() {
                        let mut cells = super::value::Obj::with_capacity(shape.fail.len());
                        for (r, ins) in shape.fail {
                            cells.insert(
                                std::rc::Rc::from(r.to_string().as_str()),
                                Val::arr(ins.into_iter().map(|i| Val::int(i as i64)).collect()),
                            );
                        }
                        map.serialize_entry("failedCells", &ValSer(&pack(&Val::obj(cells))))?;
                    }
                }
            }
            _ => {}
        }
        if self.1 {
            map.serialize_entry("writesAbsent", &true)?;
            map.serialize_entry("readsAbsent", &true)?;
            map.serialize_entry("operandsAbsent", &true)?;
        }
        if has_details(e, shaped) {
            map.serialize_entry("details", &Details(e, shaped))?;
        }
        map.end()
    }
}

struct Executions<'a>(&'a PolicyDocument);

impl Serialize for Executions<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0.runs {
            Some(runs) => {
                let mut seq = serializer.serialize_seq(Some(runs.len()))?;
                for run in runs {
                    seq.serialize_element(&Frozen::Json(run.clone()))?;
                }
                seq.end()
            }
            None => {
                let markers = !self.0.absent_default;
                let mut seq = serializer.serialize_seq(Some(self.0.executions.len()))?;
                for e in &self.0.executions {
                    seq.serialize_element(&Record(e, markers))?;
                }
                seq.end()
            }
        }
    }
}

struct Properties<'a>(&'a [(Arc<str>, Frozen)]);

impl Serialize for Properties<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Set<'a>(&'a [(Arc<str>, Frozen)]);
        impl Serialize for Set<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for (k, v) in self.0 {
                    map.serialize_entry(&Ptr(k), v)?;
                }
                map.end()
            }
        }
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("set", &Set(self.0))?;
        map.end()
    }
}

pub struct PolicyDocument {
    engine_version: Option<Arc<str>>,
    executions: Vec<Exec>,
    properties: Option<Vec<(Arc<str>, Frozen)>>,
    runs: Option<Vec<Arc<str>>>,
    absent_default: bool,
    evaluation: Option<super::Evaluation>,
}

impl std::fmt::Debug for PolicyDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyDocument")
            .field("executions", &self.executions.len())
            .finish()
    }
}

impl PolicyDocument {
    pub fn accepts(request: &Variable) -> bool {
        !super::native_policy::dotted(request)
    }

    pub fn new(level: PolicyLevel, request: &Variable) -> Result<Self, &'static str> {
        if !level.simple() {
            return Err("policy trace has writes, reads or operands");
        }
        let absent_default = Absent::defaults(level.executions.len(), level.executions.len());
        let _packing = Packing::start();
        let runs = match level.executions.iter().any(|e| e.member.is_some()) {
            true => Some(
                super::native_policy::runs(&level, request, !absent_default)?
                    .iter()
                    .map(|run| {
                        Arc::from(
                            serde_json::to_string(&ValSer(run))
                                .unwrap_or_else(|_| "null".to_string())
                                .as_str(),
                        )
                    })
                    .collect(),
            ),
            false => None,
        };
        let mut freezer = Freezer::default();
        let properties = level.properties.map(|props| {
            props
                .into_iter()
                .map(|(k, v)| (k, freezer.freeze(&v)))
                .collect::<Vec<_>>()
        });
        let executions = match runs {
            Some(_) => Vec::new(),
            None => level
                .executions
                .into_iter()
                .map(|e| Exec::of(e, &mut freezer))
                .collect(),
        };
        Ok(Self {
            engine_version: level.engine_version,
            executions,
            properties,
            runs,
            absent_default,
            evaluation: None,
        })
    }

    pub fn with_evaluation(mut self, evaluation: Option<super::Evaluation>) -> Self {
        self.evaluation = evaluation;
        self
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "null".to_string())
    }
}

struct Packing;

impl Packing {
    fn start() -> Self {
        PACKED.with(|p| p.borrow_mut().clear());
        Packing
    }
}

impl Drop for Packing {
    fn drop(&mut self) {
        PACKED.with(|p| p.borrow_mut().clear());
    }
}

impl Serialize for PolicyDocument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("version", &1)?;
        map.serialize_entry("kind", "policy")?;
        if let Some(engine) = &self.engine_version {
            map.serialize_entry("engine", &**engine)?;
        }
        if let Some(e) = &self.evaluation {
            if let Some(at) = &e.evaluated_at {
                map.serialize_entry("evaluatedAt", &**at)?;
            }
            if let Some(zone) = &e.timezone {
                map.serialize_entry("timezone", &**zone)?;
            }
            if let Some(seed) = e.seed {
                map.serialize_entry("seed", &seed)?;
            }
        }
        if self.absent_default {
            map.serialize_entry("requires", &[super::policy::ABSENT_DEFAULT])?;
            map.serialize_entry("absent", &Absent::NAMES)?;
        }
        map.serialize_entry("executions", &Executions(self))?;
        match &self.properties {
            None => {
                let mut absent = super::value::Obj::with_capacity(1);
                absent.insert(std::rc::Rc::from("absent"), Val::Bool(true));
                map.serialize_entry("properties", &ValSer(&Val::obj(absent)))?;
            }
            Some(props) if props.is_empty() => {}
            Some(props) => map.serialize_entry("properties", &Properties(props))?,
        }
        map.end()
    }
}
