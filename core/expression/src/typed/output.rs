use crate::typed::hir::{DictId, Hir, Kind, Scope, Unsupported};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use rust_decimal::Decimal;

#[derive(Clone, Debug)]
pub enum Data {
    Num { mant: Vec<i64>, scale: Vec<u8> },
    Bool(Vec<u64>),
    Code(Vec<i32>),
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
                Kind::Text => return Err(Unsupported::new(format!("text output {}", o.name))),
            };
            slots.push(Slot { name: o.name.clone(), scope: o.scope, kind: o.ty.kind, len: 0, data, valid: Vec::new(), fault: Faults::default(), wide: Vec::new() });
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
}
