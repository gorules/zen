use crate::lane::date::Date;
use crate::lane::Bits;
use crate::typed::batch::Feeds;
use crate::typed::hir::{DictId, Hir, Item, Kind, Scope, Shape, Unsupported};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use crate::variable::Variable;
use chrono::DateTime;
use chrono_tz::Tz;
use rust_decimal::Decimal;

#[derive(Clone, Debug)]
pub enum Data {
    Num {
        mant: Vec<i64>,
        scale: Vec<u8>,
    },
    Bool(Vec<u64>),
    Code(Vec<i32>),
    Text {
        span: Vec<(u32, u32)>,
        data: Vec<u8>,
    },
    Date(Vec<Option<DateTime<Tz>>>),
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub name: String,
    pub scope: Scope,
    pub kind: Kind,
    pub len: usize,
    pub data: Data,
    pub valid: Vec<u64>,
    pub fault: Faults,
    pub wide: Vec<(usize, Decimal)>,
    pub offsets: Option<Vec<i32>>,
    pub values: Option<Vec<Variable>>,
    pub(crate) keep: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Faults {
    bits: Vec<u64>,
    rows: usize,
    any: bool,
}

#[derive(Clone, Debug)]
pub struct Outputs {
    rows: usize,
    slots: Vec<Slot>,
    faults: Faults,
    visible: usize,
}

impl Slot {
    pub fn dict(&self) -> Option<DictId> {
        match self.kind {
            Kind::Code(d) => Some(d),
            _ => None,
        }
    }

    #[inline]
    pub fn is_valid(&self, i: usize) -> bool {
        Bits::get(&self.valid, i)
    }

    pub fn num(&self, i: usize) -> Option<Dec> {
        match &self.data {
            Data::Num { mant, scale }
                if self.is_valid(i)
                    && self.wide.binary_search_by_key(&i, |(at, _)| *at).is_err() =>
            {
                Some(Dec::new(*mant.get(i)?, *scale.get(i)?))
            }
            _ => None,
        }
    }

    pub fn decimal(&self, i: usize) -> Option<Decimal> {
        match self.wide.binary_search_by_key(&i, |(at, _)| *at) {
            Ok(at) if self.is_valid(i) => self.wide.get(at).map(|(_, d)| *d),
            Ok(_) => None,
            Err(_) => match &self.data {
                Data::Num { mant, scale } if self.is_valid(i) => {
                    Some(Dec::new(*mant.get(i)?, *scale.get(i)?).decimal())
                }
                _ => None,
            },
        }
    }

    pub fn boolean(&self, i: usize) -> Option<bool> {
        match &self.data {
            Data::Bool(bits) if self.is_valid(i) => Some(Bits::get(bits, i)),
            _ => None,
        }
    }

    pub fn code(&self, i: usize) -> Option<i32> {
        match &self.data {
            Data::Code(codes) if self.is_valid(i) => codes.get(i).copied(),
            _ => None,
        }
    }

    pub fn text(&self, i: usize) -> Option<&str> {
        match &self.data {
            Data::Text { span, data } if self.is_valid(i) => {
                let (lo, hi) = *span.get(i)?;
                std::str::from_utf8(data.get(lo as usize..hi as usize)?).ok()
            }
            _ => None,
        }
    }

    pub fn date(&self, i: usize) -> Option<Option<DateTime<Tz>>> {
        match &self.data {
            Data::Date(dates) if self.is_valid(i) => dates.get(i).copied(),
            _ => None,
        }
    }

    pub fn variable(&self, i: usize) -> Variable {
        match self.date(i) {
            Some(date) => Date(date).variable(),
            None => Variable::Null,
        }
    }

    pub fn take_text(&mut self) -> Option<(Vec<i32>, String, Vec<u64>)> {
        let Data::Text { span, data } = &mut self.data else {
            return None;
        };
        let (span, data) = (std::mem::take(span), std::mem::take(data));
        let len = self.len.min(span.len());
        let mut offsets = Vec::with_capacity(len + 1);
        offsets.push(0i32);
        let packed = span[..len]
            .iter()
            .try_fold(0u32, |end, (lo, hi)| (end == *lo).then_some(*hi));
        let bytes = match packed {
            Some(end) => {
                let mut data = data;
                data.truncate(end as usize);
                offsets.extend(
                    span[..len]
                        .iter()
                        .map(|(_, hi)| i32::try_from(*hi).unwrap_or(i32::MAX)),
                );
                data
            }
            None => {
                let mut packed = Vec::with_capacity(data.len());
                for (at, (lo, hi)) in span[..len].iter().enumerate() {
                    if Bits::get(&self.valid, at) {
                        packed.extend_from_slice(
                            data.get(*lo as usize..*hi as usize).unwrap_or_default(),
                        );
                    }
                    offsets.push(i32::try_from(packed.len()).unwrap_or(i32::MAX));
                }
                packed
            }
        };
        let text = String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        Some((offsets, text, std::mem::take(&mut self.valid)))
    }

    pub fn take_num(&mut self) -> Option<(Vec<i64>, Vec<u8>, Vec<u64>)> {
        match &mut self.data {
            Data::Num { mant, scale } => Some((
                std::mem::take(mant),
                std::mem::take(scale),
                std::mem::take(&mut self.valid),
            )),
            _ => None,
        }
    }

    pub fn take_bool(&mut self) -> Option<(Vec<u64>, Vec<u64>)> {
        match &mut self.data {
            Data::Bool(bits) => Some((std::mem::take(bits), std::mem::take(&mut self.valid))),
            _ => None,
        }
    }

    pub fn take_code(&mut self) -> Option<(Vec<i32>, Vec<u64>)> {
        match &mut self.data {
            Data::Code(codes) => Some((std::mem::take(codes), std::mem::take(&mut self.valid))),
            _ => None,
        }
    }

    fn spread(&mut self, sources: &[u32]) {
        let rows = sources.len();
        let pick = |id: u32| (id != u32::MAX).then_some(id as usize);
        if let Some(values) = self.values.take() {
            self.values = Some(
                sources
                    .iter()
                    .map(|&id| {
                        pick(id)
                            .and_then(|at| values.get(at).cloned())
                            .unwrap_or(Variable::Null)
                    })
                    .collect(),
            );
        }
        let mut valid = vec![0u64; K::words(rows)];
        for (row, &id) in sources.iter().enumerate() {
            if pick(id).is_some_and(|at| Bits::get(&self.valid, at)) {
                Bits::set(&mut valid, row, true);
            }
        }
        self.data = match std::mem::replace(&mut self.data, Data::Bool(Vec::new())) {
            Data::Num { mant, scale } => Data::Num {
                mant: sources
                    .iter()
                    .map(|&id| pick(id).and_then(|at| mant.get(at).copied()).unwrap_or(0))
                    .collect(),
                scale: sources
                    .iter()
                    .map(|&id| pick(id).and_then(|at| scale.get(at).copied()).unwrap_or(0))
                    .collect(),
            },
            Data::Bool(bits) => {
                let mut out = vec![0u64; K::words(rows)];
                for (row, &id) in sources.iter().enumerate() {
                    if pick(id).is_some_and(|at| Bits::get(&bits, at)) {
                        Bits::set(&mut out, row, true);
                    }
                }
                Data::Bool(out)
            }
            Data::Code(codes) => Data::Code(
                sources
                    .iter()
                    .map(|&id| pick(id).and_then(|at| codes.get(at).copied()).unwrap_or(0))
                    .collect(),
            ),
            Data::Text { span, data } => Data::Text {
                span: sources
                    .iter()
                    .map(|&id| {
                        pick(id)
                            .and_then(|at| span.get(at).copied())
                            .unwrap_or((0, 0))
                    })
                    .collect(),
                data,
            },
            Data::Date(dates) => Data::Date(
                sources
                    .iter()
                    .map(|&id| pick(id).and_then(|at| dates.get(at).copied().flatten()))
                    .collect(),
            ),
        };
        if !self.wide.is_empty() {
            let wide = std::mem::take(&mut self.wide);
            self.wide = sources
                .iter()
                .enumerate()
                .filter_map(|(row, &id)| {
                    let at = pick(id)?;
                    let found = wide.binary_search_by_key(&at, |(at, _)| *at).ok()?;
                    wide.get(found).map(|(_, d)| (row, *d))
                })
                .collect();
        }
        self.valid = valid;
        self.fault = self.fault.spread(sources);
        self.len = rows;
    }

    fn reset(&mut self, len: usize) {
        self.len = len;
        self.wide.clear();
        self.values = None;
        let words = K::words(len);
        if let Some(offsets) = &mut self.offsets {
            offsets.clear();
            self.keep.clear();
            self.keep.resize(words, u64::MAX);
        }
        self.valid.clear();
        self.valid.resize(words, u64::MAX);
        match &mut self.data {
            Data::Num { mant, scale } => {
                mant.clear();
                mant.resize(len, 0);
                scale.clear();
                scale.resize(len, 0);
            }
            Data::Bool(bits) => {
                bits.clear();
                bits.resize(words, 0);
            }
            Data::Code(codes) => {
                codes.clear();
                codes.resize(len, 0);
            }
            Data::Text { span, data } => {
                span.clear();
                span.resize(len, (0, 0));
                data.clear();
            }
            Data::Date(dates) => {
                dates.clear();
                dates.resize(len, None);
            }
        }
    }
}

impl Faults {
    fn spread(&self, sources: &[u32]) -> Self {
        let mut bits = vec![0u64; K::words(sources.len())];
        let mut any = false;
        if self.any {
            for (row, &id) in sources.iter().enumerate() {
                if id != u32::MAX && Bits::get(&self.bits, id as usize) {
                    Bits::set(&mut bits, row, true);
                    any = true;
                }
            }
        }
        Self {
            bits,
            rows: sources.len(),
            any,
        }
    }

    pub(crate) fn reset(&mut self, rows: usize) {
        self.bits.clear();
        self.bits.resize(K::words(rows), 0);
        self.rows = rows;
        self.any = false;
    }

    #[inline]
    pub(crate) fn set(&mut self, row: usize) {
        self.any = true;
        Bits::set(&mut self.bits, row, true);
    }

    #[inline]
    pub fn get(&self, row: usize) -> bool {
        Bits::get(&self.bits, row)
    }

    #[inline]
    pub fn any(&self) -> bool {
        self.any
    }

    pub fn words(&self) -> &[u64] {
        &self.bits
    }

    pub fn count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        let rows = self.rows;
        Bits::rows(&self.bits).filter(move |r| *r < rows)
    }
}

impl Outputs {
    pub(crate) fn new(hir: &Hir) -> Result<Self, Unsupported> {
        let mut slots = Vec::with_capacity(hir.outputs.len());
        for o in &hir.outputs {
            let data = match o.ty.kind {
                Kind::Num(_) | Kind::Null => Data::Num {
                    mant: Vec::new(),
                    scale: Vec::new(),
                },
                Kind::Bool => Data::Bool(Vec::new()),
                Kind::Code(_) => Data::Code(Vec::new()),
                Kind::Text => Data::Text {
                    span: Vec::new(),
                    data: Vec::new(),
                },
                Kind::Date => Data::Date(Vec::new()),
            };
            slots.push(Slot {
                name: o.name.clone(),
                scope: o.scope,
                kind: o.ty.kind,
                len: 0,
                data,
                valid: Vec::new(),
                fault: Faults::default(),
                wide: Vec::new(),
                offsets: o.collect.map(|_| Vec::new()),
                values: None,
                keep: Vec::new(),
            });
        }
        let visible = slots.len();
        Ok(Self {
            rows: 0,
            slots,
            faults: Faults::default(),
            visible,
        })
    }

    pub(crate) fn within(mut self, visible: usize) -> Self {
        self.visible = visible.min(self.slots.len());
        self
    }

    pub(crate) fn empty() -> Self {
        Self {
            rows: 0,
            slots: Vec::new(),
            faults: Faults::default(),
            visible: 0,
        }
    }

    pub(crate) fn reset(&mut self, rows: usize, len: impl Fn(Scope) -> usize) {
        self.rows = rows;
        self.faults.reset(rows);
        for slot in &mut self.slots {
            slot.reset(len(slot.scope));
            slot.fault.reset(rows);
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots[..self.visible]
    }

    pub fn slot(&self, at: usize) -> Option<&Slot> {
        self.slots().get(at)
    }

    pub fn slot_mut(&mut self, at: usize) -> Option<&mut Slot> {
        let visible = self.visible;
        self.slots[..visible].get_mut(at)
    }

    pub(crate) fn total(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn any_mut(&mut self, at: usize) -> Option<&mut Slot> {
        self.slots.get_mut(at)
    }

    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    pub fn spread(&mut self, rows: usize, ids: &[u32], positions: Option<&[usize]>) -> bool {
        let reps = self.rows;
        let visible = self.visible;
        if self.slots[..visible]
            .iter()
            .any(|slot| slot.offsets.is_some() || slot.len != reps)
            || positions.is_some_and(|p| p.len() != ids.len())
        {
            return false;
        }
        let mut sources = match positions {
            None => ids.to_vec(),
            Some(_) => vec![u32::MAX; rows],
        };
        if let Some(positions) = positions {
            positions
                .iter()
                .zip(ids)
                .for_each(|(&row, &id)| sources[row] = id);
        }
        if sources.len() != rows
            || sources
                .iter()
                .any(|&id| id != u32::MAX && id as usize >= reps)
        {
            return false;
        }
        self.rows = rows;
        self.faults = self.faults.spread(&sources);
        for slot in &mut self.slots[..visible] {
            slot.spread(&sources);
        }
        true
    }

    pub(crate) fn parts(&mut self) -> (&mut [Slot], &mut Faults) {
        (&mut self.slots, &mut self.faults)
    }

    pub(crate) fn collect(&mut self, hir: &Hir, feeds: &Feeds, select: Option<&[u64]>) {
        for (at, o) in hir.outputs.iter().enumerate() {
            let Some(c) = o.collect else {
                continue;
            };
            let Some(list) = feeds.lists.get(c.list as usize) else {
                continue;
            };
            if let (false, Some(null)) = (c.empty, &list.null) {
                for owner in (0..list.owners()).filter(|&o| Bits::get(null, o)) {
                    let row = feeds.row_of(c.list as usize, owner);
                    if select.is_none_or(|sel| Bits::get(sel, row)) {
                        self.faults.set(row);
                        if let Some(slot) = self.slots.get_mut(at) {
                            slot.fault.set(row);
                        }
                    }
                }
            }
            if let Some(slot) = self.slots.get_mut(at) {
                slot.compact(&list.offsets);
            }
        }
    }
}

impl Outputs {
    pub(crate) fn compose(&mut self, hir: &Hir) {
        let base = hir.outputs.len();
        for (at, o) in hir.outputs.iter().enumerate() {
            let Some(shape) = &o.shape else {
                continue;
            };
            let mut parts = Vec::new();
            shape.parts(&mut parts);
            let rows = self.rows;
            let mut fault = Faults::default();
            fault.reset(rows);
            for p in &parts {
                if let Some(part) = self.slots.get(base + *p as usize) {
                    if part.fault.any() {
                        for row in part.fault.iter() {
                            fault.set(row);
                        }
                    }
                }
            }
            let flat = match shape.as_ref() {
                Shape::List(items) => self.flat(hir, base, items, &fault),
                _ => None,
            };
            let values: Option<Vec<Variable>> = match flat {
                Some(_) => None,
                None => Some(
                    (0..rows)
                        .map(|row| match fault.get(row) {
                            true => Variable::Null,
                            false => self.build(hir, base, shape, row),
                        })
                        .collect(),
                ),
            };
            if let Some(slot) = self.slots.get_mut(at) {
                match flat {
                    Some((kind, data, valid, offsets)) => {
                        slot.kind = kind;
                        slot.len = offsets.last().map_or(0, |end| *end as usize);
                        slot.data = data;
                        slot.valid = valid;
                        slot.offsets = Some(offsets);
                    }
                    None => {
                        slot.len = rows;
                        slot.valid.clear();
                        slot.valid.resize(K::words(rows), u64::MAX);
                        slot.values = values;
                    }
                }
                if fault.any() {
                    for row in fault.iter() {
                        slot.fault.set(row);
                        self.faults.set(row);
                    }
                }
            }
        }
    }

    fn kept(&self, base: usize, item: &Item, row: usize) -> bool {
        item.keep.is_none_or(|k| {
            self.slots
                .get(base + k as usize)
                .and_then(|slot| slot.boolean(row))
                .unwrap_or(false)
        })
    }

    fn flat(
        &self,
        hir: &Hir,
        base: usize,
        items: &[Item],
        fault: &Faults,
    ) -> Option<(Kind, Data, Vec<u64>, Vec<i32>)> {
        let mut kind: Option<Kind> = None;
        let mut parts: Vec<&Slot> = Vec::with_capacity(items.len());
        for item in items {
            let Shape::Part(p) = item.shape else {
                return None;
            };
            let slot = self.slots.get(base + p as usize)?;
            if item.splice != slot.offsets.is_some() || !slot.wide.is_empty() {
                return None;
            }
            let family = match slot.kind {
                Kind::Num(_) => Some(Kind::Num(None)),
                Kind::Bool => Some(Kind::Bool),
                Kind::Text | Kind::Code(_) => Some(Kind::Text),
                Kind::Null => None,
                Kind::Date => return None,
            };
            match (kind, family) {
                (_, None) => {}
                (None, found) => kind = found,
                (Some(a), Some(b)) if a == b => {}
                _ => return None,
            }
            parts.push(slot);
        }
        let kind = kind.unwrap_or(Kind::Num(None));
        let mut offsets = Vec::with_capacity(self.rows + 1);
        offsets.push(0i32);
        let mut valid: Vec<bool> = Vec::new();
        let (mut mant, mut scale, mut bits) = (Vec::new(), Vec::new(), Vec::new());
        let (mut span, mut text) = (Vec::new(), Vec::new());
        for row in 0..self.rows {
            if !fault.get(row) {
                for (item, slot) in items.iter().zip(&parts) {
                    if !self.kept(base, item, row) {
                        continue;
                    }
                    let range = match &slot.offsets {
                        Some(o) => {
                            usize::try_from(*o.get(row)?).ok()?
                                ..usize::try_from(*o.get(row + 1)?).ok()?
                        }
                        None => row..row + 1,
                    };
                    for e in range {
                        let on = slot.is_valid(e);
                        valid.push(on);
                        match kind {
                            Kind::Num(_) => {
                                let d = slot.num(e).filter(|_| on);
                                mant.push(d.map_or(0, |d| d.mant));
                                scale.push(d.map_or(0, |d| d.scale));
                            }
                            Kind::Bool => bits.push(on && slot.boolean(e).unwrap_or(false)),
                            _ => {
                                let found = match (&slot.data, on) {
                                    (_, false) => None,
                                    (Data::Code(_), true) => {
                                        slot.code(e).zip(slot.dict()).and_then(|(code, d)| {
                                            hir.schema.dicts.get(d as usize)?.text(code)
                                        })
                                    }
                                    (_, true) => slot.text(e),
                                };
                                let lo = u32::try_from(text.len()).ok()?;
                                text.extend_from_slice(found.unwrap_or_default().as_bytes());
                                span.push((lo, u32::try_from(text.len()).ok()?));
                            }
                        }
                    }
                }
            }
            offsets.push(i32::try_from(valid.len()).ok()?);
        }
        let len = valid.len();
        let packed = |flags: &[bool]| {
            let mut words = vec![0u64; K::words(len)];
            flags
                .iter()
                .enumerate()
                .for_each(|(at, on)| Bits::set(&mut words, at, *on));
            words
        };
        let data = match kind {
            Kind::Num(_) => Data::Num { mant, scale },
            Kind::Bool => Data::Bool(packed(&bits)),
            _ => Data::Text { span, data: text },
        };
        Some((kind, data, packed(&valid), offsets))
    }

    fn build(&self, hir: &Hir, base: usize, shape: &Shape, row: usize) -> Variable {
        match shape {
            Shape::Part(p) => match self.slots.get(base + *p as usize) {
                Some(slot) => slot.value(hir, row),
                None => Variable::Null,
            },
            Shape::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    if !self.kept(base, item, row) {
                        continue;
                    }
                    let value = self.build(hir, base, &item.shape, row);
                    match (item.splice, value) {
                        (true, Variable::Array(inner)) => {
                            out.extend(inner.borrow().iter().cloned())
                        }
                        (_, value) => out.push(value),
                    }
                }
                Variable::from_array(out)
            }
            Shape::Object(members) => {
                let object = Variable::empty_object();
                if let Variable::Object(map) = &object {
                    let mut map = map.borrow_mut();
                    for (key, member) in members {
                        map.insert_str(key, self.build(hir, base, member, row));
                    }
                }
                object
            }
        }
    }
}

impl Slot {
    fn element(&self, hir: &Hir, at: usize) -> Variable {
        match &self.data {
            Data::Num { .. } => self.decimal(at).map_or(Variable::Null, Variable::Number),
            Data::Bool(_) => self.boolean(at).map_or(Variable::Null, Variable::Bool),
            Data::Code(_) => match (self.code(at), self.dict()) {
                (Some(code), Some(d)) => hir
                    .schema
                    .dicts
                    .get(d as usize)
                    .and_then(|dict| dict.text(code))
                    .map_or(Variable::Null, |t| Variable::String(t.into())),
                _ => Variable::Null,
            },
            Data::Text { .. } => self
                .text(at)
                .map_or(Variable::Null, |t| Variable::String(t.into())),
            Data::Date(_) => self.variable(at),
        }
    }

    fn value(&self, hir: &Hir, row: usize) -> Variable {
        match &self.offsets {
            Some(offsets) => {
                let lo = offsets.get(row).and_then(|o| usize::try_from(*o).ok());
                let hi = offsets.get(row + 1).and_then(|o| usize::try_from(*o).ok());
                match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        Variable::from_array((lo..hi).map(|at| self.element(hir, at)).collect())
                    }
                    _ => Variable::Null,
                }
            }
            None => self.element(hir, row),
        }
    }

    fn compact(&mut self, offsets: &[i32]) {
        let len = self.len;
        let kept: Vec<usize> = (0..len).filter(|&e| Bits::get(&self.keep, e)).collect();
        let mut out = Vec::with_capacity(offsets.len());
        let mut at = 0usize;
        out.push(0i32);
        for w in offsets.windows(2) {
            let hi = usize::try_from(w[1]).unwrap_or(0);
            while at < kept.len() && kept[at] < hi {
                at += 1;
            }
            out.push(i32::try_from(at).unwrap_or(i32::MAX));
        }
        if kept.len() != len {
            let mut valid = vec![0u64; K::words(kept.len())];
            for (to, &from) in kept.iter().enumerate() {
                Bits::set(&mut valid, to, Bits::get(&self.valid, from));
            }
            self.valid = valid;
            match &mut self.data {
                Data::Num { mant, scale } => {
                    *mant = kept.iter().map(|&e| mant[e]).collect();
                    *scale = kept.iter().map(|&e| scale[e]).collect();
                }
                Data::Bool(bits) => {
                    let mut packed = vec![0u64; K::words(kept.len())];
                    for (to, &from) in kept.iter().enumerate() {
                        Bits::set(&mut packed, to, Bits::get(bits, from));
                    }
                    *bits = packed;
                }
                Data::Code(codes) => *codes = kept.iter().map(|&e| codes[e]).collect(),
                Data::Text { span, .. } => *span = kept.iter().map(|&e| span[e]).collect(),
                Data::Date(dates) => *dates = kept.iter().map(|&e| dates[e]).collect(),
            }
            let wide = std::mem::take(&mut self.wide);
            self.wide = wide
                .into_iter()
                .filter_map(|(e, d)| kept.binary_search(&e).ok().map(|to| (to, d)))
                .collect();
            self.len = kept.len();
        }
        self.offsets = Some(out);
    }
}

#[cfg(test)]
mod tests {
    use super::{Bits, Data, Faults, Outputs, Slot};
    use crate::typed::hir::{Kind, Scope};
    use crate::typed::kernel::K;
    use rust_decimal::Decimal;

    struct Build;

    impl Build {
        fn slot(
            kind: Kind,
            data: Data,
            valid: &[bool],
            faulted: &[usize],
            wide: Vec<(usize, Decimal)>,
        ) -> Slot {
            let mut bits = vec![0u64; K::words(valid.len())];
            valid
                .iter()
                .enumerate()
                .for_each(|(at, &on)| Bits::set(&mut bits, at, on));
            let mut fault = Faults::default();
            fault.reset(valid.len());
            faulted.iter().for_each(|&at| fault.set(at));
            Slot {
                name: String::new(),
                scope: Scope::Row,
                kind,
                len: valid.len(),
                data,
                valid: bits,
                fault,
                wide,
                offsets: None,
                values: None,
                keep: Vec::new(),
            }
        }

        fn reps() -> Outputs {
            let num = Self::slot(
                Kind::Num(None),
                Data::Num {
                    mant: vec![15, 7, 0],
                    scale: vec![1, 0, 0],
                },
                &[true, true, false],
                &[2],
                vec![(1, Decimal::MAX)],
            );
            let text = Self::slot(
                Kind::Text,
                Data::Text {
                    span: vec![(0, 2), (2, 5), (5, 5)],
                    data: b"abxyz".to_vec(),
                },
                &[true, true, false],
                &[],
                Vec::new(),
            );
            let flag = Self::slot(
                Kind::Bool,
                Data::Bool(vec![0b101]),
                &[true, false, true],
                &[],
                Vec::new(),
            );
            let mut faults = Faults::default();
            faults.reset(3);
            faults.set(2);
            Outputs {
                rows: 3,
                slots: vec![num, text, flag],
                faults,
                visible: 3,
            }
        }
    }

    #[test]
    fn spread_maps_every_row_to_its_representative() {
        let mut out = Build::reps();
        assert!(out.spread(5, &[1, 0, 2, 1, 0], None));
        assert_eq!(out.rows(), 5);
        let num = &out.slots()[0];
        assert_eq!(
            (0..5).map(|at| num.decimal(at)).collect::<Vec<_>>(),
            vec![
                Some(Decimal::MAX),
                Some(Decimal::new(15, 1)),
                None,
                Some(Decimal::MAX),
                Some(Decimal::new(15, 1))
            ]
        );
        assert_eq!(num.fault.iter().collect::<Vec<_>>(), vec![2]);
        assert_eq!(out.faults().iter().collect::<Vec<_>>(), vec![2]);
        let text = &out.slots()[1];
        assert_eq!(
            (0..5).map(|at| text.text(at)).collect::<Vec<_>>(),
            vec![Some("xyz"), Some("ab"), None, Some("xyz"), Some("ab")]
        );
        let flag = &out.slots()[2];
        assert_eq!(
            (0..5).map(|at| flag.boolean(at)).collect::<Vec<_>>(),
            vec![None, Some(true), Some(true), None, Some(true)]
        );
    }

    #[test]
    fn spread_leaves_unselected_rows_invalid_and_unfaulted() {
        let mut out = Build::reps();
        assert!(out.spread(6, &[2, 0, 1], Some(&[1, 3, 4])));
        let num = &out.slots()[0];
        assert_eq!(
            (0..6).map(|at| num.is_valid(at)).collect::<Vec<_>>(),
            vec![false, false, false, true, true, false]
        );
        assert_eq!(num.fault.iter().collect::<Vec<_>>(), vec![1]);
        assert_eq!(num.wide, vec![(4, Decimal::MAX)]);
    }

    #[test]
    fn spread_rejects_mismatched_ids() {
        let mut out = Build::reps();
        assert!(!out.spread(2, &[0, 3], None));
        assert!(!out.spread(4, &[0, 1], Some(&[0])));
    }
}
