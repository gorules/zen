use super::frame::Fault;
use super::context::Context;
use super::frame::{Frame, Scratch};
use super::Executor;
use crate::functions::ClosureFunction;
use crate::lane::columns::{Column, Values};
use crate::lane::mask::{LaneSet, Lanes, Mask};
use crate::lane::ops::Ops;
use crate::lane::output::Item;
use crate::lane::builtins::Out;
use crate::lane::program::{ClosureOp, FoldOp, Kind, Op, Reg, Step};
use crate::lane::scaled::Scaled;
use crate::variable::Variable;
use crate::vm::VMError;

#[derive(Debug)]
pub(super) enum Pending {
    Array(Variable),
    Child { start: usize, end: usize },
}

impl Pending {
    pub(super) fn len(&self) -> usize {
        match self {
            Pending::Array(Variable::Array(arr)) => arr.borrow().len(),
            Pending::Array(_) => 0,
            Pending::Child { start, end } => end - start,
        }
    }
}

pub(super) type Results<'a> = (
    &'a mut [Variable],
    &'a [(u32, VMError)],
    &'a [u64],
    &'a [u64],
);

pub(super) struct Outcomes<'a> {
    pub(super) values: &'a mut [Variable],
    pub(super) failures: &'a [(u32, VMError)],
    pub(super) truths: &'a [u64],
    pub(super) settled: &'a [u64],
    pub(super) offset: usize,
    pub(super) next: usize,
}

impl<'a> Outcomes<'a> {
    pub(super) fn new(scratch: Results<'a>, start: usize, end: usize) -> Self {
        let (results, failures, truths, settled) = scratch;
        let lo = failures.partition_point(|(i, _)| (*i as usize) < start);
        let hi = failures.partition_point(|(i, _)| (*i as usize) < end);
        Self {
            values: &mut results[start..end],
            failures: &failures[lo..hi],
            truths,
            settled,
            offset: start,
            next: 0,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.values.len()
    }

    pub(super) fn take(&mut self, i: usize) -> Result<Variable, VMError> {
        match self.failures.get(self.next) {
            Some((at, e)) if *at as usize == self.offset + i => {
                self.next += 1;
                Err(e.clone())
            }
            _ => Ok(std::mem::replace(&mut self.values[i], Variable::Null)),
        }
    }

    pub(super) fn tally(&self) -> Option<(usize, usize)> {
        let (start, end) = (self.offset, self.offset + self.values.len());
        let mut trues = 0usize;
        let mut at = start;
        while at < end {
            let (word, shift) = (at / 64, at % 64);
            let take = (64 - shift).min(end - at);
            let mask = match take {
                64 => u64::MAX,
                n => ((1u64 << n) - 1) << shift,
            };
            if self.settled.get(word).copied().unwrap_or(0) & mask != mask {
                return None;
            }
            trues += (self.truths.get(word).copied().unwrap_or(0) & mask).count_ones() as usize;
            at += take;
        }
        Some((trues, end - start))
    }

    pub(super) fn decide(&self, kind: ClosureFunction) -> Option<Option<bool>> {
        let (trues, len) = self.tally()?;
        Some(match kind {
            ClosureFunction::Some => (trues > 0).then_some(true),
            ClosureFunction::All => (trues < len).then_some(false),
            _ => (trues > 0).then_some(false),
        })
    }

    pub(super) fn peek(&mut self, i: usize) -> Result<Option<bool>, VMError> {
        let at = self.offset + i;
        if Mask::bit(self.settled, at) {
            return Ok(Some(Mask::bit(self.truths, at)));
        }
        match self.failures.get(self.next) {
            Some((at, e)) if *at as usize == self.offset + i => {
                self.next += 1;
                Err(e.clone())
            }
            _ => Ok(match self.values[i] {
                Variable::Bool(b) => Some(b),
                _ => None,
            }),
        }
    }

    pub(super) fn truth(&mut self, i: usize) -> Result<bool, VMError> {
        match self.peek(i)? {
            Some(b) => Ok(b),
            None => Ops::truthy(&self.take(i)?, "JumpIfFalse"),
        }
    }

    pub(super) fn verdict(
        &mut self,
        kind: ClosureFunction,
        i: usize,
    ) -> Result<Option<bool>, VMError> {
        match (self.peek(i)?, kind) {
            (Some(b), ClosureFunction::Some) => Ok(b.then_some(true)),
            (Some(b), ClosureFunction::All) => Ok((!b).then_some(false)),
            (Some(b), _) => Ok(b.then_some(false)),
            (None, _) => Executor::verdict(kind, self.take(i)),
        }
    }
}

impl Executor {
    pub(super) fn ones(words: &[u64], a: usize, b: usize) -> usize {
        let mut count = 0usize;
        let mut at = a;
        while at < b {
            let (word, shift) = (at / 64, at % 64);
            let take = (64 - shift).min(b - at);
            let mask = match take {
                64 => u64::MAX,
                n => ((1u64 << n) - 1) << shift,
            };
            count += (words.get(word).copied().unwrap_or(0) & mask).count_ones() as usize;
            at += take;
        }
        count
    }

    pub(super) fn sweep<M: LaneSet>(
        c: &ClosureOp,
        active: M,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context,
    ) -> bool {
        if c.sequential || matches!(c.kind, ClosureFunction::FlatMap) || active.is_empty() {
            return false;
        }
        let source = match Self::selection(c, f, ctx) {
            Some(child) => Some((child, None)),
            None => Self::list_children(c, ctx).map(|(child, column)| (child, Some(column))),
        };
        let Some((child, column)) = source else {
            return false;
        };
        let child = &child;
        let mut picks: Vec<u32> = Vec::new();
        let mut ranges = std::mem::take(&mut f.ranges);
        ranges.clear();
        ranges.resize(f.width, (0, 0));
        let (mut lo, mut hi) = (None, 0usize);
        for lane in Lanes::of(active) {
            let range = match &column {
                Some(column) => column.range(ctx.base + f.rows[lane] as usize),
                None => match f.generic(c.list, lane) {
                    true => None,
                    false => {
                        let (x, y) = f.lists[f.at(c.list, lane)];
                        let a = picks.len();
                        f.items.rows(x as usize, y as usize).map(|rows| {
                            picks.extend(rows.iter().map(|r| *r as u32));
                            (a, picks.len())
                        })
                    }
                },
            };
            let Some((a, b)) = range else {
                f.ranges = ranges;
                return false;
            };
            match lo {
                None => lo = Some(a),
                Some(_) if a != hi => return false,
                Some(_) => {}
            }
            ranges[lane] = (a, b);
            hi = b;
        }
        let Some(lo) = lo else {
            return false;
        };
        let total = hi - lo;
        let selected = column.is_none();
        let index = |v: usize| match selected {
            true => picks.get(v).copied().unwrap_or_default() as usize,
            false => v,
        };
        let (element, out) = (c.body.element.unwrap_or_default(), c.body.out as usize);
        let predicate = !matches!(c.kind, ClosureFunction::Map);
        if !predicate && f.kinds[c.dst as usize] != Kind::List {
            return false;
        }
        let items = f.items.len();
        if !predicate && Self::project(c, child, f, lo..lo + total, selected.then_some(&picks)) {
            let base = c.dst as usize * f.width;
            for lane in Lanes::of(active) {
                let (a, b) = (ranges[lane].0 - lo, ranges[lane].1 - lo);
                f.lists[base + lane] = ((items + a) as u32, (items + b) as u32);
            }
            f.boxed[c.dst as usize] &= !active;
            f.ranges = ranges;
            return true;
        }
        let mut scratch = std::mem::take(&mut f.scratch);
        let Scratch {
            truths,
            results,
            rows,
            parents,
            ..
        } = &mut scratch;
        truths.clear();
        truths.resize(total.div_ceil(64), 0);
        results.clear();
        parents.clear();
        rows.clear();
        let pure = c.imports.is_empty() && c.body.pure();
        match pure {
            true => rows.extend(0..total.min(M::LANES) as u32),
            false => {
                for lane in Lanes::of(active) {
                    let (a, b) = ranges[lane];
                    parents.extend(std::iter::repeat_n(lane as u32, b - a));
                    rows.extend(std::iter::repeat_n(f.rows[lane], b - a));
                }
            }
        }
        let projected = predicate
            && Self::project_truths(c, child, truths, lo..lo + total, selected.then_some(&picks));
        if predicate && !projected {
            truths.fill(0);
        }
        let mut ok = true;
        let mut start = match projected {
            true => total,
            false => 0,
        };
        while start < total {
            let end = (start + M::LANES).min(total);
            let width = end - start;
            let Some(frame) = rest.first_mut() else {
                ok = false;
                break;
            };
            match pure {
                true => Self::enter(&c.body, frame, &rows[..end - start], None),
                false => Self::enter(
                    &c.body,
                    frame,
                    &rows[start..end],
                    Some((f, &parents[start..end], &c.imports)),
                ),
            }
            let viewed = Self::elements(c, frame, child, ctx, width, |lane| index(lo + start + lane));
            if !viewed && (selected || !Self::fill_dense(frame, element, child, lo + start)) {
                Self::fill_with(frame, element, child, M::all(width), |_, lane| {
                    index(lo + start + lane)
                });
            }
            Self::execute(&c.body, rest, ctx);
            let frame = &mut rest[0];
            let all = M::all(width);
            if frame.alive & !frame.boxed[out] & all != all {
                ok = false;
                break;
            }
            match (predicate, frame.kinds[out]) {
                (true, Kind::Bool) => Self::put_bits(truths, start, width, frame.bits[out] & all),
                (true, _) => {
                    ok = false;
                    break;
                }
                (false, _) => {
                    let (kind, base) = (frame.kinds[out], out * frame.width);
                    let clean = !frame.boxed[out] & !frame.wide[out];
                    if kind == Kind::Num && clean == M::all(width) {
                        f.items.extend_nums(
                            &frame.mant[base..base + width],
                            &frame.scales[base..base + width],
                        );
                        start = end;
                        continue;
                    }
                    for lane in 0..width {
                        let item = match (kind, clean.get(lane)) {
                            (Kind::Num, true) => {
                                Item::Num(frame.mant[base + lane], frame.scales[base + lane])
                            }
                            (Kind::Bool, true) => Item::Bool(frame.bits[out].get(lane)),
                            (Kind::Str, true) => {
                                let (x, y) = frame.spans[base + lane];
                                let text =
                                    frame.arena.get(x as usize..y as usize).unwrap_or_default();
                                let (origin, end) = f.intern(text);
                                Item::Text(origin, end)
                            }
                            _ => Item::Value(frame.take(out as Reg, lane)),
                        };
                        f.items.push(item);
                    }
                }
            }
            start = end;
        }
        if ok && predicate && !matches!(c.kind, ClosureFunction::Filter) {
            let d = c.dst as usize;
            let (base, kind) = (d * f.width, f.kinds[d]);
            let mut word = f.none();
            for lane in Lanes::of(active) {
                let (a, b) = (ranges[lane].0 - lo, ranges[lane].1 - lo);
                let n = Self::ones(truths, a, b);
                match (c.kind, kind) {
                    (ClosureFunction::Count, Kind::Num) => {
                        f.mant[base + lane] = n as i64;
                        f.scales[base + lane] = 0;
                    }
                    (
                        ClosureFunction::One
                        | ClosureFunction::Some
                        | ClosureFunction::All
                        | ClosureFunction::None,
                        Kind::Bool,
                    ) => word.put(lane, c.kind.holds(n, b - a)),
                    _ => {}
                }
            }
            match kind {
                Kind::Num => f.wide[d] &= !active,
                _ => f.bits[d] = (f.bits[d] & !active) | word,
            }
            f.boxed[d] &= !active;
            ok = matches!(kind, Kind::Num | Kind::Bool);
        } else if ok && !predicate {
            let base = c.dst as usize * f.width;
            for lane in Lanes::of(active) {
                let (a, b) = (ranges[lane].0 - lo, ranges[lane].1 - lo);
                f.lists[base + lane] = ((items + a) as u32, (items + b) as u32);
            }
            f.boxed[c.dst as usize] &= !active;
        } else if ok && f.kinds[c.dst as usize] == Kind::List && c.select && !selected && matches!(child.values, Values::Struct { .. }) {
            let base = c.dst as usize * f.width;
            let mut at = f.items.len();
            f.items.extend_rows(truths, 0, total, lo);
            for lane in Lanes::of(active) {
                let (a, b) = (ranges[lane].0 - lo, ranges[lane].1 - lo);
                let end = at + Self::ones(truths, a, b);
                f.lists[base + lane] = (at as u32, end as u32);
                at = end;
            }
            ctx.register(f.program, c.dst, *child);
            f.boxed[c.dst as usize] &= !active;
        } else if ok && f.kinds[c.dst as usize] == Kind::List {
            let base = c.dst as usize * f.width;
            for lane in Lanes::of(active) {
                let (a, b) = (ranges[lane].0 - lo, ranges[lane].1 - lo);
                let start = f.items.len() as u32;
                match (child.values, child.validity.is_some()) {
                    (Values::Struct { .. }, _) if c.select => {
                        for i in (a..b).filter(|i| Mask::bit(truths, *i)) {
                            f.items.push(Item::Row(index(lo + i) as u32));
                        }
                    }
                    (Values::I64(v), false) if !selected && lo + b <= v.len() => {
                        f.items
                            .extend_selected(v.get(lo..).unwrap_or_default(), truths, a, b)
                    }
                    _ => {
                        for i in (a..b).filter(|i| Mask::bit(truths, *i)) {
                            let item = Self::child_item(child, index(lo + i), f);
                            f.items.push(item);
                        }
                    }
                }
                f.lists[base + lane] = (start, f.items.len() as u32);
            }
            if c.select && matches!(child.values, Values::Struct { .. }) {
                ctx.register(f.program, c.dst, *child);
            }
            f.boxed[c.dst as usize] &= !active;
        } else if ok {
            for lane in Lanes::of(active) {
                let (a, b) = (ranges[lane].0 - lo, ranges[lane].1 - lo);
                let value = match c.kind {
                    ClosureFunction::Filter => Variable::from_array(
                        (a..b)
                            .filter(|i| Mask::bit(truths, *i))
                            .map(|i| child.variable(index(lo + i)))
                            .collect(),
                    ),
                    kind => {
                        let n = Self::ones(truths, a, b);
                        match kind {
                            ClosureFunction::Count => Variable::Number(n.into()),
                            kind => Variable::Bool(kind.holds(n, b - a)),
                        }
                    }
                };
                f.set(c.dst, lane, value);
            }
        }
        if !ok {
            f.items.truncate(items);
        }
        f.scratch = scratch;
        f.ranges = ranges;
        ok
    }

    fn projected<'c>(c: &ClosureOp, child: &Column<'c>, kind: Kind) -> Option<Column<'c>> {
        let (Some(element), [Step { op: Op::Field { dst, src, key, .. }, .. }]) =
            (c.body.element, c.body.steps.as_slice())
        else {
            return None;
        };
        let plain = *src == element
            && *dst == c.body.out
            && c.body.kinds.get(*dst as usize) == Some(&kind)
            && c.imports.is_empty()
            && child.validity.is_none();
        child.field(key).filter(|_| plain).copied()
    }

    fn project_truths(
        c: &ClosureOp,
        child: &Column,
        truths: &mut [u64],
        span: std::ops::Range<usize>,
        picks: Option<&[u32]>,
    ) -> bool {
        let Some(field) = Self::projected(c, child, Kind::Bool) else {
            return false;
        };
        let Values::Bool { bits, offset } = field.values else {
            return false;
        };
        for (k, i) in span.enumerate() {
            let row = match picks {
                Some(picks) => picks.get(i).map(|p| *p as usize),
                None => Some(i),
            };
            let valid = row.filter(|row| {
                field.validity.is_none_or(|(valid, at)| Column::bit(valid, at + row))
            });
            match (valid, truths.get_mut(k / 64)) {
                (Some(row), Some(word)) => *word |= (Column::bit(bits, offset + row) as u64) << (k % 64),
                _ => return false,
            }
        }
        true
    }

    fn project<M: LaneSet>(
        c: &ClosureOp,
        child: &Column,
        f: &mut Frame<M>,
        span: std::ops::Range<usize>,
        picks: Option<&[u32]>,
    ) -> bool {
        let Some(field) = Self::projected(c, child, Kind::Num) else {
            return false;
        };
        let valid = match (field.validity, picks) {
            (None, _) => true,
            (Some(_), None) => span.clone().all(|i| field.valid(i)),
            (Some(_), Some(picks)) => picks
                .get(span.clone())
                .is_some_and(|picks| picks.iter().all(|p| field.valid(*p as usize))),
        };
        if !valid {
            return false;
        }
        let items = f.items.len();
        let done = match (field.values, picks) {
            (Values::Scaled { mant, scale }, None) => mant
                .get(span.clone())
                .zip(scale.get(span))
                .map(|(m, s)| f.items.extend_nums(m, s))
                .is_some(),
            (Values::Scaled { mant, scale }, Some(picks)) => picks.get(span).is_some_and(|picks| {
                f.items.extend_parts(picks.iter().map(|p| {
                    let p = *p as usize;
                    mant.get(p).zip(scale.get(p)).map(|(m, s)| (*m, *s))
                }))
            }),
            (Values::I64(values), None) => values.get(span).is_some_and(|values| {
                f.items.extend_parts(values.iter().map(|n| Some((*n, 0))))
            }),
            (Values::I64(values), Some(picks)) => picks.get(span).is_some_and(|picks| {
                f.items.extend_parts(picks.iter().map(|p| values.get(*p as usize).map(|n| (*n, 0))))
            }),
            (Values::Dec(values), None) => values
                .get(span)
                .is_some_and(|values| f.items.extend_parts(values.iter().map(Scaled::parts))),
            (Values::Dec(values), Some(picks)) => picks.get(span).is_some_and(|picks| {
                f.items.extend_parts(
                    picks.iter().map(|p| values.get(*p as usize).and_then(Scaled::parts)),
                )
            }),
            _ => false,
        };
        if !done {
            f.items.truncate(items);
        }
        done
    }

    pub(super) fn child_item<M: LaneSet>(child: &Column, index: usize, f: &mut Frame<M>) -> Item {
        match (child.values, child.valid(index)) {
            (Values::I64(v), true) => v
                .get(index)
                .map_or(Item::Value(Variable::Null), |n| Item::Num(*n, 0)),
            (Values::Dec(v), true) => match v.get(index).and_then(Scaled::parts) {
                Some((m, s)) => Item::Num(m, s),
                None => Item::Value(child.variable(index)),
            },
            (Values::Utf8 { .. } | Values::Text { .. } | Values::LargeUtf8 { .. } | Values::Strs(_), true) => {
                match child.text(index) {
                    Some(text) => {
                        let (origin, end) = f.intern(text);
                        Item::Text(origin, end)
                    }
                    None => Item::Value(child.variable(index)),
                }
            }
            _ => Item::Value(child.variable(index)),
        }
    }

    pub(super) fn closure<M: LaneSet>(
        c: &ClosureOp,
        active: M,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context,
    ) {
        if Self::sweep(c, active, f, rest, ctx) {
            return;
        }
        if let Some(column) = Self::selection(c, f, ctx) {
            return Self::selected(c, &column, active, f, rest, ctx);
        }
        let decisive = matches!(
            c.kind,
            ClosureFunction::Some | ClosureFunction::All | ClosureFunction::None
        );
        if decisive {
            return Self::decisive(c, active, f, rest, ctx);
        }

        let mut scratch = std::mem::take(&mut f.scratch);
        scratch.items.clear();
        scratch.slots.clear();
        scratch.spans.clear();
        let children = Self::list_children(c, ctx);
        if let Some((child, column)) = children {
            let child = &child;
            let lanes = Lanes::of(active).filter(|lane| {
                let row = ctx.base + f.rows[*lane] as usize;
                column.range(row).is_some()
            });
            let covered = lanes.fold(f.none(), |mut acc, lane| {
                acc.set(lane);
                acc
            });
            if covered == active {
                for lane in Lanes::of(active) {
                    let row = ctx.base + f.rows[lane] as usize;
                    let (a, b) = column
                        .range(row)
                        .map_or((0, 0), |(a, b)| (a as u32, b as u32));
                    let start = scratch.slots.len();
                    scratch.slots.extend((a..b).map(|i| (lane as u32, i)));
                    scratch.spans.push((lane, start, scratch.slots.len()));
                }
                Self::body(c, &mut scratch, Some(child), f, rest, ctx);
                let Scratch {
                    slots,
                    spans,
                    results,
                    failures,
                    truths,
                    settled,
                    ..
                } = &mut scratch;
                for (lane, start, end) in spans.iter().copied() {
                    let out = Outcomes::new((results, failures, truths, settled), start, end);
                    let r =
                        Self::finish(c.kind, out, |i| child.variable(slots[start + i].1 as usize));
                    f.put(c.dst, lane, r);
                }
                f.scratch = scratch;
                return;
            }
        }
        for lane in Lanes::of(active) {
            if let Some((child, column)) = children {
                let child = &child;
                let row = ctx.base + f.rows[lane] as usize;
                if let Some((a, b)) = column.range(row) {
                    let start = scratch.items.len();
                    scratch
                        .items
                        .extend((a..b).map(|i| (lane as u32, child.variable(i))));
                    scratch.spans.push((lane, start, scratch.items.len()));
                    continue;
                }
            }
            match Self::list_of(c, lane, f, ctx) {
                Ok(Variable::Array(arr)) => {
                    let start = scratch.items.len();
                    scratch
                        .items
                        .extend(arr.borrow().iter().map(|v| (lane as u32, v.clone())));
                    scratch.spans.push((lane, start, scratch.items.len()));
                }
                Ok(_) => f.fail(lane, Ops::error("Begin", "Unsupported type")),
                Err(e) => f.fail(lane, e),
            }
        }

        Self::body(c, &mut scratch, None, f, rest, ctx);
        let Scratch {
            items,
            spans,
            results,
            failures,
            truths,
            settled,
            ..
        } = &mut scratch;
        for (lane, start, end) in spans.iter().copied() {
            let out = Outcomes::new((results, failures, truths, settled), start, end);
            let r = Self::finish(c.kind, out, |i| items[start + i].1.clone());
            f.put(c.dst, lane, r);
        }
        f.scratch = scratch;
    }

    pub(super) fn elements<'c, M: LaneSet>(
        c: &ClosureOp,
        frame: &mut Frame<M>,
        child: &Column<'c>,
        ctx: &mut Context<'c>,
        width: usize,
        index: impl Fn(usize) -> usize,
    ) -> bool {
        let (Values::Struct { .. }, Some(element)) = (child.values, c.body.element) else {
            return false;
        };
        ctx.register(c.body.id, element, *child);
        if !frame.views.contains(&element) {
            frame.views.push(element);
        }
        frame.elements.clear();
        frame.elements.extend((0..width).map(|lane| index(lane) as u32));
        if c.body.whole(element) {
            Self::fill_with(frame, element, child, M::all(width), |_, lane| index(lane));
        }
        true
    }

    pub(super) fn body<'c, M: LaneSet>(
        c: &ClosureOp,
        scratch: &mut Scratch,
        column: Option<&Column<'c>>,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context<'c>,
    ) {
        let element = c.body.element.unwrap_or_default();
        let out = c.body.out;
        let chunk = if c.sequential { 1 } else { M::LANES };
        let keep = matches!(c.kind, ClosureFunction::Filter);
        let Scratch {
            items,
            slots,
            results,
            failures,
            rows,
            parents,
            truths,
            settled,
            ..
        } = scratch;
        results.clear();
        failures.clear();
        let total = match column {
            Some(_) => slots.len(),
            None => items.len(),
        };
        let predicate = !matches!(c.kind, ClosureFunction::Map | ClosureFunction::FlatMap);
        let words = total.div_ceil(64);
        for bits in [&mut *truths, &mut *settled] {
            bits.clear();
            bits.resize(words, 0);
        }
        let mut start = 0;
        while start < total {
            let end = (start + chunk).min(total);
            let width = end - start;
            rows.clear();
            parents.clear();
            for j in start..end {
                let parent = match column {
                    Some(_) => slots[j].0,
                    None => items[j].0,
                };
                parents.push(parent);
                rows.push(f.rows[parent as usize]);
            }
            let Some(child) = rest.first_mut() else {
                break;
            };
            Self::enter(&c.body, child, rows, Some((f, parents, &c.imports)));
            match column {
                Some(column) => {
                    let slots = &slots[start..end];
                    if !Self::elements(c, child, column, ctx, width, |lane| slots[lane].1 as usize) {
                        Self::fill_with(child, element, column, M::all(width), |_, lane| {
                            slots[lane].1 as usize
                        });
                    }
                }
                None => {
                    for (lane, (_, value)) in items[start..end].iter_mut().enumerate() {
                        let value = match keep {
                            true => value.clone(),
                            false => std::mem::replace(value, Variable::Null),
                        };
                        child.set(element, lane, value);
                    }
                }
            }
            Self::execute(&c.body, rest, ctx);
            let child = &mut rest[0];
            let o = out as usize;
            let (kind, boxed, bits) = (child.kinds[o], child.boxed[o], child.bits[o]);
            let ok = match (predicate, kind) {
                (true, Kind::Bool) => child.alive & !boxed & M::all(width),
                _ => f.none(),
            };
            Self::put_bits(settled, start, width, ok);
            Self::put_bits(truths, start, width, bits & ok);
            for lane in 0..width {
                if ok.get(lane) {
                    results.push(Variable::Null);
                    continue;
                }
                if !child.alive.get(lane) {
                    results.push(Variable::Null);
                    let e = child.errors[lane]
                        .take().map(Fault::vm)
                        .unwrap_or(VMError::NumberConversionError);
                    failures.push(((start + lane) as u32, e));
                    continue;
                }
                results.push(match (kind, boxed.get(lane)) {
                    (Kind::Bool, false) => Variable::Bool(bits.get(lane)),
                    (Kind::Num, false) => Variable::Number(child.number(out, lane)),
                    _ => child.take(out, lane),
                });
            }
            start = end;
        }
    }

    #[inline]
    pub(super) fn put_bits<M: LaneSet>(words: &mut [u64], start: usize, width: usize, value: M) {
        for i in 0..width.div_ceil(64) {
            let chunk = (width - i * 64).min(64);
            let bits = value.word(i);
            let at = start + i * 64;
            let (word, shift) = (at / 64, at % 64);
            if let Some(w) = words.get_mut(word) {
                *w |= bits << shift;
            }
            if shift > 0 && shift + chunk > 64 {
                if let Some(w) = words.get_mut(word + 1) {
                    *w |= bits >> (64 - shift);
                }
            }
        }
    }

    pub(super) fn decisive<M: LaneSet>(
        c: &ClosureOp,
        active: M,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context,
    ) {
        if active.count() == 1 {
            let lane = active.first().unwrap_or_default();
            let r = match Self::list_of(c, lane, f, ctx) {
                Ok(list @ Variable::Array(_)) => Self::single(c, lane, &list, f, rest, ctx),
                Ok(_) => Err(Ops::error("Begin", "Unsupported type")),
                Err(e) => Err(e),
            };
            f.put(c.dst, lane, r);
            return;
        }
        let mut scratch = std::mem::take(&mut f.scratch);
        let mut lists = std::mem::take(&mut scratch.lists);
        let mut done = std::mem::take(&mut scratch.done);
        lists.clear();
        let children = Self::list_children(c, ctx);
        for lane in Lanes::of(active) {
            if let Some((_, column)) = children {
                let row = ctx.base + f.rows[lane] as usize;
                if let Some((a, b)) = column.range(row) {
                    lists.push((lane, Pending::Child { start: a, end: b }, 0));
                    continue;
                }
            }
            match Self::list_of(c, lane, f, ctx) {
                Ok(list @ Variable::Array(_)) => lists.push((lane, Pending::Array(list), 0)),
                Ok(_) => f.fail(lane, Ops::error("Begin", "Unsupported type")),
                Err(e) => f.fail(lane, e),
            }
        }

        if lists.len() == 1 && matches!(lists.first(), Some((_, Pending::Array(_), _))) {
            if let Some((lane, Pending::Array(list), _)) = lists.pop() {
                let r = Self::single(c, lane, &list, f, rest, ctx);
                f.put(c.dst, lane, r);
            }
            scratch.lists = lists;
            scratch.done = done;
            f.scratch = scratch;
            return;
        }

        let typed = match children {
            Some(_) => lists
                .iter()
                .all(|(_, list, _)| matches!(list, Pending::Child { .. })),
            None => false,
        };
        let column = children.filter(|_| typed).map(|(child, _)| child);
        let mut step = 1usize;
        while !lists.is_empty() {
            scratch.items.clear();
            scratch.slots.clear();
            scratch.spans.clear();
            for (index, (lane, list, next)) in lists.iter().enumerate() {
                let end = (*next + step).min(list.len());
                let (start, stop) = match (list, children) {
                    (Pending::Array(Variable::Array(arr)), _) => {
                        let arr = arr.borrow();
                        let start = scratch.items.len();
                        scratch
                            .items
                            .extend(arr[*next..end].iter().map(|v| (*lane as u32, v.clone())));
                        (start, scratch.items.len())
                    }
                    (Pending::Child { start: base, .. }, Some(_)) if typed => {
                        let start = scratch.slots.len();
                        let (a, b) = ((base + *next) as u32, (base + end) as u32);
                        scratch.slots.extend((a..b).map(|i| (*lane as u32, i)));
                        (start, scratch.slots.len())
                    }
                    (Pending::Child { start: base, .. }, Some((child, _))) => {
                        let start = scratch.items.len();
                        scratch.items.extend(
                            (base + *next..base + end).map(|i| (*lane as u32, child.variable(i))),
                        );
                        (start, scratch.items.len())
                    }
                    _ => continue,
                };
                scratch.spans.push((index, start, stop));
            }
            Self::body(c, &mut scratch, column.as_ref(), f, rest, ctx);

            done.clear();
            for (index, start, end) in scratch.spans.iter().copied() {
                let (lane, list, next) = &mut lists[index];
                let mut decided = None;
                let results = (
                    scratch.results.as_mut_slice(),
                    scratch.failures.as_slice(),
                    scratch.truths.as_slice(),
                    scratch.settled.as_slice(),
                );
                let mut out = Outcomes::new(results, start, end);
                let settled = out.decide(c.kind);
                if let Some(verdict) = settled {
                    decided = verdict.map(|b| Ok(Variable::Bool(b)));
                }
                for i in (0..out.len()).filter(|_| settled.is_none()) {
                    match out.verdict(c.kind, i) {
                        Ok(Some(b)) => {
                            decided = Some(Ok(Variable::Bool(b)));
                            break;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            decided = Some(Err(e));
                            break;
                        }
                    }
                }
                let len = list.len();
                *next += end - start;
                let decided = decided.or_else(|| {
                    (*next >= len)
                        .then(|| Ok(Variable::Bool(!matches!(c.kind, ClosureFunction::Some))))
                });
                if let Some(r) = decided {
                    f.put(c.dst, *lane, r);
                    done.push(index);
                }
            }
            done.iter().rev().for_each(|i| {
                lists.swap_remove(*i);
            });
            if !c.sequential {
                step = match column {
                    Some(_) => M::LANES,
                    None => (step * 8).min(M::LANES),
                };
            }
        }
        scratch.lists = lists;
        scratch.done = done;
        f.scratch = scratch;
    }

    fn selection<'c, M: LaneSet>(c: &ClosureOp, f: &Frame<M>, ctx: &Context<'c>) -> Option<Column<'c>> {
        match (&c.source, f.kinds.get(c.list as usize)) {
            (None, Some(Kind::List)) => ctx.view(f.program, c.list),
            _ => None,
        }
    }

    fn selected<'c, M: LaneSet>(
        c: &ClosureOp,
        column: &Column<'c>,
        active: M,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context<'c>,
    ) {
        let base = c.list as usize * f.width;
        let rows = Lanes::of(active).all(|lane| {
            let (a, b) = f.lists[base + lane];
            !f.generic(c.list, lane) && (a..b).all(|i| f.items.row(i as usize).is_some())
        });
        let mut scratch = std::mem::take(&mut f.scratch);
        scratch.items.clear();
        scratch.slots.clear();
        scratch.spans.clear();
        for lane in Lanes::of(active) {
            let (a, b) = f.lists[base + lane];
            match (rows, f.generic(c.list, lane)) {
                (true, _) => {
                    let start = scratch.slots.len();
                    scratch
                        .slots
                        .extend((a..b).filter_map(|i| f.items.row(i as usize)).map(|row| (lane as u32, row)));
                    scratch.spans.push((lane, start, scratch.slots.len()));
                }
                (false, true) => {
                    let start = scratch.items.len();
                    if let Variable::Array(items) = f.value(c.list, lane) {
                        scratch.items.extend(items.borrow().iter().map(|v| (lane as u32, v.clone())));
                    }
                    scratch.spans.push((lane, start, scratch.items.len()));
                }
                (false, false) => {
                    let start = scratch.items.len();
                    for i in a..b {
                        let value = match f.items.row(i as usize) {
                            Some(row) => column.variable(row as usize),
                            None => f.items.get(i as usize).variable(&f.arena),
                        };
                        scratch.items.push((lane as u32, value));
                    }
                    scratch.spans.push((lane, start, scratch.items.len()));
                }
            }
        }
        Self::body(c, &mut scratch, rows.then_some(column), f, rest, ctx);
        let Scratch {
            items,
            slots,
            spans,
            results,
            failures,
            truths,
            settled,
            ..
        } = &mut scratch;
        for (lane, start, end) in spans.iter().copied() {
            let out = Outcomes::new((results, failures, truths, settled), start, end);
            let r = match rows {
                true => Self::finish(c.kind, out, |i| column.variable(slots[start + i].1 as usize)),
                false => Self::finish(c.kind, out, |i| items[start + i].1.clone()),
            };
            f.put(c.dst, lane, r);
        }
        f.scratch = scratch;
    }

    pub(super) fn fold<M: LaneSet>(op: &FoldOp, active: M, f: &mut Frame<M>, ctx: &Context) {
        let done = Self::fold_lanes(op, active, f, ctx);
        f.masks[op.rest as usize] = active & !done;
    }

    fn fold_lanes<M: LaneSet>(op: &FoldOp, active: M, f: &mut Frame<M>, ctx: &Context) -> M {
        let none = f.none();
        let Some(Some(column)) = ctx.column(op.site) else {
            return none;
        };
        let Values::List { child, .. } = column.values else {
            return none;
        };
        let child = child.column();
        if !matches!(child.values, Values::Struct { .. }) || child.validity.is_some() || f.kind(op.dst) != Kind::Num {
            return none;
        }
        let Some(input) = FoldInput::of(&child, &op.field, op.filter.as_deref()) else {
            return none;
        };
        let function = op.function;
        let done = match (function, input.valid, input.filter, input.numbers) {
            (crate::functions::InternalFunction::Sum, None, None, Numbers::Scaled(mant, scale)) => {
                Self::fold_rows(op, column, active, f, ctx, |a, b| FoldInput::sum_scaled(mant.get(a..b)?, scale.get(a..b)?))
            }
            (crate::functions::InternalFunction::Sum, _, _, Numbers::Decs(values)) => {
                Self::fold_rows(op, column, active, f, ctx, |a, b| input.walk(a, b, |i| Scaled::parts(values.get(i)?)))
            }
            (crate::functions::InternalFunction::Sum, _, _, Numbers::Scaled(mant, scale)) => {
                Self::fold_rows(op, column, active, f, ctx, |a, b| input.walk(a, b, |i| Some((*mant.get(i)?, *scale.get(i)?))))
            }
            (crate::functions::InternalFunction::Sum, _, _, _) => Self::fold_rows(op, column, active, f, ctx, |a, b| input.sum(a, b)),
            _ => Self::fold_rows(op, column, active, f, ctx, |a, b| {
                if !(a..b).all(|i| input.kept(i).is_some()) {
                    return None;
                }
                let values = (a..b).filter(|i| input.kept(*i) == Some(true)).map(|i| input.number(i));
                match Self::reduce(function, values) {
                    Some(Out::Num(n)) => Scaled::parts(&n),
                    _ => None,
                }
            }),
        };
        let d = op.dst as usize;
        f.wide[d] &= !done;
        f.boxed[d] &= !done;
        done
    }

    #[inline(always)]
    fn fold_rows<M: LaneSet>(
        op: &FoldOp,
        column: &Column,
        active: M,
        f: &mut Frame<M>,
        ctx: &Context,
        total: impl Fn(usize, usize) -> Option<(i64, u8)>,
    ) -> M {
        let mut done = f.none();
        let base = op.dst as usize * f.width;
        let offsets = match (column.values, column.validity) {
            (Values::List { offsets, .. }, None) => Some(offsets),
            _ => None,
        };
        for lane in Lanes::of(active) {
            let row = ctx.base + f.rows[lane] as usize;
            let range = match offsets {
                Some(offsets) => match (offsets.get(row), offsets.get(row + 1)) {
                    (Some(&a), Some(&b)) if 0 <= a && a <= b => Some((a as usize, b as usize)),
                    _ => column.range(row),
                },
                None => column.range(row),
            };
            if let Some((m, s)) = range.and_then(|(a, b)| total(a, b)) {
                f.mant[base + lane] = m;
                f.scales[base + lane] = s;
                done.set(lane);
            }
        }
        done
    }

    pub(super) fn list_children<'c>(
        c: &ClosureOp,
        ctx: &Context<'c>,
    ) -> Option<(Column<'c>, Column<'c>)> {
        let (_, site) = c.source.as_ref()?;
        match ctx.column(*site) {
            Some(Some(column)) => match column.values {
                Values::List { child, .. } => Some((child.column(), *column)),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn list_of<M: LaneSet>(
        c: &ClosureOp,
        lane: usize,
        f: &mut Frame<M>,
        ctx: &Context,
    ) -> Result<Variable, VMError> {
        let Some((load, site)) = &c.source else {
            return f.with(c.list, lane, Ops::elements);
        };
        let value = match ctx.column(*site) {
            Some(Some(column)) => column.variable(ctx.base + f.rows[lane] as usize),
            Some(None) => Variable::Null,
            None => Self::row_value(load, *site, lane, f, ctx)?,
        };
        Ops::elements(&value)
    }

    pub(super) fn single<M: LaneSet>(
        c: &ClosureOp,
        lane: usize,
        list: &Variable,
        f: &mut Frame<M>,
        rest: &mut [Frame<M>],
        ctx: &mut Context,
    ) -> Result<Variable, VMError> {
        let fallback = Ok(Variable::Bool(!matches!(c.kind, ClosureFunction::Some)));
        let Variable::Array(arr) = list else {
            return fallback;
        };
        let len = arr.borrow().len();
        if len == 0 {
            return fallback;
        }
        let element = c.body.element.unwrap_or_default();
        let (rows, parents) = ([f.rows[lane]], [lane as u32]);
        let Some(child) = rest.first_mut() else {
            return fallback;
        };
        Self::enter(&c.body, child, &rows, Some((f, &parents, &c.imports)));
        for i in 0..len {
            let Some(item) = arr.borrow().get(i).cloned() else {
                break;
            };
            let child = &mut rest[0];
            child.restart();
            child.set(element, 0, item);
            Self::execute(&c.body, rest, ctx);
            let child = &mut rest[0];
            let r = match child.alive.get(0) {
                true => Ok(child.take(c.body.out, 0)),
                _ => Err(child.errors[0]
                    .take().map(Fault::vm)
                    .unwrap_or(VMError::NumberConversionError)),
            };
            if let Some(b) = Self::verdict(c.kind, r)? {
                return Ok(Variable::Bool(b));
            }
        }
        fallback
    }

    pub(super) fn finish(
        kind: ClosureFunction,
        mut out: Outcomes,
        item: impl Fn(usize) -> Variable,
    ) -> Result<Variable, VMError> {
        match kind {
            ClosureFunction::Map | ClosureFunction::FlatMap => {
                if let Some((_, e)) = out.failures.first() {
                    return Err(e.clone());
                }
                let values: Vec<Variable> = out
                    .values
                    .iter_mut()
                    .map(|v| std::mem::replace(v, Variable::Null))
                    .collect();
                match kind {
                    ClosureFunction::Map => Ok(Variable::from_array(values)),
                    _ => Ops::flatten(Variable::from_array(values)),
                }
            }
            ClosureFunction::Filter => {
                let mut kept = Vec::new();
                for i in 0..out.len() {
                    if out.truth(i)? {
                        kept.push(item(i));
                    }
                }
                Ok(Variable::from_array(kept))
            }
            ClosureFunction::Count | ClosureFunction::One => {
                let count = match out.tally() {
                    Some((trues, _)) => trues,
                    None => {
                        let mut count = 0usize;
                        for i in 0..out.len() {
                            if out.truth(i)? {
                                count += 1;
                            }
                        }
                        count
                    }
                };
                Ok(match kind {
                    ClosureFunction::One => Variable::Bool(count == 1),
                    _ => Variable::Number(count.into()),
                })
            }
            ClosureFunction::All | ClosureFunction::Some | ClosureFunction::None
                if out.tally().is_some() =>
            {
                let decided = out.decide(kind).flatten();
                Ok(Variable::Bool(
                    decided.unwrap_or(!matches!(kind, ClosureFunction::Some)),
                ))
            }
            ClosureFunction::All | ClosureFunction::Some | ClosureFunction::None => {
                for i in 0..out.len() {
                    if let Some(b) = out.verdict(kind, i)? {
                        return Ok(Variable::Bool(b));
                    }
                }
                Ok(Variable::Bool(!matches!(kind, ClosureFunction::Some)))
            }
        }
    }

    pub(super) fn verdict(
        kind: ClosureFunction,
        r: Result<Variable, VMError>,
    ) -> Result<Option<bool>, VMError> {
        r.and_then(|v| match kind {
            ClosureFunction::Some => Ops::truthy(&v, "JumpIfTrue").map(|t| t.then_some(true)),
            ClosureFunction::All => Ops::truthy(&v, "JumpIfFalse").map(|t| (!t).then_some(false)),
            _ => Ops::not(v)
                .and_then(|n| Ops::truthy(&n, "JumpIfFalse"))
                .map(|t| (!t).then_some(false)),
        })
    }
}

impl ClosureFunction {
    pub(super) fn holds(self, trues: usize, total: usize) -> bool {
        match self {
            ClosureFunction::One => trues == 1,
            ClosureFunction::Some => trues > 0,
            ClosureFunction::All => trues == total,
            _ => trues == 0,
        }
    }
}

#[derive(Clone, Copy)]
enum Numbers<'c> {
    Scaled(&'c [i64], &'c [u8]),
    Ints(&'c [i64]),
    Decs(&'c [rust_decimal::Decimal]),
    Other(Column<'c>),
}

type Bitmap<'c> = Option<(&'c [u64], usize)>;

struct FoldInput<'c> {
    numbers: Numbers<'c>,
    valid: Bitmap<'c>,
    filter: Option<(&'c [u64], usize, Bitmap<'c>)>,
}

impl<'c> FoldInput<'c> {
    fn of(child: &Column<'c>, field: &str, filter: Option<&str>) -> Option<Self> {
        let column = *child.field(field)?;
        let numbers = match column.values {
            Values::Scaled { mant, scale } => Numbers::Scaled(mant, scale),
            Values::I64(values) => Numbers::Ints(values),
            Values::Dec(values) => Numbers::Decs(values),
            _ => Numbers::Other(column),
        };
        let filter = match filter {
            None => None,
            Some(key) => match child.field(key).copied() {
                Some(Column { values: Values::Bool { bits, offset }, validity }) => Some((bits, offset, validity)),
                _ => return None,
            },
        };
        Some(Self {
            numbers,
            valid: column.validity,
            filter,
        })
    }

    #[inline(always)]
    fn number(&self, i: usize) -> Option<(i64, u8)> {
        if let Some((bits, offset)) = self.valid {
            if !Column::bit(bits, offset + i) {
                return None;
            }
        }
        match self.numbers {
            Numbers::Scaled(mant, scale) => Some((*mant.get(i)?, *scale.get(i)?)),
            Numbers::Ints(values) => Some((*values.get(i)?, 0)),
            Numbers::Decs(values) => Scaled::parts(values.get(i)?),
            Numbers::Other(column) => Executor::child_number(&column, i),
        }
    }

    #[inline(always)]
    fn kept(&self, i: usize) -> Option<bool> {
        let Some((bits, offset, valid)) = self.filter else {
            return Some(true);
        };
        if let Some((mask, at)) = valid {
            if !Column::bit(mask, at + i) {
                return None;
            }
        }
        Some(Column::bit(bits, offset + i))
    }

    #[inline(always)]
    fn accumulate(acc: (i64, u8), x: (i64, u8)) -> Option<(i64, u8)> {
        match (acc.0 == 0, x.0 == 0, acc.1 == x.1) {
            (true, _, _) => Some(x),
            (false, true, _) => Some(acc),
            (false, false, true) => Some((acc.0.checked_add(x.0)?, acc.1)),
            (false, false, false) => Scaled::add(acc, x, false),
        }
    }

    #[inline]
    fn sum(&self, a: usize, b: usize) -> Option<(i64, u8)> {
        match (self.valid, self.filter, self.numbers) {
            (None, None, Numbers::Scaled(mant, scale)) => Self::sum_scaled(mant.get(a..b)?, scale.get(a..b)?),
            (_, _, Numbers::Scaled(mant, scale)) => self.walk(a, b, |i| Some((*mant.get(i)?, *scale.get(i)?))),
            (_, _, Numbers::Ints(values)) => self.walk(a, b, |i| Some((*values.get(i)?, 0))),
            (_, _, Numbers::Decs(values)) => self.walk(a, b, |i| Scaled::parts(values.get(i)?)),
            (_, _, Numbers::Other(column)) => self.walk(a, b, |i| Executor::child_number(&column, i)),
        }
    }

    #[inline(always)]
    fn sum_scaled(mant: &[i64], scale: &[u8]) -> Option<(i64, u8)> {
        let mut acc = (0i64, 0u8);
        for (m, s) in mant.iter().zip(scale) {
            acc = match *s == acc.1 {
                true => (acc.0.checked_add(*m)?, acc.1),
                false => Self::accumulate(acc, (*m, *s))?,
            };
        }
        Some(acc)
    }

    #[inline(always)]
    fn walk(&self, a: usize, b: usize, number: impl Fn(usize) -> Option<(i64, u8)>) -> Option<(i64, u8)> {
        let mut acc = (0i64, 0u8);
        for i in a..b {
            let kept = self.kept(i)?;
            let valid = self.valid.is_none_or(|(bits, offset)| Column::bit(bits, offset + i));
            let x = match (kept, valid, number(i)) {
                (false, _, _) => (0, acc.1),
                (true, true, Some(x)) => x,
                (true, _, _) => return None,
            };
            acc = match x.1 == acc.1 {
                true => (acc.0.checked_add(x.0)?, acc.1),
                false => Self::accumulate(acc, x)?,
            };
        }
        Some(acc)
    }
}
