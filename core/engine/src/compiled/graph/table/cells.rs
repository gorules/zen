use crate::compiled::graph::bound::{Bound, Outs};
use crate::compiled::graph::data::Mask;
use crate::compiled::leaf::{Array, Bits, ColumnBuilder, Dict, Leaf, Literal, Store, Templated};
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding, CellSet, LaneProgram, LaneRunner, Values};
use zen_expression::Variable;
use super::{First, Found, Keyed, Kind, OutSource, Part, RuleOutputs, Table};

pub(super) type OutputCell = (Option<Literal>, Option<OutSource>);
type BuiltCache = ahash::HashMap<(u64, usize, u8), Option<Rc<Built>>>;

#[derive(Default)]
pub(super) struct OutputCells<'c> {
    cells: ahash::HashMap<&'c str, OutputCell>,
    many: ahash::HashMap<Vec<(String, &'c str)>, Option<LaneProgram>>,
}

impl<'c> OutputCells<'c> {
    pub(super) fn cell(&mut self, cell: &'c str) -> &OutputCell {
        self.cells.entry(cell).or_insert_with(|| Self::summary(cell))
    }

    fn summary(cell: &str) -> OutputCell {
        let Ok(program) = LaneProgram::standard(cell) else {
            return (None, None);
        };
        let literal = program.literal();
        let constant = literal
            .clone()
            .or_else(|| {
                let p = program.program();
                match p.site_keys.is_empty() && p.timeless() && !p.opaque() {
                    true => LaneRunner::new().evaluate_one(&program, &zen_expression::Scope::default()).ok(),
                    false => None,
                }
            })
            .as_ref()
            .and_then(Literal::of);
        let source = match literal.as_ref().and_then(Literal::of) {
            Some(literal) => Some(OutSource::Literal(literal)),
            None => program
                .path()
                .filter(|path| !path.starts_with('$'))
                .map(|path| OutSource::Path(Arc::from(path))),
        };
        (constant, source)
    }

    pub(super) fn many(&mut self, entries: &[(String, &'c str)]) -> Option<LaneProgram> {
        self.many
            .entry(entries.to_vec())
            .or_insert_with(|| {
                let refs: Vec<(&str, &str)> = entries.iter().map(|(k, v)| (k.as_str(), *v)).collect();
                LaneProgram::compile_many(&refs, false).ok()
            })
            .clone()
    }
}

pub(super) struct Built {
    shape: Rc<zen_types::variable::Shape>,
    parts: Vec<Part>,
}

impl Built {
    fn of(paths: &[(Vec<&str>, usize)]) -> Option<Built> {
        let mut keys: Vec<&str> = Vec::new();
        for (segments, _) in paths {
            let head = *segments.first()?;
            if !keys.contains(&head) {
                keys.push(head);
            }
        }
        let mut map = zen_types::variable::VariableMap::with_capacity(keys.len());
        let mut parts = Vec::with_capacity(keys.len());
        for key in &keys {
            let group: Vec<&(Vec<&str>, usize)> = paths.iter().filter(|(segments, _)| segments.first() == Some(key)).collect();
            let part = match group.as_slice() {
                [(segments, index)] if segments.len() == 1 => Part::Value(*index),
                _ if group.iter().all(|(segments, _)| segments.len() > 1) => {
                    let rest: Vec<(Vec<&str>, usize)> = group.iter().map(|(segments, index)| (segments[1..].to_vec(), *index)).collect();
                    Part::Child(Built::of(&rest)?)
                }
                _ => return None,
            };
            map.insert(zen_types::symbol::Symbol::from(*key), Variable::Null);
            parts.push(part);
        }
        let shape = map.shape().filter(|_| map.len() == keys.len())?.clone();
        Some(Built { shape, parts })
    }

    fn make(&self, values: &[Variable]) -> Variable {
        let values = self.parts.iter().map(|part| match part {
            Part::Value(index) => values.get(*index).map_or(Variable::Null, Variable::deep_clone),
            Part::Child(child) => child.make(values),
        });
        Variable::from_object(zen_types::variable::VariableMap::from_shape(self.shape.clone(), values))
    }
}

impl Table {
    pub(super) fn referenced_truths(
        runner: &mut LaneRunner,
        bound: &Bound,
        cells: &CellSet,
        other: usize,
        reference: &str,
        rows: &[usize],
        mut sink: impl FnMut(usize, bool),
    ) {
        let Some((program, _)) = cells.other(other).filter(|_| !rows.is_empty()) else {
            return;
        };
        let bind = |key: &str| Self::rebound(bound, reference, key).unwrap_or(Binding::Row);
        let mut outs = Outs::take();
        bound.export_with(runner, program, Some(rows), &bind, &mut outs, |_, _, _| {});
        let truths = outs.first_mut().map(|out| Leaf::typed(Array::from_output(out)).truths(rows.len()));
        for (i, &row) in rows.iter().enumerate() {
            sink(row, truths.as_ref().is_some_and(|t| Bits::get(t, i)));
        }
    }

    fn rebound(bound: &Bound, reference: &str, key: &str) -> Option<Binding> {
        let rest = match key.strip_prefix('$') {
            Some(rest) if rest.is_empty() || rest.starts_with('.') => rest,
            Some(_) if !key.starts_with("$nodes") => return None,
            _ => return Some((bound.bind)(key)),
        };
        match (bound.bind)(&format!("{reference}{rest}")) {
            Binding::Row => None,
            binding => Some(binding),
        }
    }

    pub(super) fn referenced(bound: &Bound, cells: &CellSet, reference: &str) -> bool {
        (0..cells.others()).all(|other| {
            cells.other(other).is_some_and(|(program, _)| {
                let p = program.program();
                !p.opaque()
                    && p.site_keys
                        .iter()
                        .all(|key| key.as_deref().is_some_and(|key| Self::rebound(bound, reference, key).is_some()))
            })
        })
    }

    pub(super) fn truths(
        runner: &mut LaneRunner,
        bound: &Bound,
        program: &LaneProgram,
        within: Option<&[usize]>,
        mut sink: impl FnMut(usize, bool),
    ) {
        let mut outs = Outs::take();
        bound.export(runner, program, within, &mut outs, |_, _, _| {});
        let count = within.map_or(bound.rows(), <[usize]>::len);
        let Some(truths) = outs.first_mut().map(|out| Leaf::typed(Array::from_output(out)).truths(count)) else {
            return;
        };
        match within {
            Some(rows) => rows.iter().enumerate().for_each(|(i, &row)| sink(row, Bits::get(&truths, i))),
            None => (0..count).for_each(|row| sink(row, Bits::get(&truths, row))),
        }
    }

    pub(super) fn ones(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
        words.iter().enumerate().flat_map(|(w, &word)| {
            let mut bits = word;
            std::iter::from_fn(move || {
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    w * 64 + bit
                })
            })
        })
    }

    pub(super) fn run_rule(
        runner: &mut LaneRunner,
        program: Option<&LaneProgram>,
        bound: &Bound,
        lanes: &[usize],
        sink: &mut dyn FnMut(usize, Option<Vec<Variable>>),
    ) {
        let Some(program) = program else {
            lanes.iter().for_each(|&lane| sink(lane, None));
            return;
        };
        bound.many(runner, program, Some(lanes), |row, r| sink(row, r.ok()));
    }

    thread_local! {
        static BUILT: std::cell::RefCell<BuiltCache> = std::cell::RefCell::new(ahash::HashMap::default());
    }

    pub(super) fn builder(&self, rule: usize, collect: Option<bool>) -> Option<Rc<Built>> {
        let outputs = &self.outputs[rule];
        let variant = match collect {
            None => 0u8,
            Some(false) => 1,
            Some(true) => 2,
        };
        Self::BUILT.with_borrow_mut(|cache| {
            if cache.len() >= Self::THAWS {
                cache.clear();
            }
            cache
                .entry((self.id, rule, variant))
                .or_insert_with(|| {
                    let paths: Vec<(Vec<&str>, usize)> = outputs
                        .paths
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, is_collect, _))| collect.is_none_or(|c| c == *is_collect))
                        .map(|(index, (path, _, _))| (path.split('.').collect(), index))
                        .collect();
                    Built::of(&paths).map(Rc::new)
                })
                .clone()
        })
    }

    pub(super) fn built(&self, builder: Option<&Rc<Built>>, rule: usize, collect: Option<bool>, values: &[Variable]) -> Variable {
        let outputs = &self.outputs[rule];
        match builder {
            Some(built) if values.len() >= outputs.paths.len() => built.make(values),
            _ => Self::assemble(&outputs.paths, values, collect),
        }
    }

    fn assemble(paths: &[(Arc<str>, bool, usize)], values: &[Variable], collect: Option<bool>) -> Variable {
        let object = Variable::empty_object();
        for ((path, is_collect, _), value) in paths.iter().zip(values) {
            if collect.is_none_or(|c| c == *is_collect) {
                object.dot_insert(path, value.deep_clone());
            }
        }
        object
    }

    pub(super) fn spread_rules(target: &mut [u64], words: usize, truth: &[u64], rules: &[u64], rows: usize) {
        for (w, &word) in truth.iter().enumerate() {
            let mut set = word;
            while set != 0 {
                let row = w * 64 + set.trailing_zeros() as usize;
                set &= set - 1;
                if row >= rows {
                    return;
                }
                match words {
                    1 => target[row] |= rules[0],
                    _ => target[row * words..(row + 1) * words].iter_mut().zip(rules).for_each(|(a, r)| *a |= r),
                }
            }
        }
    }

    pub(super) fn direct(outputs: &RuleOutputs, bound: &Bound, lanes: &[usize]) -> Option<Vec<Array>> {
        outputs
            .sources
            .as_ref()?
            .iter()
            .map(|source| match source {
                OutSource::Literal(literal) => {
                    let mut builder = ColumnBuilder::with_capacity(lanes.len());
                    lanes.iter().for_each(|_| builder.push_literal(literal));
                    Some(builder.finish())
                }
                OutSource::Path(path) => match (bound.bind)(path) {
                    Binding::Column(index) => {
                        let (_, column) = bound.columns.columns.get(index)?;
                        match column.values {
                            Values::Any(_) | Values::List { .. } | Values::Dict { .. } => None,
                            _ => Some(Array::gather(column, lanes)),
                        }
                    }
                    Binding::Absent => {
                        let mut builder = ColumnBuilder::with_capacity(lanes.len());
                        lanes.iter().for_each(|_| builder.push_null());
                        Some(builder.finish())
                    }
                    Binding::Row => None,
                },
            })
            .collect()
    }

    fn literal_at(&self, rule: usize, slot: usize, slots: usize, positions: &[Option<u32>]) -> Option<Option<&Literal>> {
        let index = positions[rule * slots + slot]? as usize;
        Some(self.outputs[rule].literals.as_ref().and_then(|literals| literals.get(index)))
    }

    pub(super) fn slot(
        &self,
        first: &First,
        slot: usize,
        slots: usize,
        positions: &[Option<u32>],
        used: &[u64],
    ) -> Option<(Dict, Vec<u64>, Vec<u64>)> {
        let rules = self.outputs.len();
        let entries = first.entries();
        let arrays: Vec<Option<&Array>> = first
            .batches
            .iter()
            .map(|batch| {
                let index = positions[batch.rule * slots + slot]? as usize;
                batch.outputs.get(index)
            })
            .collect();
        let mut kind = None;
        for rule in (0..rules).filter(|&rule| Bits::get(used, rule)) {
            if let Some(Some(literal)) = self.literal_at(rule, slot, slots, positions) {
                if literal.deep() {
                    return None;
                }
                kind = Kind::join(kind, Kind::of_literal(literal));
            }
        }
        for array in arrays.iter().flatten() {
            kind = Kind::join(kind, Kind::of_array(array));
        }
        let mut valid = vec![0u64; entries.div_ceil(64)];
        let mut written = vec![0u64; entries.div_ceil(64)];
        for rule in 0..rules {
            let literal = self.literal_at(rule, slot, slots, positions);
            Bits::set(&mut written, rule, literal.is_some_and(|l| l.is_some()));
            Bits::set(
                &mut valid,
                rule,
                literal.flatten().is_some_and(|l| Kind::of_literal(l).is_some_and(|k| kind == Some(Kind::Any) || Some(k) == kind)),
            );
        }
        for (batch, array) in arrays.iter().enumerate() {
            let start = rules + first.starts[batch];
            let len = first.starts[batch + 1] - first.starts[batch];
            let Some(array) = array else {
                continue;
            };
            let same = kind == Some(Kind::Any) || Kind::of_array(array) == kind;
            Bits::fill(&mut written, start, len);
            match (same, array.store(), array.validity_bits()) {
                (false, _, _) => {}
                (true, Store::Any(values), _) => {
                    for pos in 0..len {
                        if array.valid(pos) && !matches!(values.get(pos), None | Some(Variable::Null)) {
                            Bits::set(&mut valid, start + pos, true);
                        }
                    }
                }
                (true, _, None) => Bits::fill(&mut valid, start, len),
                (true, _, Some(bits)) => Bits::splice(&mut valid, start, bits, len),
            }
        }
        let literal = |rule: usize| self.literal_at(rule, slot, slots, positions).flatten();
        let dict = match kind.unwrap_or(Kind::Num) {
            Kind::Num => {
                let mut mant = Vec::with_capacity(entries);
                let mut scale = Vec::with_capacity(entries);
                for rule in 0..rules {
                    let (m, s) = literal(rule).and_then(Literal::scaled).unwrap_or((0, 0));
                    mant.push(m);
                    scale.push(s);
                }
                for (batch, array) in arrays.iter().enumerate() {
                    let len = first.starts[batch + 1] - first.starts[batch];
                    match array.map(|a| a.store()) {
                        Some(Store::Scaled { mant: m, scale: s }) if m.len() >= len && s.len() >= len => {
                            mant.extend_from_slice(&m[..len]);
                            scale.extend_from_slice(&s[..len]);
                        }
                        _ => {
                            mant.resize(mant.len() + len, 0);
                            scale.resize(scale.len() + len, 0);
                        }
                    }
                }
                Dict::Scaled { mant, scale }
            }
            Kind::Text => {
                let mut offsets = Vec::with_capacity(entries + 1);
                let mut data = String::new();
                offsets.push(0);
                for rule in 0..rules {
                    if let Some(Literal::Str(text)) = literal(rule) {
                        data.push_str(text);
                    }
                    offsets.push(data.len() as i32);
                }
                for (batch, array) in arrays.iter().enumerate() {
                    let len = first.starts[batch + 1] - first.starts[batch];
                    match array.map(|a| a.store()) {
                        Some(Store::Text { offsets: o, data: d }) if o.len() > len => {
                            let (from, to) = (o[0] as usize, o[len] as usize);
                            let base = data.len() as i32 - o[0];
                            data.push_str(d.get(from..to).unwrap_or_default());
                            offsets.extend(o[1..=len].iter().map(|x| x + base));
                        }
                        _ => offsets.extend(std::iter::repeat_n(data.len() as i32, len)),
                    }
                }
                Dict::Text { offsets, data }
            }
            Kind::Bool => {
                let mut bits = vec![0u64; entries.div_ceil(64).max(1)];
                for rule in 0..rules {
                    if let Some(Literal::Bool(true)) = literal(rule) {
                        Bits::set(&mut bits, rule, true);
                    }
                }
                for (batch, array) in arrays.iter().enumerate() {
                    let start = rules + first.starts[batch];
                    let len = first.starts[batch + 1] - first.starts[batch];
                    if let Some(Store::Bool(source)) = array.map(|a| a.store()) {
                        Bits::splice(&mut bits, start, source, len);
                    }
                }
                Dict::Bool(bits)
            }
            Kind::Any => {
                let mut values = Vec::with_capacity(entries);
                values.extend((0..rules).map(|rule| literal(rule).map_or(Variable::Null, Literal::variable)));
                for (batch, array) in arrays.iter().enumerate() {
                    let len = first.starts[batch + 1] - first.starts[batch];
                    match array {
                        Some(array) => {
                            let column = array.column();
                            values.extend((0..len).map(|pos| column.variable(pos)));
                        }
                        None => values.extend((0..len).map(|_| Variable::Null)),
                    }
                }
                Dict::Any(values)
            }
        };
        Some((dict, valid, written))
    }

    fn templated<'a>(&self, first: &First, slot: usize, slots: usize, positions: &[Option<u32>]) -> Option<Leaf<'a>> {
        if !first.batches.is_empty() {
            return None;
        }
        let templates: Rc<[Literal]> = (0..self.outputs.len())
            .map(|rule| self.literal_at(rule, slot, slots, positions).flatten().cloned().unwrap_or(Literal::Null))
            .collect();
        let codes: Rc<[i32]> = first.codes.iter().map(|&code| first.rule(code).map_or(-1, |rule| rule as i32)).collect();
        Some(Templated::leaf(codes, templates))
    }

    fn materialized<'a>(&self, first: &First, slot: usize, slots: usize, positions: &[Option<u32>]) -> Leaf<'a> {
        let templates: Vec<Option<Variable>> = (0..self.outputs.len())
            .map(|rule| match self.literal_at(rule, slot, slots, positions).flatten() {
                Some(literal @ (Literal::Array(_) | Literal::Object(_))) => Some(literal.variable()),
                _ => None,
            })
            .collect();
        let mut builder = ColumnBuilder::with_capacity(first.codes.len());
        for (row, &code) in first.codes.iter().enumerate() {
            match (first.rule(code), first.computed(code)) {
                (Some(rule), Some(_)) => match positions[rule * slots + slot] {
                    Some(index) => builder.push_variable(first.value(row, index as usize)),
                    None => builder.push_null(),
                },
                (Some(rule), None) => match (&templates[rule], self.literal_at(rule, slot, slots, positions).flatten()) {
                    (Some(template), _) => builder.push_any(template.deep_clone()),
                    (None, Some(literal)) => builder.push_literal(literal),
                    (None, None) => builder.push_null(),
                },
                _ => builder.push_null(),
            }
        }
        Leaf::typed(builder.finish())
    }

    pub fn columns<'a>(&self, first: First) -> (Vec<Leaf<'a>>, Vec<Mask<'a>>, Vec<u64>) {
        let rows = first.codes.len();
        let slots = self.all_paths.len();
        let positions = &self.positions;
        let entries = first.entries();
        let mut flags = vec![0u8; entries + 1];
        let mut unmatched = vec![0u64; rows.div_ceil(64)];
        for (chunk, word) in first.codes.chunks(64).zip(unmatched.iter_mut()) {
            let mut bits = 0u64;
            for (at, &code) in chunk.iter().enumerate() {
                bits |= u64::from(code < 0) << at;
                if let Some(flag) = flags.get_mut((code.max(-1) + 1) as usize) {
                    *flag = 1;
                }
            }
            *word = bits;
        }
        let used = Bits::of(entries, |code| flags[code + 1] != 0);
        let matched: Rc<[u64]> = {
            let mut bits: Vec<u64> = unmatched.iter().map(|w| !w).collect();
            Bits::trim(&mut bits, rows);
            bits.into()
        };
        let covered = |flags: &[u64]| used.iter().zip(flags).all(|(u, f)| u & !f == 0);
        let shared: Rc<[i32]> = first.codes.as_slice().into();
        let mut columns = Vec::with_capacity(slots);
        let mut present = Vec::with_capacity(slots);
        let mut masks: Vec<Keyed<'_, u64>> = Vec::new();
        let mut filtered: Vec<Keyed<'_, i32>> = Vec::new();
        for slot in 0..slots {
            let found: Option<Found<'_>> = match (self.statics.get(slot).and_then(Option::as_ref), first.batches.is_empty()) {
                (Some((dict, valid, written)), true) => Some((self.thawed(slot, dict), Cow::Borrowed(valid), Cow::Borrowed(written))),
                _ => self
                    .slot(&first, slot, slots, positions, &used)
                    .map(|(dict, valid, written)| (Rc::new(dict), Cow::Owned(valid), Cow::Owned(written))),
            };
            let Some((dict, valid, written)) = found else {
                let leaf = match self.templated(&first, slot, slots, positions) {
                    Some(leaf) => leaf,
                    None => self.materialized(&first, slot, slots, positions),
                };
                let written: Rc<[u64]> = Bits::of(rows, |row| {
                    first.rule(first.codes[row]).is_some_and(|rule| positions[rule * slots + slot].is_some())
                })
                .into();
                columns.push(leaf);
                present.push(Mask::Bits(written));
                continue;
            };
            let mask = match covered(&written) {
                true => matched.clone(),
                false => match masks.iter().find(|(key, _)| *key == written) {
                    Some((_, mask)) => mask.clone(),
                    None => {
                        let mask: Rc<[u64]> = Bits::of(rows, |row| {
                            usize::try_from(first.codes[row]).is_ok_and(|code| Bits::get(&written, code))
                        })
                        .into();
                        masks.push((written, mask.clone()));
                        mask
                    }
                },
            };
            present.push(Mask::Bits(mask));
            let codes = match covered(&valid) {
                true => shared.clone(),
                false => match filtered.iter().find(|(key, _)| *key == valid) {
                    Some((_, codes)) => codes.clone(),
                    None => {
                        let codes: Rc<[i32]> = first
                            .codes
                            .iter()
                            .map(|&code| match usize::try_from(code).is_ok_and(|c| Bits::get(&valid, c)) {
                                true => code,
                                false => -1,
                            })
                            .collect();
                        filtered.push((valid, codes.clone()));
                        codes
                    }
                },
            };
            columns.push(Leaf::typed(Array::coded(codes, dict)));
        }
        (columns, present, unmatched)
    }
}
