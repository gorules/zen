use super::typed::{Typed, TypedGroup};
use super::{Pass, Sheet};
use crate::compiled::census::Census;
use crate::compiled::core::fuse::{Fused, Runs};
use crate::compiled::core::variants::Variants;
use crate::compiled::leaf::{Array, Bits, Dict, Leaf, Literal, Store};
use crate::compiled::lowered::Exports;
use crate::compiled::policy::blocks::Operator;
use crate::compiled::policy::schedule::{End, Event, Segment};
use crate::compiled::policy::{Native, PolicyPlan};
use crate::compiled::shred::Normalizer;
use crate::compiled::table::Picked;
use crate::compiled::table::{Table, TableIr};
use crate::policy::blocks::{AssertionIr, BlockKind, DecisionTableIr, MatchIr};
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use zen_expression::lane::{Fusion, LaneProgram};
use zen_expression::typed::TableSpec;
use zen_expression::Variable;
use zen_types::path::DotPath;

type Groups = ahash::HashMap<Chain, Variants<Arc<TypedGroup>>>;
type Lanes = ahash::HashMap<Vec<usize>, Option<Arc<Fused<Sink>>>>;

#[derive(Default)]
pub(crate) struct Fusions {
    groups: RwLock<Lanes>,
    effects: RwLock<ahash::HashMap<usize, Option<Arc<Effects>>>>,
    tables: RwLock<ahash::HashMap<usize, Option<bool>>>,
    runs: Runs<Chain>,
    pub(super) typed: RwLock<Groups>,
}

pub(crate) struct Effects {
    reads: Vec<Arc<str>>,
    writes: Vec<Arc<str>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Chain {
    pub blocks: Vec<usize>,
    pub member: bool,
}

impl Chain {
    pub fn of(blocks: &[usize], member: bool) -> Self {
        Self {
            blocks: blocks.to_vec(),
            member,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Span {
    pub block: usize,
    pub start: usize,
    pub len: usize,
}

struct Item {
    units: Vec<Span>,
    gate: Option<usize>,
    domain: Option<Option<usize>>,
    written: Vec<(Arc<str>, bool)>,
}

pub(super) enum Sink {
    Write(Arc<str>),
    Assert(Arc<AssertionIr>),
    Gather(Gather),
}

pub(super) enum Cell {
    Constant(Literal),
    Computed,
}

pub(super) struct Gather {
    fields: Vec<Arc<str>>,
    scalars: Vec<bool>,
    rules: Vec<Vec<(usize, Cell)>>,
    computed: usize,
}

impl Gather {
    pub fn of(ir: &DecisionTableIr, table: &Table) -> Self {
        let profile = TableIr::policy(ir);
        let picked = table.picked();
        let mut fields: Vec<Arc<str>> = Vec::new();
        let mut scalars: Vec<bool> = Vec::new();
        let targets: Vec<usize> = profile
            .written()
            .map(|o| match fields.iter().position(|f| f.as_ref() == o.path) {
                Some(at) => at,
                None => {
                    fields.push(Arc::from(o.path));
                    scalars.push(!o.collect);
                    fields.len() - 1
                }
            })
            .collect();
        let mut computed = 0;
        let rules = profile
            .rules
            .iter()
            .enumerate()
            .map(|(at, rule)| {
                profile
                    .written()
                    .enumerate()
                    .filter(|(_, o)| profile.cell(rule, o.id).is_some())
                    .map(|(column, _)| {
                        let constant = Self::literal(picked, at, column);
                        let cell = match constant {
                            Some(value) => Cell::Constant(value),
                            None => {
                                computed += 1;
                                Cell::Computed
                            }
                        };
                        (targets[column], cell)
                    })
                    .collect()
            })
            .collect();
        Self {
            fields,
            scalars,
            rules,
            computed,
        }
    }

    pub fn literal(picked: &[Picked], rule: usize, column: usize) -> Option<Literal> {
        picked.get(column)?.constants.get(rule)?.clone()
    }

    fn leaves<'a>(&self, conds: &[Vec<u64>], values: &[Leaf<'a>], count: usize) -> Vec<Leaf<'a>> {
        let mut masks = vec![0u64; count];
        for (rule, bits) in conds.iter().enumerate() {
            for row in Bits::rows(bits) {
                if let Some(mask) = masks.get_mut(row) {
                    *mask |= 1 << rule;
                }
            }
        }
        let gather = |mask: u64, row: usize| -> Vec<Vec<Variable>> {
            let mut lists = vec![Vec::new(); self.fields.len()];
            let mut next = 0;
            for (rule, cells) in self.rules.iter().enumerate() {
                let matched = mask & (1 << rule) != 0;
                for (target, cell) in cells {
                    let taken = matched && !(self.scalars[*target] && !lists[*target].is_empty());
                    match cell {
                        Cell::Constant(value) if taken => lists[*target].push(value.variable()),
                        Cell::Constant(_) => {}
                        Cell::Computed => {
                            if taken {
                                lists[*target].push(values.get(next).map_or(Variable::Null, |leaf| leaf.get(row)));
                            }
                            next += 1;
                        }
                    }
                }
            }
            lists
        };
        let value = |scalar: bool, list: Vec<Variable>| match scalar {
            true => list.into_iter().next().unwrap_or(Variable::Null),
            false => Variable::from_array(list),
        };
        let typed = |column: Vec<Variable>| {
            let leaf = Leaf::Any(column.into());
            Normalizer::cast(&leaf).unwrap_or(leaf)
        };
        if self.computed > 0 {
            let mut columns: Vec<Vec<Variable>> = vec![Vec::with_capacity(count); self.fields.len()];
            for (row, mask) in masks.iter().enumerate() {
                for ((column, list), scalar) in columns.iter_mut().zip(gather(*mask, row)).zip(&self.scalars) {
                    column.push(value(*scalar, list));
                }
            }
            return columns
                .into_iter()
                .zip(&self.scalars)
                .map(|(column, scalar)| match scalar {
                    true => typed(column),
                    false => Leaf::Any(column.into()),
                })
                .collect();
        }
        let mut seen: ahash::HashMap<u64, i32> = ahash::HashMap::default();
        let mut distinct: Vec<u64> = Vec::new();
        let codes: Rc<[i32]> = masks
            .iter()
            .map(|mask| {
                *seen.entry(*mask).or_insert_with(|| {
                    distinct.push(*mask);
                    distinct.len() as i32 - 1
                })
            })
            .collect();
        let mut dicts: Vec<Vec<Variable>> = vec![Vec::with_capacity(distinct.len()); self.fields.len()];
        for mask in &distinct {
            for ((dict, list), scalar) in dicts.iter_mut().zip(gather(*mask, 0)).zip(&self.scalars) {
                dict.push(value(*scalar, list));
            }
        }
        dicts
            .into_iter()
            .zip(&self.scalars)
            .map(|(values, scalar)| match scalar {
                true => typed(codes.iter().map(|&code| values[code as usize].clone()).collect()),
                false => Leaf::typed(Array::coded(codes.clone(), Rc::new(Dict::Any(values)))),
            })
            .collect()
    }
}

struct Unit<'p> {
    entity: Option<usize>,
    programs: Vec<&'p LaneProgram>,
    writes: Vec<(Arc<str>, bool)>,
}

impl Fusions {
    fn table(&self, block: usize, ir: &DecisionTableIr) -> Option<bool> {
        if let Some(found) = self
            .tables
            .read()
            .ok()
            .and_then(|cache| cache.get(&block).copied())
        {
            return found;
        }
        let profile = TableIr::policy(ir);
        let found = match (profile.typable(), profile.gathers()) {
            (true, _) => Some(true),
            (false, true) => Some(false),
            (false, false) => None,
        };
        if let Ok(mut cache) = self.tables.write() {
            cache.insert(block, found);
        }
        found
    }

    fn effects(&self, plan: &PolicyPlan, block: usize) -> Option<Arc<Effects>> {
        if let Some(found) = self
            .effects
            .read()
            .ok()
            .and_then(|cache| cache.get(&block).cloned())
        {
            return found;
        }
        let built = Effects::of(plan, block).map(Arc::new);
        if let Ok(mut cache) = self.effects.write() {
            cache.insert(block, built.clone());
        }
        built
    }

    fn get(&self, plan: &PolicyPlan, blocks: &[usize], member: bool) -> Option<Arc<Fused<Sink>>> {
        if let Some(found) = self
            .groups
            .read()
            .ok()
            .and_then(|cache| cache.get(blocks).cloned())
        {
            return found;
        }
        let built = Fused::<Sink>::compile(plan, blocks, member).map(Arc::new);
        match self.groups.write() {
            Ok(mut cache) => cache.entry(blocks.to_vec()).or_insert(built).clone(),
            Err(_) => built,
        }
    }
}

impl Effects {
    fn of(plan: &PolicyPlan, block: usize) -> Option<Self> {
        let (programs, _) = Pass::block(plan, block)?;
        let rule = &plan.rule(block);
        let iteration = plan.flat(block).iteration.clone();
        if let Some((_, path, Some(owner))) = &iteration {
            if path.split('.').next() != Some(owner.as_ref()) {
                return None;
            }
        }
        let alias = iteration.map(|(name, path, _)| (name, path));
        let map = |key: &str| -> Arc<str> {
            match &alias {
                Some((name, path)) if key == name.as_ref() => path.clone(),
                Some((name, path)) => match DotPath::tail(key, name) {
                    Some(rest) => Arc::from(format!("{path}.{rest}")),
                    None => Arc::from(key),
                },
                None => Arc::from(key),
            }
        };
        let mut reads: Vec<Arc<str>> = Vec::new();
        for program in programs {
            let p = program.program();
            if p.writes_env || p.chain || p.opaque() || p.rows {
                return None;
            }
            let mut keys: Vec<&str> = p.site_keys.iter().flatten().map(String::as_str).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                if key.starts_with('$') || key.contains('[') {
                    return None;
                }
                match alias.is_none().then(|| p.source_fields(key)).flatten() {
                    Some(fields) => reads.extend(
                        fields
                            .iter()
                            .map(|field| Arc::from(format!("{key}.{field}"))),
                    ),
                    None => reads.push(map(key)),
                }
            }
        }
        let mut writes: Vec<Arc<str>> = rule
            .kind
            .write_sites()
            .into_iter()
            .map(|site| map(&site.path))
            .collect();
        let linked: Vec<Arc<str>> = writes
            .iter()
            .flat_map(|write| {
                plan.links.iter().filter_map(move |(target, path)| {
                    match write.as_ref() == target.as_ref() {
                        true => Some(path.clone()),
                        false => DotPath::tail(write, target)
                            .map(|rest| Arc::from(format!("{path}.{rest}"))),
                    }
                })
            })
            .collect();
        writes.extend(linked);
        Some(Self { reads, writes })
    }

    fn commute(&self, other: &Effects) -> bool {
        let clash = |xs: &[Arc<str>], ys: &[Arc<str>]| {
            xs.iter()
                .any(|x| ys.iter().any(|y| Sheet::related(x, y).is_some()))
        };
        !clash(&self.writes, &other.reads)
            && !clash(&self.writes, &other.writes)
            && !clash(&other.writes, &self.reads)
    }
}

impl Fused<Sink> {
    fn compile(plan: &PolicyPlan, blocks: &[usize], member: bool) -> Option<Self> {
        let mut entries: Vec<Fusion> = Vec::new();
        let mut sinks = Vec::with_capacity(blocks.len());
        for &block in blocks {
            if member && plan.flat(block).table.is_some() {
                return None;
            }
            if member && Self::siblings(plan, block) {
                return None;
            }
            let (unit, sunk) = Self::entries(plan, block, member)?;
            entries.extend(unit);
            sinks.extend(sunk);
        }
        Self::build(&entries, sinks)
    }

    fn siblings(plan: &PolicyPlan, block: usize) -> bool {
        let Some((_, path, _)) = &plan.flat(block).iteration else {
            return false;
        };
        Pass::block(plan, block).is_some_and(|(programs, _)| {
            programs.iter().any(|program| {
                program
                    .program()
                    .site_keys
                    .iter()
                    .flatten()
                    .any(|key| *key == **path)
            })
        })
    }

    pub(super) fn entries(
        plan: &PolicyPlan,
        block: usize,
        member: bool,
    ) -> Option<(Vec<Fusion>, Vec<Sink>)> {
        let mut entries: Vec<Fusion> = Vec::new();
        let mut sinks = Vec::new();
        if let BlockKind::DecisionTable(ir) = &plan.rule(block).kind {
            let (hidden, tables) = Self::table(ir, block)?;
            entries.extend(hidden);
            for entry in tables {
                if let Fusion::Rules { key, .. } = &entry {
                    sinks.push(Sink::Write(Arc::from(key.as_str())));
                }
                entries.push(entry);
            }
            return Some((entries, sinks));
        }
        let native = match member {
            true => Pass::native(plan, block)?,
            false => plan.flat(block).native()?,
        };
        match native {
            Native::Expression { key, program } => {
                entries.push(Fusion::Output {
                    key: key.to_string(),
                    source: program.source()?.to_string(),
                });
                sinks.push(Sink::Write(key.clone()));
            }
            Native::Match { ir, .. } => {
                entries.push(Fusion::Output {
                    key: ir.key.to_string(),
                    source: Self::ternary(ir),
                });
                sinks.push(Sink::Write(ir.key.clone()));
            }
            Native::Assertion { ir, .. } => {
                entries.extend(ir.conditions.iter().map(|c| Fusion::Output {
                    key: String::new(),
                    source: c.expression.to_string(),
                }));
                sinks.push(Sink::Assert(ir.clone()));
            }
        }
        Some((entries, sinks))
    }

    fn fusable(ir: &DecisionTableIr) -> bool {
        TableIr::policy(ir).filled()
    }

    pub(super) fn spec(ir: &DecisionTableIr) -> Option<TableSpec> {
        TableIr::policy(ir).spec()
    }

    fn table(ir: &DecisionTableIr, block: usize) -> Option<(Vec<Fusion>, Vec<Fusion>)> {
        if !Self::fusable(ir) {
            return None;
        }
        let mut hidden = Vec::new();
        let mut fields: Vec<(String, &Arc<str>)> = Vec::new();
        for (index, input) in ir.inputs.iter().enumerate() {
            if ir
                .rules
                .iter()
                .all(|rule| rule.get(&input.id).is_none_or(|c| c.is_empty()))
            {
                continue;
            }
            let source = input.field.as_deref()?;
            let plain = source.split('.').all(|segment| {
                let mut chars = segment.chars();
                chars
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            let key = match plain {
                true => source.to_string(),
                false => {
                    let key = format!("__fuse{block}_{index}");
                    hidden.push(Fusion::Hidden {
                        key: key.clone(),
                        source: source.to_string(),
                    });
                    key
                }
            };
            fields.push((key, &input.id));
        }
        let outputs = ir
            .outputs
            .iter()
            .filter(|o| !o.field.is_empty())
            .map(|output| {
                let rules = ir
                    .rules
                    .iter()
                    .map(|rule| {
                        let cells = fields
                            .iter()
                            .filter_map(|(key, id)| {
                                rule.get(*id)
                                    .filter(|c| !c.is_empty())
                                    .map(|c| (key.clone(), c.to_string()))
                            })
                            .collect();
                        (
                            cells,
                            rule.get(&output.id)
                                .map(|c| c.to_string())
                                .unwrap_or_default(),
                        )
                    })
                    .collect();
                Fusion::Rules {
                    key: output.field.to_string(),
                    rules,
                }
            })
            .collect();
        Some((hidden, outputs))
    }

    fn ternary(ir: &MatchIr) -> String {
        let mut source = String::new();
        let mut open = 0usize;
        let mut tail = String::from("null");
        for arm in &ir.arms {
            let value = match arm.value.is_empty() {
                true => String::from("null"),
                false => format!("({})", arm.value),
            };
            if arm.condition.is_empty() {
                tail = value;
                break;
            }
            source.push_str(&format!("(({}) == true) ? {value} : (", arm.condition));
            open += 1;
        }
        source.push_str(&tail);
        source.push_str(&")".repeat(open));
        source
    }
}

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn events(&mut self, segment: Arc<Segment>, rows: &mut Vec<usize>) -> Arc<Segment> {
        let mut last = segment;
        let mut events: Vec<Event> = last.events.clone();
        while let End::Branch { block, .. } = &last.end {
            if !self.quiet(*block) {
                break;
            }
            let next = self.plan.child(self.artifact, &last, Arc::from([]));
            events.extend_from_slice(&next.events);
            last = next;
        }
        let mut seen = Some(0);
        self.prune(rows, &mut seen);
        let typed = Exports::enabled();
        for item in self.schedule(&events, rows) {
            self.prune(rows, &mut seen);
            if rows.is_empty() {
                break;
            }
            match item.gate.and_then(|gate| self.gated(gate, rows)) {
                Some(mut gated) => {
                    let mut local = seen;
                    if !gated.is_empty() {
                        self.item(&events, &item, &mut gated, &mut local, typed);
                    }
                }
                None => self.item(&events, &item, rows, &mut seen, typed),
            }
        }
        self.prune(rows, &mut seen);
        last
    }

    fn prune(&self, rows: &mut Vec<usize>, seen: &mut Option<usize>) {
        if *seen != Some(self.fallback.fell()) {
            rows.retain(|&row| !self.fallback.fallen(row));
            *seen = Some(self.fallback.fell());
        }
    }

    fn item(
        &mut self,
        events: &[Event],
        item: &Item,
        rows: &mut Vec<usize>,
        seen: &mut Option<usize>,
        typed: bool,
    ) {
        let blocks: Vec<usize> = item.units.iter().map(|span| span.block).collect();
        if let (Some(_), true) = (item.domain, typed) {
            if let Some(entity) = self.regroup(item, rows) {
                let member = entity.is_some();
                let plan = self.plan;
                let fusions = &plan.fusions;
                let mut at = 0;
                let mut laning: Option<usize> = None;
                while at < item.units.len() {
                    self.prune(rows, seen);
                    if rows.is_empty() {
                        break;
                    }
                    let began = Census::on().then(|| (Instant::now(), rows.len()));
                    match self.typed(&item.units[at..], entity, rows) {
                        Typed::Ran(consumed, faulted) => {
                            if let Some((start, count)) = began {
                                Census::record(
                                    "policy typed | group".into(),
                                    count,
                                    start.elapsed().as_nanos(),
                                );
                            }
                            if let Some(from) = laning.take() {
                                fusions
                                    .runs
                                    .ran(Chain::of(&blocks[from..], member), at - from);
                            }
                            self.counts.typed += consumed;
                            if !faulted.is_empty() {
                                self.retry(&events, &item.units[at..at + consumed], faulted);
                            }
                            at += consumed;
                            continue;
                        }
                        Typed::Lane(units) => {
                            if let Some(from) = laning.take() {
                                fusions
                                    .runs
                                    .ran(Chain::of(&blocks[from..], member), at - from);
                            }
                            for span in &item.units[at..at + units] {
                                for event in &events[span.start..span.start + span.len] {
                                    self.prune(rows, seen);
                                    if rows.is_empty() {
                                        break;
                                    }
                                    self.event(*event, rows);
                                }
                            }
                            self.counts.laned += units;
                            at += units;
                            continue;
                        }
                        Typed::Skipped => {}
                    }
                    let census =
                        Census::on().then(|| (Census::taken(), Instant::now(), rows.len()));
                    if laning.is_none() {
                        let run = fusions
                            .runs
                            .run(&Chain::of(&blocks[at..], member))
                            .map_or(0, |run| run.min(blocks.len() - at));
                        if let Some(fused) = (run > 1)
                            .then(|| fusions.get(plan, &blocks[at..at + run], member))
                            .flatten()
                        {
                            self.fused(&fused, entity, rows);
                            self.counts.laned += run;
                            at += run;
                            if let Some((why, start, count)) = census {
                                Census::record(
                                    format!("policy lane | fused | {why}"),
                                    count,
                                    start.elapsed().as_nanos(),
                                );
                            }
                            continue;
                        }
                        laning = Some(at);
                    }
                    let span = item.units[at];
                    for event in &events[span.start..span.start + span.len] {
                        self.prune(rows, seen);
                        if rows.is_empty() {
                            break;
                        }
                        self.event(*event, rows);
                    }
                    if let Some((why, start, count)) = census {
                        Census::record(
                            format!("policy lane | unit | {why}"),
                            count,
                            start.elapsed().as_nanos(),
                        );
                    }
                    self.counts.laned += 1;
                    at += 1;
                }
                if let (Some(from), true) = (laning, at == item.units.len()) {
                    fusions
                        .runs
                        .ran(Chain::of(&blocks[from..], member), at - from);
                }
                return;
            }
        }
        self.counts.laned += item.units.len();
        let census = Census::on().then(|| (Instant::now(), rows.len(), item.domain.is_some()));
        let group = match (item.domain, blocks.len() > 1) {
            (Some(domain), true) => self.regroup(item, rows).and_then(|entity| {
                let fused = self
                    .plan
                    .fusions
                    .get(self.plan, &blocks, domain.is_some())?;
                Some((fused, entity))
            }),
            _ => None,
        };
        match group {
            Some((fused, entity)) => self.fused(&fused, entity, rows),
            None => {
                for span in &item.units {
                    for event in &events[span.start..span.start + span.len] {
                        self.prune(rows, seen);
                        if rows.is_empty() {
                            break;
                        }
                        self.event(*event, rows);
                    }
                }
            }
        }
        if let Some((start, count, domain)) = census {
            Census::record(
                format!("policy lane | whole item | domain {domain} typed {typed}"),
                count,
                start.elapsed().as_nanos(),
            );
        }
    }

    fn retry(&mut self, events: &[Event], units: &[Span], mut rows: Vec<usize>) {
        for span in units {
            for event in &events[span.start..span.start + span.len] {
                rows.retain(|&row| !self.fallback.fallen(row));
                if rows.is_empty() {
                    return;
                }
                self.event(*event, &rows);
            }
        }
    }

    fn regroup(&self, item: &Item, rows: &[usize]) -> Option<Option<usize>> {
        let mut entity = None;
        for span in &item.units {
            let unit = self.refused(span.block, span.len, rows).ok()?;
            if entity.is_some_and(|e| e != unit.entity) {
                return None;
            }
            entity = Some(unit.entity);
        }
        entity
    }

    fn schedule(&self, events: &[Event], rows: &[usize]) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        let mut at = 0;
        while at < events.len() {
            let (block, len) = match events[at..] {
                [Event::Select(a), Event::Commit(b), ..] if a == b => (a, 2),
                [Event::Select(b), ..] | [Event::Commit(b), ..] => (b, 1),
                [] => break,
            };
            let unit = match rows.is_empty() {
                true => None,
                false => self.unit(block, len, rows),
            };
            let merged = unit.as_ref().is_some_and(|unit| {
                self.merge(
                    &mut items,
                    Span {
                        block,
                        start: at,
                        len,
                    },
                    unit,
                )
            });
            if !merged {
                items.push(Item {
                    units: vec![Span {
                        block,
                        start: at,
                        len,
                    }],
                    gate: self.plan.gate(block),
                    domain: unit.as_ref().map(|unit| unit.entity),
                    written: unit.map(|unit| unit.writes).unwrap_or_default(),
                });
            }
            at += len;
        }
        items
    }

    fn merge(&self, items: &mut [Item], entry: Span, unit: &Unit) -> bool {
        let Some(target) = items
            .iter()
            .rposition(|item| item.domain == Some(unit.entity) && item.gate == self.plan.gate(entry.block))
        else {
            return false;
        };
        let effects = |block: usize| self.plan.fusions.effects(self.plan, block);
        if target + 1 < items.len() {
            let Some(own) = effects(entry.block) else {
                return false;
            };
            let free = items[target + 1..]
                .iter()
                .flat_map(|item| item.units.iter())
                .all(|other| effects(other.block).is_some_and(|theirs| own.commute(&theirs)));
            if !free {
                return false;
            }
        }
        let item = &mut items[target];
        let reads = unit
            .programs
            .iter()
            .flat_map(|p| p.program().site_keys.iter().flatten());
        let blocked = reads.clone().any(|key| {
            item.written
                .iter()
                .any(|(w, readable)| match Sheet::related(key, w) {
                    None => false,
                    Some(Ordering::Equal | Ordering::Greater) => !readable,
                    Some(Ordering::Less) => true,
                })
        });
        let nested = unit.writes.iter().any(|(key, _)| {
            item.written.iter().any(|(w, _)| {
                matches!(
                    Sheet::related(key, w),
                    Some(Ordering::Less | Ordering::Greater)
                )
            })
        });
        if blocked || nested {
            return false;
        }
        item.units.push(entry);
        item.written.extend(unit.writes.iter().cloned());
        true
    }

    fn unit(&self, block: usize, len: usize, rows: &[usize]) -> Option<Unit<'s>> {
        match self.refused(block, len, rows) {
            Ok(unit) => Some(unit),
            Err(why) => {
                if Census::on() {
                    Census::record(format!("policy unit refused | {why}"), rows.len(), 0);
                }
                None
            }
        }
    }

    fn refused(&self, block: usize, len: usize, rows: &[usize]) -> Result<Unit<'s>, &'static str> {
        let plan = self.plan;
        if self.shared {
            return Err("shared rows");
        }
        let (entity, programs, writes) =
            match (&plan.flat(block).table, &plan.rule(block).kind) {
                (Some(table), BlockKind::DecisionTable(ir)) => {
                    let entity = match self.owners[block] {
                        Some(_) if Exports::enabled() => {
                            let at = self.entity_of(block).ok_or("member table without entity")?;
                            self.supported(at, block, rows)
                                .then_some(Some(at))
                                .ok_or("member table unsupported")?
                        }
                        Some(_) => return Err("member table untyped"),
                        None => None,
                    };
                    if len != 2 {
                        return Err("table split select");
                    }
                    if !self.quiet(block) {
                        return Err("table cells read fields");
                    }
                    let readable = match plan.fusions.table(block, ir) {
                        Some(false) if !Exports::enabled() => return Err("table not typable"),
                        Some(readable) => readable,
                        None => return Err("table not typable"),
                    };
                    let writes: Vec<(Arc<str>, bool)> = table
                        .picked()
                        .iter()
                        .map(|o| (o.field.clone(), readable))
                        .collect();
                    (entity, table.programs(), writes)
                }
                (Some(_), _) => return Err("table plan on other block"),
                (None, _) => {
                    let entity = match self.owners[block] {
                        Some(_) => {
                            let at = self.entity_of(block).ok_or("member block without entity")?;
                            self.supported(at, block, rows)
                                .then_some(Some(at))
                                .ok_or("member block unsupported")?
                        }
                        None => None,
                    };
                    let native = match entity {
                        Some(_) => Pass::native(plan, block).ok_or("member block not native")?,
                        None if plan.flat(block).iterated => return Err("iterated block"),
                        None => plan.flat(block).native().ok_or("block not native")?,
                    };
                    match (native, len) {
                        (Native::Expression { key, program }, 1) => {
                            (entity, vec![program.as_ref()], vec![(key.clone(), true)])
                        }
                        (Native::Match { ir, .. }, 2) if self.quiet(block) => {
                            (entity, Pass::programs(native), vec![(ir.key.clone(), true)])
                        }
                        (Native::Match { .. }, 2) => return Err("match arm values read fields"),
                        (Native::Assertion { ir, conditions }, 1) => (
                            entity,
                            conditions.iter().collect(),
                            vec![(ir.output.clone(), false)],
                        ),
                        _ => return Err("split select and commit"),
                    }
                }
            };
        let plain = programs.iter().all(|program| {
            let p = program.program();
            !(p.writes_env || p.chain || p.opaque() || p.rows)
        });
        if !plain {
            return Err("program needs row scope");
        }
        let clear = entity.is_some()
            || writes.iter().all(|(key, _)| {
                !self
                    .sheet
                    .entities
                    .iter()
                    .any(|e| e.active && Sheet::related(key, &e.path).is_some())
            });
        match clear {
            true => Ok(Unit {
                entity,
                programs,
                writes,
            }),
            false => Err("writes into a list entity"),
        }
    }

    fn conjunction(ir: &AssertionIr) -> bool {
        let last = ir.conditions.len().saturating_sub(1);
        !ir.conditions.is_empty()
            && ir.conditions.iter().enumerate().all(|(at, c)| {
                c.depth == 0 && (at == last || Operator::of(&c.operator) == Operator::And)
            })
    }

    fn fused(&mut self, fused: &Fused<Sink>, entity: Option<usize>, rows: &[usize]) {
        match entity {
            None => {
                let (leaves, failed) = self.run_outs(&fused.program, rows);
                self.fail(rows, &failed);
                self.sink(&fused.sinks, leaves, None, rows);
            }
            Some(at) => {
                let (kids, parents) = self.kids(at, rows);
                let (leaves, failed) = self.member_outs(at, &fused.program, &kids, &parents);
                self.member_fail(&parents, &failed);
                self.sink(&fused.sinks, leaves, Some((at, &kids)), rows);
            }
        }
    }

    pub(super) fn sink(
        &mut self,
        sinks: &[Sink],
        leaves: Vec<Leaf<'a>>,
        member: Option<(usize, &Rc<[usize]>)>,
        rows: &[usize],
    ) {
        let count = member.map_or(rows.len(), |(_, kids)| kids.len());
        let mut leaves = leaves.into_iter();
        for sink in sinks {
            if let Sink::Gather(gather) = sink {
                let conds: Vec<Vec<u64>> = leaves
                    .by_ref()
                    .take(gather.rules.len())
                    .map(|leaf| leaf.truths(count))
                    .collect();
                let values: Vec<Leaf<'a>> = leaves.by_ref().take(gather.computed).collect();
                for (key, leaf) in gather.fields.iter().zip(gather.leaves(&conds, &values, count)) {
                    match member {
                        None => self.write(key, leaf, rows),
                        Some((at, kids)) => self.member_write(at, key, leaf, kids),
                    }
                }
                continue;
            }
            let leaf = match sink {
                Sink::Gather(_) => continue,
                Sink::Write(_) => leaves.next().unwrap_or_else(|| Leaf::nulls(count)),
                Sink::Assert(ir) if member.is_none() && Self::conjunction(ir) => {
                    let mut bits = Bits::ones(count);
                    for leaf in leaves.by_ref().take(ir.conditions.len()) {
                        bits.iter_mut()
                            .zip(leaf.truths(count))
                            .for_each(|(b, t)| *b &= t);
                    }
                    Leaf::typed(Array::parts(Store::Bool(bits), None, count))
                }
                Sink::Assert(ir) => {
                    let truths: Vec<Vec<u64>> = leaves
                        .by_ref()
                        .take(ir.conditions.len())
                        .map(|leaf| leaf.truths(count))
                        .collect();
                    let bits = Self::folded(ir, &truths, count);
                    Leaf::typed(Array::parts(Store::Bool(bits), None, count))
                }
            };
            let key = match sink {
                Sink::Write(key) => key,
                Sink::Assert(ir) => &ir.output,
                Sink::Gather(_) => continue,
            };
            match member {
                None => self.write(key, leaf, rows),
                Some((at, kids)) => self.member_write(at, key, leaf, kids),
            }
        }
    }
}
