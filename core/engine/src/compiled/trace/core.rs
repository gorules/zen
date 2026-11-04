use super::value::{eq, js_num_len, json_len, utf16_len, Key, Obj, Val};
use super::value::{FastMap as HashMap, FastSet};
use super::{bad, R};
use rust_decimal::Decimal;
use std::borrow::Cow;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Seg {
    K(Key),
    I(usize),
}

impl Seg {
    pub fn text(&self) -> Cow<'_, str> {
        match self {
            Seg::K(k) => Cow::Borrowed(k),
            Seg::I(i) => Cow::Owned(i.to_string()),
        }
    }

    pub fn js_len(&self) -> usize {
        match self {
            Seg::K(k) => utf16_len(k),
            Seg::I(i) => digits(*i),
        }
    }

    pub fn number(&self) -> Option<usize> {
        match self {
            Seg::I(i) => Some(*i),
            Seg::K(k) => js_number_index(k),
        }
    }

    pub fn key(&self) -> Key {
        match self {
            Seg::K(k) => k.clone(),
            Seg::I(i) => Rc::from(i.to_string().as_str()),
        }
    }
}

pub fn digits(i: usize) -> usize {
    i.checked_ilog10().map_or(1, |d| d as usize + 1)
}

pub fn js_number_index(k: &str) -> Option<usize> {
    let t = k.trim();
    if t.is_empty() {
        return Some(0);
    }
    if t.bytes().all(|c| c.is_ascii_digit()) {
        return t.parse().ok();
    }
    None
}

fn canonical_index(k: &str) -> Option<usize> {
    let b = k.as_bytes();
    if b.is_empty() || !b.iter().all(|c| c.is_ascii_digit()) || (b[0] == b'0' && b.len() > 1) {
        return None;
    }
    k.parse().ok()
}

pub fn key_seg(k: &str) -> Seg {
    Seg::K(Rc::from(k))
}

pub fn child<'a>(v: &'a Val, s: &Seg) -> Option<&'a Val> {
    match v {
        Val::Arr(a) => {
            let i = match s {
                Seg::I(i) => *i,
                Seg::K(k) => canonical_index(k)?,
            };
            a.get(i)
        }
        Val::Obj(o) => match s {
            Seg::K(k) => o.get(k),
            Seg::I(i) => o.get(&i.to_string()),
        },
        _ => None,
    }
}

pub fn get_path<'a>(v: &'a Val, path: &[Seg]) -> Option<&'a Val> {
    let mut cur = v;
    for s in path {
        cur = child(cur, s)?;
    }
    Some(cur)
}

pub fn get_opt<'a>(v: Option<&'a Val>, path: &[Seg]) -> Option<&'a Val> {
    get_path(v?, path)
}

pub fn dotted(s: &str) -> Vec<Seg> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('.').map(key_seg).collect()
}

pub fn place_at(path: &[Seg], value: Val) -> Val {
    let mut v = value;
    for s in path.iter().rev() {
        let mut o = Obj::new();
        o.insert(s.key(), v);
        v = Val::obj(o);
    }
    v
}

pub fn deep_merge(a: &Val, b: &Val) -> Val {
    if let (Val::Obj(x), Val::Obj(y)) = (a, b) {
        let mut o = (**x).clone();
        for (k, v) in y.iter() {
            let nv = match o.get(k) {
                Some(cur) => deep_merge(cur, v),
                None => v.clone(),
            };
            o.insert(k.clone(), nv);
        }
        return Val::obj(o);
    }
    b.clone()
}

pub fn escape_seg(s: &str, out: &mut String) {
    out.push('/');
    if !s.contains(['~', '/']) {
        out.push_str(s);
        return;
    }
    for c in s.chars() {
        match c {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            c => out.push(c),
        }
    }
}

pub fn to_ptr(path: &[Seg]) -> String {
    let mut s = String::new();
    for p in path {
        escape_seg(&p.text(), &mut s);
    }
    s
}

pub fn from_ptr(s: &str) -> R<Vec<Seg>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if !s.starts_with('/') {
        return Err(bad(format!(
            "not a JSON pointer: {}",
            Val::str(s).to_json()
        )));
    }
    Ok(s[1..]
        .split('/')
        .map(|x| {
            if x.contains('~') {
                Seg::K(Rc::from(x.replace("~1", "/").replace("~0", "~").as_str()))
            } else {
                key_seg(x)
            }
        })
        .collect())
}

pub fn from_ptr_strings(s: &str) -> R<Vec<String>> {
    Ok(from_ptr(s)?
        .into_iter()
        .map(|x| x.text().into_owned())
        .collect())
}

pub fn dot_to_ptr(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for p in s.split('.') {
        escape_seg(p, &mut out);
    }
    out
}

pub fn ptr_to_dot(p: &str) -> R<String> {
    Ok(from_ptr_strings(p)?.join("."))
}

thread_local! {
    static SIZES: std::cell::RefCell<Option<HashMap<usize, (Val, usize)>>> = const { std::cell::RefCell::new(None) };
}

const SIZE_MEMO_MIN: usize = 48;

pub struct SizeMemo(bool);

impl SizeMemo {
    pub fn start() -> Self {
        let fresh = SIZES.with(|m| {
            let mut m = m.borrow_mut();
            if m.is_some() {
                return false;
            }
            *m = Some(HashMap::default());
            true
        });
        SizeMemo(fresh)
    }
}

impl Drop for SizeMemo {
    fn drop(&mut self) {
        if self.0 {
            SIZES.with(|m| *m.borrow_mut() = None);
        }
    }
}

pub fn size_cap(v: &Val, _cap: usize) -> usize {
    size(v)
}

pub fn size(v: &Val) -> usize {
    match v {
        Val::Null => 4,
        Val::Str(s) => utf16_len(s) + 2,
        Val::Num(d) => js_num_len(d),
        Val::Bool(_) => 5,
        Val::Arr(a) => {
            let memo = !a.is_empty();
            if memo {
                if let Some(n) = SIZES.with(|m| {
                    m.borrow()
                        .as_ref()
                        .and_then(|m| m.get(&v.ptr()).map(|x| x.1))
                }) {
                    return n;
                }
            }
            let mut n = 2;
            for y in a.iter() {
                n += size(y) + 1;
            }
            if memo && n >= SIZE_MEMO_MIN {
                SIZES.with(|m| {
                    if let Some(m) = m.borrow_mut().as_mut() {
                        m.insert(v.ptr(), (v.clone(), n));
                    }
                });
            }
            n
        }
        Val::Obj(o) => {
            let memo = !o.is_empty();
            if memo {
                if let Some(n) = SIZES.with(|m| {
                    m.borrow()
                        .as_ref()
                        .and_then(|m| m.get(&v.ptr()).map(|x| x.1))
                }) {
                    return n;
                }
            }
            let mut n = 2;
            for (k, y) in o.iter() {
                n += utf16_len(k) + 4 + size(y);
            }
            if memo && n >= SIZE_MEMO_MIN {
                SIZES.with(|m| {
                    if let Some(m) = m.borrow_mut().as_mut() {
                        m.insert(v.ptr(), (v.clone(), n));
                    }
                });
            }
            n
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Set {
        path: Vec<Seg>,
        value: Val,
    },
    Del {
        path: Vec<Seg>,
    },
    Splice {
        path: Vec<Seg>,
        at: usize,
        del: usize,
        ins: Vec<Val>,
    },
}

impl Edit {
    pub fn path(&self) -> &[Seg] {
        match self {
            Edit::Set { path, .. } | Edit::Del { path } | Edit::Splice { path, .. } => path,
        }
    }
}

fn path_cost(path: &[Seg], base: usize) -> usize {
    base + path.iter().map(|s| s.js_len() + 1).sum::<usize>()
}

pub fn edit_cost(e: &Edit) -> usize {
    let mut n = path_cost(e.path(), 8);
    match e {
        Edit::Set { value, .. } => n += size(value),
        Edit::Splice { ins, .. } => {
            n += 6 + 2;
            for x in ins {
                n += size(x) + 1;
            }
        }
        Edit::Del { .. } => {}
    }
    n
}

fn same_prim(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Arr(x), Val::Arr(y)) => Rc::ptr_eq(x, y),
        (Val::Obj(x), Val::Obj(y)) => Rc::ptr_eq(x, y),
        (Val::Null, Val::Null) => true,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Num(x), Val::Num(y)) => x == y,
        (Val::Str(x), Val::Str(y)) => x == y,
        _ => false,
    }
}

fn with(path: &mut Vec<Seg>, s: Seg) -> Vec<Seg> {
    path.push(s);
    let p = path.clone();
    path.pop();
    p
}

fn diff_into(a: &Val, b: &Val, p: &mut Vec<Seg>, out: &mut Vec<Edit>) -> usize {
    if same_prim(a, b) {
        return 0;
    }
    match (a, b) {
        (Val::Obj(x), Val::Obj(y)) => {
            let mut sub = Vec::new();
            let mut cost = 0;
            for (k, _) in x.iter() {
                if !y.has(k) {
                    let e = Edit::Del {
                        path: with(p, Seg::K(k.clone())),
                    };
                    cost += edit_cost(&e);
                    sub.push(e);
                }
            }
            for (k, v) in y.iter() {
                match x.get(k) {
                    None => {
                        let e = Edit::Set {
                            path: with(p, Seg::K(k.clone())),
                            value: v.clone(),
                        };
                        cost += edit_cost(&e);
                        sub.push(e);
                    }
                    Some(av) => {
                        p.push(Seg::K(k.clone()));
                        cost += diff_into(av, v, p, &mut sub);
                        p.pop();
                    }
                }
            }
            if sub.is_empty() {
                return 0;
            }
            choose(sub, cost, b, p, out)
        }
        (Val::Arr(x), Val::Arr(y)) => {
            if x.len() == y.len() {
                let mut sub = Vec::new();
                let mut cost = 0;
                for i in 0..x.len() {
                    p.push(Seg::I(i));
                    cost += diff_into(&x[i], &y[i], p, &mut sub);
                    p.pop();
                }
                if sub.is_empty() {
                    return 0;
                }
                return choose(sub, cost, b, p, out);
            }
            let m = x.len().min(y.len());
            let mut pre = 0;
            while pre < m && eq(&x[pre], &y[pre]) {
                pre += 1;
            }
            let mut suf = 0;
            while suf < m - pre && eq(&x[x.len() - 1 - suf], &y[y.len() - 1 - suf]) {
                suf += 1;
            }
            let opt_a = vec![Edit::Splice {
                path: p.clone(),
                at: pre,
                del: x.len() - pre - suf,
                ins: y[pre..y.len() - suf].to_vec(),
            }];
            let cost_a = edit_cost(&opt_a[0]);
            let mut opt_b = Vec::new();
            let mut cost_b = 0;
            for i in pre..m {
                p.push(Seg::I(i));
                cost_b += diff_into(&x[i], &y[i], p, &mut opt_b);
                p.pop();
            }
            let tail = Edit::Splice {
                path: p.clone(),
                at: m,
                del: x.len() - m,
                ins: y[m..].to_vec(),
            };
            cost_b += edit_cost(&tail);
            opt_b.push(tail);
            if cost_a <= cost_b {
                choose(opt_a, cost_a, b, p, out)
            } else {
                choose(opt_b, cost_b, b, p, out)
            }
        }
        _ => {
            if a.is_container() && b.is_container() && eq(a, b) {
                return 0;
            }
            let e = Edit::Set {
                path: p.clone(),
                value: b.clone(),
            };
            let c = edit_cost(&e);
            out.push(e);
            c
        }
    }
}

fn choose(sub: Vec<Edit>, cost: usize, b: &Val, p: &[Seg], out: &mut Vec<Edit>) -> usize {
    let whole = path_cost(p, 8) + size_cap(b, cost);
    if whole < cost {
        out.push(Edit::Set {
            path: p.to_vec(),
            value: b.clone(),
        });
        return whole;
    }
    out.extend(sub);
    cost
}

pub fn diff(a: &Val, b: &Val) -> Vec<Edit> {
    let mut out = Vec::new();
    diff_into(a, b, &mut Vec::new(), &mut out);
    out
}

pub fn writes(a: Option<&Val>, b: &Val, p: &mut Vec<Seg>, out: &mut Vec<Edit>) -> usize {
    if let Some(a) = a {
        if same_prim(a, b) {
            return 0;
        }
    }
    let pc = path_cost(p, 10);
    match (a, b) {
        (None | Some(Val::Obj(_)), Val::Obj(y)) => {
            let ao = a.and_then(|v| v.as_obj());
            if !y.is_empty() || ao.is_some_and(|x| !x.is_empty()) {
                let mut sub = Vec::new();
                let mut lit = 0;
                if let Some(x) = ao {
                    for (k, _) in x.iter() {
                        if !y.has(k) {
                            sub.push(Edit::Del {
                                path: with(p, Seg::K(k.clone())),
                            });
                            lit += pc + utf16_len(k);
                        }
                    }
                }
                for (k, v) in y.iter() {
                    p.push(Seg::K(k.clone()));
                    lit += writes(ao.and_then(|x| x.get(k)), v, p, &mut sub);
                    p.pop();
                }
                if sub.is_empty() {
                    return 0;
                }
                if lit <= pc + size_cap(b, lit) {
                    out.extend(sub);
                    return lit;
                }
            } else if ao.is_some() {
                return 0;
            }
        }
        (Some(Val::Arr(x)), Val::Arr(y)) if x.len() == y.len() => {
            let mut sub = Vec::new();
            let mut lit = 0;
            for i in 0..y.len() {
                p.push(Seg::I(i));
                lit += writes(Some(&x[i]), &y[i], p, &mut sub);
                p.pop();
            }
            if sub.is_empty() {
                return 0;
            }
            if lit <= pc + size_cap(b, lit) {
                out.extend(sub);
                return lit;
            }
        }
        (Some(a), _) if eq(a, b) => return 0,
        _ => {}
    }
    let s = size(b);
    out.push(Edit::Set {
        path: p.clone(),
        value: b.clone(),
    });
    pc + s
}

#[derive(Default)]
struct Trie {
    kids: Vec<(String, Trie)>,
    index: HashMap<String, usize>,
    edits: Vec<Edit>,
}

impl Trie {
    fn kid(&mut self, k: String) -> &mut Trie {
        let i = match self.index.get(&k) {
            Some(&i) => i,
            None => {
                self.index.insert(k.clone(), self.kids.len());
                self.kids.push((k, Trie::default()));
                self.kids.len() - 1
            }
        };
        &mut self.kids[i].1
    }
}

pub fn apply_edits(root: &Val, edits: Vec<Edit>) -> R<Val> {
    if edits.is_empty() {
        return Ok(root.clone());
    }
    let mut trie = Trie::default();
    for e in edits {
        let mut t = &mut trie;
        for k in e.path() {
            t = t.kid(k.text().into_owned());
        }
        t.edits.push(e);
    }
    Ok(apply_trie(Some(root.clone()), trie)?.unwrap_or(Val::Null))
}

fn apply_trie(mut v: Option<Val>, t: Trie) -> R<Option<Val>> {
    let mut del = false;
    let mut splices = Vec::new();
    for e in t.edits {
        match e {
            Edit::Set { value, .. } => v = Some(value),
            Edit::Del { .. } => del = true,
            s @ Edit::Splice { .. } => splices.push(s),
        }
    }
    if v.as_ref().is_some_and(is_placeholder) && lenient() {
        return Ok(if del { None } else { v });
    }
    if !t.kids.is_empty() {
        match v {
            Some(Val::Arr(a)) => {
                let mut c: Vec<Val> = (*a).clone();
                let mut dels = Vec::new();
                for (k, kt) in t.kids {
                    let i =
                        js_number_index(&k).ok_or_else(|| bad(format!("bad array index {k}")))?;
                    let r = apply_trie(c.get(i).cloned(), kt)?;
                    match r {
                        None => dels.push(i),
                        Some(x) => {
                            if i > c.len() + 1_000_000 {
                                return Err(bad("array index far past the end"));
                            }
                            while c.len() <= i {
                                c.push(Val::Null);
                            }
                            c[i] = x;
                        }
                    }
                }
                dels.sort_unstable_by(|x, y| y.cmp(x));
                for i in dels {
                    if i < c.len() {
                        c.remove(i);
                    }
                }
                v = Some(Val::arr(c));
            }
            other => {
                let mut c = match &other {
                    Some(Val::Obj(o)) => (**o).clone(),
                    _ => Obj::new(),
                };
                for (k, kt) in t.kids {
                    let cur = c.get(&k).cloned();
                    match apply_trie(cur, kt)? {
                        None => {
                            c.remove(&k);
                        }
                        Some(x) => c.set(&k, x),
                    }
                }
                v = Some(Val::obj(c));
            }
        }
    }
    for e in splices {
        if let Edit::Splice {
            at, del: d, ins, ..
        } = e
        {
            let mut c: Vec<Val> = match &v {
                Some(Val::Arr(a)) => (**a).clone(),
                _ => return Err(bad("$splice on a value that is not a list")),
            };
            let at = at.min(c.len());
            let end = (at + d).min(c.len());
            c.splice(at..end, ins);
            v = Some(Val::arr(c));
        }
    }
    Ok(if del { None } else { v })
}

pub fn has_dollar(o: &Obj) -> bool {
    o.keys().any(|k| k.starts_with('$'))
}

pub fn is_literal(v: &Val) -> bool {
    matches!(v, Val::Obj(o) if o.len() == 1 && o.has("$literal"))
}

pub fn literal(v: Val) -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("$literal"), v);
    Val::obj(o)
}

pub fn escape(v: &Val) -> Val {
    match v {
        Val::Arr(a) => {
            let mut out: Option<Vec<Val>> = None;
            for (i, x) in a.iter().enumerate() {
                if let Some(e) = escape_changed(x) {
                    out.get_or_insert_with(|| (**a).clone())[i] = e;
                }
            }
            out.map(Val::arr).unwrap_or_else(|| v.clone())
        }
        Val::Obj(o) => {
            if has_dollar(o) {
                return literal(v.clone());
            }
            let mut out: Option<Obj> = None;
            for (k, x) in o.iter() {
                if let Some(e) = escape_changed(x) {
                    out.get_or_insert_with(|| (**o).clone()).set(k, e);
                }
            }
            out.map(Val::obj).unwrap_or_else(|| v.clone())
        }
        _ => v.clone(),
    }
}

fn escape_changed(v: &Val) -> Option<Val> {
    match v {
        Val::Arr(_) | Val::Obj(_) => {
            let e = escape(v);
            if e.ptr() == v.ptr() {
                None
            } else {
                Some(e)
            }
        }
        _ => None,
    }
}

pub fn unescape(v: &Val) -> Val {
    match v {
        Val::Arr(a) => {
            let mut out: Option<Vec<Val>> = None;
            for (i, x) in a.iter().enumerate() {
                if let Some(e) = unescape_changed(x) {
                    out.get_or_insert_with(|| (**a).clone())[i] = e;
                }
            }
            out.map(Val::arr).unwrap_or_else(|| v.clone())
        }
        Val::Obj(o) => {
            if is_literal(v) {
                return o.get("$literal").cloned().unwrap_or(Val::Null);
            }
            let mut out: Option<Obj> = None;
            for (k, x) in o.iter() {
                if let Some(e) = unescape_changed(x) {
                    out.get_or_insert_with(|| (**o).clone()).set(k, e);
                }
            }
            out.map(Val::obj).unwrap_or_else(|| v.clone())
        }
        _ => v.clone(),
    }
}

fn unescape_changed(v: &Val) -> Option<Val> {
    match v {
        Val::Arr(_) | Val::Obj(_) => {
            let e = unescape(v);
            if e.ptr() == v.ptr() {
                None
            } else {
                Some(e)
            }
        }
        _ => None,
    }
}

pub fn lit(v: &Val) -> Val {
    match v {
        Val::Obj(o) if has_dollar(o) => literal(v.clone()),
        _ => v.clone(),
    }
}

const PACK_MIN: usize = 4;

fn pack_mark(o: &Obj) -> bool {
    o.has("$runs") || o.has("$pick")
}

#[derive(Clone, Debug)]
enum Step {
    Zero,
    Num(Decimal, f64),
    Tree(Vec<(Key, Step)>),
}

fn f64_of(d: &Decimal) -> f64 {
    const POW10: [f64; 23] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
    ];
    let m = d.mantissa();
    let scale = d.scale() as usize;
    if m.unsigned_abs() < (1u128 << 53) && scale < POW10.len() {
        return (m as i64) as f64 / POW10[scale];
    }
    d.to_string().parse().unwrap_or(f64::NAN)
}

fn inc_of(a: &Val, b: &Val) -> Option<Step> {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => {
            if x == y {
                return Some(Step::Zero);
            }
            let fx = f64_of(x);
            let fy = f64_of(y);
            if fx == fy {
                return None;
            }
            let fd = fy - fx;
            if !fd.is_finite() || fx + fd != fy {
                return None;
            }
            let d = y.checked_sub(*x)?;
            if f64_of(&d) != fd {
                return None;
            }
            Some(Step::Num(d, fd))
        }
        (Val::Arr(x), Val::Arr(y)) => {
            if x.len() != y.len() {
                return None;
            }
            let mut d = Vec::new();
            for i in 0..x.len() {
                match inc_of(&x[i], &y[i])? {
                    Step::Zero => {}
                    s => d.push((Rc::from(i.to_string().as_str()), s)),
                }
            }
            Some(if d.is_empty() {
                Step::Zero
            } else {
                Step::Tree(d)
            })
        }
        (Val::Obj(x), Val::Obj(y)) => {
            if x.len() != y.len() || !x.keys().zip(y.keys()).all(|(p, q)| p == q) {
                return None;
            }
            let mut d = Vec::new();
            for ((k, p), (_, q)) in x.iter().zip(y.iter()) {
                match inc_of(p, q)? {
                    Step::Zero => {}
                    s => d.push((k.clone(), s)),
                }
            }
            Some(if d.is_empty() {
                Step::Zero
            } else {
                Step::Tree(d)
            })
        }
        (Val::Arr(_) | Val::Obj(_), _) | (_, Val::Arr(_) | Val::Obj(_)) => None,
        _ => {
            if same_prim(a, b) {
                Some(Step::Zero)
            } else {
                None
            }
        }
    }
}

fn step_val(s: &Step) -> Val {
    match s {
        Step::Zero => Val::int(0),
        Step::Num(d, _) => Val::Num(*d),
        Step::Tree(kids) => {
            let mut o = Obj::with_capacity(kids.len());
            for (k, s) in kids {
                o.insert(k.clone(), step_val(s));
            }
            Val::obj(o)
        }
    }
}

fn json_same(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Obj(x), Val::Obj(y)) => {
            Rc::ptr_eq(x, y)
                || x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|((k1, v1), (k2, v2))| k1 == k2 && json_same(v1, v2))
        }
        (Val::Arr(x), Val::Arr(y)) => {
            Rc::ptr_eq(x, y)
                || (x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| json_same(p, q)))
        }
        _ => same_prim(a, b),
    }
}

fn index_key(k: &str, i: usize) -> bool {
    k.parse::<usize>().is_ok_and(|n| n == i) && (k == "0" || !k.starts_with('0'))
}

fn gen_matches(v: &Val, d: &Step, t: usize, target: &Val) -> bool {
    match d {
        Step::Zero => json_same(v, target),
        Step::Num(dd, fd) => match (v, target) {
            (Val::Num(x), Val::Num(y)) => {
                let f = f64_of(x) + (t as f64) * fd;
                if f != f64_of(y) {
                    return false;
                }
                let Some(m) = dd.checked_mul(Decimal::from(t)) else {
                    return false;
                };
                x.checked_add(m).is_some_and(|s| s == *y)
            }
            _ => false,
        },
        Step::Tree(kids) => match (v, target) {
            (Val::Obj(x), Val::Obj(y)) => {
                x.len() == y.len()
                    && x.iter().zip(y.iter()).all(|((k, p), (tk, q))| {
                        k == tk
                            && match kids.iter().find(|(kk, _)| kk == k) {
                                Some((_, s)) => gen_matches(p, s, t, q),
                                None => json_same(p, q),
                            }
                    })
            }
            (Val::Arr(x), Val::Arr(y)) => {
                x.len() == y.len()
                    && x.iter().zip(y.iter()).enumerate().all(|(i, (p, q))| {
                        match kids.iter().find(|(kk, _)| index_key(kk, i)) {
                            Some((_, s)) => gen_matches(p, s, t, q),
                            None => json_same(p, q),
                        }
                    })
            }
            _ => false,
        },
    }
}

pub fn pack(v: &Val) -> Val {
    Packer::default().pack(v)
}

fn hmix(a: u64, b: u64) -> u64 {
    (a ^ b
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(a << 6)
        .wrapping_add(a >> 2))
    .wrapping_mul(0x0100_0000_01b3)
}

fn hstr(s: &str) -> u64 {
    let mut x: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        x = (x ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    x
}

fn hnum(d: &Decimal) -> u64 {
    if d.is_zero() {
        return 0x5bd1_e995;
    }
    if d.scale() == 0 {
        let m = d.mantissa();
        return hmix(hmix(m as u64, (m >> 64) as u64), 1);
    }
    let n = d.normalize();
    let m = n.mantissa();
    hmix(hmix(m as u64, (m >> 64) as u64), n.scale() as u64 + 1)
}

#[derive(Default)]
struct Packer {
    done: HashMap<usize, Val>,
    known: HashMap<usize, (Val, u64)>,
    lens: HashMap<usize, (Val, usize)>,
}

impl Packer {
    fn hash(&mut self, v: &Val) -> u64 {
        match v {
            Val::Null => 1,
            Val::Bool(true) => 2,
            Val::Bool(false) => 3,
            Val::Num(d) => hmix(4, hnum(d)),
            Val::Str(s) => hmix(5, hstr(s)),
            Val::Arr(a) => {
                if let Some((_, h)) = self.known.get(&v.ptr()) {
                    return *h;
                }
                let mut h: u64 = 6;
                for x in a.iter() {
                    h = hmix(h, self.hash(x));
                }
                self.known.insert(v.ptr(), (v.clone(), h));
                h
            }
            Val::Obj(o) => {
                if let Some((_, h)) = self.known.get(&v.ptr()) {
                    return *h;
                }
                let mut h: u64 = 7;
                for (k, x) in o.iter() {
                    h = hmix(hmix(h, hstr(k)), self.hash(x));
                }
                self.known.insert(v.ptr(), (v.clone(), h));
                h
            }
        }
    }

    fn len(&mut self, v: &Val) -> usize {
        match v {
            Val::Arr(a) => {
                if let Some((_, n)) = self.lens.get(&v.ptr()) {
                    return *n;
                }
                let mut n = 2 + a.len().saturating_sub(1);
                for x in a.iter() {
                    n += self.len(x);
                }
                self.lens.insert(v.ptr(), (v.clone(), n));
                n
            }
            Val::Obj(o) => {
                if let Some((_, n)) = self.lens.get(&v.ptr()) {
                    return *n;
                }
                let mut n = 2 + o.len().saturating_sub(1);
                for (k, x) in o.iter() {
                    n += super::value::js_str_quoted_len(k) + 1 + self.len(x);
                }
                self.lens.insert(v.ptr(), (v.clone(), n));
                n
            }
            _ => json_len(v),
        }
    }

    fn pack(&mut self, v: &Val) -> Val {
        let shared = match v {
            Val::Obj(o) => Rc::strong_count(o) > 1,
            Val::Arr(a) => Rc::strong_count(a) > 1,
            _ => return v.clone(),
        };
        if shared {
            if let Some(p) = self.done.get(&v.ptr()) {
                return p.clone();
            }
        }
        let out = match v {
            Val::Obj(_) if is_literal(v) => v.clone(),
            Val::Obj(o) if pack_mark(o) => literal(v.clone()),
            Val::Obj(o) => {
                let mut kids: Option<Vec<Val>> = None;
                for (i, (_, x)) in o.iter().enumerate() {
                    let px = self.pack(x);
                    if px.ptr() != x.ptr() {
                        kids.get_or_insert_with(|| o.values().cloned().collect())[i] = px;
                    }
                }
                match kids {
                    Some(kids) => {
                        let mut out = Obj::with_capacity(o.len());
                        for ((k, _), px) in o.iter().zip(kids) {
                            out.push_unique(k.clone(), px);
                        }
                        Val::obj(out)
                    }
                    None => v.clone(),
                }
            }
            Val::Arr(a) => {
                let mut items: Option<Vec<Val>> = None;
                for (i, x) in a.iter().enumerate() {
                    let px = self.pack(x);
                    if px.ptr() != x.ptr() {
                        items.get_or_insert_with(|| (**a).clone())[i] = px;
                    }
                }
                let list: &[Val] = items.as_deref().unwrap_or(a);
                let packed = match list.len() >= PACK_MIN {
                    true => {
                        let hashes: Vec<u64> = list.iter().map(|x| self.hash(x)).collect();
                        let lens: Vec<usize> = list.iter().map(|x| self.len(x)).collect();
                        pack_array(list, &hashes, &lens)
                    }
                    false => None,
                };
                match (packed, items) {
                    (Some(p), _) => p,
                    (None, Some(items)) => Val::arr(items),
                    (None, None) => v.clone(),
                }
            }
            _ => unreachable!(),
        };
        if shared {
            self.done.insert(v.ptr(), out.clone());
        }
        out
    }
}

struct Run {
    key: usize,
    n: usize,
    d: Option<Step>,
}

pub(crate) enum Packing<T, D> {
    Runs(Vec<(T, usize, Option<D>)>),
    Pick(Vec<T>, Vec<u32>),
}

pub(crate) type PackedInts = Packing<i64, i64>;
pub(crate) type PackedNums = Packing<Decimal, Decimal>;

struct Scalars<T, Value, Same, Stepper, Follows, Len, StepLen, KeyOf, Dense, Offset> {
    count: usize,
    value: Value,
    marker: std::marker::PhantomData<T>,
    same: Same,
    step: Stepper,
    follows: Follows,
    len: Len,
    step_len: StepLen,
    key: KeyOf,
    dense: Dense,
    offset: Offset,
}

impl<T, D, K, Value, Same, Stepper, Follows, Len, StepLen, KeyOf, Dense, Offset>
    Scalars<T, Value, Same, Stepper, Follows, Len, StepLen, KeyOf, Dense, Offset>
where
    Value: Fn(usize) -> T,
    D: Clone,
    K: std::hash::Hash + Eq,
    Same: Fn(usize, usize) -> bool,
    Stepper: Fn(usize) -> Option<D>,
    Follows: Fn(usize, &D, usize) -> bool,
    Len: Fn(usize) -> usize,
    StepLen: Fn(&D) -> usize,
    KeyOf: Fn(usize) -> K,
    Dense: Fn() -> Option<(i64, usize)>,
    Offset: Fn(usize, i64) -> usize,
{
    fn runs(&self, mut visit: impl FnMut(usize, usize, Option<D>)) {
        let len = self.count;
        let mut i = 0;
        while i < len {
            let mut j = i + 1;
            while j < len && (self.same)(j, i) {
                j += 1;
            }
            if j - i == 1 && i + 2 < len {
                if let Some(d) = (self.step)(i) {
                    let mut m = i + 1;
                    while m < len && (self.follows)(i, &d, m) {
                        m += 1;
                    }
                    if m - i >= 3 {
                        visit(i, m - i, Some(d));
                        i = m;
                        continue;
                    }
                }
            }
            visit(i, j - i, None);
            i = j;
        }
    }

    fn pack(&self) -> Option<Packing<T, D>> {
        let len = self.count;
        if len < PACK_MIN {
            return None;
        }
        let mut runs_size = 12usize;
        self.runs(|key, n, d| {
            runs_size +=
                (self.len)(key) + digits(n) + 4 + d.as_ref().map_or(0, |d| (self.step_len)(d) + 1);
        });
        let to_runs = || {
            let mut out = Vec::new();
            self.runs(|key, n, d| out.push(((self.value)(key), n, d)));
            Packing::Runs(out)
        };
        let plain: usize = (0..len).map(|i| (self.len)(i) + 1).sum::<usize>() + 1;
        let plain8 = plain as f64 * 0.8;
        let floor = 2 * len + 18;
        if (runs_size as f64) < plain8 && runs_size <= floor {
            return Some(to_runs());
        }
        if (floor as f64) >= plain8 && (runs_size as f64) >= plain8 {
            return None;
        }
        let mut at: Vec<u32> = Vec::with_capacity(len);
        let mut firsts: Vec<usize> = Vec::new();
        match (self.dense)() {
            Some((low, span)) => {
                let mut table = vec![u32::MAX; span];
                for i in 0..len {
                    let slot = &mut table[(self.offset)(i, low)];
                    if *slot == u32::MAX {
                        *slot = firsts.len() as u32;
                        firsts.push(i);
                    }
                    at.push(*slot);
                }
            }
            None => {
                let mut seen: HashMap<K, u32> = HashMap::default();
                for i in 0..len {
                    let next = firsts.len() as u32;
                    let k = *seen.entry((self.key)(i)).or_insert_with(|| {
                        firsts.push(i);
                        next
                    });
                    at.push(k);
                }
            }
        }
        let pick_size: usize = 16
            + firsts.iter().map(|&i| (self.len)(i) + 1).sum::<usize>()
            + at.iter().map(|&k| digits(k as usize) + 1).sum::<usize>();
        if (runs_size as f64) < plain8 && runs_size <= pick_size {
            return Some(to_runs());
        }
        if (pick_size as f64) < plain8 {
            let values = firsts.iter().map(|&i| (self.value)(i)).collect();
            return Some(Packing::Pick(values, at));
        }
        None
    }
}

fn int_len(v: i64) -> usize {
    match v {
        0 => 1,
        v => (v.unsigned_abs().ilog10() + 1) as usize + (v < 0) as usize,
    }
}

fn int_runs(ints: &[i64], mut visit: impl FnMut(usize, usize, Option<i64>)) {
    let len = ints.len();
    let mut i = 0;
    while i < len {
        let first = ints[i];
        let mut j = i + 1;
        while j < len && ints[j] == first {
            j += 1;
        }
        if j - i == 1 && i + 2 < len {
            let d = ints[i + 1] - first;
            if d != 0 {
                let mut m = i + 2;
                let mut expect = ints[i + 1] + d;
                while m < len && ints[m] == expect {
                    m += 1;
                    expect += d;
                }
                if m - i >= 3 {
                    visit(i, m - i, Some(d));
                    i = m;
                    continue;
                }
            }
        }
        visit(i, j - i, None);
        i = j;
    }
}

pub(crate) fn pack_ints(ints: &[i64]) -> Option<PackedInts> {
    let len = ints.len();
    if len < PACK_MIN {
        return None;
    }
    let (mut low, mut high, mut plain) = (i64::MAX, i64::MIN, 1usize);
    for &v in ints {
        if v.unsigned_abs() >= 1 << 40 {
            return None;
        }
        low = low.min(v);
        high = high.max(v);
        plain += int_len(v) + 1;
    }
    let mut runs_size = 12usize;
    int_runs(ints, |key, n, d| {
        runs_size += int_len(ints[key]) + digits(n) + 4 + d.map_or(0, |d| int_len(d) + 1);
    });
    let runs = || {
        let mut out = Vec::new();
        int_runs(ints, |key, n, d| out.push((ints[key], n, d)));
        Packing::Runs(out)
    };
    let plain8 = plain as f64 * 0.8;
    let floor = 2 * len + 18;
    if (runs_size as f64) < plain8 && runs_size <= floor {
        return Some(runs());
    }
    if (floor as f64) >= plain8 && (runs_size as f64) >= plain8 {
        return None;
    }
    let span = (high - low) as usize + 1;
    let mut at: Vec<u32> = Vec::with_capacity(len);
    let mut firsts: Vec<usize> = Vec::new();
    if span <= 4 * len.max(1024) {
        let mut table = vec![u32::MAX; span];
        for (i, &v) in ints.iter().enumerate() {
            let slot = &mut table[(v - low) as usize];
            if *slot == u32::MAX {
                *slot = firsts.len() as u32;
                firsts.push(i);
            }
            at.push(*slot);
        }
    } else {
        let mut seen: HashMap<i64, u32> = HashMap::default();
        for (i, &v) in ints.iter().enumerate() {
            let next = firsts.len() as u32;
            let k = *seen.entry(v).or_insert_with(|| {
                firsts.push(i);
                next
            });
            at.push(k);
        }
    }
    let pick_size: usize = 16
        + firsts.iter().map(|&i| int_len(ints[i]) + 1).sum::<usize>()
        + at.iter().map(|&k| digits(k as usize) + 1).sum::<usize>();
    if (runs_size as f64) < plain8 && runs_size <= pick_size {
        return Some(runs());
    }
    if (pick_size as f64) < plain8 {
        return Some(Packing::Pick(firsts.iter().map(|&i| ints[i]).collect(), at));
    }
    None
}

#[cfg(test)]
fn pack_ints_general(ints: &[i64]) -> Option<PackedInts> {
    if ints.iter().any(|v| v.unsigned_abs() >= 1 << 40) {
        return None;
    }
    Scalars {
        count: ints.len(),
        value: |i: usize| ints[i],
        marker: std::marker::PhantomData,
        same: |i: usize, j: usize| ints[i] == ints[j],
        step: |i: usize| match ints[i + 1] - ints[i] {
            0 => None,
            d => Some(d),
        },
        follows: |i: usize, d: &i64, m: usize| {
            ((m - i) as i64)
                .checked_mul(*d)
                .and_then(|x| x.checked_add(ints[i]))
                == Some(ints[m])
        },
        len: |i: usize| int_len(ints[i]),
        step_len: |d: &i64| int_len(*d),
        key: |i: usize| ints[i],
        dense: || {
            let low = *ints.iter().min()?;
            let high = *ints.iter().max()?;
            let span = (high - low) as usize + 1;
            (span <= 4 * ints.len().max(1024)).then_some((low, span))
        },
        offset: |i: usize, low: i64| (ints[i] - low) as usize,
    }
    .pack()
}

struct Scaled {
    ints: Vec<i64>,
    scales: Vec<u32>,
    scale: u32,
}

fn scaled_ints(count: usize, num: &impl Fn(usize) -> Decimal) -> Option<Scaled> {
    let scales: Vec<u32> = (0..count).map(|i| num(i).scale()).collect();
    let scale = *scales.iter().max()?;
    if scale > 18 {
        return None;
    }
    let mut ints = Vec::with_capacity(count);
    for (i, s) in scales.iter().enumerate() {
        let d = num(i);
        let m = d.mantissa();
        if m == 0 && d.is_sign_negative() {
            return None;
        }
        let wide = i64::try_from(m).ok()?.checked_mul(10i64.pow(scale - s))?;
        if wide.unsigned_abs() >= 1 << 53 {
            return None;
        }
        ints.push(wide);
    }
    Some(Scaled {
        ints,
        scales,
        scale,
    })
}

fn pack_scaled(scaled: &Scaled, floats: &[f64], lens: &[usize]) -> Option<PackedNums> {
    let Scaled {
        ints,
        scales,
        scale,
    } = scaled;
    let dec = |m: i64| Decimal::new(m, *scale);
    let packed = Scalars {
        count: ints.len(),
        value: |i: usize| ints[i],
        marker: std::marker::PhantomData,
        same: |i: usize, j: usize| ints[i] == ints[j] && scales[i] == scales[j],
        step: |i: usize| {
            let (fx, fy) = (floats[i], floats[i + 1]);
            if ints[i] == ints[i + 1] || fx == fy {
                return None;
            }
            let fd = fy - fx;
            if !fd.is_finite() || fx + fd != fy {
                return None;
            }
            let d = ints[i + 1].checked_sub(ints[i])?;
            (f64_of(&dec(d)) == fd).then_some(d)
        },
        follows: |i: usize, d: &i64, m: usize| {
            let t = m - i;
            let fd = f64_of(&dec(*d));
            floats[i] + (t as f64) * fd == floats[m]
                && (t as i64)
                    .checked_mul(*d)
                    .and_then(|x| x.checked_add(ints[i]))
                    == Some(ints[m])
        },
        len: |i: usize| lens[i],
        step_len: |d: &i64| js_num_len(&dec(*d)),
        key: |i: usize| (ints[i], scales[i]),
        dense: || None,
        offset: |_: usize, _: i64| 0,
    }
    .pack()?;
    Some(match packed {
        Packing::Runs(runs) => Packing::Runs(
            runs.into_iter()
                .map(|(v, n, d)| (dec(v), n, d.map(dec)))
                .collect(),
        ),
        Packing::Pick(values, at) => Packing::Pick(values.into_iter().map(dec).collect(), at),
    })
}

pub(crate) fn pack_decimals(count: usize, num: impl Fn(usize) -> Decimal) -> Option<PackedNums> {
    if count < PACK_MIN {
        return None;
    }
    if let Some(scaled) = scaled_ints(count, &num) {
        let floats: Vec<f64> = (0..count).map(|i| f64_of(&num(i))).collect();
        let lens: Vec<usize> = (0..count).map(|i| js_num_len(&num(i))).collect();
        return pack_scaled(&scaled, &floats, &lens);
    }
    pack_decimals_general(count, num)
}

fn pack_decimals_general(count: usize, num: impl Fn(usize) -> Decimal) -> Option<PackedNums> {
    let floats: Vec<f64> = (0..count).map(|i| f64_of(&num(i))).collect();
    let lens: Vec<usize> = (0..count).map(|i| js_num_len(&num(i))).collect();
    let walk = std::cell::Cell::new((usize::MAX, 0usize, 0f64, Decimal::ZERO));
    let same = |a: &Decimal, b: &Decimal| a.mantissa() == b.mantissa() && a.scale() == b.scale();
    Scalars {
        count,
        value: &num,
        marker: std::marker::PhantomData,
        same: |i: usize, j: usize| same(&num(i), &num(j)),
        step: |i: usize| {
            let (x, y): (Decimal, Decimal) = (num(i), num(i + 1));
            let (fx, fy) = (floats[i], floats[i + 1]);
            if x == y || fx == fy {
                return None;
            }
            let fd = fy - fx;
            if !fd.is_finite() || fx + fd != fy {
                return None;
            }
            let d = y.checked_sub(x)?;
            (f64_of(&d) == fd).then_some(d)
        },
        follows: |i: usize, d: &Decimal, m: usize| {
            let t = m - i;
            let (start, at, fd, last) = walk.get();
            let (fd, last) = match start == i && at + 1 == m {
                true => (fd, last.checked_add(*d)),
                false => (
                    f64_of(d),
                    d.checked_mul(Decimal::from(t))
                        .and_then(|step: Decimal| num(i).checked_add(step)),
                ),
            };
            let Some(v) = last else {
                return false;
            };
            walk.set((i, m, fd, v));
            floats[i] + (t as f64) * fd == floats[m] && v == num(m)
        },
        len: |i: usize| lens[i],
        step_len: |d: &Decimal| js_num_len(d),
        key: |i: usize| {
            let v = num(i);
            (v.mantissa(), v.scale())
        },
        dense: || None,
        offset: |_: usize, _: i64| 0,
    }
    .pack()
}

pub(crate) fn pack_array(items: &[Val], hashes: &[u64], lens: &[usize]) -> Option<Val> {
    if items.len() < PACK_MIN {
        return None;
    }
    let same = |i: usize, j: usize| hashes[i] == hashes[j] && json_same(&items[i], &items[j]);
    let len = items.len();
    let ints: Option<Vec<i64>> = items
        .iter()
        .map(|x| match x {
            Val::Num(d) if d.scale() == 0 && d.mantissa().unsigned_abs() < (1 << 40) => {
                Some(d.mantissa() as i64)
            }
            _ => None,
        })
        .collect();
    let numbers: Option<Vec<(Decimal, f64)>> = items
        .iter()
        .map(|x| match x {
            Val::Num(d) => Some((*d, f64_of(d))),
            _ => None,
        })
        .collect();
    let step_at = |i: usize| -> Option<Step> {
        if let Some(v) = &ints {
            let d = v[i + 1] - v[i];
            return Some(match d {
                0 => Step::Zero,
                d => Step::Num(Decimal::from(d), d as f64),
            });
        }
        match &numbers {
            None => inc_of(&items[i], &items[i + 1]),
            Some(ns) => {
                let ((x, fx), (y, fy)) = (ns[i], ns[i + 1]);
                if x == y {
                    return Some(Step::Zero);
                }
                if fx == fy {
                    return None;
                }
                let fd = fy - fx;
                if !fd.is_finite() || fx + fd != fy {
                    return None;
                }
                let d = y.checked_sub(x)?;
                (f64_of(&d) == fd).then_some(Step::Num(d, fd))
            }
        }
    };
    let follows = |i: usize, d: &Step, m: usize| -> bool {
        if let (Some(v), Step::Num(_, fd)) = (&ints, d) {
            return ((m - i) as i64)
                .checked_mul(*fd as i64)
                .and_then(|x| x.checked_add(v[i]))
                == Some(v[m]);
        }
        match (&numbers, d) {
            (Some(ns), Step::Num(dd, fd)) => {
                let ((x, fx), (y, fy)) = (ns[i], ns[m]);
                let t = m - i;
                if fx + (t as f64) * fd != fy {
                    return false;
                }
                dd.checked_mul(Decimal::from(t))
                    .and_then(|step| x.checked_add(step))
                    .is_some_and(|v| v == y)
            }
            _ => gen_matches(&items[i], d, m - i, &items[m]),
        }
    };
    let mut runs: Vec<Run> = Vec::new();
    let mut i = 0;
    while i < len {
        let mut j = i + 1;
        while j < len && same(j, i) {
            j += 1;
        }
        if j - i == 1 && i + 2 < len {
            if let Some(d) = step_at(i) {
                if !matches!(d, Step::Zero) {
                    let mut m = i + 1;
                    while m < len && follows(i, &d, m) {
                        m += 1;
                    }
                    if m - i >= 3 {
                        runs.push(Run {
                            key: i,
                            n: m - i,
                            d: Some(d),
                        });
                        i = m;
                        continue;
                    }
                }
            }
        }
        runs.push(Run {
            key: i,
            n: j - i,
            d: None,
        });
        i = j;
    }
    let mut values: Vec<usize> = Vec::new();
    let mut seen: HashMap<u64, usize> = HashMap::default();
    let mut clash: Vec<(u64, usize)> = Vec::new();
    let mut at: Vec<usize> = Vec::with_capacity(len);
    for i in 0..len {
        let first = seen.get(&hashes[i]).copied();
        let k = match first {
            Some(k) if json_same(&items[values[k]], &items[i]) => k,
            Some(_) => match clash
                .iter()
                .find(|(h, k)| *h == hashes[i] && json_same(&items[values[*k]], &items[i]))
            {
                Some((_, k)) => *k,
                None => {
                    values.push(i);
                    clash.push((hashes[i], values.len() - 1));
                    values.len() - 1
                }
            },
            None => {
                values.push(i);
                seen.insert(hashes[i], values.len() - 1);
                values.len() - 1
            }
        };
        at.push(k);
    }
    let plain: usize = lens.iter().map(|l| l + 1).sum::<usize>() + 1;
    let runs_size: usize = runs
        .iter()
        .map(|r| {
            lens[r.key]
                + digits(r.n)
                + 4
                + r.d
                    .as_ref()
                    .map(|d| json_len(&step_val(d)) + 1)
                    .unwrap_or(0)
        })
        .sum::<usize>()
        + 12;
    let pick_size: usize = values.iter().map(|&i| lens[i] + 1).sum::<usize>()
        + at.iter().map(|&i| digits(i) + 1).sum::<usize>()
        + 16;
    let plain8 = plain as f64 * 0.8;
    if (runs_size as f64) < plain8 && runs_size <= pick_size {
        let list = runs
            .iter()
            .map(|r| {
                let v = items[r.key].clone();
                match (&r.d, r.n) {
                    (Some(d), n) => Val::arr(vec![v, Val::int(n as i64), step_val(d)]),
                    (None, 1) => Val::arr(vec![v]),
                    (None, n) => Val::arr(vec![v, Val::int(n as i64)]),
                }
            })
            .collect();
        let mut o = Obj::with_capacity(1);
        o.insert(Rc::from("$runs"), Val::arr(list));
        return Some(Val::obj(o));
    }
    if (pick_size as f64) < plain8 {
        let mut o = Obj::with_capacity(2);
        o.insert(
            Rc::from("$pick"),
            Val::arr(values.iter().map(|&i| items[i].clone()).collect()),
        );
        o.insert(
            Rc::from("at"),
            Val::arr(at.iter().map(|&i| Val::int(i as i64)).collect()),
        );
        return Some(Val::obj(o));
    }
    None
}

fn gen(v: &Val, d: &Val, t: usize) -> R<Val> {
    match d {
        Val::Num(dd) => match v {
            Val::Num(x) => {
                let m = dd
                    .checked_mul(Decimal::from(t))
                    .ok_or_else(|| bad("$runs step overflow"))?;
                Ok(Val::Num(
                    x.checked_add(m).ok_or_else(|| bad("$runs step overflow"))?,
                ))
            }
            _ => Err(bad("$runs step on a value that is not a number")),
        },
        Val::Obj(steps) => match v {
            Val::Arr(a) => {
                let mut c = (**a).clone();
                for (k, s) in steps.iter() {
                    let i = js_number_index(k).ok_or_else(|| bad("$runs step key"))?;
                    let cur = c.get(i).cloned().ok_or_else(|| bad("$runs step key"))?;
                    c[i] = gen(&cur, s, t)?;
                }
                Ok(Val::arr(c))
            }
            Val::Obj(o) => {
                let mut c = (**o).clone();
                for (k, s) in steps.iter() {
                    let cur = c.get(k).cloned().ok_or_else(|| bad("$runs step key"))?;
                    c.set(k, gen(&cur, s, t)?);
                }
                Ok(Val::obj(c))
            }
            _ => Err(bad("$runs step tree on a scalar")),
        },
        _ => Err(bad("$runs step must be a number or a tree")),
    }
}

pub fn unpack(v: &Val) -> R<Val> {
    match v {
        Val::Arr(a) => Ok(Val::arr(a.iter().map(unpack).collect::<R<Vec<_>>>()?)),
        Val::Obj(o) => {
            if is_literal(v) {
                return Ok(v.clone());
            }
            if let Some(runs) = o.get("$runs") {
                let runs = runs.as_arr().ok_or_else(|| bad("$runs must be a list"))?;
                let mut out = Vec::new();
                for r in runs.iter() {
                    let r = r
                        .as_arr()
                        .ok_or_else(|| bad("$runs entry must be a list"))?;
                    let x = r.first().ok_or_else(|| bad("empty $runs entry"))?;
                    let n = match r.get(1) {
                        Some(n) => n.as_usize().ok_or_else(|| bad("$runs count"))?,
                        None => 1,
                    };
                    if n > 10_000_000 || out.len() + n > 50_000_000 {
                        return Err(bad("$runs count too large"));
                    }
                    match r.get(2) {
                        Some(d) => {
                            for t in 0..n {
                                out.push(unpack(&gen(x, d, t)?)?);
                            }
                        }
                        None => {
                            let u = unpack(x)?;
                            for _ in 0..n {
                                out.push(u.clone());
                            }
                        }
                    }
                }
                return Ok(Val::arr(out));
            }
            if let Some(p) = o.get("$pick") {
                let vals = unpack(p)?;
                let vals = vals.as_arr().ok_or_else(|| bad("$pick must be a list"))?;
                let at = unpack(o.get("at").unwrap_or(&Val::Null))?;
                let at = at.as_arr().ok_or_else(|| bad("$pick needs at"))?;
                let mut out = Vec::with_capacity(at.len());
                for i in at.iter() {
                    let i = i.as_usize().ok_or_else(|| bad("$pick index"))?;
                    out.push(
                        vals.get(i)
                            .cloned()
                            .ok_or_else(|| bad("$pick index out of range"))?,
                    );
                }
                return Ok(Val::arr(out));
            }
            let mut out = Obj::with_capacity(o.len());
            for (k, x) in o.iter() {
                out.push_unique(k.clone(), unpack(x)?);
            }
            Ok(Val::obj(out))
        }
        _ => Ok(v.clone()),
    }
}

pub fn ref_out(source: &str, segs: &[Seg], items: Option<Vec<usize>>) -> Val {
    let mut o = Obj::with_capacity(2);
    o.insert(
        Rc::from("$ref"),
        Val::str(&format!("{}{}", source, to_ptr(segs))),
    );
    if let Some(items) = items {
        o.insert(
            Rc::from("items"),
            Val::arr(items.into_iter().map(|i| Val::int(i as i64)).collect()),
        );
    }
    Val::obj(o)
}

pub fn is_ref(v: &Val) -> bool {
    match v {
        Val::Obj(o) => {
            matches!(o.get("$ref"), Some(Val::Str(_)))
                && o.keys().all(|k| &**k == "$ref" || &**k == "items")
        }
        _ => false,
    }
}

pub struct RefIn {
    pub source: String,
    pub segs: Vec<Seg>,
    pub items: Option<Val>,
}

pub fn ref_in(v: &Val) -> R<RefIn> {
    let r = v
        .get("$ref")
        .and_then(|x| x.as_str())
        .ok_or_else(|| bad("bad $ref"))?;
    let cut = r.find(|c: char| !c.is_ascii_lowercase()).unwrap_or(r.len());
    if cut == 0 || (cut < r.len() && !r[cut..].starts_with('/')) {
        return Err(bad(format!("bad $ref {r}")));
    }
    Ok(RefIn {
        source: r[..cut].to_string(),
        segs: from_ptr(&r[cut..])?,
        items: v.get("items").cloned(),
    })
}

pub fn pick_items(arr: Option<&Val>, items: &Option<Val>) -> R<Option<Val>> {
    match items {
        None => Ok(arr.cloned()),
        Some(Val::Arr(list)) => {
            let src = arr.and_then(|a| a.as_arr());
            let mut out = Vec::with_capacity(list.len());
            for i in list.iter() {
                let i = i.as_usize().ok_or_else(|| bad("$ref items index"))?;
                out.push(src.and_then(|s| s.get(i)).cloned().unwrap_or(Val::Null));
            }
            Ok(Some(Val::arr(out)))
        }
        Some(_) => Err(bad("$ref items must be a list")),
    }
}

fn fnv(s: &str) -> u32 {
    let mut x: u32 = 2166136261;
    for c in s.encode_utf16() {
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

pub struct Measure {
    memo: HashMap<usize, (Val, u32, usize)>,
    text: String,
}

impl Measure {
    pub fn new() -> Self {
        Self {
            memo: HashMap::default(),
            text: String::new(),
        }
    }

    pub fn get(&mut self, v: &Val) -> (u32, usize) {
        match v {
            Val::Null => (1, 4),
            Val::Str(s) => (mix(fnv(s), 2), utf16_len(s) + 2),
            Val::Num(d) => {
                super::value::js_num_into(d, &mut self.text);
                (mix(fnv(&self.text), 3), self.text.len())
            }
            Val::Bool(b) => (if *b { 5 } else { 7 }, 5),
            Val::Arr(a) => {
                let p = v.ptr();
                if let Some((_, h, n)) = self.memo.get(&p) {
                    return (*h, *n);
                }
                let mut h = 11;
                let mut n = 2;
                for e in a.iter() {
                    let (eh, en) = self.get(e);
                    h = mix(h, eh);
                    n += en + 1;
                }
                self.memo.insert(p, (v.clone(), h, n));
                (h, n)
            }
            Val::Obj(o) => {
                let p = v.ptr();
                if let Some((_, h, n)) = self.memo.get(&p) {
                    return (*h, *n);
                }
                let mut keys: Vec<&Key> = o.keys().collect();
                keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                let mut h = 13;
                let mut n = 2;
                for k in keys {
                    let (eh, en) = self.get(o.get(k).unwrap());
                    h = mix(mix(h, fnv(k)), eh);
                    n += en + utf16_len(k) + 4;
                }
                self.memo.insert(p, (v.clone(), h, n));
                (h, n)
            }
        }
    }
}

pub const REF_MIN: usize = 64;

pub struct Refs {
    request: Val,
    nodes: Option<Obj>,
    node_outputs: Vec<(Key, Val)>,
    min: usize,
    index: Option<HashMap<u32, (u8, Vec<Seg>)>>,
    pending: Vec<(u8, Val, Vec<Seg>)>,
    measure: Measure,
    walked: FastSet<usize>,
}

const SRC_REQUEST: u8 = 0;
const SRC_NODES: u8 = 1;

impl Refs {
    pub fn new(request: &Val, nodes: bool, min: usize) -> Self {
        Self {
            request: request.clone(),
            nodes: if nodes { Some(Obj::new()) } else { None },
            node_outputs: Vec::new(),
            min,
            index: None,
            pending: vec![(SRC_REQUEST, request.clone(), Vec::new())],
            measure: Measure::new(),
            walked: FastSet::default(),
        }
    }

    pub fn add_node(&mut self, id: &Key, output: &Val) {
        let Some(nodes) = self.nodes.as_mut() else {
            return;
        };
        if nodes.has(id) {
            return;
        }
        let mut o = Obj::with_capacity(1);
        o.insert(Rc::from("output"), output.clone());
        nodes.insert(id.clone(), Val::obj(o));
        self.node_outputs.push((id.clone(), output.clone()));
        if output.is_container() {
            self.pending.push((
                SRC_NODES,
                output.clone(),
                vec![Seg::K(id.clone()), key_seg("output")],
            ));
        }
    }

    fn source_get(&self, src: u8, path: &[Seg]) -> Option<&Val> {
        match src {
            SRC_REQUEST => get_path(&self.request, path),
            _ => {
                let (id, rest) = path.split_first()?;
                let id = id.text();
                let (o, rest) = rest.split_first()?;
                if o.text() != "output" {
                    return None;
                }
                let out = self
                    .node_outputs
                    .iter()
                    .find(|(k, _)| **k == *id)
                    .map(|(_, v)| v)?;
                get_path(out, rest)
            }
        }
    }

    fn source_placeholder(&self, path: &[Seg]) -> Option<Val> {
        if !lenient() {
            return None;
        }
        let (id, rest) = path.split_first()?;
        let id = id.text();
        let (o, rest) = rest.split_first()?;
        if o.text() != "output" {
            return None;
        }
        let out = self
            .node_outputs
            .iter()
            .find(|(k, _)| **k == *id)
            .map(|(_, v)| v)?;
        placeholder_on(out, rest)
    }

    fn build(&mut self) {
        let mut index = self.index.take().unwrap_or_default();
        let todo = std::mem::take(&mut self.pending);
        for (src, doc, base) in todo {
            let mut path = base;
            self.walk_index(&mut index, src, &doc, &mut path);
        }
        self.index = Some(index);
    }

    fn walk_index(
        &mut self,
        index: &mut HashMap<u32, (u8, Vec<Seg>)>,
        src: u8,
        v: &Val,
        path: &mut Vec<Seg>,
    ) {
        if !v.is_container() {
            return;
        }
        let (h, n) = self.measure.get(v);
        if !self.walked.insert(v.ptr()) {
            return;
        }
        if n > self.min && !index.contains_key(&h) {
            index.insert(h, (src, path.clone()));
        }
        match v {
            Val::Arr(a) => {
                for (i, e) in a.iter().enumerate() {
                    path.push(Seg::I(i));
                    self.walk_index(index, src, e, path);
                    path.pop();
                }
            }
            Val::Obj(o) => {
                for (k, e) in o.iter() {
                    path.push(Seg::K(k.clone()));
                    self.walk_index(index, src, e, path);
                    path.pop();
                }
            }
            _ => {}
        }
    }

    pub fn encode(&mut self, v: &Val) -> Val {
        let mut mine: HashMap<u32, Vec<Seg>> = HashMap::default();
        let mut path = Vec::new();
        self.encode_walk(v, v, &mut path, &mut mine)
    }

    fn encode_walk(
        &mut self,
        root: &Val,
        x: &Val,
        path: &mut Vec<Seg>,
        mine: &mut HashMap<u32, Vec<Seg>>,
    ) -> Val {
        if !x.is_container() {
            return x.clone();
        }
        if let Val::Obj(o) = x {
            if has_dollar(o) {
                return literal(x.clone());
            }
        }
        let (h, n) = self.measure.get(x);
        if n > self.min {
            if let Some(p) = mine.get(&h) {
                if get_path(root, p).is_some_and(|t| eq(t, x)) {
                    return ref_out("self", p, None);
                }
            }
            if self.index.is_none() || !self.pending.is_empty() {
                self.build();
            }
            if let Some((src, p)) = self.index.as_ref().and_then(|m| m.get(&h)).cloned() {
                if self.source_get(src, &p).is_some_and(|t| eq(t, x)) {
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
        }
        let out = match x {
            Val::Arr(a) => {
                let mut out = Vec::with_capacity(a.len());
                for (i, e) in a.iter().enumerate() {
                    path.push(Seg::I(i));
                    out.push(self.encode_walk(root, e, path, mine));
                    path.pop();
                }
                Val::arr(out)
            }
            Val::Obj(o) => {
                let mut out = Obj::with_capacity(o.len());
                for (k, e) in o.iter() {
                    path.push(Seg::K(k.clone()));
                    let ev = self.encode_walk(root, e, path, mine);
                    path.pop();
                    out.push_unique(k.clone(), ev);
                }
                Val::obj(out)
            }
            _ => unreachable!(),
        };
        if n > self.min {
            mine.entry(h).or_insert_with(|| path.clone());
        }
        out
    }

    pub fn decode(&self, x: &Val) -> R<Val> {
        let mut pending: Vec<(Vec<Seg>, Vec<Seg>)> = Vec::new();
        let mut path = Vec::new();
        let mut root = self.decode_build(x, &mut path, &mut pending)?;
        if pending.is_empty() {
            return Ok(root);
        }
        let pend: Vec<(Vec<String>, Vec<String>)> = pending
            .into_iter()
            .map(|(l, t)| {
                (
                    l.iter().map(|x| x.text().into_owned()).collect(),
                    t.iter().map(|x| x.text().into_owned()).collect(),
                )
            })
            .collect();
        let mut order: Vec<usize> = (0..pend.len()).collect();
        order.sort_by(|&a, &b| pend[a].0.cmp(&pend[b].0));
        let sorted: Vec<&Vec<String>> = order.iter().map(|&i| &pend[i].0).collect();
        let mut state = vec![0u8; pend.len()];
        for i in 0..pend.len() {
            resolve_self(i, &pend, &order, &sorted, &mut state, &mut root)?;
        }
        Ok(root)
    }

    fn decode_build(
        &self,
        y: &Val,
        path: &mut Vec<Seg>,
        pending: &mut Vec<(Vec<Seg>, Vec<Seg>)>,
    ) -> R<Val> {
        match y {
            Val::Arr(a) => {
                let mut out = Vec::with_capacity(a.len());
                for (i, e) in a.iter().enumerate() {
                    path.push(Seg::I(i));
                    out.push(self.decode_build(e, path, pending)?);
                    path.pop();
                }
                Ok(Val::arr(out))
            }
            Val::Obj(o) => {
                if is_literal(y) {
                    return Ok(o.get("$literal").cloned().unwrap_or(Val::Null));
                }
                if is_ref(y) {
                    let r = ref_in(y)?;
                    return match r.source.as_str() {
                        "self" => {
                            pending.push((path.clone(), r.segs));
                            Ok(Val::Null)
                        }
                        "request" => match get_path(&self.request, &r.segs) {
                            None => Ok(placeholder_on(&self.request, &r.segs).unwrap_or(Val::Null)),
                            found => Ok(pick_items(found, &r.items)?.unwrap_or(Val::Null)),
                        },
                        "nodes" if self.nodes.is_some() => {
                            match self.source_get(SRC_NODES, &r.segs) {
                                None => Ok(self.source_placeholder(&r.segs).unwrap_or(Val::Null)),
                                found => Ok(pick_items(found, &r.items)?.unwrap_or(Val::Null)),
                            }
                        }
                        s => Err(bad(format!("unknown $ref source {s}"))),
                    };
                }
                let mut out = Obj::with_capacity(o.len());
                for (k, e) in o.iter() {
                    path.push(Seg::K(k.clone()));
                    let v = self.decode_build(e, path, pending)?;
                    path.pop();
                    out.push_unique(k.clone(), v);
                }
                Ok(Val::obj(out))
            }
            _ => Ok(y.clone()),
        }
    }
}

fn resolve_self(
    i: usize,
    pend: &[(Vec<String>, Vec<String>)],
    order: &[usize],
    sorted: &[&Vec<String>],
    state: &mut [u8],
    root: &mut Val,
) -> R<()> {
    match state[i] {
        2 => return Ok(()),
        1 => return Err(bad("circular $ref")),
        _ => {}
    }
    state[i] = 1;
    let target = &pend[i].1;
    for d in 0..=target.len() {
        let pre = &target[..d];
        if let Ok(at) = sorted.binary_search_by(|x| x.as_slice().cmp(pre)) {
            resolve_self(order[at], pend, order, sorted, state, root)?;
        }
    }
    let lo = sorted.partition_point(|x| x.as_slice() < target.as_slice());
    let mut at = lo;
    while at < sorted.len()
        && sorted[at].len() >= target.len()
        && sorted[at][..target.len()] == target[..]
    {
        resolve_self(order[at], pend, order, sorted, state, root)?;
        at += 1;
    }
    let segs: Vec<Seg> = target.iter().map(|x| key_seg(x)).collect();
    let v = get_path(root, &segs)
        .cloned()
        .or_else(|| placeholder_on(root, &segs))
        .ok_or_else(|| bad("self $ref to a missing value"))?;
    set_in_place(root, &pend[i].0, v)?;
    state[i] = 2;
    Ok(())
}

fn set_in_place(root: &mut Val, path: &[String], v: Val) -> R<()> {
    let mut cur = root;
    for k in path {
        cur = match cur {
            Val::Arr(a) => {
                let i = js_number_index(k).ok_or_else(|| bad("bad index"))?;
                Rc::make_mut(a).get_mut(i).ok_or_else(|| bad("bad index"))?
            }
            Val::Obj(o) => Rc::make_mut(o)
                .get_mut_val(k)
                .ok_or_else(|| bad("bad key"))?,
            _ => return Err(bad("bad path")),
        };
    }
    *cur = v;
    Ok(())
}

pub const BAG_OPS: [&str; 7] = [
    "$from", "$merge", "$set", "$delete", "$splice", "$each", "$same",
];

pub fn is_bag(v: &Val) -> bool {
    match v {
        Val::Obj(o) => !o.is_empty() && o.keys().all(|k| BAG_OPS.contains(&&**k)),
        _ => false,
    }
}

fn number_of_seg(s: &Seg) -> Option<usize> {
    match s {
        Seg::I(i) => Some(*i),
        Seg::K(k) => js_number_index(k),
    }
}

pub fn build_bag(edits: Vec<Edit>, base: &Val, target: &Val) -> Obj {
    let mut bag = Obj::new();
    let mut sets: Vec<(Vec<Seg>, Val)> = Vec::new();
    let mut deletes: Vec<Val> = Vec::new();
    let mut splices = Obj::new();
    for e in edits {
        match e {
            Edit::Del { path } => {
                if !bag.has("$delete") {
                    bag.insert(Rc::from("$delete"), Val::Null);
                }
                deletes.push(Val::str(&to_ptr(&path)));
            }
            Edit::Splice { path, at, del, ins } => {
                if !bag.has("$splice") {
                    bag.insert(Rc::from("$splice"), Val::Null);
                }
                let mut t = vec![Val::int(at as i64), Val::int(del as i64)];
                t.extend(ins);
                splices.set(&to_ptr(&path), Val::arr(t));
            }
            Edit::Set { path, value } => sets.push((path, value)),
        }
    }
    if bag.has("$delete") {
        bag.set("$delete", Val::arr(deletes));
    }
    if bag.has("$splice") {
        bag.set("$splice", Val::obj(splices));
    }
    let (each, rest) = each_groups(sets, target, base);
    if !each.is_empty() {
        bag.insert(Rc::from("$each"), Val::obj(each));
    }
    if !rest.is_empty() {
        let mut s = Obj::with_capacity(rest.len());
        for (p, v) in rest {
            s.set(&to_ptr(&p), v);
        }
        bag.insert(Rc::from("$set"), Val::obj(s));
    }
    bag
}

struct Group {
    prefix: String,
    suffix: String,
    idx: HashMap<usize, Val>,
    len: usize,
}

fn each_groups(
    sets: Vec<(Vec<Seg>, Val)>,
    target: &Val,
    base: &Val,
) -> (Obj, Vec<(Vec<Seg>, Val)>) {
    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut gpos: HashMap<String, usize> = HashMap::default();
    for (path, value) in &sets {
        for d in 0..path.len() {
            let prefix = &path[..d];
            let Some(Val::Arr(arr)) = get_path(target, prefix) else {
                continue;
            };
            match get_path(base, prefix) {
                Some(Val::Arr(b)) if b.len() == arr.len() && arr.len() >= 4 => {}
                _ => break,
            }
            let pp = to_ptr(prefix);
            let sp = to_ptr(&path[d + 1..]);
            let gk = format!("set\u{0}{}\u{0}{}", pp, sp);
            let gi = match gpos.get(&gk) {
                Some(&i) => i,
                None => {
                    groups.push((
                        gk.clone(),
                        Group {
                            prefix: pp,
                            suffix: sp,
                            idx: HashMap::default(),
                            len: arr.len(),
                        },
                    ));
                    gpos.insert(gk, groups.len() - 1);
                    groups.len() - 1
                }
            };
            let n = number_of_seg(&path[d]).unwrap_or(usize::MAX);
            let g = &mut groups[gi].1;
            g.idx.insert(n, value.clone());
            break;
        }
    }
    let mut each = Obj::new();
    let mut used: FastSet<String> = FastSet::default();
    for (gk, g) in groups {
        if g.idx.len() != g.len {
            continue;
        }
        let mut col = vec![Val::Null; g.len];
        for (i, v) in g.idx {
            if i < g.len {
                col[i] = v;
            }
        }
        let mut slot = match each.get(&g.prefix) {
            Some(Val::Obj(o)) => (**o).clone(),
            _ => Obj::new(),
        };
        let mut set = match slot.get("$set") {
            Some(Val::Obj(o)) => (**o).clone(),
            _ => Obj::new(),
        };
        set.set(&g.suffix, Val::arr(col));
        slot.set("$set", Val::obj(set));
        each.set(&g.prefix, Val::obj(slot));
        used.insert(gk);
    }
    if used.is_empty() {
        return (each, sets);
    }
    let rest = sets
        .into_iter()
        .filter(|(path, _)| {
            for d in 0..path.len() {
                if matches!(get_path(target, &path[..d]), Some(Val::Arr(_))) {
                    let k = format!(
                        "set\u{0}{}\u{0}{}",
                        to_ptr(&path[..d]),
                        to_ptr(&path[d + 1..])
                    );
                    return !used.contains(&k);
                }
            }
            true
        })
        .collect();
    (each, rest)
}

pub fn apply_bag(base: &Val, bag: &Obj) -> R<Val> {
    let mut edits = Vec::new();
    if let Some(d) = bag.get("$delete") {
        for p in d
            .as_arr()
            .ok_or_else(|| bad("$delete must be a list"))?
            .iter()
        {
            edits.push(Edit::Del {
                path: from_ptr(p.as_str().ok_or_else(|| bad("$delete entry"))?)?,
            });
        }
    }
    if let Some(s) = bag.get("$splice") {
        for (p, t) in s
            .as_obj()
            .ok_or_else(|| bad("$splice must be a map"))?
            .iter()
        {
            let t = t.as_arr().ok_or_else(|| bad("$splice entry"))?;
            if t.len() < 2 {
                return Err(bad("$splice entry"));
            }
            edits.push(Edit::Splice {
                path: from_ptr(p)?,
                at: t[0].as_usize().ok_or_else(|| bad("$splice at"))?,
                del: t[1].as_usize().ok_or_else(|| bad("$splice count"))?,
                ins: t[2..].to_vec(),
            });
        }
    }
    if let Some(s) = bag.get("$set") {
        for (p, v) in s.as_obj().ok_or_else(|| bad("$set must be a map"))?.iter() {
            edits.push(Edit::Set {
                path: from_ptr(p)?,
                value: v.clone(),
            });
        }
    }
    if bag.has("$same") {
        return Err(bad("$same is only used by draft documents"));
    }
    if let Some(e) = bag.get("$each") {
        for (p, slot) in e.as_obj().ok_or_else(|| bad("$each must be a map"))?.iter() {
            let prefix = from_ptr(p)?;
            let n = match get_path(base, &prefix) {
                Some(Val::Arr(a)) => a.len(),
                Some(_) => 0,
                None if placeholder_on(base, &prefix).is_some() => 0,
                None => return Err(bad("$each on a missing list")),
            };
            if slot.has("$same") {
                return Err(bad("$same is only used by draft documents"));
            }
            if let Some(set) = slot.get("$set") {
                for (s, col) in set
                    .as_obj()
                    .ok_or_else(|| bad("$each $set must be a map"))?
                    .iter()
                {
                    let col = col
                        .as_arr()
                        .ok_or_else(|| bad("$each column must be a list"))?;
                    let suffix = from_ptr(s)?;
                    for i in 0..n {
                        let mut path = prefix.clone();
                        path.push(Seg::I(i));
                        path.extend(suffix.iter().cloned());
                        edits.push(Edit::Set {
                            path,
                            value: col
                                .get(i)
                                .cloned()
                                .ok_or_else(|| bad("$each column too short"))?,
                        });
                    }
                }
            }
        }
    }
    apply_edits(base, edits)
}

pub fn map_bag_values(bag: &Obj, f: &mut dyn FnMut(&Val) -> R<Val>) -> R<Obj> {
    let mut out = bag.clone();
    if let Some(Val::Obj(s)) = bag.get("$set") {
        let mut o = Obj::with_capacity(s.len());
        for (k, v) in s.iter() {
            o.insert(k.clone(), f(v)?);
        }
        out.set("$set", Val::obj(o));
    }
    if let Some(Val::Obj(s)) = bag.get("$splice") {
        let mut o = Obj::with_capacity(s.len());
        for (k, t) in s.iter() {
            let t = t.as_arr().ok_or_else(|| bad("$splice entry"))?;
            let mut nt = Vec::with_capacity(t.len());
            for (i, x) in t.iter().enumerate() {
                nt.push(if i < 2 { x.clone() } else { f(x)? });
            }
            o.insert(k.clone(), Val::arr(nt));
        }
        out.set("$splice", Val::obj(o));
    }
    if let Some(Val::Obj(e)) = bag.get("$each") {
        let mut o = Obj::with_capacity(e.len());
        for (k, slot) in e.iter() {
            match slot.get("$set") {
                Some(Val::Obj(set)) => {
                    let mut ns = Obj::with_capacity(set.len());
                    for (sk, col) in set.iter() {
                        ns.insert(sk.clone(), f(col)?);
                    }
                    let mut so = slot.as_obj().cloned().unwrap_or_default();
                    so.set("$set", Val::obj(ns));
                    o.insert(k.clone(), Val::obj(so));
                }
                _ => o.insert(k.clone(), slot.clone()),
            }
        }
        out.set("$each", Val::obj(o));
    }
    Ok(out)
}

const UNIT_NS: [(&str, f64); 4] = [("ns", 1.0), ("µs", 1e3), ("ms", 1e6), ("s", 1e9)];

pub fn fmt_ns(n: u64) -> String {
    let (unit, suffix) = match n {
        0..1_000 => return format!("{n}.0ns"),
        1_000..1_000_000 => (1_000, "µs"),
        1_000_000..1_000_000_000 => (1_000_000, "ms"),
        _ => (1_000_000_000, "s"),
    };
    let mut q = n / unit;
    let r = n % unit;
    let mut t = r * 10 / unit;
    let rem = r * 10 - t * unit;
    if rem * 2 > unit || (rem * 2 == unit && t % 2 == 1) {
        t += 1;
    }
    if t == 10 {
        t = 0;
        q += 1;
    }
    format!("{q}.{t}{suffix}")
}

fn parse_perf(s: &str) -> Option<(&str, f64)> {
    for (u, mult) in UNIT_NS {
        if let Some(num) = s.strip_suffix(u) {
            let (int, frac) = match num.split_once('.') {
                Some((a, b)) => (a, Some(b)),
                None => (num, None),
            };
            if int.is_empty() || !int.bytes().all(|c| c.is_ascii_digit()) {
                if u == "s" {
                    continue;
                }
                return None;
            }
            if let Some(f) = frac {
                if f.len() != 1 || !f.as_bytes()[0].is_ascii_digit() {
                    return None;
                }
            }
            return Some((num, mult));
        }
    }
    None
}

#[cfg(test)]
fn ns_text(n: u64, out: &mut [u8; 40]) -> usize {
    let (unit, suffix): (u64, &[u8]) = match n {
        0..1_000 => (1, b"ns"),
        1_000..1_000_000 => (1_000, "\u{b5}s".as_bytes()),
        1_000_000..1_000_000_000 => (1_000_000, b"ms"),
        _ => (1_000_000_000, b"s"),
    };
    let (mut q, mut t) = (n / unit, 0);
    if unit > 1 {
        let r = n % unit;
        t = r * 10 / unit;
        let rem = r * 10 - t * unit;
        if rem * 2 > unit || (rem * 2 == unit && t % 2 == 1) {
            t += 1;
        }
        if t == 10 {
            t = 0;
            q += 1;
        }
    }
    let mut digits = [0u8; 20];
    let mut d = 0;
    loop {
        digits[d] = b'0' + (q % 10) as u8;
        d += 1;
        q /= 10;
        if q == 0 {
            break;
        }
    }
    let mut at = 0;
    while d > 0 {
        d -= 1;
        out[at] = digits[d];
        at += 1;
    }
    out[at] = b'.';
    out[at + 1] = b'0' + t as u8;
    at += 2;
    out[at..at + suffix.len()].copy_from_slice(suffix);
    at + suffix.len()
}

pub fn duration_perf(d: std::time::Duration) -> Result<u64, String> {
    let secs = d.as_secs();
    let nanos = d.subsec_nanos() as u64;
    let (whole, frac, divisor, unit) = if secs > 0 {
        (secs, nanos, 100_000_000u64, 1_000_000_000u64)
    } else if nanos >= 1_000_000 {
        (nanos / 1_000_000, nanos % 1_000_000, 100_000, 1_000_000)
    } else if nanos >= 1_000 {
        (nanos / 1_000, nanos % 1_000, 100, 1_000)
    } else {
        return Ok(nanos);
    };
    let mut digit = frac / divisor;
    let rest = frac % divisor;
    let mut whole = whole;
    if rest * 2 > divisor || (rest * 2 == divisor && digit % 2 == 1) {
        digit += 1;
        if digit == 10 {
            digit = 0;
            whole += 1;
        }
    }
    if unit < 1_000_000_000 && whole >= 1_000 {
        return Err(format!("{d:.1?}"));
    }
    Ok(whole * unit + digit * (unit / 10))
}

#[cfg(test)]
pub fn perf_ns(s: &str) -> Option<u64> {
    let (num, mult) = parse_perf(s)?;
    let x: f64 = num.parse().ok()?;
    let n = (x * mult + 0.5).floor();
    if !n.is_finite() || !(0.0..=9.0e15).contains(&n) {
        return None;
    }
    let n = n as u64;
    let mut buf = [0u8; 40];
    let len = ns_text(n, &mut buf);
    (&buf[..len] == s.as_bytes()).then_some(n)
}

pub fn perf_out(v: &Val) -> Val {
    let Val::Str(s) = v else {
        let mut o = Obj::with_capacity(1);
        o.insert(Rc::from("value"), escape(v));
        return Val::obj(o);
    };
    let Some((num, mult)) = parse_perf(s) else {
        return v.clone();
    };
    let x: f64 = num.parse().unwrap_or(f64::NAN);
    let n = (x * mult + 0.5).floor();
    if !n.is_finite() || !(0.0..=9.0e15).contains(&n) {
        return v.clone();
    }
    let n = n as u64;
    if fmt_ns(n) == **s {
        Val::int(n as i64)
    } else {
        v.clone()
    }
}

pub fn perf_in(v: &Val) -> R<Val> {
    match v {
        Val::Num(_) => Ok(Val::str(&fmt_ns(
            v.as_usize()
                .ok_or_else(|| bad("perf must be a non-negative integer"))? as u64,
        ))),
        Val::Str(_) => Ok(v.clone()),
        Val::Obj(o) => Ok(unescape(o.get("value").unwrap_or(&Val::Null))),
        _ => Err(bad("bad perf")),
    }
}

pub fn canon(v: &Val, out: &mut String) {
    match v {
        Val::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon(x, out);
            }
            out.push(']');
        }
        Val::Obj(o) => {
            let mut keys: Vec<&Key> = o.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                super::value::write_str(k, out);
                out.push(':');
                canon(o.get(k).unwrap(), out);
            }
            out.push('}');
        }
        _ => super::value::write_json(v, out),
    }
}

pub const TAGS: [&str; 15] = [
    "$from",
    "$merge",
    "$set",
    "$same",
    "$delete",
    "$splice",
    "$each",
    "$ref",
    "$literal",
    "$runs",
    "$pick",
    "$writer",
    "$items",
    REDACTED_TAG,
    OMITTED_TAG,
];

pub const REDACTED_TAG: &str = "$redacted";
pub const OMITTED_TAG: &str = "$omitted";
pub const RESULT_ROWS: &str = "result-rows";
pub const REDACTED: &str = "redacted";
pub const TRUNCATED: &str = "truncated";
pub const ENVELOPE_KEYS: [&str; 9] = [
    "version",
    "kind",
    "engine",
    "requires",
    "evaluatedAt",
    "timezone",
    "seed",
    "context",
    "truncated",
];

#[derive(Default, Clone, Copy)]
pub struct Marks {
    pub redacted: bool,
    pub omitted: bool,
}

pub fn check_doc_tags(doc: &Val) -> R<Marks> {
    let mut marks = Marks::default();
    match doc {
        Val::Obj(o) => {
            for (k, x) in o.iter() {
                if &**k == "context" {
                    continue;
                }
                if k.starts_with('$') && !TAGS.contains(&&**k) {
                    return Err(bad(format!("zen-trace v1: unknown tag {k}")));
                }
                marks_in(x, KEYED.contains(&&**k), &mut marks)?;
            }
            Ok(marks)
        }
        other => marks_in(other, false, &mut marks).map(|_| marks),
    }
}

fn marks_in(v: &Val, keyed: bool, marks: &mut Marks) -> R<()> {
    match v {
        Val::Arr(a) => {
            for x in a.iter() {
                marks_in(x, keyed, marks)?;
            }
            Ok(())
        }
        Val::Obj(o) => {
            if is_literal(v) {
                return Ok(());
            }
            for (k, x) in o.iter() {
                if !keyed && k.starts_with('$') {
                    match &**k {
                        REDACTED_TAG => marks.redacted = true,
                        OMITTED_TAG => marks.omitted = true,
                        t if !TAGS.contains(&t) => {
                            return Err(bad(format!("zen-trace v1: unknown tag {k}")))
                        }
                        _ => {}
                    }
                }
                marks_in(x, !keyed && KEYED.contains(&&**k), marks)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

thread_local! {
    static LENIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub const REDACTED_TEXT: &str = "[redacted]";
pub const OMITTED_TEXT: &str = "[omitted]";

pub struct Lenient(bool);

impl Lenient {
    pub fn enter(on: bool) -> Self {
        Lenient(LENIENT.with(|l| l.replace(on)))
    }
}

impl Drop for Lenient {
    fn drop(&mut self) {
        LENIENT.with(|l| l.set(self.0));
    }
}

pub fn lenient() -> bool {
    LENIENT.with(|l| l.get())
}

pub fn is_placeholder(v: &Val) -> bool {
    matches!(v, Val::Str(s) if &**s == REDACTED_TEXT || &**s == OMITTED_TEXT)
}

pub fn placeholder_on(root: &Val, path: &[Seg]) -> Option<Val> {
    if !lenient() {
        return None;
    }
    let mut cur = root;
    for s in path {
        if is_placeholder(cur) {
            return Some(cur.clone());
        }
        cur = child(cur, s)?;
    }
    None
}
const KEYED: [&str; 7] = [
    "results",
    "keys",
    "columns",
    "perItem",
    "failedCells",
    "references",
    "cells",
];

pub fn envelope(kind: &str, engine: Option<&Val>, body: Obj, requires: &[&str]) -> Obj {
    let mut doc = Obj::with_capacity(body.len() + 4);
    doc.insert(Rc::from("version"), Val::int(1));
    doc.insert(Rc::from("kind"), Val::str(kind));
    if let Some(e) = engine {
        doc.insert(Rc::from("engine"), e.clone());
    }
    if !requires.is_empty() {
        doc.insert(
            Rc::from("requires"),
            Val::arr(requires.iter().map(|r| Val::str(r)).collect()),
        );
    }
    for (k, v) in body.iter() {
        doc.insert(k.clone(), v.clone());
    }
    doc
}

pub struct Envelope {
    pub kind: String,
    pub engine: Option<Val>,
    pub requires: Vec<String>,
    pub body: Obj,
    pub evaluated_at: Option<Val>,
    pub timezone: Option<Val>,
    pub seed: Option<Val>,
    pub context: Option<Val>,
    pub truncated: bool,
}

impl Envelope {
    pub fn has(&self, feature: &str) -> bool {
        self.requires.iter().any(|f| f == feature)
    }
}

const FEATURES: [&str; 8] = [
    "steps",
    "ref-nodes",
    "derived",
    "documents",
    super::policy::ABSENT_DEFAULT,
    RESULT_ROWS,
    REDACTED,
    TRUNCATED,
];

pub fn open_envelope(doc: &Val) -> R<Envelope> {
    let o = match doc {
        Val::Obj(o) if matches!(o.get("version"), Some(Val::Num(n)) if *n == Decimal::ONE) => o,
        _ => return Err(bad("not a zen-trace v1 document")),
    };
    if let Some(v) = o.get("view") {
        return Err(bad(format!(
            "zen-trace v1: a {} view is not a stored document",
            v.to_json()
        )));
    }
    let kind = match o.get("kind") {
        Some(Val::Str(k)) if &**k == "graph" || &**k == "policy" => k.to_string(),
        other => {
            return Err(bad(format!(
                "zen-trace v1: unknown kind {}",
                other.map(|x| x.to_json()).unwrap_or_default()
            )))
        }
    };
    let mut requires = Vec::new();
    match o.get("requires") {
        None => {}
        Some(Val::Arr(a)) => {
            for f in a.iter() {
                let f = f.as_str().unwrap_or("");
                if !FEATURES.contains(&f) {
                    return Err(bad(format!("zen-trace v1: unsupported feature {f}")));
                }
                requires.push(f.to_string());
            }
        }
        Some(_) => return Err(bad("zen-trace v1: requires must be a list")),
    }
    let truncated = match o.get("truncated") {
        None | Some(Val::Bool(false)) => false,
        Some(Val::Bool(true)) => true,
        Some(_) => return Err(bad("zen-trace v1: truncated must be true or false")),
    };
    if truncated && !requires.iter().any(|f| f == TRUNCATED) {
        return Err(bad(
            "zen-trace v1: a truncated document must require truncated",
        ));
    }
    if let Some(c) = o.get("context") {
        if !c.is_obj() {
            return Err(bad("zen-trace v1: context must be an object"));
        }
    }
    let mut body = Obj::with_capacity(o.len());
    for (k, v) in o.iter() {
        if !ENVELOPE_KEYS.contains(&&**k) {
            body.insert(k.clone(), v.clone());
        }
    }
    Ok(Envelope {
        kind,
        engine: o.get("engine").cloned(),
        requires,
        body,
        evaluated_at: o.get("evaluatedAt").cloned(),
        timezone: o.get("timezone").cloned(),
        seed: o.get("seed").cloned(),
        context: o.get("context").cloned(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_round_like_debug_formatting() {
        use super::{duration_perf, perf_ns};
        let mut n: u64 = 0;
        while n < 50_000_000_000_000 {
            let d = std::time::Duration::from_nanos(n);
            let text = format!("{d:.1?}");
            match duration_perf(d) {
                Ok(v) => assert_eq!(perf_ns(&text), Some(v), "{n} {text}"),
                Err(t) => {
                    assert_eq!(t, text);
                    assert_eq!(perf_ns(&text), None, "{n} {text}");
                }
            }
            n = if n < 2_000_000 {
                n + 1
            } else {
                n * 21 / 20 + 13
            };
        }
        for n in [
            999_950u64,
            999_949,
            999_999_950,
            1_000_000_000,
            59_999_999_999,
            1_050,
            1_150,
            1_250,
        ] {
            let d = std::time::Duration::from_nanos(n);
            let text = format!("{d:.1?}");
            match duration_perf(d) {
                Ok(v) => assert_eq!(perf_ns(&text), Some(v), "{n} {text}"),
                Err(t) => assert_eq!(t, text),
            }
        }
    }

    #[test]
    fn timings_parse_without_allocating_like_fmt_ns() {
        use super::{fmt_ns, ns_text, perf_ns, perf_out};
        use crate::compiled::trace::value::Val;
        let mut n: u64 = 0;
        while n < 5_000_000_000_000 {
            let text = fmt_ns(n);
            let slow = perf_out(&Val::str(&text)).as_usize().map(|x| x as u64);
            assert_eq!(perf_ns(&text), slow, "{text}");
            let mut buf = [0u8; 40];
            let len = ns_text(n, &mut buf);
            assert_eq!(std::str::from_utf8(&buf[..len]).unwrap(), text, "{n}");
            n = n * 3 / 2 + 7;
        }
    }

    #[test]
    fn timings_print_like_rust_duration() {
        let mut n: u64 = 0;
        while n < 5_000_000_000_000 {
            assert_eq!(
                super::fmt_ns(n),
                format!("{:.1?}", std::time::Duration::from_nanos(n)),
                "{n}"
            );
            n = n * 3 / 2 + 7 + (n % 13);
        }
        for n in [
            1050,
            1150,
            1250,
            1450,
            999_950,
            999_999_999,
            1_000_000_000,
            1_250_000_000,
        ] {
            assert_eq!(
                super::fmt_ns(n),
                format!("{:.1?}", std::time::Duration::from_nanos(n)),
                "{n}"
            );
        }
    }
}

#[cfg(test)]
mod packing_tests {
    use super::*;

    fn num(d: &Decimal) -> String {
        let mut out = String::new();
        crate::compiled::trace::value::write_num(d, &mut out);
        out
    }

    fn text(p: Option<PackedNums>) -> String {
        match p {
            None => "none".to_string(),
            Some(Packing::Runs(runs)) => runs
                .iter()
                .map(|(v, n, d)| {
                    format!("{}x{n}+{}", num(v), d.as_ref().map(num).unwrap_or_default())
                })
                .collect::<Vec<_>>()
                .join(","),
            Some(Packing::Pick(values, at)) => {
                format!("{:?}@{at:?}", values.iter().map(num).collect::<Vec<_>>())
            }
        }
    }

    #[test]
    fn scaled_decimals_pack_like_the_general_path() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for case in 0..400 {
            let scale = (case % 4) as u32;
            let mixed = case % 3 == 0;
            let len = 4 + next(60) as usize;
            let style = next(4);
            let base = next(2000) as i64 - 1000;
            let step = next(7) as i64 - 3;
            let values: Vec<Decimal> = (0..len)
                .map(|i| {
                    let m = match style {
                        0 => base + step * i as i64,
                        1 => base + next(5) as i64,
                        2 => [base, base * 3, -base][next(3) as usize],
                        _ => base + step * (i as i64 % 7),
                    };
                    match mixed && m % 10 == 0 && i % 2 == 0 {
                        true => Decimal::new(m / 10, scale.saturating_sub(1)),
                        false => Decimal::new(m, scale),
                    }
                })
                .collect();
            let fast = pack_decimals(len, |i| values[i]);
            let general = pack_decimals_general(len, |i| values[i]);
            assert_eq!(text(fast), text(general), "{values:?}");
        }
    }
}

#[cfg(test)]
mod int_packing_tests {
    use super::*;

    fn text(p: Option<PackedInts>) -> String {
        match p {
            None => "none".to_string(),
            Some(Packing::Runs(runs)) => format!("runs {runs:?}"),
            Some(Packing::Pick(values, at)) => format!("pick {values:?} {at:?}"),
        }
    }

    #[test]
    fn int_packing_matches_the_general_path() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for case in 0..2000 {
            let len = next(80) as usize;
            let base = next(4000) as i64 - 2000;
            let step = next(9) as i64 - 4;
            let spread = [3, 20, 1000, 1 << 41][case % 4];
            let ints: Vec<i64> = (0..len)
                .map(|i| match next(5) {
                    0 => base,
                    1 => base + step * i as i64,
                    2 => base + next(spread) as i64,
                    3 => base + step * (i as i64 % 5),
                    _ => (next(spread) as i64) * if next(2) == 0 { -1 } else { 1 },
                })
                .collect();
            assert_eq!(
                text(pack_ints(&ints)),
                text(pack_ints_general(&ints)),
                "{ints:?}"
            );
        }
    }
}

pub fn replace_at(root: &Val, path: &[Seg], v: Val) -> R<Val> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(v);
    };
    match root {
        Val::Arr(a) => {
            let i = match first {
                Seg::I(i) => *i,
                Seg::K(k) => js_number_index(k).ok_or_else(|| bad("bad index"))?,
            };
            let mut c = (**a).clone();
            let slot = c.get_mut(i).ok_or_else(|| bad("bad index"))?;
            *slot = replace_at(slot, rest, v)?;
            Ok(Val::arr(c))
        }
        Val::Obj(o) => {
            let k = first.text();
            let cur = o.get(&k).ok_or_else(|| bad("bad key"))?;
            let mut c = (**o).clone();
            c.set(&k, replace_at(cur, rest, v)?);
            Ok(Val::obj(c))
        }
        _ => Err(bad("bad path")),
    }
}

pub fn unpack_doc(v: &Val) -> R<Val> {
    let Val::Obj(o) = v else { return unpack(v) };
    if !o.has("context") || is_literal(v) {
        return unpack(v);
    }
    let mut out = Obj::with_capacity(o.len());
    for (k, x) in o.iter() {
        let x = match &**k {
            "context" => x.clone(),
            _ => unpack(x)?,
        };
        out.insert(k.clone(), x);
    }
    Ok(Val::obj(out))
}
