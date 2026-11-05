use ahash::HashMap;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use std::cell::OnceCell;
use std::fmt::Write as _;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Val {
    Null,
    Bool(bool),
    Num(Decimal),
    Str(Rc<str>),
    Arr(Rc<Vec<Val>>),
    Obj(Rc<Obj>),
}

pub type Key = Rc<str>;

pub(crate) type FastMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<ahash::AHasher>>;
pub(crate) type FastSet<K> =
    std::collections::HashSet<K, std::hash::BuildHasherDefault<ahash::AHasher>>;

const INDEX_AT: usize = 32;

#[derive(Default)]
pub struct Obj {
    entries: Vec<(Key, Val)>,
    ints: usize,
    index: OnceCell<HashMap<Key, usize>>,
}

impl Clone for Obj {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            ints: self.ints,
            index: OnceCell::new(),
        }
    }
}

impl std::fmt::Debug for Obj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|(k, v)| (k, v)))
            .finish()
    }
}

pub fn array_index(k: &str) -> Option<u32> {
    let b = k.as_bytes();
    if b.is_empty() || b.len() > 10 {
        return None;
    }
    if b[0] == b'0' {
        return if b.len() == 1 { Some(0) } else { None };
    }
    let mut n: u64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (c - b'0') as u64;
    }
    if n < 4294967295 {
        Some(n as u32)
    } else {
        None
    }
}

impl Obj {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            entries: Vec::with_capacity(n),
            ints: 0,
            index: OnceCell::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn find(&self, k: &str) -> Option<usize> {
        if self.entries.len() <= INDEX_AT {
            return self.entries.iter().position(|(x, _)| &**x == k);
        }
        let idx = self.index.get_or_init(|| {
            let mut m = HashMap::default();
            for (i, (x, _)) in self.entries.iter().enumerate() {
                m.insert(x.clone(), i);
            }
            m
        });
        idx.get(k).copied()
    }

    pub fn get(&self, k: &str) -> Option<&Val> {
        self.find(k).map(|i| &self.entries[i].1)
    }

    pub fn get_hinted(&self, k: &Key, i: usize) -> Option<&Val> {
        if let Some((x, v)) = self.entries.get(i) {
            if Rc::ptr_eq(x, k) || **x == **k {
                return Some(v);
            }
        }
        self.get(k)
    }

    pub fn has(&self, k: &str) -> bool {
        self.find(k).is_some()
    }

    pub fn get_mut_val(&mut self, k: &str) -> Option<&mut Val> {
        let i = self.find(k)?;
        Some(&mut self.entries[i].1)
    }

    pub fn insert(&mut self, k: Key, v: Val) {
        if let Some(i) = self.find(&k) {
            self.entries[i].1 = v;
            return;
        }
        match if k.as_bytes().first().is_some_and(|c| c.is_ascii_digit()) {
            array_index(&k)
        } else {
            None
        } {
            Some(n) => {
                let mut at = self.ints;
                while at > 0 && array_index(&self.entries[at - 1].0).unwrap_or(0) > n {
                    at -= 1;
                }
                self.entries.insert(at, (k, v));
                self.ints += 1;
                self.index = OnceCell::new();
            }
            None => {
                if let Some(m) = self.index.get_mut() {
                    m.insert(k.clone(), self.entries.len());
                }
                self.entries.push((k, v));
            }
        }
    }

    pub fn push_unique(&mut self, k: Key, v: Val) {
        if k.as_bytes().first().is_some_and(|c| c.is_ascii_digit()) && array_index(&k).is_some() {
            if self.ints != self.entries.len() {
                self.insert(k, v);
                return;
            }
            self.ints += 1;
        }
        if let Some(m) = self.index.get_mut() {
            m.insert(k.clone(), self.entries.len());
        }
        self.entries.push((k, v));
    }

    pub fn set(&mut self, k: &str, v: Val) {
        if let Some(i) = self.find(k) {
            self.entries[i].1 = v;
            return;
        }
        self.insert(Rc::from(k), v);
    }

    pub fn remove(&mut self, k: &str) -> Option<Val> {
        let i = self.find(k)?;
        let (key, v) = self.entries.remove(i);
        if array_index(&key).is_some() {
            self.ints -= 1;
        }
        self.index = OnceCell::new();
        Some(v)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Key, &Val)> + ExactSizeIterator {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.entries.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl Iterator<Item = &Val> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn first_key(&self) -> Option<&Key> {
        self.entries.first().map(|(k, _)| k)
    }
}

impl Val {
    pub fn str(s: &str) -> Val {
        Val::Str(Rc::from(s))
    }

    pub fn obj(o: Obj) -> Val {
        Val::Obj(Rc::new(o))
    }

    pub fn arr(v: Vec<Val>) -> Val {
        Val::Arr(Rc::new(v))
    }

    pub fn empty_obj() -> Val {
        Val::obj(Obj::new())
    }

    pub fn int(n: i64) -> Val {
        Val::Num(Decimal::from(n))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Val::Null)
    }

    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Val::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&Vec<Val>> {
        match self {
            Val::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_num(&self) -> Option<Decimal> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn is_obj(&self) -> bool {
        matches!(self, Val::Obj(_))
    }

    pub fn is_arr(&self) -> bool {
        matches!(self, Val::Arr(_))
    }

    pub fn is_container(&self) -> bool {
        matches!(self, Val::Obj(_) | Val::Arr(_))
    }

    pub fn get(&self, k: &str) -> Option<&Val> {
        match self {
            Val::Obj(o) => o.get(k),
            _ => None,
        }
    }

    pub fn has(&self, k: &str) -> bool {
        match self {
            Val::Obj(o) => o.has(k),
            _ => false,
        }
    }

    pub fn ptr(&self) -> usize {
        match self {
            Val::Arr(a) => Rc::as_ptr(a) as *const u8 as usize,
            Val::Obj(o) => Rc::as_ptr(o) as *const u8 as usize,
            _ => 0,
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        match self {
            Val::Num(n) if n.is_integer() && !n.is_sign_negative() => n.to_usize(),
            Val::Num(n) if n.is_zero() => Some(0),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Num(n) if n.is_integer() => n.to_i64(),
            _ => None,
        }
    }

    pub fn to_json(&self) -> String {
        let mut s = String::new();
        write_json(self, &mut s);
        s
    }
}

pub fn eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Null, Val::Null) => true,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Num(x), Val::Num(y)) => num_eq(x, y),
        (Val::Str(x), Val::Str(y)) => Rc::ptr_eq(x, y) || x == y,
        (Val::Arr(x), Val::Arr(y)) => {
            Rc::ptr_eq(x, y)
                || (x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| eq(p, q)))
        }
        (Val::Obj(x), Val::Obj(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            if x.len() != y.len() {
                return false;
            }
            x.iter()
                .enumerate()
                .all(|(i, (k, v))| match y.get_hinted(k, i) {
                    Some(w) => eq(v, w),
                    None => false,
                })
        }
        _ => false,
    }
}

pub fn utf16_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

pub fn num_eq(a: &Decimal, b: &Decimal) -> bool {
    a.serialize() == b.serialize() || a == b
}

fn small_parts(d: &Decimal) -> Option<(bool, u64, u32)> {
    let m = d.mantissa();
    let abs = m.unsigned_abs();
    if abs >= 1_000_000_000_000_000 {
        return None;
    }
    let mut m64 = abs as u64;
    let mut scale = d.scale();
    while scale > 0 && m64.is_multiple_of(10) {
        m64 /= 10;
        scale -= 1;
    }
    let digits = m64.checked_ilog10().unwrap_or(0) + 1;
    let lead = digits as i64 - 1 - scale as i64;
    (-6..21).contains(&lead).then_some((m < 0, m64, scale))
}

fn plain_parts(d: &Decimal) -> Option<(bool, u128, u32)> {
    if d.mantissa().unsigned_abs() < 1_000_000_000_000_000 {
        return small_parts(d).map(|(neg, m, scale)| (neg, m as u128, scale));
    }
    let n = d.normalize();
    let m = n.mantissa();
    let neg = m < 0;
    let m = m.unsigned_abs();
    let scale = n.scale();
    if m >= 1_000_000_000_000_000 {
        return None;
    }
    let digits = if m == 0 { 1 } else { m.ilog10() + 1 };
    let lead = digits as i64 - 1 - scale as i64;
    if !(-6..21).contains(&lead) {
        return None;
    }
    Some((neg, m, scale))
}

fn plain_text(neg: bool, m: u128, scale: u32, out: &mut String) {
    let mut buf = itoa::Buffer::new();
    let t = buf.format(m as u64);
    if neg {
        out.push('-');
    }
    let scale = scale as usize;
    if scale == 0 {
        out.push_str(t);
    } else if t.len() > scale {
        out.push_str(&t[..t.len() - scale]);
        out.push('.');
        out.push_str(&t[t.len() - scale..]);
    } else {
        out.push_str("0.");
        for _ in 0..scale - t.len() {
            out.push('0');
        }
        out.push_str(t);
    }
}

pub fn js_num(d: &Decimal) -> String {
    if d.is_zero() {
        return "0".to_string();
    }
    if let Some((neg, m, scale)) = plain_parts(d) {
        let mut s = String::with_capacity(24);
        plain_text(neg, m, scale, &mut s);
        return s;
    }
    let f: f64 = d.normalize().to_string().parse().unwrap_or(0.0);
    js_f64(f)
}

pub fn js_num_into(d: &Decimal, out: &mut String) {
    out.clear();
    if d.is_zero() {
        out.push('0');
        return;
    }
    match plain_parts(d) {
        Some((neg, m, scale)) => plain_text(neg, m, scale, out),
        None => out.push_str(&js_num(d)),
    }
}

pub fn js_f64(f: f64) -> String {
    if f == 0.0 {
        return "0".to_string();
    }
    if !f.is_finite() {
        return "null".to_string();
    }
    let e = format!("{:e}", f);
    let (neg, e) = match e.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, e.as_str()),
    };
    let (mant, exp) = e.split_once('e').unwrap_or((e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let x = n - 1;
        if x >= 0 {
            out.push('+');
        }
        let _ = write!(out, "{}", x);
    }
    out
}

pub fn js_num_len(d: &Decimal) -> usize {
    if d.is_zero() {
        return 1;
    }
    if d.scale() == 0 {
        let m = d.mantissa();
        let a = m.unsigned_abs();
        if a < 1_000_000_000_000_000_000_000 {
            return (a.ilog10() + 1) as usize + (m < 0) as usize;
        }
    }
    if let Some((neg, m, scale)) = plain_parts(d) {
        let digits = (m.ilog10() + 1) as usize;
        let scale = scale as usize;
        let body = if scale == 0 {
            digits
        } else if digits > scale {
            digits + 1
        } else {
            scale + 2
        };
        return body + neg as usize;
    }
    js_num(d).len()
}

pub fn js_str_quoted_len(s: &str) -> usize {
    let mut n = 2;
    for c in s.chars() {
        n += match c {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf16(),
        };
    }
    n
}

pub fn json_len(v: &Val) -> usize {
    match v {
        Val::Null => 4,
        Val::Bool(true) => 4,
        Val::Bool(false) => 5,
        Val::Num(d) => js_num_len(d),
        Val::Str(s) => js_str_quoted_len(s),
        Val::Arr(a) => {
            let mut n = 2 + a.len().saturating_sub(1);
            for x in a.iter() {
                n += json_len(x);
            }
            n
        }
        Val::Obj(o) => {
            let mut n = 2 + o.len().saturating_sub(1);
            for (k, x) in o.iter() {
                n += js_str_quoted_len(k) + 1 + json_len(x);
            }
            n
        }
    }
}

fn plain_ascii(bytes: &[u8]) -> bool {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        let w = u64::from_le_bytes(*c);
        let low = w.wrapping_sub(LO * 0x20) & !w;
        let q = w ^ (LO * b'"' as u64);
        let b = w ^ (LO * b'\\' as u64);
        let hit = (low | (q.wrapping_sub(LO) & !q) | (b.wrapping_sub(LO) & !b)) & HI;
        if hit != 0 {
            return false;
        }
    }
    !rest.iter().any(|&b| b < 0x20 || b == b'"' || b == b'\\')
}

pub fn write_str(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    if plain_ascii(bytes) || !bytes.iter().any(|&b| b < 0x20 || b == b'"' || b == b'\\') {
        out.reserve(bytes.len() + 2);
        out.push('"');
        out.push_str(s);
        out.push('"');
        return;
    }
    out.push('"');
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let esc: Option<&str> = match b {
            b'"' => Some("\\\""),
            b'\\' => Some("\\\\"),
            b'\n' => Some("\\n"),
            b'\r' => Some("\\r"),
            b'\t' => Some("\\t"),
            0x08 => Some("\\b"),
            0x0c => Some("\\f"),
            b if b < 0x20 => None,
            _ => continue,
        };
        out.push_str(&s[start..i]);
        match esc {
            Some(e) => out.push_str(e),
            None => {
                let _ = write!(out, "\\u{:04x}", b);
            }
        }
        start = i + 1;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

pub fn write_num(d: &Decimal, out: &mut String) {
    if d.is_zero() {
        out.push('0');
        return;
    }
    if let Some((neg, m, scale)) = plain_parts(d) {
        plain_text(neg, m, scale, out);
        return;
    }
    let n = d.normalize();
    let abs = n.abs();
    if abs >= Decimal::from_i128_with_scale(10i128.pow(21), 0) || abs < Decimal::new(1, 6) {
        let m = n.mantissa().unsigned_abs();
        let mut digits = m.to_string();
        let mut exp = digits.len() as i32 - 1 - n.scale() as i32;
        while digits.len() > 1 && digits.ends_with('0') {
            digits.pop();
        }
        if n.is_sign_negative() {
            out.push('-');
        }
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        if exp >= 0 {
            out.push('+');
        } else {
            out.push('-');
            exp = -exp;
        }
        let _ = write!(out, "{}", exp);
        return;
    }
    let _ = write!(out, "{}", n);
}

pub fn write_json(v: &Val, out: &mut String) {
    match v {
        Val::Null => out.push_str("null"),
        Val::Bool(true) => out.push_str("true"),
        Val::Bool(false) => out.push_str("false"),
        Val::Num(d) => write_num(d, out),
        Val::Str(s) => write_str(s, out),
        Val::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(x, out);
            }
            out.push(']');
        }
        Val::Obj(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write_json(x, out);
            }
            out.push('}');
        }
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub at: usize,
    pub message: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid JSON at byte {}: {}", self.at, self.message)
    }
}

pub(crate) struct Parser<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
    keys: HashMap<Box<str>, Key>,
}

pub fn parse(s: &str) -> Result<Val, ParseError> {
    let mut p = Parser {
        b: s.as_bytes(),
        s,
        i: 0,
        keys: HashMap::default(),
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

pub fn parse_number(t: &str) -> Option<Decimal> {
    Decimal::from_str_exact(t)
        .or_else(|_| Decimal::from_scientific(t))
        .ok()
        .or_else(|| t.parse::<f64>().ok().and_then(Decimal::from_f64))
}

impl<'a> Parser<'a> {
    pub(crate) fn new(s: &'a str) -> Self {
        Parser {
            b: s.as_bytes(),
            s,
            i: 0,
            keys: HashMap::default(),
        }
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    pub(crate) fn bump(&mut self) {
        self.i += 1;
    }

    pub(crate) fn done(&self) -> bool {
        self.i == self.b.len()
    }

    pub(crate) fn word(&mut self, w: &str) -> Result<(), ParseError> {
        self.lit(w, Val::Null).map(|_| ())
    }

    pub(crate) fn number_text(&mut self) -> Result<&'a str, ParseError> {
        let st = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
            )
        {
            self.i += 1;
        }
        let t = &self.s[st..self.i];
        if t.is_empty() || t == "-" {
            return Err(self.err("invalid value"));
        }
        Ok(t)
    }

    pub(crate) fn err(&self, message: &'static str) -> ParseError {
        ParseError {
            at: self.i,
            message,
        }
    }

    pub(crate) fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Val, ParseError> {
        if depth > 4096 {
            return Err(self.err("nesting too deep"));
        }
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'n') => self.lit("null", Val::Null),
            Some(b't') => self.lit("true", Val::Bool(true)),
            Some(b'f') => self.lit("false", Val::Bool(false)),
            Some(b'"') => Ok(Val::Str(Rc::from(self.string()?.as_str()))),
            Some(b'[') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Val::arr(out));
                }
                loop {
                    self.ws();
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Val::arr(out));
                        }
                        _ => return Err(self.err("expected , or ]")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut o = Obj::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Val::obj(o));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected key"));
                    }
                    let k = self.string()?;
                    let key = match self.keys.get(k.as_str()) {
                        Some(x) => x.clone(),
                        None => {
                            let x: Key = Rc::from(k.as_str());
                            if self.keys.len() < 65536 {
                                self.keys.insert(k.into_boxed_str(), x.clone());
                            }
                            x
                        }
                    };
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(self.err("expected :"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    o.insert(key, v);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Val::obj(o));
                        }
                        _ => return Err(self.err("expected , or }")),
                    }
                }
            }
            Some(_) => self.number(),
        }
    }

    fn lit(&mut self, w: &str, v: Val) -> Result<Val, ParseError> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn number(&mut self) -> Result<Val, ParseError> {
        let t = self.number_text()?;
        parse_number(t)
            .map(Val::Num)
            .ok_or_else(|| self.err("number out of range"))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        if self.i + 4 > self.b.len() {
            return Err(self.err("bad escape"));
        }
        let h =
            std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|_| self.err("bad escape"))?;
        let n = u32::from_str_radix(h, 16).map_err(|_| self.err("bad escape"))?;
        self.i += 4;
        Ok(n)
    }

    pub(crate) fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1;
        let mut out = String::new();
        let mut st = self.i;
        loop {
            match self.b.get(self.i) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    out.push_str(&self.s[st..self.i]);
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    out.push_str(&self.s[st..self.i]);
                    self.i += 1;
                    let c = *self.b.get(self.i).ok_or_else(|| self.err("bad escape"))?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let a = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&a) {
                                if self.b.get(self.i) == Some(&b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let b = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&b) {
                                        char::from_u32(
                                            0x10000 + ((a - 0xD800) << 10) + (b - 0xDC00),
                                        )
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                char::from_u32(a)
                            };
                            out.push(ch.unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                    st = self.i;
                }
                Some(_) => self.i += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_escape_wherever_the_special_byte_falls() {
        for special in ['"', '\\', '\n', '\u{1}', '\u{1f}'] {
            for len in 0..24 {
                for at in 0..=len {
                    let mut t: String = "abcdefghijklmnopqrstuvwxyz".chars().take(len).collect();
                    t.insert(at.min(t.len()), special);
                    let mut out = String::new();
                    write_str(&t, &mut out);
                    assert_eq!(out, serde_json::to_string(&t).unwrap());
                }
            }
        }
        let mut out = String::new();
        write_str("caf\u{e9} \u{7f} \u{80}", &mut out);
        assert_eq!(
            out,
            serde_json::to_string("caf\u{e9} \u{7f} \u{80}").unwrap()
        );
    }

    #[test]
    fn numbers_print_like_javascript_and_read_back_exactly() {
        for t in [
            "-5e+21",
            "1.2345678901234567890123e+22",
            "1e-7",
            "-2.5e-10",
            "0.000001",
            "123.45",
            "100000000000000000000",
            "7.9e+28",
        ] {
            let v = parse(t).unwrap();
            let mut s = String::new();
            write_json(&v, &mut s);
            assert_eq!(s, t);
            assert!(eq(&parse(&s).unwrap(), &v));
        }
    }
}
