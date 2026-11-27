use super::core::pack;
use super::value::{eq, Key, Obj, Val};
use super::{bad, R};
use base64::Engine;
use std::rc::Rc;

pub(crate) const HEAD: [&str; 2] = ["block", "policy"];

pub(crate) const COLUMNS: [&str; 6] = [
    "arms",
    "conditions",
    "rows",
    "failedCells",
    "evaluations",
    "collected",
];

pub(crate) const ORDER: [&str; 18] = [
    "block",
    "kind",
    "member",
    "members",
    "count",
    "policy",
    "arms",
    "conditions",
    "rows",
    "ruleCount",
    "inputCount",
    "failedCells",
    "evaluations",
    "collected",
    "writes",
    "writesAbsent",
    "reads",
    "readsAbsent",
];

pub(crate) fn k(s: &str) -> Key {
    Rc::from(s)
}

pub(crate) const ABSENT_DEFAULT: &str = "absent-default";

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Absent {
    pub writes: bool,
    pub reads: bool,
    pub operands: bool,
}

impl Absent {
    pub const NAMES: [&'static str; 3] = ["writes", "reads", "operands"];

    pub(crate) fn flags(self) -> [bool; 3] {
        [self.writes, self.reads, self.operands]
    }

    pub fn any(self) -> bool {
        self.writes || self.reads || self.operands
    }

    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMES
            .into_iter()
            .zip(self.flags())
            .filter_map(|(n, f)| f.then_some(n))
    }

    pub fn val(self) -> Val {
        Val::arr(self.names().map(Val::str).collect())
    }

    pub fn defaults(missing: usize, of: usize) -> bool {
        missing >= 2 && missing * 2 > of
    }

    pub(crate) fn of_execs(execs: &[Val]) -> Absent {
        let n = execs.iter().filter(|e| e.is_obj()).count();
        let most = |key: &str| {
            let missing = execs
                .iter()
                .filter(|e| e.as_obj().is_some_and(|o| !o.has(key)))
                .count();
            Self::defaults(missing, n)
        };
        Absent {
            writes: most("writes"),
            reads: most("reads"),
            operands: most("operandValues"),
        }
    }

    pub(crate) fn of_body(body: &Obj) -> R<Absent> {
        let Some(list) = body.get("absent") else {
            return Ok(Absent::default());
        };
        let names = str_list(list).ok_or_else(|| bad("absent must be a list of names"))?;
        let mut out = Absent::default();
        for name in &names {
            match name.as_str() {
                "writes" => out.writes = true,
                "reads" => out.reads = true,
                "operands" => out.operands = true,
                _ => return Err(bad(format!("unknown absent field {name}"))),
            }
        }
        Ok(out)
    }
}

pub(crate) fn str_list(v: &Val) -> Option<Vec<String>> {
    match v {
        Val::Arr(a) => a
            .iter()
            .map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => None,
    }
}

pub(crate) struct Shape {
    pub(crate) rules: usize,
    pub(crate) inputs: usize,
    pub(crate) fail: Vec<(usize, Vec<usize>)>,
}

pub(crate) fn infer_shape(b64: Option<&Val>) -> Option<Shape> {
    let Some(Val::Str(s)) = b64 else { return None };
    let b = base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .ok()?;
    if base64::engine::general_purpose::STANDARD.encode(&b) != **s {
        return None;
    }
    let mut hi: i32 = -1;
    for x in &b {
        for k in 0..8 {
            if (x >> k) & 1 == 1 {
                hi = hi.max(k);
            }
        }
    }
    let n = (hi + 1).max(1) as usize;
    let mut fail = Vec::new();
    for (r, x) in b.iter().enumerate() {
        let mut f = Vec::new();
        for i in 0..n {
            if (x >> i) & 1 == 0 {
                f.push(i);
            }
        }
        if !f.is_empty() {
            fail.push((r, f));
        }
    }
    Some(Shape {
        rules: b.len(),
        inputs: if b.is_empty() { 0 } else { n },
        fail,
    })
}

pub(crate) fn same_prim(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Arr(_) | Val::Obj(_), _) => a.ptr() == b.ptr(),
        _ => eq(a, b),
    }
}

pub(crate) fn sweep_of(at: Option<&Val>) -> Option<(String, u64)> {
    let Some(Val::Str(s)) = at else { return None };
    let dot = s.rfind('.')?;
    let tail = &s[dot + 1..];
    if tail.is_empty() || !tail.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n = tail.parse::<f64>().ok()? as u64;
    Some((s[..dot].to_string(), n))
}

pub(crate) fn same_head(a: &Obj, b: &Obj) -> bool {
    HEAD.iter().all(|h| match (a.get(h), b.get(h)) {
        (None, None) => true,
        (Some(x), Some(y)) => same_prim(x, y),
        _ => false,
    })
}

pub(crate) fn write_columns(rs: &[Obj]) -> Option<Val> {
    let ws: Vec<Option<&Val>> = rs.iter().map(|r| r.get("writes")).collect();
    let objs: Option<Vec<&Obj>> = ws
        .iter()
        .map(|w| match w {
            Some(Val::Obj(o)) if o.len() == 1 => Some(&**o),
            _ => None,
        })
        .collect();
    if let Some(objs) = objs {
        let p = objs.first()?.first_key()?.clone();
        let col: Vec<Val> = objs
            .iter()
            .map(|o| o.get(&p).cloned())
            .collect::<Option<_>>()?;
        let mut o = Obj::new();
        o.insert(p, Val::arr(col));
        return Some(Val::obj(o));
    }
    let arrs: Option<Vec<&Vec<Val>>> = ws
        .iter()
        .map(|w| match w {
            Some(Val::Arr(a)) if a.len() > 1 => Some(&**a),
            _ => None,
        })
        .collect();
    if let Some(arrs) = arrs {
        let path_of = |x: &Val| x.get("path").cloned().unwrap_or(Val::Null);
        let first = arrs.first()?;
        let paths: Vec<Val> = first.iter().map(path_of).collect();
        let pv = Val::arr(paths.clone());
        if !arrs
            .iter()
            .all(|a| eq(&Val::arr(a.iter().map(path_of).collect()), &pv))
        {
            return None;
        }
        let list = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut o = Obj::with_capacity(2);
                o.insert(k("path"), p.clone());
                o.insert(
                    k("values"),
                    Val::arr(
                        arrs.iter()
                            .map(|a| {
                                a.get(i)
                                    .and_then(|x| x.get("value"))
                                    .cloned()
                                    .unwrap_or(Val::Null)
                            })
                            .collect(),
                    ),
                );
                Val::obj(o)
            })
            .collect();
        return Some(Val::arr(list));
    }
    None
}

pub(crate) fn build_run(first: &Obj, coll: &str, count: usize, len: i64, recs: Vec<Obj>) -> Obj {
    let mut run = Obj::new();
    for h in HEAD {
        if let Some(v) = first.get(h) {
            run.insert(k(h), v.clone());
        }
    }
    run.insert(k("members"), Val::str(coll));
    if count as i64 != len {
        run.insert(k("count"), Val::int(count as i64));
    }
    let mut rs: Vec<Obj> = recs
        .into_iter()
        .map(|mut r| {
            for h in HEAD {
                r.remove(h);
            }
            r.remove("member");
            r
        })
        .collect();
    if let Some(wc) = write_columns(&rs) {
        run.insert(k("writes"), wc);
        for r in rs.iter_mut() {
            r.remove("writes");
        }
    }
    let mut keys: Vec<Key> = Vec::new();
    for r in &rs {
        for key in r.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }
    for key in keys {
        if COLUMNS.contains(&&*key) {
            run.insert(
                key.clone(),
                Val::arr(
                    rs.iter()
                        .map(|r| r.get(&key).cloned().unwrap_or(Val::Null))
                        .collect(),
                ),
            );
            for r in rs.iter_mut() {
                r.remove(&key);
            }
        } else if &*key != "writes" {
            let Some(v) = rs.first().and_then(|r| r.get(&key)).cloned() else {
                continue;
            };
            if rs.iter().all(|r| r.get(&key).is_some_and(|x| eq(x, &v))) {
                run.insert(key.clone(), v);
                for r in rs.iter_mut() {
                    r.remove(&key);
                }
            }
        }
    }
    if rs.iter().any(|r| !r.is_empty()) {
        run.insert(
            k("perMember"),
            Val::arr(rs.into_iter().map(Val::obj).collect()),
        );
    }
    run
}

pub(crate) struct Item {
    pub(crate) rec: Obj,
    pub(crate) len: i64,
}

pub(crate) fn group_runs(items: Vec<Item>) -> Vec<Obj> {
    let mut out = Vec::new();
    let mut i = 0;
    let n = items.len();
    while i < n {
        let sw = sweep_of(items[i].rec.get("member"));
        let mut j = i + 1;
        if let Some((coll, 0)) = &sw {
            while j < n
                && same_head(&items[j].rec, &items[i].rec)
                && matches!(items[j].rec.get("member"), Some(Val::Str(m)) if **m == format!("{}.{}", coll, j - i))
            {
                j += 1;
            }
        }
        let cnt = j - i;
        match &sw {
            Some((coll, 0)) if cnt >= 2 => {
                let recs: Vec<Obj> = items[i..j].iter().map(|x| x.rec.clone()).collect();
                out.push(build_run(&items[i].rec, coll, cnt, items[i].len, recs));
            }
            _ => {
                out.extend(items[i..j].iter().map(|x| x.rec.clone()));
            }
        }
        i = j;
    }
    out
}

pub(crate) fn is_run(rec: &Obj) -> bool {
    matches!(rec.get("members"), Some(Val::Str(_)))
}

pub(crate) type PathFn<'a> = &'a dyn Fn(&str) -> R<String>;

pub(crate) fn map_str(v: &Val, p: PathFn) -> R<Val> {
    match v {
        Val::Str(s) => Ok(Val::str(&p(s)?)),
        _ => Err(bad("path must be a string")),
    }
}

pub(crate) fn map_keys(o: &Obj, p: PathFn) -> R<Obj> {
    let mut out = Obj::with_capacity(o.len());
    for (key, v) in o.iter() {
        out.insert(Rc::from(p(key)?.as_str()), v.clone());
    }
    Ok(out)
}

pub(crate) fn map_list(r: &Val, p: PathFn) -> R<Val> {
    match r {
        Val::Arr(a) => Ok(Val::arr(a.iter().map(|x| map_str(x, p)).collect::<R<_>>()?)),
        Val::Obj(o) => {
            let mut out = Obj::new();
            if let Some(a) = o.get("add") {
                out.insert(k("add"), map_list(a, p)?);
            }
            if let Some(a) = o.get("remove") {
                out.insert(k("remove"), map_list(a, p)?);
            }
            Ok(Val::obj(out))
        }
        _ => Ok(r.clone()),
    }
}

pub(crate) fn paths_out(rec: &Obj, p: PathFn) -> R<Obj> {
    let mut r = rec.clone();
    let run = is_run(rec);
    if let Some(m) = rec.get("member") {
        r.set("member", map_str(m, p)?);
    }
    if let Some(m) = rec.get("members") {
        r.set("members", map_str(m, p)?);
    }
    if let Some(w) = rec.get("writes") {
        let nw = match w {
            Val::Arr(a) => Val::arr(
                a.iter()
                    .map(|x| {
                        let mut o = x
                            .as_obj()
                            .cloned()
                            .ok_or_else(|| bad("write must be a map"))?;
                        let path = o.get("path").cloned().unwrap_or(Val::Null);
                        o.set("path", map_str(&path, p)?);
                        Ok(Val::obj(o))
                    })
                    .collect::<R<_>>()?,
            ),
            Val::Obj(o) => Val::obj(map_keys(o, p)?),
            _ => w.clone(),
        };
        r.set("writes", nw);
    }
    if let Some(x) = rec.get("reads") {
        if !matches!(x, Val::Str(s) if &**s == "previous") {
            r.set("reads", map_list(x, p)?);
        }
    }
    if let Some(Val::Obj(o)) = rec.get("storedOperands") {
        r.set("storedOperands", Val::obj(map_keys(o, p)?));
    }
    if let Some(x) = rec.get("operandKeys") {
        r.set("operandKeys", map_list(x, p)?);
    }
    if let Some(x @ Val::Arr(_)) = rec.get("operandsAfterWrite") {
        r.set("operandsAfterWrite", map_list(x, p)?);
    }
    let ev = |x: &Val| -> R<Val> {
        match x {
            Val::Arr(a) => Ok(Val::arr(
                a.iter().map(|fs| map_list(fs, p)).collect::<R<_>>()?,
            )),
            _ => Ok(x.clone()),
        }
    };
    let col = |x: &Val| -> R<Val> {
        match x {
            Val::Arr(_) => map_list(x, p),
            _ => Ok(x.clone()),
        }
    };
    if let Some(x) = rec.get("evaluations") {
        let nv = if run {
            match x {
                Val::Arr(a) => Val::arr(a.iter().map(&ev).collect::<R<_>>()?),
                _ => return Err(bad("evaluations column")),
            }
        } else {
            ev(x)?
        };
        r.set("evaluations", nv);
    }
    if let Some(x) = rec.get("collected") {
        let nv = if run {
            match x {
                Val::Arr(a) => Val::arr(a.iter().map(&col).collect::<R<_>>()?),
                _ => return Err(bad("collected column")),
            }
        } else {
            col(x)?
        };
        r.set("collected", nv);
    }
    if let Some(Val::Arr(pm)) = rec.get("perMember") {
        let list = pm
            .iter()
            .map(|m| {
                Ok(Val::obj(paths_out(
                    m.as_obj().ok_or_else(|| bad("perMember entry"))?,
                    p,
                )?))
            })
            .collect::<R<_>>()?;
        r.set("perMember", Val::arr(list));
    }
    Ok(r)
}

pub(crate) fn rank(key: &str) -> usize {
    match ORDER.iter().position(|x| *x == key) {
        Some(i) => i,
        None => {
            ORDER.len()
                + if key == "details" {
                    1
                } else if key == "perMember" {
                    2
                } else {
                    0
                }
        }
    }
}

pub(crate) fn pack_record(r: &Obj) -> Val {
    let mut entries: Vec<(&Key, &Val)> = r.iter().collect();
    entries.sort_by_key(|(key, _)| rank(key));
    let mut out = Obj::with_capacity(entries.len());
    for (key, v) in entries {
        out.insert(key.clone(), pack(v));
    }
    Val::obj(out)
}
