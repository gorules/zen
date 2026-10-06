use super::frame::Frame;
use super::Executor;
use crate::functions::{FunctionKind, InternalFunction, MethodKind};
use crate::lane::builtins::{Arg, Dates};
use crate::lane::date::Date;
use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::program::{Input, Kind, Reg};
use std::str::FromStr;

impl Executor {
    pub(super) fn date_call<M: LaneSet>(
        dst: Reg,
        kind: &FunctionKind,
        args: &[Input],
        active: M,
        f: &mut Frame<M>,
    ) -> M {
        let mut done = f.none();
        let zone = match (kind, args) {
            (FunctionKind::Internal(InternalFunction::Date), [Input::Reg(_)]) => None,
            (FunctionKind::Internal(InternalFunction::Date), [Input::Reg(_), Input::Const(id)]) => {
                match f.consts[*id as usize].as_str().map(chrono_tz::Tz::from_str) {
                    Some(Ok(zone)) => Some(zone),
                    _ => return done,
                }
            }
            _ => return done,
        };
        let Some(Input::Reg(a)) = args.first().copied() else {
            return done;
        };
        if f.kind(a) != Kind::Str || f.kind(dst) != Kind::Date {
            return done;
        }
        let mut last: Option<((u32, u32), Date)> = None;
        for lane in Lanes::of(active & !f.boxed[a as usize]) {
            let span = f.spans[f.at(a, lane)];
            let text = f
                .arena
                .get(span.0 as usize..span.1 as usize)
                .unwrap_or_default();
            let date = match &last {
                Some((prev, date))
                    if f.arena.get(prev.0 as usize..prev.1 as usize) == Some(text) =>
                {
                    *date
                }
                _ => Date::text(text, zone),
            };
            let at = f.at(dst, lane);
            f.dates[at] = date;
            f.boxed[dst as usize].unset(lane);
            last = Some((span, date));
            done.set(lane);
        }
        done
    }

    pub(super) fn date_method<M: LaneSet>(
        dst: Reg,
        kind: &MethodKind,
        args: &[Input],
        active: M,
        f: &mut Frame<M>,
    ) -> M {
        let mut done = f.none();
        match (args, f.kind(dst)) {
            ([Input::Reg(this)], Kind::Num) if f.kind(*this) == Kind::Date => {
                for lane in Lanes::of(active & !f.boxed[*this as usize]) {
                    let date = &f.dates[f.at(*this, lane)];
                    if let Some(n) = Dates::part_of(kind, date) {
                        f.put_scaled(dst, lane, n, 0);
                        done.set(lane);
                    }
                }
            }
            ([Input::Reg(this), rest @ ..], Kind::Date)
                if f.kind(*this) == Kind::Date
                    && rest.iter().all(|a| matches!(a, Input::Const(_))) =>
            {
                let consts: smallvec::SmallVec<[Arg; 4]> = std::iter::once(Arg::Null)
                    .chain(rest.iter().map(|a| match a {
                        Input::Const(id) => Arg::of(&f.consts[*id as usize]),
                        Input::Reg(_) => Arg::Null,
                    }))
                    .collect();
                let Some(shift) = Dates::shift(kind, &consts) else {
                    return done;
                };
                for lane in Lanes::of(active & !f.boxed[*this as usize]) {
                    let date = shift.apply(&f.dates[f.at(*this, lane)]);
                    let at = f.at(dst, lane);
                    f.dates[at] = date;
                    f.boxed[dst as usize].unset(lane);
                    done.set(lane);
                }
            }
            ([Input::Reg(this), Input::Reg(other)], Kind::Bool)
                if f.kind(*this) == Kind::Date && f.kind(*other) == Kind::Date =>
            {
                let lanes = active & !f.boxed[*this as usize] & !f.boxed[*other as usize];
                for lane in Lanes::of(lanes) {
                    let (x, y) = (&f.dates[f.at(*this, lane)], &f.dates[f.at(*other, lane)]);
                    if let Some(hit) = Dates::order(kind, x, y) {
                        f.bits[dst as usize].put(lane, hit);
                        f.boxed[dst as usize].unset(lane);
                        done.set(lane);
                    }
                }
            }
            _ => {}
        }
        done
    }
}
