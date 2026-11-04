use super::core::{
    apply_bag, build_bag, deep_merge, diff, dotted, escape, from_ptr, get_opt, get_path, is_bag,
    lit, map_bag_values, perf_in, perf_out, place_at, size_cap, to_ptr, unescape, writes, Edit,
    Refs, Seg, REF_MIN,
};
use super::loops::{
    deep_or_flat, is_verbatim, loop_in, nest, nodes_val, sub_in, tidy_record, EncRec, EncTrace,
    NestKind, Shapes,
};
use super::value::{eq, json_len, utf16_len, Key, Obj, Val};
use super::{bad, R};
use rust_decimal::Decimal;
use std::rc::Rc;

const META: [&str; 7] = [
    "id",
    "input",
    "name",
    "order",
    "output",
    "performance",
    "traceData",
];
const MERGE_CAP: usize = 24;

pub fn well_formed(t: &Val) -> bool {
    let Val::Obj(o) = t else { return false };
    o.values().all(|e| match e {
        Val::Obj(e) => {
            e.len() == META.len()
                && META.iter().all(|m| e.has(m))
                && matches!(e.get("order"), Some(Val::Num(_)))
        }
        _ => false,
    })
}

pub(crate) fn is_graph_trace(v: &Val) -> bool {
    matches!(v, Val::Obj(o) if !o.is_empty()) && well_formed(v)
}

fn order_of(e: &Val) -> Decimal {
    e.get("order").and_then(|x| x.as_num()).unwrap_or_default()
}

pub fn ordered_keys(t: &Obj) -> Vec<Key> {
    let mut keys: Vec<(Key, Decimal)> = t.iter().map(|(k, v)| (k.clone(), order_of(v))).collect();
    keys.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.0.encode_utf16().cmp(b.0.encode_utf16()))
    });
    keys.into_iter().map(|(k, _)| k).collect()
}

fn js_falsy(v: &Val) -> bool {
    match v {
        Val::Null => true,
        Val::Bool(b) => !b,
        Val::Num(n) => n.is_zero(),
        Val::Str(s) => s.is_empty(),
        _ => false,
    }
}

struct Level {
    ids: Vec<Key>,
    outputs: Vec<(Key, Val)>,
    open: Vec<Key>,
}

impl Level {
    fn new() -> Self {
        Self {
            ids: Vec::new(),
            outputs: Vec::new(),
            open: Vec::new(),
        }
    }

    fn output(&self, id: &str) -> Option<&Val> {
        self.outputs
            .iter()
            .rev()
            .find(|(k, _)| &**k == id)
            .map(|(_, v)| v)
    }

    fn merge(&self, ids: &[Key]) -> R<Option<Val>> {
        let mut v: Option<Val> = None;
        for id in ids {
            let o = self
                .output(id)
                .ok_or_else(|| bad(format!("$merge: unknown node {id}")))?;
            v = Some(match v {
                None => o.clone(),
                Some(cur) => deep_merge(&cur, o),
            });
        }
        Ok(v)
    }

    fn add(&mut self, id: Key, output: Val, parents: &[Key]) {
        self.open.retain(|x| !parents.iter().any(|p| p == x));
        self.open.push(id.clone());
        self.ids.push(id.clone());
        match self.outputs.iter().position(|(k, _)| *k == id) {
            Some(i) => self.outputs[i].1 = output,
            None => self.outputs.push((id, output)),
        }
    }
}

struct Base {
    tag: Obj,
    doc: Option<Val>,
    penalty: usize,
}

fn tag_merge(list: &[Key]) -> Obj {
    let mut o = Obj::with_capacity(1);
    o.insert(
        Rc::from("$merge"),
        Val::arr(list.iter().map(|k| Val::Str(k.clone())).collect()),
    );
    o
}

fn tag_from(label: &str) -> Obj {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("$from"), Val::str(label));
    o
}

fn default_input(level: &Level, pos: usize, dflt: &[Key], req: &Val) -> R<Option<Val>> {
    if pos == 0 {
        return Ok(Some(Val::Null));
    }
    if dflt.is_empty() {
        return Ok(Some(req.clone()));
    }
    level.merge(dflt)
}

fn input_bases(dflt_doc: Option<Val>, req: &Val, extra: Vec<Base>) -> Vec<Base> {
    let mut bases = Vec::with_capacity(extra.len() + 3);
    bases.push(Base {
        tag: Obj::new(),
        doc: dflt_doc,
        penalty: 0,
    });
    bases.extend(extra);
    bases.push(Base {
        tag: tag_from("request"),
        doc: Some(req.clone()),
        penalty: 4,
    });
    bases.push(Base {
        tag: tag_from("null"),
        doc: Some(Val::Null),
        penalty: 0,
    });
    bases
}

fn deep_sub(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Obj(x), Val::Obj(y)) => x
            .iter()
            .all(|(k, v)| y.get(k).is_some_and(|w| deep_sub(v, w))),
        _ => eq(a, b),
    }
}

fn top_keys_in(o: &Val, i: &Obj) -> bool {
    o.as_obj().is_some_and(|o| o.keys().all(|k| i.has(k)))
}

fn same_list(a: &[Key], b: &[Key]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

fn discover_parents(input: &Val, level: &Level, dflt: &[Key]) -> Vec<Vec<Key>> {
    let mut found = Vec::new();
    let ids = &level.ids;
    if ids.is_empty() {
        return found;
    }
    let hit = |list: &[Key]| -> bool {
        match level.merge(list) {
            Ok(Some(v)) => eq(&v, input),
            _ => false,
        }
    };
    for i in (0..ids.len()).rev() {
        if same_list(std::slice::from_ref(&ids[i]), dflt) {
            continue;
        }
        if level.output(&ids[i]).is_some_and(|o| eq(o, input)) {
            return vec![vec![ids[i].clone()]];
        }
    }
    let Val::Obj(iobj) = input else { return found };
    for t in 0..2 {
        let c: Vec<Key> = ids
            .iter()
            .filter(|id| {
                let o = level.output(id).unwrap();
                o.is_obj()
                    && if t == 0 {
                        deep_sub(o, input)
                    } else {
                        top_keys_in(o, iobj)
                    }
            })
            .cloned()
            .collect();
        if c.len() < 2 || c.len() > MERGE_CAP || !hit(&c) {
            continue;
        }
        let mut list = c;
        let mut i = 0;
        while i < list.len() && list.len() > 1 {
            let next: Vec<Key> = list
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, k)| k.clone())
                .collect();
            if hit(&next) {
                list = next;
            } else {
                i += 1;
            }
        }
        if !same_list(&list, dflt) {
            found.push(list);
        }
        return found;
    }
    found
}

struct Opt {
    enc: Val,
    base: Option<Val>,
    size: usize,
}

fn encode_value(target: &Val, bases: &[Base], refs: &mut Refs) -> R<Option<Val>> {
    if let Some(d) = &bases[0].doc {
        if eq(d, target) {
            return Ok(None);
        }
    }
    let mut options: Vec<Opt> = Vec::new();
    let mut cap = usize::MAX;
    let consider =
        |options: &mut Vec<Opt>, cap: &mut usize, enc: Val, base: Option<Val>, penalty: usize| {
            if base.as_ref().map(js_falsy).unwrap_or(true) {
                *cap = options.iter().map(|o| o.size).min().unwrap_or(usize::MAX);
            }
            let size = size_cap(&enc, *cap).saturating_add(penalty);
            if size > *cap {
                return;
            }
            options.push(Opt { enc, base, size });
        };
    for b in bases {
        let Some(base) = &b.doc else { continue };
        if base.is_null() {
            continue;
        }
        let mut w: Vec<Edit> = Vec::new();
        writes(Some(base), target, &mut Vec::new(), &mut w);
        let from_writes = build_bag(w, base, target);
        let has_set = from_writes.has("$set");
        let mut variants = vec![from_writes];
        if has_set {
            variants.push(build_bag(diff(base, target), base, target));
        }
        for bag in variants {
            let mut enc = b.tag.clone();
            for (k, v) in bag.iter() {
                enc.insert(k.clone(), v.clone());
            }
            consider(
                &mut options,
                &mut cap,
                Val::obj(enc),
                Some(base.clone()),
                b.penalty,
            );
        }
        if options.iter().any(|o| o.size < 48) {
            break;
        }
    }
    for b in bases {
        if let Some(d) = &b.doc {
            if !b.tag.is_empty() && eq(d, target) {
                let t = Val::obj(b.tag.clone());
                let size = json_len(&t) + b.penalty;
                options.push(Opt {
                    enc: t,
                    base: Some(d.clone()),
                    size,
                });
            }
        }
    }
    let mut ref_lit: Option<Val> = None;
    if target.is_container() && size_cap(target, 4096) > 4096 {
        let r = refs.encode(target);
        let size = json_len(&r);
        ref_lit = Some(r.clone());
        options.push(Opt {
            enc: r,
            base: None,
            size,
        });
    } else {
        consider(&mut options, &mut cap, lit(target), None, 0);
    }
    options.sort_by_key(|o| o.size);
    for o in &options {
        let is_lit = o.base.as_ref().map(js_falsy).unwrap_or(true);
        if is_lit {
            return Ok(Some(match ref_lit {
                Some(r) => r,
                None => refs.encode(target),
            }));
        }
        let bag = o.enc.as_obj().unwrap();
        if eq(&apply_bag(o.base.as_ref().unwrap(), bag)?, target) {
            let mapped = map_bag_values(bag, &mut |v| Ok(refs.encode(v)))?;
            return Ok(Some(Val::obj(mapped)));
        }
    }
    Ok(Some(match ref_lit {
        Some(r) => r,
        None => refs.encode(target),
    }))
}

fn decode_value(
    enc: Option<&Val>,
    base_of: impl FnOnce(&Obj) -> R<Option<Val>>,
    refs: &Refs,
) -> R<Val> {
    let Some(enc) = enc else {
        return Ok(base_of(&Obj::new())?.unwrap_or(Val::Null));
    };
    if !is_bag(enc) {
        return refs.decode(enc);
    }
    let bag = enc.as_obj().unwrap();
    let base = base_of(bag)?.ok_or_else(|| bad("missing base"))?;
    let mapped = map_bag_values(bag, &mut |v| refs.decode(v))?;
    apply_bag(&base, &mapped)
}

fn is_result_entry(v: &Val) -> bool {
    matches!(v, Val::Obj(o) if o.len() == 1 && o.has("result"))
}

fn is_empty_entry(v: &Val) -> bool {
    matches!(v, Val::Obj(o) if o.is_empty())
}

enum Results {
    One(Obj, Option<Key>),
    Many(Vec<Obj>),
}

fn results_of(td: &Val, failing: bool) -> Option<Results> {
    let one = |m: &Val, allow_fail: bool| -> Option<(Obj, Option<Key>)> {
        let Val::Obj(m) = m else { return None };
        let mut results = Obj::with_capacity(m.len());
        let mut failed_key: Option<Key> = None;
        for (k, v) in m.iter() {
            if is_result_entry(v) {
                results.insert(k.clone(), v.get("result").unwrap().clone());
            } else if allow_fail && failed_key.is_none() && is_empty_entry(v) {
                failed_key = Some(k.clone());
            } else {
                return None;
            }
        }
        Some((results, failed_key))
    };
    match td {
        Val::Arr(a) => {
            if a.is_empty() {
                return None;
            }
            let mut items = Vec::with_capacity(a.len());
            for m in a.iter() {
                items.push(one(m, false)?.0);
            }
            Some(Results::Many(items))
        }
        _ => one(td, failing).map(|(r, f)| Results::One(r, f)),
    }
}

fn results_td(failed_key: Option<&Val>, results: &ResultsVal) -> Val {
    let wrap = |m: &Obj| -> Obj {
        let mut o = Obj::with_capacity(m.len());
        for (k, v) in m.iter() {
            let mut e = Obj::with_capacity(1);
            e.insert(Rc::from("result"), v.clone());
            o.insert(k.clone(), Val::obj(e));
        }
        o
    };
    match results {
        ResultsVal::Many(items) => Val::arr(items.iter().map(|m| Val::obj(wrap(m))).collect()),
        ResultsVal::One(m) => {
            let mut td = wrap(m);
            if let Some(Val::Str(k)) = failed_key {
                td.insert(k.clone(), Val::empty_obj());
            }
            Val::obj(td)
        }
    }
}

enum ResultsVal {
    One(Obj),
    Many(Vec<Obj>),
}

fn build_results(m: &Obj) -> Val {
    let mut o = Val::empty_obj();
    let mut keys: Vec<&Key> = m.keys().collect();
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    for k in keys {
        o = deep_merge(&o, &place_at(&dotted(k), m.get(k).unwrap().clone()));
    }
    o
}

struct RBase {
    label: &'static str,
    doc: Val,
}

fn results_bases(
    input: &Val,
    results: &ResultsVal,
    at: Option<&str>,
    items: Option<&str>,
) -> R<Vec<RBase>> {
    let segs = match at {
        Some(a) => from_ptr(a)?,
        None => Vec::new(),
    };
    let place = |x: Val| {
        if segs.is_empty() {
            x
        } else {
            place_at(&segs, x)
        }
    };
    let (r, many) = match results {
        ResultsVal::One(m) => (build_results(m), None),
        ResultsVal::Many(ms) => {
            let rs: Vec<Val> = ms.iter().map(build_results).collect();
            (Val::arr(rs.clone()), Some(rs))
        }
    };
    let placed = place(r);
    let mut out = vec![
        RBase {
            label: "results",
            doc: placed.clone(),
        },
        RBase {
            label: "input+results",
            doc: deep_merge(input, &placed),
        },
    ];
    if let (Some(rs), Some(items)) = (many, items) {
        if let Some(Val::Arr(arr)) = get_path(input, &from_ptr(items)?) {
            let merged: Vec<Val> = rs
                .iter()
                .enumerate()
                .map(|(i, x)| match arr.get(i) {
                    Some(a) => deep_merge(a, x),
                    None => x.clone(),
                })
                .collect();
            let pm = place(Val::arr(merged));
            out.push(RBase {
                label: "items+results",
                doc: pm.clone(),
            });
            out.push(RBase {
                label: "input+items+results",
                doc: deep_merge(input, &pm),
            });
        }
    }
    Ok(out)
}

fn walk_paths(v: &Val, visit: &mut dyn FnMut(&Val, &[Seg]) -> bool, max_depth: usize) {
    let mut budget: i64 = 200000;
    let mut path: Vec<Seg> = Vec::new();
    fn go(
        x: &Val,
        d: usize,
        max_depth: usize,
        budget: &mut i64,
        path: &mut Vec<Seg>,
        visit: &mut dyn FnMut(&Val, &[Seg]) -> bool,
    ) {
        let b = *budget;
        *budget -= 1;
        if b <= 0 {
            return;
        }
        if !x.is_container() {
            return;
        }
        if !visit(x, path) {
            return;
        }
        if d >= max_depth {
            return;
        }
        match x {
            Val::Arr(a) => {
                for (i, e) in a.iter().enumerate() {
                    path.push(Seg::K(Rc::from(i.to_string().as_str())));
                    go(e, d + 1, max_depth, budget, path, visit);
                    path.pop();
                }
            }
            Val::Obj(o) => {
                for (k, e) in o.iter() {
                    path.push(Seg::K(k.clone()));
                    go(e, d + 1, max_depth, budget, path, visit);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    go(v, 0, max_depth, &mut budget, &mut path, visit);
}

fn placement_candidates(output: &Val, results: &ResultsVal) -> Vec<String> {
    let keys: Vec<String> = match results {
        ResultsVal::One(m) => m
            .keys()
            .map(|k| {
                k.split('.')
                    .next()
                    .filter(|_| !k.is_empty())
                    .unwrap_or("undefined")
                    .to_string()
            })
            .collect(),
        ResultsVal::Many(_) => Vec::new(),
    };
    let n = match results {
        ResultsVal::Many(m) => Some(m.len()),
        _ => None,
    };
    let mut out = Vec::new();
    walk_paths(
        output,
        &mut |x, path| {
            if path.is_empty() {
                return true;
            }
            let hit = match n {
                Some(n) => matches!(x, Val::Arr(a) if a.len() == n),
                None => {
                    matches!(x, Val::Obj(o) if !keys.is_empty() && keys.iter().all(|k| o.has(k)))
                }
            };
            if hit {
                out.push(to_ptr(path));
                if out.len() >= 8 {
                    return false;
                }
            }
            !x.is_arr()
        },
        5,
    );
    out
}

fn item_candidates(input: &Val, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    walk_paths(
        input,
        &mut |x, path| {
            if let Val::Arr(a) = x {
                if a.len() == n {
                    out.push(to_ptr(path));
                }
                return false;
            }
            out.len() < 8
        },
        6,
    );
    out
}

struct Choice {
    at: Option<String>,
    items: Option<String>,
}

fn choose_results(input: &Val, output: &Val, results: &ResultsVal) -> R<Choice> {
    let try_at = |at: Option<&str>, items: Option<&str>| -> R<bool> {
        for b in results_bases(input, results, at, items)? {
            if eq(&b.doc, output) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let items_list = match results {
        ResultsVal::Many(m) => item_candidates(input, m.len()),
        _ => Vec::new(),
    };
    if try_at(None, None)? {
        return Ok(Choice {
            at: None,
            items: None,
        });
    }
    for it in &items_list {
        if try_at(None, Some(it))? {
            return Ok(Choice {
                at: None,
                items: Some(it.clone()),
            });
        }
    }
    for at in placement_candidates(output, results) {
        if try_at(Some(&at), None)? {
            return Ok(Choice {
                at: Some(at),
                items: None,
            });
        }
        for it in &items_list {
            if try_at(Some(&at), Some(it))? {
                return Ok(Choice {
                    at: Some(at.clone()),
                    items: Some(it.clone()),
                });
            }
        }
    }
    Ok(Choice {
        at: None,
        items: None,
    })
}

pub(crate) fn cell_field(k: &str) -> Option<&str> {
    if !k.ends_with(']') {
        return None;
    }
    let i = k.rfind('[')?;
    let inner = &k[i + 1..k.len() - 1];
    if inner.contains(']') {
        return None;
    }
    Some(&k[..i])
}

fn cell_fields(rule: &Obj) -> Option<Vec<String>> {
    let mut fs = Vec::new();
    for k in rule.keys() {
        if &**k == "_id" {
            continue;
        }
        fs.push(cell_field(k)?.to_string());
    }
    Some(fs)
}

pub fn plain_path(f: &str) -> bool {
    f.split('.').all(|s| {
        let b = s.as_bytes();
        !b.is_empty()
            && (b[0].is_ascii_alphabetic() || b[0] == b'_')
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
    })
}

fn derive_refs(fields: Option<&[String]>, scope: Option<&Val>) -> Obj {
    let mut m = Obj::new();
    let Some(fields) = fields else { return m };
    for f in fields {
        if f.is_empty() {
            continue;
        }
        match get_opt(scope, &dotted(f)) {
            Some(v) => m.set(f, v.clone()),
            None if plain_path(f) => m.set(f, Val::Null),
            None => {}
        }
    }
    m
}

fn is_entry(e: &Val) -> bool {
    matches!(e, Val::Obj(o) if o.len() == 3 && matches!(o.get("rule"), Some(Val::Obj(_))) && matches!(o.get("reference_map"), Some(Val::Obj(_))) && matches!(o.get("index"), Some(Val::Num(_))))
}

fn verbatim_entry(e: &Val) -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("verbatim"), escape(e));
    Val::obj(o)
}

fn entry_out(e: &Val, scope: Option<&Val>) -> Val {
    if !is_entry(e) {
        return verbatim_entry(e);
    }
    let rule = e.get("rule").unwrap().as_obj().unwrap();
    let mut o = Obj::new();
    match rule.get("_id") {
        Some(Val::Str(id)) => {
            o.insert(Rc::from("id"), Val::Str(id.clone()));
            let mut cells = rule.clone();
            cells.remove("_id");
            o.insert(Rc::from("cells"), escape(&Val::obj(cells)));
        }
        _ => o.insert(Rc::from("cells"), escape(e.get("rule").unwrap())),
    }
    o.insert(Rc::from("index"), e.get("index").unwrap().clone());
    let fields = cell_fields(rule);
    let d = derive_refs(fields.as_deref(), scope);
    let a = e.get("reference_map").unwrap().as_obj().unwrap();
    let mut refs = Obj::new();
    for (k, v) in a.iter() {
        if !d.get(k).is_some_and(|x| eq(x, v)) {
            refs.insert(k.clone(), v.clone());
        }
    }
    let drop: Vec<Val> = d
        .keys()
        .filter(|k| !a.has(k))
        .map(|k| Val::Str(k.clone()))
        .collect();
    if !refs.is_empty() {
        o.insert(Rc::from("references"), escape(&Val::obj(refs)));
    }
    if !drop.is_empty() {
        o.insert(Rc::from("referencesAbsent"), Val::arr(drop));
    }
    Val::obj(o)
}

fn entry_in(x: &Val, scope: Option<&Val>) -> R<Val> {
    if let Some(v) = x.get("verbatim") {
        return Ok(unescape(v));
    }
    let rule = match x.get("cells") {
        Some(c) => {
            let cells = unescape(c);
            match x.get("id") {
                Some(id) => {
                    let mut r = Obj::new();
                    r.insert(Rc::from("_id"), id.clone());
                    for (k, v) in cells
                        .as_obj()
                        .ok_or_else(|| bad("cells must be a map"))?
                        .iter()
                    {
                        r.insert(k.clone(), v.clone());
                    }
                    Val::obj(r)
                }
                None => cells,
            }
        }
        None => return Err(bad("rule cells need the release")),
    };
    let fields = rule.as_obj().and_then(cell_fields);
    let mut d = derive_refs(fields.as_deref(), scope);
    if let Some(Val::Arr(abs)) = x.get("referencesAbsent") {
        for k in abs.iter() {
            if let Some(k) = k.as_str() {
                d.remove(k);
            }
        }
    }
    if let Some(r) = x.get("references") {
        if let Val::Obj(r) = unescape(r) {
            for (k, v) in r.iter() {
                d.set(k, v.clone());
            }
        }
    }
    let mut o = Obj::with_capacity(3);
    o.insert(
        Rc::from("index"),
        x.get("index").cloned().unwrap_or(Val::Null),
    );
    o.insert(Rc::from("reference_map"), Val::obj(d));
    o.insert(Rc::from("rule"), rule);
    Ok(Val::obj(o))
}

fn refs_match(e: &Val, scope: Option<&Val>) -> bool {
    let rule = e.get("rule").unwrap().as_obj().unwrap();
    let d = derive_refs(cell_fields(rule).as_deref(), scope);
    eq(&Val::obj(d), e.get("reference_map").unwrap())
}

fn loop_entries(td: &[Val], items: Option<&Vec<Val>>) -> Vec<Val> {
    let mut cur: i64 = -1;
    let at = |j: i64| items.and_then(|a| if j >= 0 { a.get(j as usize) } else { None });
    td.iter()
        .map(|e| {
            let mut j = cur + 1;
            if is_entry(e) && !refs_match(e, at(j)) {
                if let Some(k) = items.and_then(|a| a.iter().position(|x| refs_match(e, Some(x)))) {
                    j = k as i64;
                }
            }
            let mut o = entry_out(e, at(j));
            if j != cur + 1 && !o.has("verbatim") {
                if let Val::Obj(r) = &mut o {
                    Rc::make_mut(r).insert(Rc::from("item"), Val::int(j));
                }
            }
            cur = j;
            o
        })
        .collect()
}

fn scope_candidates(input: &Val, entries: &[&Val]) -> (Vec<String>, Vec<String>) {
    let mut probes: Vec<(Vec<Seg>, Val)> = Vec::new();
    for e in entries {
        if !is_entry(e) {
            continue;
        }
        for (f, v) in e.get("reference_map").unwrap().as_obj().unwrap().iter() {
            if !v.is_null() && plain_path(f) {
                probes.push((dotted(f), v.clone()));
                break;
            }
        }
        if probes.len() >= 2 {
            break;
        }
    }
    let mut objs = Vec::new();
    let mut arrs = Vec::new();
    if probes.is_empty() {
        return (objs, arrs);
    }
    let (f, v) = &probes[0];
    walk_paths(
        input,
        &mut |x, path| {
            if let Val::Arr(a) = x {
                if arrs.len() < 6 && a.iter().any(|it| get_path(it, f).is_some_and(|g| eq(g, v))) {
                    arrs.push(to_ptr(path));
                }
                return objs.len() < 6 && a.len() <= 64;
            }
            if !path.is_empty() && objs.len() < 6 && get_path(x, f).is_some_and(|g| eq(g, v)) {
                objs.push(to_ptr(path));
            }
            true
        },
        6,
    );
    (objs, arrs)
}

struct TableEnc {
    field: &'static str,
    value: Val,
    scope: Option<String>,
    items: Option<String>,
}

fn encode_table(td: &Val, input: &Val) -> R<TableEnc> {
    let cost = |v: &Val| json_len(v);
    if is_entry(td) {
        let mut best = TableEnc {
            field: "rule",
            value: entry_out(td, Some(input)),
            scope: None,
            items: None,
        };
        if !refs_match(td, Some(input)) {
            for p in scope_candidates(input, &[td]).0 {
                let v = entry_out(td, get_path(input, &from_ptr(&p)?));
                if cost(&v) + utf16_len(&p) + 10 < cost(&best.value) {
                    best = TableEnc {
                        field: "rule",
                        value: v,
                        scope: Some(p),
                        items: None,
                    };
                }
            }
        }
        return Ok(best);
    }
    let a = td.as_arr().ok_or_else(|| bad("table trace"))?;
    if a.iter()
        .all(|x| matches!(x, Val::Arr(l) if l.iter().all(is_entry)))
    {
        let flat: Vec<&Val> = a.iter().flat_map(|x| x.as_arr().unwrap().iter()).collect();
        let try_items = |p: Option<&String>| -> R<TableEnc> {
            let items = match p {
                Some(p) => get_path(input, &from_ptr(p)?),
                None => None,
            };
            let value = Val::arr(
                a.iter()
                    .enumerate()
                    .map(|(j, arr)| {
                        let it = items
                            .and_then(|i| i.as_arr())
                            .and_then(|i| i.get(j))
                            .or_else(|| match items {
                                Some(Val::Obj(o)) => o.get(&j.to_string()),
                                _ => None,
                            });
                        Val::arr(
                            arr.as_arr()
                                .unwrap()
                                .iter()
                                .map(|e| entry_out(e, it))
                                .collect(),
                        )
                    })
                    .collect(),
            );
            Ok(TableEnc {
                field: "rulesPerItem",
                value,
                scope: None,
                items: p.cloned(),
            })
        };
        let mut best = try_items(None)?;
        for p in scope_candidates(input, &flat).1 {
            let v = try_items(Some(&p))?;
            if cost(&v.value) < cost(&best.value) {
                best = v;
            }
        }
        return Ok(best);
    }
    let all_match = a.iter().all(|e| refs_match(e, Some(input)));
    let mut best = TableEnc {
        field: "rules",
        value: Val::arr(a.iter().map(|e| entry_out(e, Some(input))).collect()),
        scope: None,
        items: None,
    };
    if all_match {
        return Ok(best);
    }
    let refs: Vec<&Val> = a.iter().collect();
    let (objs, arrs) = scope_candidates(input, &refs);
    for p in objs {
        let s = get_path(input, &from_ptr(&p)?);
        let v = Val::arr(a.iter().map(|e| entry_out(e, s)).collect());
        if cost(&v) + utf16_len(&p) + 10 < cost(&best.value) {
            best = TableEnc {
                field: "rules",
                value: v,
                scope: Some(p),
                items: None,
            };
        }
    }
    for p in arrs {
        let items = get_path(input, &from_ptr(&p)?).and_then(|x| x.as_arr());
        let v = Val::arr(loop_entries(a, items));
        if cost(&v) + utf16_len(&p) + 10 < cost(&best.value) {
            best = TableEnc {
                field: "rules",
                value: v,
                scope: None,
                items: Some(p),
            };
        }
    }
    Ok(best)
}

fn index_in(items: Option<&Val>, j: usize) -> Option<&Val> {
    match items {
        Some(Val::Arr(a)) => a.get(j),
        _ => None,
    }
}

struct RecView<'a> {
    slots: [Option<&'a Val>; 22],
}

fn rec_slot(k: &str) -> usize {
    match k {
        "id" => 0,
        "name" => 1,
        "type" => 2,
        "perf" => 3,
        "failed" => 4,
        "error" => 5,
        "input" => 6,
        "output" => 7,
        "results" => 8,
        "failedKey" => 9,
        "resultsAt" => 10,
        "scope" => 11,
        "items" => 12,
        "rule" => 13,
        "rules" => 14,
        "rulesPerItem" => 15,
        "branches" => 16,
        "log" => 17,
        "details" => 18,
        "sub" => 19,
        "loop" => 20,
        _ => 21,
    }
}

impl<'a> RecView<'a> {
    fn new(rec: &'a Obj) -> Self {
        let mut slots = [None; 22];
        for (k, v) in rec.iter() {
            let i = rec_slot(k);
            if i < 21 {
                slots[i] = Some(v);
            }
        }
        Self { slots }
    }

    fn get(&self, k: &str) -> Option<&'a Val> {
        self.slots[rec_slot(k)]
    }

    fn has(&self, k: &str) -> bool {
        self.get(k).is_some()
    }
}

fn decode_table(rec: &RecView, input: &Val) -> R<Val> {
    let scope_of = || -> R<Option<&Val>> {
        match rec.get("scope") {
            Some(s) => Ok(get_path(
                input,
                &from_ptr(s.as_str().ok_or_else(|| bad("scope must be a pointer"))?)?,
            )),
            None => Ok(Some(input)),
        }
    };
    let items = match rec.get("items") {
        Some(p) => get_path(
            input,
            &from_ptr(p.as_str().ok_or_else(|| bad("items must be a pointer"))?)?,
        ),
        None => None,
    };
    if let Some(r) = rec.get("rule") {
        if r.is_null() {
            return Ok(Val::Null);
        }
        return entry_in(r, scope_of()?);
    }
    if let Some(rpi) = rec.get("rulesPerItem") {
        let rpi = rpi
            .as_arr()
            .ok_or_else(|| bad("rulesPerItem must be a list"))?;
        let mut out = Vec::with_capacity(rpi.len());
        for (j, arr) in rpi.iter().enumerate() {
            let arr = arr.as_arr().ok_or_else(|| bad("rulesPerItem entry"))?;
            let it = index_in(items, j);
            out.push(Val::arr(
                arr.iter().map(|x| entry_in(x, it)).collect::<R<_>>()?,
            ));
        }
        return Ok(Val::arr(out));
    }
    let rules = rec
        .get("rules")
        .and_then(|r| r.as_arr())
        .ok_or_else(|| bad("rules must be a list"))?;
    if !rec.has("items") {
        let s = scope_of()?;
        return Ok(Val::arr(
            rules.iter().map(|x| entry_in(x, s)).collect::<R<_>>()?,
        ));
    }
    let mut cur: i64 = -1;
    let mut out = Vec::with_capacity(rules.len());
    for x in rules.iter() {
        let j = match x.get("item") {
            Some(i) => i.as_i64().ok_or_else(|| bad("item must be an index"))?,
            None => cur + 1,
        };
        cur = j;
        out.push(entry_in(
            x,
            if j >= 0 {
                index_in(items, j as usize)
            } else {
                None
            },
        )?);
    }
    Ok(Val::arr(out))
}

fn log_only(td: &Val) -> bool {
    matches!(td, Val::Obj(o) if o.len() == 1 && matches!(o.get("log"), Some(Val::Arr(_))))
}

fn is_switch(td: &Val) -> bool {
    matches!(td, Val::Obj(o) if o.len() == 1 && matches!(o.get("statements"), Some(Val::Arr(a)) if a.iter().all(|s| matches!(s, Val::Obj(s) if s.len() == 1 && matches!(s.get("id"), Some(Val::Str(_)))))))
}

fn is_table_td(td: &Val) -> bool {
    is_entry(td)
        || matches!(td, Val::Arr(a) if !a.is_empty() && (a.iter().all(is_entry) || a.iter().all(|x| matches!(x, Val::Arr(l) if l.iter().all(is_entry)))))
}

enum SubScope {
    None,
    Ok,
    Scope(String),
    Items(String),
}

fn sub_scope(input: &Val, reqs: &[Option<Val>], is_loop: bool) -> SubScope {
    if !is_loop {
        let Some(r) = &reqs[0] else {
            return SubScope::None;
        };
        if eq(input, r) {
            return SubScope::Ok;
        }
        let mut found: Option<String> = None;
        walk_paths(
            input,
            &mut |x, path| {
                if found.is_some() {
                    return false;
                }
                if !path.is_empty() && eq(x, r) {
                    found = Some(to_ptr(path));
                    return false;
                }
                true
            },
            6,
        );
        return match found {
            Some(p) => SubScope::Scope(p),
            None => SubScope::None,
        };
    }
    let mut found: Option<String> = None;
    walk_paths(
        input,
        &mut |x, path| {
            if found.is_some() {
                return false;
            }
            if let Val::Arr(a) = x {
                if a.len() >= reqs.len()
                    && reqs
                        .iter()
                        .enumerate()
                        .all(|(i, r)| r.as_ref().is_some_and(|r| eq(&a[i], r)))
                {
                    found = Some(to_ptr(path));
                }
                return false;
            }
            true
        },
        6,
    );
    match found {
        Some(p) => SubScope::Items(p),
        None => SubScope::None,
    }
}

fn sub_request(t: &Val) -> Option<Val> {
    if !is_graph_trace(t) {
        return None;
    }
    let o = t.as_obj().unwrap();
    let first = ordered_keys(o).into_iter().next()?;
    let e = o.get(&first)?;
    if e.get("input").is_some_and(|i| i.is_null()) {
        e.get("output").cloned()
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Input,
    Output,
    Table,
    Expression,
    Switch,
    Function,
    Decision,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Input => "input",
            Kind::Output => "output",
            Kind::Table => "table",
            Kind::Expression => "expression",
            Kind::Switch => "switch",
            Kind::Function => "function",
            Kind::Decision => "decision",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "input" => Kind::Input,
            "output" => Kind::Output,
            "table" => Kind::Table,
            "expression" => Kind::Expression,
            "switch" => Kind::Switch,
            "function" => Kind::Function,
            "decision" => Kind::Decision,
            _ => return None,
        })
    }
}

fn infer_type(entry: &Obj, pos: usize, last: bool, failed: bool) -> Option<Kind> {
    let td = entry.get("traceData").unwrap();
    if td.is_null() {
        if failed {
            return None;
        }
        if pos == 0 && entry.get("input").unwrap().is_null() {
            return Some(Kind::Input);
        }
        if entry.get("output").unwrap().is_null() && last {
            return Some(Kind::Output);
        }
        return None;
    }
    if is_graph_trace(td)
        || matches!(td, Val::Arr(a) if !a.is_empty() && a.iter().all(is_graph_trace))
    {
        return Some(Kind::Decision);
    }
    if is_switch(td) {
        return Some(Kind::Switch);
    }
    if log_only(td) {
        return Some(Kind::Function);
    }
    if is_table_td(td) {
        return Some(Kind::Table);
    }
    if results_of(td, failed).is_some() {
        return Some(Kind::Expression);
    }
    None
}

struct OBase {
    label: &'static str,
    doc: Val,
}

fn output_bases(
    kind: Option<Kind>,
    input: &Val,
    req: &Val,
    failed: bool,
    rbases: Option<&[RBase]>,
) -> (Vec<OBase>, &'static str) {
    let dflt = if failed {
        "null"
    } else if kind == Some(Kind::Input) {
        "request"
    } else if kind == Some(Kind::Output) {
        "null"
    } else if rbases.is_some() {
        "results"
    } else if kind == Some(Kind::Function) {
        "empty"
    } else {
        "input"
    };
    let mut all: Vec<OBase> = Vec::new();
    for b in rbases.unwrap_or(&[]) {
        all.push(OBase {
            label: b.label,
            doc: b.doc.clone(),
        });
    }
    all.push(OBase {
        label: "input",
        doc: input.clone(),
    });
    if kind == Some(Kind::Input) {
        all.push(OBase {
            label: "request",
            doc: req.clone(),
        });
    }
    all.push(OBase {
        label: "empty",
        doc: Val::empty_obj(),
    });
    all.push(OBase {
        label: "null",
        doc: Val::Null,
    });
    if let Some(i) = all.iter().position(|b| b.label == dflt) {
        let b = all.remove(i);
        all.insert(0, b);
    }
    (all, dflt)
}

fn output_base_list(
    kind: Option<Kind>,
    input: &Val,
    req: &Val,
    failed: bool,
    rbases: Option<&[RBase]>,
) -> Vec<Base> {
    let (all, dflt) = output_bases(kind, input, req, failed, rbases);
    all.into_iter()
        .map(|b| Base {
            tag: if b.label == dflt {
                Obj::new()
            } else {
                tag_from(b.label)
            },
            doc: Some(b.doc),
            penalty: 0,
        })
        .collect()
}

pub struct ErrorInfo {
    pub node: String,
    pub message: Option<String>,
}

fn results_val(r: &Results) -> ResultsVal {
    match r {
        Results::One(m, _) => ResultsVal::One(m.clone()),
        Results::Many(ms) => ResultsVal::Many(ms.clone()),
    }
}

fn map_vals(m: &Obj, f: &mut dyn FnMut(&Val) -> R<Val>) -> R<Obj> {
    let mut o = Obj::with_capacity(m.len());
    for (k, v) in m.iter() {
        o.push_unique(k.clone(), f(v)?);
    }
    Ok(o)
}

fn verbatim_trace(t: &Val) -> EncTrace {
    EncTrace::Verbatim(escape(t))
}

pub(crate) fn encode_trace(
    trace: &Val,
    req: &Val,
    d: &mut Shapes,
    failing: bool,
    err: Option<&ErrorInfo>,
) -> R<EncTrace> {
    if !well_formed(trace) {
        return Ok(verbatim_trace(trace));
    }
    let tobj = trace.as_obj().unwrap();
    let keys = ordered_keys(tobj);
    for (pos, key) in keys.iter().enumerate() {
        let e = tobj.get(key).unwrap();
        let id_ok = matches!(e.get("id"), Some(Val::Str(s)) if s == key);
        let order_ok = matches!(e.get("order"), Some(Val::Num(n)) if *n == Decimal::from(pos));
        if !id_ok || !order_ok {
            return Ok(verbatim_trace(trace));
        }
    }
    let entries: Vec<(Key, &Obj)> = keys
        .iter()
        .map(|k| (k.clone(), tobj.get(k).unwrap().as_obj().unwrap()))
        .collect();
    encode_entries(&entries, req, d, failing, err)
}

pub(crate) fn encode_entries(
    entries: &[(Key, &Obj)],
    req: &Val,
    d: &mut Shapes,
    failing: bool,
    err: Option<&ErrorInfo>,
) -> R<EncTrace> {
    let mut refs = Refs::new(req, true, REF_MIN);
    let mut level = Level::new();
    let mut nodes: Vec<EncRec> = Vec::with_capacity(entries.len());
    for (pos, (key, entry)) in entries.iter().enumerate() {
        let entry: &Obj = entry;
        let last = pos == entries.len() - 1;
        let e_input = entry.get("input").unwrap();
        let e_output = entry.get("output").unwrap();
        let td = entry.get("traceData").unwrap();
        let mut rec = Obj::new();
        rec.insert(Rc::from("id"), Val::Str(key.clone()));
        let failed = failing && last && e_output.is_null();
        let name = entry.get("name").unwrap();
        if !matches!(name, Val::Str(s) if s == key) {
            rec.insert(Rc::from("name"), name.clone());
        }
        let kind = infer_type(entry, pos, last, failed);
        if let Some(k) = kind {
            rec.insert(Rc::from("type"), Val::str(k.name()));
        }
        rec.insert(
            Rc::from("perf"),
            perf_out(entry.get("performance").unwrap()),
        );
        if failed {
            rec.insert(Rc::from("failed"), Val::Bool(true));
            if let Some(err) = err {
                if err.node == **key {
                    if let Some(m) = &err.message {
                        rec.insert(Rc::from("error"), Val::str(m));
                    }
                }
            }
        }
        let dflt: Vec<Key> = if pos == 0 {
            Vec::new()
        } else {
            level.open.clone()
        };
        let mut extra: Vec<Base> = Vec::new();
        let dflt_doc = default_input(&level, pos, &dflt, req)?;
        if pos > 0 && !e_input.is_null() && !dflt_doc.as_ref().is_some_and(|d| eq(d, e_input)) {
            for list in discover_parents(e_input, &level, &dflt) {
                let doc = level.merge(&list)?;
                extra.push(Base {
                    tag: tag_merge(&list),
                    doc,
                    penalty: 0,
                });
            }
            if extra.is_empty() {
                for i in (0..level.ids.len()).rev() {
                    let id = &level.ids[i];
                    if dflt.len() == 1 && dflt[0] == *id {
                        continue;
                    }
                    extra.push(Base {
                        tag: tag_merge(std::slice::from_ref(id)),
                        doc: level.output(id).cloned(),
                        penalty: 0,
                    });
                }
            }
        }
        let input = encode_value(e_input, &input_bases(dflt_doc, req, extra), &mut refs)?;
        let used_parents: Vec<Key> = match &input {
            None => dflt.clone(),
            Some(v) if is_bag(v) => match v.get("$merge") {
                Some(Val::Arr(a)) => a.iter().filter_map(|x| x.as_str().map(Rc::from)).collect(),
                _ => Vec::new(),
            },
            Some(_) => Vec::new(),
        };
        if let Some(i) = &input {
            rec.insert(Rc::from("input"), i.clone());
        }
        let mut detail: Vec<(&'static str, Val)> = Vec::new();
        let mut nest_out: Option<super::loops::Nest> = None;
        let mut rres: Option<Results> = None;
        let mut rb: Option<Vec<RBase>> = None;
        match kind {
            Some(Kind::Expression) => {
                let r = results_of(td, failed).unwrap();
                let rv = results_val(&r);
                let ch = choose_results(e_input, e_output, &rv)?;
                if let Some(a) = &ch.at {
                    detail.push(("resultsAt", Val::str(a)));
                }
                if let Some(it) = &ch.items {
                    detail.push(("items", Val::str(it)));
                }
                rb = Some(results_bases(
                    e_input,
                    &rv,
                    ch.at.as_deref(),
                    ch.items.as_deref(),
                )?);
                rres = Some(r);
            }
            Some(Kind::Switch) => {
                let st = td.get("statements").unwrap().as_arr().unwrap();
                detail.push((
                    "branches",
                    Val::arr(st.iter().map(|s| s.get("id").unwrap().clone()).collect()),
                ));
            }
            Some(Kind::Function) => {
                let log = td.get("log").unwrap();
                if !log.as_arr().unwrap().is_empty() {
                    detail.push(("log", escape(log)));
                }
            }
            Some(Kind::Table) => {
                let t = encode_table(td, e_input)?;
                if let Some(s) = t.scope {
                    detail.push(("scope", Val::str(&s)));
                }
                if let Some(i) = t.items {
                    detail.push(("items", Val::str(&i)));
                }
                detail.push((t.field, t.value));
            }
            Some(Kind::Decision) => {
                let is_loop = td.is_arr();
                let subs: Vec<Val> = match td {
                    Val::Arr(a) => a.to_vec(),
                    _ => vec![td.clone()],
                };
                let reqs: Vec<Option<Val>> = subs.iter().map(sub_request).collect();
                let sc = sub_scope(e_input, &reqs, is_loop);
                let mut scoped = Some(e_input.clone());
                let mut items_arr: Option<Val> = None;
                match &sc {
                    SubScope::Scope(p) => {
                        detail.push(("scope", Val::str(p)));
                        scoped = get_path(e_input, &from_ptr(p)?).cloned();
                    }
                    SubScope::Items(p) => {
                        detail.push(("items", Val::str(p)));
                        items_arr = get_path(e_input, &from_ptr(p)?).cloned();
                    }
                    _ => {}
                }
                let n = subs.len();
                let mut encoded = Vec::with_capacity(n);
                for (i, s) in subs.iter().enumerate() {
                    let sreq = if is_loop {
                        index_in(items_arr.as_ref(), i)
                            .cloned()
                            .unwrap_or(Val::Null)
                    } else {
                        scoped.clone().unwrap_or(Val::Null)
                    };
                    encoded.push(encode_trace(s, &sreq, d, failed && i == n - 1, None)?);
                }
                nest_out = Some(nest(
                    d,
                    if is_loop {
                        NestKind::Loop
                    } else {
                        NestKind::Sub
                    },
                    encoded,
                ));
            }
            _ => {
                if matches!(td, Val::Arr(a) if a.is_empty()) {
                    detail.push(("loop", Val::arr(Vec::new())));
                }
            }
        }
        let obases = output_base_list(kind, e_input, req, failed, rb.as_deref());
        let output = encode_value(e_output, &obases, &mut refs)?;
        if let Some(o) = &output {
            rec.insert(Rc::from("output"), o.clone());
        }
        if let Some(r) = &rres {
            match r {
                Results::Many(ms) => {
                    let list = ms
                        .iter()
                        .map(|m| map_vals(m, &mut |v| Ok(refs.encode(v))).map(Val::obj))
                        .collect::<R<Vec<_>>>()?;
                    rec.insert(Rc::from("results"), Val::arr(list));
                }
                Results::One(m, fk) => {
                    let mv = map_vals(m, &mut |v| Ok(refs.encode(v)))?;
                    rec.insert(Rc::from("results"), Val::obj(mv));
                    if let Some(fk) = fk {
                        rec.insert(Rc::from("failedKey"), Val::Str(fk.clone()));
                    }
                }
            }
        }
        for (k, v) in detail {
            rec.insert(Rc::from(k), v);
        }
        let expect = match &rres {
            Some(r) => Some(results_td(rec.get("failedKey"), &results_val(r))),
            None if kind.is_none() && !matches!(td, Val::Arr(a) if a.is_empty()) => Some(Val::Null),
            None => None,
        };
        if let Some(x) = expect {
            if !eq(&x, td) {
                for k in [
                    "results",
                    "failedKey",
                    "resultsAt",
                    "items",
                    "loop",
                    "output",
                ] {
                    rec.remove(k);
                }
                let ob = output_base_list(kind, e_input, req, failed, None);
                if let Some(o) = encode_value(e_output, &ob, &mut refs)? {
                    rec.insert(Rc::from("output"), o);
                }
                rec.insert(Rc::from("details"), escape(td));
            }
        }
        let rec = tidy_record(&Val::obj(rec)).as_obj().unwrap().clone();
        nodes.push(EncRec {
            obj: rec,
            nest: nest_out,
        });
        level.add(key.clone(), e_output.clone(), &used_parents);
        refs.add_node(key, e_output);
    }
    Ok(EncTrace::Nodes(nodes))
}

fn ptr_field<'a>(rec: &RecView<'a>, k: &str) -> R<Option<&'a str>> {
    match rec.get(k) {
        None => Ok(None),
        Some(Val::Str(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("{k} must be a pointer"))),
    }
}

pub(crate) struct DecodeCtx {
    rows: bool,
    at: String,
    keys: Vec<(String, Vec<(Key, Key)>)>,
}

fn fold_rows(rows: &[Val], refs: &Refs, ids: &mut Vec<(Key, Key)>) -> R<Obj> {
    let mut m = Obj::with_capacity(rows.len());
    for row in rows {
        let (Some(Val::Str(id)), Some(Val::Str(key)), Some(value)) =
            (row.get("id"), row.get("key"), row.get("value"))
        else {
            return Err(bad("a result row is {id, key, value}"));
        };
        if !super::marks::is_row(row) {
            return Err(bad("a result row is {id, key, value}"));
        }
        m.set(key, refs.decode(value)?);
        ids.push((id.clone(), key.clone()));
    }
    Ok(m)
}

fn decode_trace(t: &Val, req: &Val, paths: &[Val], ctx: &mut DecodeCtx) -> R<Val> {
    if is_verbatim(t) {
        return Ok(unescape(t.get("verbatim").unwrap()));
    }
    let recs = t
        .get("nodes")
        .and_then(|n| n.as_arr())
        .ok_or_else(|| bad("trace needs nodes"))?;
    let mut refs = Refs::new(req, true, REF_MIN);
    let mut level = Level::new();
    let mut trace = Obj::with_capacity(recs.len());
    for (pos, rv) in recs.iter().enumerate() {
        let rec_obj = rv.as_obj().ok_or_else(|| bad("record must be an object"))?;
        let rec = &RecView::new(rec_obj);
        let key: Key = match rec.get("id") {
            Some(Val::Str(s)) => s.clone(),
            _ => return Err(bad("record needs an id")),
        };
        let kind = match rec.get("type") {
            Some(Val::Str(s)) => Kind::parse(s),
            _ => None,
        };
        let failed = matches!(rec.get("failed"), Some(Val::Bool(true)));
        let dflt: Vec<Key> = if pos == 0 {
            Vec::new()
        } else {
            level.open.clone()
        };
        let rinput = rec.get("input");
        let input = decode_value(
            rinput,
            |bag| {
                if let Some(m) = bag.get("$merge") {
                    let list: Vec<Key> = m
                        .as_arr()
                        .ok_or_else(|| bad("$merge must be a list"))?
                        .iter()
                        .map(|x| x.as_str().map(Rc::from).ok_or_else(|| bad("$merge entry")))
                        .collect::<R<_>>()?;
                    return level.merge(&list);
                }
                match bag.get("$from").map(|f| f.as_str().unwrap_or("")) {
                    Some("request") => Ok(Some(req.clone())),
                    Some("null") => Ok(Some(Val::Null)),
                    Some(f) => Err(bad(format!("unknown input $from {f}"))),
                    None => default_input(&level, pos, &dflt, req),
                }
            },
            &refs,
        )?;
        let used_parents: Vec<Key> = match rinput {
            None => dflt.clone(),
            Some(v) if is_bag(v) => match v.get("$merge") {
                Some(Val::Arr(a)) => a.iter().filter_map(|x| x.as_str().map(Rc::from)).collect(),
                _ => Vec::new(),
            },
            Some(_) => Vec::new(),
        };
        let mut rb: Option<Vec<RBase>> = None;
        let mut results: Option<ResultsVal> = None;
        let mut nulls: Vec<usize> = Vec::new();
        if let Some(r) = rec.get("results") {
            let mut ids: Vec<(Key, Key)> = Vec::new();
            let rv = match r {
                Val::Arr(a) if ctx.rows && a.iter().all(super::marks::is_row) => {
                    ResultsVal::One(fold_rows(a, &refs, &mut ids)?)
                }
                Val::Arr(a) => {
                    let mut items = Vec::with_capacity(a.len());
                    for (i, m) in a.iter().enumerate() {
                        items.push(match m {
                            Val::Obj(m) => map_vals(m, &mut |v| refs.decode(v))?,
                            Val::Arr(rows) if ctx.rows => fold_rows(rows, &refs, &mut ids)?,
                            Val::Null if super::core::lenient() => {
                                nulls.push(i);
                                Obj::new()
                            }
                            _ => return Err(bad("results entry must be a map")),
                        });
                    }
                    ResultsVal::Many(items)
                }
                Val::Obj(m) => ResultsVal::One(map_vals(m, &mut |v| refs.decode(v))?),
                _ => return Err(bad("results must be a map")),
            };
            if !ids.is_empty() {
                let mut at = ctx.at.clone();
                at.push('\u{0}');
                at.push_str(&key);
                ctx.keys.push((at, ids));
            }
            rb = Some(results_bases(
                &input,
                &rv,
                ptr_field(rec, "resultsAt")?,
                ptr_field(rec, "items")?,
            )?);
            results = Some(rv);
        }
        let td = if let Some(dt) = rec.get("details") {
            unescape(dt)
        } else if let Some(r) = &results {
            let mut td = results_td(rec.get("failedKey"), r);
            if let Val::Arr(items) = &mut td {
                let items = Rc::make_mut(items);
                for &i in &nulls {
                    if let Some(x) = items.get_mut(i) {
                        *x = Val::Null;
                    }
                }
            }
            td
        } else if let Some(b) = rec.get("branches") {
            let b = b.as_arr().ok_or_else(|| bad("branches must be a list"))?;
            let st = b
                .iter()
                .map(|id| {
                    let mut o = Obj::with_capacity(1);
                    o.insert(Rc::from("id"), id.clone());
                    Val::obj(o)
                })
                .collect();
            let mut o = Obj::with_capacity(1);
            o.insert(Rc::from("statements"), Val::arr(st));
            Val::obj(o)
        } else if let Some(l) = rec.get("log") {
            let mut o = Obj::with_capacity(1);
            o.insert(Rc::from("log"), unescape(l));
            Val::obj(o)
        } else if rec.has("rule") || rec.has("rules") || rec.has("rulesPerItem") {
            decode_table(rec, &input)?
        } else if matches!(rec.get("loop"), Some(Val::Arr(a)) if a.is_empty()) {
            Val::arr(Vec::new())
        } else if rec.has("loop") || rec.has("sub") {
            let is_loop = rec.has("loop");
            let scoped = match ptr_field(rec, "scope")? {
                Some(p) => get_path(&input, &from_ptr(p)?).cloned(),
                None => Some(input.clone()),
            };
            let items_arr = match ptr_field(rec, "items")? {
                Some(p) => get_path(&input, &from_ptr(p)?).cloned(),
                None => None,
            };
            let subs = if is_loop {
                loop_in(rec.get("loop").unwrap(), paths)?
            } else {
                vec![sub_in(rec.get("sub").unwrap(), paths)?]
            };
            let mut out = Vec::with_capacity(subs.len());
            let outer = ctx.at.clone();
            for (i, s) in subs.iter().enumerate() {
                let sreq = if is_loop {
                    index_in(items_arr.as_ref(), i)
                        .cloned()
                        .unwrap_or(Val::Null)
                } else {
                    scoped.clone().unwrap_or(Val::Null)
                };
                ctx.at = outer.clone();
                ctx.at.push('/');
                super::core::escape_seg(&key, &mut ctx.at);
                if is_loop {
                    ctx.at.push('/');
                    ctx.at.push_str(&i.to_string());
                }
                out.push(decode_trace(s, &sreq, paths, ctx)?);
            }
            ctx.at = outer;
            if is_loop {
                Val::arr(out)
            } else {
                out.pop().unwrap_or(Val::Null)
            }
        } else if kind == Some(Kind::Function) {
            let mut o = Obj::with_capacity(1);
            o.insert(Rc::from("log"), Val::arr(Vec::new()));
            Val::obj(o)
        } else {
            Val::Null
        };
        let routput = rec.get("output");
        let output = decode_value(
            routput,
            |bag| {
                let (all, dflt) = output_bases(kind, &input, req, failed, rb.as_deref());
                let label = match bag.get("$from") {
                    Some(f) => f
                        .as_str()
                        .ok_or_else(|| bad("$from must be a label"))?
                        .to_string(),
                    None => dflt.to_string(),
                };
                all.into_iter()
                    .find(|b| b.label == label)
                    .map(|b| Some(b.doc))
                    .ok_or_else(|| bad(format!("unknown output $from {label}")))
            },
            &refs,
        )?;
        let name = rec
            .get("name")
            .cloned()
            .unwrap_or_else(|| Val::Str(key.clone()));
        let perf = perf_in(rec.get("perf").ok_or_else(|| bad("record needs perf"))?)?;
        let mut e = Obj::with_capacity(7);
        e.insert(Rc::from("id"), Val::Str(key.clone()));
        e.insert(Rc::from("input"), input);
        e.insert(Rc::from("name"), name);
        e.insert(Rc::from("order"), Val::int(pos as i64));
        e.insert(Rc::from("output"), output.clone());
        e.insert(Rc::from("performance"), perf);
        e.insert(Rc::from("traceData"), td);
        trace.insert(key.clone(), Val::obj(e));
        level.add(key.clone(), output.clone(), &used_parents);
        refs.add_node(&key, &output);
    }
    Ok(Val::obj(trace))
}

fn is_step_list(s: &Val) -> bool {
    matches!(s, Val::Arr(a) if a.iter().all(|x| matches!(x.get("expr"), Some(Val::Str(_))) && x.is_obj()))
}

type Split = (Val, Option<Vec<(Key, Found)>>);

pub(crate) struct Found {
    pub(crate) steps: Val,
    pub(crate) no_result: bool,
}

fn split_td(td: &Val) -> (Val, Option<Vec<(Key, Found)>>) {
    let Val::Obj(o) = td else {
        return (td.clone(), None);
    };
    let mut found: Option<Vec<(Key, Found)>> = None;
    let mut out = Obj::with_capacity(o.len());
    for (k, v) in o.iter() {
        let ok = match v {
            Val::Obj(vo) => {
                vo.get("steps").is_some_and(is_step_list)
                    && vo.keys().all(|x| &**x == "steps" || &**x == "result")
            }
            _ => false,
        };
        if ok {
            let no_result = !v.has("result");
            found.get_or_insert_with(Vec::new).push((
                k.clone(),
                Found {
                    steps: v.get("steps").unwrap().clone(),
                    no_result,
                },
            ));
            let mut e = Obj::new();
            if !no_result {
                e.insert(Rc::from("result"), v.get("result").unwrap().clone());
            }
            out.insert(k.clone(), Val::obj(e));
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    match found {
        Some(f) => (Val::obj(out), Some(f)),
        None => (td.clone(), None),
    }
}

pub(crate) enum Block {
    Keys(Vec<Seg>, Key, Vec<(Key, Found)>),
    Items(Vec<Seg>, Key, Vec<Option<Vec<(Key, Found)>>>),
}

pub(crate) fn extract(trace: &Val, path: &[Seg], blocks: &mut Vec<Block>) -> Val {
    if !is_graph_trace(trace) {
        return trace.clone();
    }
    let o = trace.as_obj().unwrap();
    let mut out = Obj::with_capacity(o.len());
    for id in ordered_keys(o) {
        let rec = o.get(&id).unwrap().as_obj().unwrap();
        out.insert(id.clone(), Val::obj(extract_record(&id, rec, path, blocks)));
    }
    Val::obj(out)
}

pub(crate) fn extract_record(id: &Key, rec: &Obj, path: &[Seg], blocks: &mut Vec<Block>) -> Obj {
    {
        let td = rec.get("traceData").unwrap();
        let ntd = match td {
            Val::Arr(a) if !a.is_empty() && a.iter().all(is_graph_trace) => Val::arr(
                a.iter()
                    .enumerate()
                    .map(|(i, x)| {
                        let mut p = path.to_vec();
                        p.push(Seg::K(id.clone()));
                        p.push(Seg::I(i));
                        extract(x, &p, blocks)
                    })
                    .collect(),
            ),
            _ if is_graph_trace(td) => {
                let mut p = path.to_vec();
                p.push(Seg::K(id.clone()));
                extract(td, &p, blocks)
            }
            Val::Arr(a) => {
                let items: Vec<Split> = a.iter().map(split_td).collect();
                if items.iter().any(|x| x.1.is_some()) {
                    let mut tds = Vec::with_capacity(items.len());
                    let mut founds = Vec::with_capacity(items.len());
                    for (t, f) in items {
                        tds.push(t);
                        founds.push(f);
                    }
                    blocks.push(Block::Items(path.to_vec(), id.clone(), founds));
                    Val::arr(tds)
                } else {
                    td.clone()
                }
            }
            _ => {
                let (t, f) = split_td(td);
                if let Some(f) = f {
                    blocks.push(Block::Keys(path.to_vec(), id.clone(), f));
                    t
                } else {
                    td.clone()
                }
            }
        };
        let mut r = rec.clone();
        r.set("traceData", ntd);
        r
    }
}

fn step_entry(f: &Found) -> Val {
    if f.no_result {
        let mut o = Obj::with_capacity(2);
        o.insert(Rc::from("steps"), escape(&f.steps));
        o.insert(Rc::from("noResult"), Val::Bool(true));
        return Val::obj(o);
    }
    escape(&f.steps)
}

fn step_columns(found: &[Option<&Found>]) -> Option<Val> {
    if found.len() < 2 || found.iter().any(|f| f.is_none_or(|f| f.no_result)) {
        return None;
    }
    let lists: Vec<&Vec<Val>> = found
        .iter()
        .map(|f| f.unwrap().steps.as_arr().unwrap())
        .collect();
    let first = lists[0];
    let same = lists.iter().all(|l| {
        l.len() == first.len()
            && l.iter().enumerate().all(|(j, x)| {
                x.get("expr").and_then(|e| e.as_str())
                    == first[j].get("expr").and_then(|e| e.as_str())
                    && x.as_obj().is_some_and(|o| o.len() == 2 && o.has("value"))
            })
    });
    if !same {
        return None;
    }
    let cols = first
        .iter()
        .enumerate()
        .map(|(j, x)| {
            let mut o = Obj::with_capacity(2);
            o.insert(Rc::from("expr"), x.get("expr").unwrap().clone());
            o.insert(
                Rc::from("values"),
                Val::arr(
                    lists
                        .iter()
                        .map(|l| l[j].get("value").unwrap().clone())
                        .collect(),
                ),
            );
            Val::obj(o)
        })
        .collect();
    Some(escape(&Val::arr(cols)))
}

pub(crate) fn encode_steps(blocks: &[Block]) -> Val {
    Val::arr(
        blocks
            .iter()
            .map(|b| {
                let (path, id) = match b {
                    Block::Keys(p, id, _) | Block::Items(p, id, _) => (p, id),
                };
                let mut entry = Obj::new();
                entry.insert(Rc::from("node"), Val::Str(id.clone()));
                if !path.is_empty() {
                    entry.insert(Rc::from("in"), Val::str(&to_ptr(path)));
                }
                match b {
                    Block::Keys(_, _, keys) => {
                        let mut k = Obj::with_capacity(keys.len());
                        for (key, f) in keys {
                            k.insert(key.clone(), step_entry(f));
                        }
                        entry.insert(Rc::from("keys"), Val::obj(k));
                    }
                    Block::Items(_, _, items) => {
                        let mut keys: Vec<Key> = Vec::new();
                        for it in items.iter().flatten() {
                            for (k, _) in it {
                                if !keys.contains(k) {
                                    keys.push(k.clone());
                                }
                            }
                        }
                        keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                        let mut columns: Option<Obj> = None;
                        let mut per_item: Option<Obj> = None;
                        let mut columns_first = None;
                        for k in keys {
                            let found: Vec<Option<&Found>> = items
                                .iter()
                                .map(|x| {
                                    x.as_ref().and_then(|l| {
                                        l.iter().find(|(kk, _)| *kk == k).map(|(_, f)| f)
                                    })
                                })
                                .collect();
                            let cols = step_columns(&found);
                            columns_first.get_or_insert(cols.is_some());
                            match cols {
                                Some(c) => {
                                    columns.get_or_insert_with(Obj::new).insert(k.clone(), c)
                                }
                                None => per_item.get_or_insert_with(Obj::new).insert(
                                    k.clone(),
                                    Val::arr(
                                        found
                                            .iter()
                                            .map(|f| f.map(step_entry).unwrap_or(Val::Null))
                                            .collect(),
                                    ),
                                ),
                            }
                        }
                        let columns = columns.map(|c| ("columns", c));
                        let per_item = per_item.map(|p| ("perItem", p));
                        let ordered = if columns_first == Some(false) {
                            [per_item, columns]
                        } else {
                            [columns, per_item]
                        };
                        for (name, o) in ordered.into_iter().flatten() {
                            entry.insert(Rc::from(name), Val::obj(o));
                        }
                    }
                }
                Val::obj(entry)
            })
            .collect(),
    )
}

fn rekey_steps(e: &Val, rows: &[(Key, Key)]) -> Val {
    let Val::Obj(o) = e else { return e.clone() };
    let mut out = (**o).clone();
    for field in ["keys", "perItem", "columns"] {
        let Some(Val::Obj(m)) = o.get(field) else {
            continue;
        };
        let mut keyed = Obj::with_capacity(m.len());
        for (id, _) in rows {
            if let Some(x) = m.get(id) {
                let key = rows
                    .iter()
                    .rev()
                    .find(|(i, _)| i == id)
                    .map(|(_, k)| k.clone())
                    .unwrap_or_else(|| id.clone());
                keyed.set(&key, x.clone());
            }
        }
        for (k, x) in m.iter() {
            if !rows.iter().any(|(id, _)| id == k) {
                keyed.set(k, x.clone());
            }
        }
        out.set(field, Val::obj(keyed));
    }
    Val::obj(out)
}

fn drop_final(steps: Val) -> Val {
    if let Val::Arr(a) = &steps {
        if a.first()
            .and_then(|x| x.get("final"))
            .is_some_and(|f| matches!(f, Val::Bool(true)))
            && a[0].is_obj()
        {
            return Val::arr(a[1..].to_vec());
        }
    }
    steps
}

fn read_entry(x: &Val) -> (Val, bool) {
    if let Val::Obj(o) = x {
        if let Some(s) = o.get("steps") {
            return (
                drop_final(unescape(s)),
                matches!(o.get("noResult"), Some(Val::Bool(true))),
            );
        }
    }
    (drop_final(unescape(x)), false)
}

fn attach(td: &mut Obj, key: &str, steps: Val, no_result: bool) {
    if no_result {
        let mut o = Obj::with_capacity(1);
        o.insert(Rc::from("steps"), steps);
        td.set(key, Val::obj(o));
    } else {
        let mut o = td
            .get(key)
            .and_then(|x| x.as_obj())
            .cloned()
            .unwrap_or_default();
        o.set("steps", steps);
        td.set(key, Val::obj(o));
    }
}

fn obj_mut(v: &mut Val) -> R<&mut Obj> {
    match v {
        Val::Obj(o) => Ok(Rc::make_mut(o)),
        _ => Err(bad("steps: not an object")),
    }
}

fn level_mut<'a>(trace: &'a mut Val, path: &[Seg]) -> R<&'a mut Val> {
    let mut level = trace;
    let mut i = 0;
    while i < path.len() {
        let k = path[i].text().into_owned();
        let rec = obj_mut(level)?
            .get_mut_val(&k)
            .ok_or_else(|| bad("steps: unknown node"))?;
        let ro = obj_mut(rec)?;
        let td = ro
            .get_mut_val("traceData")
            .ok_or_else(|| bad("steps: no traceData"))?;
        if let Val::Arr(a) = td {
            i += 1;
            let j = path
                .get(i)
                .and_then(|s| s.number())
                .ok_or_else(|| bad("steps: bad iteration"))?;
            level = Rc::make_mut(a)
                .get_mut(j)
                .ok_or_else(|| bad("steps: bad iteration"))?;
        } else {
            if !td.is_obj() {
                *td = Val::empty_obj();
            }
            level = td;
        }
        i += 1;
    }
    Ok(level)
}

fn attach_steps(trace: &mut Val, entries: &[Val], ids: &[(String, Vec<(Key, Key)>)]) -> R<()> {
    for e in entries {
        let path = match e.get("in") {
            Some(Val::Str(p)) => from_ptr(p)?,
            Some(_) => return Err(bad("steps in must be a pointer")),
            None => Vec::new(),
        };
        let node = e
            .get("node")
            .and_then(|n| n.as_str())
            .ok_or_else(|| bad("steps entry needs node"))?
            .to_string();
        let rows = {
            let mut at = match e.get("in") {
                Some(Val::Str(p)) => p.to_string(),
                _ => String::new(),
            };
            at.push('\u{0}');
            at.push_str(&node);
            ids.iter()
                .find(|(k, _)| *k == at)
                .map(|(_, r)| r.as_slice())
        };
        let e = &match rows {
            Some(rows) => rekey_steps(e, rows),
            None => e.clone(),
        };
        let level = level_mut(trace, &path)?;
        let rec = obj_mut(level)?
            .get_mut_val(&node)
            .ok_or_else(|| bad("steps: unknown node"))?;
        let ro = obj_mut(rec)?;
        let td = ro
            .get_mut_val("traceData")
            .ok_or_else(|| bad("steps: no traceData"))?;
        if let Some(keys) = e.get("keys") {
            if !td.is_obj() {
                *td = Val::empty_obj();
            }
            let tdo = obj_mut(td)?;
            for (k, x) in keys
                .as_obj()
                .ok_or_else(|| bad("steps keys must be a map"))?
                .iter()
            {
                let (steps, nr) = read_entry(x);
                attach(tdo, k, steps, nr);
            }
        } else {
            let Val::Arr(items) = td else {
                return Err(bad("steps: traceData must be a list"));
            };
            let items = Rc::make_mut(items);
            if let Some(cols) = e.get("columns") {
                for (k, c0) in cols
                    .as_obj()
                    .ok_or_else(|| bad("steps columns must be a map"))?
                    .iter()
                {
                    let c = unescape(c0);
                    let cols: Vec<&Val> = c
                        .as_arr()
                        .ok_or_else(|| bad("steps column list"))?
                        .iter()
                        .filter(|x| !matches!(x.get("final"), Some(Val::Bool(true))))
                        .collect();
                    for (i, it) in items.iter_mut().enumerate() {
                        let list = cols
                            .iter()
                            .map(|col| {
                                let mut o = Obj::with_capacity(2);
                                o.insert(
                                    Rc::from("expr"),
                                    col.get("expr").cloned().unwrap_or(Val::Null),
                                );
                                if let Some(v) = col
                                    .get("values")
                                    .and_then(|v| v.as_arr())
                                    .and_then(|v| v.get(i))
                                {
                                    o.insert(Rc::from("value"), v.clone());
                                }
                                Val::obj(o)
                            })
                            .collect();
                        attach(obj_mut(it)?, k, Val::arr(list), false);
                    }
                }
            }
            if let Some(per) = e.get("perItem") {
                for (k, list) in per
                    .as_obj()
                    .ok_or_else(|| bad("steps perItem must be a map"))?
                    .iter()
                {
                    for (i, x) in list
                        .as_arr()
                        .ok_or_else(|| bad("steps perItem list"))?
                        .iter()
                        .enumerate()
                    {
                        if x.is_null() {
                            continue;
                        }
                        let it = items
                            .get_mut(i)
                            .ok_or_else(|| bad("steps perItem too long"))?;
                        let (steps, nr) = read_entry(x);
                        attach(obj_mut(it)?, k, steps, nr);
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn encode_graph(
    trace: &Val,
    request: &Val,
    error: Option<&ErrorInfo>,
    engine: Option<&Val>,
) -> R<Val> {
    let mut blocks = Vec::new();
    let plain = extract(trace, &[], &mut blocks);
    let mut d = Shapes::default();
    let enc = encode_trace(&plain, request, &mut d, error.is_some(), error)?;
    Ok(finish_graph(enc, d, &blocks, engine, &[]))
}

pub(crate) fn finish_graph(
    enc: EncTrace,
    d: Shapes,
    blocks: &[Block],
    engine: Option<&Val>,
    extra: &[&str],
) -> Val {
    let body_val = match enc {
        EncTrace::Verbatim(v) => {
            let mut o = Obj::with_capacity(1);
            o.insert(Rc::from("verbatim"), v);
            o
        }
        EncTrace::Nodes(nodes) => {
            let (nodes, shared) = deep_or_flat(nodes, &d);
            let mut o = Obj::with_capacity(2);
            o.insert(
                Rc::from("nodes"),
                Val::arr(nodes.iter().map(tidy_record).collect()),
            );
            if !shared.is_empty() {
                o.insert(
                    Rc::from("paths"),
                    super::loops::tidy_trace(&Val::arr(shared)),
                );
            }
            o
        }
    };
    let mut body = body_val;
    let mut requires: Vec<&str> = Vec::new();
    if !blocks.is_empty() {
        body.insert(Rc::from("steps"), encode_steps(blocks));
        requires.push("steps");
    }
    requires.extend_from_slice(extra);
    let env = super::core::envelope("graph", engine, body, &requires);
    super::core::pack(&Val::obj(env))
}

pub fn decode_graph(body: &Obj, request: &Val, rows: bool) -> R<Val> {
    if let Some(v) = body.get("verbatim") {
        return Ok(unescape(v));
    }
    let paths: Vec<Val> = match body.get("paths") {
        Some(Val::Arr(a)) => a.to_vec(),
        Some(_) => return Err(bad("paths must be a list")),
        None => Vec::new(),
    };
    let nodes = body
        .get("nodes")
        .cloned()
        .ok_or_else(|| bad("graph body needs nodes"))?;
    let mut ctx = DecodeCtx {
        rows,
        at: String::new(),
        keys: Vec::new(),
    };
    let mut trace = decode_trace(
        &nodes_val(
            nodes
                .as_arr()
                .ok_or_else(|| bad("nodes must be a list"))?
                .to_vec(),
        ),
        request,
        &paths,
        &mut ctx,
    )?;
    if let Some(Val::Arr(steps)) = body.get("steps") {
        attach_steps(&mut trace, steps, &ctx.keys)?;
    }
    Ok(trace)
}

pub fn graph_fallback(trace: &Val, engine: Option<&Val>) -> Val {
    let mut body = Obj::with_capacity(1);
    body.insert(Rc::from("verbatim"), escape(trace));
    super::core::pack(&Val::obj(super::core::envelope("graph", engine, body, &[])))
}
