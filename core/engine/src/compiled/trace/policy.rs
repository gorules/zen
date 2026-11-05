use super::core::{
    dot_to_ptr, envelope, escape, is_ref, pack, ptr_to_dot, ref_in, ref_out, unescape, Seg,
};
use super::sim::{
    apply_delta, find_alias, get, list_delta, props_of, reads_of, split, write_paths, Alias, Info,
    Sim,
};
use super::value::{eq, json_len, Key, Obj, Val};
use super::{bad, Need, TraceError, R};
use ahash::{HashMap, HashSet};
use base64::Engine;
use std::rc::Rc;

const TOP: [&str; 3] = ["engineVersion", "executions", "properties"];
const EXEC_KEYS: [&str; 7] = [
    "blockId",
    "instancePath",
    "policyPath",
    "operandValues",
    "reads",
    "trace",
    "writes",
];
const KINDS: [&str; 4] = ["expression", "match", "assertion", "decisionTable"];
const HEAD: [&str; 2] = ["block", "policy"];
const COLUMNS: [&str; 6] = [
    "arms",
    "conditions",
    "rows",
    "failedCells",
    "evaluations",
    "collected",
];
const ORDER: [&str; 18] = [
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

fn k(s: &str) -> Key {
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

    fn flags(self) -> [bool; 3] {
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

    fn of_execs(execs: &[Val]) -> Absent {
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

    fn of_body(body: &Obj) -> R<Absent> {
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

fn absent_flag(rec: &Obj, key: &str, default: bool) -> bool {
    match rec.get(key) {
        Some(Val::Bool(b)) => *b,
        Some(_) => false,
        None => default,
    }
}

fn mark_absent(rec: &mut Obj, key: &str, absent: bool, default: bool) {
    if absent != default {
        rec.insert(k(key), Val::Bool(absent));
    }
}

fn strs(v: &[String]) -> Val {
    Val::arr(v.iter().map(|s| Val::str(s)).collect())
}

fn str_list(v: &Val) -> Option<Vec<String>> {
    match v {
        Val::Arr(a) => a
            .iter()
            .map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => None,
    }
}

fn js_string(v: &Val) -> String {
    match v {
        Val::Str(s) => s.to_string(),
        Val::Num(n) => super::value::js_num(n),
        Val::Null => "null".to_string(),
        Val::Bool(b) => b.to_string(),
        _ => v.to_json(),
    }
}

fn template(v: Option<&Val>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(v) => js_string(v),
    }
}

type Writes = Vec<(Val, Val)>;

fn writes_pairs(ws: &Val) -> Writes {
    match ws {
        Val::Arr(a) => a
            .iter()
            .map(|w| match w {
                Val::Obj(o) => (
                    o.get("path").cloned().unwrap_or(Val::Null),
                    o.get("value").cloned().unwrap_or(Val::Null),
                ),
                _ => (Val::Null, Val::Null),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn ok_writes(ws: Option<&Val>) -> bool {
    let Some(Val::Arr(a)) = ws else { return false };
    let mut seen: HashSet<&str> = HashSet::default();
    for w in a.iter() {
        let Val::Obj(o) = w else { return false };
        if o.len() != 2 || !o.has("value") {
            return false;
        }
        let Some(Val::Str(p)) = o.get("path") else {
            return false;
        };
        if !seen.insert(p) {
            return false;
        }
    }
    true
}

fn arm_list(l: Option<&Val>) -> Option<Vec<(&Val, &Val)>> {
    let Some(Val::Arr(a)) = l else { return None };
    a.iter()
        .map(|x| match x {
            Val::Obj(o) if o.len() == 2 => match (o.get("id"), o.get("result")) {
                (Some(id @ Val::Str(_)), Some(result @ Val::Bool(_))) => Some((id, result)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn default_ids(block: &str, n: usize) -> Vec<Val> {
    (0..n).map(|i| Val::str(&format!("{block}:{i}"))).collect()
}

fn from_value(sim: &Sim, al: &Alias) -> Val {
    let arr = get(&sim.state, &al.0.split('.').collect::<Vec<_>>());
    Val::arr(
        al.1.iter()
            .map(|&i| {
                arr.as_ref()
                    .and_then(|a| a.as_arr())
                    .and_then(|a| a.get(i))
                    .cloned()
                    .unwrap_or(Val::Null)
            })
            .collect(),
    )
}

fn enc_val(v: &Val, al: Option<&Alias>) -> Val {
    if let Some(al) = al {
        let segs: Vec<Seg> = split(&al.0)
            .into_iter()
            .map(|s| Seg::K(Rc::from(s.as_str())))
            .collect();
        let r = ref_out("current", &segs, Some(al.1.clone()));
        if json_len(&r) < json_len(v) {
            return r;
        }
    }
    escape(v)
}

fn dec_val(sim: &Sim, v: &Val) -> R<Val> {
    if is_ref(v) {
        let r = ref_in(v)?;
        if r.source != "current" {
            return Err(bad(format!("policy $ref source {}", r.source)));
        }
        let items: Vec<usize> = match &r.items {
            Some(Val::Arr(a)) => a
                .iter()
                .map(|i| i.as_usize().ok_or_else(|| bad("$ref items index")))
                .collect::<R<_>>()?,
            _ => return Err(bad("policy $ref needs items")),
        };
        let path: Vec<String> = r.segs.iter().map(|s| s.text().into_owned()).collect();
        return Ok(from_value(sim, &(path.join("."), items)));
    }
    Ok(unescape(v))
}

fn aliases_of(
    sim: &Sim,
    ids: &[String],
    writes: &Writes,
    inst: Option<&str>,
) -> Option<HashMap<String, Alias>> {
    if ids.is_empty() {
        return None;
    }
    let mut from: Option<HashMap<String, Alias>> = None;
    for (path, value) in writes {
        if let Some(al) = find_alias(sim, ids, value, inst) {
            if eq(&from_value(sim, &al), value) {
                from.get_or_insert_with(HashMap::default)
                    .insert(js_string(path), al);
            }
        }
    }
    from
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

fn pass_bytes(rules: usize, inputs: usize, fail: Option<&Val>) -> R<String> {
    if rules > 1_000_000 || inputs > 4096 {
        return Err(bad("decision table shape too large"));
    }
    let per = inputs.div_ceil(8);
    let mut bytes = Vec::with_capacity(rules * per);
    for r in 0..rules {
        let f: HashSet<usize> = match fail.and_then(|f| f.get(&r.to_string())) {
            Some(Val::Arr(a)) => a.iter().filter_map(|x| x.as_usize()).collect(),
            _ => HashSet::default(),
        };
        for b in 0..per {
            let mut x: u8 = 0;
            for k in 0..8 {
                let i = b * 8 + k;
                if i < inputs && !f.contains(&i) {
                    x |= 1 << k;
                }
            }
            bytes.push(x);
        }
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn writes_map(writes: &Writes) -> Obj {
    let mut o = Obj::new();
    for (p, v) in writes {
        o.set(&js_string(p), v.clone());
    }
    o
}

fn wv_get<'a>(writes: &'a Writes, f: &str) -> Option<&'a Val> {
    writes
        .iter()
        .rev()
        .find(|(p, _)| matches!(p, Val::Str(s) if &**s == f))
        .map(|(_, v)| v)
}

fn sorted_keys(o: &Obj) -> Vec<String> {
    let mut v: Vec<String> = o.keys().map(|k| k.to_string()).collect();
    super::sim::js_sort(&mut v);
    v
}

fn evals_in(evaluations: &[Vec<String>], collected: &[String], writes: &Writes) -> Option<Val> {
    let col: HashSet<&String> = collected.iter().collect();
    let mut at: HashMap<String, usize> = HashMap::default();
    let mut out = Vec::with_capacity(evaluations.len());
    for fs in evaluations {
        let mut ev = Obj::new();
        for f in fs {
            if col.contains(f) {
                let i = *at.get(f).unwrap_or(&0);
                at.insert(f.clone(), i + 1);
                let v = match wv_get(writes, f) {
                    Some(Val::Arr(a)) => a.get(i).cloned(),
                    Some(p) if super::core::lenient() && super::core::is_placeholder(p) => {
                        Some(p.clone())
                    }
                    _ => None,
                }?;
                ev.set(f, v);
            } else {
                ev.set(f, wv_get(writes, f)?.clone());
            }
        }
        out.push(Val::obj(ev));
    }
    Some(Val::arr(out))
}

fn parse_evals(rec: &Obj) -> Option<(Vec<Vec<String>>, Vec<String>)> {
    let ev = rec.get("evaluations")?;
    let list: Vec<Vec<String>> = ev.as_arr()?.iter().map(str_list).collect::<Option<_>>()?;
    let col = match rec.get("collected") {
        Some(c) => str_list(c)?,
        None => Vec::new(),
    };
    Some((list, col))
}

enum EvalsOut {
    Bad,
    Default,
    Stored(Vec<Vec<String>>, Vec<String>),
}

fn evals_out(evals: Option<&Val>, rows: &[Val], writes: &Writes) -> EvalsOut {
    let Some(Val::Arr(evals)) = evals else {
        return EvalsOut::Bad;
    };
    if evals.len() != rows.len() || !evals.iter().all(|e| e.is_obj()) {
        return EvalsOut::Bad;
    }
    if rows.is_empty() {
        return EvalsOut::Default;
    }
    if rows.len() == 1 && eq(&evals[0], &Val::obj(writes_map(writes))) {
        return EvalsOut::Default;
    }
    let Some(fields) = evals
        .iter()
        .map(|e| e.as_obj().map(sorted_keys))
        .collect::<Option<Vec<Vec<String>>>>()
    else {
        return EvalsOut::Bad;
    };
    let mut all: Vec<String> = Vec::new();
    for f in fields.iter().flatten() {
        if !all.contains(f) {
            all.push(f.clone());
        }
    }
    if !all.iter().all(|f| wv_get(writes, f).is_some()) {
        return EvalsOut::Bad;
    }
    let collected: Vec<String> = all
        .iter()
        .filter(|f| {
            !evals.iter().all(|ev| match ev.get(f) {
                None => true,
                Some(x) => wv_get(writes, f).is_some_and(|w| eq(x, w)),
            })
        })
        .cloned()
        .collect();
    match evals_in(&fields, &collected, writes) {
        Some(back) if eq(&back, &Val::arr(evals.to_vec())) => EvalsOut::Stored(fields, collected),
        _ => EvalsOut::Bad,
    }
}

struct TraceOut {
    rec: Obj,
    details: Obj,
}

fn whole_trace(kind: Option<&Val>, tr: &Val) -> TraceOut {
    let mut rec = Obj::new();
    if let Some(kv) = kind {
        rec.insert(k("kind"), kv.clone());
    }
    let mut details = Obj::new();
    details.insert(k("trace"), tr.clone());
    TraceOut { rec, details }
}

fn trace_out(e: &Obj, block: &str, writes: Option<&Writes>) -> R<TraceOut> {
    let tr = e.get("trace");
    let kind = tr.and_then(|t| t.get("kind"));
    let kind_s = kind.and_then(|x| x.as_str()).filter(|s| KINDS.contains(s));
    let (Some(tr @ Val::Obj(to)), Some(kind_s), Some(kind)) = (tr, kind_s, kind) else {
        let mut details = Obj::new();
        details.insert(k("trace"), tr.cloned().unwrap_or(Val::Null));
        return Ok(TraceOut {
            rec: Obj::new(),
            details,
        });
    };
    let empty: Writes = Vec::new();
    let ws = writes.unwrap_or(&empty);
    let w0 = ws.first();
    let mut rec = Obj::new();
    rec.insert(k("kind"), kind.clone());
    let mut det = Obj::new();
    match kind_s {
        "expression" => {
            let (Some(prop), Some(v)) = (to.get("property"), to.get("value")) else {
                return Ok(whole_trace(Some(kind), tr));
            };
            if to
                .keys()
                .any(|x| !matches!(&**x, "kind" | "property" | "value"))
            {
                return Ok(whole_trace(Some(kind), tr));
            }
            if !w0.is_some_and(|w| same_prim(&w.0, prop)) {
                det.insert(k("property"), prop.clone());
            }
            if !w0.is_some_and(|w| eq(&w.1, v)) {
                det.insert(k("value"), v.clone());
            }
        }
        "match" | "assertion" => {
            let field = if kind_s == "match" {
                "arms"
            } else {
                "conditions"
            };
            let Some(list) = arm_list(to.get(field)) else {
                return Ok(whole_trace(Some(kind), tr));
            };
            rec.insert(
                k(field),
                Val::arr(list.iter().map(|(_, r)| (*r).clone()).collect()),
            );
            let ids: Vec<Val> = list.iter().map(|(id, _)| (*id).clone()).collect();
            if !eq(
                &Val::arr(ids.clone()),
                &Val::arr(default_ids(block, list.len())),
            ) {
                det.insert(k("ids"), Val::arr(ids));
            }
            let vkey = if kind_s == "match" { "value" } else { "result" };
            let Some(v) = to.get(vkey) else {
                return Ok(whole_trace(Some(kind), tr));
            };
            if !w0.is_some_and(|w| eq(&w.1, v)) {
                det.insert(k("value"), v.clone());
            }
            if kind_s == "match" {
                let first = list
                    .iter()
                    .find(|(_, r)| matches!(r, Val::Bool(true)))
                    .map(|(id, _)| (*id).clone());
                let m = to.get("matchedArm");
                let same = match (m, &first) {
                    (None, None) => true,
                    (Some(a), Some(b)) => same_prim(a, b),
                    _ => false,
                };
                if !same {
                    det.insert(k("matchedArm"), m.cloned().unwrap_or(Val::Null));
                }
            }
        }
        _ => {
            let rows = to.get("matchedRows");
            let rows_ok = matches!(rows, Some(Val::Arr(a)) if a.iter().all(|r| matches!(r, Val::Num(n) if n.is_integer() && !n.is_sign_negative() || n.is_zero())));
            match rows {
                Some(r) if rows_ok => rec.insert(k("rows"), r.clone()),
                Some(r) => det.insert(k("matchedRows"), r.clone()),
                None => return Ok(whole_trace(Some(kind), tr)),
            }
            let extras = to.get("extras");
            let shape = infer_shape(extras.and_then(|x| x.get("inputPass")));
            match (shape, extras) {
                (Some(s), Some(Val::Obj(x))) if x.len() == 1 => {
                    rec.insert(k("ruleCount"), Val::int(s.rules as i64));
                    rec.insert(k("inputCount"), Val::int(s.inputs as i64));
                    if !s.fail.is_empty() {
                        let mut fc = Obj::new();
                        for (r, ins) in s.fail {
                            fc.insert(
                                Rc::from(r.to_string().as_str()),
                                Val::arr(ins.into_iter().map(|i| Val::int(i as i64)).collect()),
                            );
                        }
                        rec.insert(k("failedCells"), Val::obj(fc));
                    }
                }
                (_, Some(x)) => det.insert(k("extras"), x.clone()),
                (_, None) => return Ok(whole_trace(Some(kind), tr)),
            }
            let ev = match rows {
                Some(Val::Arr(r)) => evals_out(to.get("evaluations"), r, ws),
                _ => EvalsOut::Bad,
            };
            match ev {
                EvalsOut::Bad => match to.get("evaluations") {
                    Some(x) => det.insert(k("evaluations"), x.clone()),
                    None => return Ok(whole_trace(Some(kind), tr)),
                },
                EvalsOut::Default => {}
                EvalsOut::Stored(fields, collected) => {
                    rec.insert(
                        k("evaluations"),
                        Val::arr(fields.iter().map(|f| strs(f)).collect()),
                    );
                    if !collected.is_empty() {
                        rec.insert(k("collected"), strs(&collected));
                    }
                }
            }
        }
    }
    let back = trace_in(&rec, &det, block, ws).ok().flatten();
    if !back.as_ref().is_some_and(|b| eq(b, tr)) {
        return Ok(whole_trace(Some(kind), tr));
    }
    Ok(TraceOut { rec, details: det })
}

fn same_prim(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Arr(_) | Val::Obj(_), _) => a.ptr() == b.ptr(),
        _ => eq(a, b),
    }
}

fn trace_in(rec: &Obj, det: &Obj, block: &str, writes: &Writes) -> R<Option<Val>> {
    if let Some(t) = det.get("trace") {
        return Ok(Some(t.clone()));
    }
    let Some(kind) = rec.get("kind") else {
        return Ok(None);
    };
    let w0 = writes.first();
    let val = match det.get("value") {
        Some(v) => Some(v.clone()),
        None => w0.map(|w| w.1.clone()),
    };
    let ks = kind.as_str().unwrap_or("");
    match ks {
        "expression" => {
            let mut o = Obj::new();
            o.insert(k("kind"), kind.clone());
            let prop = match det.get("property") {
                Some(p) => Some(p.clone()),
                None => w0.map(|w| w.0.clone()),
            };
            o.insert(
                k("property"),
                prop.ok_or_else(|| bad("expression without property"))?,
            );
            o.insert(
                k("value"),
                val.ok_or_else(|| bad("expression without value"))?,
            );
            Ok(Some(Val::obj(o)))
        }
        "match" | "assertion" => {
            let field = if ks == "match" { "arms" } else { "conditions" };
            let bits = rec
                .get(field)
                .and_then(|b| b.as_arr())
                .cloned()
                .unwrap_or_default();
            let ids: Vec<Val> = match det.get("ids") {
                Some(Val::Arr(a)) => a.to_vec(),
                _ => default_ids(block, bits.len()),
            };
            let list: Vec<Val> = ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let mut o = Obj::with_capacity(2);
                    o.insert(k("id"), id.clone());
                    o.insert(k("result"), bits.get(i).cloned().unwrap_or(Val::Null));
                    Val::obj(o)
                })
                .collect();
            let val = val.ok_or_else(|| bad("match without value"))?;
            let mut t = Obj::new();
            if ks == "assertion" {
                t.insert(k("conditions"), Val::arr(list));
                t.insert(k("kind"), kind.clone());
                t.insert(k("result"), val);
                return Ok(Some(Val::obj(t)));
            }
            let first = list
                .iter()
                .find(|a| matches!(a.get("result"), Some(Val::Bool(true))))
                .and_then(|a| a.get("id").cloned());
            t.insert(k("arms"), Val::arr(list));
            t.insert(k("kind"), kind.clone());
            t.insert(k("value"), val);
            let m = match det.get("matchedArm") {
                Some(m) => Some(m.clone()),
                None => first,
            };
            if let Some(m) = m.filter(|m| !m.is_null()) {
                t.insert(k("matchedArm"), m);
            }
            Ok(Some(Val::obj(t)))
        }
        "decisionTable" => {
            let rows = match det.get("matchedRows") {
                Some(r) => r.clone(),
                None => rec
                    .get("rows")
                    .cloned()
                    .ok_or_else(|| bad("table without rows"))?,
            };
            let extras = match det.get("extras") {
                Some(x) => x.clone(),
                None => {
                    let rc = rec
                        .get("ruleCount")
                        .and_then(|x| x.as_usize())
                        .ok_or_else(|| bad("table without ruleCount"))?;
                    let ic = rec
                        .get("inputCount")
                        .and_then(|x| x.as_usize())
                        .ok_or_else(|| bad("table without inputCount"))?;
                    let mut o = Obj::new();
                    o.insert(
                        k("inputPass"),
                        Val::str(&pass_bytes(rc, ic, rec.get("failedCells"))?),
                    );
                    Val::obj(o)
                }
            };
            let evaluations = match det.get("evaluations") {
                Some(e) => e.clone(),
                None if rec.has("evaluations") => {
                    let (list, col) = parse_evals(rec).ok_or_else(|| bad("bad evaluations"))?;
                    evals_in(&list, &col, writes)
                        .ok_or_else(|| bad("evaluations need the writes"))?
                }
                None => {
                    if rows.as_arr().is_some_and(|r| r.len() == 1) {
                        Val::arr(vec![Val::obj(writes_map(writes))])
                    } else {
                        Val::arr(Vec::new())
                    }
                }
            };
            let mut o = Obj::new();
            o.insert(k("evaluations"), evaluations);
            o.insert(k("extras"), extras);
            o.insert(k("kind"), kind.clone());
            o.insert(k("matchedRows"), rows);
            Ok(Some(Val::obj(o)))
        }
        _ => Ok(None),
    }
}

fn doc_for(
    sim: &mut Sim,
    block: &Val,
    policy: Option<&Val>,
    kind: &str,
    paths: &[Option<String>],
    rows: &[Val],
) -> usize {
    let guess = sim.doc_of(policy);
    let mut order = vec![guess];
    order.extend((0..sim.rel.len()).filter(|i| *i != guess));
    let mut first: Option<usize> = None;
    for doc in order {
        let Some(info) = sim.info(doc, block) else {
            continue;
        };
        if info.kind.as_deref() != Some(kind) {
            continue;
        }
        first.get_or_insert(doc);
        if let Some(p) = write_paths(&info, kind, rows) {
            if p.len() == paths.len()
                && p.iter()
                    .zip(paths)
                    .all(|(a, b)| b.as_deref() == Some(a.as_str()))
            {
                return doc;
            }
        }
    }
    first.unwrap_or(guess)
}

fn derived_reads(
    sim: &mut Sim,
    block: &Val,
    policy: Option<&Val>,
    trace: Option<&Val>,
    writes: Option<&Val>,
) -> Vec<String> {
    let Some(tr) = trace.filter(|t| t.is_obj()) else {
        return Vec::new();
    };
    let Some(kind) = tr
        .get("kind")
        .and_then(|k| k.as_str())
        .filter(|k| KINDS.contains(k))
    else {
        return Vec::new();
    };
    let paths: Vec<Option<String>> = match writes {
        Some(Val::Arr(a)) => a
            .iter()
            .map(|w| {
                w.get("path")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
            })
            .collect(),
        _ => Vec::new(),
    };
    let rows = tr
        .get("matchedRows")
        .and_then(|r| r.as_arr())
        .cloned()
        .unwrap_or_default();
    let doc = doc_for(sim, block, policy, kind, &paths, &rows);
    let info: Option<Rc<Info>> = sim.info(doc, block);
    match info {
        Some(info) => reads_of(&sim.models, &info, kind, tr),
        None => Vec::new(),
    }
}

fn reads_out(base: &[String], reads: &[String]) -> Option<Val> {
    if base == reads {
        return None;
    }
    let (add, del) = list_delta(base, reads);
    let mut fix = Obj::new();
    if !add.is_empty() {
        fix.insert(k("add"), strs(&add));
    }
    if !del.is_empty() {
        fix.insert(k("remove"), strs(&del));
    }
    let fixv = Val::obj(fix);
    if apply_delta(base, &add, &del) == reads && json_len(&fixv) < json_len(&strs(reads)) {
        return Some(fixv);
    }
    Some(strs(reads))
}

fn inst_of(e: &Obj) -> Option<String> {
    e.get("instancePath").map(js_string)
}

struct Recorded {
    rec: Obj,
    from: Option<HashMap<String, Alias>>,
    writes: Writes,
}

fn to_record(sim: &mut Sim, e: &Obj, derive: bool, def: Absent) -> R<Recorded> {
    let mut rec = Obj::new();
    let block = e.get("blockId").cloned();
    if let Some(b) = &block {
        rec.insert(k("block"), b.clone());
    }
    let inst_v = e.get("instancePath");
    if let Some(i) = inst_v {
        rec.insert(k("member"), i.clone());
    }
    let inst = inst_of(e);
    let inst = inst.as_deref();
    if let Some(p) = e.get("policyPath") {
        rec.insert(k("policy"), p.clone());
    }
    let mut extra = Obj::new();
    for (key, v) in e.iter() {
        if !EXEC_KEYS.contains(&&**key) {
            extra.insert(key.clone(), v.clone());
        }
    }
    let mut details = Obj::new();
    let mut reads: Vec<String> = Vec::new();
    mark_absent(&mut rec, "readsAbsent", !e.has("reads"), def.reads);
    match e.get("reads") {
        None => {}
        Some(r) => match str_list(r) {
            Some(list) => {
                reads = list;
                if derive {
                    let base = derived_reads(
                        sim,
                        block.as_ref().unwrap_or(&Val::Null),
                        e.get("policyPath"),
                        e.get("trace"),
                        e.get("writes"),
                    );
                    if let Some(x) = reads_out(&base, &reads) {
                        rec.insert(k("reads"), x);
                    }
                } else if !reads.is_empty() {
                    rec.insert(k("reads"), strs(&reads));
                }
            }
            None => details.insert(k("reads"), r.clone()),
        },
    }
    let good = ok_writes(e.get("writes"));
    let mut from = None;
    let writes = e.get("writes").map(writes_pairs).unwrap_or_default();
    mark_absent(&mut rec, "writesAbsent", !e.has("writes"), def.writes);
    match e.get("writes") {
        None => {}
        Some(w) if !good => details.insert(k("writes"), w.clone()),
        Some(_) => {
            from = aliases_of(sim, &reads, &writes, inst);
            let vals: Vec<Val> = writes
                .iter()
                .map(|(p, v)| enc_val(v, from.as_ref().and_then(|f| f.get(&js_string(p)))))
                .collect();
            if writes.len() == 1 {
                let mut o = Obj::new();
                o.insert(Rc::from(js_string(&writes[0].0).as_str()), vals[0].clone());
                rec.insert(k("writes"), Val::obj(o));
            } else {
                let list = writes
                    .iter()
                    .zip(vals)
                    .map(|((p, _), v)| {
                        let mut o = Obj::with_capacity(2);
                        o.insert(k("path"), p.clone());
                        o.insert(k("value"), v);
                        Val::obj(o)
                    })
                    .collect();
                rec.insert(k("writes"), Val::arr(list));
            }
        }
    }
    let block_s = template(block.as_ref());
    let t = trace_out(e, &block_s, if good { Some(&writes) } else { None })?;
    for (key, v) in t.rec.iter() {
        rec.insert(key.clone(), v.clone());
    }
    for (key, v) in t.details.iter() {
        details.insert(key.clone(), v.clone());
    }
    mark_absent(
        &mut rec,
        "operandsAbsent",
        !e.has("operandValues"),
        def.operands,
    );
    match e.get("operandValues") {
        None => {}
        Some(Val::Obj(ov)) => {
            let pk: Vec<String> = reads
                .iter()
                .filter(|r| sim.val(r, inst).is_some())
                .cloned()
                .collect();
            let ak: Vec<String> = ov.keys().map(|k| k.to_string()).collect();
            let ak_len = ak.len();
            let (add, del) = list_delta(&pk, &ak);
            if !add.is_empty() || !del.is_empty() {
                let mut o = Obj::new();
                if !add.is_empty() {
                    o.insert(k("add"), strs(&add));
                }
                if !del.is_empty() {
                    o.insert(k("remove"), strs(&del));
                }
                rec.insert(k("operandKeys"), Val::obj(o));
            }
            let mut lit: Option<Obj> = None;
            let mut post: Option<Vec<String>> = None;
            let mut after: Option<Val> = None;
            for (key, v) in ov.iter() {
                let key: &str = key;
                if sim.val(key, inst).is_some_and(|x| eq(v, &x)) {
                    continue;
                }
                if after.is_none() && good {
                    after = Some(sim.after(&writes, inst, from.as_ref()));
                }
                if after
                    .as_ref()
                    .is_some_and(|a| sim.val_in(a, key, inst).is_some_and(|x| eq(v, &x)))
                {
                    post.get_or_insert_with(Vec::new).push(key.to_string());
                } else {
                    lit.get_or_insert_with(Obj::new)
                        .insert(Rc::from(key), v.clone());
                }
            }
            if let Some(p) = post {
                rec.insert(
                    k("operandsAfterWrite"),
                    if p.len() == ak_len {
                        Val::Bool(true)
                    } else {
                        strs(&p)
                    },
                );
            }
            if let Some(l) = lit {
                rec.insert(k("storedOperands"), escape(&Val::obj(l)));
            }
        }
        Some(ov) => details.insert(k("operandValues"), ov.clone()),
    }
    if !extra.is_empty() {
        rec.insert(k("extraFields"), escape(&Val::obj(extra)));
    }
    if !details.is_empty() {
        rec.insert(k("details"), escape(&Val::obj(details)));
    }
    let out_writes = if good || e.get("writes").is_some_and(|w| w.is_arr()) {
        writes
    } else {
        Vec::new()
    };
    Ok(Recorded {
        rec,
        from,
        writes: out_writes,
    })
}

struct Restored {
    e: Obj,
    from: Option<HashMap<String, Alias>>,
    writes: Writes,
}

fn from_record(
    sim: &mut Sim,
    rec: &Obj,
    prev_reads: &mut HashMap<String, Vec<String>>,
    derive: bool,
    def: Absent,
) -> R<Restored> {
    let mut e = Obj::new();
    let block = rec.get("block").cloned();
    if let Some(b) = &block {
        e.insert(k("blockId"), b.clone());
    }
    let inst_v = rec.get("member").cloned();
    if let Some(i) = &inst_v {
        e.insert(k("instancePath"), i.clone());
    }
    let inst_s = inst_v.as_ref().map(js_string);
    let inst = inst_s.as_deref();
    if let Some(p) = rec.get("policy") {
        e.insert(k("policyPath"), p.clone());
    }
    let det = match rec.get("details") {
        Some(d) => unescape(d)
            .as_obj()
            .cloned()
            .ok_or_else(|| bad("details must be a map"))?,
        None => Obj::new(),
    };
    let rkey = format!(
        "{}|{}",
        rec.get("policy").map(js_string).unwrap_or_default(),
        template(block.as_ref())
    );
    let writes_val: Option<Val> = if absent_flag(rec, "writesAbsent", def.writes) {
        None
    } else if let Some(w) = det.get("writes") {
        Some(w.clone())
    } else {
        let list: Vec<Val> = match rec.get("writes") {
            Some(Val::Arr(a)) => a
                .iter()
                .map(|x| {
                    let mut o = Obj::with_capacity(2);
                    o.insert(k("path"), x.get("path").cloned().unwrap_or(Val::Null));
                    o.insert(
                        k("value"),
                        dec_val(sim, x.get("value").unwrap_or(&Val::Null))?,
                    );
                    Ok(Val::obj(o))
                })
                .collect::<R<_>>()?,
            Some(Val::Obj(o)) => o
                .iter()
                .map(|(p, v)| {
                    let mut w = Obj::with_capacity(2);
                    w.insert(k("path"), Val::Str(p.clone()));
                    w.insert(k("value"), dec_val(sim, v)?);
                    Ok(Val::obj(w))
                })
                .collect::<R<_>>()?,
            None => Vec::new(),
            Some(_) => return Err(bad("writes must be a map or a list")),
        };
        Some(Val::arr(list))
    };
    let good = ok_writes(writes_val.as_ref());
    let writes: Writes = writes_val.as_ref().map(writes_pairs).unwrap_or_default();
    let empty: Writes = Vec::new();
    let block_s = template(block.as_ref());
    let tr = trace_in(rec, &det, &block_s, if good { &writes } else { &empty })?;
    let mut reads: Vec<String> = Vec::new();
    let mut reads_kept: Option<Val> = None;
    if let Some(r) = det.get("reads") {
        reads_kept = Some(r.clone());
    } else if !absent_flag(rec, "readsAbsent", def.reads) {
        match rec.get("reads") {
            Some(Val::Str(s)) if &**s == "previous" => {
                reads = prev_reads
                    .get(&rkey)
                    .cloned()
                    .ok_or_else(|| bad("reads: previous without an earlier list"))?;
            }
            Some(r @ Val::Arr(_)) => {
                reads = str_list(r).ok_or_else(|| bad("reads must be strings"))?
            }
            r if derive => {
                let base = derived_reads(
                    sim,
                    block.as_ref().unwrap_or(&Val::Null),
                    rec.get("policy"),
                    tr.as_ref(),
                    writes_val.as_ref(),
                );
                reads = match r {
                    Some(Val::Obj(fix)) => {
                        let add = fix
                            .get("add")
                            .map(|x| str_list(x).ok_or_else(|| bad("reads add")))
                            .transpose()?
                            .unwrap_or_default();
                        let del = fix
                            .get("remove")
                            .map(|x| str_list(x).ok_or_else(|| bad("reads remove")))
                            .transpose()?
                            .unwrap_or_default();
                        apply_delta(&base, &add, &del)
                    }
                    _ => base,
                };
            }
            None => {}
            Some(_) => return Err(bad("policy reads correction without documents")),
        }
        prev_reads.insert(rkey, reads.clone());
        reads_kept = Some(strs(&reads));
    }
    let mut from = None;
    if good && !det.has("writes") {
        from = aliases_of(sim, &reads, &writes, inst);
    }
    if let Some(ov) = det.get("operandValues") {
        e.insert(k("operandValues"), ov.clone());
    } else if !absent_flag(rec, "operandsAbsent", def.operands) {
        let pk: Vec<String> = reads
            .iter()
            .filter(|r| sim.val(r, inst).is_some())
            .cloned()
            .collect();
        let ok = rec.get("operandKeys");
        let add = ok
            .and_then(|o| o.get("add"))
            .map(|x| str_list(x).ok_or_else(|| bad("operandKeys add")))
            .transpose()?
            .unwrap_or_default();
        let del = ok
            .and_then(|o| o.get("remove"))
            .map(|x| str_list(x).ok_or_else(|| bad("operandKeys remove")))
            .transpose()?
            .unwrap_or_default();
        let keys = apply_delta(&pk, &add, &del);
        let lit = match rec.get("storedOperands") {
            Some(s) => unescape(s)
                .as_obj()
                .cloned()
                .ok_or_else(|| bad("storedOperands must be a map"))?,
            None => Obj::new(),
        };
        let pj = rec.get("operandsAfterWrite");
        let all_after = matches!(pj, Some(Val::Bool(true)));
        let ps: HashSet<String> = match pj {
            Some(a @ Val::Arr(_)) => str_list(a).unwrap_or_default().into_iter().collect(),
            _ => HashSet::default(),
        };
        let mut ov = Obj::new();
        let mut after: Option<Val> = None;
        for key in &keys {
            if let Some(v) = lit.get(key) {
                ov.set(key, v.clone());
            } else if all_after || ps.contains(key) {
                let state = after.get_or_insert_with(|| {
                    sim.after(if good { &writes } else { &empty }, inst, from.as_ref())
                });
                if let Some(v) = sim.val_in(state, key, inst) {
                    ov.set(key, v);
                }
            } else if let Some(v) = sim.val(key, inst) {
                ov.set(key, v);
            }
        }
        e.insert(k("operandValues"), Val::obj(ov));
    }
    if let Some(r) = reads_kept {
        e.insert(k("reads"), r);
    }
    if let Some(t) = tr {
        e.insert(k("trace"), t);
    }
    if let Some(w) = &writes_val {
        e.insert(k("writes"), w.clone());
    }
    if let Some(x) = rec.get("extraFields") {
        if let Val::Obj(x) = unescape(x) {
            for (key, v) in x.iter() {
                e.set(key, v.clone());
            }
        }
    }
    let out_writes = if writes_val.as_ref().is_some_and(|w| w.is_arr()) {
        writes
    } else {
        Vec::new()
    };
    Ok(Restored {
        e,
        from,
        writes: out_writes,
    })
}

fn sweep_of(at: Option<&Val>) -> Option<(String, u64)> {
    let Some(Val::Str(s)) = at else { return None };
    let dot = s.rfind('.')?;
    let tail = &s[dot + 1..];
    if tail.is_empty() || !tail.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n = tail.parse::<f64>().ok()? as u64;
    Some((s[..dot].to_string(), n))
}

fn len_at(sim: &Sim, coll: &str) -> i64 {
    match get(&sim.state, &split(coll)) {
        Some(Val::Arr(a)) => a.len() as i64,
        _ => -1,
    }
}

fn same_head(a: &Obj, b: &Obj) -> bool {
    HEAD.iter().all(|h| match (a.get(h), b.get(h)) {
        (None, None) => true,
        (Some(x), Some(y)) => same_prim(x, y),
        _ => false,
    })
}

fn write_columns(rs: &[Obj]) -> Option<Val> {
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

fn build_run(first: &Obj, coll: &str, count: usize, len: i64, recs: Vec<Obj>) -> Obj {
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

fn is_run(rec: &Obj) -> bool {
    matches!(rec.get("members"), Some(Val::Str(_)))
}

const RUN_KEYS: [&str; 6] = ["block", "policy", "members", "count", "perMember", "writes"];

fn member_record(run: &Obj, idx: usize) -> R<Obj> {
    let mut rec = Obj::new();
    for h in HEAD {
        if let Some(v) = run.get(h) {
            rec.insert(k(h), v.clone());
        }
    }
    let members = run.get("members").and_then(|m| m.as_str()).unwrap_or("");
    rec.insert(k("member"), Val::str(&format!("{members}.{idx}")));
    for (key, v) in run.iter() {
        if RUN_KEYS.contains(&&**key) {
            continue;
        }
        if COLUMNS.contains(&&**key) {
            let x = v
                .as_arr()
                .and_then(|a| a.get(idx))
                .cloned()
                .unwrap_or(Val::Null);
            if !x.is_null() {
                rec.insert(key.clone(), x);
            }
        } else {
            rec.insert(key.clone(), v.clone());
        }
    }
    match run.get("writes") {
        Some(Val::Arr(cols)) => {
            let list = cols
                .iter()
                .map(|c| {
                    let mut o = Obj::with_capacity(2);
                    o.insert(k("path"), c.get("path").cloned().unwrap_or(Val::Null));
                    o.insert(
                        k("value"),
                        c.get("values")
                            .and_then(|v| v.as_arr())
                            .and_then(|v| v.get(idx))
                            .cloned()
                            .unwrap_or(Val::Null),
                    );
                    Val::obj(o)
                })
                .collect();
            rec.insert(k("writes"), Val::arr(list));
        }
        Some(Val::Obj(w)) => {
            let mut o = Obj::new();
            for (p, col) in w.iter() {
                o.insert(
                    p.clone(),
                    col.as_arr()
                        .and_then(|c| c.get(idx))
                        .cloned()
                        .ok_or_else(|| bad("writes column too short"))?,
                );
            }
            rec.insert(k("writes"), Val::obj(o));
        }
        _ => {}
    }
    if let Some(Val::Arr(pm)) = run.get("perMember") {
        if let Some(Val::Obj(m)) = pm.get(idx) {
            for (key, v) in m.iter() {
                rec.set(key, v.clone());
            }
        }
    }
    Ok(rec)
}

pub(crate) type PathFn<'a> = &'a dyn Fn(&str) -> R<String>;

fn map_str(v: &Val, p: PathFn) -> R<Val> {
    match v {
        Val::Str(s) => Ok(Val::str(&p(s)?)),
        _ => Err(bad("path must be a string")),
    }
}

fn map_keys(o: &Obj, p: PathFn) -> R<Obj> {
    let mut out = Obj::with_capacity(o.len());
    for (key, v) in o.iter() {
        out.insert(Rc::from(p(key)?.as_str()), v.clone());
    }
    Ok(out)
}

fn map_list(r: &Val, p: PathFn) -> R<Val> {
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

fn exec_paths(e: &Obj) -> Vec<Val> {
    let mut out = Vec::new();
    if let Some(i) = e.get("instancePath") {
        out.push(i.clone());
    }
    if let Some(Val::Arr(ws)) = e.get("writes") {
        for w in ws.iter() {
            out.push(w.get("path").cloned().unwrap_or(Val::Null));
        }
    }
    if let Some(Val::Arr(rs)) = e.get("reads") {
        out.extend(rs.iter().cloned());
    }
    if let Some(Val::Obj(ov)) = e.get("operandValues") {
        out.extend(ov.keys().map(|k| Val::Str(k.clone())));
    }
    if let Some(Val::Arr(evs)) = e.get("trace").and_then(|t| t.get("evaluations")) {
        for ev in evs.iter() {
            if let Val::Obj(o) = ev {
                out.extend(o.keys().map(|k| Val::Str(k.clone())));
            }
        }
    }
    out
}

fn rank(key: &str) -> usize {
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

fn previous_reads(recs: Vec<Obj>, def: Absent) -> Vec<Obj> {
    let mut last: HashMap<String, Vec<String>> = HashMap::default();
    recs.into_iter()
        .map(|r| {
            let key = format!("{}|{}", r.get("policy").map(js_string).unwrap_or_default(), template(r.get("block")));
            let own = r.get("reads").and_then(|x| if x.is_arr() { str_list(x) } else { None });
            let mine = own.clone().or_else(|| match r.get("perMember") {
                Some(Val::Arr(pm)) => pm.last().and_then(|m| m.get("reads")).and_then(str_list),
                _ => None,
            });
            let prev = last.get(&key).cloned();
            let reads_absent = absent_flag(&r, "readsAbsent", def.reads);
            if let Some(o) = &own {
                last.insert(key.clone(), o.clone());
            } else if is_run(&r) && !reads_absent {
                last.insert(key.clone(), mine.unwrap_or_default());
            } else if !reads_absent && !matches!(r.get("details"), Some(Val::Obj(d)) if d.has("reads") || d.has("execution")) {
                last.insert(key.clone(), Vec::new());
            }
            if let (Some(o), Some(p)) = (&own, &prev) {
                if o == p {
                    let mut r = r.clone();
                    r.set("reads", Val::str("previous"));
                    return r;
                }
            }
            r
        })
        .collect()
}

pub fn is_policy(trace: &Val) -> bool {
    matches!(trace.get("executions"), Some(Val::Arr(_)))
}

fn dotted_keys(docs: &[&Val]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::default();
    fn walk(v: &Val, out: &mut Vec<String>, seen: &mut HashSet<String>) {
        match v {
            Val::Arr(a) => a.iter().for_each(|x| walk(x, out, seen)),
            Val::Obj(o) => {
                for (key, x) in o.iter() {
                    if key.contains('.') && seen.insert(key.to_string()) {
                        out.push(key.to_string());
                    }
                    walk(x, out, seen);
                }
            }
            _ => {}
        }
    }
    for d in docs {
        walk(d, &mut out, &mut seen);
    }
    out
}

fn dot_safe(s: &Val, keys: &[String]) -> bool {
    let Val::Str(s) = s else { return false };
    keys.iter().all(|key| !s.contains(key.as_str()))
}

fn documents_of(docs: Option<&Val>) -> Option<Vec<Val>> {
    match docs {
        Some(Val::Arr(a)) if !a.is_empty() => Some(a.to_vec()),
        _ => None,
    }
}

pub fn encode_policy(trace: &Val, request: &Val, documents: Option<&Val>) -> R<Val> {
    let docs = documents_of(documents);
    let derive = docs.is_some();
    let mut sim = Sim::new(request, docs.unwrap_or_default());
    let execs = trace
        .get("executions")
        .and_then(|e| e.as_arr())
        .cloned()
        .unwrap_or_default();
    let mut written: Vec<Val> = Vec::new();
    for e in execs.iter() {
        if let Some(Val::Arr(ws)) = e.get("writes") {
            for w in ws.iter() {
                written.push(w.get("value").cloned().unwrap_or(Val::Null));
            }
        }
    }
    let wv = Val::arr(written);
    let dkeys = dotted_keys(&[request, &wv]);
    let def = Absent::of_execs(&execs);
    let mut items: Vec<Item> = Vec::with_capacity(execs.len());
    for e in execs.iter() {
        let safe = match e {
            Val::Obj(eo) => (dkeys.is_empty()
                || exec_paths(eo)
                    .iter()
                    .all(|p| p.as_str().is_none() || dot_safe(p, &dkeys)))
            .then_some(eo),
            _ => None,
        };
        let Some(eo) = safe else {
            let mut rec = Obj::new();
            if let Some(b) = e.get("blockId") {
                rec.insert(k("block"), b.clone());
            }
            let mut d = Obj::new();
            d.insert(k("execution"), escape(e));
            rec.insert(k("details"), Val::obj(d));
            items.push(Item { rec, len: -1 });
            if let Some(w @ Val::Arr(_)) = e.get("writes") {
                let inst = e.get("instancePath").map(js_string);
                sim.apply(&writes_pairs(w), inst.as_deref(), None);
            }
            continue;
        };
        let sw = sweep_of(eo.get("instancePath"));
        let len = match &sw {
            Some((coll, 0)) => len_at(&sim, coll),
            _ => -1,
        };
        let r = to_record(&mut sim, eo, derive, def)?;
        items.push(Item { rec: r.rec, len });
        let inst = inst_of(eo);
        sim.apply(&r.writes, inst.as_deref(), r.from.as_ref());
    }
    let recs = group_runs(items);
    let recs = if derive {
        recs
    } else {
        previous_reads(recs, def)
    };
    let to_ptr_fn = |s: &str| -> R<String> { Ok(dot_to_ptr(s)) };
    let executions = recs
        .iter()
        .map(|r| Ok(pack_record(&paths_out(r, &to_ptr_fn)?)))
        .collect::<R<Vec<_>>>()?;
    let mut body = Obj::new();
    if def.any() {
        body.insert(k("absent"), def.val());
    }
    body.insert(k("executions"), Val::arr(executions));
    let pred = props_of(&execs, request, &sim.state);
    let act = trace.get("properties");
    if !act.is_some_and(|a| eq(&Val::obj(pred.clone()), a)) {
        match act {
            None => {
                let mut o = Obj::new();
                o.insert(k("absent"), Val::Bool(true));
                body.insert(k("properties"), Val::obj(o));
            }
            Some(av @ Val::Obj(a)) => {
                let mut set = Obj::new();
                let mut del: Vec<String> = Vec::new();
                for (key, v) in a.iter() {
                    if !pred.get(key).is_some_and(|x| eq(x, v)) {
                        set.insert(key.clone(), v.clone());
                    }
                }
                for key in pred.keys() {
                    if !a.has(key) {
                        del.push(key.to_string());
                    }
                }
                let all_safe = set
                    .keys()
                    .map(|x| x.to_string())
                    .chain(del.iter().cloned())
                    .all(|x| dot_safe(&Val::str(&x), &dkeys));
                let mut props = Obj::new();
                if !all_safe {
                    props.insert(k("verbatim"), pack(&escape(av)));
                } else {
                    if !set.is_empty() {
                        props.insert(
                            k("set"),
                            pack(&escape(&Val::obj(map_keys(&set, &to_ptr_fn)?))),
                        );
                    }
                    if !del.is_empty() {
                        props.insert(
                            k("remove"),
                            Val::arr(del.iter().map(|d| Val::str(&dot_to_ptr(d))).collect()),
                        );
                    }
                }
                body.insert(k("properties"), Val::obj(props));
            }
            Some(other) => {
                let mut o = Obj::new();
                o.insert(k("verbatim"), pack(&escape(other)));
                body.insert(k("properties"), Val::obj(o));
            }
        }
    }
    let mut extra = Obj::new();
    if let Some(t) = trace.as_obj() {
        for (key, v) in t.iter() {
            if !TOP.contains(&&**key) {
                extra.insert(key.clone(), v.clone());
            }
        }
    }
    if !extra.is_empty() {
        body.insert(k("extraFields"), pack(&escape(&Val::obj(extra))));
    }
    let mut requires: Vec<&str> = Vec::with_capacity(2);
    if derive {
        requires.push("documents");
    }
    if def.any() {
        requires.push(ABSENT_DEFAULT);
    }
    Ok(Val::obj(envelope(
        "policy",
        trace.get("engineVersion"),
        body,
        &requires,
    )))
}

pub fn needs_documents(requires: &[String]) -> bool {
    requires.iter().any(|r| r == "documents")
}

pub fn decode_policy(
    body: &Obj,
    request: &Val,
    documents: Option<&Val>,
    engine: Option<&Val>,
    derive: bool,
) -> R<Val> {
    if let Some(v) = body.get("verbatim") {
        return Ok(unescape(v));
    }
    let docs = documents_of(documents);
    if derive && docs.is_none() {
        return Err(TraceError::DocumentsRequired);
    }
    let mut sim = Sim::new(
        request,
        if derive {
            docs.unwrap_or_default()
        } else {
            Vec::new()
        },
    );
    let def = Absent::of_body(body)?;
    let mut prev: HashMap<String, Vec<String>> = HashMap::default();
    let mut executions: Vec<Val> = Vec::new();
    let to_dot = |s: &str| -> R<String> { ptr_to_dot(s) };
    let list = body
        .get("executions")
        .and_then(|e| e.as_arr())
        .ok_or_else(|| bad("policy body needs executions"))?;
    let mut run_one = |sim: &mut Sim, rec: &Obj, executions: &mut Vec<Val>| -> R<()> {
        if let Some(Val::Obj(d)) = rec.get("details") {
            if let Some(x) = d.get("execution") {
                let e = unescape(x);
                if let Some(w @ Val::Arr(_)) = e.get("writes") {
                    let inst = e.get("instancePath").map(js_string);
                    sim.apply(&writes_pairs(w), inst.as_deref(), None);
                }
                executions.push(e);
                return Ok(());
            }
        }
        let r = from_record(sim, rec, &mut prev, derive, def)?;
        let inst = r.e.get("instancePath").map(js_string);
        sim.apply(&r.writes, inst.as_deref(), r.from.as_ref());
        executions.push(Val::obj(r.e));
        Ok(())
    };
    for rec0 in list.iter() {
        let rec = paths_out(
            rec0.as_obj()
                .ok_or_else(|| bad("execution record must be a map"))?,
            &to_dot,
        )?;
        if !is_run(&rec) {
            run_one(&mut sim, &rec, &mut executions)?;
            continue;
        }
        let n = match rec.get("count") {
            Some(c) => c
                .as_i64()
                .filter(|n| *n <= 10_000_000)
                .ok_or_else(|| bad("count must be a small integer"))?,
            None => len_at(
                &sim,
                rec.get("members")
                    .and_then(|m| m.as_str())
                    .need("a member run needs members")?,
            ),
        };
        for idx in 0..n.max(0) as usize {
            let m = member_record(&rec, idx)?;
            run_one(&mut sim, &m, &mut executions)?;
        }
    }
    let mut t = Obj::new();
    if let Some(e) = engine {
        t.insert(k("engineVersion"), e.clone());
    }
    let props = body.get("properties");
    let verbatim = props.and_then(|p| p.get("verbatim"));
    let absent = props.is_some_and(
        |p| matches!(p.get("absent"), Some(v) if !matches!(v, Val::Null | Val::Bool(false))),
    );
    match (verbatim, props) {
        (Some(v), _) => t.insert(k("properties"), unescape(v)),
        _ if absent => {}
        (None, p) => {
            let mut pred = props_of(&executions, request, &sim.state);
            if let Some(p) = p {
                if let Some(Val::Arr(rm)) = p.get("remove") {
                    for x in rm.iter() {
                        pred.remove(&ptr_to_dot(x.as_str().ok_or_else(|| bad("remove entry"))?)?);
                    }
                }
                if let Some(s) = p.get("set") {
                    if let Val::Obj(s) = unescape(s) {
                        for (key, v) in s.iter() {
                            pred.set(&ptr_to_dot(key)?, v.clone());
                        }
                    }
                }
            }
            t.insert(k("properties"), Val::obj(pred));
        }
    }
    t.insert(k("executions"), Val::arr(executions));
    if let Some(x) = body.get("extraFields") {
        if let Val::Obj(x) = unescape(x) {
            for (key, v) in x.iter() {
                t.set(key, v.clone());
            }
        }
    }
    Ok(Val::obj(t))
}

pub fn policy_fallback(trace: &Val) -> Val {
    let mut body = Obj::new();
    body.insert(k("verbatim"), pack(&escape(trace)));
    Val::obj(envelope("policy", trace.get("engineVersion"), body, &[]))
}
