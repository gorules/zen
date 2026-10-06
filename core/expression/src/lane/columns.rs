use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::program::{Binding, Kind};
use crate::variable::Variable;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Debug, Clone, Copy)]
pub enum Values<'a> {
    Dec(&'a [Decimal]),
    Scaled {
        mant: &'a [i64],
        scale: &'a [u8],
    },
    I64(&'a [i64]),
    F64(&'a [f64]),
    Bool {
        bits: &'a [u64],
        offset: usize,
    },
    Utf8 {
        offsets: &'a [i32],
        data: &'a [u8],
    },
    Text {
        offsets: &'a [i32],
        data: &'a str,
    },
    LargeUtf8 {
        offsets: &'a [i64],
        data: &'a [u8],
    },
    Strs(&'a [&'a str]),
    Dict {
        keys: &'a [i32],
        values: Dictionary<'a>,
    },
    List {
        offsets: &'a [i32],
        child: Dictionary<'a>,
    },
    Any(&'a [Variable]),
}

#[derive(Debug, Clone, Copy)]
pub enum Dictionary<'a> {
    Column(&'a Column<'a>),
    Text { offsets: &'a [i32], data: &'a str },
    Scaled { mant: &'a [i64], scale: &'a [u8] },
    Bool { bits: &'a [u64] },
    Any(&'a [Variable]),
}

impl<'a> From<&'a Column<'a>> for Dictionary<'a> {
    fn from(column: &'a Column<'a>) -> Self {
        Dictionary::Column(column)
    }
}

impl<'a> Dictionary<'a> {
    pub fn column(self) -> Column<'a> {
        match self {
            Dictionary::Column(column) => *column,
            Dictionary::Text { offsets, data } => Column::new(Values::Text { offsets, data }),
            Dictionary::Scaled { mant, scale } => Column::new(Values::Scaled { mant, scale }),
            Dictionary::Bool { bits } => Column::new(Values::Bool { bits, offset: 0 }),
            Dictionary::Any(values) => Column::new(Values::Any(values)),
        }
    }

    pub fn len(self) -> usize {
        self.column().len()
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn kind(self) -> Kind {
        self.column().kind()
    }

    pub fn valid(self, row: usize) -> bool {
        self.column().valid(row)
    }

    pub fn number(self, row: usize) -> Option<Decimal> {
        self.column().number(row)
    }

    pub fn boolean(self, row: usize) -> Option<bool> {
        self.column().boolean(row)
    }

    pub fn text(self, row: usize) -> Option<&'a str> {
        self.column().text(row)
    }

    pub fn variable(self, row: usize) -> Variable {
        self.column().variable(row)
    }

    pub fn equals(self, row: usize, k: &Variable) -> bool {
        self.column().equals(row, k)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Column<'a> {
    pub values: Values<'a>,
    pub validity: Option<(&'a [u64], usize)>,
}

impl<'a> Column<'a> {
    pub fn new(values: Values<'a>) -> Self {
        Self {
            values,
            validity: None,
        }
    }

    pub fn with_validity(values: Values<'a>, bits: &'a [u64], offset: usize) -> Self {
        Self {
            values,
            validity: Some((bits, offset)),
        }
    }

    pub fn kind(&self) -> Kind {
        match self.values {
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => Kind::Num,
            Values::Bool { .. } => Kind::Bool,
            Values::Utf8 { .. } | Values::Text { .. } | Values::LargeUtf8 { .. } | Values::Strs(_) => Kind::Str,
            Values::Dict { values, .. } => values.kind(),
            Values::List { child, .. } => match child.column().values {
                Values::Scaled { .. } | Values::Text { .. } | Values::Bool { .. } if child.column().validity.is_none() => Kind::List,
                _ => Kind::Dyn,
            },
            Values::Any(_) => Kind::Dyn,
        }
    }

    #[inline]
    pub(crate) fn bit(bits: &[u64], index: usize) -> bool {
        bits.get(index / 64)
            .is_some_and(|w| w >> (index % 64) & 1 == 1)
    }

    pub(crate) fn valid_mask<M: LaneSet>(&self, start: usize, width: usize) -> M {
        match self.validity {
            None => M::all(width),
            Some((bits, offset)) => Self::mask(bits, offset + start, width),
        }
    }

    pub(crate) fn mask<M: LaneSet>(bits: &[u64], at: usize, width: usize) -> M {
        let mut mask = M::none(width);
        for i in 0..width.div_ceil(64) {
            let take = (width - i * 64).min(64);
            mask.set_word(i, Self::word(bits, at + i * 64, take));
        }
        mask
    }

    #[inline]
    pub fn word(bits: &[u64], at: usize, width: usize) -> u64 {
        let all = match width >= 64 {
            true => u64::MAX,
            false => (1u64 << width) - 1,
        };
        if at == usize::MAX {
            return all;
        }
        let (word, shift) = (at / 64, at % 64);
        let low = bits.get(word).copied().unwrap_or(0) >> shift;
        let high = match shift {
            0 => 0,
            _ => bits.get(word + 1).copied().unwrap_or(0) << (64 - shift),
        };
        (low | high) & all
    }

    #[inline]
    pub fn range(&self, row: usize) -> Option<(usize, usize)> {
        let Values::List { offsets, .. } = self.values else {
            return None;
        };
        match (self.valid(row), offsets.get(row), offsets.get(row + 1)) {
            (true, Some(a), Some(b)) => {
                let a = usize::try_from(*a).unwrap_or(0);
                Some((a, usize::try_from(*b).unwrap_or(0).max(a)))
            }
            _ => None,
        }
    }

    #[inline]
    pub fn valid(&self, row: usize) -> bool {
        match self.validity {
            None => true,
            Some((bits, offset)) => Self::bit(bits, offset + row),
        }
    }

    #[inline]
    pub fn code(&self, row: usize) -> Option<usize> {
        match self.values {
            Values::Dict { keys, .. } => keys.get(row).and_then(|k| usize::try_from(*k).ok()),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        match self.values {
            Values::Dec(v) => v.len(),
            Values::Scaled { mant, .. } => mant.len(),
            Values::I64(v) => v.len(),
            Values::F64(v) => v.len(),
            Values::Bool { bits, offset } => (bits.len() * 64).saturating_sub(offset),
            Values::Utf8 { offsets, .. } => offsets.len().saturating_sub(1),
            Values::Text { offsets, .. } => offsets.len().saturating_sub(1),
            Values::LargeUtf8 { offsets, .. } => offsets.len().saturating_sub(1),
            Values::Strs(v) => v.len(),
            Values::Dict { keys, .. } => keys.len(),
            Values::List { offsets, .. } => offsets.len().saturating_sub(1),
            Values::Any(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn number(&self, row: usize) -> Option<Decimal> {
        match self.values {
            Values::Dec(v) => v.get(row).copied(),
            Values::Scaled { mant, scale } => Some(Decimal::new(*mant.get(row)?, *scale.get(row)? as u32)),
            Values::I64(v) => v.get(row).map(|n| Decimal::from(*n)),
            Values::F64(v) => v.get(row).and_then(|f| Self::float(*f)),
            Values::Dict { values, .. } => {
                let code = self.code(row)?;
                values.valid(code).then(|| values.number(code)).flatten()
            }
            Values::Any(v) => match v.get(row) {
                Some(Variable::Number(n)) => Some(*n),
                _ => None,
            },
            _ => None,
        }
    }

    fn float(f: f64) -> Option<Decimal> {
        if !f.is_finite() {
            return None;
        }
        let mut buffer = [0u8; 512];
        let mut cursor = std::io::Cursor::new(&mut buffer[..]);
        let written = std::io::Write::write_fmt(&mut cursor, format_args!("{f}")).is_ok();
        let len = cursor.position() as usize;
        written
            .then(|| std::str::from_utf8(&buffer[..len]).ok())
            .flatten()
            .and_then(|text| Decimal::from_str(text).ok())
            .or_else(|| Decimal::from_f64(f))
    }

    #[inline]
    pub fn boolean(&self, row: usize) -> Option<bool> {
        match self.values {
            Values::Bool { bits, offset } => Some(Self::bit(bits, offset + row)),
            Values::Dict { values, .. } => {
                let code = self.code(row)?;
                values.valid(code).then(|| values.boolean(code)).flatten()
            }
            _ => None,
        }
    }

    pub(crate) fn texts(&self, base: usize, n: usize) -> Option<Texts<'a>> {
        match self.values {
            Values::Utf8 { offsets, data } => {
                let window = offsets.get(base..=base + n)?;
                let (first, last) = (*window.first()? as usize, *window.last()? as usize);
                window.windows(2).all(|w| w[0] <= w[1]).then_some(())?;
                let block = std::str::from_utf8(data.get(first..last)?).ok()?;
                Some(Texts::Utf8(block, window, first))
            }
            Values::Text { offsets, data } => {
                let window = offsets.get(base..=base + n)?;
                let (first, last) = (*window.first()? as usize, *window.last()? as usize);
                window.windows(2).all(|w| w[0] <= w[1]).then_some(())?;
                Some(Texts::Utf8(data.get(first..last)?, window, first))
            }
            Values::LargeUtf8 { offsets, data } => {
                let window = offsets.get(base..=base + n)?;
                let (first, last) = (*window.first()? as usize, *window.last()? as usize);
                window.windows(2).all(|w| w[0] <= w[1]).then_some(())?;
                let block = std::str::from_utf8(data.get(first..last)?).ok()?;
                Some(Texts::Large(block, window, first))
            }
            Values::Strs(v) => v.get(base..base + n).map(Texts::Strs),
            _ => None,
        }
    }

    pub fn bytes(&self, row: usize) -> Option<&'a [u8]> {
        match self.values {
            Values::Utf8 { offsets, data } => {
                let (a, b) = (*offsets.get(row)? as usize, *offsets.get(row + 1)? as usize);
                data.get(a..b)
            }
            Values::Text { offsets, data } => {
                let (a, b) = (*offsets.get(row)? as usize, *offsets.get(row + 1)? as usize);
                data.as_bytes().get(a..b)
            }
            Values::LargeUtf8 { offsets, data } => {
                let (a, b) = (*offsets.get(row)? as usize, *offsets.get(row + 1)? as usize);
                data.get(a..b)
            }
            _ => self.text(row).map(str::as_bytes),
        }
    }

    pub fn scaled(&self, row: usize) -> Option<(i128, u32)> {
        match self.values {
            Values::Scaled { mant, scale } => Some((*mant.get(row)? as i128, *scale.get(row)? as u32)),
            Values::I64(v) => Some((*v.get(row)? as i128, 0)),
            Values::Dec(v) => v.get(row).map(|d| (d.mantissa(), d.scale())),
            _ => None,
        }
    }

    pub fn text(&self, row: usize) -> Option<&'a str> {
        match self.values {
            Values::Utf8 { offsets, data } => {
                let (a, b) = (*offsets.get(row)? as usize, *offsets.get(row + 1)? as usize);
                std::str::from_utf8(data.get(a..b)?).ok()
            }
            Values::Text { offsets, data } => {
                let (a, b) = (*offsets.get(row)? as usize, *offsets.get(row + 1)? as usize);
                data.get(a..b)
            }
            Values::LargeUtf8 { offsets, data } => {
                let (a, b) = (*offsets.get(row)? as usize, *offsets.get(row + 1)? as usize);
                std::str::from_utf8(data.get(a..b)?).ok()
            }
            Values::Strs(v) => v.get(row).copied(),
            Values::Dict { values, .. } => {
                let code = self.code(row)?;
                values.valid(code).then(|| values.text(code)).flatten()
            }
            _ => None,
        }
    }

    pub fn equals(&self, row: usize, k: &Variable) -> bool {
        if !self.valid(row) {
            return matches!(k, Variable::Null);
        }
        match (self.values, k) {
            (Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_), Variable::Number(n)) => self
                .number(row)
                .is_some_and(|m| crate::lane::ops::Ops::same_number(&m, n)),
            (Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_), Variable::Null) => {
                self.number(row).is_none()
            }
            (Values::Bool { .. }, Variable::Bool(b)) => self.boolean(row) == Some(*b),
            (
                Values::Utf8 { .. } | Values::Text { .. } | Values::LargeUtf8 { .. } | Values::Strs(_),
                Variable::String(s),
            ) => self.bytes(row) == Some(s.as_bytes()),
            (Values::Any(v), k) => v
                .get(row)
                .is_some_and(|v| crate::lane::ops::Ops::equal(v, k)),
            (Values::Dict { values, .. }, k) => {
                self.code(row).is_some_and(|code| values.equals(code, k))
            }
            (Values::List { .. }, _) => false,
            _ => false,
        }
    }

    pub fn borrowed(&self, row: usize) -> Option<&'a Variable> {
        if !self.valid(row) {
            return None;
        }
        match self.values {
            Values::Any(values) => values.get(row),
            Values::Dict {
                values: Dictionary::Any(values),
                ..
            } => values.get(self.code(row)?),
            _ => None,
        }
    }

    pub fn variable(&self, row: usize) -> Variable {
        if !self.valid(row) {
            return Variable::Null;
        }
        match self.values {
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => {
                self.number(row).map_or(Variable::Null, Variable::Number)
            }
            Values::Bool { .. } => self.boolean(row).map_or(Variable::Null, Variable::Bool),
            Values::Any(v) => v.get(row).cloned().unwrap_or(Variable::Null),
            Values::Dict { values, .. } => self
                .code(row)
                .map_or(Variable::Null, |code| values.variable(code)),
            Values::List { child, .. } => {
                let items = self
                    .range(row)
                    .map(|(a, b)| (a..b).map(|i| child.variable(i)).collect())
                    .unwrap_or_default();
                Variable::from_array(items)
            }
            _ => self
                .text(row)
                .map_or(Variable::Null, |s| Variable::String(s.into())),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Columns<'a> {
    pub rows: usize,
    pub columns: Vec<(&'a str, Column<'a>)>,
}

impl<'a> Columns<'a> {
    pub fn new(rows: usize) -> Self {
        Self {
            rows,
            columns: Vec::new(),
        }
    }

    pub fn column(mut self, path: &'a str, column: Column<'a>) -> Self {
        self.columns.push((path, column));
        self
    }

    pub fn find(&self, path: &str) -> Option<usize> {
        self.columns.iter().rposition(|(p, _)| *p == path)
    }

    pub fn bind(&self, path: &str) -> Binding {
        let covered = self.columns.iter().any(|(p, _)| {
            p.strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('.'))
                || path
                    .strip_prefix(*p)
                    .is_some_and(|rest| rest.starts_with('.'))
        });
        match (covered, self.find(path)) {
            (true, _) => Binding::Row,
            (false, Some(i)) => Binding::Column(i),
            (false, None) => Binding::Absent,
        }
    }

    pub fn row(&self, row: usize) -> Variable {
        let object = Variable::empty_object();
        for (path, column) in &self.columns {
            let value = match column.values {
                Values::Any(_) => column.variable(row).depth_clone(usize::MAX),
                _ => column.variable(row),
            };
            if !matches!(value, Variable::Null) || column.valid(row) {
                object.dot_insert(path, value);
            }
        }
        object
    }
}

pub(crate) enum Texts<'a> {
    Utf8(&'a str, &'a [i32], usize),
    Large(&'a str, &'a [i64], usize),
    Strs(&'a [&'a str]),
}

impl<'a> Texts<'a> {
    #[inline]
    pub(crate) fn each_bytes<M: LaneSet>(&self, lanes: M, mut f: impl FnMut(usize, &'a [u8])) {
        match self {
            Texts::Utf8(block, offsets, first) => {
                let block = block.as_bytes();
                for lane in Lanes::of(lanes) {
                    let range = offsets
                        .get(lane)
                        .zip(offsets.get(lane + 1))
                        .and_then(|(a, b)| (*a as usize).checked_sub(*first).zip((*b as usize).checked_sub(*first)));
                    if let Some(x) = range.and_then(|(a, b)| block.get(a..b)) {
                        f(lane, x);
                    }
                }
            }
            Texts::Large(block, offsets, first) => {
                let block = block.as_bytes();
                for lane in Lanes::of(lanes) {
                    let range = offsets
                        .get(lane)
                        .zip(offsets.get(lane + 1))
                        .and_then(|(a, b)| (*a as usize).checked_sub(*first).zip((*b as usize).checked_sub(*first)));
                    if let Some(x) = range.and_then(|(a, b)| block.get(a..b)) {
                        f(lane, x);
                    }
                }
            }
            Texts::Strs(v) => {
                for lane in Lanes::of(lanes) {
                    if let Some(x) = v.get(lane) {
                        f(lane, x.as_bytes());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod float_tests {
    use super::Column;
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn stack_formatting_matches_heap_formatting() {
        let mut state = 0xBB67AE8584CAA73Bu64;
        for i in 0..300_000u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let f = match i % 4 {
                0 => f64::from_bits(state),
                1 => (state % 10_000_000) as f64 / 100.0,
                2 => (state as i64 as f64) * 1e-30,
                _ => f64::from_bits(state % 0x0010_0000_0000_0000),
            };
            let expected = f
                .is_finite()
                .then(|| {
                    Decimal::from_str(&format!("{f}"))
                        .ok()
                        .or_else(|| Decimal::from_f64(f))
                })
                .flatten();
            let got = Column::float(f);
            assert_eq!(
                got.map(|d| d.serialize()),
                expected.map(|d| d.serialize()),
                "{f:e}"
            );
        }
    }
}
