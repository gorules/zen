use super::collect::{Fact, Hit, Level, NodeT, RuleT};
use super::convert::from_variable;
use super::core::{
    digits, dot_to_ptr, dotted, js_number_index, literal, pack, ref_out, to_ptr, Seg,
};
use super::graph::{finish_graph, plain_path, ErrorInfo};
use super::loops::{nest, rec_rank, EncRec, EncTrace, NestKind, Shapes};
use super::value::{
    js_num_len, json_len, num_eq, utf16_len, write_json, write_num, write_str, Key, Obj, Val,
};
use super::value::{FastMap as HashMap, FastSet as HashSet};
use smallvec::SmallVec;
use std::rc::Rc;
use std::sync::Arc;
use zen_types::decision::{
    DecisionNode, DecisionNodeKind, TransformAttributes, TransformExecutionMode,
};
use zen_types::symbol::Symbol;
use zen_types::variable::{Variable, VariableMap};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Input,
    Output,
    Table,
    Expression,
    Switch,
    Function,
    Decision,
    Custom,
}

impl Kind {
    pub(crate) fn of(node: &DecisionNode) -> (Kind, Option<&TransformAttributes>) {
        match &node.kind {
            DecisionNodeKind::InputNode { .. } => (Kind::Input, None),
            DecisionNodeKind::OutputNode { .. } => (Kind::Output, None),
            DecisionNodeKind::FunctionNode { .. } => (Kind::Function, None),
            DecisionNodeKind::DecisionNode { content } => {
                (Kind::Decision, Some(&content.transform_attributes))
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                (Kind::Table, Some(&content.transform_attributes))
            }
            DecisionNodeKind::ExpressionNode { content } => {
                (Kind::Expression, Some(&content.transform_attributes))
            }
            DecisionNodeKind::SwitchNode { .. } => (Kind::Switch, None),
            DecisionNodeKind::CustomNode { .. } => (Kind::Custom, None),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Input => "input",
            Kind::Output => "output",
            Kind::Table => "table",
            Kind::Expression => "expression",
            Kind::Switch => "switch",
            Kind::Function => "function",
            Kind::Decision => "decision",
            Kind::Custom => "custom",
        }
    }
}

pub(crate) type Miss = &'static str;

fn ptr_of(v: &Variable) -> usize {
    match v {
        Variable::Array(a) => Rc::as_ptr(a) as *const u8 as usize,
        Variable::Object(o) => Rc::as_ptr(o) as *const u8 as usize,
        _ => 0,
    }
}

fn is_null(v: &Variable) -> bool {
    matches!(v, Variable::Null)
}

fn is_container(v: &Variable) -> bool {
    matches!(v, Variable::Array(_) | Variable::Object(_))
}

type Entries<'a> = SmallVec<[(&'a str, &'a Variable); 8]>;

fn ordered(m: &VariableMap) -> Entries<'_> {
    let mut out: Entries = m.iter().map(|(k, v)| (&**k, v)).collect();
    if m.shape().is_none() {
        out.sort_unstable_by(|a, b| a.0.cmp(b.0));
    }
    out
}

pub(crate) fn veq(a: &Variable, b: &Variable) -> bool {
    match (a, b) {
        (Variable::Null, Variable::Null) => true,
        (Variable::Bool(x), Variable::Bool(y)) => x == y,
        (Variable::Number(x), Variable::Number(y)) => num_eq(x, y),
        (Variable::String(x), Variable::String(y)) => x == y,
        (Variable::Array(x), Variable::Array(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| veq(p, q))
        }
        (Variable::Object(x), Variable::Object(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            if let (Some((sx, vx)), Some((sy, vy))) = (x.slots(), y.slots()) {
                if std::ptr::eq(sx, sy) {
                    return vx.iter().zip(vy.iter()).all(|(p, q)| veq(p, q));
                }
            }
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get_str(k).is_some_and(|w| veq(v, w)))
        }
        (Variable::Dynamic(_), _) | (_, Variable::Dynamic(_)) => {
            super::value::eq(&from_variable(a), &from_variable(b))
        }
        _ => false,
    }
}

pub(crate) fn same_prim(a: &Variable, b: &Variable) -> bool {
    match (a, b) {
        (Variable::Array(x), Variable::Array(y)) => Rc::ptr_eq(x, y),
        (Variable::Object(x), Variable::Object(y)) => Rc::ptr_eq(x, y),
        (Variable::Null, Variable::Null) => true,
        (Variable::Bool(x), Variable::Bool(y)) => x == y,
        (Variable::Number(x), Variable::Number(y)) => x == y,
        (Variable::String(x), Variable::String(y)) => x == y,
        _ => false,
    }
}

fn falsy(v: &Variable) -> bool {
    match v {
        Variable::Null => true,
        Variable::Bool(b) => !b,
        Variable::Number(n) => n.is_zero(),
        Variable::String(s) => s.is_empty(),
        _ => false,
    }
}

fn canonical_index(k: &str) -> Option<usize> {
    let b = k.as_bytes();
    if b.is_empty() || !b.iter().all(|c| c.is_ascii_digit()) || (b[0] == b'0' && b.len() > 1) {
        return None;
    }
    k.parse().ok()
}

fn child(v: &Variable, s: &Seg) -> Option<Variable> {
    match v {
        Variable::Array(a) => {
            let i = match s {
                Seg::I(i) => *i,
                Seg::K(k) => canonical_index(k)?,
            };
            a.borrow().get(i).cloned()
        }
        Variable::Object(o) => match s {
            Seg::K(k) => o.borrow().get_str(k).cloned(),
            Seg::I(i) => o.borrow().get_str(&i.to_string()).cloned(),
        },
        _ => None,
    }
}

pub(crate) fn vpath(v: &Variable, path: &[Seg]) -> Option<Variable> {
    let mut cur = v.clone();
    for s in path {
        cur = child(&cur, s)?;
    }
    Some(cur)
}

fn dotted_get(v: Option<&Variable>, f: &str) -> Option<Variable> {
    let mut cur = v?.clone();
    for s in f.split('.') {
        cur = match &cur {
            Variable::Array(a) => a.borrow().get(canonical_index(s)?).cloned()?,
            Variable::Object(o) => o.borrow().get_str(s).cloned()?,
            _ => return None,
        };
    }
    Some(cur)
}

fn has_dollar_map(m: &VariableMap) -> bool {
    m.iter().any(|(k, _)| k.starts_with('$'))
}

fn convert_escaped(v: &Variable) -> Val {
    match v {
        Variable::Object(o) if has_dollar_map(&o.borrow()) => literal(from_variable(v)),
        Variable::Object(o) => {
            let o = o.borrow();
            let mut out = Obj::with_capacity(o.len());
            for (k, x) in ordered(&o) {
                out.insert(Rc::from(k), convert_escaped(x));
            }
            Val::obj(out)
        }
        Variable::Array(a) => Val::arr(a.borrow().iter().map(convert_escaped).collect()),
        _ => from_variable(v),
    }
}

fn fnv(s: &str) -> u32 {
    let mut x: u32 = 2166136261;
    for c in s.bytes() {
        x = (x ^ c as u32).wrapping_mul(16777619);
    }
    x
}

fn mix(a: u32, b: u32) -> u32 {
    let s = b
        .wrapping_add(0x9e3779b9)
        .wrapping_add(a << 6)
        .wrapping_add(a >> 2);
    (a ^ s).wrapping_mul(2654435761)
}

#[derive(Default)]
struct Measure {
    sizes: HashMap<usize, (Variable, usize)>,
    hashes: HashMap<usize, (Variable, u32)>,
}

const SIZE_MEMO: usize = 128;

impl Measure {
    fn size(&mut self, v: &Variable) -> usize {
        match v {
            Variable::Null => 4,
            Variable::Bool(_) => 5,
            Variable::String(s) => utf16_len(s) + 2,
            Variable::Number(d) => js_num_len(d),
            Variable::Array(a) => {
                let p = ptr_of(v);
                if let Some((_, n)) = self.sizes.get(&p) {
                    return *n;
                }
                let mut n = 2;
                for e in a.borrow().iter() {
                    n += self.size(e) + 1;
                }
                if n >= SIZE_MEMO {
                    self.sizes.insert(p, (v.clone(), n));
                }
                n
            }
            Variable::Object(o) => {
                let p = ptr_of(v);
                if let Some((_, n)) = self.sizes.get(&p) {
                    return *n;
                }
                let mut n = 2;
                for (k, e) in o.borrow().iter() {
                    n += self.size(e) + utf16_len(k) + 4;
                }
                if n >= SIZE_MEMO {
                    self.sizes.insert(p, (v.clone(), n));
                }
                n
            }
            Variable::Dynamic(_) => super::core::size(&from_variable(v)),
        }
    }

    fn at_least(&mut self, v: &Variable, x: usize) -> bool {
        if x <= 2 {
            return true;
        }
        if let Some((_, n)) = self.sizes.get(&ptr_of(v)) {
            return *n >= x;
        }
        let mut n = 2;
        match v {
            Variable::Array(a) => {
                for e in a.borrow().iter() {
                    n += self.size(e) + 1;
                    if n >= x {
                        return true;
                    }
                }
            }
            Variable::Object(o) => {
                for (k, e) in o.borrow().iter() {
                    n += self.size(e) + utf16_len(k) + 4;
                    if n >= x {
                        return true;
                    }
                }
            }
            _ => return self.size(v) >= x,
        }
        false
    }

    fn hash(&mut self, v: &Variable) -> u32 {
        match v {
            Variable::Null => 1,
            Variable::Bool(b) => {
                if *b {
                    5
                } else {
                    7
                }
            }
            Variable::String(s) => mix(fnv(s), 2),
            Variable::Number(d) => {
                if d.is_zero() {
                    return mix(0, 3);
                }
                if d.scale() == 0 {
                    let m = d.mantissa();
                    return mix(
                        mix(mix(m as u32, (m >> 32) as u32), mix((m >> 64) as u32, 0)),
                        3,
                    );
                }
                let n = d.normalize();
                let h = mix(
                    mix(n.mantissa() as u32, (n.mantissa() >> 32) as u32),
                    mix((n.mantissa() >> 64) as u32, n.scale()),
                );
                mix(h, 3)
            }
            Variable::Array(a) => {
                let p = ptr_of(v);
                if let Some((_, h)) = self.hashes.get(&p) {
                    return *h;
                }
                let mut h: u32 = 11;
                for e in a.borrow().iter() {
                    h = mix(h, self.hash(e));
                }
                self.hashes.insert(p, (v.clone(), h));
                h
            }
            Variable::Object(o) => {
                let p = ptr_of(v);
                if let Some((_, h)) = self.hashes.get(&p) {
                    return *h;
                }
                let mut acc: u32 = 0;
                for (k, e) in o.borrow().iter() {
                    acc = acc.wrapping_add(mix(fnv(k), self.hash(e)));
                }
                let h = mix(13, acc);
                self.hashes.insert(p, (v.clone(), h));
                h
            }
            Variable::Dynamic(_) => mix(fnv(&from_variable(v).to_json()), 17),
        }
    }
}

const DIRECT: usize = 64;
const REF_LOOKUP: usize = 256;

#[derive(Default)]
struct Group {
    members: Vec<(u32, Variable)>,
    hashed: Option<HashMap<u32, (u32, Variable)>>,
}

impl Group {
    fn add(&mut self, m: &mut Measure, spot: u32, v: &Variable) {
        if let Some(h) = self.hashed.as_mut() {
            h.entry(m.hash(v)).or_insert((spot, v.clone()));
        }
        self.members.push((spot, v.clone()));
    }

    fn find(&mut self, m: &mut Measure, x: &Variable) -> Option<u32> {
        if self.hashed.is_none() && self.members.len() <= DIRECT {
            return self
                .members
                .iter()
                .find(|(_, c)| veq(c, x))
                .map(|(s, _)| *s);
        }
        if self.hashed.is_none() {
            let mut map: HashMap<u32, (u32, Variable)> = HashMap::default();
            for (spot, c) in &self.members {
                map.entry(m.hash(c)).or_insert((*spot, c.clone()));
            }
            self.hashed = Some(map);
        }
        let h = m.hash(x);
        let (spot, c) = self.hashed.as_ref()?.get(&h)?;
        veq(c, x).then_some(*spot)
    }
}

fn group_key(v: &Variable) -> Option<(bool, usize)> {
    match v {
        Variable::Array(a) => Some((true, a.borrow().len())),
        Variable::Object(o) => Some((false, o.borrow().len())),
        _ => None,
    }
}

#[derive(Default)]
struct Groups(HashMap<(bool, usize), Group>);

impl Groups {
    fn add(&mut self, m: &mut Measure, spot: u32, v: &Variable) {
        if let Some(k) = group_key(v) {
            self.0.entry(k).or_default().add(m, spot, v);
        }
    }

    fn find(&mut self, m: &mut Measure, x: &Variable) -> Option<u32> {
        self.0.get_mut(&group_key(x)?)?.find(m, x)
    }
}

const SRC_REQUEST: u8 = 0;
const SRC_NODES: u8 = 1;
const NO_SPOT: u32 = u32::MAX;

type Path = SmallVec<[PSeg; 4]>;

#[derive(Clone, PartialEq)]
enum PSeg {
    K(Symbol),
    I(usize),
    Root(u8, Option<Key>),
}

struct Spot {
    parent: u32,
    seg: PSeg,
}

#[derive(Default)]
struct Spots(Vec<Spot>);

impl Spots {
    fn push(&mut self, parent: u32, seg: PSeg) -> u32 {
        self.0.push(Spot { parent, seg });
        (self.0.len() - 1) as u32
    }

    fn path(&self, mut at: u32) -> (u8, Vec<Seg>) {
        let mut segs: Vec<Seg> = Vec::new();
        let mut src = SRC_REQUEST;
        while at != NO_SPOT {
            let spot = &self.0[at as usize];
            match &spot.seg {
                PSeg::K(k) => segs.push(Seg::K(Rc::from(&**k))),
                PSeg::I(i) => segs.push(Seg::I(*i)),
                PSeg::Root(s, id) => {
                    src = *s;
                    if let Some(id) = id {
                        segs.push(Seg::K(Rc::from("output")));
                        segs.push(Seg::K(id.clone()));
                    }
                }
            }
            at = spot.parent;
        }
        segs.reverse();
        (src, segs)
    }
}

#[derive(Default)]
struct Keys(HashSet<Key>);

fn text_val(s: &'static str) -> Val {
    thread_local! {
        static TEXTS: std::cell::RefCell<Vec<(&'static str, Val)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    TEXTS.with(|t| {
        let mut t = t.borrow_mut();
        if let Some((_, v)) = t.iter().find(|(l, _)| std::ptr::eq(*l, s) || *l == s) {
            return v.clone();
        }
        let v = Val::str(s);
        t.push((s, v.clone()));
        v
    })
}

impl Keys {
    fn get(&mut self, k: &str) -> Key {
        if let Some(x) = self.0.get(k) {
            return x.clone();
        }
        let x: Key = Rc::from(k);
        self.0.insert(x.clone());
        x
    }
}

struct Refs {
    request: Variable,
    outputs: Vec<(Key, Variable)>,
    spots: Spots,
    groups: Groups,
    pending: Vec<(Variable, u32)>,
    walked: HashSet<usize>,
    keys: Keys,
}

impl Refs {
    fn new(request: &Variable) -> Self {
        let mut spots = Spots::default();
        let root = spots.push(NO_SPOT, PSeg::Root(SRC_REQUEST, None));
        Self {
            request: request.clone(),
            outputs: Vec::new(),
            spots,
            groups: Groups::default(),
            pending: vec![(request.clone(), root)],
            walked: HashSet::default(),
            keys: Keys::default(),
        }
    }

    fn add_node(&mut self, id: &Key, output: &Variable) {
        if self.outputs.iter().any(|(k, _)| k == id) {
            return;
        }
        self.outputs.push((id.clone(), output.clone()));
        if is_container(output) {
            let root = self
                .spots
                .push(NO_SPOT, PSeg::Root(SRC_NODES, Some(id.clone())));
            self.pending.push((output.clone(), root));
        }
    }

    fn build(&mut self, m: &mut Measure) {
        for (doc, spot) in std::mem::take(&mut self.pending) {
            self.walk(m, &doc, spot);
        }
    }

    fn walk(&mut self, m: &mut Measure, v: &Variable, spot: u32) {
        if !is_container(v) || !self.walked.insert(ptr_of(v)) {
            return;
        }
        self.groups.add(m, spot, v);
        match v {
            Variable::Array(a) => {
                for (i, e) in a.borrow().iter().enumerate() {
                    if is_container(e) {
                        let at = self.spots.push(spot, PSeg::I(i));
                        self.walk(m, e, at);
                    }
                }
            }
            Variable::Object(o) => {
                let o = o.borrow();
                let mut visit = |k: &Symbol, e: &Variable, refs: &mut Self| {
                    if is_container(e) {
                        let at = refs.spots.push(spot, PSeg::K(k.clone()));
                        refs.walk(m, e, at);
                    }
                };
                match o.shape() {
                    Some(_) => o.iter().for_each(|(k, e)| visit(k, e, self)),
                    None => {
                        let mut all: Vec<(&Symbol, &Variable)> = o.iter().collect();
                        all.sort_unstable_by(|a, b| a.0.cmp(b.0));
                        all.into_iter().for_each(|(k, e)| visit(k, e, self));
                    }
                }
            }
            _ => {}
        }
    }

    fn whole(&self, v: &Variable) -> bool {
        veq(&self.request, v) || self.outputs.iter().any(|(_, o)| veq(o, v))
    }

    fn encode(&mut self, m: &mut Measure, v: &Variable) -> Val {
        let mut mine = Groups::default();
        let mut local = Spots::default();
        let root = local.push(NO_SPOT, PSeg::Root(SRC_REQUEST, None));
        self.encode_walk(m, v, root, &mut local, &mut mine)
    }

    fn encode_walk(
        &mut self,
        m: &mut Measure,
        x: &Variable,
        spot: u32,
        local: &mut Spots,
        mine: &mut Groups,
    ) -> Val {
        if !is_container(x) {
            return match x {
                Variable::String(s) => Val::Str(Rc::from(&**s)),
                _ => from_variable(x),
            };
        }
        if let Variable::Object(o) = x {
            if has_dollar_map(&o.borrow()) {
                return literal(from_variable(x));
            }
        }
        let big = m.size(x) > REF_LOOKUP;
        if big {
            if let Some(at) = mine.find(m, x) {
                let (_, p) = local.path(at);
                return ref_out("self", &p, None);
            }
            if !self.pending.is_empty() {
                self.build(m);
            }
            if let Some(at) = self.groups.find(m, x) {
                let (src, p) = self.spots.path(at);
                return ref_out(
                    if src == SRC_REQUEST {
                        "request"
                    } else {
                        "nodes"
                    },
                    &p,
                    None,
                );
            }
        }
        let out = match x {
            Variable::Array(a) => {
                let a = a.borrow();
                let mut out = Vec::with_capacity(a.len());
                for (i, e) in a.iter().enumerate() {
                    let at = match is_container(e) {
                        true => local.push(spot, PSeg::I(i)),
                        false => NO_SPOT,
                    };
                    out.push(self.encode_walk(m, e, at, local, mine));
                }
                Val::arr(out)
            }
            Variable::Object(o) => {
                let o = o.borrow();
                let mut out = Obj::with_capacity(o.len());
                let mut visit = |k: &Symbol, e: &Variable, refs: &mut Self| {
                    let at = match is_container(e) {
                        true => local.push(spot, PSeg::K(k.clone())),
                        false => NO_SPOT,
                    };
                    let ev = refs.encode_walk(m, e, at, local, mine);
                    out.insert(refs.keys.get(k), ev);
                };
                match o.shape() {
                    Some(_) => o.iter().for_each(|(k, e)| visit(k, e, self)),
                    None => {
                        let mut all: Vec<(&Symbol, &Variable)> = o.iter().collect();
                        all.sort_unstable_by(|a, b| a.0.cmp(b.0));
                        all.into_iter().for_each(|(k, e)| visit(k, e, self));
                    }
                }
                Val::obj(out)
            }
            _ => from_variable(x),
        };
        if big {
            mine.add(m, spot, x);
        }
        out
    }
}

enum Edit {
    Set(Path, Variable),
    Del(Path),
    Splice(Path, usize, usize, Vec<Variable>),
}

#[derive(Default)]
struct WPath {
    segs: Vec<PSeg>,
}

impl WPath {
    fn push(&mut self, s: PSeg) {
        self.segs.push(s);
    }

    fn pop(&mut self) {
        self.segs.pop();
    }

    fn cost(&self) -> usize {
        self.segs.iter().map(|s| pseg_len(s) + 1).sum()
    }

    fn path(&self) -> Path {
        self.segs.iter().cloned().collect()
    }
}

fn each_entry(m: &VariableMap, mut f: impl FnMut(&Symbol, &Variable)) {
    match m.shape() {
        Some(_) => m.iter().for_each(|(k, v)| f(k, v)),
        None => {
            let mut all: Vec<(&Symbol, &Variable)> = m.iter().collect();
            all.sort_unstable_by(|a, b| a.0.cmp(b.0));
            all.into_iter().for_each(|(k, v)| f(k, v));
        }
    }
}

fn pseg_len(s: &PSeg) -> usize {
    match s {
        PSeg::K(k) => utf16_len(k),
        PSeg::I(i) => digits(*i),
        PSeg::Root(..) => 0,
    }
}

fn path_cost(path: &[PSeg], base: usize) -> usize {
    base + path.iter().map(|s| pseg_len(s) + 1).sum::<usize>()
}

fn pchild(v: &Variable, s: &PSeg) -> Option<Variable> {
    match s {
        PSeg::K(k) => child(v, &Seg::K(Rc::from(&**k))),
        PSeg::I(i) => child(v, &Seg::I(*i)),
        PSeg::Root(..) => None,
    }
}

fn pchild_fast(v: &Variable, s: &PSeg) -> Option<Variable> {
    match (v, s) {
        (Variable::Object(o), PSeg::K(k)) => o.borrow().get_str(k).cloned(),
        (Variable::Array(a), PSeg::I(i)) => a.borrow().get(*i).cloned(),
        _ => pchild(v, s),
    }
}

fn ppath(v: &Variable, path: &[PSeg]) -> Option<Variable> {
    let mut cur = v.clone();
    for s in path {
        cur = pchild_fast(&cur, s)?;
    }
    Some(cur)
}

fn write_ptr(path: &[PSeg], out: &mut String) {
    for s in path {
        match s {
            PSeg::K(k) => super::core::escape_seg(k, out),
            PSeg::I(i) => {
                out.push('/');
                push_usize(*i, out);
            }
            PSeg::Root(..) => {}
        }
    }
}

fn push_usize(mut n: usize, out: &mut String) {
    let mut buf = [0u8; 20];
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.push_str(std::str::from_utf8(&buf[at..]).unwrap_or("0"));
}

fn ptr_len(path: &[PSeg]) -> usize {
    path.iter()
        .map(|s| match s {
            PSeg::K(k) => 1 + utf16_len(k) + k.bytes().filter(|b| *b == b'~' || *b == b'/').count(),
            PSeg::I(i) => 1 + digits(*i),
            PSeg::Root(..) => 0,
        })
        .sum()
}

fn write_ptr_json(path: &[PSeg], out: &mut String) {
    let plain = path.iter().all(|s| match s {
        PSeg::K(k) => !k
            .bytes()
            .any(|b| b < 0x20 || matches!(b, b'"' | b'\\' | b'~' | b'/')),
        _ => true,
    });
    if !plain {
        write_str(&pptr(path), out);
        return;
    }
    out.push('"');
    write_ptr(path, out);
    out.push('"');
}

fn pptr(path: &[PSeg]) -> String {
    let mut s = String::new();
    write_ptr(path, &mut s);
    s
}

fn number_of_pseg(s: &PSeg) -> Option<usize> {
    match s {
        PSeg::I(i) => Some(*i),
        PSeg::K(k) => js_number_index(k),
        PSeg::Root(..) => None,
    }
}

struct Coder {
    m: Measure,
    refs: Refs,
    path: WPath,
}

impl Coder {
    fn size(&mut self, v: &Variable) -> usize {
        self.m.size(v)
    }

    fn floor(&mut self, v: &Variable) -> usize {
        match v {
            Variable::Object(o) => {
                let o = o.borrow();
                let mut n = 1;
                for (k, x) in o.iter() {
                    n += utf16_len(k) + 3 + self.size(x).min(15).saturating_sub(1);
                }
                n
            }
            Variable::Array(a) => {
                let a = a.borrow();
                let mut n = 1;
                for x in a.iter() {
                    n += self.size(x).min(15);
                }
                n
            }
            _ => self.size(v).saturating_sub(1),
        }
    }

    fn edit_cost(&mut self, e: &Edit) -> usize {
        match e {
            Edit::Set(p, v) => path_cost(p, 8) + self.size(v),
            Edit::Del(p) => path_cost(p, 8),
            Edit::Splice(p, _, _, ins) => {
                let mut n = path_cost(p, 8) + 8;
                for x in ins {
                    n += self.size(x) + 1;
                }
                n
            }
        }
    }

    fn writes(
        &mut self,
        a: Option<&Variable>,
        b: &Variable,
        p: &mut WPath,
        out: &mut Vec<Edit>,
    ) -> usize {
        if let Some(a) = a {
            if same_prim(a, b) {
                return 0;
            }
        }
        let pc = p.cost() + 10;
        if a.is_none() {
            return self.fresh_edits(b, p, out);
        }
        match (a, b) {
            (None | Some(Variable::Object(_)), Variable::Object(y)) => {
                let ao = match a {
                    Some(Variable::Object(x)) => Some(x.borrow()),
                    _ => None,
                };
                let y = y.borrow();
                if !y.is_empty() || ao.as_ref().is_some_and(|x| !x.is_empty()) {
                    let mut sub = Vec::new();
                    let mut lit = 0;
                    let prefix = match (ao.as_ref().and_then(|x| x.slots()), y.slots()) {
                        (Some((xs, xv)), Some((ys, yv))) if xv.len() <= yv.len() => {
                            let (xk, yk) = (xs.keys(), ys.keys());
                            (std::ptr::eq(xs, ys) || xk.iter().zip(yk).all(|(p, q)| p == q))
                                .then_some((xv, ys, yv))
                        }
                        _ => None,
                    };
                    if let Some((xv, ys, yv)) = prefix {
                        for (i, (k, v)) in ys.keys().iter().zip(yv).enumerate() {
                            p.push(PSeg::K(k.clone()));
                            lit += self.writes(xv.get(i), v, p, &mut sub);
                            p.pop();
                        }
                    } else {
                        if let Some(x) = &ao {
                            each_entry(x, |k, _| {
                                if !y.contains_key_str(k) {
                                    p.push(PSeg::K(k.clone()));
                                    sub.push(Edit::Del(p.path()));
                                    p.pop();
                                    lit += pc + utf16_len(k);
                                }
                            });
                        }
                        each_entry(&y, |k, v| {
                            p.push(PSeg::K(k.clone()));
                            let prev = ao.as_ref().and_then(|x| x.get_str(k));
                            lit += self.writes(prev, v, p, &mut sub);
                            p.pop();
                        });
                    }
                    if sub.is_empty() {
                        return 0;
                    }
                    let floor = 2 + y.iter().map(|(k, _)| utf16_len(k) + 5).sum::<usize>();
                    drop(y);
                    if lit <= pc + floor || self.m.at_least(b, lit.saturating_sub(pc)) {
                        out.extend(sub);
                        return lit;
                    }
                } else if ao.is_some() {
                    return 0;
                }
            }
            (Some(Variable::Array(x)), Variable::Array(y))
                if x.borrow().len() == y.borrow().len() =>
            {
                let (x, y) = (x.borrow(), y.borrow());
                let mut sub = Vec::new();
                let mut lit = 0;
                for i in 0..y.len() {
                    p.push(PSeg::I(i));
                    lit += self.writes(Some(&x[i]), &y[i], p, &mut sub);
                    p.pop();
                }
                if sub.is_empty() {
                    return 0;
                }
                let floor = 2 + 2 * y.len();
                drop(y);
                if lit <= pc + floor || self.m.at_least(b, lit.saturating_sub(pc)) {
                    out.extend(sub);
                    return lit;
                }
            }
            (Some(a), _) if veq(a, b) => return 0,
            _ => {}
        }
        let s = self.size(b);
        out.push(Edit::Set(p.path(), b.clone()));
        pc + s
    }

    fn fresh(&mut self, b: &Variable, cost: usize) -> (usize, bool) {
        let whole = cost + 10 + self.size(b);
        let Variable::Object(y) = b else {
            return (whole, true);
        };
        let y = y.borrow();
        if y.is_empty() {
            return (whole, true);
        }
        let mut lit = 0;
        for (k, v) in y.iter() {
            lit += self.fresh(v, cost + utf16_len(k) + 1).0;
        }
        match lit <= whole {
            true => (lit, false),
            false => (whole, true),
        }
    }

    fn fresh_edits(&mut self, b: &Variable, p: &mut WPath, out: &mut Vec<Edit>) -> usize {
        let (cost, whole) = self.fresh(b, p.cost());
        if whole {
            out.push(Edit::Set(p.path(), b.clone()));
            return cost;
        }
        if let Variable::Object(y) = b {
            each_entry(&y.borrow(), |k, v| {
                p.push(PSeg::K(k.clone()));
                self.fresh_edits(v, p, out);
                p.pop();
            });
        }
        cost
    }

    fn choose(
        &mut self,
        sub: Vec<Edit>,
        cost: usize,
        b: &Variable,
        p: &mut WPath,
        out: &mut Vec<Edit>,
    ) -> usize {
        let whole = p.cost() + 8 + self.size(b);
        if whole < cost {
            out.push(Edit::Set(p.path(), b.clone()));
            return whole;
        }
        out.extend(sub);
        cost
    }

    fn diff(&mut self, a: &Variable, b: &Variable, p: &mut WPath, out: &mut Vec<Edit>) -> usize {
        if same_prim(a, b) {
            return 0;
        }
        match (a, b) {
            (Variable::Object(x), Variable::Object(y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                let mut sub = Vec::new();
                let mut cost = 0;
                each_entry(&x, |k, _| {
                    if !y.contains_key_str(k) {
                        p.push(PSeg::K(k.clone()));
                        let e = Edit::Del(p.path());
                        p.pop();
                        cost += self.edit_cost(&e);
                        sub.push(e);
                    }
                });
                each_entry(&y, |k, v| {
                    p.push(PSeg::K(k.clone()));
                    match x.get_str(k) {
                        None => {
                            let e = Edit::Set(p.path(), v.clone());
                            cost += self.edit_cost(&e);
                            sub.push(e);
                        }
                        Some(av) => cost += self.diff(av, v, p, &mut sub),
                    }
                    p.pop();
                });
                if sub.is_empty() {
                    return 0;
                }
                drop((x, y));
                self.choose(sub, cost, b, p, out)
            }
            (Variable::Array(xa), Variable::Array(ya)) => {
                let (x, y) = (xa.borrow(), ya.borrow());
                if x.len() == y.len() {
                    let mut sub = Vec::new();
                    let mut cost = 0;
                    for i in 0..x.len() {
                        p.push(PSeg::I(i));
                        cost += self.diff(&x[i], &y[i], p, &mut sub);
                        p.pop();
                    }
                    if sub.is_empty() {
                        return 0;
                    }
                    drop((x, y));
                    return self.choose(sub, cost, b, p, out);
                }
                let m = x.len().min(y.len());
                let mut pre = 0;
                while pre < m && veq(&x[pre], &y[pre]) {
                    pre += 1;
                }
                let mut suf = 0;
                while suf < m - pre && veq(&x[x.len() - 1 - suf], &y[y.len() - 1 - suf]) {
                    suf += 1;
                }
                let here = p.path();
                let opt_a = vec![Edit::Splice(
                    here.clone(),
                    pre,
                    x.len() - pre - suf,
                    y[pre..y.len() - suf].to_vec(),
                )];
                let cost_a = self.edit_cost(&opt_a[0]);
                let mut opt_b = Vec::new();
                let mut cost_b = 0;
                for i in pre..m {
                    p.push(PSeg::I(i));
                    cost_b += self.diff(&x[i], &y[i], p, &mut opt_b);
                    p.pop();
                }
                let tail = Edit::Splice(here, m, x.len() - m, y[m..].to_vec());
                cost_b += self.edit_cost(&tail);
                opt_b.push(tail);
                drop((x, y));
                if cost_a <= cost_b {
                    self.choose(opt_a, cost_a, b, p, out)
                } else {
                    self.choose(opt_b, cost_b, b, p, out)
                }
            }
            _ => {
                if is_container(a) && is_container(b) && veq(a, b) {
                    return 0;
                }
                let e = Edit::Set(p.path(), b.clone());
                let c = self.edit_cost(&e);
                out.push(e);
                c
            }
        }
    }
}

type Splices = Vec<(String, (usize, usize, Vec<Variable>))>;
type Each = Vec<(String, Vec<(String, Vec<Variable>)>)>;

struct Bag {
    tag: Option<Val>,
    delete: Vec<Path>,
    splice: Splices,
    each: Each,
    set: Vec<(Path, Variable)>,
    first: Option<&'static str>,
}

fn set_entry<T>(list: &mut Vec<(String, T)>, k: String, v: T) {
    match list.iter_mut().find(|(x, _)| *x == k) {
        Some(slot) => slot.1 = v,
        None => list.push((k, v)),
    }
}

fn arr_len(v: &Variable) -> Option<usize> {
    match v {
        Variable::Array(a) => Some(a.borrow().len()),
        _ => None,
    }
}

impl Coder {
    fn bag(
        &mut self,
        tag: &Option<Val>,
        edits: Vec<Edit>,
        base: &Variable,
        target: &Variable,
    ) -> Bag {
        let mut bag = Bag {
            tag: tag.clone(),
            delete: Vec::new(),
            splice: Vec::new(),
            each: Vec::new(),
            set: Vec::new(),
            first: None,
        };
        let mut sets: Vec<(Path, Variable)> = Vec::new();
        for e in edits {
            match e {
                Edit::Del(path) => {
                    bag.first.get_or_insert("$delete");
                    bag.delete.push(path);
                }
                Edit::Splice(path, at, del, ins) => {
                    bag.first.get_or_insert("$splice");
                    set_entry(&mut bag.splice, pptr(&path), (at, del, ins));
                }
                Edit::Set(path, value) => sets.push((path, value)),
            }
        }
        let indexed = sets
            .iter()
            .any(|(p, _)| p.iter().any(|s| matches!(s, PSeg::I(_))));
        let (each, rest) = match indexed {
            true => self.each_groups(sets, target, base),
            false => (Vec::new(), sets),
        };
        bag.each = each;
        for (p, v) in rest {
            bag.set.push((p, v));
        }
        bag
    }

    fn each_groups(
        &mut self,
        sets: Vec<(Path, Variable)>,
        target: &Variable,
        base: &Variable,
    ) -> (Each, Vec<(Path, Variable)>) {
        struct Group {
            prefix: Vec<PSeg>,
            suffix: Vec<PSeg>,
            col: Vec<Option<Variable>>,
            filled: usize,
        }
        let mut groups: Vec<Group> = Vec::new();
        let mut member: Vec<Option<usize>> = Vec::with_capacity(sets.len());
        for (path, value) in &sets {
            let mut t = Some(target.clone());
            let mut b = Some(base.clone());
            let mut slot = None;
            for d in 0..path.len() {
                let tl = t.as_ref().and_then(arr_len);
                if let Some(tl) = tl {
                    match b.as_ref().and_then(arr_len) {
                        Some(bl) if bl == tl && tl >= 4 => {}
                        _ => break,
                    }
                    let (prefix, suffix) = (&path[..d], &path[d + 1..]);
                    let gi = match groups
                        .iter()
                        .position(|g| g.prefix == prefix && g.suffix == suffix)
                    {
                        Some(i) => i,
                        None => {
                            groups.push(Group {
                                prefix: prefix.to_vec(),
                                suffix: suffix.to_vec(),
                                col: vec![None; tl],
                                filled: 0,
                            });
                            groups.len() - 1
                        }
                    };
                    if let Some(n) = number_of_pseg(&path[d]) {
                        let g = &mut groups[gi];
                        if n < g.col.len() {
                            if g.col[n].is_none() {
                                g.filled += 1;
                            }
                            g.col[n] = Some(value.clone());
                        } else {
                            g.filled = usize::MAX;
                        }
                    } else {
                        groups[gi].filled = usize::MAX;
                    }
                    slot = Some(gi);
                    break;
                }
                t = t.and_then(|x| pchild_fast(&x, &path[d]));
                b = b.and_then(|x| pchild_fast(&x, &path[d]));
                if t.is_none() {
                    break;
                }
            }
            member.push(slot);
        }
        let complete: Vec<bool> = groups.iter().map(|g| g.filled == g.col.len()).collect();
        let mut each: Each = Vec::new();
        for (g, done) in groups.into_iter().zip(&complete) {
            if !done {
                continue;
            }
            let col: Vec<Variable> = g
                .col
                .into_iter()
                .map(|v| v.unwrap_or(Variable::Null))
                .collect();
            let (prefix, suffix) = (pptr(&g.prefix), pptr(&g.suffix));
            match each.iter_mut().find(|(k, _)| *k == prefix) {
                Some((_, slot)) => set_entry(slot, suffix, col),
                None => each.push((prefix, vec![(suffix, col)])),
            }
        }
        if !complete.iter().any(|c| *c) {
            return (each, sets);
        }
        let rest = sets
            .into_iter()
            .zip(member)
            .filter(|(_, m)| !m.is_some_and(|g| complete[g]))
            .map(|(s, _)| s)
            .collect();
        (each, rest)
    }

    fn bag_size(&mut self, bag: &Bag) -> usize {
        let mut n = bag.tag.as_ref().map_or(2, super::core::size);
        if !bag.delete.is_empty() {
            let mut a = 2;
            for p in &bag.delete {
                a += ptr_len(p) + 2 + 1;
            }
            n += 7 + 4 + a;
        }
        if !bag.splice.is_empty() {
            let mut o = 2;
            for (p, (at, del, ins)) in &bag.splice {
                let mut a = 2 + digits(*at) + 1 + digits(*del) + 1;
                for x in ins {
                    a += self.size(x) + 1;
                }
                o += utf16_len(p) + 4 + a;
            }
            n += 7 + 4 + o;
        }
        if !bag.each.is_empty() {
            let mut o = 2;
            for (prefix, cols) in &bag.each {
                let mut s = 2;
                for (suffix, col) in cols {
                    let mut c = 2;
                    for x in col {
                        c += self.size(x) + 1;
                    }
                    s += utf16_len(suffix) + 4 + c;
                }
                o += utf16_len(prefix) + 4 + (2 + 4 + 4 + s);
            }
            n += 5 + 4 + o;
        }
        if !bag.set.is_empty() {
            let mut o = 2;
            for (p, v) in &bag.set {
                o += ptr_len(p) + 4 + self.size(v);
            }
            n += 4 + 4 + o;
        }
        n
    }

    fn bag_val(&mut self, bag: Bag) -> Val {
        let mut o = match bag.tag {
            Some(Val::Obj(t)) => (*t).clone(),
            _ => Obj::new(),
        };
        let mut order: Vec<&str> = Vec::new();
        if let Some(f) = bag.first {
            order.push(f);
        }
        for k in ["$delete", "$splice"] {
            if !order.contains(&k) {
                order.push(k);
            }
        }
        for k in order {
            match k {
                "$delete" if !bag.delete.is_empty() => {
                    o.insert(
                        self.refs.keys.get("$delete"),
                        Val::arr(bag.delete.iter().map(|p| Val::str(&pptr(p))).collect()),
                    );
                }
                "$splice" if !bag.splice.is_empty() => {
                    let mut s = Obj::with_capacity(bag.splice.len());
                    for (p, (at, del, ins)) in &bag.splice {
                        let mut t = vec![Val::int(*at as i64), Val::int(*del as i64)];
                        for x in ins {
                            t.push(self.refs.encode(&mut self.m, x));
                        }
                        s.set(p, Val::arr(t));
                    }
                    o.insert(self.refs.keys.get("$splice"), Val::obj(s));
                }
                _ => {}
            }
        }
        if !bag.each.is_empty() {
            let mut e = Obj::with_capacity(bag.each.len());
            for (prefix, cols) in bag.each {
                let mut set = Obj::with_capacity(cols.len());
                for (suffix, col) in cols {
                    let col = Variable::from_array(col);
                    set.set(&suffix, self.refs.encode(&mut self.m, &col));
                }
                let mut slot = Obj::with_capacity(1);
                slot.insert(self.refs.keys.get("$set"), Val::obj(set));
                e.set(&prefix, Val::obj(slot));
            }
            o.insert(self.refs.keys.get("$each"), Val::obj(e));
        }
        if !bag.set.is_empty() {
            let mut s = Obj::with_capacity(bag.set.len());
            for (p, v) in &bag.set {
                s.set(&pptr(p), self.refs.encode(&mut self.m, v));
            }
            o.insert(self.refs.keys.get("$set"), Val::obj(s));
        }
        Val::obj(o)
    }
}

struct Base {
    tag: Option<Val>,
    doc: Option<Variable>,
    penalty: usize,
}

enum Opt {
    Bag(Bag),
    Ready(Val),
    Lit,
}

struct Option_ {
    opt: Opt,
    lit: bool,
    size: usize,
}

impl Coder {
    fn value(&mut self, target: &Variable, bases: &[Base]) -> Option<Piece<'static>> {
        if let Some(d) = &bases[0].doc {
            if veq(d, target) {
                return None;
            }
        }
        let dollar = matches!(target, Variable::Object(o) if has_dollar_map(&o.borrow()));
        let lit_size = self.size(target) + if dollar { 14 } else { 0 };
        for b in &bases[1..] {
            let Some(d) = &b.doc else { continue };
            let Some(tag) = b.tag.clone() else { continue };
            if falsy(d) || !veq(d, target) {
                continue;
            }
            let tag_size = json_len(&tag) + b.penalty;
            if lit_size < tag_size {
                return Some(Piece::Lit(target.clone()));
            }
            if tag_size > 48 {
                if let Some(first) = bases[0].doc.as_ref().filter(|d| !is_null(d)) {
                    let mut w = Vec::new();
                    let mut p = std::mem::take(&mut self.path);
                    self.writes(Some(first), target, &mut p, &mut w);
                    self.path = p;
                    let bag = self.bag(&bases[0].tag, w, first, target);
                    if !falsy(first) && self.bag_size(&bag) < tag_size {
                        return Some(Piece::Bag(Box::new(bag)));
                    }
                }
            }
            return Some(Piece::Val(tag));
        }
        let mut options: Vec<Option_> = Vec::new();
        let mut cap = usize::MAX;
        let hollow =
            |b: &&Base| matches!(&b.doc, Some(Variable::Object(o)) if o.borrow().is_empty());
        let order = bases
            .iter()
            .filter(|b| !hollow(b))
            .chain(bases.iter().filter(|b| hollow(b)));
        for b in order {
            let Some(base) = &b.doc else { continue };
            if is_null(base) {
                continue;
            }
            if hollow(&b) {
                let best = options.iter().map(|o| o.size).min();
                if best.is_some_and(|best| best <= self.floor(target)) {
                    continue;
                }
            }
            let mut w = Vec::new();
            let mut p = std::mem::take(&mut self.path);
            self.writes(Some(base), target, &mut p, &mut w);
            self.path = p;
            let resized = w.iter().any(|e| match e {
                Edit::Set(path, Variable::Array(a)) => ppath(base, path)
                    .and_then(|x| arr_len(&x))
                    .is_some_and(|n| n != a.borrow().len()),
                _ => false,
            });
            let from_writes = self.bag(&b.tag, w, base, target);
            let has_set = resized && !from_writes.set.is_empty();
            let mut variants = vec![from_writes];
            if has_set {
                let mut d = Vec::new();
                let mut p = std::mem::take(&mut self.path);
                self.diff(base, target, &mut p, &mut d);
                self.path = p;
                variants.push(self.bag(&b.tag, d, base, target));
            }
            let lit = falsy(base);
            for bag in variants {
                if lit {
                    cap = options.iter().map(|o| o.size).min().unwrap_or(usize::MAX);
                }
                let size = self.bag_size(&bag).saturating_add(b.penalty);
                if size > cap {
                    continue;
                }
                options.push(Option_ {
                    opt: Opt::Bag(bag),
                    lit,
                    size,
                });
            }
            if options.iter().any(|o| o.size < 48) {
                break;
            }
        }
        let size = self.size(target);
        if is_container(target) && size > 4096 {
            let best = options.iter().map(|o| o.size).min();
            let hopeless = best.is_some_and(|b| b <= self.floor(target) || b * 4 < size);
            if !hopeless || self.refs.whole(target) {
                let r = self.refs.encode(&mut self.m, target);
                let size = json_len(&r);
                options.push(Option_ {
                    opt: Opt::Ready(r),
                    lit: true,
                    size,
                });
            }
        } else {
            cap = options.iter().map(|o| o.size).min().unwrap_or(usize::MAX);
            let dollar = matches!(target, Variable::Object(o) if has_dollar_map(&o.borrow()));
            let size = size + if dollar { 14 } else { 0 };
            if size <= cap {
                options.push(Option_ {
                    opt: Opt::Lit,
                    lit: true,
                    size,
                });
            }
        }
        options.sort_by_key(|o| o.size);
        let first = options.into_iter().next();
        Some(match first {
            Some(Option_ {
                opt: Opt::Ready(r), ..
            }) => Piece::Val(r),
            Some(Option_ {
                opt: Opt::Bag(bag),
                lit: false,
                ..
            }) => Piece::Bag(Box::new(bag)),
            _ => Piece::Lit(target.clone()),
        })
    }
}

fn tag_from(label: &'static str) -> Val {
    thread_local! {
        static TAGS: std::cell::RefCell<Vec<(&'static str, Val)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    TAGS.with(|t| {
        let mut t = t.borrow_mut();
        if let Some((_, v)) = t.iter().find(|(l, _)| *l == label) {
            return v.clone();
        }
        let mut o = Obj::with_capacity(1);
        o.insert(Rc::from("$from"), Val::str(label));
        let v = Val::obj(o);
        t.push((label, v.clone()));
        v
    })
}

fn tag_merge(list: &[Key]) -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(
        Rc::from("$merge"),
        Val::arr(list.iter().map(|k| Val::Str(k.clone())).collect()),
    );
    Val::obj(o)
}

pub(crate) fn deep_merge(a: &Variable, b: &Variable) -> Variable {
    if let (Variable::Object(x), Variable::Object(y)) = (a, b) {
        let mut o: VariableMap = x.borrow().clone();
        for (k, v) in ordered(&y.borrow()) {
            let nv = match o.get_str(k) {
                Some(cur) => deep_merge(cur, v),
                None => v.clone(),
            };
            o.insert(k.into(), nv);
        }
        return Variable::from_object(o);
    }
    b.clone()
}

pub(crate) fn place_at(path: &[Seg], value: Variable) -> Variable {
    let mut v = value;
    for s in path.iter().rev() {
        let mut o = VariableMap::with_capacity(1);
        o.insert((&*s.key()).into(), v);
        v = Variable::from_object(o);
    }
    v
}

pub(crate) fn build_results(m: &[(Key, Variable)]) -> Variable {
    let mut keys: Vec<&(Key, Variable)> = m.iter().collect();
    keys.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
    if keys.iter().all(|(k, _)| !k.is_empty() && !k.contains('.')) {
        let mut o = VariableMap::with_capacity(keys.len());
        for (k, v) in keys {
            o.insert((&**k).into(), v.clone());
        }
        return Variable::from_object(o);
    }
    let mut o = Variable::empty_object();
    for (k, v) in keys {
        o = deep_merge(&o, &place_at(&dotted(k), v.clone()));
    }
    o
}

enum Results {
    One(Vec<(Key, Variable)>, Option<Key>),
    Many(Vec<Vec<(Key, Variable)>>),
}

type HitAt<'t> = (&'t RuleT, &'t Hit, Option<Variable>, Option<usize>);

fn hit_derived<'r>(
    rule: &'r RuleT,
    scope: Option<&Variable>,
) -> SmallVec<[(&'r str, Variable); 8]> {
    let mut derived: SmallVec<[(&str, Variable); 8]> = SmallVec::new();
    let Some(fields) = &rule.derive else {
        return derived;
    };
    for f in fields {
        if f.is_empty() {
            continue;
        }
        let v = match dotted_get(scope, f) {
            Some(v) => v,
            None if plain_path(f) => Variable::Null,
            None => continue,
        };
        match derived.iter_mut().find(|(k, _)| *k == &**f) {
            Some(slot) => slot.1 = v,
            None => derived.push((&**f, v)),
        }
    }
    derived
}

pub(crate) fn derived<'r>(rule: &'r RuleT, scope: Option<&Variable>) -> Vec<(&'r str, Variable)> {
    hit_derived(rule, scope).into_iter().collect()
}

pub(crate) fn table_env(node: &DecisionNode, input: &Variable) -> Variable {
    let (_, attrs) = Kind::of(node);
    match attrs.and_then(|a| a.input_field.as_deref()) {
        Some(field) => dotted_or_null(input, field),
        None => input.clone(),
    }
}

pub(crate) fn dotted_or_null(v: &Variable, f: &str) -> Variable {
    dotted_get(Some(v), f).unwrap_or(Variable::Null)
}

pub(crate) fn table_scope(
    node: &DecisionNode,
    input: &Variable,
    item: Option<usize>,
) -> Option<Variable> {
    let (_, attrs) = Kind::of(node);
    let scoped = scope_of(input, attrs);
    match (scoped, looped(attrs), item) {
        (Some((_, Variable::Array(a))), true, Some(j)) => a.borrow().get(j).cloned(),
        (_, true, Some(_)) => Some(input.clone()),
        (Some((_, v)), false, _) => Some(v),
        _ => Some(input.clone()),
    }
}

fn hit_refs(hit: &Hit) -> SmallVec<[(&str, &Variable); 8]> {
    let mut out: SmallVec<[(&str, &Variable); 8]> = SmallVec::new();
    for (k, v) in hit.refs.iter().flatten() {
        match out.iter_mut().find(|(x, _)| *x == &**k) {
            Some(slot) => slot.1 = v,
            None => out.push((&**k, v)),
        }
    }
    out.sort_unstable_by(|a, b| a.0.cmp(b.0));
    out
}

fn hit_exact(rule: &RuleT, hit: &Hit, scope: Option<&Variable>) -> bool {
    if hit.refs.is_none() {
        return true;
    }
    let derived = hit_derived(rule, scope);
    let refs = hit_refs(hit);
    derived.len() == refs.len()
        && derived.iter().all(|(f, d)| {
            refs.iter()
                .find(|(k, _)| k == f)
                .is_some_and(|(_, v)| veq(d, v))
        })
}

impl Coder {
    fn hit_val(&mut self, at: &HitAt) -> Val {
        let (rule, hit, scope, item) = at;
        let mut o = Obj::with_capacity(5);
        if let Some(id) = &rule.id {
            o.insert(self.refs.keys.get("id"), Val::str(id));
        }
        let mut cells = Obj::with_capacity(rule.cells.len());
        for (k, v) in &rule.cells {
            cells.insert(self.refs.keys.get(k), Val::str(v));
        }
        let cells = Val::obj(cells);
        o.insert(
            Rc::from("cells"),
            if rule.dollar { literal(cells) } else { cells },
        );
        o.insert(self.refs.keys.get("index"), Val::int(rule.index as i64));
        if hit.refs.is_none() {
            if let Some(j) = item {
                o.insert(Rc::from("item"), Val::int(*j as i64));
            }
            return Val::obj(o);
        }
        let derived = hit_derived(rule, scope.as_ref());
        let refmap = hit_refs(hit);
        let mut refs = Obj::new();
        let mut raw = Obj::new();
        for (k, v) in &refmap {
            let same = derived
                .iter()
                .find(|(f, _)| f == k)
                .is_some_and(|(_, d)| veq(d, v));
            if !same {
                refs.insert(Rc::from(*k), convert_escaped(v));
                raw.insert(Rc::from(*k), from_variable(v));
            }
        }
        if raw.keys().any(|k| k.starts_with('$')) {
            refs = Obj::with_capacity(1);
            refs.insert(Rc::from("$literal"), Val::obj(raw));
        }
        let drop: Vec<Val> = derived
            .iter()
            .filter(|(f, _)| !refmap.iter().any(|(k, _)| k == f))
            .map(|(f, _)| Val::str(f))
            .collect();
        if !refs.is_empty() {
            o.insert(self.refs.keys.get("references"), Val::obj(refs));
        }
        if !drop.is_empty() {
            o.insert(self.refs.keys.get("referencesAbsent"), Val::arr(drop));
        }
        if let Some(j) = item {
            o.insert(Rc::from("item"), Val::int(*j as i64));
        }
        Val::obj(o)
    }

    fn hits_val(&mut self, list: &[HitAt]) -> Val {
        let mut seen: Vec<(usize, Val)> = Vec::new();
        let mut out = Vec::with_capacity(list.len());
        for at in list {
            let index = at.3.is_none().then_some(at.0.index);
            let known = index.and_then(|i| {
                seen.iter()
                    .find(|(k, _)| *k == i)
                    .map(|(_, v)| v.clone())
                    .filter(|v| !v.has("references") && !v.has("referencesAbsent"))
            });
            match known {
                Some(v) if hit_exact(at.0, at.1, at.2.as_ref()) => out.push(v),
                _ => {
                    let v = self.hit_val(at);
                    if let Some(i) = index {
                        if !seen.iter().any(|(k, _)| *k == i) {
                            seen.push((i, v.clone()));
                        }
                    }
                    out.push(v);
                }
            }
        }
        Val::arr(out)
    }

    fn put_hit(&mut self, at: &HitAt, out: &mut String) {
        let (rule, hit, scope, item) = at;
        out.push_str(&rule.head);
        if hit.refs.is_none() {
            if let Some(j) = item {
                out.push_str(",\"item\":");
                out.push_str(&j.to_string());
            }
            out.push('}');
            return;
        }
        let derived = hit_derived(rule, scope.as_ref());
        let refmap = hit_refs(hit);
        let differ: SmallVec<[(&str, &Variable); 8]> = refmap
            .iter()
            .filter(|(k, v)| {
                !derived
                    .iter()
                    .find(|(f, _)| f == k)
                    .is_some_and(|(_, d)| veq(d, v))
            })
            .cloned()
            .collect();
        if !differ.is_empty() {
            let dollar = differ.iter().any(|(k, _)| k.starts_with('$'));
            out.push_str(if dollar {
                ",\"references\":{\"$literal\":{"
            } else {
                ",\"references\":{"
            });
            for (i, (k, v)) in differ.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_key(k, out);
                match dollar {
                    true => {
                        let mark = out.len();
                        if !write_raw(v, out) {
                            out.truncate(mark);
                            write_json(&from_variable(v), out);
                        }
                    }
                    false => self.put_esc(v, out),
                }
            }
            out.push_str(if dollar { "}}" } else { "}" });
        }
        let mut absent = false;
        for (f, _) in derived
            .iter()
            .filter(|(f, _)| !refmap.iter().any(|(k, _)| k == f))
        {
            out.push_str(if absent {
                ","
            } else {
                ",\"referencesAbsent\":["
            });
            absent = true;
            write_str(f, out);
        }
        if absent {
            out.push(']');
        }
        if let Some(j) = item {
            out.push_str(",\"item\":");
            out.push_str(&j.to_string());
        }
        out.push('}');
    }
}

fn scope_of(input: &Variable, attrs: Option<&TransformAttributes>) -> Option<(String, Variable)> {
    let field = attrs?.input_field.as_deref()?;
    if !plain_path(field) {
        return None;
    }
    let v = dotted_get(Some(input), field)?;
    Some((dot_to_ptr(field), v))
}

fn looped(attrs: Option<&TransformAttributes>) -> bool {
    attrs.is_some_and(|a| matches!(a.execution_mode, TransformExecutionMode::Loop))
}

struct Table<'t> {
    field: &'static str,
    value: Piece<'t>,
    scope: Option<String>,
    items: Option<String>,
}

impl Coder {
    fn table<'t>(
        &mut self,
        n: &'t NodeT,
        fact: &'t Fact,
        input: &Variable,
        attrs: Option<&TransformAttributes>,
    ) -> Option<Table<'t>> {
        let scoped = scope_of(input, attrs);
        let is_loop = looped(attrs);
        let rule = |hit: &Hit| n.rules.get(hit.rule as usize);
        let single = |scoped: &Option<(String, Variable)>| match (scoped, is_loop) {
            (Some((p, v)), false) => (Some(p.clone()), v.clone()),
            _ => (None, input.clone()),
        };
        let items_of = |s: &Option<(String, Variable)>| match s {
            Some((p, Variable::Array(arr))) if is_loop => Some((p.clone(), arr.borrow().clone())),
            _ => None,
        };
        match fact {
            Fact::Hit(hit) => {
                let (scope, s) = single(&scoped);
                Some(Table {
                    field: "rule",
                    value: Piece::Hit((rule(hit)?, hit, Some(s), None)),
                    scope,
                    items: None,
                })
            }
            Fact::Hits(hits) => {
                let (scope, s) = single(&scoped);
                let list = hits
                    .iter()
                    .map(|h| Some((rule(h)?, h, Some(s.clone()), None)))
                    .collect::<Option<Vec<HitAt>>>()?;
                Some(Table {
                    field: "rules",
                    value: Piece::Hits(list),
                    scope,
                    items: None,
                })
            }
            Fact::Items(items) if present(items).all(|f| matches!(f, Fact::Hits(_))) => {
                let found = items_of(&scoped);
                let list = found.as_ref().map(|(_, v)| v.as_slice());
                let mut rows = Vec::with_capacity(items.len());
                for (j, f) in present(items).enumerate() {
                    let it = list.and_then(|i| i.get(j)).cloned();
                    let hits: &[Hit] = match f {
                        Fact::Hits(h) => h,
                        _ => &[],
                    };
                    let mut row = Vec::with_capacity(hits.len());
                    for h in hits {
                        row.push(self.hit_val(&(rule(h)?, h, it.clone(), None)));
                    }
                    rows.push(Val::arr(row));
                }
                Some(Table {
                    field: "rulesPerItem",
                    value: Piece::Val(Val::arr(rows)),
                    scope: None,
                    items: found.map(|(p, _)| p),
                })
            }
            Fact::Items(items) => {
                let hits: Vec<(usize, &Hit)> = items
                    .iter()
                    .enumerate()
                    .filter_map(|(j, f)| match f {
                        Fact::Hit(h) => Some(Some((j, h))),
                        Fact::None => None,
                        _ => Some(None),
                    })
                    .collect::<Option<_>>()?;
                if let Some((p, list)) = items_of(&scoped) {
                    let mut cur: i64 = -1;
                    let mut out = Vec::with_capacity(hits.len());
                    for (j, h) in hits {
                        let next = (cur + 1) as usize;
                        out.push((rule(h)?, h, list.get(j).cloned(), (j != next).then_some(j)));
                        cur = j as i64;
                    }
                    return Some(Table {
                        field: "rules",
                        value: Piece::Hits(out),
                        scope: None,
                        items: Some(p),
                    });
                }
                let (scope, s) = single(&scoped);
                let list = hits
                    .into_iter()
                    .map(|(_, h)| Some((rule(h)?, h, Some(s.clone()), None)))
                    .collect::<Option<Vec<HitAt>>>()?;
                Some(Table {
                    field: "rules",
                    value: Piece::Hits(list),
                    scope,
                    items: None,
                })
            }
            _ => None,
        }
    }
}

fn empty() -> Variable {
    thread_local! {
        static EMPTY: Variable = Variable::empty_object();
    }
    EMPTY.with(|e| e.clone())
}

fn output_bases(
    kind: Option<Kind>,
    input: &Variable,
    req: &Variable,
    failed: bool,
    rbases: Vec<(&'static str, Variable)>,
) -> Vec<Base> {
    let dflt = if failed {
        "null"
    } else if kind == Some(Kind::Input) {
        "request"
    } else if kind == Some(Kind::Output) {
        "null"
    } else if !rbases.is_empty() {
        "results"
    } else if kind == Some(Kind::Function) {
        "empty"
    } else {
        "input"
    };
    let mut all: Vec<Base> = Vec::with_capacity(rbases.len() + 4);
    let mut push = |label: &'static str, doc: Variable| {
        let base = Base {
            tag: (label != dflt).then(|| tag_from(label)),
            doc: Some(doc),
            penalty: 0,
        };
        match label == dflt {
            true => all.insert(0, base),
            false => all.push(base),
        }
    };
    for (label, doc) in rbases {
        push(label, doc);
    }
    push("input", input.clone());
    if kind == Some(Kind::Input) {
        push("request", req.clone());
    }
    push("empty", empty());
    push("null", Variable::Null);
    all
}

fn results_bases(
    input: &Variable,
    results: &Results,
    at: Option<&[Seg]>,
    items: Option<&[Seg]>,
    output: Option<&Variable>,
) -> (Vec<(&'static str, Variable)>, Option<usize>) {
    let place = |x: Variable| match at {
        Some(segs) if !segs.is_empty() => place_at(segs, x),
        _ => x,
    };
    let (r, many) = match results {
        Results::One(m, _) => (build_results(m), None),
        Results::Many(ms) => {
            let rs: Vec<Variable> = ms.iter().map(|m| build_results(m)).collect();
            (Variable::from_array(rs.clone()), Some(rs))
        }
    };
    let placed = place(r);
    let hits = |v: &Variable| output.is_some_and(|o| veq(v, o));
    if hits(&placed) {
        return (vec![("results", placed)], Some(0));
    }
    let merged = deep_merge(input, &placed);
    let hit = hits(&merged);
    let mut out = vec![("results", placed), ("input+results", merged)];
    if hit {
        return (out, Some(1));
    }
    if let (Some(rs), Some(items)) = (many, items) {
        if let Some(Variable::Array(arr)) = vpath(input, items) {
            let arr = arr.borrow();
            let merged: Vec<Variable> = rs
                .iter()
                .enumerate()
                .map(|(i, x)| match arr.get(i) {
                    Some(a) => deep_merge(a, x),
                    None => x.clone(),
                })
                .collect();
            let pm = place(Variable::from_array(merged));
            out.push(("items+results", pm.clone()));
            out.push(("input+items+results", deep_merge(input, &pm)));
        }
    }
    let hit = out.iter().position(|b| hits(&b.1));
    (out, hit)
}

enum Used {
    Open,
    Parents,
    Nothing,
}

pub(crate) enum Doc {
    Text(String),
    Tree(Val),
}

fn present(items: &[Fact]) -> impl Iterator<Item = &Fact> {
    items.iter().filter(|f| !matches!(f, Fact::None))
}

fn empty_fact(fact: &Fact) -> bool {
    match fact {
        Fact::Hits(h) => h.is_empty(),
        Fact::Items(items) => present(items).next().is_none(),
        _ => false,
    }
}

type Rows = Vec<(Arc<str>, Key, Variable)>;

fn result_rows(n: &NodeT, m: &[(Arc<str>, Variable)], force: bool) -> Option<Rows> {
    let repeats = m
        .iter()
        .enumerate()
        .any(|(i, (k, _))| m[..i].iter().any(|(x, _)| x == k));
    if !(repeats || force) || m.len() > n.exprs.len() {
        return None;
    }
    m.iter()
        .zip(&n.exprs)
        .map(|((k, v), (id, key))| (k == key).then(|| (id.clone(), Rc::from(&**k), v.clone())))
        .collect()
}

fn row_shaped(m: &[(Key, Variable)]) -> bool {
    m.len() == 3
        && ["id", "key", "value"]
            .iter()
            .all(|k| m.iter().any(|(x, _)| &**x == *k))
        && m.iter()
            .filter(|(k, _)| &**k != "value")
            .all(|(_, v)| matches!(v, Variable::String(_)))
}

fn sorted_results(m: &[(Arc<str>, Variable)]) -> Vec<(Key, Variable)> {
    let mut out: Vec<(Key, Variable)> = Vec::with_capacity(m.len());
    for (k, v) in m {
        match out.iter_mut().find(|(x, _)| **x == **k) {
            Some(slot) => slot.1 = v.clone(),
            None => out.push((Rc::from(&**k), v.clone())),
        }
    }
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

fn has_subs(level: &Level) -> bool {
    level.records.iter().any(|r| {
        level
            .template
            .node(r.slot)
            .is_some_and(|n| matches!(n.node.kind, DecisionNodeKind::DecisionNode { .. }))
    })
}

pub(crate) fn encode_doc(
    level: &Level,
    request: &Variable,
    error: Option<&ErrorInfo>,
    engine: Option<&Val>,
    evaluation: Option<&super::Evaluation>,
) -> Result<Doc, Miss> {
    let direct = !has_subs(level);
    let mut shapes = Shapes::default();
    let mut rows = false;
    if !direct {
        let enc = encode_level(
            level,
            request,
            error,
            error.is_some(),
            &mut shapes,
            None,
            &mut rows,
        )?;
        let extra: &[&str] = match rows {
            true => &[super::core::RESULT_ROWS],
            false => &[],
        };
        let doc = finish_graph(enc, shapes, &[], engine, extra)
            .map_err(|_| "nested trace shapes do not resolve")?;
        return Ok(Doc::Tree(match evaluation {
            Some(e) => e.stamp(doc),
            None => doc,
        }));
    }
    let mut text = String::with_capacity(512 + 256 * level.records.len());
    text.push_str("{\"version\":1,\"kind\":\"graph\"");
    if let Some(e) = engine {
        text.push_str(",\"engine\":");
        write_json(e, &mut text);
    }
    if let Some(e) = evaluation {
        e.write(&mut text);
    }
    text.push_str(",\"nodes\":");
    encode_level(
        level,
        request,
        error,
        error.is_some(),
        &mut shapes,
        Some(&mut text),
        &mut rows,
    )?;
    if rows {
        text.push_str(",\"requires\":[\"result-rows\"]");
    }
    text.push('}');
    Ok(Doc::Text(text))
}

fn encode_level(
    level: &Level,
    request: &Variable,
    error: Option<&ErrorInfo>,
    failing: bool,
    shapes: &mut Shapes,
    mut text: Option<&mut String>,
    rows_used: &mut bool,
) -> Result<EncTrace, Miss> {
    let template = &level.template;
    let mut coder = Coder {
        m: Measure::default(),
        refs: Refs::new(request),
        path: WPath::default(),
    };
    let total = level.records.len();
    let mut open: Vec<Key> = Vec::new();
    let mut outputs: Vec<(Key, Variable)> = Vec::new();
    let mut nodes: Vec<EncRec> = Vec::with_capacity(if text.is_some() { 0 } else { total });
    let mut keys: Vec<Key> = Vec::with_capacity(total);
    for (pos, r) in level.records.iter().enumerate() {
        let n = template
            .node(r.slot)
            .ok_or("record without a template node")?;
        let key: Key = Rc::from(&*n.id);
        if keys.contains(&key) {
            return Err("node ran twice at one level");
        }
        keys.push(key.clone());
        let last = pos + 1 == total;
        let (kind, attrs) = Kind::of(&n.node);
        let failed = failing && last && is_null(&r.output);
        let mut rec: Vec<(&'static str, Piece)> = Vec::with_capacity(10);
        rec.push(("id", Piece::Val(Val::Str(key.clone()))));
        if n.node.name != n.node.id {
            rec.push(("name", Piece::Val(Val::str(&n.node.name))));
        }
        let data = match &r.fact {
            Fact::Data(v) => v,
            _ => &Variable::Null,
        };
        let decode_kind = match kind {
            Kind::Function if !matches!(data, Variable::Object(o) if function_td(&o.borrow())) => {
                None
            }
            Kind::Custom => None,
            k => Some(k),
        };
        if decode_kind.is_some() || kind == Kind::Custom {
            rec.push(("type", Piece::Val(text_val(kind.name()))));
        }
        rec.push((
            "perf",
            Piece::Val(
                match super::core::duration_perf(std::time::Duration::from_nanos(r.ns)) {
                    Ok(ns) => Val::int(ns as i64),
                    Err(text) => Val::str(&text),
                },
            ),
        ));
        if failed {
            rec.push(("failed", Piece::Val(Val::Bool(true))));
            if let Some(err) = error {
                if err.node == *n.id {
                    if let Some(m) = &err.message {
                        rec.push(("error", Piece::Val(Val::str(m))));
                    }
                }
            }
        }
        let parent_ids = || -> SmallVec<[Key; 4]> {
            r.parents
                .iter()
                .filter_map(|p| template.node(*p))
                .map(|p| Rc::from(&*p.id))
                .collect()
        };
        let output_of = |id: &str| outputs.iter().find(|(k, _)| &**k == id).map(|(_, v)| v);
        let merge_of = |ids: &[Key]| -> Option<Variable> {
            let mut v: Option<Variable> = None;
            for id in ids {
                let o = output_of(id)?;
                v = Some(match v {
                    None => o.clone(),
                    Some(cur) => deep_merge(&cur, o),
                });
            }
            v
        };
        let mut parents_used: SmallVec<[Key; 4]> = SmallVec::new();
        let used = if pos == 0 {
            if !is_null(&r.input) {
                return Err("first node input is not null");
            }
            Used::Nothing
        } else {
            let fits = match open.len() {
                0 => false,
                1 => output_of(&open[0]).is_some_and(|o| veq(o, &r.input)),
                _ => merge_of(&open).is_some_and(|m| veq(&m, &r.input)),
            };
            if fits {
                Used::Open
            } else {
                let parents = parent_ids();
                let mut bases = vec![Base {
                    tag: None,
                    doc: merge_of(&open),
                    penalty: 0,
                }];
                if !parents.is_empty() && parents.as_slice() != open.as_slice() {
                    if let Some(doc) = merge_of(&parents) {
                        bases.push(Base {
                            tag: Some(tag_merge(&parents)),
                            doc: Some(doc),
                            penalty: 0,
                        });
                    }
                }
                bases.push(Base {
                    tag: Some(tag_from("request")),
                    doc: Some(request.clone()),
                    penalty: 4,
                });
                bases.push(Base {
                    tag: Some(tag_from("null")),
                    doc: Some(Variable::Null),
                    penalty: 0,
                });
                match coder.value(&r.input, &bases) {
                    None => Used::Open,
                    Some(v) => {
                        let merged = match &v {
                            Piece::Bag(b) if b.tag.as_ref().is_some_and(|t| t.has("$merge")) => {
                                true
                            }
                            Piece::Val(t) if t.has("$merge") => true,
                            _ => false,
                        };
                        rec.push(("input", v));
                        match merged {
                            true => {
                                parents_used = parents;
                                Used::Parents
                            }
                            false => Used::Nothing,
                        }
                    }
                }
            }
        };
        let mut detail: Vec<(&'static str, Piece)> = Vec::new();
        let mut nest_out = None;
        let mut rbases: Vec<(&'static str, Variable)> = Vec::new();
        let mut results: Option<Results> = None;
        let mut rows: Vec<Option<Rows>> = Vec::new();
        let fact = &r.fact;
        match kind {
            Kind::Table | Kind::Decision | Kind::Expression if empty_fact(fact) => {
                detail.push(("loop", Piece::Val(Val::arr(Vec::new()))));
            }
            Kind::Expression if matches!(fact, Fact::Results(_) | Fact::Items(_)) => {
                let res = match fact {
                    Fact::Items(items) => Results::Many(
                        present(items)
                            .map(|f| match f {
                                Fact::Results(m) => {
                                    let sorted = sorted_results(m);
                                    rows.push(result_rows(n, m, row_shaped(&sorted)));
                                    Some(sorted)
                                }
                                _ => None,
                            })
                            .collect::<Option<_>>()
                            .ok_or("loop item without expression results")?,
                    ),
                    Fact::Results(m) => {
                        rows.push(result_rows(n, m, false));
                        Results::One(sorted_results(m), None)
                    }
                    _ => return Err("expression facts are neither results nor items"),
                };
                if rows.iter().any(Option::is_some) {
                    *rows_used = true;
                }
                let at = attrs.and_then(|a| a.output_path.as_deref()).map(dotted);
                let items = match (&res, attrs.and_then(|a| a.input_field.as_deref())) {
                    (Results::Many(_), Some(f)) if plain_path(f) => Some(dotted(f)),
                    _ => None,
                };
                let target = (!failed).then_some(&r.output);
                let (mut bases, hit) =
                    results_bases(&r.input, &res, at.as_deref(), items.as_deref(), target);
                if !failed {
                    if let Some(a) = at.as_ref().filter(|a| !a.is_empty()) {
                        detail.push(("resultsAt", Piece::Val(Val::str(&to_ptr(a)))));
                    }
                    match (hit.is_none_or(|h| h >= 2), &items) {
                        (true, Some(it)) if bases.len() > 2 => {
                            detail.push(("items", Piece::Val(Val::str(&to_ptr(it)))))
                        }
                        _ => bases.truncate(2),
                    }
                }
                rbases = bases;
                results = Some(res);
            }
            Kind::Switch => {
                let ids: Vec<Val> = match fact {
                    Fact::Branches(b) => b
                        .iter()
                        .map(|i| n.statements.get(*i as usize).map(|s| Val::str(s)))
                        .collect::<Option<_>>()
                        .ok_or("switch branch out of range")?,
                    _ => Vec::new(),
                };
                detail.push(("branches", Piece::Val(Val::arr(ids))));
            }
            Kind::Function => {
                if let Variable::Object(o) = data {
                    let o = o.borrow();
                    if function_td(&o) {
                        if let Some(log) = o.get_str("log") {
                            if !matches!(log, Variable::Array(a) if a.borrow().is_empty()) {
                                detail.push(("log", Piece::Esc(log.clone())));
                            }
                        }
                    } else {
                        detail.push(("details", Piece::Esc(data.clone())));
                    }
                } else if !is_null(data) {
                    detail.push(("details", Piece::Esc(data.clone())));
                }
            }
            Kind::Table if matches!(fact, Fact::Hit(_) | Fact::Hits(_) | Fact::Items(_)) => {
                match coder.table(n, fact, &r.input, attrs) {
                    Some(t) => {
                        if let Some(s) = t.scope {
                            detail.push(("scope", Piece::Val(Val::str(&s))));
                        }
                        if let Some(i) = t.items {
                            detail.push(("items", Piece::Val(Val::str(&i))));
                        }
                        detail.push((t.field, t.value));
                    }
                    None => return Err("table facts do not match the template"),
                }
            }
            Kind::Decision if matches!(fact, Fact::Sub(_) | Fact::Items(_)) => {
                let is_loop = matches!(fact, Fact::Items(_));
                let subs: Vec<&Level> = match fact {
                    Fact::Sub(l) => vec![&**l],
                    Fact::Items(items) => present(items)
                        .map(|f| match f {
                            Fact::Sub(l) => Some(&**l),
                            _ => None,
                        })
                        .collect::<Option<_>>()
                        .ok_or("loop item without a sub-decision")?,
                    _ => return Err("decision facts are neither a sub-decision nor items"),
                };
                let scoped = scope_of(&r.input, attrs);
                let mut items_arr: Option<Vec<Variable>> = None;
                let mut sreq_one = r.input.clone();
                match (&scoped, is_loop) {
                    (Some((p, Variable::Array(a))), true) => {
                        detail.push(("items", Piece::Val(Val::str(p))));
                        items_arr = Some(a.borrow().clone());
                    }
                    (Some((p, v)), false) if !veq(v, &r.input) => {
                        detail.push(("scope", Piece::Val(Val::str(p))));
                        sreq_one = v.clone();
                    }
                    _ => {}
                }
                let count = subs.len();
                let mut encoded = Vec::with_capacity(count);
                for (i, s) in subs.into_iter().enumerate() {
                    let sreq = match is_loop {
                        true => items_arr
                            .as_ref()
                            .and_then(|a| a.get(i))
                            .cloned()
                            .unwrap_or(Variable::Null),
                        false => sreq_one.clone(),
                    };
                    encoded.push(encode_level(
                        s,
                        &sreq,
                        None,
                        failed && i + 1 == count,
                        shapes,
                        None,
                        rows_used,
                    )?);
                }
                nest_out = Some(
                    nest(
                        shapes,
                        match is_loop {
                            true => NestKind::Loop,
                            false => NestKind::Sub,
                        },
                        encoded,
                    )
                    .map_err(|_| "nested trace without runs")?,
                );
            }
            Kind::Input | Kind::Output | Kind::Custom => {
                if matches!(data, Variable::Array(a) if a.borrow().is_empty()) {
                    detail.push(("loop", Piece::Val(Val::arr(Vec::new()))));
                } else if !is_null(data) {
                    detail.push(("details", Piece::Esc(data.clone())));
                }
            }
            _ => {
                if !is_null(data) {
                    detail.push(("details", Piece::Esc(data.clone())));
                }
            }
        }
        let bases = output_bases(decode_kind, &r.input, request, failed, rbases);
        if let Some(o) = coder.value(&r.output, &bases) {
            rec.push(("output", o));
        }
        match results {
            Some(Results::Many(ms)) => {
                let mut list = Vec::with_capacity(ms.len());
                for (i, m) in ms.into_iter().enumerate() {
                    match rows.get_mut(i).and_then(Option::take) {
                        Some(item) => list.push(coder.rows_val(item)),
                        None => {
                            let mut o = Obj::with_capacity(m.len());
                            for (k, v) in m {
                                o.insert(k, coder.refs.encode(&mut coder.m, &v));
                            }
                            list.push(Val::obj(o));
                        }
                    }
                }
                rec.push(("results", Piece::Val(Val::arr(list))));
            }
            Some(Results::One(m, fk)) => {
                match rows.pop().flatten() {
                    Some(list) => rec.push(("results", Piece::Rows(list))),
                    None => rec.push(("results", Piece::Results(m))),
                }
                if let Some(fk) = fk {
                    rec.push(("failedKey", Piece::Val(Val::Str(fk))));
                }
            }
            None => {}
        }
        rec.extend(detail);
        rec.sort_by_key(|(k, _)| rec_rank(k));
        match text.as_deref_mut() {
            Some(text) => {
                text.push(if pos == 0 { '[' } else { ',' });
                text.push('{');
                for (i, (k, v)) in rec.into_iter().enumerate() {
                    if i > 0 {
                        text.push(',');
                    }
                    write_key(k, text);
                    coder.put(v, text);
                }
                text.push('}');
            }
            None => {
                let mut obj = Obj::with_capacity(rec.len());
                for (k, v) in rec {
                    let v = coder.piece_val(v);
                    obj.push_unique(coder.refs.keys.get(k), v);
                }
                nodes.push(EncRec {
                    obj,
                    nest: nest_out,
                });
            }
        }
        match used {
            Used::Open => open.clear(),
            Used::Parents => open.retain(|x| !parents_used.iter().any(|p| **p == **x)),
            Used::Nothing => {}
        }
        open.push(key.clone());
        match outputs.iter().position(|(k, _)| *k == key) {
            Some(i) => outputs[i].1 = r.output.clone(),
            None => outputs.push((key.clone(), r.output.clone())),
        }
        coder.refs.add_node(&key, &r.output);
    }
    if let Some(text) = text {
        if total == 0 {
            text.push('[');
        }
        text.push(']');
    }
    Ok(EncTrace::Nodes(nodes))
}

enum Piece<'t> {
    Val(Val),
    Lit(Variable),
    Esc(Variable),
    Bag(Box<Bag>),
    Results(Vec<(Key, Variable)>),
    Rows(Rows),
    Hit(HitAt<'t>),
    Hits(Vec<HitAt<'t>>),
}

pub(crate) fn write_key(k: &str, out: &mut String) {
    write_str(k, out);
    out.push(':');
}

fn write_raw(v: &Variable, out: &mut String) -> bool {
    match v {
        Variable::Null => out.push_str("null"),
        Variable::Bool(true) => out.push_str("true"),
        Variable::Bool(false) => out.push_str("false"),
        Variable::Number(d) => write_num(d, out),
        Variable::String(x) => write_str(x, out),
        Variable::Array(a) => {
            out.push('[');
            for (i, x) in a.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if !write_raw(x, out) {
                    return false;
                }
            }
            out.push(']');
        }
        Variable::Object(o) => {
            let o = o.borrow();
            out.push('{');
            let mut ok = true;
            let mut first = true;
            each_entry(&o, |k, x| {
                if !ok {
                    return;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                write_key(k, out);
                ok = write_raw(x, out);
            });
            if !ok {
                return false;
            }
            out.push('}');
        }
        Variable::Dynamic(_) => return false,
    }
    true
}

pub(crate) fn write_esc(v: &Variable, out: &mut String) -> bool {
    match v {
        Variable::Array(a) => {
            let a = a.borrow();
            if a.len() >= 4 {
                return false;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if !write_esc(x, out) {
                    return false;
                }
            }
            out.push(']');
            true
        }
        Variable::Object(o) => {
            let o = o.borrow();
            if has_dollar_map(&o) {
                out.push_str("{\"$literal\":");
                drop(o);
                if !write_raw(v, out) {
                    return false;
                }
                out.push('}');
                return true;
            }
            out.push('{');
            let mut ok = true;
            let mut first = true;
            each_entry(&o, |k, x| {
                if !ok {
                    return;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                write_key(k, out);
                ok = write_esc(x, out);
            });
            if !ok {
                return false;
            }
            out.push('}');
            true
        }
        Variable::Dynamic(_) => false,
        _ => write_raw(v, out),
    }
}

impl Coder {
    fn put_val(&mut self, v: &Val, out: &mut String) {
        write_json(&pack(v), out);
    }

    fn put_lit(&mut self, v: &Variable, out: &mut String) {
        let mark = out.len();
        if (!is_container(v) || self.size(v) <= REF_LOOKUP) && write_esc(v, out) {
            return;
        }
        out.truncate(mark);
        let val = self.refs.encode(&mut self.m, v);
        self.put_val(&val, out);
    }

    fn put_esc(&mut self, v: &Variable, out: &mut String) {
        let mark = out.len();
        if write_esc(v, out) {
            return;
        }
        out.truncate(mark);
        self.put_val(&convert_escaped(v), out);
    }

    fn put(&mut self, p: Piece<'_>, out: &mut String) {
        match p {
            Piece::Val(v) => self.put_val(&v, out),
            Piece::Lit(v) => self.put_lit(&v, out),
            Piece::Esc(v) => self.put_esc(&v, out),
            Piece::Bag(b) if !b.splice.is_empty() => {
                let v = self.bag_val(*b);
                self.put_val(&v, out);
            }
            Piece::Bag(b) => self.put_bag(*b, out),
            Piece::Results(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_key(k, out);
                    self.put_lit(v, out);
                }
                out.push('}');
            }
            Piece::Rows(list) => {
                out.push('[');
                for (i, (id, k, v)) in list.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str("{\"id\":");
                    write_str(id, out);
                    out.push_str(",\"key\":");
                    write_str(k, out);
                    out.push_str(",\"value\":");
                    self.put_lit(v, out);
                    out.push('}');
                }
                out.push(']');
            }
            Piece::Hit(at) => self.put_hit(&at, out),
            Piece::Hits(list) if list.len() < 4 => {
                out.push('[');
                for (i, at) in list.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.put_hit(at, out);
                }
                out.push(']');
            }
            Piece::Hits(list) => {
                let v = self.hits_val(&list);
                self.put_val(&v, out);
            }
        }
    }

    fn piece_val(&mut self, p: Piece<'_>) -> Val {
        match p {
            Piece::Val(v) => v,
            Piece::Lit(v) => self.refs.encode(&mut self.m, &v),
            Piece::Esc(v) => convert_escaped(&v),
            Piece::Bag(b) => self.bag_val(*b),
            Piece::Results(m) => {
                let mut o = Obj::with_capacity(m.len());
                for (k, v) in m {
                    o.insert(k, self.refs.encode(&mut self.m, &v));
                }
                Val::obj(o)
            }
            Piece::Rows(list) => self.rows_val(list),
            Piece::Hit(at) => self.hit_val(&at),
            Piece::Hits(list) => self.hits_val(&list),
        }
    }

    fn rows_val(&mut self, list: Rows) -> Val {
        Val::arr(
            list.into_iter()
                .map(|(id, k, v)| {
                    let mut o = Obj::with_capacity(3);
                    o.insert(Rc::from("id"), Val::str(&id));
                    o.insert(Rc::from("key"), Val::Str(k));
                    o.insert(Rc::from("value"), self.refs.encode(&mut self.m, &v));
                    Val::obj(o)
                })
                .collect(),
        )
    }

    fn put_bag(&mut self, bag: Bag, out: &mut String) {
        out.push('{');
        let mut first = true;
        let mut sep = |out: &mut String| {
            if !first {
                out.push(',');
            }
            first = false;
        };
        if let Some(Val::Obj(t)) = &bag.tag {
            for (k, v) in t.iter() {
                sep(out);
                write_key(k, out);
                write_json(v, out);
            }
        }
        if !bag.delete.is_empty() {
            sep(out);
            out.push_str("\"$delete\":[");
            for (i, p) in bag.delete.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_ptr_json(p, out);
            }
            out.push(']');
        }
        if !bag.each.is_empty() {
            sep(out);
            out.push_str("\"$each\":{");
            for (i, (prefix, cols)) in bag.each.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_key(&prefix, out);
                out.push_str("{\"$set\":{");
                for (j, (suffix, col)) in cols.into_iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    write_key(&suffix, out);
                    self.put_lit(&Variable::from_array(col), out);
                }
                out.push_str("}}");
            }
            out.push('}');
        }
        if !bag.set.is_empty() {
            sep(out);
            out.push_str("\"$set\":{");
            for (i, (p, v)) in bag.set.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_ptr_json(p, out);
                out.push(':');
                self.put_lit(v, out);
            }
            out.push('}');
        }
        out.push('}');
    }
}

pub(crate) fn hit_text(
    rule: &RuleT,
    hit: &Hit,
    scope: Option<&Variable>,
    item: Option<usize>,
    out: &mut String,
) -> bool {
    out.push_str(&rule.head);
    if hit.refs.is_some() {
        let derived = hit_derived(rule, scope);
        let refmap = hit_refs(hit);
        let differ: SmallVec<[(&str, &Variable); 8]> = refmap
            .iter()
            .filter(|(k, v)| {
                !derived
                    .iter()
                    .find(|(f, _)| f == k)
                    .is_some_and(|(_, d)| veq(d, v))
            })
            .cloned()
            .collect();
        if !differ.is_empty() {
            let dollar = differ.iter().any(|(k, _)| k.starts_with('$'));
            out.push_str(if dollar {
                ",\"references\":{\"$literal\":{"
            } else {
                ",\"references\":{"
            });
            for (i, (k, v)) in differ.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_key(k, out);
                let ok = match dollar {
                    true => clean_raw(v, out),
                    false => write_clean(v, out),
                };
                if !ok {
                    return false;
                }
            }
            out.push_str(if dollar { "}}" } else { "}" });
        }
        let mut absent = false;
        for (f, _) in derived
            .iter()
            .filter(|(f, _)| !refmap.iter().any(|(k, _)| k == f))
        {
            out.push_str(if absent {
                ","
            } else {
                ",\"referencesAbsent\":["
            });
            absent = true;
            write_str(f, out);
        }
        if absent {
            out.push(']');
        }
    }
    if let Some(j) = item {
        out.push_str(",\"item\":");
        out.push_str(&j.to_string());
    }
    out.push('}');
    true
}

const NODES_KEY: &str = "$nodes";

fn clean_dollar(m: &VariableMap) -> bool {
    m.iter()
        .any(|(k, _)| k.starts_with('$') && &**k != NODES_KEY)
}

fn clean_raw(v: &Variable, out: &mut String) -> bool {
    match v {
        Variable::Array(a) => {
            out.push('[');
            for (i, x) in a.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if !clean_raw(x, out) {
                    return false;
                }
            }
            out.push(']');
            true
        }
        Variable::Object(o) => clean_object(&o.borrow(), out, clean_raw),
        _ => write_raw(v, out),
    }
}

fn clean_object(
    o: &VariableMap,
    out: &mut String,
    each: fn(&Variable, &mut String) -> bool,
) -> bool {
    out.push('{');
    let mut ok = true;
    let mut first = true;
    each_entry(o, |k, x| {
        if !ok || &**k == NODES_KEY {
            return;
        }
        if !first {
            out.push(',');
        }
        first = false;
        write_key(k, out);
        ok = each(x, out);
    });
    out.push('}');
    ok
}

pub(crate) fn write_clean(v: &Variable, out: &mut String) -> bool {
    match v {
        Variable::Array(a) => {
            out.push('[');
            for (i, x) in a.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if !write_clean(x, out) {
                    return false;
                }
            }
            out.push(']');
            true
        }
        Variable::Object(o) => {
            let o = o.borrow();
            if clean_dollar(&o) {
                out.push_str("{\"$literal\":");
                if !clean_object(&o, out, clean_raw) {
                    return false;
                }
                out.push('}');
                return true;
            }
            clean_object(&o, out, write_clean)
        }
        Variable::Dynamic(_) => false,
        _ => write_raw(v, out),
    }
}

fn function_td(o: &VariableMap) -> bool {
    o.len() == 1 && matches!(o.get_str("log"), Some(Variable::Array(_)))
}
