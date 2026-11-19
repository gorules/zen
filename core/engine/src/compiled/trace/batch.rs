use super::collect::{Fact, Hit, Level, Record, Slot, Template};
use super::rows::RowWriter;
use crate::compiled::graph::data::{Data, Keys};
use crate::compiled::host::graph::walker::GraphWalker;
use smallvec::SmallVec;
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::Variable;

pub(crate) type Masked<'a> = (crate::compiled::leaf::Leaf<'a>, Option<Rc<[u64]>>);

pub(crate) enum Cols<'a> {
    Owned(Vec<crate::compiled::leaf::Leaf<'static>>),
    Lazy(Vec<Masked<'a>>, Option<Rc<[usize]>>),
}

impl Cols<'_> {
    pub(crate) fn len(&self) -> usize {
        match self {
            Cols::Owned(leaves) => leaves.len(),
            Cols::Lazy(leaves, _) => leaves.len(),
        }
    }

    pub(crate) fn get(&self, index: usize, position: usize) -> Variable {
        match self {
            Cols::Owned(leaves) => leaves[index].get(position),
            Cols::Lazy(leaves, rows) => {
                let row = match rows {
                    Some(rows) => rows[position],
                    None => position,
                };
                let (leaf, present) = &leaves[index];
                match present
                    .as_ref()
                    .is_some_and(|present| !crate::compiled::leaf::Bits::get(present, row))
                {
                    true => Variable::Null,
                    false => leaf.get(row),
                }
            }
        }
    }
}

pub(crate) enum Facts<'a> {
    None,
    Results((Arc<[Arc<str>]>, Cols<'a>)),
    Objects(Arc<[Arc<str>]>, Rc<[Variable]>),
    First(Vec<Option<u32>>),
    Branches(Vec<SmallVec<[u32; 4]>>),
    Rows(Vec<Fact>),
    Fields(Vec<Option<u32>>, Rc<[super::record::FieldColumn]>),
    Loop(Rc<[(u32, u32)]>, Box<Facts<'a>>),
}

impl Facts<'_> {
    fn at(&self, position: usize, template: &Template, slot: Slot, input: &Variable) -> Fact {
        match self {
            Facts::None => Fact::None,
            Facts::Results((keys, leaves)) => Fact::Results(
                keys.iter()
                    .enumerate()
                    .map(|(index, key)| (key.clone(), leaves.get(index, position)))
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
            Facts::Fields(rules, fields) => rules
                .get(position)
                .copied()
                .flatten()
                .and_then(|rule| {
                    super::record::KernelFacts::captured_hit(template, slot, rule, fields, position)
                })
                .map_or(Fact::None, Fact::Hit),
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

pub(crate) struct BatchEvent<'a> {
    pub(crate) slot: Slot,
    pub(crate) parents: Box<[Slot]>,
    pub(crate) rows: Rc<[usize]>,
    pub(crate) input: bool,
    pub(crate) output: Option<Value<'a>>,
    pub(crate) ns: u64,
    pub(crate) facts: Facts<'a>,
    pub(crate) encoding: Encoding<'a>,
}

pub(crate) struct Asked<'x> {
    cell: OnceCell<Variable>,
    make: &'x dyn Fn() -> Variable,
}

impl<'x> Asked<'x> {
    pub(crate) fn new(make: &'x dyn Fn() -> Variable) -> Self {
        Self {
            cell: OnceCell::new(),
            make,
        }
    }

    pub(crate) fn get(&self) -> &Variable {
        self.cell.get_or_init(|| (self.make)())
    }
}

pub(crate) struct Reserved<'a> {
    pub(crate) paths: bool,
    pub(crate) leaves: Vec<crate::compiled::leaf::Leaf<'a>>,
    pub(crate) rows: usize,
    plain: OnceCell<bool>,
}

impl<'a> Reserved<'a> {
    pub(crate) fn new(
        paths: bool,
        leaves: Vec<crate::compiled::leaf::Leaf<'a>>,
        rows: usize,
    ) -> Self {
        Self {
            paths,
            leaves,
            rows,
            plain: OnceCell::new(),
        }
    }

    fn plain(&self) -> bool {
        *self.plain.get_or_init(|| {
            self.paths
                && self
                    .leaves
                    .iter()
                    .all(|leaf| (0..self.rows).all(|row| !super::rows::reserved(&leaf.get(row))))
        })
    }
}

pub(crate) struct Traces<'a> {
    pub(crate) derived: Option<Reserved<'a>>,
    pub(crate) template: Arc<Template>,
    pub(crate) events: Vec<BatchEvent<'a>>,
    pub(crate) slots: usize,
    pub(crate) replaced: Vec<Option<Level>>,
    plan: OnceCell<Plan<'a>>,
}

#[derive(Default)]
struct Bodies {
    texts: super::value::FastMap<Vec<u64>, Box<str>>,
    hits: u32,
    misses: u32,
    off: bool,
}

enum Body {
    Unknown,
    Rowwise,
    Text(Box<str>),
}

enum Bodied {
    Dense(Vec<Body>),
    Sparse(super::value::FastMap<u64, Body>),
}

struct Keyer {
    facts: u64,
    columns: Vec<(Vec<u32>, u64)>,
    bodies: std::cell::RefCell<Bodied>,
}

impl Keyer {
    fn key(&self, fact: u64, position: usize) -> u64 {
        self.columns.iter().fold(fact, |key, (codes, weight)| {
            key + u64::from(codes[position]) * weight
        })
    }
}

struct Plan<'a> {
    heads: Vec<Option<Box<str>>>,
    labels: Vec<Option<Box<[Box<str>]>>>,
    keyers: Vec<OnceCell<Option<Keyer>>>,
    keys: std::cell::RefCell<Keys>,
    bodies: Vec<std::cell::RefCell<Bodies>>,
    positions: Vec<Box<[u32]>>,
    order: Vec<Option<Box<[u32]>>>,
    edits: Vec<Option<crate::compiled::graph::data::Edits<'a>>>,
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
            derived: None,
            template,
            events: Vec::new(),
            slots,
            replaced: (0..rows).map(|_| None).collect(),
            plan: OnceCell::new(),
        }
    }

    pub(crate) fn record_batch(&mut self, event: BatchEvent<'a>) {
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

impl BatchEvent<'_> {
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

impl<'a> Traces<'a> {
    fn plan(&self) -> &Plan<'a> {
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
            let heads = {
                let writer = RowWriter::new(&self.template);
                self.events
                    .iter()
                    .map(|event| writer.head(event.slot, &super::rows::perf(event.ns)))
                    .collect()
            };
            let labels = self
                .events
                .iter()
                .map(|event| match &event.facts {
                    Facts::Results((keys, _)) => sorted_keys(keys).map(|order| {
                        order
                            .iter()
                            .enumerate()
                            .map(|(i, &k)| {
                                let mut text = String::new();
                                if i > 0 {
                                    text.push(',');
                                }
                                super::value::write_str(&keys[k as usize], &mut text);
                                text.push(':');
                                text.into_boxed_str()
                            })
                            .collect()
                    }),
                    _ => None,
                })
                .collect();
            Plan {
                heads,
                labels,
                keyers: self.events.iter().map(|_| OnceCell::new()).collect(),
                keys: Default::default(),
                bodies: self.events.iter().map(|_| Default::default()).collect(),
                positions,
                edits: self
                    .events
                    .iter()
                    .map(|event| match (&event.output, &event.encoding) {
                        (Some(Value::Data(after)), Encoding::Touched(Some(input))) => {
                            let keys = self.template.touched(event.slot)?;
                            after.edits(input, keys, super::core::escape_seg)
                        }
                        _ => None,
                    })
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

    fn fact(&self, at: usize, position: usize) -> Option<u64> {
        match &self.events[at].facts {
            Facts::None => Some(0),
            Facts::First(rules) | Facts::Fields(rules, _) => Some(
                rules
                    .get(position)
                    .copied()
                    .flatten()
                    .map_or(0, |rule| u64::from(rule) + 1),
            ),
            Facts::Rows(facts) => match facts.get(position) {
                Some(Fact::None) => Some(0),
                Some(Fact::Hit(Hit { rule, refs: None })) => Some(u64::from(*rule) + 1),
                _ => None,
            },
            _ => None,
        }
    }

    fn keyer<'p>(&self, plan: &'p Plan<'a>, at: usize) -> Option<&'p Keyer> {
        plan.keyers[at]
            .get_or_init(|| {
                let event = &self.events[at];
                let (Some(edits), Some(Value::Data(after)), Encoding::Touched(Some(input))) =
                    (&plan.edits[at], &event.output, &event.encoding)
                else {
                    return None;
                };
                let facts = match &event.facts {
                    Facts::None => 1,
                    Facts::First(rules) | Facts::Fields(rules, _) => rules
                        .iter()
                        .flatten()
                        .max()
                        .map_or(1, |rule| u64::from(*rule) + 2),
                    Facts::Rows(facts) => facts
                        .iter()
                        .filter_map(|fact| match fact {
                            Fact::Hit(hit) => Some(u64::from(hit.rule) + 2),
                            _ => None,
                        })
                        .max()
                        .unwrap_or(1),
                    _ => return None,
                };
                let mut weight = facts;
                let mut columns = Vec::new();
                let mut keyed = after.key_columns(input, edits)?;
                if let Facts::Fields(_, fields) = &event.facts {
                    let count = after.len();
                    for field in fields.iter() {
                        let values: Rc<[Variable]> =
                            field.values.iter().take(count).cloned().collect();
                        let (codes, card) = crate::compiled::leaf::Leaf::Any(values).codes(count);
                        keyed.push((
                            codes
                                .iter()
                                .enumerate()
                                .map(|(row, code)| {
                                    match field.failed.get(row).copied().unwrap_or(true) {
                                        true => 0,
                                        false => code + 1,
                                    }
                                })
                                .collect(),
                            card + 1,
                        ));
                    }
                    keyed.extend(input.scope_columns(&self.derived(event.slot)?)?);
                }
                for (codes, card) in keyed {
                    columns.push((codes, weight));
                    weight = weight.checked_mul(u64::from(card))?;
                }
                let bodies = match weight <= 1 << 16 {
                    true => Bodied::Dense((0..weight).map(|_| Body::Unknown).collect()),
                    false => Bodied::Sparse(Default::default()),
                };
                Some(Keyer {
                    facts,
                    columns,
                    bodies: std::cell::RefCell::new(bodies),
                })
            })
            .as_ref()
    }

    fn derived(&self, slot: Slot) -> Option<Vec<Arc<str>>> {
        let node = self.template.node(slot)?;
        let mut fields: Vec<Arc<str>> = Vec::new();
        for rule in &node.rules {
            for field in rule.derive.iter().flatten() {
                if !fields.contains(field) {
                    fields.push(field.clone());
                }
            }
        }
        Some(fields)
    }

    fn keyed(
        &self,
        plan: &Plan<'a>,
        at: usize,
        position: usize,
        walk: &Walk,
        key: &mut Keys,
        columnar: bool,
    ) -> bool {
        let event = &self.events[at];
        let (Some(edits), Some(Value::Data(after)), Encoding::Touched(Some(input))) =
            (&plan.edits[at], &event.output, &event.encoding)
        else {
            return false;
        };
        if walk
            .inputs
            .get(event.slot as usize)
            .is_some_and(Option::is_some)
        {
            return false;
        }
        key.words.clear();
        match &event.facts {
            Facts::None => key.mark(10),
            Facts::First(rules) => match rules.get(position).copied().flatten() {
                Some(rule) => {
                    key.mark(11);
                    key.mark(u64::from(rule));
                }
                None => key.mark(12),
            },
            Facts::Rows(facts) => match facts.get(position) {
                Some(Fact::None) => key.mark(13),
                Some(Fact::Hit(Hit { rule, refs: None })) => {
                    key.mark(14);
                    key.mark(u64::from(*rule));
                }
                _ => return false,
            },
            Facts::Fields(rules, fields) if columnar => {
                key.mark(15);
                if let Some(rule) = rules.get(position).copied().flatten() {
                    key.mark(u64::from(rule));
                    let flat = fields.iter().all(|field| {
                        field.failed.get(position).copied().unwrap_or(true)
                            || field.values.get(position).is_some_and(|value| {
                                !matches!(
                                    value,
                                    Variable::Array(_) | Variable::Object(_) | Variable::Dynamic(_)
                                )
                            })
                    });
                    let scoped = self
                        .derived(event.slot)
                        .is_some_and(|derived| input.scope_flat(position, &derived));
                    if !flat || !scoped {
                        return false;
                    }
                }
            }
            _ => return false,
        }
        after.edit_key(input, edits, position, key)
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
                Variable::Object(_) if !super::rows::merge_exact(&merged, &value) => return None,
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
        request: &Asked,
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
        let mut key = plan.keys.borrow_mut();
        for (at, event) in self.events.iter().enumerate() {
            let position = plan.positions[at][row];
            if position == ABSENT {
                continue;
            }
            let position = position as usize;
            let Some(head) = plan.heads[at].as_deref() else {
                return Err("node kind");
            };
            let parents: SmallVec<[Slot; 4]> = event
                .parents
                .iter()
                .copied()
                .filter(|p| walk.ran.get(*p as usize).is_some_and(Option::is_some))
                .collect();
            let mut shown: SmallVec<[Slot; 4]> = SmallVec::new();
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
                    let open: SmallVec<[Slot; 4]> = writer.open().iter().copied().collect();
                    let reordered = open.len() == parents.len()
                        && open != parents
                        && parents.iter().all(|parent| open.contains(parent));
                    if reordered
                        && self
                            .merged(&mut walk, &open)
                            .is_some_and(|other| super::rows::same(&other, &merged))
                    {
                        shown = open;
                    }
                    if let Some(slot) = walk.inputs.get_mut(event.slot as usize) {
                        *slot = Some(merged);
                    }
                }
                _ => return Err("input shape"),
            }
            let shown = match shown.is_empty() {
                true => &parents,
                false => &shown,
            };
            if !writer.node(head, shown, out) {
                return Err("node");
            }
            if let Some(slot) = walk.ran.get_mut(event.slot as usize) {
                *slot = Some((at, position));
            }
            let parent = parents.first().copied();
            let mark = out.len();
            let merged = walk
                .inputs
                .get(event.slot as usize)
                .is_some_and(Option::is_some);
            let columnar = match (parent, merged, self.keyer(plan, at)) {
                (Some(_), false, Some(keyer)) => self
                    .fact(at, position)
                    .filter(|fact| *fact < keyer.facts)
                    .map(|fact| (keyer, keyer.key(fact, position))),
                _ => None,
            };
            let mut pending = None;
            if let Some((keyer, code)) = columnar {
                let mut bodied = keyer.bodies.borrow_mut();
                let mut spare = Body::Rowwise;
                let body = match &mut *bodied {
                    Bodied::Dense(bodies) => &mut bodies[code as usize],
                    Bodied::Sparse(bodies) if bodies.len() >= 1 << 16 => {
                        bodies.get_mut(&code).unwrap_or(&mut spare)
                    }
                    Bodied::Sparse(bodies) => bodies.entry(code).or_insert(Body::Unknown),
                };
                match body {
                    Body::Text(text) => {
                        out.push_str(text);
                        drop(bodied);
                        writer.end(event.slot, out);
                        continue;
                    }
                    Body::Rowwise => {}
                    Body::Unknown => match self.keyed(plan, at, position, &walk, &mut key, true) {
                        true => pending = Some((keyer, code)),
                        false => *body = Body::Rowwise,
                    },
                }
            }
            let mut bodies = plan.bodies[at].borrow_mut();
            let keyed = columnar.is_none()
                && !bodies.off
                && parent.is_some()
                && self.keyed(plan, at, position, &walk, &mut key, false);
            if keyed {
                if let Some(text) = bodies.texts.get(key.words.as_slice()) {
                    out.push_str(text);
                    bodies.hits += 1;
                    drop(bodies);
                    writer.end(event.slot, out);
                    continue;
                }
                bodies.misses += 1;
                if bodies.misses > 512 && bodies.hits < bodies.misses || bodies.texts.len() > 4096 {
                    bodies.off = true;
                    bodies.texts = Default::default();
                }
            }
            writer.request_only();
            let encoded = self.encode(
                plan,
                at,
                position,
                parent,
                &mut walk,
                request,
                owned,
                &mut writer,
                out,
            );
            match writer.missed() {
                false => encoded?,
                true => {
                    out.truncate(mark);
                    writer.retry();
                    let sources: SmallVec<[(Slot, Variable); 4]> = parents
                        .iter()
                        .filter_map(|p| self.value(&mut walk, *p).map(|value| (*p, value)))
                        .collect();
                    writer.sources(request.get(), sources.into_iter());
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
                }
            }
            if keyed && !bodies.off {
                bodies.texts.insert(key.words.clone(), out[mark..].into());
            }
            if let Some((keyer, code)) = pending {
                let text = Body::Text(out[mark..].into());
                match &mut *keyer.bodies.borrow_mut() {
                    Bodied::Dense(bodies) => bodies[code as usize] = text,
                    Bodied::Sparse(bodies) => {
                        bodies.insert(code, text);
                    }
                }
            }
            drop(bodies);
            writer.end(event.slot, out);
        }
        writer.finish(out);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn encode(
        &self,
        plan: &Plan<'a>,
        at: usize,
        position: usize,
        parent: Option<Slot>,
        walk: &mut Walk,
        request: &Asked,
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
            (Facts::Rows(facts), _) => matches!(facts.get(position), Some(Fact::Results(_))),
            _ => false,
        };
        let rowed: Option<Vec<(&str, Variable)>> = match &event.facts {
            Facts::Rows(facts) => match facts.get(position) {
                Some(Fact::Results(found)) => {
                    let mut sorted: Vec<(&str, Variable)> = found
                        .iter()
                        .map(|(key, value)| (&**key, value.clone()))
                        .collect();
                    sorted.sort_by(|a, b| a.0.cmp(b.0));
                    if sorted.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                        return Err("results keys");
                    }
                    let nested = |a: &str, b: &str| {
                        b.len() > a.len() && b.starts_with(a) && b.as_bytes()[a.len()] == b'.'
                    };
                    let tangled = sorted.iter().any(|(key, _)| {
                        key.split('.').any(|part| part.starts_with('$'))
                            || sorted.iter().any(|(other, _)| nested(key, other))
                    });
                    let reserved = self
                        .template
                        .node(slot)
                        .and_then(|node| node.at.as_deref())
                        .is_some_and(|path| path.split('.').any(|part| part.starts_with('$')));
                    if tangled || reserved {
                        return Err("row fact: results");
                    }
                    Some(sorted)
                }
                _ => None,
            },
            _ => None,
        };
        let fetched: SmallVec<[Variable; 8]> = match &event.facts {
            Facts::Results((_, leaves)) => (0..leaves.len())
                .map(|index| leaves.get(index, position))
                .collect(),
            _ => SmallVec::new(),
        };
        let mut items = false;
        match &event.encoding {
            Encoding::Generic => return Err("generic output"),
            Encoding::Null | Encoding::Passed => {}
            Encoding::Request => match self.derived.as_ref().map(Reserved::plain) {
                Some(true) => {}
                Some(false) => {
                    let request = request.get();
                    if super::rows::reserved(request) && !writer.output(request, out) {
                        return Err("value");
                    }
                }
                None => {
                    let Some(value) = self.value(walk, slot) else {
                        return Err("request");
                    };
                    let request = request.get();
                    let same = match (&value, owned) {
                        (Variable::Object(a), Some(Variable::Object(b))) => Rc::ptr_eq(a, b),
                        (Variable::Array(a), Some(Variable::Array(b))) => Rc::ptr_eq(a, b),
                        _ => false,
                    };
                    if !same && !super::rows::same(&value, request) {
                        return Err("request changed");
                    }
                    if super::rows::reserved(request) && !writer.output(&value, out) {
                        return Err("value");
                    }
                }
            },
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
                let entries = match (results, &rowed) {
                    (true, Some(rowed)) => vec![rowed.clone()],
                    (true, None) => self.entries(plan, at, position),
                    (false, _) => Vec::new(),
                };
                let fits = results
                    && writer
                        .expected(slot, before.as_ref(), &entries, span.is_some())
                        .is_some_and(|expected| super::rows::same(&expected, &after));
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
                let free = match &rowed {
                    Some(rowed) => rowed.iter().all(|(_, value)| super::rows::null_free(value)),
                    None => self.null_free(plan, at, position, &fetched),
                };
                if results && free {
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
                    let edited = match (&plan.edits[at], &event.output, &event.encoding) {
                        (Some(edits), Some(Value::Data(after)), Encoding::Touched(Some(input)))
                            if walk.inputs.get(slot as usize).is_none_or(Option::is_none) =>
                        {
                            after.edit(input, edits, position, &mut |pointer, before, after| {
                                writer.diff_at(pointer, before, after)
                            })
                        }
                        _ => false,
                    };
                    if !edited {
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
            Facts::Results(_) => {
                let (Some(order), Some(labels)) = (&plan.order[at], &plan.labels[at]) else {
                    return Err("results keys");
                };
                writer.results_with(
                    slot,
                    order
                        .iter()
                        .zip(labels.iter())
                        .map(|(&i, label)| (&**label, &fetched[i as usize])),
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
                                            (&*keys[k as usize], leaves.get(k as usize, item))
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
                                                leaves.get(k as usize, item),
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
            Facts::Fields(rules, fields) => match rules.get(position).copied().flatten() {
                None => true,
                Some(rule) => {
                    let hit = super::record::KernelFacts::captured_hit(
                        &self.template,
                        slot,
                        rule,
                        fields,
                        position,
                    )
                    .ok_or("table field references")?;
                    let scope = self
                        .scope(walk, slot, parent, std::slice::from_ref(&hit))
                        .ok_or("hit scope")?;
                    writer.hits(slot, std::slice::from_ref(&hit), true, &scope, out)
                }
            },
            Facts::Rows(facts) => match facts.get(position) {
                Some(Fact::None) => true,
                Some(Fact::Hit(Hit { rule, refs: None })) => writer.rule(slot, *rule, out),
                Some(Fact::Hit(hit)) => {
                    let scope = self
                        .scope(walk, slot, parent, std::slice::from_ref(hit))
                        .ok_or("hit scope")?;
                    writer.hits(slot, std::slice::from_ref(hit), true, &scope, out)
                }
                Some(Fact::Hits(hits)) if hits.is_empty() => {
                    writer.empty_loop(out);
                    true
                }
                Some(Fact::Hits(hits)) => {
                    let scope = self.scope(walk, slot, parent, hits).ok_or("hit scope")?;
                    writer.hits(slot, hits, false, &scope, out)
                }
                Some(Fact::Results(_)) => match rowed {
                    Some(rowed) => writer.results(slot, rowed.into_iter(), out),
                    None => return Err("row fact: results"),
                },
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

    fn scope(
        &self,
        walk: &mut Walk,
        slot: Slot,
        parent: Option<Slot>,
        hits: &[Hit],
    ) -> Option<Variable> {
        let sparse = || {
            if walk.inputs.get(slot as usize)?.is_some() {
                return None;
            }
            let (at, position) = (*walk.ran.get(parent? as usize)?)?;
            let Some(Value::Data(data)) = &self.events[at].output else {
                return None;
            };
            let node = self.template.node(slot)?;
            let mut fields: SmallVec<[Arc<str>; 8]> = SmallVec::new();
            for hit in hits {
                if let Some(derive) = &node.rules.get(hit.rule as usize)?.derive {
                    fields.extend(derive.iter().cloned());
                }
            }
            data.sparse(position, &fields)
        };
        match sparse() {
            Some(scope) => Some(scope),
            None => self.input_of(walk, slot, parent),
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
                .map(|&k| (&*keys[k as usize], leaves.get(k as usize, position)))
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
                            .map(|&k| (&*keys[k as usize], leaves.get(k as usize, item as usize)))
                            .collect()
                    })
                    .collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn null_free(&self, plan: &Plan, at: usize, position: usize, fetched: &[Variable]) -> bool {
        if plan.order[at].is_none() {
            return false;
        }
        match &self.events[at].facts {
            Facts::Results(_) => fetched.iter().all(super::rows::null_free),
            Facts::Loop(spans, inner) => {
                let (Some(&(start, count)), Facts::Results((_, leaves))) =
                    (spans.get(position), inner.as_ref())
                else {
                    return false;
                };
                (start..start + count).all(|item| {
                    (0..leaves.len())
                        .all(|index| super::rows::null_free(&leaves.get(index, item as usize)))
                })
            }
            Facts::Objects(keys, values) => {
                let object = values.get(position).cloned().unwrap_or(Variable::Null);
                keys.iter()
                    .all(|key| object.dot(key).is_some_and(|v| super::rows::null_free(&v)))
            }
            _ => false,
        }
    }
}
