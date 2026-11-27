use super::frame::Frame;
use super::Executor;
use crate::functions::FunctionKind;
use crate::lane::builtins::{Text, TextKernel};
use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::ops::Ops;
use crate::lane::program::{Input, Kind, Reg};
use crate::variable::Variable;
use zen_types::symbol::Symbol;

impl Executor {
    pub(super) fn text_call<M: LaneSet>(
        dst: Reg,
        kind: &FunctionKind,
        args: &[Input],
        active: M,
        f: &mut Frame<M>,
    ) -> M {
        let Some(kernel) = Text::kernel(kind) else {
            return f.none();
        };
        let (a, k) = match (kernel, args) {
            (TextKernel::Test(_), [Input::Reg(a), Input::Const(id)]) => {
                match &f.consts[*id as usize] {
                    Variable::String(k) => (*a, Some(k.clone())),
                    _ => return f.none(),
                }
            }
            (TextKernel::Map(_) | TextKernel::Size, [Input::Reg(a)]) => (*a, None),
            _ => return f.none(),
        };
        if f.kind(a) != Kind::Str {
            return f.none();
        }
        Self::text_kernel(kernel, dst, a, active, k.as_deref(), f)
    }

    pub(super) fn text_kernel<M: LaneSet>(
        kernel: TextKernel,
        dst: Reg,
        a: Reg,
        lanes: M,
        k: Option<&str>,
        f: &mut Frame<M>,
    ) -> M {
        let d = dst as usize;
        let (base, from) = (d * f.width, a as usize * f.width);
        let done = lanes & !f.boxed[a as usize];
        match kernel {
            TextKernel::Test(op) => {
                let (Some(k), Kind::Bool) = (k, f.kind(dst)) else {
                    return f.none();
                };
                let mut word = f.none();
                for lane in Lanes::of(done) {
                    word.put(lane, op.apply(f.text_at(a, lane), k));
                }
                f.bits[d] = (f.bits[d] & !done) | word;
            }
            TextKernel::Size => {
                if f.kind(dst) != Kind::Num {
                    return f.none();
                }
                for lane in Lanes::of(done) {
                    let (x, y) = f.spans[from + lane];
                    f.mant[base + lane] = (y - x) as i64;
                    f.scales[base + lane] = 0;
                }
                f.wide[d] &= !done;
            }
            TextKernel::Map(op) => {
                if f.kind(dst) != Kind::Str {
                    return f.none();
                }
                let spans = &f.spans[from..from + f.width];
                let block = (done == M::all(f.width))
                    .then(|| Frame::<M>::block(spans))
                    .flatten()
                    .and_then(|(x, y)| f.arena.get(x..y).map(|t| (x, y, t)));
                let bulk = match (op.bytewise(), block) {
                    (Some(bytewise), Some((x, y, text))) if text.is_ascii() => {
                        Some((Some(bytewise), x, y))
                    }
                    (None, Some((x, y, text)))
                        if spans.iter().all(|(p, q)| {
                            op.unchanged(
                                text.get(*p as usize - x..*q as usize - x)
                                    .unwrap_or_default(),
                            )
                        }) =>
                    {
                        Some((None, x, y))
                    }
                    _ => None,
                };
                match bulk {
                    Some((None, _, _)) => {
                        for lane in 0..f.width {
                            f.spans[base + lane] = f.spans[from + lane];
                        }
                    }
                    Some((Some(bytewise), x, y)) => {
                        let origin = f.arena.len();
                        f.arena.extend_from_within(x..y);
                        if let Some(tail) = f.arena.get_mut(origin..) {
                            bytewise(tail);
                        }
                        let shift = origin as u32 - x as u32;
                        for lane in 0..f.width {
                            let (p, q) = f.spans[from + lane];
                            f.spans[base + lane] = (p + shift, q + shift);
                        }
                    }
                    None => {
                        let mut scratch = std::mem::take(&mut f.text);
                        scratch.clear();
                        let origin = f.arena.len() as u32;
                        let (arena, spans) = (&f.arena, &mut f.spans);
                        for lane in Lanes::of(done) {
                            let before = scratch.len() as u32;
                            let (x, y) = spans[from + lane];
                            let text = arena.get(x as usize..y as usize).unwrap_or_default();
                            spans[base + lane] = match op.apply(text, &mut scratch) {
                                Some((p, q)) => (x + p as u32, x + q as u32),
                                None => (origin + before, origin + scratch.len() as u32),
                            };
                        }
                        f.arena.push_str(&scratch);
                        f.text = scratch;
                    }
                }
            }
        }
        f.boxed[d] &= !done;
        done
    }

    pub(super) fn join<M: LaneSet>(dst: Reg, parts: &[Input], active: M, f: &mut Frame<M>) {
        let texts = f.kind(dst) == Kind::Str
            && parts.iter().all(|p| match p {
                Input::Const(id) => matches!(f.consts[*id as usize], Variable::String(_)),
                Input::Reg(r) => f.kind(*r) == Kind::Str,
            });
        let typed = match texts {
            true => parts.iter().fold(active, |acc, p| match p {
                Input::Reg(r) => acc & !f.boxed[*r as usize],
                Input::Const(_) => acc,
            }),
            false => f.none(),
        };
        if typed.any() {
            let mut text = std::mem::take(&mut f.text);
            text.clear();
            let origin = f.arena.len() as u32;
            let base = dst as usize * f.width;
            let (arena, spans, consts, width) =
                (f.arena.as_str(), &mut f.spans, &f.consts, f.width);
            for lane in Lanes::of(typed) {
                let before = text.len() as u32;
                for part in parts {
                    let piece = match part {
                        Input::Const(id) => consts[*id as usize].as_str().unwrap_or_default(),
                        Input::Reg(r) => {
                            let (x, y) = spans[*r as usize * width + lane];
                            arena.get(x as usize..y as usize).unwrap_or_default()
                        }
                    };
                    text.push_str(piece);
                }
                spans[base + lane] = (origin + before, origin + text.len() as u32);
            }
            f.arena.push_str(&text);
            f.text = text;
            f.boxed[dst as usize] &= !typed;
        }
        Self::join_rows(dst, parts, active & !typed, f);
    }

    pub(super) fn join_rows<M: LaneSet>(dst: Reg, parts: &[Input], active: M, f: &mut Frame<M>) {
        let mut text = std::mem::take(&mut f.text);
        let typed = f.kind(dst) == Kind::Str;
        for lane in Lanes::of(active) {
            text.clear();
            let mut failed = None;
            for (i, p) in parts.iter().enumerate() {
                let pushed = match p {
                    Input::Const(id) => Self::push_text(&mut text, &f.consts[*id as usize]),
                    Input::Reg(r) if f.kind(*r) == Kind::Str && !f.boxed[*r as usize].get(lane) => {
                        text.push_str(f.text_at(*r, lane));
                        true
                    }
                    Input::Reg(r) => f.with(*r, lane, |v| Self::push_text(&mut text, v)),
                };
                if !pushed {
                    failed = Some(i);
                    break;
                }
            }
            match (failed, typed) {
                (Some(i), _) => f.fail(
                    lane,
                    Ops::error("Join", format!("Unexpected type in array on index {i}")),
                ),
                (None, true) => {
                    f.put_text(dst, lane, &text);
                    f.boxed[dst as usize].unset(lane);
                }
                (None, false) => f.set(dst, lane, Variable::String(Symbol::from(text.as_str()))),
            }
        }
        f.text = text;
    }

    pub(super) fn push_text(text: &mut String, v: &Variable) -> bool {
        match v {
            Variable::String(s) => {
                text.push_str(s);
                true
            }
            _ => false,
        }
    }
}
