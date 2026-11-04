use super::core::{is_literal, OMITTED_TAG, OMITTED_TEXT, REDACTED_TAG, REDACTED_TEXT};
use super::value::{Obj, Val};
use super::{bad, R};
use std::rc::Rc;

const KEYED: [&str; 7] = [
    "results",
    "keys",
    "columns",
    "perItem",
    "failedCells",
    "references",
    "cells",
];

fn marker(v: &Val) -> R<Option<Val>> {
    let Val::Obj(o) = v else { return Ok(None) };
    if let Some(flag) = o.get(REDACTED_TAG) {
        if !matches!(flag, Val::Bool(true))
            || !o.keys().all(|k| &**k == REDACTED_TAG || &**k == "reason")
            || o.get("reason").is_some_and(|r| r.as_str().is_none())
        {
            return Err(bad(
                "zen-trace v1: a $redacted marker is {\"$redacted\": true, \"reason\"?}",
            ));
        }
        return Ok(Some(Val::str(REDACTED_TEXT)));
    }
    if let Some(inner) = o.get(OMITTED_TAG) {
        let fits = match inner {
            Val::Obj(i) => i
                .iter()
                .all(|(k, n)| &**k == "bytes" && n.as_usize().is_some()),
            _ => false,
        };
        if o.len() != 1 || !fits {
            return Err(bad(
                "zen-trace v1: a value is omitted as {\"$omitted\": {}} or {\"$omitted\": {\"bytes\": n}}",
            ));
        }
        return Ok(Some(Val::str(OMITTED_TEXT)));
    }
    Ok(None)
}

fn omitted_count(v: &Val, what: &str) -> R<Option<usize>> {
    let Val::Obj(o) = v else { return Ok(None) };
    let Some(inner) = o.get(OMITTED_TAG) else {
        return Ok(None);
    };
    match (inner.get(what), inner.as_obj().map(|i| i.len())) {
        (Some(n), Some(1)) if o.len() == 1 => n
            .as_usize()
            .map(Some)
            .ok_or_else(|| bad(format!("zen-trace v1: $omitted {what} must be a count"))),
        (Some(_), _) => Err(bad(format!("zen-trace v1: a bad $omitted {what} marker"))),
        (None, _) => Ok(None),
    }
}

pub(crate) fn data(v: &Val, keyed: bool) -> R<Val> {
    if !keyed {
        if let Some(m) = marker(v)? {
            return Ok(m);
        }
    }
    match v {
        Val::Arr(a) => Ok(Val::arr(
            a.iter().map(|x| data(x, keyed)).collect::<R<Vec<_>>>()?,
        )),
        Val::Obj(o) if is_literal(v) => Ok(Val::Obj(o.clone())),
        Val::Obj(o) => {
            let mut out = Obj::with_capacity(o.len());
            for (k, x) in o.iter() {
                let x = match keyed {
                    true => data(x, false)?,
                    false => data(x, KEYED.contains(&&**k))?,
                };
                out.insert(k.clone(), x);
            }
            Ok(Val::obj(out))
        }
        other => Ok(other.clone()),
    }
}

fn verbatim_null() -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("verbatim"), Val::Null);
    Val::obj(o)
}

fn trace(v: &Val) -> R<Val> {
    let Val::Obj(o) = v else {
        return data(v, false);
    };
    let mut out = Obj::with_capacity(o.len());
    for (k, x) in o.iter() {
        let x = match &**k {
            "nodes" => nodes(x)?,
            "paths" => list_of(x, trace)?,
            _ => data(x, false)?,
        };
        out.insert(k.clone(), x);
    }
    Ok(Val::obj(out))
}

fn list_of(v: &Val, f: fn(&Val) -> R<Val>) -> R<Val> {
    match v {
        Val::Arr(a) => Ok(Val::arr(a.iter().map(f).collect::<R<Vec<_>>>()?)),
        other => f(other),
    }
}

fn nodes(v: &Val) -> R<Val> {
    let Val::Arr(a) = v else {
        return data(v, false);
    };
    let mut out = Vec::with_capacity(a.len());
    for (i, x) in a.iter().enumerate() {
        if omitted_count(x, "nodes")?.is_some() {
            if i + 1 != a.len() {
                return Err(bad("zen-trace v1: omitted nodes must come last"));
            }
            continue;
        }
        out.push(record(x)?);
    }
    Ok(Val::arr(out))
}

fn record(v: &Val) -> R<Val> {
    let Val::Obj(o) = v else {
        return data(v, false);
    };
    let mut out = Obj::with_capacity(o.len());
    for (k, x) in o.iter() {
        let x = match &**k {
            "sub" => sub(x)?,
            "loop" => looped(x)?,
            "results" => results(x)?,
            "id" | "name" | "type" | "perf" | "failed" | "error" | "failedKey" | "resultsAt"
            | "scope" | "items" | "branches" => x.clone(),
            _ => data(x, KEYED.contains(&&**k))?,
        };
        out.insert(k.clone(), x);
    }
    Ok(Val::obj(out))
}

fn sub(v: &Val) -> R<Val> {
    match v {
        Val::Obj(o) if o.has("nodes") || o.has("verbatim") => trace(v),
        Val::Obj(o) => shaped(o),
        other => Ok(other.clone()),
    }
}

fn shape(v: &Val) -> R<Val> {
    match v {
        Val::Obj(_) => trace(v),
        other => Ok(other.clone()),
    }
}

fn shaped(o: &Obj) -> R<Val> {
    let mut out = Obj::with_capacity(o.len());
    for (k, x) in o.iter() {
        let x = match &**k {
            "paths" => list_of(x, trace)?,
            "path" => shape(x)?,
            "iterationPaths" => list_of(x, shape)?,
            _ => x.clone(),
        };
        out.insert(k.clone(), x);
    }
    Ok(Val::obj(out))
}

fn looped(v: &Val) -> R<Val> {
    match v {
        Val::Arr(a) => {
            let mut out = Vec::with_capacity(a.len());
            for x in a.iter() {
                match omitted_count(x, "iterations")? {
                    Some(n) => out.extend(std::iter::repeat_n(verbatim_null(), n)),
                    None => out.push(trace(x)?),
                }
            }
            Ok(Val::arr(out))
        }
        Val::Obj(o) => shaped(o),
        other => Ok(other.clone()),
    }
}

fn results(v: &Val) -> R<Val> {
    match v {
        Val::Arr(a) => {
            let mut out = Vec::with_capacity(a.len());
            for x in a.iter() {
                match omitted_count(x, "iterations")? {
                    Some(n) => out.extend(std::iter::repeat_n(Val::Null, n)),
                    None => out.push(data(x, x.is_obj() && !is_row(x))?),
                }
            }
            Ok(Val::arr(out))
        }
        other => data(other, true),
    }
}

pub(crate) fn is_row(v: &Val) -> bool {
    match v {
        Val::Obj(o) => {
            o.len() == 3
                && matches!(o.get("id"), Some(Val::Str(_)))
                && matches!(o.get("key"), Some(Val::Str(_)))
                && o.has("value")
        }
        _ => false,
    }
}

pub(crate) fn settle(doc: &Val, kind: &str) -> R<Val> {
    let Val::Obj(o) = doc else {
        return Ok(doc.clone());
    };
    let mut out = Obj::with_capacity(o.len());
    for (k, x) in o.iter() {
        let x = match (&**k, kind) {
            (
                "version" | "kind" | "engine" | "requires" | "evaluatedAt" | "timezone" | "seed"
                | "context" | "truncated",
                _,
            ) => x.clone(),
            ("nodes", "graph") => nodes(x)?,
            ("paths", "graph") => list_of(x, trace)?,
            _ => data(x, KEYED.contains(&&**k))?,
        };
        out.insert(k.clone(), x);
    }
    Ok(Val::obj(out))
}
