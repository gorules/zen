use super::closure::Pending;
use crate::lane::builtins::{Arg, Out};
use crate::lane::date::Date;
use crate::lane::mask::{LaneSet, Lanes, Mask};
use crate::lane::ops::Ops;
use crate::lane::output::Items;
use crate::lane::program::{Const, Input, Kind, Operand, Program, Reg};
use crate::lane::scaled::Scaled;
use crate::variable::Variable;
use crate::vm::VMError;
use rust_decimal::Decimal;
use std::rc::Rc;
use zen_types::symbol::Symbol;
use zen_types::variable::{Shape as MapShape, ShapeHint};

#[derive(Debug, Default)]
pub struct Frame<M: LaneSet = Mask> {
    pub(super) program: u64,
    pub(super) capacity: usize,
    pub(super) width: usize,
    pub(super) kinds: Vec<Kind>,
    pub(super) pinned: Vec<bool>,
    pub(super) fixed: Vec<bool>,
    pub(super) filled: Vec<M>,
    pub(super) regs: Vec<Variable>,
    pub(super) mant: Vec<i64>,
    pub(super) spans: Vec<(u32, u32)>,
    pub(super) lists: Vec<(u32, u32)>,
    pub(super) items: Items,
    pub(super) arena: String,
    pub(super) bytes: Vec<u8>,
    pub(super) ranges: Vec<(usize, usize)>,
    pub(super) dicts: Vec<Vec<(u32, u32)>>,
    pub(super) shared: Vec<Vec<(u32, u32)>>,
    pub(super) scales: Vec<u8>,
    pub(super) wide: Vec<M>,
    pub(super) nums: Vec<Decimal>,
    pub(super) dates: Vec<Date>,
    pub(super) bits: Vec<M>,
    pub(super) boxed: Vec<M>,
    pub(super) masks: Vec<M>,
    pub(super) alive: M,
    pub(super) dense: bool,
    pub(super) errors: Vec<Option<Fault>>,
    pub(super) rows: Vec<u32>,
    pub(super) parents: Vec<u32>,
    pub(super) scratch: Scratch,
    pub(super) hints: Vec<ShapeHint>,
    pub(super) consts: Vec<Variable>,
    pub(super) text: String,
    pub(super) buffers: Buffers,
    pub(super) memo: Vec<Vec<Option<Result<Out, Fault>>>>,
    pub(super) owner: u64,
    pub(super) parked: Vec<(u64, Caches)>,
    pub(super) shapes: Vec<Option<Rc<MapShape>>>,
    pub(super) stage: u16,
    pub(super) stages: Vec<u16>,
    pub(super) soft: Vec<(usize, u16, Fault)>,
}

#[derive(Debug, Default)]
pub(super) struct Caches {
    pub(super) memo: Vec<Vec<Option<Result<Out, Fault>>>>,
    pub(super) dicts: Vec<Vec<(u32, u32)>>,
    pub(super) shared: Vec<Vec<(u32, u32)>>,
}

impl Caches {
    pub(super) fn clear_spans(&mut self) {
        self.dicts.iter_mut().for_each(Vec::clear);
        self.shared.iter_mut().for_each(Vec::clear);
    }
}

pub(super) const SEGMENTS: usize = 8;

#[derive(Debug, Default)]
pub(super) struct Buffers {
    pub(super) m: Vec<i64>,
    pub(super) s: Vec<u8>,
    pub(super) ka: Vec<i64>,
    pub(super) sa: Vec<u8>,
    pub(super) kb: Vec<i64>,
    pub(super) sb: Vec<u8>,
}

#[derive(Debug, Default)]
pub(super) struct Scratch {
    pub(super) items: Vec<(u32, Variable)>,
    pub(super) slots: Vec<(u32, u32)>,
    pub(super) truths: Vec<u64>,
    pub(super) settled: Vec<u64>,
    pub(super) spans: Vec<(usize, usize, usize)>,
    pub(super) results: Vec<Variable>,
    pub(super) failures: Vec<(u32, VMError)>,
    pub(super) rows: Vec<u32>,
    pub(super) parents: Vec<u32>,
    pub(super) lists: Vec<(usize, Pending, usize)>,
    pub(super) done: Vec<usize>,
}

#[derive(Debug, Clone)]
pub enum Fault {
    Vm(VMError),
    Unsupported(&'static str),
    Call(Box<CallFault>),
}

#[derive(Debug, Clone)]
pub struct CallFault {
    kind: crate::functions::FunctionKind,
    detail: CallDetail,
}

#[derive(Clone)]
enum CallDetail {
    Message(String),
    Fail(&'static crate::lane::builtins::Builtin, Box<[Variable]>),
}

impl std::fmt::Debug for CallDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallDetail::Message(message) => f.debug_tuple("Message").field(message).finish(),
            CallDetail::Fail(_, values) => f.debug_tuple("Fail").field(values).finish(),
        }
    }
}

impl CallFault {
    pub(crate) fn message(kind: &crate::functions::FunctionKind, message: String) -> Fault {
        Fault::Call(Box::new(CallFault {
            kind: kind.clone(),
            detail: CallDetail::Message(message),
        }))
    }

    pub(crate) fn fail(
        kind: &crate::functions::FunctionKind,
        builtin: &'static crate::lane::builtins::Builtin,
        args: &[crate::lane::builtins::Arg],
    ) -> Fault {
        Fault::Call(Box::new(CallFault {
            kind: kind.clone(),
            detail: CallDetail::Fail(builtin, args.iter().map(|a| a.variable()).collect()),
        }))
    }

    fn error(&self) -> VMError {
        let message = match &self.detail {
            CallDetail::Message(message) => message.clone(),
            CallDetail::Fail(builtin, values) => {
                let args: smallvec::SmallVec<[crate::lane::builtins::Arg; 4]> =
                    values.iter().map(crate::lane::builtins::Arg::of).collect();
                (builtin.fail)(&args)
            }
        };
        crate::lane::ops::Ops::error("CallFunction", format!("Function `{}` failed: {message}", self.kind))
    }
}

impl Fault {
    pub fn vm(self) -> VMError {
        match self {
            Fault::Vm(error) => error,
            Fault::Unsupported(opcode) => crate::lane::ops::Ops::unsupported(opcode),
            Fault::Call(call) => call.error(),
        }
    }
}

impl<M: LaneSet> Frame<M> {
    pub(super) const ARENA: usize = 1 << 20;

    #[inline]
    pub(super) fn text_at(&self, reg: Reg, lane: usize) -> &str {
        let (a, b) = self.spans[self.at(reg, lane)];
        self.arena.get(a as usize..b as usize).unwrap_or_default()
    }

    #[inline]
    pub(super) fn intern(&mut self, text: &str) -> (u32, u32) {
        let start = self.arena.len() as u32;
        self.arena.push_str(text);
        (start, self.arena.len() as u32)
    }

    #[inline]
    pub(super) fn put_scaled(&mut self, reg: Reg, lane: usize, mant: i64, scale: u8) {
        let at = self.at(reg, lane);
        self.mant[at] = mant;
        self.scales[at] = scale;
        self.wide[reg as usize].unset(lane);
        self.boxed[reg as usize].unset(lane);
    }

    #[inline]
    pub(super) fn put_mask(&mut self, reg: Reg, lanes: M, word: M) {
        let r = reg as usize;
        self.bits[r] = (self.bits[r] & !lanes) | word;
        self.boxed[r] &= !lanes;
    }

    #[inline]
    pub(super) fn fill_typed(&mut self, reg: Reg, lanes: M, value: &Variable) -> bool {
        match (self.kind(reg), value) {
            (Kind::Num, Variable::Number(n)) => self.fill_num(reg, lanes, *n),
            (Kind::Str, Variable::String(text)) => self.fill_text(reg, lanes, text),
            (Kind::Bool, Variable::Bool(b)) => self.fill_bool(reg, lanes, *b),
            (Kind::List, Variable::Array(items)) if items.borrow().is_empty() => {
                let start = self.items.len() as u32;
                for lane in Lanes::of(lanes) {
                    let at = self.at(reg, lane);
                    self.lists[at] = (start, start);
                }
                self.boxed[reg as usize] &= !lanes;
            }
            _ => return false,
        }
        true
    }

    #[inline]
    pub(super) fn split(&mut self, cond: Reg, active: M, opcode: &str) -> (M, M) {
        let (mut t, mut e) = (self.none(), self.none());
        let mut slow = active;
        if self.kind(cond) == Kind::Bool {
            let boxed = self.boxed[cond as usize];
            let bits = self.bits[cond as usize];
            t = active & !boxed & bits;
            e = active & !boxed & !bits;
            slow = active & boxed;
        }
        for lane in Lanes::of(slow) {
            match self.with(cond, lane, |v| Ops::truthy(v, opcode)) {
                Ok(true) => t.set(lane),
                Ok(false) => e.set(lane),
                Err(err) => self.fail(lane, err),
            }
        }
        (t, e)
    }

    #[inline]
    pub(super) fn fill_text(&mut self, reg: Reg, lanes: M, text: &str) {
        let span = self.intern(text);
        let base = reg as usize * self.width;
        for lane in Lanes::of(lanes) {
            self.spans[base + lane] = span;
        }
        self.boxed[reg as usize] &= !lanes;
    }

    pub(super) fn box_lists(&mut self, args: &[Input], lanes: M) {
        for input in args {
            let Input::Reg(r) = *input else {
                continue;
            };
            if self.kinds[r as usize] != Kind::List {
                continue;
            }
            for lane in Lanes::of(lanes & !self.boxed[r as usize]) {
                let value = self.list_value(r, lane);
                let at = self.at(r, lane);
                self.regs[at] = value;
                self.boxed[r as usize].set(lane);
            }
        }
    }

    pub(super) fn list_value(&self, reg: Reg, lane: usize) -> Variable {
        let (a, b) = self.lists[self.at(reg, lane)];
        Variable::from_array(
            (a as usize..b as usize)
                .map(|i| self.items.variable(i, &self.arena))
                .collect(),
        )
    }

    #[inline]
    pub(super) fn put_text(&mut self, reg: Reg, lane: usize, text: &str) {
        let span = self.intern(text);
        let at = self.at(reg, lane);
        self.spans[at] = span;
    }

    pub(super) fn park(&mut self, id: u64) {
        if self.owner == id {
            return;
        }
        let current = Caches {
            memo: std::mem::take(&mut self.memo),
            dicts: std::mem::take(&mut self.dicts),
            shared: std::mem::take(&mut self.shared),
        };
        self.parked.push((self.owner, current));
        let next = match self.parked.iter().position(|(owner, _)| *owner == id) {
            Some(i) => self.parked.swap_remove(i).1,
            None => Caches::default(),
        };
        if self.parked.len() > Self::PARKED {
            self.parked.remove(0);
        }
        self.memo = next.memo;
        self.dicts = next.dicts;
        self.shared = next.shared;
        self.owner = id;
    }

    pub(super) fn prepare(&mut self, program: &Program, width: usize) {
        self.park(program.id);
        if self.program != program.id || self.capacity < width {
            let size = program.regs as usize * width.max(self.capacity);
            if self.regs.len() < size {
                self.regs.resize(size, Variable::Null);
                self.mant.resize(size, 0);
                self.spans.resize(size, (0, 0));
                self.lists.resize(size, (0, 0));
                self.scales.resize(size, 0);
                self.nums.resize(size, Decimal::ZERO);
                self.dates.resize(size, Date(None));
            }
            self.capacity = width.max(self.capacity);
            self.kinds.clear();
            self.kinds.extend_from_slice(&program.kinds);
            self.pinned.clear();
            self.pinned.extend_from_slice(&program.pinned);
            self.fixed.clear();
            self.fixed.extend_from_slice(&program.fixed);
            self.consts.clear();
            self.consts
                .extend(program.consts.iter().map(Const::variable));
            let hints = program.sites as usize * SEGMENTS;
            self.hints.clear();
            self.hints.resize(hints, ShapeHint::default());
            self.masks.clear();
            self.masks
                .resize(program.masks as usize + 1, M::none(width));
            if self.errors.len() < self.capacity {
                self.errors.resize(self.capacity, None);
            }
            if self.stages.len() < self.capacity {
                self.stages.resize(self.capacity, 0);
            }
            self.width = usize::MAX;
            self.program = program.id;
            self.shapes.clear();
            if let Some(layout) = &program.layout {
                Self::collect_shapes(layout, &mut self.shapes);
            }
        }
        if self.arena.len() > Self::ARENA || self.items.len() > Self::ARENA {
            self.arena.clear();
            self.items.clear();
            self.dicts.iter_mut().for_each(Vec::clear);
            self.shared.iter_mut().for_each(Vec::clear);
            self.parked
                .iter_mut()
                .for_each(|(_, caches)| caches.clear_spans());
            self.filled.iter_mut().for_each(|w| *w = M::none(width));
        }
        if self.width != width || self.filled.len() != program.regs as usize {
            let regs = program.regs as usize;
            for bits in [
                &mut self.filled,
                &mut self.bits,
                &mut self.boxed,
                &mut self.wide,
            ] {
                bits.clear();
                bits.resize(regs, M::none(width));
            }
        }
        self.width = width;
        self.stage = 0;
        self.soft.clear();
        self.masks.iter_mut().for_each(|m| *m = M::none(width));
        self.alive = M::all(width);
        self.masks[0] = self.alive;
    }

    pub(super) fn restart(&mut self) {
        self.stage = 0;
        self.soft.clear();
        let width = self.width;
        self.masks.iter_mut().for_each(|m| *m = M::none(width));
        self.alive = M::all(self.width);
        self.masks[0] = self.alive;
    }

    pub fn forget(&mut self) {
        self.memo.iter_mut().for_each(Vec::clear);
        self.dicts.iter_mut().for_each(Vec::clear);
        self.shared.iter_mut().for_each(Vec::clear);
        self.parked.clear();
    }

    const PARKED: usize = 8;
    const INLINE_TEXT: usize = 256;

    pub(super) const UNSET: (u32, u32) = (u32::MAX, u32::MAX);

    pub(super) fn cached(
        cache: &mut Vec<Vec<(u32, u32)>>,
        slot: usize,
        size: usize,
    ) -> &mut Vec<(u32, u32)> {
        if cache.len() <= slot {
            cache.resize_with(slot + 1, Vec::new);
        }
        let entries = &mut cache[slot];
        if entries.len() < size {
            entries.resize(size, Self::UNSET);
        }
        entries
    }

    pub fn restrict(&mut self, mask: M) {
        self.masks[0] &= mask;
    }

    pub fn stage_of(&self, lane: usize) -> u16 {
        self.stages[lane]
    }

    pub fn active(&self, lane: usize) -> bool {
        self.masks[0].get(lane)
    }

    pub fn outputs(&self, program: &Program, lane: usize) -> Vec<Variable> {
        program
            .outputs
            .iter()
            .map(|r| self.value(*r, lane))
            .collect()
    }

    #[inline]
    pub(super) fn none(&self) -> M {
        M::none(self.width)
    }

    #[inline]
    pub(super) fn at(&self, reg: Reg, lane: usize) -> usize {
        reg as usize * self.width + lane
    }

    #[inline]
    pub(super) fn kind(&self, reg: Reg) -> Kind {
        self.kinds[reg as usize]
    }

    #[inline]
    pub(super) fn generic(&self, reg: Reg, lane: usize) -> bool {
        matches!(self.kinds[reg as usize], Kind::Dyn) || self.boxed[reg as usize].get(lane)
    }

    #[inline]
    pub(super) fn value(&self, reg: Reg, lane: usize) -> Variable {
        let at = self.at(reg, lane);
        match self.generic(reg, lane) {
            true => self.regs[at].clone(),
            false => match self.kinds[reg as usize] {
                Kind::Num => Variable::Number(self.number(reg, lane)),
                Kind::Str => Variable::String(Symbol::from(self.text_at(reg, lane))),
                Kind::List => self.list_value(reg, lane),
                Kind::Date => self.dates[at].variable(),
                _ => Variable::Bool(self.bits[reg as usize].get(lane)),
            },
        }
    }

    #[inline]
    pub(super) fn with<T>(&self, reg: Reg, lane: usize, f: impl FnOnce(&Variable) -> T) -> T {
        match self.generic(reg, lane) {
            true => f(&self.regs[self.at(reg, lane)]),
            false => f(&self.value(reg, lane)),
        }
    }

    #[inline]
    pub(super) fn take(&mut self, reg: Reg, lane: usize) -> Variable {
        if self.pinned[reg as usize] {
            return self.value(reg, lane);
        }
        match self.generic(reg, lane) {
            true => {
                let at = self.at(reg, lane);
                std::mem::replace(&mut self.regs[at], Variable::Null)
            }
            false => self.value(reg, lane),
        }
    }

    #[inline]
    pub(super) fn set(&mut self, reg: Reg, lane: usize, value: Variable) {
        let at = self.at(reg, lane);
        let r = reg as usize;
        match (self.kinds[r], value) {
            (Kind::Dyn, v) => self.regs[at] = v,
            (Kind::Str, Variable::String(text)) if text.len() <= Self::INLINE_TEXT => {
                self.put_text(reg, lane, &text);
                self.boxed[r].unset(lane);
            }
            (Kind::Num, Variable::Number(n)) => {
                self.put_number(reg, lane, n);
                self.boxed[r].unset(lane);
            }
            (Kind::Bool, Variable::Bool(b)) => {
                self.bits[r].put(lane, b);
                self.boxed[r].unset(lane);
            }
            (Kind::Date, v) => match Date::of(&v).filter(|_| !Date::sourced(&v)) {
                Some(d) => {
                    self.dates[at] = d;
                    self.boxed[r].unset(lane);
                }
                None => {
                    self.regs[at] = v;
                    self.boxed[r].set(lane);
                }
            },
            (_, v) => {
                self.regs[at] = v;
                self.boxed[r].set(lane);
            }
        }
    }

    #[inline]
    pub(super) fn store(&mut self, reg: Reg, lane: usize, value: &Variable) {
        let r = reg as usize;
        match (self.kinds[r], value) {
            (Kind::Num, Variable::Number(n)) => {
                self.put_number(reg, lane, *n);
                self.boxed[r].unset(lane);
            }
            (Kind::Str, Variable::String(text)) if text.len() <= Self::INLINE_TEXT => {
                self.put_text(reg, lane, text);
                self.boxed[r].unset(lane);
            }
            (Kind::Bool, Variable::Bool(b)) => {
                self.bits[r].put(lane, *b);
                self.boxed[r].unset(lane);
            }
            (_, v) => self.set(reg, lane, v.clone()),
        }
    }

    pub(super) fn arg_view(&self, reg: Reg, lane: usize) -> Arg<'_> {
        let r = reg as usize;
        let boxed = self.boxed[r].get(lane);
        match (self.kinds[r], boxed) {
            (Kind::Num, false) => Arg::Num(self.number(reg, lane)),
            (Kind::Bool, false) => Arg::Bool(self.bits[r].get(lane)),
            (Kind::Str, false) => Arg::Str(self.text_at(reg, lane)),
            (Kind::Date, false) => Arg::Date(self.dates[self.at(reg, lane)]),
            _ => Arg::of(&self.regs[self.at(reg, lane)]),
        }
    }

    #[inline]
    pub(super) fn fill_num(&mut self, reg: Reg, active: M, n: Decimal) {
        let base = reg as usize * self.width;
        let r = reg as usize;
        match (Scaled::parts(&n), active == M::all(self.width)) {
            (Some((m, s)), true) => {
                self.mant[base..base + self.width].fill(m);
                self.scales[base..base + self.width].fill(s);
                self.wide[r] &= !active;
            }
            (Some((m, s)), false) => {
                for lane in Lanes::of(active) {
                    self.mant[base + lane] = m;
                    self.scales[base + lane] = s;
                }
                self.wide[r] &= !active;
            }
            (None, _) => {
                Lanes::of(active).for_each(|lane| self.nums[base + lane] = n);
                self.wide[r] |= active;
            }
        }
        self.boxed[r] &= !active;
    }

    #[inline]
    pub(super) fn number(&self, reg: Reg, lane: usize) -> Decimal {
        let at = self.at(reg, lane);
        match self.wide[reg as usize].get(lane) {
            true => self.nums[at],
            false => Scaled::decimal(self.mant[at], self.scales[at]),
        }
    }

    #[inline]
    pub(super) fn put_number(&mut self, reg: Reg, lane: usize, n: Decimal) {
        let at = self.at(reg, lane);
        match Scaled::parts(&n) {
            Some((m, s)) => {
                self.mant[at] = m;
                self.scales[at] = s;
                self.wide[reg as usize].unset(lane);
            }
            None => {
                self.nums[at] = n;
                self.wide[reg as usize].set(lane);
            }
        }
    }

    #[inline]
    pub(super) fn fill_bool(&mut self, reg: Reg, active: M, b: bool) {
        let word = if b { active } else { self.none() };
        self.put_mask(reg, active, word);
    }

    #[inline]
    pub(super) fn typed(&self, reg: Reg) -> M {
        match self.kinds[reg as usize] {
            Kind::Dyn => self.none(),
            _ => !self.boxed[reg as usize],
        }
    }

    #[inline]
    pub(super) fn copy_typed(&mut self, dst: Reg, src: Reg, lanes: M) {
        let (d, s) = (dst as usize, src as usize);
        let (db, sb) = (d * self.width, s * self.width);
        if lanes == M::all(self.width) && d != s {
            let width = self.width;
            match self.kinds[s] {
                Kind::Num => {
                    self.mant.copy_within(sb..sb + width, db);
                    self.scales.copy_within(sb..sb + width, db);
                    for lane in Lanes::of(self.wide[s]) {
                        self.nums[db + lane] = self.nums[sb + lane];
                    }
                    self.wide[d] = self.wide[s];
                }
                Kind::Str => self.spans.copy_within(sb..sb + width, db),
                Kind::List => self.lists.copy_within(sb..sb + width, db),
                Kind::Date => {
                    for lane in 0..width {
                        self.dates[db + lane] = self.dates[sb + lane];
                    }
                }
                _ => self.bits[d] = self.bits[s],
            }
            self.boxed[d] &= !lanes;
            return;
        }
        match self.kinds[s] {
            Kind::Num => {
                for w in 0..lanes.words() {
                    let (word, at) = (lanes.word(w), w * 64);
                    match word {
                        0 => {}
                        u64::MAX => {
                            self.mant.copy_within(sb + at..sb + at + 64, db + at);
                            self.scales.copy_within(sb + at..sb + at + 64, db + at);
                        }
                        mut bits => {
                            while bits != 0 {
                                let lane = at + bits.trailing_zeros() as usize;
                                self.mant[db + lane] = self.mant[sb + lane];
                                self.scales[db + lane] = self.scales[sb + lane];
                                bits &= bits - 1;
                            }
                        }
                    }
                }
                for lane in Lanes::of(lanes & self.wide[s]) {
                    self.nums[db + lane] = self.nums[sb + lane];
                }
                self.wide[d] = (self.wide[d] & !lanes) | (self.wide[s] & lanes);
            }
            Kind::Str => {
                for lane in Lanes::of(lanes) {
                    self.spans[db + lane] = self.spans[sb + lane];
                }
            }
            Kind::List => {
                for lane in Lanes::of(lanes) {
                    self.lists[db + lane] = self.lists[sb + lane];
                }
            }
            Kind::Date => {
                for lane in Lanes::of(lanes) {
                    self.dates[db + lane] = self.dates[sb + lane];
                }
            }
            _ => self.bits[d] = (self.bits[d] & !lanes) | (self.bits[s] & lanes),
        }
        self.boxed[d] &= !lanes;
    }

    pub(super) fn block(spans: &[(u32, u32)]) -> Option<(usize, usize)> {
        let (first, last) = (spans.first()?, spans.last()?);
        spans
            .windows(2)
            .all(|w| w[0].1 == w[1].0)
            .then_some((first.0 as usize, last.1 as usize))
    }

    #[inline]
    pub(super) fn num(&self, o: Operand, lane: usize) -> Decimal {
        match o {
            Operand::Num(n) => n,
            Operand::Reg(r) => self.number(r, lane),
        }
    }

    #[inline]
    pub(super) fn operand_boxed(&self, o: Operand) -> M {
        match o {
            Operand::Num(_) => self.none(),
            Operand::Reg(r) => match self.kinds[r as usize] {
                Kind::Num => self.boxed[r as usize],
                _ => M::all(self.width),
            },
        }
    }

    #[inline]
    pub(super) fn side<'s>(
        &'s self,
        o: Operand,
        m: &'s mut Vec<i64>,
        sc: &'s mut Vec<u8>,
    ) -> Option<(&'s [i64], &'s [u8])> {
        let width = self.width;
        match o {
            Operand::Num(n) => {
                let (mm, ss) = Scaled::parts(&n)?;
                if m.len() < width {
                    m.resize(width, 0);
                    sc.resize(width, 0);
                }
                m[..width].fill(mm);
                sc[..width].fill(ss);
                Some((&m[..width], &sc[..width]))
            }
            Operand::Reg(r) => {
                let base = r as usize * width;
                Some((
                    &self.mant[base..base + width],
                    &self.scales[base..base + width],
                ))
            }
        }
    }

    #[inline]
    pub(super) fn operand_wide(&self, o: Operand) -> M {
        match o {
            Operand::Num(_) => self.none(),
            Operand::Reg(r) => self.wide[r as usize],
        }
    }

    #[inline]
    pub(super) fn uniform(s: &[u8], lanes: M) -> Option<u8> {
        let first = *s.get(lanes.first()?)?;
        if lanes == M::all(s.len()) {
            return (s.iter().fold(0u8, |acc, x| acc | (x ^ first)) == 0).then_some(first);
        }
        let mixed = s
            .iter()
            .enumerate()
            .fold(0u8, |acc, (lane, x)| acc | ((x ^ first) & (lanes.get(lane) as u8).wrapping_neg()));
        (mixed == 0).then_some(first)
    }

    #[inline]
    pub(super) fn operand_value(&mut self, o: Operand, lane: usize) -> Variable {
        match o {
            Operand::Num(n) => Variable::Number(n),
            Operand::Reg(r) => self.take(r, lane),
        }
    }

    #[inline]
    pub(super) fn fail(&mut self, lane: usize, error: VMError) {
        self.fault(lane, Fault::Vm(error));
    }

    pub(super) fn revive(&mut self) {
        let dead = self.masks[0] & !self.alive;
        for lane in Lanes::of(dead) {
            if let Some(fault) = self.errors[lane].take() {
                self.soft.push((lane, self.stages[lane], fault));
            }
        }
        self.alive |= dead;
    }

    pub(super) fn fault(&mut self, lane: usize, fault: Fault) {
        if self.alive.get(lane) {
            self.alive.unset(lane);
            self.errors[lane] = Some(fault);
            self.stages[lane] = self.stage;
        }
    }

    #[inline]
    pub(super) fn write(&mut self, reg: Reg, lane: usize, result: Result<Out, VMError>) {
        let r = reg as usize;
        match (self.kinds[r], result) {
            (Kind::Num, Ok(Out::Num(n))) => {
                self.put_number(reg, lane, n);
                self.boxed[r].unset(lane);
            }
            (Kind::Bool, Ok(Out::Bool(b))) => {
                self.bits[r].put(lane, b);
                self.boxed[r].unset(lane);
            }
            (Kind::Date, Ok(Out::Date(d))) => {
                let at = self.at(reg, lane);
                self.dates[at] = d;
                self.boxed[r].unset(lane);
            }
            (_, Ok(out)) => self.set(reg, lane, out.variable()),
            (_, Err(e)) => self.fail(lane, e),
        }
    }

    #[inline]
    pub(super) fn write_call(&mut self, reg: Reg, lane: usize, result: Result<Out, Fault>) {
        match result {
            Ok(out) => self.write(reg, lane, Ok(out)),
            Err(fault) => self.fault(lane, fault),
        }
    }

    #[inline]
    pub(super) fn put(&mut self, reg: Reg, lane: usize, result: Result<Variable, VMError>) {
        match result {
            Ok(v) => self.set(reg, lane, v),
            Err(e) => self.fail(lane, e),
        }
    }
}
