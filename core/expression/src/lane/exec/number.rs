use super::frame::Fault;
use super::frame::{Buffers, Frame};
use super::Executor;
use crate::compiler::Compare;
use crate::functions::{FunctionKind, InternalFunction};
use crate::lane::builtins::{Arg, Builtins, Out};
use crate::lane::columns::{Column, Values};
use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::ops::Ops;
use crate::lane::program::{Input, Kind, NumCmp, NumOp, Operand, Reg};
use crate::lane::scaled::{Kernel, Scaled};
use crate::lexer::Bracket;
use crate::variable::Variable;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

impl Executor {
    pub(super) fn reducer(kind: &FunctionKind) -> Option<InternalFunction> {
        match kind {
            FunctionKind::Internal(
                function @ (InternalFunction::Sum
                | InternalFunction::Avg
                | InternalFunction::Min
                | InternalFunction::Max),
            ) => Some(*function),
            _ => None,
        }
    }

    pub(super) fn reduce(
        function: InternalFunction,
        mut values: impl Iterator<Item = Option<(i64, u8)>>,
    ) -> Option<Out> {
        let first = values.next()??;
        let (mut acc, mut len) = (first, 1usize);
        if matches!(function, InternalFunction::Sum | InternalFunction::Avg) {
            acc = Scaled::add((0, 0), first, false)?;
        }
        for value in values {
            let x = value?;
            len += 1;
            acc = match function {
                InternalFunction::Sum | InternalFunction::Avg => Scaled::add(acc, x, false)?,
                InternalFunction::Max if Scaled::compare(x, acc)?.is_ge() => x,
                InternalFunction::Min if Scaled::compare(x, acc)?.is_lt() => x,
                _ => acc,
            };
        }
        let total = Scaled::decimal(acc.0, acc.1);
        Some(Out::Num(match function {
            InternalFunction::Avg => total.checked_div(Decimal::from(len))?,
            _ => total,
        }))
    }

    pub(super) fn reduce_items(
        function: InternalFunction,
        numbers: Option<(&[i64], &[u8])>,
    ) -> Option<(i64, u8)> {
        let (mant, scales) = numbers?;
        let scale = *scales.first()?;
        let uniform = scales.iter().all(|s| *s == scale);
        match function {
            InternalFunction::Sum if uniform => match Self::reduce_ints(function, mant)? {
                0 if scale > 0 => None,
                total => Some((total, scale)),
            },
            InternalFunction::Min | InternalFunction::Max if uniform => {
                Some((Self::reduce_ints(function, mant)?, scale))
            }
            _ => None,
        }
    }

    pub(super) fn reduce_ints(function: InternalFunction, items: &[i64]) -> Option<i64> {
        let first = *items.first()?;
        match function {
            InternalFunction::Min => Some(items.iter().copied().fold(first, i64::min)),
            InternalFunction::Max => Some(items.iter().copied().fold(first, i64::max)),
            _ => i64::try_from(items.iter().map(|x| *x as i128).sum::<i128>()).ok(),
        }
    }

    pub(super) fn reduce_scaled(function: InternalFunction, mant: &[i64], scale: &[u8]) -> Option<(i64, u8)> {
        let (&first, &unit) = (mant.first()?, scale.first()?);
        if scale.iter().any(|s| *s != unit) {
            return None;
        }
        match function {
            InternalFunction::Min => Some((mant.iter().copied().fold(first, i64::min), unit)),
            InternalFunction::Max => Some((mant.iter().copied().fold(first, i64::max), unit)),
            _ => i64::try_from(mant.iter().map(|x| *x as i128).sum::<i128>()).ok().map(|n| (n, unit)),
        }
    }

    pub(super) fn child_number(child: &Column, index: usize) -> Option<(i64, u8)> {
        match (child.values, child.valid(index)) {
            (Values::I64(v), true) => v.get(index).map(|n| (*n, 0)),
            (Values::Dec(v), true) => v.get(index).and_then(Scaled::parts),
            (Values::Scaled { mant, scale }, true) => Some((*mant.get(index)?, *scale.get(index)?)),
            _ => None,
        }
    }

    pub(super) fn list_call<M: LaneSet>(
        dst: Reg,
        kind: &FunctionKind,
        args: &[Input],
        active: M,
        f: &mut Frame<M>,
    ) -> M {
        let (Some(function), [Input::Reg(a)]) = (Self::reducer(kind), args) else {
            return f.none();
        };
        if f.kind(*a) != Kind::List {
            return f.none();
        }
        let mut done = f.none();
        for lane in Lanes::of(active & !f.boxed[*a as usize]) {
            let (x, y) = f.lists[f.at(*a, lane)];
            if let Some((v, scale)) =
                Self::reduce_items(function, f.items.numbers(x as usize, y as usize))
            {
                match f.kind(dst) {
                    Kind::Num => {
                        f.put_scaled(dst, lane, v, scale);
                    }
                    _ => f.write(dst, lane, Ok(Out::Num(Scaled::decimal(v, scale)))),
                }
                done.set(lane);
                continue;
            }
            let values = (x as usize..y as usize).map(|i| f.items.number(i));
            if let Some(out) = Self::reduce(function, values) {
                f.write(dst, lane, Ok(out));
                done.set(lane);
            }
        }
        done
    }

    pub(super) fn number_call<M: LaneSet>(
        dst: Reg,
        kind: &FunctionKind,
        args: &[Input],
        active: M,
        f: &mut Frame<M>,
    ) -> M {
        let FunctionKind::Internal(function) = kind else {
            return f.none();
        };
        let (a, places) = match args {
            [Input::Reg(a)] => (*a, None),
            [Input::Reg(a), Input::Const(id)] => match &f.consts[*id as usize] {
                Variable::Number(n) => match n.to_u32().filter(|p| *p <= 28) {
                    Some(p) => (*a, Some(p as u8)),
                    None => return f.none(),
                },
                _ => return f.none(),
            },
            _ => return f.none(),
        };
        let op: fn(i64, u8, u8) -> Option<(i64, u8)> = match (function, places) {
            (InternalFunction::Abs, None) => |m, s, _| Scaled::abs(m, s),
            (InternalFunction::Floor, None) => |m, s, _| Scaled::floor(m, s),
            (InternalFunction::Ceil, None) => |m, s, _| Scaled::ceil(m, s),
            (InternalFunction::Round, _) => Scaled::round,
            (InternalFunction::Trunc, _) => Scaled::trunc,
            _ => return f.none(),
        };
        if f.kind(a) != Kind::Num || f.kind(dst) != Kind::Num {
            return f.none();
        }
        let places = places.unwrap_or(0);
        let lanes = active & !f.boxed[a as usize] & !f.wide[a as usize];
        let (from, base) = (a as usize * f.width, dst as usize * f.width);
        let mut done = f.none();
        for lane in Lanes::of(lanes) {
            if let Some((m, s)) = op(f.mant[from + lane], f.scales[from + lane], places) {
                f.mant[base + lane] = m;
                f.scales[base + lane] = s;
                done.set(lane);
            }
        }
        f.wide[dst as usize] &= !done;
        f.boxed[dst as usize] &= !done;
        done
    }

    #[inline(never)]
    pub(super) fn scaled_num<M: LaneSet>(
        op: NumOp,
        dst: Reg,
        a: Operand,
        b: Operand,
        lanes: M,
        f: &mut Frame<M>,
    ) -> M {
        let ok = lanes & !f.operand_wide(a) & !f.operand_wide(b);
        if ok.is_empty() {
            return f.none();
        }
        let width = f.width;
        let mut buffers = std::mem::take(&mut f.buffers);
        let Buffers {
            m,
            s,
            ka,
            sa,
            kb,
            sb,
        } = &mut buffers;
        if m.len() < width {
            m.resize(width, 0);
            s.resize(width, 0);
        }
        let (m, s) = (&mut m[..width], &mut s[..width]);
        let mut done = f.none();
        if let (Some((xa, ya)), Some((xb, yb))) = (f.side(a, ka, sa), f.side(b, kb, sb)) {
            let uniform = (ok == M::all(width))
                .then(|| Frame::<M>::uniform(ya, ok).zip(Frame::<M>::uniform(yb, ok)))
                .flatten();
            let vector = uniform.is_some_and(|(x, y)| !match op {
                NumOp::Multiply => Kernel::multiply((xa, x), (xb, y), m, s),
                NumOp::Subtract => Kernel::add((xa, x), (xb, y), true, m, s),
                _ => Kernel::add((xa, x), (xb, y), false, m, s),
            });
            done = match vector {
                true => ok,
                false => {
                    let mut done = f.none();
                    for lane in Lanes::of(ok) {
                        let (x, y) = ((xa[lane], ya[lane]), (xb[lane], yb[lane]));
                        if let Some((mm, sc)) = op.scaled(x, y) {
                            m[lane] = mm;
                            s[lane] = sc;
                            done.set(lane);
                        }
                    }
                    done
                }
            };
        }
        let base = dst as usize * width;
        match done == M::all(width) {
            true => {
                f.mant[base..base + width].copy_from_slice(m);
                f.scales[base..base + width].copy_from_slice(s);
            }
            false => {
                for lane in Lanes::of(done) {
                    f.mant[base + lane] = m[lane];
                    f.scales[base + lane] = s[lane];
                }
            }
        }
        f.wide[dst as usize] &= !done;
        f.buffers = buffers;
        done
    }

    const NARROW: usize = 8;

    #[inline]
    pub(super) fn scaled_at<M: LaneSet>(
        o: Operand,
        lane: usize,
        f: &Frame<M>,
    ) -> Option<(i64, u8)> {
        match o {
            Operand::Num(n) => Scaled::parts(&n),
            Operand::Reg(r) => {
                let at = f.at(r, lane);
                (!f.wide[r as usize].get(lane)).then(|| (f.mant[at], f.scales[at]))
            }
        }
    }

    pub(super) fn extreme<M: LaneSet>(
        dst: Reg,
        items: &[Operand],
        largest: bool,
        active: M,
        f: &mut Frame<M>,
    ) {
        let mut slow = f.none();
        for lane in Lanes::of(active) {
            let mut best: Option<(i64, u8)> = None;
            let mut ok = true;
            for o in items {
                let boxed = matches!(o, Operand::Reg(r) if f.boxed[*r as usize].get(lane));
                let Some(x) = Self::scaled_at(*o, lane, f).filter(|_| !boxed) else {
                    ok = false;
                    break;
                };
                best = match best.map(|b| Scaled::compare(x, b).map(|o| (b, o))) {
                    None => Some(x),
                    Some(None) => {
                        ok = false;
                        break;
                    }
                    Some(Some((b, order))) => match largest {
                        true => Some(if order.is_ge() { x } else { b }),
                        false => Some(if order.is_lt() { x } else { b }),
                    },
                };
            }
            match (ok, best) {
                (true, Some((m, s))) => {
                    f.put_scaled(dst, lane, m, s);
                }
                _ => slow.set(lane),
            }
        }
        if slow.is_empty() {
            return;
        }
        let kind = FunctionKind::Internal(if largest {
            InternalFunction::Max
        } else {
            InternalFunction::Min
        });
        let Some(builtin) = Builtins::of(&kind) else {
            return;
        };
        for lane in Lanes::of(slow) {
            let values: Vec<Variable> = items
                .iter()
                .map(|o| match o {
                    Operand::Num(n) => Variable::Number(*n),
                    Operand::Reg(r) => f.value(*r, lane),
                })
                .collect();
            let array = Variable::from_array(values);
            let r = Builtins::call(builtin, &kind, &[Arg::of(&array)]);
            f.write(dst, lane, r);
        }
    }

    pub(super) fn lane_num<M: LaneSet>(
        op: NumOp,
        dst: Reg,
        a: Operand,
        b: Operand,
        lanes: M,
        f: &mut Frame<M>,
    ) -> M {
        let mut done = f.none();
        for lane in Lanes::of(lanes) {
            let (Some(x), Some(y)) = (Self::scaled_at(a, lane, f), Self::scaled_at(b, lane, f))
            else {
                continue;
            };
            if let Some((m, s)) = op.scaled(x, y) {
                f.put_scaled(dst, lane, m, s);
                done.set(lane);
            }
        }
        done
    }

    pub(super) fn lane_cmp<M: LaneSet>(
        op: NumCmp,
        a: Operand,
        b: Operand,
        lanes: M,
        f: &Frame<M>,
    ) -> (M, M) {
        let (mut done, mut word) = (f.none(), f.none());
        for lane in Lanes::of(lanes) {
            let (Some(x), Some(y)) = (Self::scaled_at(a, lane, f), Self::scaled_at(b, lane, f))
            else {
                continue;
            };
            if let Some(o) = Scaled::compare(x, y) {
                word.put(lane, op.test(o));
                done.set(lane);
            }
        }
        (done, word)
    }

    pub(super) fn num<M: LaneSet>(
        op: NumOp,
        dst: Reg,
        a: Operand,
        b: Operand,
        active: M,
        f: &mut Frame<M>,
    ) {
        let slow = active & (f.operand_boxed(a) | f.operand_boxed(b));
        let fast = active & !slow;
        let d = dst as usize;
        f.boxed[d] &= !fast;
        let done = match (op, f.width < Self::NARROW) {
            (NumOp::Add | NumOp::Subtract | NumOp::Multiply, false) => {
                Self::scaled_num(op, dst, a, b, fast, f)
            }
            _ => Self::lane_num(op, dst, a, b, fast, f),
        };
        for lane in Lanes::of(fast & !done) {
            let (x, y) = (f.num(a, lane), f.num(b, lane));
            let r = match op {
                NumOp::Add => x.checked_add(y).ok_or("Add"),
                NumOp::Subtract => x.checked_sub(y).ok_or("Subtract"),
                NumOp::Multiply => x.checked_mul(y).ok_or("Multiply"),
                NumOp::Divide => x.checked_div(y).ok_or(""),
                NumOp::Modulo => x.checked_rem(y).ok_or(""),
            };
            match r {
                Ok(v) => f.put_number(dst, lane, v),
                Err("") => f.set(dst, lane, Variable::Null),
                Err(name) => f.fail(lane, Ops::error(name, "Number overflow")),
            }
        }
        for lane in Lanes::of(slow) {
            let (x, y) = (f.operand_value(a, lane), f.operand_value(b, lane));
            let numeric = matches!((&x, &y), (Variable::Number(_), Variable::Number(_)));
            let unsupported = match op {
                NumOp::Add => None,
                NumOp::Subtract => Some("Subtract"),
                NumOp::Multiply => Some("Multiply"),
                NumOp::Divide => Some("Divide"),
                NumOp::Modulo => Some("Modulo"),
            };
            if let (false, Some(opcode)) = (numeric, unsupported) {
                f.fault(lane, Fault::Unsupported(opcode));
                continue;
            }
            let r = match op {
                NumOp::Add => Ops::add(x, y),
                NumOp::Subtract => Ops::subtract(x, y),
                NumOp::Multiply => Ops::multiply(x, y),
                NumOp::Divide => Ops::divide(x, y),
                NumOp::Modulo => Ops::modulo(x, y),
            };
            f.put(dst, lane, r);
        }
    }

    fn constant_cmp<M: LaneSet>(
        op: NumCmp,
        a: Operand,
        b: Operand,
        ok: M,
        f: &Frame<M>,
    ) -> Option<M> {
        let (reg, n, op) = match (a, b) {
            (Operand::Reg(r), Operand::Num(n)) => (r, n, op),
            (Operand::Num(n), Operand::Reg(r)) => (r, n, op.mirrored()),
            _ => return None,
        };
        let base = reg as usize * f.width;
        let values = f.mant.get(base..base + f.width)?;
        let scale = Frame::<M>::uniform(f.scales.get(base..base + f.width)?, ok)?;
        let (k, s) = Scaled::parts(&n)?;
        match Threshold::of(op, scale, k, s)? {
            Threshold::At(op, k) => Some(Kernel::threshold::<M>(values, k, |o| op.test(o))),
            Threshold::Never => Some(f.none()),
        }
    }

    pub(super) fn negate<M: LaneSet>(dst: Reg, a: Reg, active: M, f: &mut Frame<M>) {
        let typed = match (f.kind(a), f.kind(dst)) {
            (Kind::Num, Kind::Num) => active & !f.boxed[a as usize],
            _ => f.none(),
        };
        let wide = f.wide[a as usize];
        let sb = a as usize * f.width;
        for lane in Lanes::of(typed) {
            let (m, s) = (f.mant[sb + lane], f.scales[sb + lane]);
            match (wide.get(lane), m) {
                (false, m) if m != 0 && m != i64::MIN => f.put_scaled(dst, lane, -m, s),
                (false, m) => f.put_number(dst, lane, -Scaled::decimal(m, s)),
                (true, _) => {
                    let n = f.nums[sb + lane];
                    f.put_number(dst, lane, -n);
                }
            }
        }
        f.boxed[dst as usize] &= !typed;
        for lane in Lanes::of(active & !typed) {
            let r = Ops::negate(f.take(a, lane));
            f.put(dst, lane, r);
        }
    }

    pub(super) fn scaled_cmp<M: LaneSet>(
        op: NumCmp,
        a: Operand,
        b: Operand,
        lanes: M,
        f: &mut Frame<M>,
    ) -> (M, M) {
        let ok = lanes & !f.operand_wide(a) & !f.operand_wide(b);
        if ok.is_empty() {
            return (f.none(), f.none());
        }
        if let Some(word) = Self::constant_cmp(op, a, b, ok, f) {
            return (ok, word & ok);
        }
        let mut buffers = std::mem::take(&mut f.buffers);
        let Buffers { ka, sa, kb, sb, .. } = &mut buffers;
        let mut out = (f.none(), f.none());
        if let (Some((xa, ya)), Some((xb, yb))) = (f.side(a, ka, sa), f.side(b, kb, sb)) {
            let vector = Frame::<M>::uniform(ya, ok)
                .zip(Frame::<M>::uniform(yb, ok))
                .and_then(|(x, y)| Kernel::compare::<M>((xa, x), (xb, y), |o| op.test(o)));
            out = match vector {
                Some(word) => (ok, word & ok),
                None => {
                    let (mut done, mut word) = (f.none(), f.none());
                    for lane in Lanes::of(ok) {
                        if let Some(o) = Scaled::compare((xa[lane], ya[lane]), (xb[lane], yb[lane]))
                        {
                            word.put(lane, op.test(o));
                            done.set(lane);
                        }
                    }
                    (done, word)
                }
            };
        }
        f.buffers = buffers;
        out
    }

    pub(super) fn any_number<M: LaneSet>(a: Reg, id: u16, active: M, f: &mut Frame<M>) -> (M, M) {
        let lanes = match (f.kind(a), &f.consts[id as usize]) {
            (Kind::Num | Kind::Str, Variable::Array(_)) => active & !f.boxed[a as usize],
            _ => return (f.none(), f.none()),
        };
        if f.kind(a) == Kind::Str {
            let Variable::Array(list) = &f.consts[id as usize] else {
                return (f.none(), f.none());
            };
            let list = list.borrow();
            let texts: smallvec::SmallVec<[&str; 8]> =
                list.iter().filter_map(Variable::as_str).collect();
            let mut word = f.none();
            for lane in Lanes::of(lanes) {
                let x = f.text_at(a, lane);
                word.put(lane, texts.contains(&x));
            }
            return (lanes, word);
        }
        let numbers: smallvec::SmallVec<[Decimal; 8]> = match &f.consts[id as usize] {
            Variable::Array(list) => list
                .borrow()
                .iter()
                .filter_map(Variable::as_number)
                .collect(),
            _ => return (f.none(), f.none()),
        };
        let (mut done, mut word) = (lanes, f.none());
        for k in numbers {
            let (d, w) =
                Self::scaled_cmp(NumCmp::Equal, Operand::Reg(a), Operand::Num(k), lanes, f);
            done &= d;
            word |= w;
        }
        (done, word & done)
    }

    pub(super) fn within<M: LaneSet>(
        a: Reg,
        lo: Decimal,
        hi: Decimal,
        left: Bracket,
        right: Bracket,
        lanes: M,
        f: &mut Frame<M>,
    ) -> (M, M) {
        let order = |k: Decimal, f: &mut Frame<M>| {
            let (d1, less) = Self::scaled_cmp(
                NumCmp::Order(Compare::Less),
                Operand::Reg(a),
                Operand::Num(k),
                lanes,
                f,
            );
            let (d2, equal) =
                Self::scaled_cmp(NumCmp::Equal, Operand::Reg(a), Operand::Num(k), lanes, f);
            (d1 & d2, less, equal)
        };
        let (d1, lt_lo, eq_lo) = order(lo, f);
        let (d2, lt_hi, eq_hi) = order(hi, f);
        let done = d1 & d2;
        let (gt_lo, gt_hi) = (!lt_lo & !eq_lo, !lt_hi & !eq_hi);
        let (first, open) = match left {
            Bracket::LeftParenthesis => (gt_lo, false),
            Bracket::LeftSquareBracket => (gt_lo | eq_lo, false),
            Bracket::RightParenthesis => (lt_lo, true),
            Bracket::RightSquareBracket => (lt_lo | eq_lo, true),
            _ => return (f.none(), f.none()),
        };
        let second = match right {
            Bracket::RightParenthesis => lt_hi,
            Bracket::RightSquareBracket => lt_hi | eq_hi,
            Bracket::LeftParenthesis => gt_hi,
            Bracket::LeftSquareBracket => gt_hi | eq_hi,
            _ => return (f.none(), f.none()),
        };
        let word = match open {
            true => first | second,
            false => first & second,
        };
        (done, word & done)
    }

    #[inline(never)]
    pub(super) fn cmp<M: LaneSet>(
        op: NumCmp,
        dst: Reg,
        a: Operand,
        b: Operand,
        active: M,
        f: &mut Frame<M>,
    ) {
        let slow = active & (f.operand_boxed(a) | f.operand_boxed(b));
        let fast = active & !slow;
        let d = dst as usize;
        let (done, mut word) = match f.width < Self::NARROW {
            true => Self::lane_cmp(op, a, b, fast, f),
            false => Self::scaled_cmp(op, a, b, fast, f),
        };
        for lane in Lanes::of(fast & !done) {
            let (x, y) = (f.num(a, lane), f.num(b, lane));
            let hit = match op {
                NumCmp::Order(c) => Ops::ordered_number(&x, &y, c),
                NumCmp::Equal => Ops::same_number(&x, &y),
            };
            word.put(lane, hit);
        }
        f.put_mask(d as Reg, fast, word);
        for lane in Lanes::of(slow) {
            let (x, y) = (f.operand_value(a, lane), f.operand_value(b, lane));
            match op {
                NumCmp::Order(c) => match Ops::compared(&x, &y, c) {
                    Some(hit) => f.set(dst, lane, Variable::Bool(hit)),
                    None => f.fault(lane, Fault::Unsupported("Compare")),
                },
                NumCmp::Equal => f.set(dst, lane, Variable::Bool(Ops::equal(&x, &y))),
            }
        }
    }
}

impl NumOp {
    #[inline(always)]
    pub(super) fn scaled(self, x: (i64, u8), y: (i64, u8)) -> Option<(i64, u8)> {
        match self {
            NumOp::Multiply => Scaled::multiply(x, y),
            NumOp::Subtract => Scaled::add(x, y, true),
            NumOp::Divide => Scaled::divide(x, y),
            NumOp::Modulo => Scaled::remainder(x, y),
            NumOp::Add => Scaled::add(x, y, false),
        }
    }
}

impl NumCmp {
    pub(super) fn mirrored(self) -> Self {
        match self {
            NumCmp::Order(Compare::More) => NumCmp::Order(Compare::Less),
            NumCmp::Order(Compare::Less) => NumCmp::Order(Compare::More),
            NumCmp::Order(Compare::MoreOrEqual) => NumCmp::Order(Compare::LessOrEqual),
            NumCmp::Order(Compare::LessOrEqual) => NumCmp::Order(Compare::MoreOrEqual),
            NumCmp::Equal => NumCmp::Equal,
        }
    }

    #[inline(always)]
    pub(super) fn test(self, o: std::cmp::Ordering) -> bool {
        match self {
            NumCmp::Equal => o.is_eq(),
            NumCmp::Order(Compare::More) => o.is_gt(),
            NumCmp::Order(Compare::MoreOrEqual) => o.is_ge(),
            NumCmp::Order(Compare::Less) => o.is_lt(),
            NumCmp::Order(Compare::LessOrEqual) => o.is_le(),
        }
    }
}

pub(super) enum Threshold {
    At(NumCmp, i64),
    Never,
}

impl Threshold {
    pub(super) fn of(op: NumCmp, scale: u8, k: i64, s: u8) -> Option<Threshold> {
        if s <= scale {
            let k = k.checked_mul(*Scaled::POW10.get((scale - s) as usize)?)?;
            return Some(Threshold::At(op, k));
        }
        let p = *Scaled::POW10.get((s - scale) as usize)?;
        let floor = k.div_euclid(p);
        let exact = k.rem_euclid(p) == 0;
        let ceil = floor + (!exact) as i64;
        Some(match op {
            NumCmp::Order(Compare::More | Compare::LessOrEqual) => Threshold::At(op, floor),
            NumCmp::Order(Compare::MoreOrEqual | Compare::Less) => Threshold::At(op, ceil),
            NumCmp::Equal if exact => Threshold::At(op, floor),
            NumCmp::Equal => Threshold::Never,
        })
    }
}
