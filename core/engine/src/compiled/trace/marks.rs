use super::core::{
    escape_seg, is_bag, is_literal, ENVELOPE_KEYS, OMITTED_TAG, OMITTED_TEXT, REDACTED_TAG,
    REDACTED_TEXT,
};
use super::value::{Obj, Val};
use super::{bad, R};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pos {
    Value,
    Fixed,
    Free,
    Graph,
    Policy,
    Trace,
    Traces,
    Nodes,
    Record,
    Results,
    ResultMap,
    Rows,
    Row,
    Entries,
    EntryLists,
    Entry,
    Sub,
    Shaped,
    Shape,
    Shapes,
    Loop,
    Steps,
    Step,
    StepKeys,
    StepPerItem,
    StepItems,
    StepColumns,
    Columns,
    Column,
    StepList,
    StepItem,
    Execs,
    Exec { run: bool },
    Members,
    Writes { run: bool },
    Write,
    Properties,
    PolicyDetails,
    Evaluations,
    TodayTrace,
    TodayExec,
    Values,
    ValueMap,
    KeyedMap,
    SetMap,
    SpliceMap,
    Splice,
    EachMap,
    Each,
    EachSet,
}

const RECORD_FIXED: [&str; 11] = [
    "id",
    "name",
    "type",
    "perf",
    "failed",
    "error",
    "failedKey",
    "resultsAt",
    "scope",
    "items",
    "branches",
];

const EXEC_FIXED: [&str; 20] = [
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
    "writesAbsent",
    "reads",
    "readsAbsent",
    "operandKeys",
    "operandsAbsent",
    "operandsAfterWrite",
];

pub(crate) fn root(kind: &str) -> Pos {
    match kind {
        "policy" => Pos::Policy,
        _ => Pos::Graph,
    }
}

pub(crate) fn child(pos: Pos, parent: &Val, key: &str) -> Pos {
    let arr = parent.is_arr();
    match pos {
        Pos::Value => match parent {
            Val::Obj(o) if o.has("$ref") => Pos::Fixed,
            v if is_bag(v) => match key {
                "$set" => Pos::SetMap,
                "$splice" => Pos::SpliceMap,
                "$each" => Pos::EachMap,
                _ => Pos::Fixed,
            },
            _ => Pos::Value,
        },
        Pos::Fixed => Pos::Fixed,
        Pos::Free => Pos::Free,
        Pos::Graph => match key {
            "nodes" => Pos::Nodes,
            "paths" => Pos::Traces,
            "steps" => Pos::Steps,
            "verbatim" => Pos::Value,
            "context" => Pos::Free,
            k if ENVELOPE_KEYS.contains(&k) => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Policy => match key {
            "executions" => Pos::Execs,
            "properties" => Pos::Properties,
            "extraFields" => Pos::ValueMap,
            "absent" => Pos::Fixed,
            "context" => Pos::Free,
            k if ENVELOPE_KEYS.contains(&k) => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Trace | Pos::Shape => match key {
            "nodes" => Pos::Nodes,
            "paths" => Pos::Traces,
            "verbatim" => Pos::Value,
            _ => Pos::Free,
        },
        Pos::Traces => Pos::Trace,
        Pos::Nodes => Pos::Record,
        Pos::Record => match key {
            "input" | "output" | "log" | "details" => Pos::Value,
            "results" => Pos::Results,
            "rule" => Pos::Entry,
            "rules" => Pos::Entries,
            "rulesPerItem" => Pos::EntryLists,
            "sub" => Pos::Sub,
            "loop" => Pos::Loop,
            k if RECORD_FIXED.contains(&k) => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Results if !arr => Pos::Value,
        Pos::Results => match parent.as_arr().and_then(|a| a.get(index(key)?)) {
            Some(x) if is_marker(x) => Pos::Fixed,
            Some(x) if is_row(x) => Pos::Row,
            Some(Val::Arr(_)) => Pos::Rows,
            _ => Pos::ResultMap,
        },
        Pos::ResultMap | Pos::ValueMap | Pos::KeyedMap | Pos::SetMap | Pos::Values => Pos::Value,
        Pos::Rows => Pos::Row,
        Pos::Row => match key {
            "value" => Pos::Value,
            _ => Pos::Fixed,
        },
        Pos::Entries => Pos::Entry,
        Pos::EntryLists => Pos::Entries,
        Pos::Entry => match key {
            "cells" | "references" => Pos::KeyedMap,
            "verbatim" => Pos::Value,
            "id" | "index" | "item" | "referencesAbsent" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Sub if parent.has("nodes") || parent.has("verbatim") => child(Pos::Trace, parent, key),
        Pos::Sub | Pos::Shaped => match key {
            "paths" => Pos::Traces,
            "path" => Pos::Shape,
            "iterationPaths" => Pos::Shapes,
            _ => Pos::Fixed,
        },
        Pos::Shapes => Pos::Shape,
        Pos::Loop if arr => Pos::Trace,
        Pos::Loop => child(Pos::Shaped, parent, key),
        Pos::Steps => Pos::Step,
        Pos::Step => match key {
            "keys" => Pos::StepKeys,
            "perItem" => Pos::StepPerItem,
            "columns" => Pos::StepColumns,
            "node" | "in" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::StepKeys | Pos::StepItems => Pos::StepList,
        Pos::StepPerItem => Pos::StepItems,
        Pos::StepColumns => Pos::Columns,
        Pos::Columns => Pos::Column,
        Pos::Column => match key {
            "values" => Pos::Values,
            "expr" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::StepList if arr => Pos::StepItem,
        Pos::StepList => match key {
            "steps" => Pos::StepList,
            "noResult" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::StepItem => match key {
            "value" => Pos::Value,
            "values" => Pos::Values,
            "expr" | "at" | "count" | "error" | "errorAt" | "final" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Execs => Pos::Exec {
            run: parent
                .as_arr()
                .and_then(|a| a.get(index(key)?))
                .is_some_and(|x| x.has("members")),
        },
        Pos::Exec { run } => match key {
            "writes" => Pos::Writes { run },
            "storedOperands" | "extraFields" => Pos::ValueMap,
            "details" => Pos::PolicyDetails,
            "perMember" => Pos::Members,
            k if EXEC_FIXED.contains(&k) => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Members => Pos::Exec { run: false },
        Pos::Writes { run } if !arr => match run {
            true => Pos::Values,
            false => Pos::Value,
        },
        Pos::Writes { .. } => Pos::Write,
        Pos::Write => match key {
            "value" => Pos::Value,
            "values" => Pos::Values,
            "path" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::Properties => match key {
            "set" => Pos::ValueMap,
            "verbatim" => Pos::Value,
            "remove" | "absent" => Pos::Fixed,
            _ => Pos::Free,
        },
        Pos::PolicyDetails => match key {
            "value" => Pos::Value,
            "operandValues" => Pos::ValueMap,
            "writes" => Pos::Writes { run: false },
            "evaluations" => Pos::Evaluations,
            "trace" => Pos::TodayTrace,
            "execution" => Pos::TodayExec,
            _ => Pos::Fixed,
        },
        Pos::Evaluations => Pos::ValueMap,
        Pos::TodayTrace => match key {
            "value" => Pos::Value,
            "evaluations" => Pos::Evaluations,
            _ => Pos::Fixed,
        },
        Pos::TodayExec => match key {
            "writes" => Pos::Writes { run: false },
            "operandValues" => Pos::ValueMap,
            "trace" => Pos::TodayTrace,
            _ => Pos::Fixed,
        },
        Pos::SpliceMap => Pos::Splice,
        Pos::Splice => match index(key) {
            Some(i) if i >= 2 => Pos::Value,
            _ => Pos::Fixed,
        },
        Pos::EachMap => Pos::Each,
        Pos::Each => match key {
            "$set" => Pos::EachSet,
            _ => Pos::Fixed,
        },
        Pos::EachSet => Pos::Values,
    }
}

pub(crate) fn keyed(pos: Pos) -> bool {
    matches!(
        pos,
        Pos::Results
            | Pos::ResultMap
            | Pos::KeyedMap
            | Pos::StepKeys
            | Pos::StepPerItem
            | Pos::StepColumns
    )
}

fn index(key: &str) -> Option<usize> {
    super::value::array_index(key).map(|i| i as usize)
}

pub(crate) fn is_marker(v: &Val) -> bool {
    matches!(v, Val::Obj(o) if o.has(REDACTED_TAG) || o.has(OMITTED_TAG))
}

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

fn out_of_place(at: &str) -> super::TraceError {
    bad(format!(
        "zen-trace v1: a $redacted or $omitted marker at {} stands for a structural field; markers may only replace values (4.1)",
        if at.is_empty() { "/" } else { at }
    ))
}

fn has_marker(v: &Val) -> bool {
    match v {
        _ if is_literal(v) => false,
        _ if is_marker(v) => true,
        Val::Arr(a) => a.iter().any(has_marker),
        Val::Obj(o) => o.values().any(has_marker),
        _ => false,
    }
}

fn joined(at: &str, key: &str) -> String {
    let mut s = String::with_capacity(at.len() + key.len() + 1);
    s.push_str(at);
    escape_seg(key, &mut s);
    s
}

fn verbatim_null() -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("verbatim"), Val::Null);
    Val::obj(o)
}

fn walk(v: &Val, pos: Pos, at: &str) -> R<Val> {
    if is_literal(v) {
        return Ok(v.clone());
    }
    match pos {
        Pos::Free => return Ok(v.clone()),
        Pos::Fixed => {
            return match has_marker(v) {
                true => Err(out_of_place(&first_marker(v, at))),
                false => Ok(v.clone()),
            }
        }
        Pos::Value => {
            if let Some(m) = marker(v)? {
                return Ok(m);
            }
        }
        _ if !keyed(pos) && is_marker(v) => return Err(out_of_place(at)),
        _ => {}
    }
    match v {
        Val::Arr(a) => {
            let mut out = Vec::with_capacity(a.len());
            for (i, x) in a.iter().enumerate() {
                let key = i.to_string();
                match pos {
                    Pos::Nodes if omitted_count(x, "nodes")?.is_some() => {
                        if i + 1 != a.len() {
                            return Err(bad("zen-trace v1: omitted nodes must come last"));
                        }
                        continue;
                    }
                    Pos::Loop | Pos::Results => {
                        if let Some(n) = omitted_count(x, "iterations")? {
                            let fill = match pos {
                                Pos::Loop => verbatim_null(),
                                _ => Val::Null,
                            };
                            out.extend(std::iter::repeat_n(fill, n));
                            continue;
                        }
                    }
                    _ => {}
                }
                out.push(walk(x, child(pos, v, &key), &joined(at, &key))?);
            }
            Ok(Val::arr(out))
        }
        Val::Obj(o) => {
            let mut out = Obj::with_capacity(o.len());
            for (k, x) in o.iter() {
                out.insert(k.clone(), walk(x, child(pos, v, k), &joined(at, k))?);
            }
            Ok(Val::obj(out))
        }
        other => Ok(other.clone()),
    }
}

fn first_marker(v: &Val, at: &str) -> String {
    if is_marker(v) {
        return at.to_string();
    }
    match v {
        _ if is_literal(v) => None,
        Val::Arr(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, x)| has_marker(x).then(|| first_marker(x, &joined(at, &i.to_string())))),
        Val::Obj(o) => o
            .iter()
            .find_map(|(k, x)| has_marker(x).then(|| first_marker(x, &joined(at, k)))),
        _ => None,
    }
    .unwrap_or_else(|| at.to_string())
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

pub(crate) fn envelope(doc: &Val) -> R<()> {
    let Val::Obj(o) = doc else { return Ok(()) };
    for (k, x) in o.iter() {
        if &**k != "context" && ENVELOPE_KEYS.contains(&&**k) && has_marker(x) {
            return Err(out_of_place(&first_marker(x, &joined("", k))));
        }
    }
    Ok(())
}

pub(crate) fn settle(body: &Val, kind: &str) -> R<Val> {
    walk(body, root(kind), "")
}
