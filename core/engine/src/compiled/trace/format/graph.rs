use super::core::{escape, to_ptr, Seg};
use super::loops::{deep_or_flat, tidy_record, EncTrace, Shapes};
use super::value::{Key, Obj, Val};
use super::R;
use std::rc::Rc;

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

pub(crate) fn cell_parts(k: &str) -> Option<(&str, &str)> {
    let field = cell_field(k)?;
    Some((field, &k[field.len() + 1..k.len() - 1]))
}

pub fn plain_path(f: &str) -> bool {
    f.split('.').all(|s| {
        let b = s.as_bytes();
        !b.is_empty()
            && (b[0].is_ascii_alphabetic() || b[0] == b'_')
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
    })
}

pub(crate) struct RecView<'a> {
    pub(crate) slots: [Option<&'a Val>; 23],
}

pub(crate) fn rec_slot(k: &str) -> usize {
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
        "inputs" => 13,
        "rule" => 14,
        "rules" => 15,
        "rulesPerItem" => 16,
        "branches" => 17,
        "log" => 18,
        "details" => 19,
        "sub" => 20,
        "loop" => 21,
        _ => 22,
    }
}

impl<'a> RecView<'a> {
    pub(crate) fn new(rec: &'a Obj) -> Self {
        let mut slots = [None; 23];
        for (k, v) in rec.iter() {
            let i = rec_slot(k);
            if i < 22 {
                slots[i] = Some(v);
            }
        }
        Self { slots }
    }

    pub(crate) fn get(&self, k: &str) -> Option<&'a Val> {
        self.slots[rec_slot(k)]
    }

    pub(crate) fn has(&self, k: &str) -> bool {
        self.get(k).is_some()
    }
}

pub struct ErrorInfo {
    pub node: String,
    pub message: Option<String>,
}

pub(crate) struct Found {
    pub(crate) steps: Val,
    pub(crate) no_result: bool,
}

pub(crate) enum Block {
    Keys(Vec<Seg>, Key, Vec<(Key, Found)>),
    Items(Vec<Seg>, Key, Vec<Option<Vec<(Key, Found)>>>),
}

pub(crate) fn step_entry(f: &Found) -> Val {
    if f.no_result {
        let mut o = Obj::with_capacity(2);
        o.insert(Rc::from("steps"), escape(&f.steps));
        o.insert(Rc::from("noResult"), Val::Bool(true));
        return Val::obj(o);
    }
    escape(&f.steps)
}

pub(crate) fn step_columns(found: &[Option<&Found>]) -> Option<Val> {
    if found.len() < 2 || found.iter().any(|f| f.is_none_or(|f| f.no_result)) {
        return None;
    }
    let lists: Vec<&Vec<Val>> = found
        .iter()
        .map(|f| f.and_then(|f| f.steps.as_arr()))
        .collect::<Option<_>>()?;
    let first = *lists.first()?;
    let same = lists.iter().all(|l| {
        l.len() == first.len()
            && l.iter().zip(first.iter()).all(|(x, f)| {
                x.get("expr").and_then(|e| e.as_str()) == f.get("expr").and_then(|e| e.as_str())
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
            o.insert(Rc::from("expr"), x.get("expr")?.clone());
            o.insert(
                Rc::from("values"),
                Val::arr(
                    lists
                        .iter()
                        .map(|l| l.get(j).and_then(|s| s.get("value")).cloned())
                        .collect::<Option<_>>()?,
                ),
            );
            Some(Val::obj(o))
        })
        .collect::<Option<_>>()?;
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

pub(crate) fn finish_graph(
    enc: EncTrace,
    d: Shapes,
    blocks: &[Block],
    engine: Option<&Val>,
    extra: &[&str],
) -> R<Val> {
    let body_val = match enc {
        EncTrace::Verbatim(v) => {
            let mut o = Obj::with_capacity(1);
            o.insert(Rc::from("verbatim"), v);
            o
        }
        EncTrace::Nodes(nodes) => {
            let (nodes, shared) = deep_or_flat(nodes, &d)?;
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
    Ok(super::core::pack(&Val::obj(env)))
}
