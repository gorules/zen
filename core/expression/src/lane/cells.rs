use crate::lane::date::DynamicVariableExt;
use crate::lane::{Binding, Column, Columns, Kind, LaneProgram, LaneRunner, Values};
use crate::lexer::{ComparisonOperator, Lexer, LogicalOperator, Operator};
use crate::parser::{Node, Parser};
use crate::scope::Scope;
use crate::variable::Variable;
use crate::{Isolate, IsolateError};
use ahash::HashMap;
use bumpalo::Bump;
use rust_decimal::Decimal;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Num(Decimal),
    Str(Arc<str>),
    Bool(bool),
    Null,
}

impl Key {
    fn literal(node: &Node) -> Option<Key> {
        match node {
            Node::Number(n) => Some(Key::Num(n.normalize())),
            Node::String(s) => Some(Key::Str(Arc::from(*s))),
            Node::Bool(b) => Some(Key::Bool(*b)),
            Node::Null => Some(Key::Null),
            _ => None,
        }
    }
}

enum Shape {
    Equal(Vec<Key>),
    Range(Vec<Decimal>),
    Other,
}

impl Shape {
    fn classify(node: &Node) -> Shape {
        if let Some(keys) = Self::equalities(node) {
            return Shape::Equal(keys);
        }
        match node {
            Node::Binary {
                left: Node::Identifier("$"),
                operator:
                    Operator::Comparison(
                        ComparisonOperator::LessThan
                        | ComparisonOperator::LessThanOrEqual
                        | ComparisonOperator::GreaterThan
                        | ComparisonOperator::GreaterThanOrEqual,
                    ),
                right: Node::Number(k),
            } => Shape::Range(vec![*k]),
            Node::Binary {
                left: Node::Identifier("$"),
                operator: Operator::Comparison(ComparisonOperator::In),
                right:
                    Node::Interval {
                        left: Node::Number(a),
                        right: Node::Number(b),
                        ..
                    },
            } => Shape::Range(vec![*a, *b]),
            Node::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::And | LogicalOperator::Or),
                right,
            } => match (Self::classify(left), Self::classify(right)) {
                (Shape::Range(mut a), Shape::Range(b)) => {
                    a.extend(b);
                    Shape::Range(a)
                }
                _ => Shape::Other,
            },
            Node::Parenthesized(inner) => match Self::classify(inner) {
                range @ Shape::Range(_) => range,
                _ => Shape::Other,
            },
            _ => Shape::Other,
        }
    }

    fn equalities(node: &Node) -> Option<Vec<Key>> {
        match node {
            Node::Binary {
                left: Node::Identifier("$"),
                operator: Operator::Comparison(ComparisonOperator::Equal),
                right,
            } => Key::literal(right).map(|k| vec![k]),
            Node::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::Or),
                right,
            } => {
                let mut keys = Self::equalities(left)?;
                keys.extend(Self::equalities(right)?);
                Some(keys)
            }
            Node::Parenthesized(inner) => Self::equalities(inner),
            _ => None,
        }
    }
}

struct Other {
    program: LaneProgram,
    rules: Vec<u64>,
}

type Keyed = (u64, Box<[u8]>, Vec<u64>);

pub struct CellEnv<'a> {
    pub scopes: &'a [Scope],
    pub columns: &'a Columns<'a>,
    pub bind: &'a dyn Fn(&str) -> Binding,
}

struct Scaling {
    mult: i64,
    bounds: Vec<i64>,
}

pub struct Pieces {
    pub bits: Vec<Vec<u64>>,
    null: u16,
    truth: [u16; 2],
    keys: Vec<(u64, usize, Box<[u8]>, u16)>,
    text: u16,
    regions: Vec<u16>,
    other: u16,
    pub failed: u16,
}

impl Pieces {
    fn intern(bits: &mut Vec<Vec<u64>>, piece: Vec<u64>) -> u16 {
        match bits.iter().position(|b| *b == piece) {
            Some(at) => at as u16,
            None => {
                bits.push(piece);
                (bits.len() - 1) as u16
            }
        }
    }

    #[inline]
    fn word(text: &[u8]) -> u64 {
        match text.get(..8) {
            Some(head) => {
                let mut word = [0u8; 8];
                word.copy_from_slice(head);
                u64::from_le_bytes(word)
            }
            None => text.iter().enumerate().fold(0u64, |w, (i, b)| w | (*b as u64) << (8 * i)),
        }
    }

    #[inline]
    fn text(&self, text: &[u8]) -> u16 {
        let (word, len) = (Self::word(text), text.len());
        self.keys
            .iter()
            .find(|(w, l, key, _)| *w == word && *l == len && (len <= 8 || key.as_ref() == text))
            .map_or(self.text, |(_, _, _, piece)| *piece)
    }

    pub fn len(&self) -> usize {
        self.bits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }
}

pub struct CellSet {
    rules: usize,
    words: usize,
    always: Vec<u64>,
    strs: HashMap<Box<[u8]>, Vec<u64>>,
    few: Option<Vec<Keyed>>,
    scaled: Option<(u32, Vec<i128>)>,
    bools: [Option<Vec<u64>>; 2],
    null: Option<Vec<u64>>,
    bounds: Vec<Decimal>,
    regions: Vec<Vec<u64>>,
    others: Vec<Other>,
    owners: Vec<Option<usize>>,
    optimistic: Vec<u64>,
}

impl CellSet {
    const FEW: usize = 8;

    fn prefix(text: &[u8]) -> u64 {
        let mut word = [0u8; 8];
        let take = text.len().min(7);
        word[..take].copy_from_slice(&text[..take]);
        word[7] = text.len().min(255) as u8;
        u64::from_le_bytes(word)
    }

    fn text(&self, text: &[u8]) -> Option<&Vec<u64>> {
        match &self.few {
            Some(few) => {
                let prefix = Self::prefix(text);
                few.iter()
                    .find(|(p, k, _)| *p == prefix && k.as_ref() == text)
                    .map(|(_, _, v)| v)
            }
            None => self.strs.get(text),
        }
    }

    fn scaled_bounds(bounds: &[Decimal]) -> Option<(u32, Vec<i128>)> {
        let scale = bounds.iter().map(Decimal::scale).max().unwrap_or(0);
        if scale > 18 {
            return None;
        }
        bounds
            .iter()
            .map(|b| b.mantissa().checked_mul(10i128.checked_pow(scale - b.scale())?))
            .collect::<Option<Vec<_>>>()
            .map(|v| (scale, v))
    }

    fn scaling(&self, scale: u8) -> Option<Scaling> {
        let (target, bounds) = self.scaled.as_ref()?;
        let (mult, lift) = match (scale as u32).checked_sub(*target) {
            None => (10i64.checked_pow(target - scale as u32)?, 1i128),
            Some(up) => (1, 10i128.checked_pow(up)?),
        };
        let bounds = bounds
            .iter()
            .map(|b| b.checked_mul(lift).and_then(|b| i64::try_from(b).ok()))
            .collect::<Option<Vec<_>>>()?;
        Some(Scaling { mult, bounds })
    }

    #[inline]
    fn region_of(bounds: &[i64], value: i64) -> usize {
        match bounds.len() <= 16 {
            true => {
                let (mut below, mut equal) = (0usize, false);
                for bound in bounds {
                    below += usize::from(*bound < value);
                    equal |= *bound == value;
                }
                below * 2 + usize::from(equal)
            }
            false => match bounds.binary_search(&value) {
                Ok(i) => i * 2 + 1,
                Err(i) => i * 2,
            },
        }
    }

    fn numbers(&self, column: &Column, rows: usize, out: &mut [u64]) -> bool {
        let words = self.words;
        let parts = |row: usize| -> Option<(i64, u8)> {
            match column.values {
                Values::Scaled { mant, scale } => Some((*mant.get(row)?, *scale.get(row)?)),
                Values::I64(values) => Some((*values.get(row)?, 0)),
                Values::Dec(values) => {
                    let d = values.get(row)?;
                    Some((i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?))
                }
                _ => None,
            }
        };
        if !matches!(column.values, Values::Scaled { .. } | Values::I64(_) | Values::Dec(_)) || column.len() < rows {
            return false;
        }
        if self.regions.is_empty() {
            for (row, bits) in out.chunks_exact_mut(words).enumerate().take(rows) {
                bits.copy_from_slice(&self.always);
                if !column.valid(row) {
                    if let Some(matched) = &self.null {
                        Self::or(bits, matched);
                    }
                }
            }
            return true;
        }
        if self.scaled.is_none() {
            return false;
        }
        let mut cached: Vec<Option<Option<Scaling>>> = Vec::new();
        for (row, bits) in out.chunks_exact_mut(words).enumerate().take(rows) {
            if !column.valid(row) {
                bits.copy_from_slice(&self.always);
                if let Some(matched) = &self.null {
                    Self::or(bits, matched);
                }
                continue;
            }
            let fast = parts(row).and_then(|(mant, scale)| {
                let at = scale as usize;
                if cached.len() <= at {
                    cached.resize_with(at + 1, || None);
                }
                let scaling = cached[at].get_or_insert_with(|| self.scaling(scale)).as_ref()?;
                let value = mant.checked_mul(scaling.mult)?;
                Some(Self::region_of(&scaling.bounds, value))
            });
            match fast.and_then(|region| self.regions.get(region)) {
                Some(region) => bits
                    .iter_mut()
                    .zip(self.always.iter().zip(region))
                    .for_each(|(b, (a, r))| *b = a | r),
                None => self.row_bits(column, row, (true, false, false), bits),
            }
        }
        true
    }

    fn set(bits: &mut [u64], rule: usize) {
        bits[rule / 64] |= 1 << (rule % 64);
    }

    fn or(into: &mut [u64], from: &[u64]) {
        into.iter_mut().zip(from).for_each(|(a, b)| *a |= b);
    }

    pub fn compile(cells: &[Option<&str>]) -> Result<Self, IsolateError> {
        let rules = cells.len();
        let words = rules.div_ceil(64).max(1);
        let mut always = vec![0u64; words];
        let mut equal: HashMap<Key, Vec<u64>> = HashMap::default();
        let mut ranges: Vec<(Arc<str>, Vec<usize>)> = Vec::new();
        let mut others: Vec<(Arc<str>, Vec<usize>)> = Vec::new();
        let mut bounds: Vec<Decimal> = Vec::new();

        for (rule, cell) in cells.iter().enumerate() {
            let source = match cell.map(str::trim) {
                None | Some("") => {
                    Self::set(&mut always, rule);
                    continue;
                }
                Some(s) => s,
            };
            let bump = Bump::new();
            let mut lexer = Lexer::new();
            let shape = match lexer.tokenize(&bump, source).ok().and_then(|tokens| {
                Parser::try_new(&tokens, &bump)
                    .ok()
                    .map(|p| p.unary().parse())
            }) {
                Some(parsed) if parsed.error().is_ok() => Shape::classify(parsed.root),
                _ => Shape::Other,
            };
            match shape {
                Shape::Equal(keys) => {
                    for key in keys {
                        let bits = equal.entry(key).or_insert_with(|| vec![0u64; words]);
                        Self::set(bits, rule);
                    }
                }
                Shape::Range(points) => {
                    bounds.extend(points);
                    match ranges.iter_mut().find(|(s, _)| s.as_ref() == source) {
                        Some((_, r)) => r.push(rule),
                        None => ranges.push((Arc::from(source), vec![rule])),
                    }
                }
                Shape::Other => match others.iter_mut().find(|(s, _)| s.as_ref() == source) {
                    Some((_, r)) => r.push(rule),
                    None => others.push((Arc::from(source), vec![rule])),
                },
            }
        }

        let mut nums: Vec<(Decimal, Vec<u64>)> = Vec::new();
        let mut strs: HashMap<Box<[u8]>, Vec<u64>> = HashMap::default();
        let mut bools: [Option<Vec<u64>>; 2] = [None, None];
        let mut null: Option<Vec<u64>> = None;
        for (key, bits) in equal {
            match key {
                Key::Num(n) => {
                    bounds.push(n);
                    nums.push((n, bits));
                }
                Key::Str(s) => {
                    strs.insert(Box::from(s.as_bytes()), bits);
                }
                Key::Bool(b) => bools[usize::from(b)] = Some(bits),
                Key::Null => null = Some(bits),
            }
        }

        bounds.sort();
        bounds.dedup_by(|a, b| a == b);
        let regions = match bounds.is_empty() {
            true => Vec::new(),
            false => {
                let mut isolate = Isolate::new();
                (0..bounds.len() * 2 + 1)
                    .map(|region| {
                        let mut bits = vec![0u64; words];
                        if !ranges.is_empty() {
                            let probe = Self::probe(&bounds, region);
                            isolate.set_environment(Variable::empty_object());
                            let _ = isolate.set_reference_value(Variable::Number(probe));
                            for (source, rules) in &ranges {
                                if isolate.run_unary(source).unwrap_or(false) {
                                    rules.iter().for_each(|r| Self::set(&mut bits, *r));
                                }
                            }
                        }
                        if region % 2 == 1 {
                            for (n, matched) in &nums {
                                if *n == bounds[region / 2] {
                                    Self::or(&mut bits, matched);
                                }
                            }
                        }
                        bits
                    })
                    .collect()
            }
        };

        let others: Vec<Other> = others
            .into_iter()
            .filter_map(|(source, rules)| {
                let mut bits = vec![0u64; words];
                rules.iter().for_each(|r| Self::set(&mut bits, *r));
                LaneProgram::unary(&source).ok().map(|program| Other {
                    program,
                    rules: bits,
                })
            })
            .collect();
        let mut owners = vec![None; rules];
        let mut optimistic = vec![0u64; words];
        for (index, other) in others.iter().enumerate() {
            Self::or(&mut optimistic, &other.rules);
            for (rule, owner) in owners.iter_mut().enumerate() {
                if other.rules[rule / 64] >> (rule % 64) & 1 == 1 {
                    *owner = Some(index);
                }
            }
        }

        Ok(Self {
            rules,
            words,
            always,
            few: (strs.len() <= Self::FEW)
                .then(|| strs.iter().map(|(k, v)| (Self::prefix(k), k.clone(), v.clone())).collect()),
            scaled: Self::scaled_bounds(&bounds),
            strs,
            bools,
            null,
            bounds,
            regions,
            others,
            owners,
            optimistic,
        })
    }

    fn probe(bounds: &[Decimal], region: usize) -> Decimal {
        let i = region / 2;
        if region % 2 == 1 {
            return bounds[i];
        }
        match (i.checked_sub(1).map(|j| bounds[j]), bounds.get(i)) {
            (None, Some(b)) => b.checked_sub(Decimal::ONE).unwrap_or(*b),
            (Some(a), None) => a.checked_add(Decimal::ONE).unwrap_or(a),
            (Some(a), Some(b)) => b
                .checked_sub(a)
                .and_then(|gap| a.checked_add(gap / Decimal::TWO))
                .filter(|m| a < *m && *m < *b)
                .unwrap_or_else(|| a / Decimal::TWO + *b / Decimal::TWO),
            (None, None) => Decimal::ZERO,
        }
    }

    pub fn rules(&self) -> usize {
        self.rules
    }

    pub fn words(&self) -> usize {
        self.words
    }

    pub fn evaluate(
        &self,
        runner: &mut LaneRunner,
        values: &[Variable],
        envs: &[Scope],
        out: &mut Vec<u64>,
    ) {
        let columns = Columns::new(values.len());
        let env = CellEnv {
            scopes: envs,
            columns: &columns,
            bind: &|_| Binding::Row,
        };
        self.evaluate_within(runner, values, &env, None, out)
    }

    pub fn programs(&self) -> impl Iterator<Item = &LaneProgram> {
        self.others.iter().map(|o| &o.program)
    }

    pub fn scopes(values: &[Variable], env: &CellEnv) -> Vec<Scope> {
        values
            .iter()
            .enumerate()
            .map(|(row, value)| Self::scope(value, env.scopes.get(row)))
            .collect()
    }

    fn run(
        program: &LaneProgram,
        runner: &mut LaneRunner,
        scopes: &[Scope],
        env: &CellEnv,
        rows: &[usize],
        mut sink: impl FnMut(usize, bool),
    ) {
        runner.evaluate_bound(program, scopes, env.columns, env.bind, Some(rows), |row, result| {
            sink(row, matches!(result.as_deref(), Ok([Variable::Bool(true)])))
        });
    }

    pub fn evaluate_within(
        &self,
        runner: &mut LaneRunner,
        values: &[Variable],
        env: &CellEnv,
        within: Option<&[u64]>,
        out: &mut Vec<u64>,
    ) {
        let words = self.words;
        self.indexed(values, out);
        if self.others.is_empty() {
            return;
        }
        let scopes = Self::scopes(values, env);
        for other in &self.others {
            let rows: Vec<usize> = match within {
                None => (0..values.len()).collect(),
                Some(mask) => (0..values.len())
                    .filter(|&row| {
                        mask[row * words..(row + 1) * words]
                            .iter()
                            .zip(&other.rules)
                            .any(|(m, r)| m & r != 0)
                    })
                    .collect(),
            };
            if rows.is_empty() {
                continue;
            }
            Self::run(&other.program, runner, &scopes, env, &rows, |row, pass| {
                if pass {
                    Self::or(&mut out[row * words..(row + 1) * words], &other.rules);
                }
            });
        }
    }

    pub fn test(
        &self,
        runner: &mut LaneRunner,
        other: usize,
        scopes: &[Scope],
        env: &CellEnv,
        rows: &[usize],
        mut sink: impl FnMut(usize, bool),
    ) {
        let Some(other) = self.others.get(other) else {
            rows.iter().for_each(|&row| sink(row, false));
            return;
        };
        Self::run(&other.program, runner, scopes, env, rows, sink);
    }

    fn matched(&self, value: &Variable) -> Option<&Vec<u64>> {
        match value {
            Variable::String(s) => self.text(s.as_bytes()),
            Variable::Bool(b) => self.bools[usize::from(*b)].as_ref(),
            Variable::Null => self.null.as_ref(),
            _ => None,
        }
    }

    fn dated(&self, value: &Variable, bits: &mut [u64]) {
        let Variable::Dynamic(dynamic) = value else {
            return;
        };
        let Some(date) = dynamic.as_date() else {
            return;
        };
        let keys: Box<dyn Iterator<Item = (&[u8], &Vec<u64>)>> = match &self.few {
            Some(few) => Box::new(few.iter().map(|(_, k, v)| (k.as_ref(), v))),
            None => Box::new(self.strs.iter().map(|(k, v)| (k.as_ref(), v))),
        };
        for (key, matched) in keys {
            let text = Variable::String(String::from_utf8_lossy(key).as_ref().into());
            if date.matches(&text) {
                Self::or(bits, matched);
            }
        }
    }

    fn region(&self, n: &Decimal) -> Option<&Vec<u64>> {
        if self.regions.is_empty() {
            return None;
        }
        let region = match self.bounds.binary_search(n) {
            Ok(i) => i * 2 + 1,
            Err(i) => i * 2,
        };
        self.regions.get(region)
    }

    pub fn indexed_column(&self, column: &Column, rows: usize, out: &mut Vec<u64>) {
        let words = self.words;
        out.clear();
        out.resize(rows * words, 0);
        let numeric = matches!(column.kind(), Kind::Num);
        let text = matches!(column.kind(), Kind::Str);
        let boolean = matches!(column.values, Values::Bool { .. });
        if column.is_empty() && rows > 0 {
            let (first, rest) = out.split_at_mut(words);
            self.row_bits(column, 0, (numeric, text, boolean), first);
            rest.chunks_exact_mut(words).for_each(|chunk| chunk.copy_from_slice(first));
            return;
        }
        if let Values::Dict { keys, values } = column.values {
            let dictionary = values.column();
            let size = dictionary.len();
            if keys.len() >= rows && size <= rows.saturating_mul(2) + 64 {
                let mut table = Vec::new();
                self.indexed_column(&dictionary, size, &mut table);
                for (row, bits) in out.chunks_exact_mut(words).enumerate() {
                    let code = usize::try_from(keys[row]).ok().filter(|&c| c < size);
                    match (column.valid(row), code) {
                        (true, Some(code)) => bits.copy_from_slice(&table[code * words..(code + 1) * words]),
                        _ => self.row_bits(column, row, (numeric, text, boolean), bits),
                    }
                }
                return;
            }
        }
        if numeric && self.numbers(column, rows, out) {
            return;
        }
        if words == 1 && self.indexed_word(column, rows, out) {
            return;
        }
        for row in 0..rows {
            self.row_bits(column, row, (numeric, text, boolean), &mut out[row * words..(row + 1) * words]);
        }
    }

    fn indexed_word(&self, column: &Column, rows: usize, out: &mut [u64]) -> bool {
        let always = self.always[0];
        let null = self.null.as_ref().map_or(0, |m| m[0]);
        let word = |matched: Option<&Vec<u64>>| matched.map_or(0, |m| m[0]);
        let texts = |offsets: &[i32], data: &[u8], out: &mut [u64]| {
            for (row, bits) in out.iter_mut().enumerate().take(rows) {
                *bits = always
                    | match column.valid(row) {
                        false => null,
                        true => {
                            let (a, b) = (offsets[row] as usize, offsets[row + 1] as usize);
                            word(data.get(a..b).and_then(|t| self.text(t)))
                        }
                    };
            }
        };
        match column.values {
            Values::Bool { bits, offset } => {
                let on = word(self.bools[1].as_ref());
                let off = word(self.bools[0].as_ref());
                for (row, out) in out.iter_mut().enumerate().take(rows) {
                    *out = always
                        | match column.valid(row) {
                            false => null,
                            true => match bits.get((offset + row) / 64).map(|w| w >> ((offset + row) % 64) & 1 == 1) {
                                Some(true) => on,
                                Some(false) => off,
                                None => 0,
                            },
                        };
                }
            }
            Values::Text { offsets, data } if offsets.len() > rows => texts(offsets, data.as_bytes(), out),
            Values::Utf8 { offsets, data } if offsets.len() > rows => texts(offsets, data, out),
            _ => return false,
        }
        true
    }

    fn row_bits(&self, column: &Column, row: usize, (numeric, text, boolean): (bool, bool, bool), bits: &mut [u64]) {
        bits.copy_from_slice(&self.always);
        if !column.valid(row) {
            if let Some(matched) = &self.null {
                Self::or(bits, matched);
            }
            return;
        }
        match (numeric, text, boolean) {
            (true, _, _) => {
                let Some(n) = column.number(row) else {
                    if let Some(matched) = &self.null {
                        Self::or(bits, matched);
                    }
                    return;
                };
                if let Some(region) = self.region(&n) {
                    Self::or(bits, region);
                }
            }
            (_, true, _) => {
                if let Some(matched) = column.bytes(row).and_then(|t| self.text(t)) {
                    Self::or(bits, matched);
                }
            }
            (_, _, true) => {
                if let Some(matched) = column.boolean(row).and_then(|b| self.bools[usize::from(b)].as_ref()) {
                    Self::or(bits, matched);
                }
            }
            _ => {
                let value = column.variable(row);
                if let Some(matched) = self.matched(&value) {
                    Self::or(bits, matched);
                }
                self.dated(&value, bits);
                if let Variable::Number(n) = &value {
                    if let Some(region) = self.region(n) {
                        Self::or(bits, region);
                    }
                }
            }
        }
    }

    pub fn pieces(&self, empty: &[u64]) -> Option<Pieces> {
        if !self.others.is_empty() || self.strs.len() > 64 {
            return None;
        }
        let mut bits: Vec<Vec<u64>> = Vec::new();
        let with = |extra: Option<&Vec<u64>>| {
            let mut piece = self.always.clone();
            if let Some(extra) = extra {
                Self::or(&mut piece, extra);
            }
            piece
        };
        let null = Pieces::intern(&mut bits, with(self.null.as_ref()));
        let truth = [
            Pieces::intern(&mut bits, with(self.bools[0].as_ref())),
            Pieces::intern(&mut bits, with(self.bools[1].as_ref())),
        ];
        let keys = self
            .strs
            .iter()
            .map(|(key, matched)| {
                let piece = Pieces::intern(&mut bits, with(Some(matched)));
                (Pieces::word(key), key.len(), key.clone(), piece)
            })
            .collect();
        let text = Pieces::intern(&mut bits, with(None));
        let regions = self.regions.iter().map(|r| Pieces::intern(&mut bits, with(Some(r)))).collect();
        let other = text;
        let failed = Pieces::intern(&mut bits, empty.to_vec());
        Some(Pieces {
            bits,
            null,
            truth,
            keys,
            text,
            regions,
            other,
            failed,
        })
    }

    fn piece_of(&self, pieces: &Pieces, value: &Variable) -> u16 {
        match value {
            Variable::Null => pieces.null,
            Variable::Bool(b) => pieces.truth[usize::from(*b)],
            Variable::String(s) => pieces.text(s.as_bytes()),
            Variable::Number(n) => self.number_piece(pieces, n),
            Variable::Dynamic(dynamic) => match dynamic.as_date() {
                Some(date) => pieces
                    .keys
                    .iter()
                    .find(|(_, _, key, _)| date.matches(&Variable::String(String::from_utf8_lossy(key).as_ref().into())))
                    .map_or(pieces.other, |(_, _, _, piece)| *piece),
                None => pieces.other,
            },
            _ => pieces.other,
        }
    }

    fn number_piece(&self, pieces: &Pieces, n: &Decimal) -> u16 {
        if pieces.regions.is_empty() {
            return pieces.other;
        }
        let region = match self.bounds.binary_search(n) {
            Ok(i) => i * 2 + 1,
            Err(i) => i * 2,
        };
        pieces.regions.get(region).copied().unwrap_or(pieces.other)
    }

    fn numbers_into(&self, pieces: &Pieces, column: &Column, parts: impl Iterator<Item = Option<(i64, u8)>>, out: &mut [u16]) {
        if pieces.regions.is_empty() {
            for (row, piece) in out.iter_mut().enumerate() {
                *piece = match column.valid(row) {
                    true => pieces.other,
                    false => pieces.null,
                };
            }
            return;
        }
        let mut cached: Vec<Option<Option<Scaling>>> = Vec::new();
        for ((row, piece), part) in out.iter_mut().enumerate().zip(parts) {
            if !column.valid(row) {
                *piece = pieces.null;
                continue;
            }
            let fast = part.and_then(|(mant, scale)| {
                let at = scale as usize;
                if cached.len() <= at {
                    cached.resize_with(at + 1, || None);
                }
                let scaling = cached[at].get_or_insert_with(|| self.scaling(scale)).as_ref()?;
                Some(Self::region_of(&scaling.bounds, mant.checked_mul(scaling.mult)?))
            });
            *piece = match fast {
                Some(region) => pieces.regions.get(region).copied().unwrap_or(pieces.other),
                None => match column.number(row) {
                    Some(n) => self.number_piece(pieces, &n),
                    None => pieces.null,
                },
            };
        }
    }

    pub fn classify(&self, pieces: &Pieces, column: &Column, rows: usize, out: &mut [u16]) {
        let out = &mut out[..rows];
        if column.is_empty() {
            out.fill(pieces.null);
            return;
        }
        match column.values {
            Values::Text { offsets, data } if offsets.len() > rows => {
                let data = data.as_bytes();
                for (row, (piece, pair)) in out.iter_mut().zip(offsets.windows(2)).enumerate() {
                    *piece = match column.valid(row) {
                        false => pieces.null,
                        true => match data.get(pair[0] as usize..pair[1] as usize) {
                            Some(text) => pieces.text(text),
                            None => pieces.text,
                        },
                    };
                }
            }
            Values::Text { .. } | Values::Utf8 { .. } | Values::LargeUtf8 { .. } | Values::Strs(_) => {
                for (row, piece) in out.iter_mut().enumerate() {
                    *piece = match (column.valid(row), column.bytes(row)) {
                        (true, Some(text)) => pieces.text(text),
                        (true, None) => pieces.text,
                        (false, _) => pieces.null,
                    };
                }
            }
            Values::Bool { bits, offset } => {
                for (row, piece) in out.iter_mut().enumerate() {
                    *piece = match column.valid(row) {
                        true => pieces.truth[usize::from(bits.get((offset + row) / 64).is_some_and(|w| w >> ((offset + row) % 64) & 1 == 1))],
                        false => pieces.null,
                    };
                }
            }
            Values::Scaled { mant, scale } if mant.len() >= rows && scale.len() >= rows => {
                self.numbers_into(pieces, column, mant.iter().zip(scale).map(|(m, s)| Some((*m, *s))), out)
            }
            Values::I64(values) if values.len() >= rows => {
                self.numbers_into(pieces, column, values.iter().map(|v| Some((*v, 0u8))), out)
            }
            Values::Dec(values) if values.len() >= rows => self.numbers_into(
                pieces,
                column,
                values
                    .iter()
                    .map(|d| Some((i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?))),
                out,
            ),
            Values::Dict { keys, values } => {
                let dictionary = values.column();
                let size = dictionary.len();
                let mut coded = vec![0u16; size];
                self.classify(pieces, &dictionary, size, &mut coded);
                for (row, piece) in out.iter_mut().enumerate() {
                    let code = keys.get(row).and_then(|k| usize::try_from(*k).ok()).filter(|c| *c < size);
                    *piece = match (column.valid(row), code) {
                        (true, Some(code)) => coded[code],
                        _ => pieces.null,
                    };
                }
            }
            _ => {
                for (row, piece) in out.iter_mut().enumerate() {
                    *piece = self.piece_of(pieces, &column.variable(row));
                }
            }
        }
    }

    pub fn evaluate_indexed_column(&self, column: &Column, rows: usize, out: &mut Vec<u64>) {
        self.indexed_column(column, rows, out);
        for row in 0..rows {
            Self::or(&mut out[row * self.words..(row + 1) * self.words], &self.optimistic);
        }
    }

    pub fn peelable(&self) -> bool {
        self.others.iter().all(|other| {
            let p = other.program.program();
            !p.opaque() && !p.writes_env && p.site_keys.iter().all(|key| key.as_deref() == Some("$"))
        })
    }

    pub fn others(&self) -> usize {
        self.others.len()
    }

    pub fn other(&self, index: usize) -> Option<(&LaneProgram, &[u64])> {
        self.others.get(index).map(|other| (&other.program, other.rules.as_slice()))
    }

    pub fn owner(&self, rule: usize) -> Option<usize> {
        self.owners.get(rule).copied().flatten()
    }

    pub fn evaluate_indexed(&self, values: &[Variable], out: &mut Vec<u64>) {
        self.indexed(values, out);
        for row in 0..values.len() {
            Self::or(&mut out[row * self.words..(row + 1) * self.words], &self.optimistic);
        }
    }

    fn scope(value: &Variable, env: Option<&Scope>) -> Scope {
        let mut scope = Scope::new(env.map_or(Variable::Null, |e| e.base().clone()));
        if let Some(env) = env {
            for (k, v) in env.locals() {
                scope.set_local(k.clone(), v.clone());
            }
        }
        scope.set_local(Variable::dollar_key(), value.clone());
        scope
    }

    fn indexed(&self, values: &[Variable], out: &mut Vec<u64>) {
        let words = self.words;
        out.clear();
        out.resize(values.len() * words, 0);
        for (row, value) in values.iter().enumerate() {
            let bits = &mut out[row * words..(row + 1) * words];
            bits.copy_from_slice(&self.always);
            if let Some(matched) = self.matched(value) {
                Self::or(bits, matched);
            }
            self.dated(value, bits);
            if let (Variable::Number(n), false) = (value, self.regions.is_empty()) {
                let region = match self.bounds.binary_search(n) {
                    Ok(i) => i * 2 + 1,
                    Err(i) => i * 2,
                };
                Self::or(bits, &self.regions[region]);
            }
        }
    }

}
