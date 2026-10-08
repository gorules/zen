use super::context::Context;
use super::frame::Frame;
use super::Executor;
use crate::lane::columns::Values;
use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::ops::Ops;
use crate::lane::program::{Kind, Subject, SwitchOp};
use crate::lane::scaled::Scaled;
use crate::variable::Variable;
use zen_types::symbol::Symbol;

impl Executor {
    pub(super) fn switch<M: LaneSet>(op: &SwitchOp, active: M, f: &mut Frame<M>, ctx: &Context) {
        let mut picks = std::mem::take(&mut f.picks);
        picks.clear();
        picks.resize(f.width, 0);
        let done = match op.subject {
            Subject::Site(site) => Self::switch_column(op, site, active, f, ctx, &mut picks),
            Subject::Reg(reg) => Self::switch_register(op, reg, active, f, &mut picks),
        };
        if done.any() {
            Self::switch_write(op, done, f, &picks);
        }
        f.picks = picks;
        if let Subject::Site(_) = op.subject {
            f.masks[op.rest as usize] = active & !done;
        }
    }

    fn switch_column<M: LaneSet>(op: &SwitchOp, site: u16, active: M, f: &mut Frame<M>, ctx: &Context, picks: &mut [u8]) -> M {
        let fallback = op.cases.len() as u8;
        let width = f.width;
        let Some(Some(column)) = ctx.column(site) else {
            return f.none();
        };
        let text = match column.values {
            Values::Utf8 { offsets, data } => Some((offsets, data)),
            Values::Text { offsets, data } => Some((offsets, data.as_bytes())),
            _ => None,
        };
        match (column.values, text) {
            (_, Some((offsets, data))) if f.dense => {
                let Some(at) = Self::prefixes(f, offsets, data, ctx.base) else {
                    return f.none();
                };
                let Some(window) = offsets.get(ctx.base..=ctx.base + width) else {
                    return f.none();
                };
                let keys: Vec<(u32, u64, &[u8])> = op
                    .cases
                    .iter()
                    .map(|case| (u32::try_from(case.len()).unwrap_or(u32::MAX), Self::prefix(case.as_bytes()), case.as_bytes()))
                    .collect();
                let valid: M = column.valid_mask(ctx.base, width);
                let entry = &f.prefixes[at];
                let long = keys.iter().any(|(_, _, text)| text.len() > 8);
                for (lane, ((pick, len), word)) in picks.iter_mut().zip(&entry.lens).zip(&entry.words).enumerate() {
                    let mut chosen = fallback;
                    for (case, (n, key, _)) in keys.iter().enumerate().rev() {
                        let hit = (*n == *len) & (*key == *word);
                        chosen = [chosen, case as u8][hit as usize];
                    }
                    if long && chosen != fallback && keys[chosen as usize].2.len() > 8 {
                        let text = data.get(window[lane] as usize..window[lane + 1] as usize);
                        chosen = keys
                            .iter()
                            .position(|(_, _, case)| text == Some(*case))
                            .map_or(fallback, |at| at as u8);
                    }
                    *pick = chosen;
                }
                for lane in Lanes::of(active & !valid) {
                    picks[lane] = fallback;
                }
                active
            }
            (Values::Dict { keys, values }, _) if values.len() <= 256 => {
                let codes: Vec<u8> = (0..values.len())
                    .map(|code| {
                        op.cases
                            .iter()
                            .position(|case| values.equals(code, &Variable::String(Symbol::from(case.as_ref()))))
                            .map_or(fallback, |at| at as u8)
                    })
                    .collect();
                for lane in Lanes::of(active) {
                    let row = ctx.base + f.rows[lane] as usize;
                    picks[lane] = match (column.valid(row), keys.get(row).and_then(|k| usize::try_from(*k).ok())) {
                        (true, Some(code)) => codes.get(code).copied().unwrap_or(fallback),
                        _ => fallback,
                    };
                }
                active
            }
            _ => f.none(),
        }
    }

    fn switch_register<M: LaneSet>(op: &SwitchOp, reg: u16, active: M, f: &mut Frame<M>, picks: &mut [u8]) -> M {
        let fallback = op.cases.len() as u8;
        let typed = match f.kind(reg) {
            Kind::Str => active & !f.boxed[reg as usize],
            _ => f.none(),
        };
        let mut memo = [((u32::MAX, u32::MAX), fallback); 4];
        let base = reg as usize * f.width;
        for lane in Lanes::of(typed) {
            let span = f.spans[base + lane];
            let slot = (span.0 as usize ^ (span.1 as usize).rotate_left(2)) & 3;
            picks[lane] = match memo[slot].0 == span {
                true => memo[slot].1,
                false => {
                    let text = f.text_at(reg, lane);
                    let pick = op.cases.iter().position(|case| case.as_ref() == text).map_or(fallback, |at| at as u8);
                    memo[slot] = (span, pick);
                    pick
                }
            };
        }
        let cases: Vec<Variable> = op.cases.iter().map(|case| Variable::String(Symbol::from(case.as_ref()))).collect();
        for lane in Lanes::of(active & !typed) {
            picks[lane] = f.with(reg, lane, |value| cases.iter().position(|case| Ops::equal(value, case))).map_or(fallback, |at| at as u8);
        }
        active
    }

    fn switch_write<M: LaneSet>(op: &SwitchOp, lanes: M, f: &mut Frame<M>, picks: &[u8]) {
        let (d, width) = (op.dst as usize, f.width);
        let base = d * width;
        let values: Vec<Variable> = op.values.iter().map(|id| f.consts[*id as usize].clone()).collect();
        let strings: Option<Vec<&str>> = values.iter().map(|v| v.as_str()).collect();
        let numbers: Option<Vec<(i64, u8)>> = values
            .iter()
            .map(|v| match v {
                Variable::Number(n) => Scaled::parts(n),
                _ => None,
            })
            .collect();
        let bools: Option<Vec<bool>> = values.iter().map(Variable::as_bool).collect();
        match (f.kinds[d], strings, numbers, bools) {
            (Kind::Str, Some(texts), _, _) => {
                let spans: Vec<(u32, u32)> = texts.iter().map(|text| f.intern(text)).collect();
                match lanes == M::all(width) {
                    true => f.spans[base..base + width]
                        .iter_mut()
                        .zip(picks)
                        .for_each(|(span, pick)| *span = spans[*pick as usize]),
                    false => Lanes::of(lanes).for_each(|lane| f.spans[base + lane] = spans[picks[lane] as usize]),
                }
                f.boxed[d] &= !lanes;
            }
            (Kind::Num, _, Some(parts), _) => {
                match lanes == M::all(width) {
                    true => f.mant[base..base + width]
                        .iter_mut()
                        .zip(f.scales[base..base + width].iter_mut())
                        .zip(picks)
                        .for_each(|((m, s), pick)| (*m, *s) = parts[*pick as usize]),
                    false => Lanes::of(lanes).for_each(|lane| {
                        let (m, s) = parts[picks[lane] as usize];
                        f.mant[base + lane] = m;
                        f.scales[base + lane] = s;
                    }),
                }
                f.wide[d] &= !lanes;
                f.boxed[d] &= !lanes;
            }
            (Kind::Bool, _, _, Some(flags)) => {
                let mut word = f.none();
                for lane in Lanes::of(lanes) {
                    word.put(lane, flags[picks[lane] as usize]);
                }
                f.put_mask(op.dst, lanes, word);
            }
            _ => {
                for lane in Lanes::of(lanes) {
                    f.set(op.dst, lane, values[picks[lane] as usize].clone());
                }
            }
        }
    }
}
