use super::context::Context;
use super::frame::{Fault, Frame, SEGMENTS};
use super::Executor;
use crate::compiler::FetchFastTarget;
use crate::functions::{FunctionKind, InternalFunction};
use crate::lane::builtins::{Arg, Builtins, ListView, Out, Text, TextKernel};
use crate::lane::columns::{Column, Dictionary, Values};
use crate::lane::output::Item;
use crate::lane::mask::{LaneSet, Lanes, Mask};
use crate::lane::ops::Ops;
use crate::lane::program::{Kind, Load, LoadCall, Reg};
use crate::lane::scaled::Scaled;
use crate::variable::Variable;
use crate::vm::VMError;
use rust_decimal::Decimal;
use std::sync::Arc;
use zen_types::variable::ShapeHint;

impl Executor {
    const MEMO: usize = 256;

    #[inline(never)]
    pub(super) fn load_call<M: LaneSet>(
        c: &LoadCall,
        active: M,
        f: &mut Frame<M>,
        ctx: &mut Context,
    ) {
        let Some(builtin) = Builtins::of(&c.kind) else {
            let e = Ops::error("CallFunction", format!("Function `{}` not found", c.kind));
            for lane in Lanes::of(active) {
                f.fail(lane, e.clone());
            }
            return;
        };
        let consts: smallvec::SmallVec<[Variable; 2]> = c
            .args
            .iter()
            .map(|id| f.consts[*id as usize].clone())
            .collect();
        let mut hint = 0usize;
        let mut call = |first: Arg| -> Result<Out, Fault> {
            let mut args: smallvec::SmallVec<[Arg; 4]> = smallvec::SmallVec::new();
            args.push(first);
            args.extend(consts.iter().map(Arg::of));
            Builtins::call_hinted(builtin, &c.kind, &args, &mut hint)
        };
        let column = ctx.column(c.site);
        if let Some(Some(col)) = column {
            let cheap = matches!(col.values, Values::Dict { .. })
                && f.kind(c.scratch) == Kind::Str
                && matches!(
                    Text::kernel(&c.kind),
                    Some(TextKernel::Test(_) | TextKernel::Size)
                );
            let rows = ctx.columns.map_or(0, |c| c.rows);
            let wide = matches!(col.values, Values::Dict { values, .. } if values.len() > Self::MEMO || values.len() * 4 > rows * 3);
            if cheap
                || wide
                || !matches!(
                    col.values,
                    Values::Dict { .. } | Values::Any(_) | Values::List { .. }
                )
            {
                Self::column_load(&c.site, &c.scratch, active, f, ctx);
                Self::step(&c.call, active, f, &mut [], ctx);
                return;
            }
        }
        let mut done = f.none();
        if let (
            true,
            Some(Some(
                list @ Column {
                    values: Values::List { offsets, child },
                    ..
                },
            )),
        ) = (c.args.is_empty(), column)
        {
            let len = matches!(c.kind, FunctionKind::Internal(InternalFunction::Len));
            let function = Self::reducer(&c.kind);
            let child = &child.column();
            if len || function.is_some() {
                for lane in Lanes::of(active) {
                    let row = ctx.base + f.rows[lane] as usize;
                    let (Some(a), Some(b), true) =
                        (offsets.get(row), offsets.get(row + 1), list.valid(row))
                    else {
                        continue;
                    };
                    let a = usize::try_from(*a).unwrap_or(0);
                    let b = usize::try_from(*b).unwrap_or(0).max(a);
                    let int = match (len, function, child.values) {
                        (true, ..) => i64::try_from(b - a).ok().map(|n| (n, 0)),
                        (false, Some(InternalFunction::Sum), _) if a == b => Some((0, 0)),
                        (false, Some(function), Values::I64(v)) if child.validity.is_none() => v
                            .get(a..b)
                            .and_then(|items| Self::reduce_ints(function, items))
                            .map(|n| (n, 0)),
                        (false, Some(function), Values::Scaled { mant, scale }) if child.validity.is_none() => mant
                            .get(a..b)
                            .zip(scale.get(a..b))
                            .and_then(|(mant, scale)| Self::reduce_scaled(function, mant, scale)),
                        _ => None,
                    };
                    match (int, function, f.kind(c.dst)) {
                        (Some((v, s)), Some(InternalFunction::Avg), _) => {
                            let r = Scaled::decimal(v, s)
                                .checked_div(Decimal::from(b - a))
                                .map(Out::Num);
                            if let Some(out) = r {
                                f.write(c.dst, lane, Ok(out));
                                done.set(lane);
                                continue;
                            }
                        }
                        (Some((v, s)), _, Kind::Num) => {
                            let at = f.at(c.dst, lane);
                            f.mant[at] = v;
                            f.scales[at] = s;
                            f.wide[c.dst as usize].unset(lane);
                            f.boxed[c.dst as usize].unset(lane);
                            done.set(lane);
                            continue;
                        }
                        (Some((v, s)), _, _) => {
                            f.write(c.dst, lane, Ok(Out::Num(Scaled::decimal(v, s))));
                            done.set(lane);
                            continue;
                        }
                        _ => {}
                    }
                    let out = function.and_then(|function| {
                        Self::reduce(function, (a..b).map(|i| Self::child_number(child, i)))
                    });
                    if let (false, Some(out)) = (len, out) {
                        f.write(c.dst, lane, Ok(out));
                        done.set(lane);
                    }
                }
            }
        }
        if let (FunctionKind::Internal(InternalFunction::Contains), [Variable::String(needle)], Some(Some(list @ Column { values: Values::List { offsets, child }, .. }))) =
            (&c.kind, consts.as_slice(), column)
        {
            let child = child.column();
            if matches!(child.values, Values::Text { .. } | Values::Utf8 { .. } | Values::LargeUtf8 { .. } | Values::Strs(_)) {
                let needle = needle.as_bytes();
                for lane in Lanes::of(active & !done) {
                    let row = ctx.base + f.rows[lane] as usize;
                    let (Some(a), Some(b), true) = (offsets.get(row), offsets.get(row + 1), list.valid(row)) else {
                        continue;
                    };
                    let (a, b) = (usize::try_from(*a).unwrap_or(0), usize::try_from(*b).unwrap_or(0));
                    let hit = (a..b).any(|i| child.valid(i) && child.bytes(i) == Some(needle));
                    f.write(c.dst, lane, Ok(Out::Bool(hit)));
                    done.set(lane);
                }
            }
        }
        let slot = c.memo as usize;
        if f.memo.len() <= slot {
            f.memo.resize_with(slot + 1, Vec::new);
        }
        let text = f.kind(c.dst) == Kind::Str;
        let pure = !matches!(c.kind, FunctionKind::Internal(InternalFunction::Rand));
        for lane in Lanes::of(active & !done) {
            let mut share = None;
            let r = match column {
                Some(Some(col)) => {
                    let row = ctx.base + f.rows[lane] as usize;
                    match (col.values, col.valid(row), col.code(row)) {
                        (Values::Dict { values, .. }, true, Some(code)) if text && pure => {
                            let span = Frame::<M>::cached(&mut f.shared, slot, values.len())
                                .get(code)
                                .copied();
                            if let Some(span) = span.filter(|s| *s != Frame::<M>::UNSET) {
                                let at = f.at(c.dst, lane);
                                f.spans[at] = span;
                                f.boxed[c.dst as usize].unset(lane);
                                continue;
                            }
                            share = Some(code);
                            let memo = &mut f.memo[slot];
                            if memo.len() < values.len() {
                                memo.resize(values.len(), None);
                            }
                            match memo.get(code) {
                                Some(Some(done)) => done.clone(),
                                _ => {
                                    let r = Self::with_column_arg(&values.column(), code, &mut call);
                                    if let Some(entry) = f.memo[slot].get_mut(code) {
                                        *entry = Some(r.clone());
                                    }
                                    r
                                }
                            }
                        }
                        (Values::Dict { values, .. }, true, Some(code)) if pure => {
                            let memo = &mut f.memo[slot];
                            if memo.len() < values.len() {
                                memo.resize(values.len(), None);
                            }
                            match memo.get(code) {
                                Some(Some(done)) => done.clone(),
                                _ => {
                                    let r = Self::with_column_arg(&values.column(), code, &mut call);
                                    if let Some(entry) = f.memo[slot].get_mut(code) {
                                        *entry = Some(r.clone());
                                    }
                                    r
                                }
                            }
                        }
                        _ => Self::with_column_arg(col, row, &mut call),
                    }
                }
                Some(None) => call(Arg::Null),
                None => Self::with_row_value(&c.load, c.site, lane, f, ctx, |v| match v {
                    Ok(v) => call(Arg::of(v)),
                    Err(e) => Err(Fault::Vm(e)),
                }),
            };
            f.write_call(c.dst, lane, r);
            if let (Some(code), false, true) =
                (share, f.boxed[c.dst as usize].get(lane), f.alive.get(lane))
            {
                let span = f.spans[f.at(c.dst, lane)];
                if let Some(entry) = f.shared.get_mut(slot).and_then(|e| e.get_mut(code)) {
                    *entry = span;
                }
            }
        }
    }

    pub(super) fn with_column_arg<T>(col: &Column, row: usize, f: impl FnOnce(Arg) -> T) -> T {
        if !col.valid(row) {
            return f(Arg::Null);
        }
        match col.values {
            Values::Utf8 { .. } | Values::Text { .. } | Values::LargeUtf8 { .. } | Values::Strs(_) => match col.text(row)
            {
                Some(text) => f(Arg::Str(text)),
                None => f(Arg::Null),
            },
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => match col.number(row) {
                Some(n) => f(Arg::Num(n)),
                None => f(Arg::Null),
            },
            Values::Bool { .. } => match col.boolean(row) {
                Some(b) => f(Arg::Bool(b)),
                None => f(Arg::Null),
            },
            Values::List { child, .. } => {
                let (start, end) = col.range(row).unwrap_or_default();
                let child = child.column();
                f(Arg::List(ListView { child: &child, start, end }))
            }
            _ => match col.borrowed(row) {
                Some(value) => f(Arg::of(value)),
                None => {
                    let value = col.variable(row);
                    f(Arg::of(&value))
                }
            },
        }
    }

    pub(super) fn with_row_value<T, M: LaneSet>(
        load: &Load,
        site: u16,
        lane: usize,
        f: &mut Frame<M>,
        ctx: &Context,
        then: impl FnOnce(Result<&Variable, VMError>) -> T,
    ) -> T {
        let slot = site as usize * SEGMENTS;
        let row = f.rows[lane];
        let env = ctx.envs.get(row);
        let null = Variable::Null;
        match load {
            Load::Env(key) => match env.local_str(key) {
                Some(v) => then(Ok(v)),
                None => match env.base() {
                    Variable::Object(o) => {
                        let o = o.borrow();
                        then(Ok(o.get_hinted(&mut f.hints[slot], key).unwrap_or(&null)))
                    }
                    Variable::Null => then(Ok(&null)),
                    _ => then(Err(Ops::error("FetchEnv", "Unsupported type"))),
                },
            },
            Load::Path(_) => match Self::row_value(load, site, lane, f, ctx) {
                Ok(v) => then(Ok(&v)),
                Err(e) => then(Err(e)),
            },
        }
    }

    pub(super) fn row_value<M: LaneSet>(
        load: &Load,
        site: u16,
        lane: usize,
        f: &mut Frame<M>,
        ctx: &Context,
    ) -> Result<Variable, VMError> {
        let row = f.rows[lane];
        match load {
            Load::Env(key) => Self::lookup(key, site, row, f, ctx, |_, r| r.cloned()),
            Load::Path(path) => {
                let env = ctx.envs.get(row);
                let slot = site as usize * SEGMENTS;
                Ok(match Self::simple(path) && env.locals().is_empty() {
                    true => {
                        Self::path_with(env.base(), &path[1..], &mut f.hints[slot..], |v| v.clone())
                    }
                    false => Ops::fetch_fast(path, ctx.roots.get(row), env),
                })
            }
        }
    }

    #[inline(never)]
    pub(super) fn column_load<M: LaneSet>(
        site: &u16,
        dst: &Reg,
        active: M,
        f: &mut Frame<M>,
        ctx: &mut Context,
    ) {
        match ctx.column(*site).flatten() {
            Some(
                column @ Column {
                    values: Values::Dict { keys, values },
                    ..
                },
            ) if f.kind(*dst) == Kind::Str => {
                let base = *dst as usize * f.width;
                let mut slow = f.none();
                let cache = Frame::<M>::cached(&mut f.dicts, *site as usize, values.len());
                for lane in Lanes::of(active) {
                    let row = ctx.base + f.rows[lane] as usize;
                    let code = keys.get(row).and_then(|k| usize::try_from(*k).ok());
                    let (Some(code), true) = (code, column.valid(row)) else {
                        slow.set(lane);
                        continue;
                    };
                    let span = match cache.get(code) {
                        Some(span) if *span != Frame::<M>::UNSET => *span,
                        _ => match values.valid(code).then(|| values.text(code)).flatten() {
                            Some(text) => {
                                let start = f.arena.len() as u32;
                                f.arena.push_str(text);
                                let span = (start, f.arena.len() as u32);
                                if let Some(entry) = cache.get_mut(code) {
                                    *entry = span;
                                }
                                span
                            }
                            None => {
                                slow.set(lane);
                                continue;
                            }
                        },
                    };
                    f.spans[base + lane] = span;
                }
                f.boxed[*dst as usize] &= !(active & !slow);
                Self::fill_slow(f, *dst, column, slow, |f, lane| {
                    ctx.base + f.rows[lane] as usize
                });
            }
            Some(column) => Self::fill(f, *dst, column, ctx.base, active),
            None if f.kind(*dst) == Kind::Dyn && active == M::all(f.width) => {
                let base = *dst as usize * f.width;
                f.regs[base..base + f.width].fill(Variable::Null);
            }
            None => {
                for lane in Lanes::of(active) {
                    f.set(*dst, lane, Variable::Null);
                }
            }
        }
    }

    #[inline(never)]
    pub(super) fn env<M: LaneSet>(
        dst: &Reg,
        key: &Arc<str>,
        site: &u16,
        active: M,
        f: &mut Frame<M>,
        ctx: &mut Context,
    ) {
        let slot = *site as usize * SEGMENTS;
        for lane in Lanes::of(active) {
            let env = ctx.envs.get(f.rows[lane]);
            if let Some(v) = env.local_str(key) {
                let v = v.clone();
                f.set(*dst, lane, v);
                continue;
            }
            match env.base() {
                Variable::Object(o) => {
                    let o = o.borrow();
                    match o.get_hinted(&mut f.hints[slot], key) {
                        Some(v) => f.store(*dst, lane, v),
                        None => f.set(*dst, lane, Variable::Null),
                    }
                }
                Variable::Null => f.set(*dst, lane, Variable::Null),
                _ => f.fail(lane, Ops::error("FetchEnv", "Unsupported type")),
            }
        }
    }

    #[inline(never)]
    pub(super) fn path<M: LaneSet>(
        dst: &Reg,
        path: &[FetchFastTarget],
        site: &u16,
        active: M,
        f: &mut Frame<M>,
        ctx: &mut Context,
    ) {
        let slot = *site as usize * SEGMENTS;
        let simple = Self::simple(path);
        let mut hints = std::mem::take(&mut f.hints);
        for lane in Lanes::of(active) {
            let row = f.rows[lane];
            let env = ctx.envs.get(row);
            if simple && env.locals().is_empty() {
                Self::path_with(env.base(), &path[1..], &mut hints[slot..], |v| {
                    f.store(*dst, lane, v)
                });
                continue;
            }
            let v = Ops::fetch_fast(path, ctx.roots.get(row), env);
            f.set(*dst, lane, v);
        }
        f.hints = hints;
    }

    #[inline(never)]
    pub(super) fn field<M: LaneSet>(
        dst: &Reg,
        src: &Reg,
        key: &Arc<str>,
        site: &u16,
        active: M,
        f: &mut Frame<M>,
    ) {
        let slot = *site as usize * SEGMENTS;
        for lane in Lanes::of(active) {
            let object = match f.generic(*src, lane) {
                true => match &f.regs[f.at(*src, lane)] {
                    Variable::Object(o) => Some(o.clone()),
                    _ => None,
                },
                false => None,
            };
            match object {
                Some(o) => {
                    let o = o.borrow();
                    match o.get_hinted(&mut f.hints[slot], key) {
                        Some(v) => f.store(*dst, lane, v),
                        None => f.set(*dst, lane, Variable::Null),
                    }
                }
                None => f.set(*dst, lane, Variable::Null),
            }
        }
    }

    #[inline]
    fn same(x: &[u8], k: &[u8]) -> bool {
        x.len() == k.len() && x.iter().zip(k).all(|(a, b)| a == b)
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn load_eq<M: LaneSet>(
        dst: &Reg,
        load: &Load,
        site: &u16,
        id: &u16,
        not: &bool,
        active: M,
        f: &mut Frame<M>,
        ctx: &mut Context,
    ) {
        let k = f.consts[*id as usize].clone();
        let d = *dst as usize;
        let mut word = f.none();
        match ctx.column(*site) {
            Some(Some(column)) if matches!(column.values, Values::Dict { values, .. } if values.len() <= Self::MEMO) =>
            {
                let mut matched = [0u64; 4];
                if let Values::Dict { values, .. } = column.values {
                    for code in 0..values.len() {
                        matched[code / 64] |= (values.equals(code, &k) as u64) << (code % 64);
                    }
                }
                let null = matches!(k, Variable::Null);
                for lane in Lanes::of(active) {
                    let row = ctx.base + f.rows[lane] as usize;
                    let hit = match (column.valid(row), column.code(row)) {
                        (false, _) => null,
                        (true, Some(code)) => Mask::bit(&matched, code),
                        (true, None) => false,
                    };
                    word.put(lane, hit);
                }
            }
            Some(Some(column)) => {
                let texts = match (&k, f.dense) {
                    (Variable::String(_), true) => column.texts(ctx.base, f.width),
                    _ => None,
                };
                let bytes = match column.values {
                    Values::Text { offsets, data } => Some((offsets, data.as_bytes())),
                    Values::Utf8 { offsets, data } => Some((offsets, data)),
                    _ => None,
                };
                match (texts, bytes, k.as_str()) {
                    (Some(texts), _, Some(k)) => {
                        let valid = active & column.valid_mask(ctx.base, f.width);
                        let k = k.as_bytes();
                        texts.each_bytes(valid, |lane, x| word.put(lane, Self::same(x, k)));
                    }
                    (None, Some((offsets, data)), Some(k)) => {
                        let k = k.as_bytes();
                        for lane in Lanes::of(active) {
                            let row = ctx.base + f.rows[lane] as usize;
                            let hit = column.valid(row)
                                && offsets
                                    .get(row)
                                    .zip(offsets.get(row + 1))
                                    .and_then(|(a, b)| data.get(*a as usize..*b as usize))
                                    .is_some_and(|x| Self::same(x, k));
                            word.put(lane, hit);
                        }
                    }
                    _ => {
                        for lane in Lanes::of(active) {
                            let row = ctx.base + f.rows[lane] as usize;
                            word.put(lane, column.equals(row, &k));
                        }
                    }
                }
            }
            Some(None) => {
                if matches!(k, Variable::Null) {
                    word = active;
                }
            }
            None => {
                let slot = *site as usize * SEGMENTS;
                for lane in Lanes::of(active) {
                    let row = f.rows[lane];
                    let env = ctx.envs.get(row);
                    let hit = match load {
                        Load::Env(key) => Self::lookup(key, *site, row, f, ctx, |_, r| {
                            r.map(|v| Ops::equal(v, &k))
                        }),
                        Load::Path(path) => match Self::simple(path) && env.locals().is_empty() {
                            true => Ok(Self::path_with(
                                env.base(),
                                &path[1..],
                                &mut f.hints[slot..],
                                |v| Ops::equal(v, &k),
                            )),
                            false => Ok(Ops::equal(
                                &Ops::fetch_fast(path, ctx.roots.get(row), env),
                                &k,
                            )),
                        },
                    };
                    match hit {
                        Ok(h) => word.put(lane, h),
                        Err(e) => f.fail(lane, e),
                    }
                }
            }
        }
        let word = if *not { !word & active } else { word & active };
        f.put_mask(d as Reg, active, word);
    }

    #[inline(never)]
    pub(super) fn load_in<M: LaneSet>(
        dst: &Reg,
        a: &Reg,
        load: &Load,
        site: &u16,
        active: M,
        f: &mut Frame<M>,
        ctx: &mut Context,
    ) {
        let list = match ctx.column(*site) {
            Some(Some(column)) => match column.values {
                Values::List { child, .. } if Self::scannable(&child.column()) => Some((column, child.column())),
                _ => None,
            },
            _ => None,
        };
        for lane in Lanes::of(active) {
            let hit = match list {
                Some((column, child)) => {
                    let row = ctx.base + f.rows[lane] as usize;
                    match (column.valid(row), column.range(row)) {
                        (true, Some((from, to))) => {
                            f.with(*a, lane, |needle| Self::scan(needle, &child, from, to))
                        }
                        _ => None,
                    }
                }
                None => None,
            };
            let result = match hit {
                Some(hit) => Ok(hit),
                None => {
                    let haystack = match ctx.column(*site) {
                        Some(Some(column)) => Ok(column.variable(ctx.base + f.rows[lane] as usize)),
                        Some(None) => Ok(Variable::Null),
                        None => Self::row_value(load, *site, lane, f, ctx),
                    };
                    haystack.and_then(|h| Ops::membership(f.take(*a, lane), &h))
                }
            };
            f.put(*dst, lane, result.map(Variable::Bool));
        }
    }

    fn scannable(child: &Column) -> bool {
        matches!(
            child.values,
            Values::Strs(_)
                | Values::Utf8 { .. }
                | Values::Text { .. }
                | Values::LargeUtf8 { .. }
                | Values::I64(_)
                | Values::Dec(_)
                | Values::Scaled { .. }
                | Values::Bool { .. }
        )
    }

    fn scan(needle: &Variable, child: &Column, from: usize, to: usize) -> Option<bool> {
        let mut items = from..to;
        Some(match needle {
            Variable::Null => items.any(|i| !child.valid(i)),
            Variable::Number(n) => items.any(|i| {
                child.valid(i) && child.number(i).is_some_and(|x| Ops::same_number(&x, n))
            }),
            Variable::String(s) => {
                items.any(|i| child.valid(i) && child.text(i) == Some(s.as_ref()))
            }
            Variable::Bool(b) => items.any(|i| child.valid(i) && child.boolean(i) == Some(*b)),
            _ => return None,
        })
    }

    #[inline(never)]
    pub(super) fn coalesce<M: LaneSet>(dst: &Reg, a: &Reg, id: &u16, active: M, f: &mut Frame<M>) {
        let typed = match f.kind(*dst) == f.kind(*a) && f.kind(*a) != Kind::Dyn {
            true => active & f.typed(*a),
            false => f.none(),
        };
        f.copy_typed(*dst, *a, typed);
        let mut null = f.none();
        for lane in Lanes::of(active & !typed) {
            match f.with(*a, lane, |v| matches!(v, Variable::Null)) {
                true => null.set(lane),
                false => {
                    let v = f.take(*a, lane);
                    f.set(*dst, lane, v);
                }
            }
        }
        if null.is_empty() {
            return;
        }
        let value = f.consts[*id as usize].clone();
        if f.fill_typed(*dst, null, &value) {
            return;
        }
        match value {
            fallback @ (Variable::Array(_) | Variable::Object(_)) => {
                for lane in Lanes::of(null) {
                    f.set(*dst, lane, fallback.depth_clone(usize::MAX));
                }
            }
            fallback => {
                for lane in Lanes::of(null) {
                    f.set(*dst, lane, fallback.clone());
                }
            }
        }
    }

    #[inline]
    pub(super) fn lookup<T, M: LaneSet>(
        key: &str,
        site: u16,
        row: u32,
        f: &mut Frame<M>,
        ctx: &Context,
        then: impl FnOnce(&mut Frame<M>, Result<&Variable, VMError>) -> T,
    ) -> T {
        let env = ctx.envs.get(row);
        if let Some(v) = env.local_str(key) {
            return then(f, Ok(v));
        }
        match env.base() {
            Variable::Object(o) => {
                let o = o.borrow();
                match o.get_hinted(&mut f.hints[site as usize * SEGMENTS], key) {
                    Some(v) => then(f, Ok(v)),
                    None => then(f, Ok(&Variable::Null)),
                }
            }
            Variable::Null => then(f, Ok(&Variable::Null)),
            _ => then(f, Err(Ops::error("FetchEnv", "Unsupported type"))),
        }
    }

    pub(super) fn simple(path: &[FetchFastTarget]) -> bool {
        matches!(path.first(), Some(FetchFastTarget::Begin))
            && path.len() <= SEGMENTS
            && path[1..]
                .iter()
                .all(|p| matches!(p, FetchFastTarget::String(_)))
    }

    pub(super) fn path_with<T>(
        base: &Variable,
        path: &[FetchFastTarget],
        hints: &mut [ShapeHint],
        then: impl FnOnce(&Variable) -> T,
    ) -> T {
        let Some((FetchFastTarget::String(key), rest)) = path.split_first() else {
            return then(base);
        };
        let Variable::Object(o) = base else {
            return then(&Variable::Null);
        };
        let o = o.borrow();
        let (hint, tail) = hints.split_at_mut(1);
        match o.get_hinted(&mut hint[0], key) {
            Some(next) => Self::path_with(next, rest, tail, then),
            None => then(&Variable::Null),
        }
    }

    pub(super) fn fill<M: LaneSet>(
        f: &mut Frame<M>,
        dst: Reg,
        column: &Column,
        base: usize,
        active: M,
    ) {
        if f.dense && active == M::all(f.width) && Self::fill_dense(f, dst, column, base) {
            return;
        }
        Self::fill_with(f, dst, column, active, |f, lane| {
            base + f.rows[lane] as usize
        })
    }

    pub(super) fn fill_dense<M: LaneSet>(
        f: &mut Frame<M>,
        dst: Reg,
        column: &Column,
        base: usize,
    ) -> bool {
        let width = f.width;
        let d = dst as usize;
        let at = d * width;
        let all = M::all(width);
        let valid: M = column.valid_mask(base, width);
        let mut slow = all & !valid;
        match (f.kind(dst), column.values) {
            (Kind::Num, Values::I64(values)) => {
                let Some(src) = values.get(base..base + width) else {
                    return false;
                };
                f.mant[at..at + width].copy_from_slice(src);
                f.scales[at..at + width].fill(0);
            }
            (Kind::Num, Values::Scaled { mant, scale }) => {
                let (Some(m), Some(sc)) = (mant.get(base..base + width), scale.get(base..base + width)) else {
                    return false;
                };
                f.mant[at..at + width].copy_from_slice(m);
                f.scales[at..at + width].copy_from_slice(sc);
            }
            (Kind::Num, Values::Dec(values)) => {
                let Some(src) = values.get(base..base + width) else {
                    return false;
                };
                for (lane, n) in src.iter().enumerate() {
                    match Scaled::parts(n) {
                        Some((m, sc)) => {
                            f.mant[at + lane] = m;
                            f.scales[at + lane] = sc;
                        }
                        None => slow.set(lane),
                    }
                }
            }
            (Kind::Bool, Values::Bool { bits, offset }) => {
                f.bits[d] = Column::mask(bits, offset + base, width);
            }
            (Kind::Dyn, Values::Any(values)) => {
                let Some(src) = values.get(base..base + width) else {
                    return false;
                };
                f.regs[at..at + width].clone_from_slice(src);
            }
            (
                Kind::Num,
                Values::Dict {
                    keys,
                    values: Dictionary::Scaled { mant, scale },
                },
            ) => {
                let Some(codes) = keys.get(base..base + width) else {
                    return false;
                };
                for (lane, code) in codes.iter().enumerate() {
                    match usize::try_from(*code).ok().and_then(|c| Some((*mant.get(c)?, *scale.get(c)?))) {
                        Some((m, sc)) => {
                            f.mant[at + lane] = m;
                            f.scales[at + lane] = sc;
                        }
                        None => slow.set(lane),
                    }
                }
            }
            (
                Kind::Bool,
                Values::Dict {
                    keys,
                    values: Dictionary::Bool { bits },
                },
            ) => {
                let Some(codes) = keys.get(base..base + width) else {
                    return false;
                };
                let mut word = f.none();
                for (lane, code) in codes.iter().enumerate() {
                    match usize::try_from(*code).ok().and_then(|c| bits.get(c / 64).map(|w| w >> (c % 64) & 1 == 1)) {
                        Some(b) => word.put(lane, b),
                        None => slow.set(lane),
                    }
                }
                f.bits[d] = word;
            }
            (Kind::Str, Values::Utf8 { .. } | Values::Text { .. }) => {
                let (offsets, text) = match column.values {
                    Values::Text { offsets, data } => {
                        let (Some(first), Some(last)) = (offsets.get(base), offsets.get(base + width)) else {
                            return false;
                        };
                        let Some(text) = data.get(*first as usize..*last as usize) else {
                            return false;
                        };
                        (offsets, text)
                    }
                    Values::Utf8 { offsets, data } => {
                        let (Some(first), Some(last)) = (offsets.get(base), offsets.get(base + width)) else {
                            return false;
                        };
                        let Some(text) = data
                            .get(*first as usize..*last as usize)
                            .and_then(|b| std::str::from_utf8(b).ok())
                        else {
                            return false;
                        };
                        (offsets, text)
                    }
                    _ => return false,
                };
                let first = &offsets[base];
                let window = &offsets[base..=base + width];
                let monotonic = window.windows(2).fold(true, |ok, w| ok & (w[0] <= w[1]));
                let bounded = monotonic
                    && (text.is_ascii()
                        || window.iter().all(|o| {
                            usize::try_from(o - first).is_ok_and(|i| text.is_char_boundary(i))
                        }));
                if !bounded {
                    return false;
                }
                let start = f.arena.len() as i64 - *first as i64;
                f.arena.push_str(text);
                for (lane, pair) in offsets[base..=base + width].windows(2).enumerate() {
                    f.spans[at + lane] = (
                        (start + pair[0] as i64) as u32,
                        (start + pair[1] as i64) as u32,
                    );
                }
            }
            (Kind::Str, Values::Strs(texts)) => {
                let Some(texts) = texts.get(base..base + width) else {
                    return false;
                };
                for (lane, text) in texts.iter().enumerate() {
                    let span = f.intern(text);
                    f.spans[at + lane] = span;
                }
            }
            _ => return false,
        }
        let fast = all & !slow;
        if f.kind(dst) != Kind::Dyn {
            f.boxed[d] &= !fast;
        }
        if f.kind(dst) == Kind::Num {
            f.wide[d] &= !fast;
        }
        Self::fill_slow(f, dst, column, slow, |_, lane| base + lane);
        true
    }

    #[inline]
    pub(super) fn fill_with<M: LaneSet>(
        f: &mut Frame<M>,
        dst: Reg,
        column: &Column,
        active: M,
        index: impl Fn(&Frame<M>, usize) -> usize,
    ) {
        let kind = f.kind(dst);
        let d = dst as usize;
        let at = d * f.width;
        let mut slow = f.none();
        match (kind, column.values) {
            (Kind::Num, Values::Dec(values)) => {
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    match (column.valid(row), values.get(row)) {
                        (true, Some(n)) => match Scaled::parts(n) {
                            Some((m, s)) => {
                                f.mant[at + lane] = m;
                                f.scales[at + lane] = s;
                            }
                            None => slow.set(lane),
                        },
                        _ => slow.set(lane),
                    }
                }
            }
            (Kind::Num, Values::Scaled { mant, scale }) => {
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    match (column.valid(row), mant.get(row), scale.get(row)) {
                        (true, Some(m), Some(s)) => {
                            f.mant[at + lane] = *m;
                            f.scales[at + lane] = *s;
                        }
                        _ => slow.set(lane),
                    }
                }
            }
            (Kind::Num, Values::I64(values)) => {
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    match (column.valid(row), values.get(row)) {
                        (true, Some(n)) => {
                            f.mant[at + lane] = *n;
                            f.scales[at + lane] = 0;
                        }
                        _ => slow.set(lane),
                    }
                }
            }
            (
                Kind::Str,
                Values::Utf8 { .. }
                | Values::Text { .. }
                | Values::LargeUtf8 { .. }
                | Values::Strs(_)
                | Values::Dict { .. },
            ) => {
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    match (column.valid(row), column.text(row)) {
                        (true, Some(t)) => f.put_text(dst, lane, t),
                        _ => slow.set(lane),
                    }
                }
            }
            (
                Kind::Num,
                Values::Dict {
                    keys,
                    values: Dictionary::Scaled { mant, scale },
                },
            ) => {
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    let parts = keys
                        .get(row)
                        .and_then(|code| usize::try_from(*code).ok())
                        .and_then(|c| Some((*mant.get(c)?, *scale.get(c)?)));
                    match (column.valid(row), parts) {
                        (true, Some((m, s))) => {
                            f.mant[at + lane] = m;
                            f.scales[at + lane] = s;
                        }
                        _ => slow.set(lane),
                    }
                }
            }
            (Kind::Dyn, Values::Any(values)) => {
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    match (column.valid(row), values.get(row)) {
                        (true, Some(value)) => f.regs[at + lane] = value.clone(),
                        _ => slow.set(lane),
                    }
                }
            }
            (Kind::Bool, Values::Bool { .. } | Values::Dict { values: Dictionary::Bool { .. }, .. }) => {
                let mut word = f.none();
                for lane in Lanes::of(active) {
                    let row = index(f, lane);
                    match (column.valid(row), column.boolean(row)) {
                        (true, Some(b)) => word.put(lane, b),
                        _ => slow.set(lane),
                    }
                }
                f.bits[d] = (f.bits[d] & !active) | word;
            }
            (Kind::List, Values::List { offsets, child }) => {
                let child = child.column();
                if child.validity.is_some() || !matches!(child.values, Values::Scaled { .. } | Values::Text { .. } | Values::Bool { .. }) {
                    slow = active;
                } else {
                    for lane in Lanes::of(active) {
                        let row = index(f, lane);
                        let range = offsets
                            .get(row)
                            .zip(offsets.get(row + 1))
                            .map(|(a, b)| (*a as usize, (*b as usize).max(*a as usize)));
                        let (true, Some((a, b))) = (column.valid(row), range) else {
                            slow.set(lane);
                            continue;
                        };
                        let start = f.items.len() as u32;
                        match child.values {
                            Values::Scaled { mant, scale } => match mant.get(a..b).zip(scale.get(a..b)) {
                                Some((m, sc)) => f.items.extend_nums(m, sc),
                                None => {
                                    slow.set(lane);
                                    continue;
                                }
                            },
                            Values::Text { offsets: co, data } => {
                                for i in a..b {
                                    let text = co
                                        .get(i)
                                        .zip(co.get(i + 1))
                                        .and_then(|(x, y)| data.get(*x as usize..*y as usize))
                                        .unwrap_or_default();
                                    let (x, y) = f.intern(text);
                                    f.items.push(Item::Text(x, y));
                                }
                            }
                            Values::Bool { bits, offset } => {
                                for i in a..b {
                                    f.items.push(Item::Bool(Column::bit(bits, offset + i)));
                                }
                            }
                            _ => {
                                slow.set(lane);
                                continue;
                            }
                        }
                        f.lists[at + lane] = (start, f.items.len() as u32);
                    }
                }
            }
            _ => slow = active,
        }
        if kind != Kind::Dyn {
            f.boxed[d] &= !(active & !slow);
        }
        if kind == Kind::Num {
            f.wide[d] &= !(active & !slow);
        }
        Self::fill_slow(f, dst, column, slow, index);
    }

    pub(super) fn fill_slow<M: LaneSet>(
        f: &mut Frame<M>,
        dst: Reg,
        column: &Column,
        slow: M,
        index: impl Fn(&Frame<M>, usize) -> usize,
    ) {
        let kind = f.kind(dst);
        for lane in Lanes::of(slow) {
            let row = index(f, lane);
            if !column.valid(row) {
                f.set(dst, lane, Variable::Null);
                continue;
            }
            match kind {
                Kind::Num => match column.number(row) {
                    Some(n) => {
                        f.put_number(dst, lane, n);
                        f.boxed[dst as usize].unset(lane);
                    }
                    None => f.set(dst, lane, column.variable(row)),
                },
                Kind::Bool => match column.boolean(row) {
                    Some(b) => f.set(dst, lane, Variable::Bool(b)),
                    None => f.set(dst, lane, column.variable(row)),
                },
                Kind::Dyn | Kind::Str | Kind::Date | Kind::List => {
                    f.set(dst, lane, column.variable(row))
                }
            }
        }
    }
}
