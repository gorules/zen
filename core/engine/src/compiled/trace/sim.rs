use super::value::{eq, Obj, Val};
use ahash::{HashMap, HashSet};
use regex::Regex;
use std::rc::Rc;
use std::sync::OnceLock;

pub fn split(p: &str) -> Vec<String> {
    if p.is_empty() {
        Vec::new()
    } else {
        p.split('.').map(|s| s.to_string()).collect()
    }
}

fn js_string(v: &Val) -> String {
    match v {
        Val::Str(s) => s.to_string(),
        Val::Num(n) => super::value::js_num(n),
        Val::Null => "null".to_string(),
        Val::Bool(b) => b.to_string(),
        Val::Arr(_) | Val::Obj(_) => v.to_json(),
    }
}

fn digits_only(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

fn canonical_index(k: &str) -> Option<usize> {
    let b = k.as_bytes();
    if b.is_empty() || !b.iter().all(|c| c.is_ascii_digit()) || (b[0] == b'0' && b.len() > 1) {
        return None;
    }
    k.parse().ok()
}

fn own(o: &Val, s: &str) -> Option<Val> {
    match o {
        Val::Obj(m) => m.get(s).cloned(),
        Val::Arr(a) => {
            if s == "length" {
                return Some(Val::int(a.len() as i64));
            }
            canonical_index(s).and_then(|i| a.get(i).cloned())
        }
        _ => None,
    }
}

fn pool(root: &Val, id: &Val) -> Option<Val> {
    let Val::Obj(o) = root else { return None };
    for v in o.values() {
        if let Val::Arr(a) = v {
            for x in a.iter() {
                if let Val::Obj(xo) = x {
                    match (xo.get("id"), id) {
                        (Some(Val::Str(p)), Val::Str(q)) if p == q => return Some(x.clone()),
                        (Some(Val::Num(p)), Val::Num(q)) if p == q => return Some(x.clone()),
                        _ => {}
                    }
                }
            }
        }
    }
    None
}

pub fn get<S: AsRef<str>>(root: &Val, segs: &[S]) -> Option<Val> {
    let mut o = root.clone();
    for s in segs {
        if matches!(o, Val::Str(_) | Val::Num(_)) {
            if let Some(hit) = pool(root, &o) {
                o = hit;
            }
        }
        o = own(&o, s.as_ref())?;
    }
    Some(o)
}

pub fn set_in(o: Option<&Val>, segs: &[String], v: &Val, i: usize) -> Val {
    if i == segs.len() {
        return v.clone();
    }
    let s = &segs[i];
    match o {
        Some(Val::Arr(a)) => {
            let mut c = (**a).clone();
            let cur = if s == "length" {
                Some(Val::int(c.len() as i64))
            } else {
                canonical_index(s).and_then(|j| c.get(j).cloned())
            };
            let nx = set_in(cur.as_ref(), segs, v, i + 1);
            if digits_only(s) {
                if let Ok(j) = s.parse::<usize>() {
                    if j < 10_000_000 {
                        while c.len() <= j {
                            c.push(Val::Null);
                        }
                        c[j] = nx;
                    }
                }
            }
            Val::arr(c)
        }
        Some(Val::Obj(m)) => {
            let mut c = (**m).clone();
            let cur = c.get(s).cloned();
            let nx = set_in(cur.as_ref(), segs, v, i + 1);
            c.set(s, nx);
            Val::obj(c)
        }
        Some(Val::Null) | None => {
            let mut c = Obj::new();
            let nx = set_in(None, segs, v, i + 1);
            c.set(s, nx);
            Val::obj(c)
        }
        Some(p) => p.clone(),
    }
}

pub fn abs_path(state: &Val, path: &str, inst: Option<&str>) -> Vec<String> {
    let s = split(path);
    let Some(inst) = inst.filter(|i| !i.is_empty()) else {
        return s;
    };
    if s.len() < 2 {
        return s;
    }
    let head = state.as_obj().and_then(|o| o.get(&s[0]));
    let rebase = match head {
        None => true,
        Some(Val::Arr(_)) => inst.starts_with(&format!("{}.", s[0])),
        _ => false,
    };
    if rebase {
        let mut out = split(inst);
        out.extend(s[1..].iter().cloned());
        return out;
    }
    s
}

fn id0(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn idc(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

const KW: [&str; 11] = [
    "and", "or", "not", "in", "true", "false", "null", "if", "then", "else", "matches",
];

pub fn idents(src: &str, out: &mut Vec<String>) {
    let s: Vec<char> = src.chars().collect();
    idents_chars(&s, out);
}

fn at(s: &[char], i: isize) -> Option<char> {
    if i < 0 {
        None
    } else {
        s.get(i as usize).copied()
    }
}

fn idents_chars(src: &[char], out: &mut Vec<String>) {
    let n = src.len();
    let mut stack: Vec<char> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let c = src[i];
        if c == '\'' || c == '"' {
            i += 1;
            while i < n && src[i] != c {
                if src[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '`' {
            i += 1;
            while i < n && src[i] != '`' {
                if src[i] == '\\' {
                    i += 2;
                    continue;
                }
                if src[i] == '$' && at(src, i as isize + 1) == Some('{') {
                    let mut d = 1;
                    let mut j = i + 2;
                    while j < n && d > 0 {
                        if src[j] == '{' {
                            d += 1;
                        } else if src[j] == '}' {
                            d -= 1;
                        }
                        j += 1;
                    }
                    let end = j.saturating_sub(1);
                    if i + 2 <= end {
                        idents_chars(&src[i + 2..end], out);
                    }
                    i = j;
                    continue;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '{' || c == '[' || c == '(' {
            stack.push(c);
            i += 1;
            continue;
        }
        if c == '}' || c == ']' || c == ')' {
            stack.pop();
            i += 1;
            continue;
        }
        if id0(c) {
            let mut segs: Vec<String> = Vec::new();
            let mut j = i;
            let word = |j: &mut usize, segs: &mut Vec<String>| {
                let st = *j;
                while *j < n && idc(src[*j]) {
                    *j += 1;
                }
                segs.push(src[st..*j].iter().collect());
            };
            word(&mut j, &mut segs);
            while at(src, j as isize) == Some('.') && at(src, j as isize + 1).is_some_and(id0) {
                j += 1;
                word(&mut j, &mut segs);
            }
            let prev = at(src, i as isize - 1);
            let mut k = j;
            while at(src, k as isize) == Some(' ') {
                k += 1;
            }
            if at(src, k as isize) == Some('(') {
                segs.pop();
            }
            let is_key = at(src, k as isize) == Some(':') && stack.last() == Some(&'{');
            if !segs.is_empty()
                && (prev != Some('.') || at(src, i as isize - 2) == Some('.'))
                && prev != Some('#')
                && !is_key
                && !KW.contains(&segs[0].as_str())
                && !segs[0].starts_with('$')
            {
                out.push(segs.join("."));
            }
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            while i < n && (src[i].is_ascii_digit() || matches!(src[i], '.' | '_' | 'e' | 'E')) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
}

fn data_of(blk: &Val) -> Val {
    blk.get("props")
        .and_then(|p| p.get("data"))
        .cloned()
        .unwrap_or_else(Val::empty_obj)
}

pub fn doc_guess(rel: &[Val]) -> HashMap<String, usize> {
    let mut m: HashMap<String, usize> = HashMap::default();
    let mut next = 1;
    let mut q = std::collections::VecDeque::from([0usize]);
    while let Some(d) = q.pop_front() {
        if let Some(Val::Arr(imports)) = rel.get(d).and_then(|x| x.get("imports")) {
            for name in imports.iter() {
                let name = js_string(name);
                if let std::collections::hash_map::Entry::Vacant(v) = m.entry(name) {
                    v.insert(next);
                    q.push_back(next);
                    next += 1;
                }
            }
        }
    }
    m
}

fn empty_cell(c: Option<&Val>) -> bool {
    match c {
        None | Some(Val::Null) => true,
        Some(Val::Str(s)) => s.is_empty(),
        _ => false,
    }
}

pub struct DtField {
    pub id: Option<Val>,
    pub field: String,
    pub arr: bool,
}

pub fn dt_fields(d: &Val) -> Vec<DtField> {
    let collect = matches!(d.get("hitPolicy"), Some(Val::Str(s)) if &**s == "collect");
    let outs = match d.get("outputs") {
        Some(Val::Arr(a)) => a.to_vec(),
        _ => Vec::new(),
    };
    outs.iter()
        .map(|o| {
            let f = match o.get("field") {
                None | Some(Val::Null) => String::new(),
                Some(x) => js_string(x),
            };
            let arr = f.ends_with("[]");
            DtField {
                id: o.get("id").cloned(),
                field: if arr { f[..f.len() - 2].to_string() } else { f },
                arr: collect || arr,
            }
        })
        .collect()
}

fn rule_cell<'a>(rule: &'a Val, id: &Option<Val>) -> Option<&'a Val> {
    let id = id.as_ref()?;
    rule.get(&js_string(id))
}

pub fn dt_write_paths(d: &Val, rows: &[Val]) -> Vec<String> {
    let f = dt_fields(d);
    if matches!(d.get("hitPolicy"), Some(Val::Str(s)) if &**s == "collect") {
        return f.into_iter().map(|o| o.field).collect();
    }
    let rules = d
        .get("rules")
        .and_then(|r| r.as_arr())
        .cloned()
        .unwrap_or_default();
    let mut found: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::default();
    for r in rows {
        let Some(rule) = r.as_usize().and_then(|i| rules.get(i)) else {
            continue;
        };
        for o in &f {
            if !o.arr && !seen.contains(&o.field) && !empty_cell(rule_cell(rule, &o.id)) {
                seen.insert(o.field.clone());
                found.push(o.field.clone());
            }
        }
    }
    for o in &f {
        if !o.arr && !seen.contains(&o.field) {
            seen.insert(o.field.clone());
            found.push(o.field.clone());
        }
    }
    for o in &f {
        if o.arr && !seen.contains(&o.field) {
            seen.insert(o.field.clone());
            found.push(o.field.clone());
        }
    }
    found
}

pub fn list_delta(base: &[String], actual: &[String]) -> (Vec<String>, Vec<String>) {
    let bs: HashSet<&String> = base.iter().collect();
    let as_: HashSet<&String> = actual.iter().collect();
    let add = actual.iter().filter(|x| !bs.contains(x)).cloned().collect();
    let del = base.iter().filter(|x| !as_.contains(x)).cloned().collect();
    (add, del)
}

fn js_lt(a: &str, b: &str) -> bool {
    a.encode_utf16().lt(b.encode_utf16())
}

pub fn js_sort(v: &mut [String]) {
    v.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
}

pub fn apply_delta(base: &[String], add: &[String], del: &[String]) -> Vec<String> {
    let ds: HashSet<&String> = del.iter().collect();
    let mut out: Vec<String> = base.iter().filter(|x| !ds.contains(x)).cloned().collect();
    for a in add {
        let mut i = 0;
        while i < out.len() && js_lt(&out[i], a) {
            i += 1;
        }
        out.insert(i, a.clone());
    }
    out
}

fn prefix(p: &[String], s: &[String]) -> bool {
    p.len() <= s.len() && p.iter().zip(s).all(|(x, y)| x == y)
}

pub type Alias = (String, Vec<usize>);

pub fn find_alias(sim: &Sim, ids: &[String], value: &Val, inst: Option<&str>) -> Option<Alias> {
    let Val::Arr(value) = value else { return None };
    if value.is_empty() || !value.iter().all(|x| x.is_obj()) {
        return None;
    }
    for id in ids {
        let segs = sim.abs(id, inst);
        let Some(Val::Arr(src)) = get(&sim.state, &segs) else {
            continue;
        };
        if src.len() < value.len() {
            continue;
        }
        let mut idx = Vec::with_capacity(value.len());
        let mut at = 0;
        for el in value.iter() {
            while at < src.len() && !eq(&src[at], el) {
                at += 1;
            }
            if at == src.len() {
                break;
            }
            idx.push(at);
            at += 1;
        }
        if idx.len() == value.len() {
            return Some((segs.join("."), idx));
        }
    }
    None
}

pub struct AliasLink {
    a: Vec<String>,
    b: Vec<String>,
    ab: HashMap<usize, usize>,
    ba: HashMap<usize, usize>,
}

pub struct Info {
    pub kind: Option<String>,
    pub d: Val,
}

pub struct Sim {
    pub rel: Vec<Val>,
    pub state: Val,
    guess: HashMap<String, usize>,
    cache: HashMap<(usize, String), Option<Rc<Info>>>,
    pub aliases: Vec<Rc<AliasLink>>,
    pub models: HashMap<String, Val>,
}

impl Sim {
    pub fn new(request: &Val, rel: Vec<Val>) -> Self {
        let models = models_of(&rel);
        Self {
            guess: doc_guess(&rel),
            rel,
            state: request.clone(),
            cache: HashMap::default(),
            aliases: Vec::new(),
            models,
        }
    }

    pub fn doc_of(&self, pp: Option<&Val>) -> usize {
        match pp {
            None => 0,
            Some(v) if js_falsy_str(v) => 0,
            Some(v) => self.guess.get(&js_string(v)).copied().unwrap_or(0),
        }
    }

    pub fn info(&mut self, doc: usize, id: &Val) -> Option<Rc<Info>> {
        let key = (doc, js_string(id));
        if let Some(r) = self.cache.get(&key) {
            return r.clone();
        }
        let blk = self
            .rel
            .get(doc)
            .and_then(|d| d.get("blocks"))
            .and_then(|b| b.as_arr())
            .and_then(|bs| {
                bs.iter()
                    .find(|b| b.get("id").is_some_and(|x| same_id(x, id)))
            })
            .cloned();
        let r = blk.map(|b| {
            Rc::new(Info {
                kind: b
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string()),
                d: data_of(&b),
            })
        });
        self.cache.insert(key, r.clone());
        r
    }

    pub fn abs(&self, path: &str, inst: Option<&str>) -> Vec<String> {
        abs_path(&self.state, path, inst)
    }

    pub fn val(&self, path: &str, inst: Option<&str>) -> Option<Val> {
        get(&self.state, &self.abs(path, inst))
    }

    pub fn val_in(&self, state: &Val, path: &str, inst: Option<&str>) -> Option<Val> {
        get(state, &self.abs(path, inst))
    }

    pub fn after(
        &mut self,
        writes: &[(Val, Val)],
        inst: Option<&str>,
        from: Option<&HashMap<String, Alias>>,
    ) -> Val {
        let keep_state = self.state.clone();
        let keep_aliases = self.aliases.clone();
        self.apply(writes, inst, from);
        let st = std::mem::replace(&mut self.state, keep_state);
        self.aliases = keep_aliases;
        st
    }

    pub fn apply(
        &mut self,
        writes: &[(Val, Val)],
        inst: Option<&str>,
        from: Option<&HashMap<String, Alias>>,
    ) {
        for (path, value) in writes {
            let Val::Str(p) = path else { continue };
            let segs = self.abs(p, inst);
            self.put(&segs, value, None, 0);
            if let Some(f) = from.and_then(|m| m.get(&**p)) {
                let ab = f.1.iter().enumerate().map(|(i, x)| (i, *x)).collect();
                let ba = f.1.iter().enumerate().map(|(i, x)| (*x, i)).collect();
                self.aliases.push(Rc::new(AliasLink {
                    a: segs.clone(),
                    b: split(&f.0),
                    ab,
                    ba,
                }));
            }
        }
    }

    fn put(&mut self, segs: &[String], v: &Val, skip: Option<&Rc<AliasLink>>, depth: usize) {
        self.state = set_in(Some(&self.state), segs, v, 0);
        if self.aliases.is_empty() {
            return;
        }
        let live: Vec<Rc<AliasLink>> = self
            .aliases
            .iter()
            .filter(|al| !(prefix(segs, &al.a) || prefix(segs, &al.b)))
            .cloned()
            .collect();
        if live.len() != self.aliases.len() {
            self.aliases = live.clone();
        }
        if depth > 4 {
            return;
        }
        for al in &live {
            if skip.is_some_and(|s| Rc::ptr_eq(s, al)) {
                continue;
            }
            for (x, y, m) in [(&al.a, &al.b, &al.ab), (&al.b, &al.a, &al.ba)] {
                if segs.len() > x.len() + 1 && prefix(x, segs) {
                    let j = segs[x.len()]
                        .parse::<usize>()
                        .ok()
                        .filter(|_| digits_only(&segs[x.len()]))
                        .and_then(|n| m.get(&n).copied());
                    if let Some(j) = j {
                        let mut p = y.clone();
                        p.push(j.to_string());
                        p.extend(segs[x.len() + 1..].iter().cloned());
                        self.put(&p, v, Some(al), depth + 1);
                    }
                }
            }
        }
    }
}

fn js_falsy_str(v: &Val) -> bool {
    match v {
        Val::Null => true,
        Val::Bool(b) => !b,
        Val::Num(n) => n.is_zero(),
        Val::Str(s) => s.is_empty(),
        _ => false,
    }
}

fn same_id(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Str(x), Val::Str(y)) => x == y,
        (Val::Num(x), Val::Num(y)) => x == y,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Null, Val::Null) => true,
        _ => false,
    }
}

pub fn write_paths(info: &Info, kind: &str, rows: &[Val]) -> Option<Vec<String>> {
    let d = &info.d;
    match kind {
        "expression" | "match" => d
            .get("key")
            .and_then(|k| k.as_str())
            .map(|k| vec![k.to_string()]),
        "assertion" => d
            .get("output")
            .and_then(|k| k.as_str())
            .map(|k| vec![k.to_string()]),
        "decisionTable" => {
            if d.get("outputs").is_some_and(|o| o.is_arr())
                && d.get("rules").is_some_and(|o| o.is_arr())
            {
                Some(dt_write_paths(d, rows))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn models_of(rel: &[Val]) -> HashMap<String, Val> {
    let mut m = HashMap::default();
    for d in rel {
        let Some(Val::Arr(blocks)) = d.get("blocks") else {
            continue;
        };
        for b in blocks.iter() {
            if !matches!(b.get("type"), Some(Val::Str(t)) if &**t == "dataModel") {
                continue;
            }
            let Some(Val::Arr(props)) = b
                .get("props")
                .and_then(|p| p.get("data"))
                .and_then(|d| d.get("properties"))
            else {
                continue;
            };
            for p in props.iter() {
                if let Some(Val::Str(id)) = p.get("id") {
                    m.insert(id.to_string(), p.clone());
                }
            }
        }
    }
    m
}

fn target_of(p: &Val) -> Option<String> {
    match p.get("target") {
        Some(Val::Str(s)) if !s.is_empty() => Some(s.to_string()),
        Some(Val::Num(n)) if !n.is_zero() => Some(super::value::js_num(n)),
        Some(Val::Bool(true)) => Some("true".to_string()),
        Some(v @ (Val::Arr(_) | Val::Obj(_))) => Some(js_string(v)),
        _ => None,
    }
}

fn truthy(v: Option<&Val>) -> bool {
    match v {
        None => false,
        Some(v) => !js_falsy_str(v),
    }
}

pub struct ReadSet {
    order: Vec<String>,
    seen: HashSet<String>,
}

impl ReadSet {
    pub fn new() -> Self {
        Self {
            order: Vec::new(),
            seen: HashSet::default(),
        }
    }

    fn add(&mut self, s: String) {
        if self.seen.insert(s.clone()) {
            self.order.push(s);
        }
    }

    pub fn sorted(self) -> Vec<String> {
        let mut v = self.order;
        js_sort(&mut v);
        v
    }
}

fn expand_id(models: &HashMap<String, Val>, id: &str, out: &mut ReadSet) {
    out.add(id.to_string());
    let s: Vec<&str> = id.split('.').collect();
    let mut model = s[0].to_string();
    let mut last: isize = -1;
    let mut j = 1;
    while j + 1 < s.len() {
        let q = models.get(&format!("{}.{}", model, s[j]));
        let Some(q) = q else { break };
        let Some(t) = target_of(q) else { break };
        if matches!(q.get("type"), Some(Val::Str(x)) if &**x == "reference") {
            break;
        }
        model = t;
        last = j as isize;
        j += 1;
    }
    if last >= 0 {
        out.add(format!("{}.{}", model, s[(last as usize) + 1..].join(".")));
    }
}

fn skip_str(src: &[char], i: usize) -> usize {
    let c = src[i];
    let mut i = i + 1;
    while i < src.len() && src[i] != c {
        if src[i] == '\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

fn close_paren(src: &[char], mut i: usize) -> usize {
    let mut d: i64 = 0;
    while i < src.len() {
        let c = src[i];
        if c == '\'' || c == '"' || c == '`' {
            i = skip_str(src, i);
            continue;
        }
        if c == '(' || c == '[' || c == '{' {
            d += 1;
        } else if c == ')' || c == ']' || c == '}' {
            d -= 1;
            if d == 0 {
                return i;
            }
        }
        i += 1;
    }
    src.len()
}

fn split_top(src: &[char]) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    let mut d: i64 = 0;
    let mut st = 0;
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if c == '\'' || c == '"' || c == '`' {
            i = skip_str(src, i);
            continue;
        }
        if c == '(' || c == '[' || c == '{' {
            d += 1;
        } else if c == ')' || c == ']' || c == '}' {
            d -= 1;
        } else if c == ',' && d == 0 {
            out.push(src[st..i].to_vec());
            st = i + 1;
        }
        i += 1;
    }
    out.push(src[st.min(src.len())..].to_vec());
    out
}

fn slice(src: &[char], a: usize, b: usize) -> &[char] {
    let b = b.min(src.len());
    let a = a.min(b);
    &src[a..b]
}

fn re_call() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").unwrap())
}

fn re_hash() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^#\.([A-Za-z_$][A-Za-z0-9_$]*)").unwrap())
}

fn re_head() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\(?\s*([A-Za-z_$][A-Za-z0-9_$.]*)").unwrap())
}

fn re_as() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"([A-Za-z_$][A-Za-z0-9_$.]*)\s+as\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
    })
}

fn re_index() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"([A-Za-z_$][A-Za-z0-9_$.]*)\[[^\]]*\]\.([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
    })
}

fn is_array_prop(p: &Val) -> bool {
    truthy(p.get("array"))
}

fn coll_target(models: &HashMap<String, Val>, expr: &[char], t: Option<&str>) -> Option<String> {
    let s: String = expr.iter().collect();
    let e = s.trim();
    if let Some(c) = re_call().find(e) {
        let ec: Vec<char> = e.chars().collect();
        let k = e[..c.end()].chars().count() - 1;
        let end = close_paren(&ec, k);
        let args = split_top(slice(&ec, k + 1, end));
        return coll_target(models, &args[0], t);
    }
    let p = if let Some(m) = re_hash().captures(e) {
        match t {
            Some(t) => models.get(&format!("{}.{}", t, &m[1])),
            None => None,
        }
    } else {
        re_head().captures(e).and_then(|m| models.get(&m[1]))
    };
    let p = p?;
    if is_array_prop(p) {
        target_of(p)
    } else {
        None
    }
}

fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.'
}

fn closures(models: &HashMap<String, Val>, src: &[char], t: Option<&str>, out: &mut ReadSet) {
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        if c == '\'' || c == '"' || c == '`' {
            i = skip_str(src, i);
            continue;
        }
        if c == '#' && i + 1 < n && src[i + 1] == '.' {
            let mut j = i + 2;
            while j < n && ident_char(src[j]) {
                j += 1;
            }
            if let Some(t) = t {
                let mut tail: String = src[i + 2..j].iter().collect();
                if tail.ends_with('.') {
                    tail.pop();
                }
                expand_id(models, &format!("{}.{}", t, tail), out);
            }
            i = j;
            continue;
        }
        let prev_ok = i == 0 || !ident_char(src[i - 1]);
        if (c.is_ascii_alphabetic() || c == '_' || c == '$') && prev_ok {
            let mut j = i;
            while j < n && ident_char(src[j]) {
                j += 1;
            }
            let mut k = j;
            while k < n && src[k] == ' ' {
                k += 1;
            }
            if k < n && src[k] == '(' {
                let end = close_paren(src, k);
                let args = split_top(slice(src, k + 1, end));
                closures(models, &args[0], t, out);
                if args.len() > 1 {
                    let ct = coll_target(models, &args[0], t);
                    for a in &args[1..] {
                        closures(models, a, ct.as_deref(), out);
                    }
                }
                i = end + 1;
                continue;
            }
            i = j;
            continue;
        }
        i += 1;
    }
}

fn expr_reads(models: &HashMap<String, Val>, src: Option<&Val>, out: &mut ReadSet) {
    let Some(Val::Str(src)) = src else { return };
    if src.is_empty() {
        return;
    }
    let mut ids = Vec::new();
    idents(src, &mut ids);
    let mut alias: HashMap<String, String> = HashMap::default();
    for m in re_as().captures_iter(src) {
        if let Some(p) = models.get(&m[1]) {
            if let Some(t) = target_of(p) {
                alias.insert(m[2].to_string(), t);
            }
        }
    }
    for id in ids {
        if id == "as" || alias.contains_key(&id) {
            continue;
        }
        let head = id.split('.').next().unwrap_or("");
        match alias.get(head) {
            Some(t) => expand_id(models, &format!("{}{}", t, &id[head.len()..]), out),
            None => expand_id(models, &id, out),
        }
    }
    if src.contains('#') {
        let chars: Vec<char> = src.chars().collect();
        closures(models, &chars, None, out);
    }
    for m in re_index().captures_iter(src) {
        if let Some(p) = models.get(&m[1]) {
            if is_array_prop(p) {
                if let Some(t) = target_of(p) {
                    expand_id(models, &format!("{}.{}", t, &m[2]), out);
                }
            }
        }
    }
}

fn by_id<'a>(list: Option<&'a Val>, key: &Val) -> Option<&'a Val> {
    let Some(Val::Arr(a)) = list else { return None };
    a.iter()
        .rev()
        .find(|x| x.get("id").is_some_and(|i| same_id(i, key)))
}

pub fn reads_of(models: &HashMap<String, Val>, info: &Info, kind: &str, tr: &Val) -> Vec<String> {
    let d = &info.d;
    let mut out = ReadSet::new();
    match kind {
        "expression" => expr_reads(models, d.get("value"), &mut out),
        "match" => {
            if let Some(Val::Arr(arms)) = tr.get("arms") {
                for a in arms.iter() {
                    let id = a.get("id").cloned().unwrap_or(Val::Null);
                    expr_reads(
                        models,
                        by_id(d.get("arms"), &id).and_then(|x| x.get("condition")),
                        &mut out,
                    );
                }
            }
            if let Some(m) = tr.get("matchedArm") {
                expr_reads(
                    models,
                    by_id(d.get("arms"), m).and_then(|x| x.get("value")),
                    &mut out,
                );
            }
        }
        "assertion" => {
            if let Some(Val::Arr(cs)) = tr.get("conditions") {
                for c in cs.iter() {
                    let id = c.get("id").cloned().unwrap_or(Val::Null);
                    expr_reads(
                        models,
                        by_id(d.get("conditions"), &id).and_then(|x| x.get("expression")),
                        &mut out,
                    );
                }
            }
        }
        "decisionTable" => {
            let inputs = d
                .get("inputs")
                .and_then(|x| x.as_arr())
                .cloned()
                .unwrap_or_default();
            for x in inputs.iter() {
                expr_reads(models, x.get("field"), &mut out);
            }
            let rules = d
                .get("rules")
                .and_then(|x| x.as_arr())
                .cloned()
                .unwrap_or_default();
            for r in rules.iter() {
                for x in inputs.iter() {
                    expr_reads(models, rule_cell(r, &x.get("id").cloned()), &mut out);
                }
            }
            let rows = tr
                .get("matchedRows")
                .and_then(|x| x.as_arr())
                .cloned()
                .unwrap_or_default();
            if let Some(Val::Arr(evs)) = tr.get("evaluations") {
                let fields = dt_fields(d);
                for (j, ev) in evs.iter().enumerate() {
                    let Some(rule) = rows
                        .get(j)
                        .and_then(|r| r.as_usize())
                        .and_then(|r| rules.get(r))
                    else {
                        continue;
                    };
                    for o in &fields {
                        if ev.as_obj().is_some_and(|e| e.has(&o.field)) {
                            expr_reads(models, rule_cell(rule, &o.id), &mut out);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out.sorted()
}

pub fn props_of(execs: &[Val], request: &Val, final_state: &Val) -> Obj {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::default();
    let mut add = |k: String, keys: &mut Vec<String>| {
        if seen.insert(k.clone()) {
            keys.push(k);
        }
    };
    for e in execs {
        if truthy(e.get("instancePath")) {
            continue;
        }
        let Some(Val::Arr(ws)) = e.get("writes") else {
            continue;
        };
        for w in ws.iter() {
            let Some(Val::Str(p)) = w.get("path") else {
                continue;
            };
            let s = split(p);
            for i in 1..s.len() {
                let q = &s[..i];
                if get(request, q).is_none() {
                    add(q.join("."), &mut keys);
                }
            }
            add(p.to_string(), &mut keys);
        }
    }
    let mut out = Obj::new();
    for k in keys {
        if let Some(v) = get(final_state, &split(&k)) {
            out.set(&k, v);
        }
    }
    out
}
