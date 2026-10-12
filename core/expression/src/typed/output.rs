use crate::typed::batch::Feeds;
use crate::typed::hir::{DictId, Hir, Kind, Scope, Unsupported};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use crate::lane::date::Date;
use crate::variable::Variable;
use chrono::DateTime;
use chrono_tz::Tz;
use rust_decimal::Decimal;

#[derive(Clone, Debug)]
pub enum Data {
    Num { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Code(Vec<i32>),
    Text { span: Vec<(u32, u32)>, data: Vec<u8> },
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
        K::get(&self.valid, i)
    }

    pub fn num(&self, i: usize) -> Option<Dec> {
        match &self.data {
            Data::Num { mant, scale } if self.is_valid(i) && self.wide.binary_search_by_key(&i, |(at, _)| *at).is_err() => Some(Dec::new(*mant.get(i)?, *scale.get(i)?)),
            _ => None,
        }
    }

    pub fn decimal(&self, i: usize) -> Option<Decimal> {
        match self.wide.binary_search_by_key(&i, |(at, _)| *at) {
            Ok(at) if self.is_valid(i) => self.wide.get(at).map(|(_, d)| *d),
            Ok(_) => None,
            Err(_) => match &self.data {
                Data::Num { mant, scale } if self.is_valid(i) => Some(Dec::new(*mant.get(i)?, *scale.get(i)?).decimal()),
                _ => None,
            },
        }
    }

    pub fn boolean(&self, i: usize) -> Option<bool> {
        match &self.data {
            Data::Bool(bits) if self.is_valid(i) => Some(K::get(bits, i)),
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
        let packed = span[..len].iter().try_fold(0u32, |end, (lo, hi)| (end == *lo).then_some(*hi));
        let bytes = match packed {
            Some(end) => {
                let mut data = data;
                data.truncate(end as usize);
                offsets.extend(span[..len].iter().map(|(_, hi)| i32::try_from(*hi).unwrap_or(i32::MAX)));
                data
            }
            None => {
                let mut packed = Vec::with_capacity(data.len());
                for (at, (lo, hi)) in span[..len].iter().enumerate() {
                    if K::get(&self.valid, at) {
                        packed.extend_from_slice(data.get(*lo as usize..*hi as usize).unwrap_or_default());
                    }
                    offsets.push(i32::try_from(packed.len()).unwrap_or(i32::MAX));
                }
                packed
            }
        };
        let text = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        Some((offsets, text, std::mem::take(&mut self.valid)))
    }

    pub fn take_num(&mut self) -> Option<(Vec<i64>, Vec<u8>, Vec<u64>)> {
        match &mut self.data {
            Data::Num { mant, scale } => Some((std::mem::take(mant), std::mem::take(scale), std::mem::take(&mut self.valid))),
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
        let mut valid = vec![0u64; K::words(rows)];
        for (row, &id) in sources.iter().enumerate() {
            if pick(id).is_some_and(|at| K::get(&self.valid, at)) {
                K::set(&mut valid, row, true);
            }
        }
        self.data = match std::mem::replace(&mut self.data, Data::Bool(Vec::new())) {
            Data::Num { mant, scale } => Data::Num {
                mant: sources.iter().map(|&id| pick(id).and_then(|at| mant.get(at).copied()).unwrap_or(0)).collect(),
                scale: sources.iter().map(|&id| pick(id).and_then(|at| scale.get(at).copied()).unwrap_or(0)).collect(),
            },
            Data::Bool(bits) => {
                let mut out = vec![0u64; K::words(rows)];
                for (row, &id) in sources.iter().enumerate() {
                    if pick(id).is_some_and(|at| K::get(&bits, at)) {
                        K::set(&mut out, row, true);
                    }
                }
                Data::Bool(out)
            }
            Data::Code(codes) => Data::Code(sources.iter().map(|&id| pick(id).and_then(|at| codes.get(at).copied()).unwrap_or(0)).collect()),
            Data::Text { span, data } => Data::Text {
                span: sources.iter().map(|&id| pick(id).and_then(|at| span.get(at).copied()).unwrap_or((0, 0))).collect(),
                data,
            },
            Data::Date(dates) => Data::Date(sources.iter().map(|&id| pick(id).and_then(|at| dates.get(at).copied().flatten())).collect()),
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
                if id != u32::MAX && K::get(&self.bits, id as usize) {
                    K::set(&mut bits, row, true);
                    any = true;
                }
            }
        }
        Self { bits, rows: sources.len(), any }
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
        K::set(&mut self.bits, row, true);
    }

    #[inline]
    pub fn get(&self, row: usize) -> bool {
        K::get(&self.bits, row)
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
        self.bits.iter().enumerate().flat_map(move |(w, word)| {
            let mut bits = *word;
            std::iter::from_fn(move || {
                (bits != 0).then(|| {
                    let at = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    w * 64 + at
                })
            })
            .filter(move |r| *r < rows)
        })
    }
}

impl Outputs {
    pub(crate) fn new(hir: &Hir) -> Result<Self, Unsupported> {
        let mut slots = Vec::with_capacity(hir.outputs.len());
        for o in &hir.outputs {
            let data = match o.ty.kind {
                Kind::Num(_) | Kind::Null => Data::Num { mant: Vec::new(), scale: Vec::new() },
                Kind::Bool => Data::Bool(Vec::new()),
                Kind::Code(_) => Data::Code(Vec::new()),
                Kind::Text => Data::Text { span: Vec::new(), data: Vec::new() },
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
                keep: Vec::new(),
            });
        }
        Ok(Self { rows: 0, slots, faults: Faults::default() })
    }

    pub(crate) fn empty() -> Self {
        Self { rows: 0, slots: Vec::new(), faults: Faults::default() }
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
        &self.slots
    }

    pub fn slot(&self, at: usize) -> Option<&Slot> {
        self.slots.get(at)
    }

    pub fn slot_mut(&mut self, at: usize) -> Option<&mut Slot> {
        self.slots.get_mut(at)
    }

    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    pub fn spread(&mut self, rows: usize, ids: &[u32], positions: Option<&[usize]>) -> bool {
        let reps = self.rows;
        if self.slots.iter().any(|slot| slot.offsets.is_some() || slot.len != reps) || positions.is_some_and(|p| p.len() != ids.len()) {
            return false;
        }
        let mut sources = match positions {
            None => ids.to_vec(),
            Some(_) => vec![u32::MAX; rows],
        };
        if let Some(positions) = positions {
            positions.iter().zip(ids).for_each(|(&row, &id)| sources[row] = id);
        }
        if sources.len() != rows || sources.iter().any(|&id| id != u32::MAX && id as usize >= reps) {
            return false;
        }
        self.rows = rows;
        self.faults = self.faults.spread(&sources);
        for slot in &mut self.slots {
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
                for owner in (0..list.owners()).filter(|&o| K::get(null, o)) {
                    let row = feeds.row_of(c.list as usize, owner);
                    if select.is_none_or(|sel| K::get(sel, row)) {
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

impl Slot {
    fn compact(&mut self, offsets: &[i32]) {
        let len = self.len;
        let kept: Vec<usize> = (0..len).filter(|&e| K::get(&self.keep, e)).collect();
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
                K::set(&mut valid, to, K::get(&self.valid, from));
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
                        K::set(&mut packed, to, K::get(bits, from));
                    }
                    *bits = packed;
                }
                Data::Code(codes) => *codes = kept.iter().map(|&e| codes[e]).collect(),
                Data::Text { span, .. } => *span = kept.iter().map(|&e| span[e]).collect(),
                Data::Date(dates) => *dates = kept.iter().map(|&e| dates[e]).collect(),
            }
            let wide = std::mem::take(&mut self.wide);
            self.wide = wide.into_iter().filter_map(|(e, d)| kept.binary_search(&e).ok().map(|to| (to, d))).collect();
            self.len = kept.len();
        }
        self.offsets = Some(out);
    }
}

#[cfg(test)]
mod tests {
    use super::{Data, Faults, Outputs, Slot};
    use crate::typed::hir::{Kind, Scope};
    use crate::typed::kernel::K;
    use rust_decimal::Decimal;

    struct Build;

    impl Build {
        fn slot(kind: Kind, data: Data, valid: &[bool], faulted: &[usize], wide: Vec<(usize, Decimal)>) -> Slot {
            let mut bits = vec![0u64; K::words(valid.len())];
            valid.iter().enumerate().for_each(|(at, &on)| K::set(&mut bits, at, on));
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
                keep: Vec::new(),
            }
        }

        fn reps() -> Outputs {
            let num = Self::slot(Kind::Num(None), Data::Num { mant: vec![15, 7, 0], scale: vec![1, 0, 0] }, &[true, true, false], &[2], vec![(1, Decimal::MAX)]);
            let text = Self::slot(Kind::Text, Data::Text { span: vec![(0, 2), (2, 5), (5, 5)], data: b"abxyz".to_vec() }, &[true, true, false], &[], Vec::new());
            let flag = Self::slot(Kind::Bool, Data::Bool(vec![0b101]), &[true, false, true], &[], Vec::new());
            let mut faults = Faults::default();
            faults.reset(3);
            faults.set(2);
            Outputs { rows: 3, slots: vec![num, text, flag], faults }
        }
    }

    #[test]
    fn spread_maps_every_row_to_its_representative() {
        let mut out = Build::reps();
        assert!(out.spread(5, &[1, 0, 2, 1, 0], None));
        assert_eq!(out.rows(), 5);
        let num = &out.slots()[0];
        assert_eq!((0..5).map(|at| num.decimal(at)).collect::<Vec<_>>(), vec![Some(Decimal::MAX), Some(Decimal::new(15, 1)), None, Some(Decimal::MAX), Some(Decimal::new(15, 1))]);
        assert_eq!(num.fault.iter().collect::<Vec<_>>(), vec![2]);
        assert_eq!(out.faults().iter().collect::<Vec<_>>(), vec![2]);
        let text = &out.slots()[1];
        assert_eq!((0..5).map(|at| text.text(at)).collect::<Vec<_>>(), vec![Some("xyz"), Some("ab"), None, Some("xyz"), Some("ab")]);
        let flag = &out.slots()[2];
        assert_eq!((0..5).map(|at| flag.boolean(at)).collect::<Vec<_>>(), vec![None, Some(true), Some(true), None, Some(true)]);
    }

    #[test]
    fn spread_leaves_unselected_rows_invalid_and_unfaulted() {
        let mut out = Build::reps();
        assert!(out.spread(6, &[2, 0, 1], Some(&[1, 3, 4])));
        let num = &out.slots()[0];
        assert_eq!((0..6).map(|at| num.is_valid(at)).collect::<Vec<_>>(), vec![false, false, false, true, true, false]);
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
