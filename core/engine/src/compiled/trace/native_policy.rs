use super::collect::{PolicyExecution, PolicyFact, PolicyLevel};
use super::convert::from_variable;
use super::core::{dot_to_ptr, escape};
use super::policy::{group_runs, pack_record, paths_out, Item};
use super::value::{Key, Obj, Val};
use std::rc::Rc;
use zen_types::variable::Variable;

pub(crate) type Miss = &'static str;

pub(crate) fn dotted(v: &Variable) -> bool {
    match v {
        Variable::Object(o) => o.borrow().iter().any(|(k, x)| k.contains('.') || dotted(x)),
        Variable::Array(a) => a.borrow().iter().any(dotted),
        _ => false,
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

fn evaluations_of(evaluations: &[Vec<(std::sync::Arc<str>, Variable)>]) -> Variable {
    Variable::from_array(
        evaluations
            .iter()
            .map(|fields| {
                let mut map = zen_types::variable::VariableMap::with_capacity(fields.len());
                for (k, v) in fields {
                    map.insert((&**k).into(), v.clone());
                }
                Variable::from_object(map)
            })
            .collect(),
    )
}

fn bools(list: &[(std::sync::Arc<str>, bool)]) -> Val {
    Val::arr(list.iter().map(|(_, r)| Val::Bool(*r)).collect())
}

fn ids_val(block: &str, list: &[(std::sync::Arc<str>, bool)]) -> Option<Val> {
    let default = list
        .iter()
        .enumerate()
        .all(|(i, (id, _))| default_id(block, i, id));
    (!default).then(|| Val::arr(list.iter().map(|(id, _)| Val::str(id)).collect()))
}

fn record(e: &PolicyExecution, markers: bool) -> Result<Obj, Miss> {
    let k = |s: &str| -> Key { Rc::from(s) };
    let mut rec = Obj::new();
    rec.insert(k("block"), Val::str(&e.block));
    if let Some(m) = &e.member {
        rec.insert(k("member"), Val::str(m));
    }
    if let Some(p) = &e.policy {
        rec.insert(k("policy"), Val::str(p));
    }
    if markers {
        rec.insert(k("readsAbsent"), Val::Bool(true));
        rec.insert(k("writesAbsent"), Val::Bool(true));
    }
    rec.insert(k("kind"), Val::str(e.fact.kind()));
    let mut det = Obj::new();
    match &e.fact {
        PolicyFact::Expression { property, value } => {
            det.insert(k("property"), Val::str(property));
            det.insert(k("value"), from_variable(value));
        }
        PolicyFact::Match {
            arms,
            matched,
            value,
        } => {
            rec.insert(k("arms"), bools(arms));
            if let Some(ids) = ids_val(&e.block, arms) {
                det.insert(k("ids"), ids);
            }
            det.insert(k("value"), from_variable(value));
            let first = arms.iter().find(|(_, r)| *r).map(|(id, _)| id);
            if first.map(|f| &**f) != matched.as_deref() {
                det.insert(
                    k("matchedArm"),
                    matched.as_deref().map(Val::str).unwrap_or(Val::Null),
                );
            }
        }
        PolicyFact::Assertion { conditions, result } => {
            rec.insert(k("conditions"), bools(conditions));
            if let Some(ids) = ids_val(&e.block, conditions) {
                det.insert(k("ids"), ids);
            }
            det.insert(k("value"), Val::Bool(*result));
        }
        PolicyFact::Table {
            rows,
            evaluations,
            input_pass: None,
        } => {
            let mut whole = Obj::new();
            whole.insert(k("kind"), Val::str("decisionTable"));
            whole.insert(
                k("matchedRows"),
                Val::arr(rows.iter().map(|r| Val::int(*r as i64)).collect()),
            );
            whole.insert(
                k("evaluations"),
                from_variable(&evaluations_of(evaluations)),
            );
            det.insert(k("trace"), Val::obj(whole));
        }
        PolicyFact::Table {
            rows,
            evaluations,
            input_pass: Some(input_pass),
        } => {
            rec.insert(
                k("rows"),
                Val::arr(rows.iter().map(|r| Val::int(*r as i64)).collect()),
            );
            match super::policy::infer_shape(Some(&Val::str(input_pass))) {
                Some(shape) => {
                    rec.insert(k("ruleCount"), Val::int(shape.rules as i64));
                    rec.insert(k("inputCount"), Val::int(shape.inputs as i64));
                    if !shape.fail.is_empty() {
                        let mut fc = Obj::new();
                        for (r, ins) in shape.fail {
                            fc.insert(
                                Rc::from(r.to_string().as_str()),
                                Val::arr(ins.into_iter().map(|i| Val::int(i as i64)).collect()),
                            );
                        }
                        rec.insert(k("failedCells"), Val::obj(fc));
                    }
                }
                None => {
                    let mut x = Obj::new();
                    x.insert(k("inputPass"), Val::str(input_pass));
                    det.insert(k("extras"), Val::obj(x));
                }
            }
            let default = rows.is_empty()
                || (rows.len() == 1 && evaluations.first().is_some_and(|e| e.is_empty()));
            if evaluations.len() != rows.len() || !default {
                det.insert(
                    k("evaluations"),
                    from_variable(&evaluations_of(evaluations)),
                );
            }
        }
    }
    if markers {
        rec.insert(k("operandsAbsent"), Val::Bool(true));
    }
    if !det.is_empty() {
        rec.insert(k("details"), escape(&Val::obj(det)));
    }
    Ok(rec)
}

fn list_len(request: &Variable, path: &str) -> i64 {
    let mut cur = request.clone();
    for seg in path.split('.') {
        let next = match &cur {
            Variable::Object(o) => o.borrow().get_str(seg).cloned(),
            Variable::Array(a) => seg
                .parse::<usize>()
                .ok()
                .and_then(|i| a.borrow().get(i).cloned()),
            _ => None,
        };
        match next {
            Some(v) => cur = v,
            None => return -1,
        }
    }
    match cur {
        Variable::Array(a) => a.borrow().len() as i64,
        _ => -1,
    }
}

fn sweep(member: Option<&str>) -> Option<(&str, bool)> {
    let m = member?;
    let dot = m.rfind('.')?;
    let tail = &m[dot + 1..];
    if tail.is_empty() || !tail.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((&m[..dot], tail.parse::<f64>().ok()? as u64 == 0))
}

pub(crate) fn runs(
    level: &PolicyLevel,
    request: &Variable,
    markers: bool,
) -> Result<Vec<Val>, Miss> {
    let mut items = Vec::with_capacity(level.executions.len());
    for e in &level.executions {
        let len = match sweep(e.member.as_deref()) {
            Some((coll, true)) => list_len(request, coll),
            _ => -1,
        };
        items.push(Item {
            rec: record(e, markers)?,
            len,
        });
    }
    let to_ptr = |s: &str| -> super::R<String> { Ok(dot_to_ptr(s)) };
    group_runs(items)
        .iter()
        .map(|r| {
            paths_out(r, &to_ptr)
                .map(|r| pack_record(&r))
                .map_err(|_| "member path")
        })
        .collect()
}
