use crate::compiled::graph::data::Data;
use crate::compiled::graph::tap::Results;
use crate::compiled::host::graph::walker::GraphWalker;
use crate::compiled::trace::collect::{Fact, Hit, Level, Record, Slot, Template};
use crate::compiled::trace::rows::RowWriter;
use smallvec::SmallVec;
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::Variable;

pub(crate) enum Facts {
    None,
    Results(Results),
    Objects(Arc<[Arc<str>]>, Rc<[Variable]>),
    First(Vec<Option<u32>>),
    Branches(Vec<SmallVec<[u32; 4]>>),
    Rows(Vec<Fact>),
    Loop(Rc<[(u32, u32)]>, Box<Facts>),
}

impl Facts {
    fn at(&self, position: usize, template: &Template, slot: Slot, input: &Variable) -> Fact {
        match self {
            Facts::None => Fact::None,
            Facts::Results((keys, leaves)) => Fact::Results(
                keys.iter()
                    .zip(leaves.iter())
                    .map(|(key, leaf)| (key.clone(), leaf.get(position)))
                    .collect(),
            ),
            Facts::Objects(keys, values) => {
                let object = values.get(position).cloned().unwrap_or(Variable::Null);
                Fact::Results(
                    keys.iter()
                        .map(|key| (key.clone(), object.dot(key).unwrap_or(Variable::Null)))
                        .collect(),
                )
            }
            Facts::First(rules) => match rules.get(position).copied().flatten() {
                Some(rule) => Fact::Hit(Hit {
                    rule,
                    refs: Some(template.refs(slot, rule, input)),
                }),
                None => Fact::None,
            },
            Facts::Branches(taken) => {
                Fact::Branches(taken.get(position).cloned().unwrap_or_default())
            }
            Facts::Rows(facts) => facts.get(position).cloned().unwrap_or(Fact::None),
            Facts::Loop(spans, inner) => {
                let Some(&(start, count)) = spans.get(position) else {
                    return Fact::None;
                };
                Fact::Items(
                    (start..start + count)
                        .map(|item| match inner.as_ref() {
                            Facts::First(rules) => {
                                match rules.get(item as usize).copied().flatten() {
                                    Some(rule) => Fact::Hit(Hit { rule, refs: None }),
                                    None => Fact::None,
                                }
                            }
                            other => other.at(item as usize, template, slot, input),
                        })
                        .collect(),
                )
            }
        }
    }
}

pub(crate) enum Value<'a> {
    Null,
    Data(Rc<Data<'a>>),
}

impl Value<'_> {
    fn row(&self, position: usize) -> Variable {
        match self {
            Value::Null => Variable::Null,
            Value::Data(data) => data.materialize_row(position),
        }
    }
}

pub(crate) enum Encoding<'a> {
    Generic,
    Request,
    Null,
    Passed,
    Touched(Option<Rc<Data<'a>>>),
    Record,
    Checked(bool),
    Results(Rc<[bool]>),
}

pub(crate) struct Event<'a> {
    pub(crate) slot: Slot,
    pub(crate) parents: Box<[Slot]>,
    pub(crate) rows: Rc<[usize]>,
    pub(crate) input: bool,
    pub(crate) output: Option<Value<'a>>,
    pub(crate) ns: u64,
    pub(crate) facts: Facts,
    pub(crate) encoding: Encoding<'a>,
}

pub(crate) struct Traces<'a> {
    pub(crate) template: Arc<Template>,
    pub(crate) events: Vec<Event<'a>>,
    pub(crate) slots: usize,
    pub(crate) replaced: Vec<Option<Level>>,
    plan: OnceCell<Plan>,
}

struct Plan {
    positions: Vec<Box<[u32]>>,
    perf: Vec<String>,
    order: Vec<Option<Box<[u32]>>>,
}

struct Walk {
    ran: SmallVec<[Option<(usize, usize)>; 16]>,
    values: SmallVec<[Option<Variable>; 16]>,
    inputs: SmallVec<[Option<Variable>; 16]>,
}

const ABSENT: u32 = u32::MAX;

impl<'a> Traces<'a> {
    pub(crate) fn new(template: Arc<Template>, slots: usize, rows: usize) -> Self {
        Self {
            template,
            events: Vec::new(),
            slots,
            replaced: (0..rows).map(|_| None).collect(),
            plan: OnceCell::new(),
        }
    }

    pub(crate) fn push(&mut self, event: Event<'a>) {
        self.events.push(event);
    }

    pub(crate) fn rows(&self) -> usize {
        self.replaced.len()
    }

    pub(crate) fn level(&self, row: usize) -> Level {
        if let Some(level) = self.replaced.get(row).and_then(Option::as_ref) {
            return level.clone();
        }
        let mut outputs: Vec<Option<Variable>> = vec![None; self.slots];
        let mut level = Level::with_capacity(self.template.clone(), self.events.len());
        for event in &self.events {
            let Ok(position) = event.rows.binary_search(&row) else {
                continue;
            };
            let parents: SmallVec<[Slot; 4]> = event
                .parents
                .iter()
                .copied()
                .filter(|p| outputs.get(*p as usize).is_some_and(Option::is_some))
                .collect();
            let merged = GraphWalker::merge_values(
                parents
                    .iter()
                    .filter_map(|p| outputs.get(*p as usize)?.as_ref()),
            );
            let value = match &event.output {
                Some(value) => value.row(position),
                None => merged.clone(),
            };
            let input = match event.input {
                true => merged,
                false => Variable::Null,
            };
            let output = match &event.output {
                Some(Value::Null) => Variable::Null,
                _ => value.clone(),
            };
            let fact = event.facts.at(position, &self.template, event.slot, &input);
            level.push(Record {
                slot: event.slot,
                parents,
                input,
                output,
                ns: event.ns,
                fact,
            });
            if let Some(slot) = outputs.get_mut(event.slot as usize) {
                *slot = Some(value);
            }
        }
        level
    }
}

fn sorted_keys(keys: &[Arc<str>]) -> Option<Box<[u32]>> {
    let mut order: Vec<u32> = (0..keys.len() as u32).collect();
    order.sort_by(|a, b| keys[*a as usize].cmp(&keys[*b as usize]));
    order.dedup_by(|b, a| keys[*a as usize] == keys[*b as usize]);
    (order.len() == keys.len()).then(|| order.into())
}

impl Event<'_> {
    fn container(&self, position: usize) -> bool {
        match &self.output {
            None => true,
            Some(Value::Null) => false,
            Some(Value::Data(data)) => data.container(position),
        }
    }

    fn output_row(&self, position: usize) -> Option<Variable> {
        match &self.output {
            Some(Value::Data(data)) => Some(data.materialize_row(position)),
            _ => None,
        }
    }
}

impl Traces<'_> {
    fn plan(&self) -> &Plan {
        self.plan.get_or_init(|| {
            let rows = self.rows();
            let positions = self
                .events
                .iter()
                .map(|event| {
                    let mut at = vec![ABSENT; rows];
                    for (position, &row) in event.rows.iter().enumerate() {
                        if let Some(slot) = at.get_mut(row) {
                            *slot = position as u32;
                        }
                    }
                    at.into_boxed_slice()
                })
                .collect();
            Plan {
                positions,
                perf: self
                    .events
                    .iter()
                    .map(|event| crate::compiled::trace::rows::perf(event.ns))
                    .collect(),
                order: self
                    .events
                    .iter()
                    .map(|event| match &event.facts {
                        Facts::Results((keys, _)) | Facts::Objects(keys, _) => sorted_keys(keys),
                        Facts::Loop(_, inner) => match inner.as_ref() {
                            Facts::Results((keys, _)) => sorted_keys(keys),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect(),
            }
        })
    }

    fn value(&self, walk: &mut Walk, slot: Slot) -> Option<Variable> {
        let index = slot as usize;
        if let Some(Some(value)) = walk.values.get(index) {
            return Some(value.clone());
        }
        let (at, position) = (*walk.ran.get(index)?)?;
        let event = &self.events[at];
        let parent = || {
            event
                .parents
                .iter()
                .find(|p| walk.ran.get(**p as usize).is_some_and(Option::is_some))
                .copied()
        };
        let value = match (&event.output, &event.encoding) {
            (Some(Value::Data(data)), Encoding::Touched(input)) => {
                let layer = input.as_ref().and_then(|input| data.extends(input));
                let before = match (layer, walk.inputs.get(index).cloned().flatten(), parent()) {
                    (None, _, _) => None,
                    (Some(_), Some(merged), _) => Some(merged),
                    (Some(_), None, Some(p)) => self.value(walk, p),
                    (Some(_), None, None) => None,
                };
                match (layer, before) {
                    (Some(layer), Some(before)) => Data::apply(layer, position, &before),
                    _ => None,
                }
                .unwrap_or_else(|| data.materialize_row(position))
            }
            (Some(Value::Data(data)), _) => data.materialize_row(position),
            (Some(Value::Null), _) => Variable::Null,
            (None, _) => match walk.inputs.get(index).cloned().flatten() {
                Some(merged) => merged,
                None => self.value(walk, parent()?)?,
            },
        };
        if let Some(cached) = walk.values.get_mut(index) {
            *cached = Some(value.clone());
        }
        Some(value)
    }

    fn merged(&self, walk: &mut Walk, parents: &[Slot]) -> Option<Variable> {
        let (first, rest) = parents.split_first()?;
        let mut merged = self.value(walk, *first)?;
        if !matches!(merged, Variable::Object(_) | Variable::Array(_)) {
            return None;
        }
        for p in rest {
            let value = self.value(walk, *p)?;
            match &value {
                Variable::Object(_)
                    if !crate::compiled::trace::rows::merge_exact(&merged, &value) =>
                {
                    return None
                }
                Variable::Object(_) | Variable::Array(_) => {}
                _ => return None,
            }
            merged = merged.merge_clone(&value);
        }
        Some(merged)
    }

    pub(crate) fn write_row(
        &self,
        row: usize,
        request: &Variable,
        owned: Option<&Variable>,
        engine: Option<&str>,
        evaluation: Option<&crate::compiled::trace::Evaluation>,
        out: &mut String,
    ) -> Result<(), &'static str> {
        if self.replaced.get(row).is_some_and(Option::is_some) {
            return Err("replaced row");
        }
        let plan = self.plan();
        let mut writer = RowWriter::new(&self.template);
        let mut walk = Walk {
            ran: SmallVec::from_elem(None, self.slots),
            values: SmallVec::from_elem(None, self.slots),
            inputs: SmallVec::from_elem(None, self.slots),
        };
        writer.begin(engine, evaluation, out);
        for (at, event) in self.events.iter().enumerate() {
            let position = plan.positions[at][row];
            if position == ABSENT {
                continue;
            }
            let position = position as usize;
            if !writer.fits(event.slot) {
                return Err("node kind");
            }
            let parents: SmallVec<[Slot; 4]> = event
                .parents
                .iter()
                .copied()
                .filter(|p| walk.ran.get(*p as usize).is_some_and(Option::is_some))
                .collect();
            match (writer.first(), parents.as_slice(), event.input) {
                (true, [], false) => {}
                (false, [p], true) => {
                    let Some(Some((pe, pp))) = walk.ran.get(*p as usize).copied() else {
                        return Err("input shape");
                    };
                    if !self.events[pe].container(pp) {
                        return Err("input shape");
                    }
                }
                (false, [_, _, ..], true) => {
                    let merged = self.merged(&mut walk, &parents).ok_or("merge")?;
                    if let Some(slot) = walk.inputs.get_mut(event.slot as usize) {
                        *slot = Some(merged);
                    }
                }
                _ => return Err("input shape"),
            }
            if !writer.node(event.slot, &plan.perf[at], &parents, out) {
                return Err("node");
            }
            if let Some(slot) = walk.ran.get_mut(event.slot as usize) {
                *slot = Some((at, position));
            }
            let parent = parents.first().copied();
            let sources: SmallVec<[(Slot, Variable); 4]> = parents
                .iter()
                .filter_map(|p| self.value(&mut walk, *p).map(|value| (*p, value)))
                .collect();
            writer.sources(request, sources.into_iter());
            self.encode(
                plan,
                at,
                position,
                parent,
                &mut walk,
                request,
                owned,
                &mut writer,
                out,
            )?;
            writer.end(event.slot, out);
        }
        writer.finish(out);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn encode(
        &self,
        plan: &Plan,
        at: usize,
        position: usize,
        parent: Option<Slot>,
        walk: &mut Walk,
        request: &Variable,
        owned: Option<&Variable>,
        writer: &mut RowWriter<'_>,
        out: &mut String,
    ) -> Result<(), &'static str> {
        let event = &self.events[at];
        let slot = event.slot;
        let span = match &event.facts {
            Facts::Loop(spans, _) => spans.get(position).copied(),
            _ => None,
        };
        let results = match (&event.facts, span) {
            (Facts::Results(_) | Facts::Objects(..), _) => true,
            (Facts::Loop(_, inner), Some((_, count))) => {
                count > 0 && matches!(inner.as_ref(), Facts::Results(_))
            }
            _ => false,
        };
        let mut items = false;
        match &event.encoding {
            Encoding::Generic => return Err("generic output"),
            Encoding::Null | Encoding::Passed => {}
            Encoding::Request => {
                let Some(value) = event.output_row(position) else {
                    return Err("request");
                };
                let same = match (&value, owned) {
                    (Variable::Object(a), Some(Variable::Object(b))) => Rc::ptr_eq(a, b),
                    (Variable::Array(a), Some(Variable::Array(b))) => Rc::ptr_eq(a, b),
                    _ => false,
                };
                if !same && !crate::compiled::trace::rows::same(&value, request) {
                    return Err("request changed");
                }
                if crate::compiled::trace::rows::reserved(request) && !writer.output(&value, out) {
                    return Err("value");
                }
            }
            Encoding::Record => {
                if !results {
                    let Some(value) = event.output_row(position) else {
                        return Err("record");
                    };
                    if !writer.output(&value, out) {
                        return Err("value");
                    }
                }
            }
            Encoding::Results(complete) => {
                if !complete.get(position).copied().unwrap_or(false) {
                    let value = self.value(walk, slot).ok_or("results output")?;
                    if !writer.output(&value, out) {
                        return Err("value");
                    }
                }
            }
            Encoding::Checked(pass) => {
                let after = self.value(walk, slot).ok_or("checked output")?;
                let before = match pass {
                    false => None,
                    true => {
                        let before = match walk.inputs.get(slot as usize).cloned().flatten() {
                            Some(merged) => Some(merged),
                            None => parent.and_then(|p| self.value(walk, p)),
                        };
                        Some(before.ok_or("checked input")?)
                    }
                };
                let entries = match results {
                    true => self.entries(plan, at, position),
                    false => Vec::new(),
                };
                let fits = results
                    && writer
                        .expected(slot, before.as_ref(), &entries, span.is_some())
                        .is_some_and(|expected| {
                            crate::compiled::trace::rows::same(&expected, &after)
                        });
                match (before, fits) {
                    (None, true) => {}
                    (None, false) => {
                        if !writer.output(&after, out) {
                            return Err("value");
                        }
                    }
                    (Some(_), true) => match span {
                        None => writer.output_from("input+results", out),
                        Some(_) => {
                            writer.output_from("input+items+results", out);
                            items = true;
                        }
                    },
                    (Some(before), false) => {
                        let (Variable::Object(a), Variable::Object(b)) = (&before, &after) else {
                            return Err("checked shape");
                        };
                        {
                            let (a, b) = (a.borrow(), b.borrow());
                            for (key, value) in b.iter() {
                                writer.touched(key, a.get(key), Some(value));
                            }
                            for (key, value) in a.iter() {
                                if !b.contains_key(key) {
                                    writer.touched(key, Some(value), None);
                                }
                            }
                        }
                        if !writer.edits(results.then_some("input"), out) {
                            return Err("value");
                        }
                    }
                }
            }
            Encoding::Touched(_) => {
                if results && self.null_free(plan, at, position) {
                    match span {
                        None => writer.output_from("input+results", out),
                        Some(_) => {
                            writer.output_from("input+items+results", out);
                            items = true;
                        }
                    }
                } else {
                    let (Some(keys), Some(parent)) = (self.template.touched(slot), parent) else {
                        return Err("touched");
                    };
                    let before = match walk.inputs.get(slot as usize).cloned().flatten() {
                        Some(merged) => Some(merged),
                        None => self.value(walk, parent),
                    };
                    let (Some(before), Some(after)) = (before, self.value(walk, slot)) else {
                        return Err("touched input");
                    };
                    let (Variable::Object(a), Variable::Object(b)) = (&before, &after) else {
                        return Err("touched shape");
                    };
                    {
                        let (a, b) = (a.borrow(), b.borrow());
                        for key in keys.iter() {
                            writer.touched(key, a.get_str(key), b.get_str(key));
                        }
                    }
                    if !writer.edits(results.then_some("input"), out) {
                        return Err("value");
                    }
                }
            }
        }
        let ok = match &event.facts {
            Facts::None => true,
            Facts::Results((keys, leaves)) => {
                let Some(order) = &plan.order[at] else {
                    return Err("results keys");
                };
                writer.results(
                    slot,
                    order
                        .iter()
                        .map(|&i| (&*keys[i as usize], leaves[i as usize].get(position))),
                    out,
                )
            }
            Facts::Objects(keys, values) => {
                let Some(order) = &plan.order[at] else {
                    return Err("results keys");
                };
                let object = values.get(position).cloned().unwrap_or(Variable::Null);
                writer.results(
                    slot,
                    order.iter().map(|&i| {
                        let key = &*keys[i as usize];
                        (key, object.dot(key).unwrap_or(Variable::Null))
                    }),
                    out,
                )
            }
            Facts::Loop(_, inner) => {
                let Some((start, count)) = span else {
                    return Err("loop span");
                };
                let range = start as usize..(start + count) as usize;
                match inner.as_ref() {
                    Facts::Results((keys, leaves)) if count > 0 => {
                        let Some(order) = &plan.order[at] else {
                            return Err("results keys");
                        };
                        let ok = match keys.iter().any(|k| k.starts_with('$')) {
                            true => {
                                writer.results_open(out);
                                let mut ok = true;
                                for (i, item) in range.enumerate() {
                                    ok &= writer.result_item(
                                        i == 0,
                                        order.iter().map(|&k| {
                                            (&*keys[k as usize], leaves[k as usize].get(item))
                                        }),
                                        out,
                                    );
                                }
                                writer.results_close(slot, out);
                                ok
                            }
                            false => {
                                let list: Vec<Variable> = range
                                    .map(|item| {
                                        let mut map =
                                            zen_types::variable::VariableMap::with_capacity(
                                                order.len(),
                                            );
                                        for &k in order.iter() {
                                            map.insert(
                                                (&*keys[k as usize]).into(),
                                                leaves[k as usize].get(item),
                                            );
                                        }
                                        Variable::from_object(map)
                                    })
                                    .collect();
                                writer.results_list(slot, &list, out)
                            }
                        };
                        ok && (!items || writer.items(slot, out))
                    }
                    Facts::First(rules) => {
                        let hits = rules[range.clone()]
                            .iter()
                            .enumerate()
                            .filter_map(|(j, rule)| rule.map(|r| (j, r)));
                        match hits.clone().next() {
                            None => {
                                writer.empty_loop(out);
                                true
                            }
                            Some(_) => {
                                writer.items(slot, out) && writer.looped_rules(slot, hits, out)
                            }
                        }
                    }
                    Facts::Results(_) => {
                        writer.empty_loop(out);
                        true
                    }
                    _ => false,
                }
            }
            Facts::First(rules) => match rules.get(position).copied().flatten() {
                Some(rule) => writer.rule(slot, rule, out),
                None => true,
            },
            Facts::Branches(taken) => match taken.get(position) {
                Some(taken) => writer.branches(slot, taken, out),
                None => false,
            },
            Facts::Rows(facts) => match facts.get(position) {
                Some(Fact::None) => true,
                Some(Fact::Hit(Hit { rule, refs: None })) => writer.rule(slot, *rule, out),
                Some(Fact::Hit(hit)) => {
                    let scope = self.input_of(walk, slot, parent).ok_or("hit scope")?;
                    writer.hits(slot, std::slice::from_ref(hit), true, &scope, out)
                }
                Some(Fact::Hits(hits)) if hits.is_empty() => {
                    writer.empty_loop(out);
                    true
                }
                Some(Fact::Hits(hits)) => {
                    let scope = self.input_of(walk, slot, parent).ok_or("hit scope")?;
                    writer.hits(slot, hits, false, &scope, out)
                }
                Some(Fact::Results(_)) => return Err("row fact: results"),
                Some(Fact::Items(_)) => return Err("row fact: items"),
                Some(Fact::Data(_)) => return Err("row fact: data"),
                Some(Fact::Sub(_)) => return Err("row fact: sub"),
                Some(Fact::Branches(_)) => return Err("row fact: branches"),
                None => false,
            },
        };
        match ok {
            true => Ok(()),
            false => Err("fact"),
        }
    }

    fn input_of(&self, walk: &mut Walk, slot: Slot, parent: Option<Slot>) -> Option<Variable> {
        match walk.inputs.get(slot as usize).cloned().flatten() {
            Some(merged) => Some(merged),
            None => self.value(walk, parent?),
        }
    }

    fn entries(&self, plan: &Plan, at: usize, position: usize) -> Vec<Vec<(&str, Variable)>> {
        let Some(order) = &plan.order[at] else {
            return Vec::new();
        };
        match &self.events[at].facts {
            Facts::Results((keys, leaves)) => vec![order
                .iter()
                .map(|&k| (&*keys[k as usize], leaves[k as usize].get(position)))
                .collect()],
            Facts::Objects(keys, values) => {
                let object = values.get(position).cloned().unwrap_or(Variable::Null);
                vec![order
                    .iter()
                    .map(|&k| {
                        let key = &*keys[k as usize];
                        (key, object.dot(key).unwrap_or(Variable::Null))
                    })
                    .collect()]
            }
            Facts::Loop(spans, inner) => match (spans.get(position), inner.as_ref()) {
                (Some(&(start, count)), Facts::Results((keys, leaves))) => (start..start + count)
                    .map(|item| {
                        order
                            .iter()
                            .map(|&k| (&*keys[k as usize], leaves[k as usize].get(item as usize)))
                            .collect()
                    })
                    .collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn null_free(&self, plan: &Plan, at: usize, position: usize) -> bool {
        if plan.order[at].is_none() {
            return false;
        }
        match &self.events[at].facts {
            Facts::Results((_, leaves)) => leaves
                .iter()
                .all(|leaf| crate::compiled::trace::rows::null_free(&leaf.get(position))),
            Facts::Loop(spans, inner) => {
                let (Some(&(start, count)), Facts::Results((_, leaves))) =
                    (spans.get(position), inner.as_ref())
                else {
                    return false;
                };
                (start..start + count).all(|item| {
                    leaves.iter().all(|leaf| {
                        crate::compiled::trace::rows::null_free(&leaf.get(item as usize))
                    })
                })
            }
            Facts::Objects(keys, values) => {
                let object = values.get(position).cloned().unwrap_or(Variable::Null);
                keys.iter().all(|key| {
                    object
                        .dot(key)
                        .is_some_and(|v| crate::compiled::trace::rows::null_free(&v))
                })
            }
            _ => false,
        }
    }
}
