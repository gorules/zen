use crate::typed::batch::Feeds;
use crate::typed::hir::{DictId, Hir, Kind, Scope, Unsupported};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use rust_decimal::Decimal;

#[derive(Clone, Debug)]
pub enum Data {
    Num { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Code(Vec<i32>),
    Text { span: Vec<(u32, u32)>, data: Vec<u8> },
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
        }
    }
}

impl Faults {
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
            }
            let wide = std::mem::take(&mut self.wide);
            self.wide = wide.into_iter().filter_map(|(e, d)| kept.binary_search(&e).ok().map(|to| (to, d))).collect();
            self.len = kept.len();
        }
        self.offsets = Some(out);
    }
}
