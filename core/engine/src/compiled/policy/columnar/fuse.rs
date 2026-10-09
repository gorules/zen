use super::{Pass, Sheet};
use crate::compiled::policy::schedule::{End, Event, Segment};
use crate::compiled::policy::{Native, PolicyPlan};
use crate::compiled::typed::{Array, Bits, Leaf, Store};
use crate::policy::blocks::{AssertionIr, BlockKind, ConditionOperator, DecisionTableIr, MatchIr};
use std::cmp::Ordering;
use std::rc::Rc;
use super::typed::TypedGroup;
use std::sync::{Arc, RwLock};
use crate::compiled::lowered::Exports;
use zen_expression::lane::{Fusion, LaneProgram};
use zen_expression::typed::{Hit, TableRule, TableSpec};

type Variants = ahash::HashMap<(Vec<usize>, bool), Vec<Arc<TypedGroup>>>;

#[derive(Default)]
pub(crate) struct Fusions {
    groups: RwLock<ahash::HashMap<Vec<usize>, Option<Arc<Fused>>>>,
    effects: RwLock<ahash::HashMap<usize, Option<Arc<Effects>>>>,
    tables: RwLock<ahash::HashMap<usize, bool>>,
    runs: RwLock<ahash::HashMap<(Vec<usize>, bool), usize>>,
    pub(super) typed: RwLock<Variants>,
}

pub(crate) struct Effects {
    reads: Vec<Arc<str>>,
    writes: Vec<Arc<str>>,
}

struct Item {
    units: Vec<(usize, usize, usize)>,
    domain: Option<Option<usize>>,
    written: Vec<(Arc<str>, bool)>,
}

pub(crate) struct Fused {
    program: LaneProgram,
    sinks: Vec<Sink>,
}

pub(super) enum Sink {
    Write(Arc<str>),
    Assert(Arc<AssertionIr>),
}

struct Unit<'p> {
    entity: Option<usize>,
    programs: Vec<&'p LaneProgram>,
    writes: Vec<(Arc<str>, bool)>,
}

impl Fusions {
    fn table(&self, block: usize, ir: &DecisionTableIr) -> bool {
        if let Some(found) = self.tables.read().ok().and_then(|cache| cache.get(&block).copied()) {
            return found;
        }
        let fusable = Fused::fusable(ir);
        if let Ok(mut cache) = self.tables.write() {
            cache.insert(block, fusable);
        }
        fusable
    }

    fn effects(&self, plan: &PolicyPlan, block: usize) -> Option<Arc<Effects>> {
        if let Some(found) = self.effects.read().ok().and_then(|cache| cache.get(&block).cloned()) {
            return found;
        }
        let built = Effects::of(plan, block).map(Arc::new);
        if let Ok(mut cache) = self.effects.write() {
            cache.insert(block, built.clone());
        }
        built
    }

    fn run(&self, blocks: &[usize], member: bool) -> Option<usize> {
        self.runs.read().ok().and_then(|cache| cache.get(&(blocks.to_vec(), member)).copied())
    }

    fn ran(&self, blocks: &[usize], member: bool, run: usize) {
        if self.run(blocks, member) == Some(run) {
            return;
        }
        if let Ok(mut cache) = self.runs.write() {
            cache.insert((blocks.to_vec(), member), run);
        }
    }

    fn get(&self, plan: &PolicyPlan, blocks: &[usize], member: bool) -> Option<Arc<Fused>> {
        if let Some(found) = self.groups.read().ok().and_then(|cache| cache.get(blocks).cloned()) {
            return found;
        }
        let built = Fused::compile(plan, blocks, member).map(Arc::new);
        match self.groups.write() {
            Ok(mut cache) => cache.entry(blocks.to_vec()).or_insert(built).clone(),
            Err(_) => built,
        }
    }
}

impl Effects {
    fn of(plan: &PolicyPlan, block: usize) -> Option<Self> {
        let (programs, _) = Pass::block(plan, block)?;
        let rule = &plan.blocks.rules[block];
        let iteration = plan.iterations[block].clone();
        if let Some((_, path, Some(owner))) = &iteration {
            if path.split('.').next() != Some(owner.as_ref()) {
                return None;
            }
        }
        let alias = iteration.map(|(name, path, _)| (name, path));
        let map = |key: &str| -> Arc<str> {
            match &alias {
                Some((name, path)) if key == name.as_ref() => path.clone(),
                Some((name, path)) => match key.strip_prefix(name.as_ref()).and_then(|rest| rest.strip_prefix('.')) {
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
                    Some(fields) => reads.extend(fields.iter().map(|field| Arc::from(format!("{key}.{field}")))),
                    None => reads.push(map(key)),
                }
            }
        }
        let writes = rule.kind.write_sites().into_iter().map(|site| map(&site.path)).collect();
        Some(Self { reads, writes })
    }

    fn commute(&self, other: &Effects) -> bool {
        let clash = |xs: &[Arc<str>], ys: &[Arc<str>]| xs.iter().any(|x| ys.iter().any(|y| Sheet::related(x, y).is_some()));
        !clash(&self.writes, &other.reads) && !clash(&self.writes, &other.writes) && !clash(&other.writes, &self.reads)
    }
}

impl Fused {
    const RULES: usize = 64;

    fn compile(plan: &PolicyPlan, blocks: &[usize], member: bool) -> Option<Self> {
        let mut entries: Vec<Fusion> = Vec::new();
        let mut sinks = Vec::with_capacity(blocks.len());
        for &block in blocks {
            if member && plan.tables[block].is_some() {
                return None;
            }
            let (unit, sunk) = Self::entries(plan, block, member)?;
            entries.extend(unit);
            sinks.extend(sunk);
        }
        let outputs = entries.iter().filter(|entry| !matches!(entry, Fusion::Hidden { .. })).count();
        let program = LaneProgram::compile_fused(&entries).ok()?;
        let p = program.program();
        (!(p.writes_env || p.chain || p.opaque() || p.rows) && p.outputs.len() == outputs).then_some(Self { program, sinks })
    }

    pub(super) fn entries(plan: &PolicyPlan, block: usize, member: bool) -> Option<(Vec<Fusion>, Vec<Sink>)> {
        let mut entries: Vec<Fusion> = Vec::new();
        let mut sinks = Vec::new();
        if let BlockKind::DecisionTable(ir) = &plan.blocks.rules[block].kind {
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
            false => plan.natives[block].as_ref()?,
        };
        match native {
            Native::Expression { key, program } => {
                entries.push(Fusion::Output { key: key.to_string(), source: program.source()?.to_string() });
                sinks.push(Sink::Write(key.clone()));
            }
            Native::Match { ir, .. } => {
                entries.push(Fusion::Output { key: ir.key.to_string(), source: Self::ternary(ir) });
                sinks.push(Sink::Write(ir.key.clone()));
            }
            Native::Assertion { ir, .. } => {
                entries.extend(ir.conditions.iter().map(|c| Fusion::Output { key: String::new(), source: c.expression.to_string() }));
                sinks.push(Sink::Assert(ir.clone()));
            }
        }
        Some((entries, sinks))
    }

    fn fusable(ir: &DecisionTableIr) -> bool {
        let outputs: Vec<_> = ir.outputs.iter().filter(|o| !o.field.is_empty()).collect();
        let cell = |rule: &ahash::HashMap<Arc<str>, Arc<str>>, id: &Arc<str>| rule.get(id).is_some_and(|c| !c.is_empty());
        ir.rules.len() <= Self::RULES
            && !outputs.is_empty()
            && outputs.iter().all(|o| !o.collect && ir.rules.iter().all(|rule| cell(rule, &o.id)))
            && ir.inputs.iter().all(|input| input.field.as_deref().is_some_and(|f| !f.trim().is_empty()) || ir.rules.iter().all(|rule| !cell(rule, &input.id)))
    }

    pub(super) fn spec(ir: &DecisionTableIr) -> Option<TableSpec> {
        if !Self::fusable(ir) {
            return None;
        }
        let used = |id: &Arc<str>| ir.rules.iter().any(|rule| rule.get(id).is_some_and(|c| !c.is_empty()));
        let inputs: Vec<_> = ir.inputs.iter().filter(|input| used(&input.id)).collect();
        let outputs: Vec<_> = ir.outputs.iter().filter(|o| !o.field.is_empty()).collect();
        Some(TableSpec {
            inputs: inputs.iter().map(|input| input.field.as_deref().map(str::to_string)).collect(),
            outputs: outputs.iter().map(|o| o.field.to_string()).collect(),
            rules: ir
                .rules
                .iter()
                .map(|rule| TableRule {
                    cells: inputs.iter().map(|input| rule.get(&input.id).filter(|c| !c.is_empty()).map(|c| c.to_string())).collect(),
                    values: outputs.iter().map(|o| rule.get(&o.id).map(|c| c.to_string())).collect(),
                })
                .collect(),
            hit: Hit::Strict,
            selector: false,
        })
    }

    fn table(ir: &DecisionTableIr, block: usize) -> Option<(Vec<Fusion>, Vec<Fusion>)> {
        if !Self::fusable(ir) {
            return None;
        }
        let mut hidden = Vec::new();
        let mut fields: Vec<(String, &Arc<str>)> = Vec::new();
        for (index, input) in ir.inputs.iter().enumerate() {
            if ir.rules.iter().all(|rule| rule.get(&input.id).is_none_or(|c| c.is_empty())) {
                continue;
            }
            let source = input.field.as_deref()?;
            let plain = source.split('.').all(|segment| {
                let mut chars = segment.chars();
                chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            let key = match plain {
                true => source.to_string(),
                false => {
                    let key = format!("__fuse{block}_{index}");
                    hidden.push(Fusion::Hidden { key: key.clone(), source: source.to_string() });
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
                            .filter_map(|(key, id)| rule.get(*id).filter(|c| !c.is_empty()).map(|c| (key.clone(), c.to_string())))
                            .collect();
                        (cells, rule.get(&output.id).map(|c| c.to_string()).unwrap_or_default())
                    })
                    .collect();
                Fusion::Rules { key: output.field.to_string(), rules }
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
        let mut fresh = |pass: &mut Self, rows: &mut Vec<usize>| {
            if seen != Some(pass.fell) {
                rows.retain(|&row| !pass.fallback[row]);
                seen = Some(pass.fell);
            }
        };
        fresh(self, rows);
        let typed = Exports::enabled();
        for item in self.schedule(&events, rows) {
            fresh(self, rows);
            if rows.is_empty() {
                break;
            }
            let blocks: Vec<usize> = item.units.iter().map(|(block, _, _)| *block).collect();
            if let (Some(_), true) = (item.domain, typed) {
                if let Some(entity) = self.regroup(&item, rows) {
                    let member = entity.is_some();
                    let plan = self.plan;
                    let fusions = &plan.fusions;
                    let mut at = 0;
                    let mut laning: Option<usize> = None;
                    while at < item.units.len() {
                        fresh(self, rows);
                        if rows.is_empty() {
                            break;
                        }
                        let (consumed, faulted) = self.typed(&item.units[at..], entity, rows);
                        if consumed > 0 {
                            if let Some(from) = laning.take() {
                                fusions.ran(&blocks[from..], member, at - from);
                            }
                            self.counts.0 += consumed;
                            if !faulted.is_empty() {
                                self.retry(&events, &item.units[at..at + consumed], faulted);
                            }
                            at += consumed;
                            continue;
                        }
                        if laning.is_none() {
                            let run = fusions.run(&blocks[at..], member).map_or(0, |run| run.min(blocks.len() - at));
                            if let Some(fused) = (run > 1).then(|| fusions.get(plan, &blocks[at..at + run], member)).flatten() {
                                self.fused(&fused, entity, rows);
                                self.counts.1 += run;
                                at += run;
                                continue;
                            }
                            laning = Some(at);
                        }
                        let (_, start, len) = item.units[at];
                        for event in &events[start..start + len] {
                            fresh(self, rows);
                            if rows.is_empty() {
                                break;
                            }
                            self.event(*event, rows);
                        }
                        self.counts.1 += 1;
                        at += 1;
                    }
                    if let (Some(from), true) = (laning, at == item.units.len()) {
                        fusions.ran(&blocks[from..], member, at - from);
                    }
                    continue;
                }
            }
            self.counts.1 += item.units.len();
            let group = match (item.domain, blocks.len() > 1) {
                (Some(domain), true) => self.regroup(&item, rows).and_then(|entity| {
                    let fused = self.plan.fusions.get(self.plan, &blocks, domain.is_some())?;
                    Some((fused, entity))
                }),
                _ => None,
            };
            match group {
                Some((fused, entity)) => self.fused(&fused, entity, rows),
                None => {
                    for (_, start, len) in &item.units {
                        for event in &events[*start..start + len] {
                            fresh(self, rows);
                            if rows.is_empty() {
                                break;
                            }
                            self.event(*event, rows);
                        }
                    }
                }
            }
        }
        fresh(self, rows);
        last
    }

    fn retry(&mut self, events: &[Event], units: &[(usize, usize, usize)], mut rows: Vec<usize>) {
        for (_, start, len) in units {
            for event in &events[*start..start + len] {
                rows.retain(|&row| !self.fallback[row]);
                if rows.is_empty() {
                    return;
                }
                self.event(*event, &rows);
            }
        }
    }

    fn regroup(&self, item: &Item, rows: &[usize]) -> Option<Option<usize>> {
        let mut entity = None;
        for (block, _, len) in &item.units {
            let unit = self.unit(*block, *len, rows)?;
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
            let merged = unit.as_ref().is_some_and(|unit| self.merge(&mut items, (block, at, len), unit));
            if !merged {
                items.push(Item {
                    units: vec![(block, at, len)],
                    domain: unit.as_ref().map(|unit| unit.entity),
                    written: unit.map(|unit| unit.writes).unwrap_or_default(),
                });
            }
            at += len;
        }
        items
    }

    fn merge(&self, items: &mut [Item], entry: (usize, usize, usize), unit: &Unit) -> bool {
        let Some(target) = items.iter().rposition(|item| item.domain == Some(unit.entity)) else {
            return false;
        };
        let effects = |block: usize| self.plan.fusions.effects(self.plan, block);
        if target + 1 < items.len() {
            let Some(own) = effects(entry.0) else {
                return false;
            };
            let free = items[target + 1..]
                .iter()
                .flat_map(|item| item.units.iter())
                .all(|(other, _, _)| effects(*other).is_some_and(|theirs| own.commute(&theirs)));
            if !free {
                return false;
            }
        }
        let item = &mut items[target];
        let reads = unit.programs.iter().flat_map(|p| p.program().site_keys.iter().flatten());
        let blocked = reads.clone().any(|key| {
            item.written.iter().any(|(w, readable)| match Sheet::related(key, w) {
                None => false,
                Some(Ordering::Equal | Ordering::Greater) => !readable,
                Some(Ordering::Less) => true,
            })
        });
        let nested = unit
            .writes
            .iter()
            .any(|(key, _)| item.written.iter().any(|(w, _)| matches!(Sheet::related(key, w), Some(Ordering::Less | Ordering::Greater))));
        if blocked || nested {
            return false;
        }
        item.units.push(entry);
        item.written.extend(unit.writes.iter().cloned());
        true
    }

    fn unit(&self, block: usize, len: usize, rows: &[usize]) -> Option<Unit<'s>> {
        let plan = self.plan;
        if self.shared {
            return None;
        }
        let (entity, programs, writes) = match (&plan.tables[block], &plan.blocks.rules[block].kind) {
            (Some(table), BlockKind::DecisionTable(ir)) => {
                let entity = match self.owners[block] {
                    Some(_) if Exports::enabled() => {
                        let at = self.entity_of(block)?;
                        self.supported(at, block, rows).then_some(Some(at))?
                    }
                    Some(_) => return None,
                    None => None,
                };
                let ready = len == 2 && self.quiet(block) && plan.fusions.table(block, ir);
                let writes: Vec<(Arc<str>, bool)> = table.outputs.iter().map(|o| (o.field.clone(), true)).collect();
                ready.then_some((entity, table.programs(), writes))?
            }
            (Some(_), _) => return None,
            (None, _) => {
                let entity = match self.owners[block] {
                    Some(_) => {
                        let at = self.entity_of(block)?;
                        self.supported(at, block, rows).then_some(Some(at))?
                    }
                    None => None,
                };
                let native = match entity {
                    Some(_) => Pass::native(plan, block)?,
                    None if plan.iterated[block] => return None,
                    None => plan.natives[block].as_ref()?,
                };
                match (native, len) {
                    (Native::Expression { key, program }, 1) => (entity, vec![program.as_ref()], vec![(key.clone(), true)]),
                    (Native::Match { ir, .. }, 2) if self.quiet(block) => (entity, Pass::programs(native), vec![(ir.key.clone(), true)]),
                    (Native::Assertion { ir, conditions }, 1) => (entity, conditions.iter().collect(), vec![(ir.output.clone(), false)]),
                    _ => return None,
                }
            }
        };
        let plain = programs.iter().all(|program| {
            let p = program.program();
            !(p.writes_env || p.chain || p.opaque() || p.rows)
        });
        let clear = entity.is_some()
            || writes
                .iter()
                .all(|(key, _)| !self.sheet.entities.iter().any(|e| e.active && Sheet::related(key, &e.path).is_some()));
        (plain && clear).then_some(Unit { entity, programs, writes })
    }

    fn conjunction(ir: &AssertionIr) -> bool {
        let last = ir.conditions.len().saturating_sub(1);
        !ir.conditions.is_empty()
            && ir
                .conditions
                .iter()
                .enumerate()
                .all(|(at, c)| c.depth == 0 && (at == last || matches!(c.operator, ConditionOperator::And)))
    }

    fn fused(&mut self, fused: &Fused, entity: Option<usize>, rows: &[usize]) {
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

    pub(super) fn sink(&mut self, sinks: &[Sink], leaves: Vec<Leaf<'a>>, member: Option<(usize, &Rc<[usize]>)>, rows: &[usize]) {
        let count = member.map_or(rows.len(), |(_, kids)| kids.len());
        let mut leaves = leaves.into_iter();
        for sink in sinks {
            let leaf = match sink {
                Sink::Write(_) => leaves.next().unwrap_or_else(|| Leaf::nulls(count)),
                Sink::Assert(ir) if member.is_none() && Self::conjunction(ir) => {
                    let mut bits = Bits::ones(count);
                    for leaf in leaves.by_ref().take(ir.conditions.len()) {
                        bits.iter_mut().zip(leaf.truths(count)).for_each(|(b, t)| *b &= t);
                    }
                    Leaf::typed(Array::parts(Store::Bool(bits), None, count))
                }
                Sink::Assert(ir) => {
                    let truths: Vec<Vec<u64>> = leaves.by_ref().take(ir.conditions.len()).map(|leaf| leaf.truths(count)).collect();
                    let bits = Self::folded(ir, &truths, count);
                    Leaf::typed(Array::parts(Store::Bool(bits), None, count))
                }
            };
            let key = match sink {
                Sink::Write(key) => key,
                Sink::Assert(ir) => &ir.output,
            };
            match member {
                None => self.write(key, leaf, rows),
                Some((at, kids)) => self.member_write(at, key, leaf, kids),
            }
        }
    }
}
