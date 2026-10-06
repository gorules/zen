mod closure;
mod context;
mod date;
mod export;
mod frame;
mod load;
mod number;
mod text;

use crate::functions::{FunctionKind, InternalFunction};
use crate::lane::builtins::{Arg, Builtins, Out};
use crate::lane::interval::{Interval, IntervalData};
use crate::lane::output::Item;
use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::ops::Ops;
use crate::lane::program::{
    Binary, Const, Input, Kind, NumCmp, ObjectKey, Op, Operand, Program, Reg,
};
use crate::lane::scaled::Scaled;
use crate::variable::Variable;
use rust_decimal::Decimal;
use zen_types::symbol::Symbol;

pub use context::Context;
pub use frame::{CallFault, Fault, Frame};

pub(super) type Parent<'a, M> = (&'a Frame<M>, &'a [u32], &'a [(Reg, Reg)]);

pub struct Executor;

impl Executor {
    fn member_call<M: LaneSet>(dst: Reg, kind: &FunctionKind, args: &[Input], active: M, f: &mut Frame<M>) -> M {
        let (FunctionKind::Internal(InternalFunction::Contains), [Input::Reg(a), Input::Const(id)]) = (kind, args) else {
            return f.none();
        };
        let Some(Variable::String(needle)) = f.consts.get(*id as usize).cloned() else {
            return f.none();
        };
        let (mut word, mut done) = (f.none(), f.none());
        let needle_bytes = needle.as_bytes();
        for lane in Lanes::of(active) {
            if f.kind(*a) == Kind::List && !f.generic(*a, lane) {
                let (lo, hi) = f.lists[f.at(*a, lane)];
                let mut hit = Some(false);
                for i in lo as usize..hi as usize {
                    match f.items.get(i) {
                        Item::Text(x, y) => {
                            if f.arena.as_bytes().get(x as usize..y as usize) == Some(needle_bytes) {
                                hit = Some(true);
                                break;
                            }
                        }
                        Item::Value(Variable::String(s)) if s.as_str() == needle.as_str() => {
                            hit = Some(true);
                            break;
                        }
                        Item::Value(Variable::Dynamic(_)) => {
                            hit = None;
                            break;
                        }
                        _ => {}
                    }
                }
                if let Some(hit) = hit {
                    word.put(lane, hit);
                    done.set(lane);
                }
                continue;
            }
            if !f.generic(*a, lane) {
                continue;
            }
            let Variable::Array(items) = &f.regs[f.at(*a, lane)] else {
                continue;
            };
            let items = items.borrow();
            if items.iter().any(|item| matches!(item, Variable::Dynamic(_))) {
                continue;
            }
            word.put(lane, items.iter().any(|item| matches!(item, Variable::String(s) if s.as_str() == needle.as_str())));
            done.set(lane);
        }
        match f.kind(dst) {
            Kind::Bool => {
                let d = dst as usize;
                f.bits[d] = (f.bits[d] & !done) | word;
                f.boxed[d] &= !done;
            }
            _ => {
                for lane in Lanes::of(done) {
                    f.set(dst, lane, Variable::Bool(word.get(lane)));
                }
            }
        }
        done
    }

    #[inline]
    fn equal_lane<M: LaneSet>(f: &Frame<M>, a: Reg, b: Reg, lane: usize) -> Option<bool> {
        let typed = |reg: Reg, value: &Variable| match (f.kind(reg), value) {
            (Kind::List, _) => Some(false),
            (Kind::Str, Variable::String(s)) => Some(f.text_at(reg, lane) == s.as_str()),
            (Kind::Str, Variable::Dynamic(_)) => None,
            (Kind::Str, _) => Some(false),
            (Kind::Num, Variable::Number(n)) => Some(Ops::same_number(&f.number(reg, lane), n)),
            (Kind::Num, _) => Some(false),
            (Kind::Bool, Variable::Bool(x)) => Some(f.bits[reg as usize].get(lane) == *x),
            (Kind::Bool, _) => Some(false),
            _ => None,
        };
        match (f.generic(a, lane), f.generic(b, lane)) {
            (true, true) => Some(Ops::equal(&f.regs[f.at(a, lane)], &f.regs[f.at(b, lane)])),
            (false, true) => typed(a, &f.regs[f.at(b, lane)]),
            (true, false) => typed(b, &f.regs[f.at(a, lane)]),
            (false, false) => match (f.kind(a), f.kind(b)) {
                (Kind::List, _) | (_, Kind::List) => Some(false),
                (Kind::Str, Kind::Str) => Some(f.text_at(a, lane) == f.text_at(b, lane)),
                (Kind::Num, Kind::Num) => Some(Ops::same_number(&f.number(a, lane), &f.number(b, lane))),
                (Kind::Bool, Kind::Bool) => Some(f.bits[a as usize].get(lane) == f.bits[b as usize].get(lane)),
                _ => None,
            },
        }
    }

    pub fn run<M: LaneSet>(
        program: &Program,
        frames: &mut [Frame<M>],
        ctx: &mut Context,
        rows: &[u32],
    ) {
        let Some((frame, _)) = frames.split_first_mut() else {
            return;
        };
        Self::enter(program, frame, rows, None);
        Self::execute(program, frames, ctx);
    }

    pub fn enter_top<M: LaneSet>(program: &Program, frame: &mut Frame<M>, rows: &[u32]) {
        Self::enter(program, frame, rows, None);
    }

    pub fn execute_top<M: LaneSet>(program: &Program, frames: &mut [Frame<M>], ctx: &mut Context) {
        Self::execute(program, frames, ctx);
    }

    pub(super) fn enter<M: LaneSet>(
        program: &Program,
        frame: &mut Frame<M>,
        rows: &[u32],
        parent: Option<Parent<M>>,
    ) {
        let width = rows.len();
        frame.prepare(program, width);
        frame.rows.clear();
        frame.rows.extend_from_slice(rows);
        frame.dense = parent.is_none()
            && rows
                .iter()
                .zip(0u32..)
                .fold(0u32, |acc, (r, i)| acc | (r ^ i))
                == 0;
        if let Some((parent, parents, imports)) = parent {
            frame.parents.clear();
            frame.parents.extend_from_slice(parents);
            for (from, to) in imports {
                for (lane, p) in parents.iter().enumerate() {
                    let value = parent.value(*from, *p as usize);
                    frame.set(*to, lane, value);
                }
            }
        }
    }

    pub(super) fn execute<M: LaneSet>(
        program: &Program,
        frames: &mut [Frame<M>],
        ctx: &mut Context,
    ) {
        let Some((frame, rest)) = frames.split_first_mut() else {
            return;
        };
        for step in &program.steps {
            if program.isolated && matches!(step.op, Op::Stage { .. }) {
                frame.revive();
            }
            let active = frame.masks[step.mask as usize] & frame.alive;
            if active.is_empty() {
                continue;
            }
            Self::step(&step.op, active, frame, rest, ctx);
        }
    }

    pub(super) fn step<M: LaneSet>(
        op: &Op,
        active: M,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context,
    ) {
        match op {
            Op::Const { dst, id } if matches!(f.kind(*dst), Kind::Num | Kind::Bool) => {
                match (f.kind(*dst), &f.consts[*id as usize]) {
                    (Kind::Num, Variable::Number(n)) => f.fill_num(*dst, active, *n),
                    (Kind::Bool, Variable::Bool(b)) => f.fill_bool(*dst, active, *b),
                    _ => Self::cold(op, active, f, rest, ctx),
                }
            }
            Op::Branch {
                cond,
                opcode,
                on_true,
                on_false,
            } => {
                let (t, e) = f.split(*cond, active, opcode);
                f.masks[*on_true as usize] = t;
                f.masks[*on_false as usize] = e;
            }
            Op::Merge { dst, mask, a, b }
                if f.kind(*dst) == Kind::Bool
                    && f.kind(*a) == Kind::Bool
                    && f.kind(*b) == Kind::Bool =>
            {
                let m = f.masks[*mask as usize];
                let (ta, tb) = (active & m, active & !m);
                let (d, ra, rb) = (*dst as usize, *a as usize, *b as usize);
                let bits = (f.bits[ra] & ta) | (f.bits[rb] & tb);
                let boxed = (f.boxed[ra] & ta) | (f.boxed[rb] & tb);
                for lane in Lanes::of(boxed) {
                    let src = if ta.get(lane) { *a } else { *b };
                    let v = f.take(src, lane);
                    let at = f.at(*dst, lane);
                    f.regs[at] = v;
                }
                f.bits[d] = (f.bits[d] & !active) | bits;
                f.boxed[d] = (f.boxed[d] & !active) | boxed;
            }
            Op::Stage { index } => f.stage = *index,
            Op::Num { dst, op, a, b } => Self::num(*op, *dst, *a, *b, active, f),
            Op::Cmp { dst, op, a, b } => Self::cmp(*op, *dst, *a, *b, active, f),
            Op::Env { site, dst, .. } | Op::Path { site, dst, .. }
                if ctx.column(*site).is_some() =>
            {
                Self::column_load(site, dst, active, f, ctx)
            }
            Op::Env { dst, key, site } => Self::env(dst, key, site, active, f, ctx),
            Op::Path { dst, path, site } => Self::path(dst, path, site, active, f, ctx),
            Op::Field {
                dst,
                src,
                key,
                site,
            } => Self::field(dst, src, key, site, active, f),
            Op::LoadEq {
                dst,
                load,
                site,
                id,
                not,
            } => Self::load_eq(dst, load, site, id, not, active, f, ctx),
            Op::LoadIn { dst, a, load, site } => Self::load_in(dst, a, load, site, active, f, ctx),
            Op::Coalesce { dst, a, id } => Self::coalesce(dst, a, id, active, f),
            Op::LoadCall(c) => Self::load_call(c, active, f, ctx),
            _ => Self::cold(op, active, f, rest, ctx),
        }
    }

    #[inline(never)]
    fn static_object<M: LaneSet>(dst: Reg, pairs: &[(ObjectKey, Reg)], active: M, f: &mut Frame<M>) -> bool {
        if pairs.len() >= 32 {
            return false;
        }
        let mut template = crate::variable::VariableMap::with_capacity(pairs.len());
        for (key, _) in pairs.iter().rev() {
            let ObjectKey::Static(key) = key else {
                return false;
            };
            template.insert(Symbol::from(key.as_ref()), Variable::Null);
        }
        let Some(shape) = template.shape().filter(|_| template.len() == pairs.len()).cloned() else {
            return false;
        };
        for lane in Lanes::of(active) {
            let values: Vec<Variable> = pairs.iter().rev().map(|(_, v)| f.take(*v, lane)).collect();
            f.set(dst, lane, Variable::from_object(crate::variable::VariableMap::from_shape(shape.clone(), values)));
        }
        true
    }

    fn boxed_call<M: LaneSet>(dst: Reg, kind: &FunctionKind, args: &[Input], active: M, f: &mut Frame<M>) -> M {
        let FunctionKind::Internal(function) = kind else {
            return f.none();
        };
        let mut done = f.none();
        match (function, args) {
            (InternalFunction::Contains, [Input::Reg(a), Input::Const(id)]) if f.kind(*a) == Kind::Dyn => {
                let Variable::String(needle) = f.consts[*id as usize].clone() else {
                    return done;
                };
                for lane in Lanes::of(active) {
                    let found = f.with(*a, lane, |value| match value {
                        Variable::String(text) => Some(text.contains(needle.as_ref() as &str)),
                        Variable::Array(items) => {
                            let items = items.borrow();
                            items
                                .iter()
                                .all(|item| matches!(item, Variable::String(_) | Variable::Null))
                                .then(|| items.iter().any(|item| matches!(item, Variable::String(text) if text.as_ref() as &str == needle.as_ref() as &str)))
                        }
                        _ => None,
                    });
                    if let Some(found) = found {
                        f.write(dst, lane, Ok(Out::Bool(found)));
                        done.set(lane);
                    }
                }
            }
            (InternalFunction::Len, [Input::Reg(a)]) if f.kind(*a) == Kind::Dyn => {
                for lane in Lanes::of(active) {
                    let len = f.with(*a, lane, |value| match value {
                        Variable::String(text) => Some(text.len()),
                        Variable::Array(items) => Some(items.borrow().len()),
                        _ => None,
                    });
                    if let Some(len) = len {
                        f.write(dst, lane, Ok(Out::Num(Decimal::from(len))));
                        done.set(lane);
                    }
                }
            }
            _ => {}
        }
        done
    }

    fn identity_call<M: LaneSet>(dst: Reg, kind: &FunctionKind, args: &[Input], active: M, f: &mut Frame<M>) -> M {
        let (FunctionKind::Internal(InternalFunction::Bool), [Input::Reg(a)]) = (kind, args) else {
            return f.none();
        };
        let (a, d) = (*a as usize, dst as usize);
        if f.kind(a as Reg) != Kind::Bool || f.kind(dst) != Kind::Bool {
            return f.none();
        }
        let lanes = active & !f.boxed[a];
        let bits = f.bits[a];
        f.bits[d] = (bits & lanes) | (f.bits[d] & !lanes);
        f.boxed[d] &= !lanes;
        lanes
    }

    pub(super) fn cold<M: LaneSet>(
        op: &Op,
        active: M,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context,
    ) {
        match op {
            Op::Const { dst, id } => {
                let r = *dst as usize;
                let pinned = f.fixed[r];
                let missing = match pinned {
                    true => active & !f.filled[r],
                    false => active,
                };
                if missing.any() {
                    let v = f.consts[*id as usize].clone();
                    match (f.kind(*dst), &v) {
                        (Kind::Str, Variable::String(text)) => {
                            let start = f.arena.len() as u32;
                            f.arena.push_str(text);
                            let span = (start, f.arena.len() as u32);
                            for lane in Lanes::of(missing) {
                                let at = f.at(*dst, lane);
                                f.spans[at] = span;
                            }
                            f.boxed[r] &= !missing;
                        }
                        _ => {
                            for lane in Lanes::of(missing) {
                                let value = match pinned {
                                    true => v.clone(),
                                    false => v.deep_clone(),
                                };
                                f.set(*dst, lane, value);
                            }
                        }
                    }
                    if pinned {
                        f.filled[r] |= missing;
                    }
                }
            }
            Op::SelectConst { dst, cond, a, b } => {
                let (va, vb) = (f.consts[*a as usize].clone(), f.consts[*b as usize].clone());
                let (mut t, mut e) = (f.none(), f.none());
                let mut slow = active;
                if f.kind(*cond) == Kind::Bool {
                    let boxed = f.boxed[*cond as usize];
                    let bits = f.bits[*cond as usize];
                    t = active & !boxed & bits;
                    e = active & !boxed & !bits;
                    slow = active & boxed;
                }
                for lane in Lanes::of(slow) {
                    match f.with(*cond, lane, |v| Ops::truthy(v, "JumpIfFalse")) {
                        Ok(true) => t.set(lane),
                        Ok(false) => e.set(lane),
                        Err(err) => f.fail(lane, err),
                    }
                }
                for (lanes, v) in [(t, va), (e, vb)] {
                    match (f.kind(*dst), v) {
                        (Kind::Num, Variable::Number(n)) => f.fill_num(*dst, lanes, n),
                        (Kind::Str, Variable::String(text)) => f.fill_text(*dst, lanes, &text),
                        (Kind::Bool, Variable::Bool(b)) => f.fill_bool(*dst, lanes, b),
                        (_, v) => {
                            for lane in Lanes::of(lanes) {
                                f.set(*dst, lane, v.clone());
                            }
                        }
                    }
                }
            }
            Op::Not { dst, a } if f.kind(*a) == Kind::Bool && f.kind(*dst) == Kind::Bool => {
                let (d, r) = (*dst as usize, *a as usize);
                let fast = active & !f.boxed[r];
                f.bits[d] = (f.bits[d] & !fast) | (!f.bits[r] & fast);
                f.boxed[d] &= !fast;
                for lane in Lanes::of(active & f.boxed[r]) {
                    let v = Ops::not(f.take(*a, lane));
                    f.put(*dst, lane, v);
                }
            }
            Op::Not { dst, a } => {
                for lane in Lanes::of(active) {
                    let r = Ops::not(f.take(*a, lane));
                    f.put(*dst, lane, r);
                }
            }
            Op::NullBranch { a, null, other } => {
                let typed = active & f.typed(*a);
                let (mut n, mut o) = (f.none(), typed);
                for lane in Lanes::of(active & !typed) {
                    match f.with(*a, lane, |v| matches!(v, Variable::Null)) {
                        true => n.set(lane),
                        false => o.set(lane),
                    }
                }
                f.masks[*null as usize] = n;
                f.masks[*other as usize] = o;
            }
            Op::Merge { dst, mask, a, b } => {
                let m = f.masks[*mask as usize];
                let kind = f.kind(*dst);
                let (ta, tb) = match (kind == f.kind(*a), kind == f.kind(*b)) {
                    _ if kind == Kind::Dyn => (f.none(), f.none()),
                    (ka, kb) => (
                        if ka {
                            active & m & f.typed(*a)
                        } else {
                            f.none()
                        },
                        if kb {
                            active & !m & f.typed(*b)
                        } else {
                            f.none()
                        },
                    ),
                };
                if ta.any() {
                    f.copy_typed(*dst, *a, ta);
                }
                if tb.any() {
                    f.copy_typed(*dst, *b, tb);
                }
                for lane in Lanes::of(active & !ta & !tb) {
                    let src = if m.get(lane) { *a } else { *b };
                    let v = f.take(src, lane);
                    f.set(*dst, lane, v);
                }
            }
            Op::Move { dst, src } => {
                let typed = match f.kind(*dst) == f.kind(*src) {
                    true => active & f.typed(*src),
                    false => f.none(),
                };
                if typed.any() {
                    f.copy_typed(*dst, *src, typed);
                }
                for lane in Lanes::of(active & !typed) {
                    let v = f.value(*src, lane);
                    f.set(*dst, lane, v);
                }
            }
            Op::EqConst {
                dst,
                a,
                value,
                id,
                not,
            } => {
                let d = *dst as usize;
                let mut word = f.none();
                let mut slow = active;
                if let (Kind::Bool, Const::Bool(k)) = (f.kind(*a), value) {
                    let r = *a as usize;
                    let bits = if *k { f.bits[r] } else { !f.bits[r] };
                    word = active & !f.boxed[r] & bits;
                    slow = active & f.boxed[r];
                }
                if let (Kind::Str, Const::String(k)) = (f.kind(*a), value) {
                    let fast = active & !f.boxed[*a as usize];
                    for lane in Lanes::of(fast) {
                        word.put(lane, f.text_at(*a, lane) == k.as_ref());
                    }
                    slow = active & f.boxed[*a as usize];
                }
                if let (Kind::Num, Const::Number(k)) = (f.kind(*a), value) {
                    let fast = active & !f.boxed[*a as usize];
                    let (done, hits) = Self::scaled_cmp(
                        NumCmp::Equal,
                        Operand::Reg(*a),
                        Operand::Num(*k),
                        fast,
                        f,
                    );
                    word |= hits;
                    for lane in Lanes::of(fast & !done) {
                        if Ops::same_number(&f.number(*a, lane), k) {
                            word.set(lane);
                        }
                    }
                    slow = active & f.boxed[*a as usize];
                }
                if slow.any() {
                    let k = f.consts[*id as usize].clone();
                    for lane in Lanes::of(slow) {
                        if f.with(*a, lane, |v| Ops::equal(v, &k)) {
                            word.set(lane);
                        }
                    }
                }
                let word = if *not { !word & active } else { word };
                f.bits[d] = (f.bits[d] & !active) | word;
                f.boxed[d] &= !active;
            }
            Op::InRange {
                dst,
                a,
                lo,
                hi,
                left,
                right,
            } => {
                let interval = Interval {
                    left_bracket: *left,
                    right_bracket: *right,
                    left: IntervalData::Number(*lo),
                    right: IntervalData::Number(*hi),
                };
                let includes = |n: Decimal| {
                    interval
                        .includes(IntervalData::Number(n))
                        .map_err(|err| Ops::error("In", err.to_string()))
                };
                let d = *dst as usize;
                let typed = match f.kind(*a) {
                    Kind::Num => active & !f.boxed[*a as usize],
                    _ => f.none(),
                };
                let (done, mut word) = Self::within(*a, *lo, *hi, *left, *right, typed, f);
                for lane in Lanes::of(typed & !done) {
                    match includes(f.number(*a, lane)) {
                        Ok(h) => word.put(lane, h),
                        Err(e) => f.fail(lane, e),
                    }
                }
                let mut generic = None;
                for lane in Lanes::of(active & !typed) {
                    let r = match f.take(*a, lane) {
                        Variable::Number(n) => includes(n),
                        x => match generic.get_or_insert_with(|| {
                            Ops::interval(
                                &Variable::Number(*lo),
                                &Variable::Number(*hi),
                                *left,
                                *right,
                            )
                        }) {
                            Ok(range) => Ops::membership(x, range),
                            Err(e) => Err(e.clone()),
                        },
                    };
                    match r {
                        Ok(h) => word.put(lane, h),
                        Err(e) => f.fail(lane, e),
                    }
                }
                f.bits[d] = (f.bits[d] & !active) | (word & active);
                f.boxed[d] &= !active;
            }
            Op::EqAny { dst, a, id } => {
                let d = *dst as usize;
                let (done, mut word) = Self::any_number(*a, *id, active, f);
                if let Variable::Array(list) = &f.consts[*id as usize] {
                    let list = list.borrow();
                    for lane in Lanes::of(active & !done) {
                        let hit = f.with(*a, lane, |x| list.iter().any(|k| Ops::equal(x, k)));
                        word.put(lane, hit);
                    }
                }
                f.bits[d] = (f.bits[d] & !active) | word;
                f.boxed[d] &= !active;
            }
            Op::InConst { dst, a, id } => {
                let (done, word) = Self::any_number(*a, *id, active, f);
                let d = *dst as usize;
                f.bits[d] = (f.bits[d] & !done) | word;
                f.boxed[d] &= !done;
                for lane in Lanes::of(active & !done) {
                    let x = f.take(*a, lane);
                    let r = Ops::membership(x, &f.consts[*id as usize]).map(Variable::Bool);
                    f.put(*dst, lane, r);
                }
            }
            Op::RootEnv { dst } => {
                for lane in Lanes::of(active) {
                    let v = ctx.envs.get(f.rows[lane]).materialize();
                    f.set(*dst, lane, v);
                }
            }
            Op::Fetch { dst, a, b } => {
                for lane in Lanes::of(active) {
                    let r = Ops::fetch(f.take(*a, lane), f.take(*b, lane));
                    f.put(*dst, lane, r);
                }
            }
            Op::Negate { dst, a } => Self::negate(*dst, *a, active, f),
            Op::Binary {
                dst,
                op: Binary::Compare(c),
                a,
                b,
            } => {
                for lane in Lanes::of(active) {
                    let (x, y) = (f.take(*a, lane), f.take(*b, lane));
                    match Ops::compared(&x, &y, *c) {
                        Some(hit) => f.set(*dst, lane, Variable::Bool(hit)),
                        None => f.fault(lane, Fault::Unsupported("Compare")),
                    }
                }
            }
            Op::Binary {
                dst,
                op: Binary::Equal,
                a,
                b,
            } => {
                let (mut word, mut done) = (f.none(), f.none());
                for lane in Lanes::of(active) {
                    if let Some(hit) = Self::equal_lane(f, *a, *b, lane) {
                        word.put(lane, hit);
                        done.set(lane);
                    }
                }
                match f.kind(*dst) {
                    Kind::Bool => {
                        let d = *dst as usize;
                        f.bits[d] = (f.bits[d] & !done) | word;
                        f.boxed[d] &= !done;
                    }
                    _ => {
                        for lane in Lanes::of(done) {
                            f.set(*dst, lane, Variable::Bool(word.get(lane)));
                        }
                    }
                }
                for lane in Lanes::of(active & !done) {
                    let (x, y) = (f.take(*a, lane), f.take(*b, lane));
                    f.put(*dst, lane, Ok(Variable::Bool(Ops::equal(&x, &y))));
                }
            }
            Op::Binary { dst, op, a, b } => {
                let unsupported = match op {
                    Binary::Subtract => Some("Subtract"),
                    Binary::Multiply => Some("Multiply"),
                    Binary::Divide => Some("Divide"),
                    Binary::Modulo => Some("Modulo"),
                    _ => None,
                };
                for lane in Lanes::of(active) {
                    let (x, y) = (f.take(*a, lane), f.take(*b, lane));
                    if let (false, Some(opcode)) = (matches!((&x, &y), (Variable::Number(_), Variable::Number(_))), unsupported) {
                        f.fault(lane, Fault::Unsupported(opcode));
                        continue;
                    }
                    let r = match op {
                        Binary::Add => Ops::add(x, y),
                        Binary::Subtract => Ops::subtract(x, y),
                        Binary::Multiply => Ops::multiply(x, y),
                        Binary::Divide => Ops::divide(x, y),
                        Binary::Modulo => Ops::modulo(x, y),
                        Binary::Exponent => Ops::exponent(x, y),
                        Binary::Equal => Ok(Variable::Bool(Ops::equal(&x, &y))),
                        Binary::In => Ops::membership(x, &y).map(Variable::Bool),
                        Binary::Compare(c) => Ops::compare(&x, &y, *c).map(Variable::Bool),
                    };
                    f.put(*dst, lane, r);
                }
            }
            Op::Interval {
                dst,
                a,
                b,
                left,
                right,
            } => {
                for lane in Lanes::of(active) {
                    let r = Ops::interval(&f.value(*a, lane), &f.value(*b, lane), *left, *right);
                    f.put(*dst, lane, r);
                }
            }
            Op::Slice { dst, a, to, from } => {
                for lane in Lanes::of(active) {
                    let r = Ops::slice(f.take(*a, lane), f.take(*to, lane), f.take(*from, lane));
                    f.put(*dst, lane, r);
                }
            }
            Op::Len { dst, a } => {
                for lane in Lanes::of(active) {
                    let r = f.with(*a, lane, Ops::len);
                    f.put(*dst, lane, r);
                }
            }
            Op::Array { dst, items } => {
                for lane in Lanes::of(active) {
                    let arr = items.iter().map(|r| f.take(*r, lane)).collect();
                    f.set(*dst, lane, Variable::from_array(arr));
                }
            }
            Op::Object { dst, pairs } if Self::static_object(*dst, pairs, active, f) => {}
            Op::Object { dst, pairs } => {
                for lane in Lanes::of(active) {
                    let mut map = crate::variable::VariableMap::with_capacity(pairs.len());
                    let mut failed = None;
                    for (k, v) in pairs.iter().rev() {
                        let key = match k {
                            ObjectKey::Static(k) => Ok(Symbol::from(k.as_ref())),
                            ObjectKey::Reg(k) => Ops::object_key(f.take(*k, lane)),
                        };
                        match key {
                            Ok(key) => {
                                map.insert(key, f.take(*v, lane));
                            }
                            Err(e) => {
                                failed = Some(e);
                                break;
                            }
                        }
                    }
                    match failed {
                        Some(e) => f.fail(lane, e),
                        None => f.set(*dst, lane, Variable::from_object(map)),
                    }
                }
            }
            Op::Join { dst, parts } => Self::join(*dst, parts, active, f),
            Op::Extreme {
                dst,
                items,
                largest,
            } => Self::extreme(*dst, items, *largest, active, f),
            Op::Concat { dst, a, b } => {
                let typed = match f.kind(*dst) {
                    Kind::Str => active & !f.boxed[*a as usize] & !f.boxed[*b as usize],
                    _ => f.none(),
                };
                let (ba, bb, bd) = (
                    *a as usize * f.width,
                    *b as usize * f.width,
                    *dst as usize * f.width,
                );
                let mut text = std::mem::take(&mut f.text);
                text.clear();
                let origin = f.arena.len() as u32;
                let (arena, spans) = (f.arena.as_str(), &mut f.spans);
                for lane in Lanes::of(typed) {
                    let before = text.len() as u32;
                    for (x, y) in [spans[ba + lane], spans[bb + lane]] {
                        text.push_str(arena.get(x as usize..y as usize).unwrap_or_default());
                    }
                    spans[bd + lane] = (origin + before, origin + text.len() as u32);
                }
                f.arena.push_str(&text);
                f.text = text;
                f.boxed[*dst as usize] &= !typed;
                for lane in Lanes::of(active & !typed) {
                    let r = Ops::add(f.take(*a, lane), f.take(*b, lane));
                    f.put(*dst, lane, r);
                }
            }
            Op::Call {
                dst,
                kind: FunctionKind::Internal(InternalFunction::String),
                args,
            } if matches!(**args, [Input::Reg(a)] if matches!(f.kind(a), Kind::Str | Kind::Num))
                && f.kind(*dst) == Kind::Str =>
            {
                let [Input::Reg(a)] = **args else { return };
                let typed = match f.kind(a) {
                    Kind::Str => {
                        let typed = active & f.typed(a);
                        f.copy_typed(*dst, a, typed);
                        typed
                    }
                    _ => {
                        let typed = active & !f.boxed[a as usize] & !f.wide[a as usize];
                        let (ba, bd) = (a as usize * f.width, *dst as usize * f.width);
                        for lane in Lanes::of(typed) {
                            let start = f.arena.len() as u32;
                            Scaled::write(f.mant[ba + lane], f.scales[ba + lane], &mut f.arena);
                            f.spans[bd + lane] = (start, f.arena.len() as u32);
                        }
                        f.boxed[*dst as usize] &= !typed;
                        typed
                    }
                };
                let kind = FunctionKind::Internal(InternalFunction::String);
                let Some(builtin) = Builtins::of(&kind) else {
                    return;
                };
                let mut null = None;
                for lane in Lanes::of(active & !typed) {
                    if f.with(a, lane, |value| matches!(value, Variable::Null)) {
                        let r = null.get_or_insert_with(|| Builtins::call(builtin, &kind, &[Arg::Null])).clone();
                        f.write(*dst, lane, r);
                        continue;
                    }
                    let r = {
                        let value = f.arg_view(a, lane);
                        Builtins::call(builtin, &kind, &[value])
                    };
                    f.write(*dst, lane, r);
                }
            }
            Op::Call { dst, kind, args } => {
                let Some(builtin) = Builtins::of(kind) else {
                    let e = Ops::error("CallFunction", format!("Function `{kind}` not found"));
                    for lane in Lanes::of(active) {
                        f.fail(lane, e.clone());
                    }
                    return;
                };
                let mut done = Self::text_call(*dst, kind, args, active, f);
                if done.is_empty() {
                    done = Self::number_call(*dst, kind, args, active, f)
                        | Self::list_call(*dst, kind, args, active, f)
                        | Self::date_call(*dst, kind, args, active, f);
                }
                done |= Self::member_call(*dst, kind, args, active & !done, f);
                done |= Self::identity_call(*dst, kind, args, active & !done, f);
                done |= Self::boxed_call(*dst, kind, args, active & !done, f);
                let mut hint = 0usize;
                let rest = active & !done;
                if rest.any() && args.iter().all(|a| matches!(a, Input::Const(_))) {
                    let result = {
                        let values: smallvec::SmallVec<[Arg; 4]> = args
                            .iter()
                            .filter_map(|a| match *a {
                                Input::Const(id) => Some(Arg::of(&f.consts[id as usize])),
                                Input::Reg(_) => None,
                            })
                            .collect();
                        Builtins::call_hinted(builtin, kind, &values, &mut hint)
                    };
                    for lane in Lanes::of(rest) {
                        let copy = match &result {
                            Ok(Out::Var(value @ (Variable::Array(_) | Variable::Object(_)))) => Ok(Out::Var(value.deep_clone())),
                            other => other.clone(),
                        };
                        f.write_call(*dst, lane, copy);
                    }
                    return;
                }
                f.box_lists(args, active & !done);
                for lane in Lanes::of(active & !done) {
                    let result = {
                        let values: smallvec::SmallVec<[Arg; 4]> = args
                            .iter()
                            .map(|a| match *a {
                                Input::Const(id) => Arg::of(&f.consts[id as usize]),
                                Input::Reg(r) => f.arg_view(r, lane),
                            })
                            .collect();
                        Builtins::call_hinted(builtin, kind, &values, &mut hint)
                    };
                    f.write_call(*dst, lane, result);
                }
            }
            Op::Method { dst, kind, args } => {
                let builtin = Builtins::method(kind);
                f.box_lists(args, active);
                let done = Self::date_method(*dst, kind, args, active, f);
                for lane in Lanes::of(active & !done) {
                    let result = {
                        let values: smallvec::SmallVec<[Arg; 4]> = args
                            .iter()
                            .map(|a| match *a {
                                Input::Const(id) => Arg::of(&f.consts[id as usize]),
                                Input::Reg(r) => f.arg_view(r, lane),
                            })
                            .collect();
                        Builtins::call_method(builtin, kind, &values)
                    };
                    f.write(*dst, lane, result);
                }
            }
            Op::AssignBegin { dst } => {
                for lane in Lanes::of(active) {
                    f.set(*dst, lane, Variable::empty_object());
                }
            }
            Op::AssignStep { object, key, value } => {
                for lane in Lanes::of(active) {
                    let key = match Ops::assigned_key(f.take(*key, lane)) {
                        Ok(k) => k,
                        Err(e) => {
                            f.fail(lane, e);
                            continue;
                        }
                    };
                    let (obj, val) = (f.value(*object, lane), f.take(*value, lane));
                    let r = match ctx.envs.get_mut(f.rows[lane]) {
                        Some(env) => Ops::assign(env, &obj, &key, val),
                        None => Err(Ops::error(
                            "AssignedObjectStep",
                            "Failed to mutate existing env",
                        )),
                    };
                    if let Err(e) = r {
                        f.fail(lane, e);
                    }
                }
            }
            Op::Closure(closure) => Self::closure(closure, active, f, rest, ctx),
            Op::Rewind => {
                for lane in Lanes::of(active) {
                    let row = f.rows[lane];
                    let root = ctx.roots.get(row).clone();
                    if let Some(env) = ctx.envs.get_mut(row) {
                        *env = root;
                    }
                }
            }
            Op::DollarInsert { key, value } => {
                for lane in Lanes::of(active) {
                    let row = f.rows[lane];
                    let v = f.value(*value, lane);
                    let Some(root) = ctx.roots.get_mut(row) else {
                        continue;
                    };
                    let dollar = match root.local(&Variable::dollar_key()) {
                        Some(existing @ Variable::Object(_)) => existing.shallow_clone(),
                        _ => {
                            let created = Variable::empty_object();
                            root.set_local(Variable::dollar_key(), created.clone());
                            created
                        }
                    };
                    let _ = dollar.dot_insert(key, v);
                    if let Some(env) = ctx.envs.get_mut(row) {
                        env.set_local(Variable::dollar_key(), dollar);
                    }
                }
            }
            Op::Fail { error } => {
                for lane in Lanes::of(active) {
                    f.fail(lane, error.clone());
                }
            }
            _ => {}
        }
    }
}
