use super::fuse::{Fused, Fusions, Sink};
use super::{Bind, Member, Pass, Sheet};
use crate::compiled::core::switch::{Density, Path, Switch};
use crate::compiled::core::variants::Variants;
use crate::compiled::lowered::Exports;
use crate::compiled::shred::{Listing, Normalizer};
use crate::compiled::leaf::{Bits, Leaf};
use crate::policy::blocks::BlockKind;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::typed::{Batch, List, Lowering, Outputs, Program, Resolve, Resolved};

pub(crate) struct TypedGroup {
    program: Option<Program>,
    sinks: Vec<Sink>,
    units: usize,
    probe: Box<[Probed]>,
    columns: Vec<usize>,
    lists: Vec<Option<usize>>,
    fields: Vec<usize>,
    exports: Exports,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Path,
    Alias,
    Nested,
}

struct Probed {
    ask: Ask,
    path: Arc<str>,
    answer: Resolved,
}

enum Source {
    Entry(usize),
    Field(usize),
    List(usize, Vec<usize>),
    Listed(usize),
    Absent,
}

enum Part<'a> {
    Entity(Rc<[i32]>, Option<Rc<[u64]>>, Vec<Leaf<'a>>),
    Listed(Rc<Listing<'a>>),
}

struct Probe<'p, 's, 'a> {
    pass: &'p Pass<'s, 'a>,
    entity: Option<usize>,
    rows: &'p [usize],
    record: Vec<Probed>,
}

impl Resolve for Probe<'_, '_, '_> {
    fn path(&mut self, path: &str) -> Resolved {
        self.ask(Ask::Path, path)
    }

    fn alias(&mut self, field: &str) -> Resolved {
        self.ask(Ask::Alias, field)
    }

    fn nested(&mut self, chain: &[&str]) -> Resolved {
        self.ask(Ask::Nested, &chain.join("\n"))
    }
}

impl Probe<'_, '_, '_> {
    fn ask(&mut self, ask: Ask, path: &str) -> Resolved {
        let (answer, _) = self.pass.locate(self.entity, self.rows, path, ask);
        if !self.record.iter().any(|p| p.ask == ask && p.path.as_ref() == path) {
            self.record.push(Probed {
                ask,
                path: Arc::from(path),
                answer: answer.clone(),
            });
        }
        answer
    }
}

impl TypedGroup {
    fn lower(pass: &Pass, units: &[(usize, usize, usize)], entity: Option<usize>, rows: &[usize]) -> Self {
        let mut limit = units.len();
        loop {
            let mut probe = Probe {
                pass,
                entity,
                rows,
                record: Vec::new(),
            };
            let member = entity.map(|at| &pass.sheet.entities[at]).map(|e| (e.name.as_ref(), e.path.as_ref()));
            let mut lowering = Lowering::new(member);
            let mut sinks = Vec::new();
            for &(block, _, _) in &units[..limit] {
                let lowered = match &pass.plan.blocks.rules[block].kind {
                    BlockKind::DecisionTable(ir) => Fused::spec(ir).map(|spec| {
                        let sunk: Vec<Sink> = spec.outputs.iter().map(|key| Sink::Write(Arc::from(key.as_str()))).collect();
                        (lowering.table(&spec, &mut probe), sunk)
                    }),
                    _ => Fused::entries(pass.plan, block, entity.is_some()).map(|(entries, sunk)| (lowering.unit(&entries, &mut probe), sunk)),
                };
                match lowered {
                    Some((Ok(()), sunk)) => sinks.extend(sunk),
                    _ => break,
                }
            }
            let consumed = lowering.units();
            let record = probe.record;
            if consumed == 0 {
                return Self::empty(record);
            }
            let built = match Program::new(lowering.finish()) {
                Ok(program) => Self::new(program, sinks, consumed, record, entity.is_some()),
                Err(_) => Err(record),
            };
            match built {
                Ok(group) => return group,
                Err(record) if consumed == 1 => return Self::empty(record),
                Err(_) => limit = consumed - 1,
            }
        }
    }

    fn empty(record: Vec<Probed>) -> Self {
        Self {
            program: None,
            sinks: Vec::new(),
            units: 0,
            probe: record.into(),
            columns: Vec::new(),
            lists: Vec::new(),
            fields: Vec::new(),
            exports: Exports::of(&Default::default()),
        }
    }

    fn new(program: Program, sinks: Vec<Sink>, units: usize, record: Vec<Probed>, member: bool) -> Result<Self, Vec<Probed>> {
        let hir = program.hir();
        let schema = &hir.schema;
        let find = |ask: Ask, path: &str| record.iter().position(|p| p.ask == ask && p.path.as_ref() == path);
        let columns: Option<Vec<usize>> = schema.columns.iter().map(|c| find(Ask::Path, &c.path)).collect();
        let lists: Option<Vec<Option<usize>>> = schema
            .lists
            .iter()
            .enumerate()
            .map(|(at, list)| match (member && at == 0, list.parent) {
                (true, _) => Some(None),
                (false, None) => find(Ask::Path, &list.path).map(Some),
                (false, Some(_)) => find(Ask::Nested, &schema.chain(at as u16).join("\n")).map(Some),
            })
            .collect();
        let fields: Option<Vec<usize>> = match (member, schema.lists.first()) {
            (true, Some(list)) => list.fields.iter().map(|f| find(Ask::Alias, &f.path)).collect(),
            (true, None) => None,
            (false, _) => Some(Vec::new()),
        };
        let (Some(columns), Some(lists), Some(fields)) = (columns, lists, fields) else {
            return Err(record);
        };
        let exports = Exports::of(hir);
        Ok(Self {
            program: Some(program),
            sinks,
            units,
            probe: record.into(),
            columns,
            lists,
            fields,
            exports,
        })
    }

    fn replay(&self, pass: &Pass, entity: Option<usize>, rows: &[usize]) -> Option<Vec<Source>> {
        self.probe
            .iter()
            .map(|probed| {
                let (answer, source) = pass.locate(entity, rows, &probed.path, probed.ask);
                (answer == probed.answer).then_some(source)
            })
            .collect()
    }
}

impl Fusions {
    const VARIANTS: usize = 8;

    fn group(&self, pass: &Pass, units: &[(usize, usize, usize)], entity: Option<usize>, rows: &[usize]) -> Option<(Arc<TypedGroup>, Vec<Source>)> {
        let key = (units.iter().map(|(block, _, _)| *block).collect::<Vec<usize>>(), entity.is_some());
        let found = self
            .typed
            .read()
            .ok()
            .and_then(|cache| cache.get(&key)?.find_map(|group| Some((group.clone(), group.replay(pass, entity, rows)?))));
        if found.is_some() {
            return found;
        }
        let group = Arc::new(TypedGroup::lower(pass, units, entity, rows));
        let sources = group.replay(pass, entity, rows)?;
        if let Ok(mut cache) = self.typed.write() {
            cache.entry(key).or_insert_with(|| Variants::new(Self::VARIANTS)).keep(group.clone());
        }
        Some((group, sources))
    }
}

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn typed(&mut self, units: &[(usize, usize, usize)], entity: Option<usize>, rows: &[usize]) -> (usize, Vec<usize>) {
        let total = self.sheet.rows;
        let full = rows.len() == total;
        if Density::sparse(rows.len(), total) || Switch::Always.path().0 == Path::Lane {
            return (0, Vec::new());
        }
        let plan = self.plan;
        self.listed.borrow_mut().retain(|(_, stable, _)| *stable);
        let Some((group, sources)) = plan.fusions.group(self, units, entity, rows) else {
            return (0, Vec::new());
        };
        let Some(program) = group.program.as_ref() else {
            return (0, Vec::new());
        };
        let span = match entity {
            Some(at) => match self.span(at) {
                Some(span) => Some((at, span)),
                None => return (0, Vec::new()),
            },
            None => None,
        };
        let select = (!full).then(|| {
            let mut bits = vec![0u64; total.div_ceil(64)];
            rows.iter().for_each(|&row| Bits::set(&mut bits, row, true));
            bits
        });
        let Some(mut out) = self.execute(&group, program, &sources, entity, select.as_deref()) else {
            return (0, Vec::new());
        };
        let faulted: Vec<usize> = match out.faults().any() {
            true => rows.iter().copied().filter(|&row| out.faults().get(row)).collect(),
            false => Vec::new(),
        };
        let kept: Vec<usize>;
        let rows = match faulted.is_empty() {
            true => rows,
            false => {
                kept = rows.iter().copied().filter(|&row| !out.faults().get(row)).collect();
                kept.as_slice()
            }
        };
        if rows.is_empty() {
            return (group.units, faulted);
        }
        let full = rows.len() == total;
        let leaves: Vec<Leaf<'a>> = group.exports.leaves(&mut out);
        match span {
            None => {
                let leaves = match full {
                    true => leaves,
                    false => {
                        let picks: Rc<[usize]> = rows.into();
                        leaves.iter().map(|leaf| leaf.pick(&picks)).collect()
                    }
                };
                self.sink(&group.sinks, leaves, None, rows);
            }
            Some((at, (base, end))) => {
                let (kids, leaves) = match full {
                    true => ((base..end).collect::<Rc<[usize]>>(), leaves),
                    false => {
                        let (kids, _) = self.gather(at, rows);
                        let picks: Rc<[usize]> = kids.iter().map(|&kid| kid - base).collect();
                        (kids, leaves.iter().map(|leaf| leaf.pick(&picks)).collect())
                    }
                };
                self.sink(&group.sinks, leaves, Some((at, &kids)), rows);
            }
        }
        (group.units, faulted)
    }

    fn span(&self, at: usize) -> Option<(usize, usize)> {
        let entity = &self.sheet.entities[at];
        let offsets = entity.offsets.get(..=self.sheet.rows)?;
        let base = usize::try_from(*offsets.first()?).ok()?;
        let end = usize::try_from(*offsets.last()?).ok()?;
        let sorted = offsets.windows(2).all(|pair| pair[0] <= pair[1]);
        (sorted && base <= end && end <= entity.children).then_some((base, end))
    }

    fn execute(&self, group: &TypedGroup, program: &Program, sources: &[Source], entity: Option<usize>, select: Option<&[u64]>) -> Option<Outputs> {
        let leaf = |at: usize| -> Option<Leaf<'a>> {
            match sources.get(at)? {
                Source::Entry(entry) => Some(self.sheet.entries[*entry].leaf.clone()),
                Source::Field(field) => Some(self.sheet.entities[entity?].view(*field)),
                Source::List(..) | Source::Listed(_) | Source::Absent => None,
            }
        };
        let columns: Vec<Leaf<'a>> = group.columns.iter().map(|&at| leaf(at)).collect::<Option<_>>()?;
        let mut parts: Vec<Part<'a>> = Vec::with_capacity(group.lists.len());
        for list in &group.lists {
            parts.push(match list {
                None => {
                    let owner = &self.sheet.entities[entity?];
                    let fields = group.fields.iter().map(|&at| leaf(at)).collect::<Option<_>>()?;
                    Part::Entity(owner.offsets.clone(), owner.validity.clone(), fields)
                }
                Some(at) => match sources.get(*at)? {
                    Source::List(owner, fields) => {
                        let owner = &self.sheet.entities[*owner];
                        Part::Entity(owner.offsets.clone(), owner.validity.clone(), fields.iter().map(|&f| owner.view(f)).collect())
                    }
                    Source::Listed(index) => Part::Listed(self.listed.borrow().get(*index)?.2.clone()?),
                    _ => return None,
                },
            });
        }
        let mut batch = Batch::new(self.sheet.rows);
        columns.iter().for_each(|leaf| batch.push_column(leaf.column()));
        for (part, def) in parts.iter().zip(&program.hir().schema.lists) {
            batch.push_list(match part {
                Part::Entity(offsets, validity, fields) => List::new(offsets, validity.as_deref().map(|bits| (bits, 0)), fields.iter().map(Leaf::column).collect()),
                Part::Listed(listing) => {
                    let list = List::new(&listing.offsets, listing.validity.as_deref().map(|bits| (bits, 0)), listing.columns(&def.fields)?);
                    match listing.hosted.as_deref() {
                        Some(bits) => list.faults(bits, 0),
                        None => list,
                    }
                }
            });
        }
        let mut out = program.outputs();
        program.run(&batch, &mut out, select).ok()?;
        Some(out)
    }

    fn listing(&self, key: String, stable: bool, build: impl FnOnce() -> Option<Listing<'a>>) -> Option<(usize, Rc<Listing<'a>>)> {
        if let Some(at) = self.listed.borrow().iter().position(|(k, _, _)| k.as_ref() == key) {
            return self.listed.borrow()[at].2.clone().map(|listing| (at, listing));
        }
        let built = build().map(Rc::new);
        let mut listed = self.listed.borrow_mut();
        listed.push((Rc::from(key), stable, built.clone()));
        built.map(|listing| (listed.len() - 1, listing))
    }

    fn listed_entry(&self, entry: usize) -> (Resolved, Source) {
        let leaf = &self.sheet.entries[entry].leaf;
        match self.listing(format!("#{entry}"), true, || Listing::leaf(leaf, self.sheet.rows)) {
            Some((at, listing)) => (Resolved::List { fields: listing.classes() }, Source::Listed(at)),
            None => (Resolved::Unsupported, Source::Absent),
        }
    }

    fn bound(&self, entry: usize) -> (Resolved, Source) {
        match Normalizer::class(&self.sheet.entries[entry].leaf.column().values) {
            Some(class) => (Resolved::Column(class), Source::Entry(entry)),
            None => self.listed_entry(entry),
        }
    }

    fn nested_list(&self, entity: Option<usize>, rows: &[usize], chain: &[&str]) -> Option<(usize, Rc<Listing<'a>>)> {
        let (root, rest) = chain.split_first()?;
        let (first, rest) = rest.split_first()?;
        let entities = &self.sheet.entities;
        let owner = match entity {
            Some(at) => (entities[at].path.as_ref() == *root).then_some(at),
            None => entities.iter().position(|e| e.active && e.path.as_ref() == *root),
        };
        let (mut key, stable, mut current) = match owner {
            Some(at) => {
                let owner = &entities[at];
                if owner.cached() || !owner.flat() {
                    return None;
                }
                let f = owner.field(first)?;
                let key = format!("@{at}:{f}");
                let (_, current) = self.listing(key.clone(), false, || Listing::leaf(&owner.view(f), owner.children))?;
                (key, false, current)
            }
            None => {
                let entry = match entity {
                    None => match self.sheet.bind(root, rows, false) {
                        Bind::Column(entry) => entry,
                        _ => return None,
                    },
                    Some(at) => match self.site(at, Some(root), rows) {
                        Member::Root(entry) => entry,
                        _ => return None,
                    },
                };
                let (_, base) = self.listing(format!("#{entry}"), true, || Listing::leaf(&self.sheet.entries[entry].leaf, self.sheet.rows))?;
                let key = format!("#{entry}\n{first}");
                let (_, current) = self.listing(key.clone(), true, || base.nested(first))?;
                (key, true, current)
            }
        };
        let mut at = self.listed.borrow().iter().position(|(k, _, _)| k.as_ref() == key)?;
        for field in rest {
            key.push('\n');
            key.push_str(field);
            let parent = current.clone();
            let (next, listing) = self.listing(key.clone(), stable, || parent.nested(field))?;
            at = next;
            current = listing;
        }
        Some((at, current))
    }

    fn locate(&self, entity: Option<usize>, rows: &[usize], key: &str, ask: Ask) -> (Resolved, Source) {
        let unsupported = (Resolved::Unsupported, Source::Absent);
        let column = |leaf: &Leaf, source: Source| match Normalizer::class(&leaf.column().values) {
            Some(class) => (Resolved::Column(class), source),
            None => (Resolved::Unsupported, Source::Absent),
        };
        match (entity, ask) {
            (_, Ask::Nested) => {
                let chain: Vec<&str> = key.split('\n').collect();
                match self.nested_list(entity, rows, &chain) {
                    Some((at, listing)) => (Resolved::List { fields: listing.classes() }, Source::Listed(at)),
                    None => unsupported,
                }
            }
            (Some(at), Ask::Alias) => {
                let owner = &self.sheet.entities[at];
                match self.site(at, Some(&format!("{}.{key}", owner.name)), rows) {
                    Member::Field(field) => column(&owner.view(field), Source::Field(field)),
                    Member::Absent => (Resolved::Absent, Source::Absent),
                    _ => unsupported,
                }
            }
            (Some(at), Ask::Path) => match self.site(at, Some(key), rows) {
                Member::Root(entry) => self.bound(entry),
                Member::Absent => (Resolved::Absent, Source::Absent),
                _ => unsupported,
            },
            (None, Ask::Alias) => unsupported,
            (None, Ask::Path) => {
                let entities = &self.sheet.entities;
                if let Some(at) = entities.iter().position(|e| e.active && e.path.as_ref() == key) {
                    let owner = &entities[at];
                    if owner.cached() || !owner.flat() {
                        return unsupported;
                    }
                    let mut fields = Vec::new();
                    let mut index = Vec::new();
                    for (f, field) in owner.fields.iter().enumerate() {
                        let class = Normalizer::class(&owner.view(f).column().values);
                        if let (Some(class), true) = (class, owner.field(&field.name) == Some(f)) {
                            fields.push((field.name.clone(), class));
                            index.push(f);
                        }
                    }
                    return (Resolved::List { fields }, Source::List(at, index));
                }
                if entities.iter().any(|e| e.active && Sheet::related(key, &e.path).is_some()) {
                    return unsupported;
                }
                match self.sheet.bind(key, rows, false) {
                    Bind::Column(entry) => self.bound(entry),
                    Bind::Absent => (Resolved::Absent, Source::Absent),
                    Bind::Row => unsupported,
                }
            }
        }
    }
}
