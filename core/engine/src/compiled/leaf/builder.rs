use zen_expression::lane::{Column, Dictionary, Values};
use zen_expression::Variable;
use super::{Array, Bits, Literal, Store};

#[derive(Default)]
pub(crate) struct ColumnBuilder {
    kind: Option<u8>,
    mant: Vec<i64>,
    scale: Vec<u8>,
    bits: Vec<u64>,
    offsets: Vec<i32>,
    data: String,
    any: Vec<Variable>,
    valid: Vec<u64>,
    nulls: bool,
    rows: usize,
    capacity: usize,
}

impl ColumnBuilder {
    const NUM: u8 = 0;
    const BOOL: u8 = 1;
    const TEXT: u8 = 2;
    const ANY: u8 = 3;

    pub fn with_capacity(rows: usize) -> Self {
        Self {
            valid: Vec::with_capacity(rows.div_ceil(64)),
            capacity: rows,
            ..Default::default()
        }
    }

    pub fn null_at(column: &Column, row: usize) -> bool {
        if !column.valid(row) {
            return true;
        }
        match column.values {
            Values::Any(values) => values.get(row).is_none_or(|v| matches!(v, Variable::Null)),
            Values::Dict { values, .. } => column.code(row).is_none_or(|code| {
                !values.valid(code)
                    || matches!(values, Dictionary::Any(v) if v.get(code).is_none_or(|v| matches!(v, Variable::Null)))
            }),
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => column.number(row).is_none(),
            Values::Bool { .. } | Values::List { .. } => false,
            _ => column.text(row).is_none(),
        }
    }

    fn open(&mut self) -> usize {
        let row = self.rows;
        self.rows += 1;
        if row.is_multiple_of(64) {
            self.valid.push(0);
            if self.kind == Some(Self::BOOL) {
                self.bits.push(0);
            }
        }
        row
    }

    fn fill(&mut self) {
        match self.kind {
            Some(Self::NUM) => {
                self.mant.push(0);
                self.scale.push(0);
            }
            Some(Self::TEXT) => self.offsets.push(self.data.len() as i32),
            Some(Self::ANY) => self.any.push(Variable::Null),
            _ => {}
        }
    }

    fn start(&mut self, kind: u8) {
        let rows = self.rows;
        let capacity = self.capacity.max(rows);
        self.kind = Some(kind);
        match kind {
            Self::NUM => {
                self.mant = Vec::with_capacity(capacity);
                self.mant.resize(rows, 0);
                self.scale = Vec::with_capacity(capacity);
                self.scale.resize(rows, 0);
            }
            Self::BOOL => self.bits = vec![0; rows.div_ceil(64)],
            Self::TEXT => self.offsets = vec![0; rows + 1],
            _ => self.any = vec![Variable::Null; rows],
        }
    }

    fn degrade(&mut self) {
        let typed = Array {
            values: match self.kind {
                Some(Self::NUM) => Store::Scaled {
                    mant: std::mem::take(&mut self.mant),
                    scale: std::mem::take(&mut self.scale),
                },
                Some(Self::BOOL) => Store::Bool(std::mem::take(&mut self.bits)),
                Some(Self::TEXT) => Store::Text {
                    offsets: std::mem::take(&mut self.offsets),
                    data: std::mem::take(&mut self.data),
                },
                _ => Store::Any(Vec::new()),
            },
            valid: Some(self.valid.clone()),
            rows: self.rows,
        };
        let column = typed.column();
        self.any = (0..self.rows).map(|row| column.variable(row)).collect();
        self.kind = Some(Self::ANY);
    }

    pub fn push_cell(&mut self, column: &Column, row: usize) {
        if !column.valid(row) {
            return self.push_null();
        }
        match column.values {
            Values::Scaled { mant, scale } => match (mant.get(row), scale.get(row)) {
                (Some(m), Some(s)) => self.push_scaled(*m, *s),
                _ => self.push_null(),
            },
            Values::I64(values) => match values.get(row) {
                Some(v) => self.push_scaled(*v, 0),
                None => self.push_null(),
            },
            Values::Dec(values) => match values.get(row) {
                Some(d) => match (i64::try_from(d.mantissa()), u8::try_from(d.scale())) {
                    (Ok(m), Ok(s)) => self.push_scaled(m, s),
                    _ => self.push_variable(Variable::Number(*d)),
                },
                None => self.push_null(),
            },
            Values::Bool { .. } => match column.boolean(row) {
                Some(b) => self.push_bool(b),
                None => self.push_null(),
            },
            Values::Text { .. } | Values::Utf8 { .. } => match column.text(row) {
                Some(text) => self.push_text(text),
                None => self.push_null(),
            },
            Values::Dict { keys, values } => match keys.get(row).and_then(|k| usize::try_from(*k).ok()) {
                None => self.push_null(),
                Some(code) => match values {
                    Dictionary::Scaled { mant, scale } => match (mant.get(code), scale.get(code)) {
                        (Some(m), Some(s)) => self.push_scaled(*m, *s),
                        _ => self.push_null(),
                    },
                    Dictionary::Text { offsets, data } => {
                        let text = offsets
                            .get(code)
                            .zip(offsets.get(code + 1))
                            .and_then(|(a, b)| data.get(*a as usize..*b as usize));
                        match text {
                            Some(text) => self.push_text(text),
                            None => self.push_null(),
                        }
                    }
                    Dictionary::Bool { bits } => self.push_bool(Bits::get(bits, code)),
                    Dictionary::Any(values) => self.push_variable(values.get(code).cloned().unwrap_or(Variable::Null)),
                    Dictionary::Column(inner) => self.push_cell(inner, code),
                },
            },
            Values::Any(values) => self.push_variable(values.get(row).cloned().unwrap_or(Variable::Null)),
            _ => self.push_variable(column.variable(row)),
        }
    }

    pub fn push_scaled(&mut self, mant: i64, scale: u8) {
        if self.kind.is_none() {
            self.start(Self::NUM);
        }
        if self.kind != Some(Self::NUM) {
            return self.push_variable(Variable::Number(rust_decimal::Decimal::new(mant, scale as u32)));
        }
        let row = self.open();
        self.mark(row);
        self.mant.push(mant);
        self.scale.push(scale);
    }

    pub fn push_text(&mut self, text: &str) {
        if self.kind.is_none() {
            self.start(Self::TEXT);
        }
        if self.kind != Some(Self::TEXT) {
            return self.push_variable(Variable::String(text.into()));
        }
        let row = self.open();
        self.mark(row);
        self.data.push_str(text);
        self.offsets.push(self.data.len() as i32);
    }

    pub fn push_null(&mut self) {
        self.open();
        self.nulls = true;
        self.fill();
    }

    fn mark(&mut self, row: usize) {
        if let Some(word) = self.valid.get_mut(row / 64) {
            *word |= 1 << (row % 64);
        }
    }

    pub fn push_literal(&mut self, literal: &Literal) {
        match literal {
            Literal::Null => self.push_null(),
            Literal::Array(_) | Literal::Object(_) => self.push_any(literal.variable()),
            Literal::Num(n) => match (i64::try_from(n.mantissa()), u8::try_from(n.scale())) {
                (Ok(m), Ok(s)) => self.push_scaled(m, s),
                _ => self.push_any(Variable::Number(*n)),
            },
            Literal::Str(text) => self.push_text(text),
            Literal::Bool(b) => self.push_bool(*b),
        }
    }

    pub fn push_any(&mut self, value: Variable) {
        match self.kind {
            None => self.start(Self::ANY),
            Some(Self::ANY) => {}
            Some(_) => self.degrade(),
        }
        let row = self.open();
        self.mark(row);
        self.any.push(value);
    }

    pub fn push_variable(&mut self, value: Variable) {
        let typed = |kind: u8| self.kind.is_none_or(|k| k == kind);
        match &value {
            Variable::Null => return self.push_null(),
            Variable::Number(n) if typed(Self::NUM) => {
                if let (Ok(m), Ok(s)) = (i64::try_from(n.mantissa()), u8::try_from(n.scale())) {
                    return self.push_scaled(m, s);
                }
            }
            Variable::String(text) if typed(Self::TEXT) => return self.push_text(text),
            Variable::Bool(b) if typed(Self::BOOL) => return self.push_bool(*b),
            _ => {}
        }
        self.push_any(value)
    }

    pub fn push_bool(&mut self, b: bool) {
        if self.kind.is_none() {
            self.start(Self::BOOL);
        }
        if self.kind != Some(Self::BOOL) {
            return self.push_any(Variable::Bool(b));
        }
        let row = self.open();
        self.mark(row);
        if b {
            if let Some(word) = self.bits.get_mut(row / 64) {
                *word |= 1 << (row % 64);
            }
        }
    }

    pub fn finish(mut self) -> Array {
        let values = match self.kind {
            Some(Self::NUM) => Store::Scaled {
                mant: self.mant,
                scale: self.scale,
            },
            Some(Self::BOOL) => Store::Bool(self.bits),
            Some(Self::TEXT) => Store::Text {
                offsets: self.offsets,
                data: self.data,
            },
            Some(_) => Store::Any(self.any),
            None => {
                self.nulls = true;
                Store::Any(vec![Variable::Null; self.rows])
            }
        };
        Array {
            values,
            valid: self.nulls.then_some(self.valid),
            rows: self.rows,
        }
    }
}
