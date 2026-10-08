use super::fuse::{Fused, Fusions, Sink};
use super::{Bind, Member, Pass, Sheet};
use crate::compiled::typed::{Array, Bits, Dict, Leaf, Store};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Dictionary, Values};
use zen_expression::typed::{Batch, Class, Data, List, Lowering, Outputs, Program, Resolve, Resolved, Slot};

pub(crate) struct TypedGroup {
    program: Option<Program>,
    sinks: Vec<Sink>,
    units: usize,
    probe: Box<[Probed]>,
    columns: Vec<usize>,
    lists: Vec<usize>,
    fields: Vec<usize>,
    dicts: Vec<(Vec<i32>, String)>,
}

struct Probed {
    alias: bool,
    path: Arc<str>,
    answer: Resolved,
}

enum Source {
    Entry(usize),
    Field(usize),
    List(usize, Vec<usize>),
    Absent,
}

struct Probe<'p, 's, 'a> {
    pass: &'p Pass<'s, 'a>,
    entity: Option<usize>,
    rows: &'p [usize],
    record: Vec<Probed>,
}

type Lists<'a> = Vec<(Rc<[i32]>, Option<Rc<[u64]>>, Vec<Leaf<'a>>)>;

impl Resolve for Probe<'_, '_, '_> {
    fn path(&mut self, path: &str) -> Resolved {
        self.ask(false, path)
    }

    fn alias(&mut self, field: &str) -> Resolved {
        self.ask(true, field)
    }
}

impl Probe<'_, '_, '_> {
    fn ask(&mut self, alias: bool, path: &str) -> Resolved {
        let (answer, _) = self.pass.locate(self.entity, self.rows, path, alias);
        if !self.record.iter().any(|p| p.alias == alias && p.path.as_ref() == path) {
            self.record.push(Probed {
                alias,
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
                let Some((entries, sunk)) = Fused::entries(pass.plan, block, entity.is_some()) else {
                    break;
                };
                if lowering.unit(&entries, &mut probe).is_err() {
                    break;
                }
                sinks.extend(sunk);
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
            dicts: Vec::new(),
        }
    }

    fn new(program: Program, sinks: Vec<Sink>, units: usize, record: Vec<Probed>, member: bool) -> Result<Self, Vec<Probed>> {
        let hir = program.hir();
        let find = |alias: bool, path: &str| record.iter().position(|p| p.alias == alias && p.path.as_ref() == path);
        let columns: Option<Vec<usize>> = hir.schema.columns.iter().map(|c| find(false, &c.path)).collect();
        let (lists, fields): (Option<Vec<usize>>, Option<Vec<usize>>) = match (member, hir.schema.lists.as_slice()) {
            (false, lists) => (lists.iter().map(|l| find(false, &l.path)).collect(), Some(Vec::new())),
            (true, [list]) => (Some(Vec::new()), list.fields.iter().map(|f| find(true, &f.path)).collect()),
            (true, _) => (None, None),
        };
        let (Some(columns), Some(lists), Some(fields)) = (columns, lists, fields) else {
            return Err(record);
        };
        let dicts = hir
            .schema
            .dicts
            .iter()
            .map(|dict| {
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for value in &dict.values {
                    data.push_str(value);
                    offsets.push(i32::try_from(data.len()).unwrap_or(i32::MAX));
                }
                (offsets, data)
            })
            .collect();
        Ok(Self {
            program: Some(program),
            sinks,
            units,
            probe: record.into(),
            columns,
            lists,
            fields,
            dicts,
        })
    }

    fn replay(&self, pass: &Pass, entity: Option<usize>, rows: &[usize]) -> Option<Vec<Source>> {
        self.probe
            .iter()
            .map(|probed| {
                let (answer, source) = pass.locate(entity, rows, &probed.path, probed.alias);
                (answer == probed.answer).then_some(source)
            })
            .collect()
    }
}

impl Fusions {
    const VARIANTS: usize = 8;

    fn group(&self, pass: &Pass, units: &[(usize, usize, usize)], entity: Option<usize>, rows: &[usize]) -> Option<(Arc<TypedGroup>, Vec<Source>)> {
        let key = (units.iter().map(|(block, _, _)| *block).collect::<Vec<usize>>(), entity.is_some());
        let found = self.typed.read().ok().and_then(|cache| {
            let variants = cache.get(&key)?;
            variants.iter().find_map(|group| Some((group.clone(), group.replay(pass, entity, rows)?)))
        });
        if found.is_some() {
            return found;
        }
        let group = Arc::new(TypedGroup::lower(pass, units, entity, rows));
        let sources = group.replay(pass, entity, rows)?;
        if let Ok(mut cache) = self.typed.write() {
            let variants = cache.entry(key).or_default();
            if variants.len() < Self::VARIANTS {
                variants.push(group.clone());
            }
        }
        Some((group, sources))
    }
}

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn typed(&mut self, units: &[(usize, usize, usize)], entity: Option<usize>, rows: &[usize]) -> usize {
        let total = self.sheet.rows;
        let full = rows.len() == total;
        if !full && rows.len() * 4 < total {
            return 0;
        }
        let plan = self.plan;
        let Some((group, sources)) = plan.fusions.group(self, units, entity, rows) else {
            return 0;
        };
        let Some(program) = group.program.as_ref() else {
            return 0;
        };
        let span = match entity {
            Some(at) => match self.span(at) {
                Some(span) => Some((at, span)),
                None => return 0,
            },
            None => None,
        };
        let select = (!full).then(|| {
            let mut bits = vec![0u64; total.div_ceil(64)];
            rows.iter().for_each(|&row| Bits::set(&mut bits, row, true));
            bits
        });
        let Some(mut out) = self.execute(&group, program, &sources, entity, select.as_deref()) else {
            return 0;
        };
        if out.faults().any() {
            for row in out.faults().iter() {
                self.fall(row);
            }
        }
        let mut dicts: Vec<Option<Rc<Dict>>> = vec![None; group.dicts.len()];
        let mut leaves: Vec<Leaf<'a>> = Vec::with_capacity(out.slots().len());
        for at in 0..out.slots().len() {
            if let Some(slot) = out.slot_mut(at) {
                leaves.push(Self::slotted(slot, &group, &mut dicts));
            }
        }
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
        group.units
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
                Source::List(..) | Source::Absent => None,
            }
        };
        let columns: Vec<Leaf<'a>> = group.columns.iter().map(|&at| leaf(at)).collect::<Option<_>>()?;
        let mut lists: Lists<'a> = Vec::with_capacity(group.lists.len().max(1));
        match entity {
            Some(at) => {
                let owner = &self.sheet.entities[at];
                let fields = group.fields.iter().map(|&at| leaf(at)).collect::<Option<_>>()?;
                lists.push((owner.offsets.clone(), owner.validity.clone(), fields));
            }
            None => {
                for &at in &group.lists {
                    let Some(Source::List(owner, fields)) = sources.get(at) else {
                        return None;
                    };
                    let owner = &self.sheet.entities[*owner];
                    lists.push((owner.offsets.clone(), owner.validity.clone(), fields.iter().map(|&f| owner.view(f)).collect()));
                }
            }
        }
        let mut batch = Batch::new(self.sheet.rows);
        columns.iter().for_each(|leaf| batch.push_column(leaf.column()));
        for (offsets, validity, fields) in &lists {
            batch.push_list(List::new(offsets, validity.as_deref().map(|bits| (bits, 0)), fields.iter().map(Leaf::column).collect()));
        }
        let mut out = program.outputs();
        program.run(&batch, &mut out, select).ok()?;
        Some(out)
    }

    fn slotted(slot: &mut Slot, group: &TypedGroup, dicts: &mut [Option<Rc<Dict>>]) -> Leaf<'a> {
        let len = slot.len;
        let dict = slot.dict();
        let array = match &slot.data {
            Data::Num { .. } => match slot.take_num() {
                Some((mut mant, mut scale, valid)) => {
                    let valid = Self::validity(valid, len);
                    if let Some(bits) = &valid {
                        for at in (0..len).filter(|&at| !Bits::get(bits, at)) {
                            mant[at] = 0;
                            scale[at] = 0;
                        }
                    }
                    Array::parts(Store::Scaled { mant, scale }, valid, len)
                }
                None => return Leaf::nulls(len),
            },
            Data::Bool(_) => match slot.take_bool() {
                Some((mut bits, valid)) => {
                    Bits::trim(&mut bits, len);
                    Array::parts(Store::Bool(bits), Self::validity(valid, len), len)
                }
                None => return Leaf::nulls(len),
            },
            Data::Code(_) => {
                let (Some((mut codes, valid)), Some(d)) = (slot.take_code(), dict) else {
                    return Leaf::nulls(len);
                };
                let Some(dict) = Self::dictionary(group, dicts, usize::from(d)) else {
                    return Leaf::nulls(len);
                };
                match Self::validity(valid, len) {
                    None => Array::coded(codes.into(), dict),
                    Some(bits) => {
                        for at in (0..len).filter(|&at| !Bits::get(&bits, at)) {
                            codes[at] = -1;
                        }
                        Array::coded_with(codes.into(), dict, bits)
                    }
                }
            }
        };
        Leaf::typed(array)
    }

    fn dictionary(group: &TypedGroup, dicts: &mut [Option<Rc<Dict>>], at: usize) -> Option<Rc<Dict>> {
        let slot = dicts.get_mut(at)?;
        if slot.is_none() {
            let (offsets, data) = group.dicts.get(at)?;
            *slot = Some(Rc::new(Dict::Text {
                offsets: offsets.clone(),
                data: data.clone(),
            }));
        }
        slot.clone()
    }

    fn validity(mut valid: Vec<u64>, len: usize) -> Option<Vec<u64>> {
        valid.resize(len.div_ceil(64), 0);
        Bits::trim(&mut valid, len);
        let full = valid.iter().map(|w| w.count_ones() as usize).sum::<usize>() == len;
        (!full).then_some(valid)
    }

    fn locate(&self, entity: Option<usize>, rows: &[usize], key: &str, alias: bool) -> (Resolved, Source) {
        let unsupported = (Resolved::Unsupported, Source::Absent);
        let column = |leaf: &Leaf, source: Source| match Self::class(&leaf.column().values) {
            Some(class) => (Resolved::Column(class), source),
            None => (Resolved::Unsupported, Source::Absent),
        };
        match (entity, alias) {
            (Some(at), true) => {
                let owner = &self.sheet.entities[at];
                match self.site(at, Some(&format!("{}.{key}", owner.name)), rows) {
                    Member::Field(field) => column(&owner.view(field), Source::Field(field)),
                    Member::Absent => (Resolved::Absent, Source::Absent),
                    _ => unsupported,
                }
            }
            (Some(at), false) => match self.site(at, Some(key), rows) {
                Member::Root(entry) => column(&self.sheet.entries[entry].leaf, Source::Entry(entry)),
                Member::Absent => (Resolved::Absent, Source::Absent),
                _ => unsupported,
            },
            (None, true) => unsupported,
            (None, false) => {
                let entities = &self.sheet.entities;
                if let Some(at) = entities.iter().position(|e| e.active && e.path.as_ref() == key) {
                    let owner = &entities[at];
                    if owner.cached() || !owner.flat() {
                        return unsupported;
                    }
                    let mut fields = Vec::new();
                    let mut index = Vec::new();
                    for (f, field) in owner.fields.iter().enumerate() {
                        let class = Self::class(&owner.view(f).column().values);
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
                    Bind::Column(entry) => column(&self.sheet.entries[entry].leaf, Source::Entry(entry)),
                    Bind::Absent => (Resolved::Absent, Source::Absent),
                    Bind::Row => unsupported,
                }
            }
        }
    }

    fn class(values: &Values) -> Option<Class> {
        match values {
            Values::Scaled { .. } | Values::I64(_) | Values::Dec(_) => Some(Class::Num),
            Values::Bool { .. } => Some(Class::Bool),
            Values::Text { .. } | Values::Utf8 { .. } => Some(Class::Text),
            Values::Dict { values, .. } => match values {
                Dictionary::Scaled { .. } => Some(Class::Num),
                Dictionary::Bool { .. } => Some(Class::Bool),
                Dictionary::Text { .. } => Some(Class::Text),
                Dictionary::Column(column) => match column.values {
                    Values::Scaled { .. } => Some(Class::Num),
                    Values::Bool { .. } => Some(Class::Bool),
                    Values::Text { .. } | Values::Utf8 { .. } => Some(Class::Text),
                    _ => None,
                },
                Dictionary::Any(_) => None,
            },
            _ => None,
        }
    }
}
