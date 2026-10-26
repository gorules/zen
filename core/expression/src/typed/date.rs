use crate::functions::{DateMethod, FunctionKind, InternalFunction, MethodKind};
use crate::lane::builtins::{Arg as Given, Builtin, Builtins, Dates, Out};
use crate::lane::date::Date;
use crate::lane::Bits;
use crate::typed::exec::{Buf, Exec, Others, Reg};
use crate::typed::hir::{Clock, Host};
use crate::typed::ins::{Arg, Ins, Rep};
use crate::typed::num::{Dec, N};
use crate::typed::text::TV;
use crate::variable::Variable;
use smallvec::SmallVec;

#[derive(Clone, Copy)]
enum DV<'a, 'x> {
    Text(TV<'a, 'x>),
    Reg(&'a Reg, Rep),
}

impl Host {
    pub(crate) fn builtin(self) -> Option<&'static Builtin> {
        match self {
            Host::Construct => Builtins::of(&FunctionKind::Internal(InternalFunction::Date)),
            Host::Legacy(function) => Builtins::of(&FunctionKind::Deprecated(function)),
            Host::Method(method) => Some(Builtins::method(&MethodKind::DateMethod(method))),
        }
    }
}

impl<'a, 'x> DV<'a, 'x> {
    #[inline]
    fn given(self, start: usize, i: usize) -> Option<Given<'a>> {
        match self {
            DV::Text(text) => match text.get(start, i) {
                None => Some(Given::Null),
                Some(bytes) => std::str::from_utf8(bytes).ok().map(Given::Str),
            },
            DV::Reg(r, _) if !Bits::get(&r.valid, i) => Some(Given::Null),
            DV::Reg(r, rep) => Some(match rep {
                Rep::Fix(_) | Rep::Dyn => Given::Num(r.num(rep, i).decimal()),
                Rep::Bool => Given::Bool(Bits::get(&r.bits, i)),
                Rep::Date => Given::Date(r.date(i)),
                Rep::Null => Given::Null,
                Rep::Text | Rep::Code => return None,
            }),
        }
    }

    #[inline]
    fn bad(self, start: usize, w: usize) -> u64 {
        match self {
            DV::Text(text) => text.bad(start, w),
            DV::Reg(r, _) => r.fault[w],
        }
    }
}

impl Reg {
    pub(crate) fn stamps(&mut self) {
        if self.dates.len() < Reg::LANES {
            self.dates.resize(Reg::LANES, Date(None));
        }
    }

    #[inline]
    pub(crate) fn date(&self, i: usize) -> Date {
        self.dates.get(i).copied().unwrap_or(Date(None))
    }

    #[inline]
    fn store(&mut self, rep: Rep, i: usize, out: Out) -> bool {
        match (rep, out) {
            (_, Out::Var(Variable::Null)) => Bits::set(&mut self.valid, i, false),
            (Rep::Date, Out::Date(date)) => self.dates[i] = date,
            (Rep::Fix(_) | Rep::Dyn, Out::Num(n)) => self.keep(rep, i, N::of(n)),
            (Rep::Bool, Out::Bool(b)) => Bits::set(&mut self.bits, i, b),
            (Rep::Text, Out::Str(s)) => self.push(i, s.as_bytes()),
            _ => return false,
        }
        true
    }
}

impl Buf {
    pub(crate) fn stamps(&mut self) {
        if self.dates.len() < self.mant.len() {
            self.dates.resize(self.mant.len(), Date(None));
        }
    }
}

impl<'e, 'x> Exec<'e, 'x> {
    fn same(a: &Given, b: &Given) -> bool {
        match (a, b) {
            (Given::Null, Given::Null) => true,
            (Given::Num(x), Given::Num(y)) => {
                x.mantissa() == y.mantissa() && x.scale() == y.scale()
            }
            (Given::Bool(x), Given::Bool(y)) => x == y,
            (Given::Str(x), Given::Str(y)) => x == y,
            (Given::Date(x), Given::Date(y)) => {
                x == y && x.0.map(|at| at.timezone()) == y.0.map(|at| at.timezone())
            }
            _ => false,
        }
    }

    fn dated<'a>(&'a self, arg: &'a Arg, rep: Rep, regs: &'a Others) -> DV<'a, 'x> {
        match (arg, rep) {
            (Arg::Reg(r), rep) if rep != Rep::Text => DV::Reg(&regs[*r as usize], rep),
            _ => DV::Text(self.view(arg, regs)),
        }
    }

    fn broken(d: &mut Reg, lanes: Vec<usize>) {
        lanes
            .into_iter()
            .for_each(|i| Bits::set(&mut d.fault, i, true));
    }

    pub(crate) fn date(&self, ins: &Ins, regs: &mut [Reg], act: Option<&[u64; Reg::WORDS]>) {
        let (n, words, start) = (self.n, self.words.min(Reg::WORDS), self.start);
        match ins {
            Ins::Now { dst } => Self::def(regs, *dst, |d, _| {
                d.stamps();
                d.dates[..n].fill(self.feeds.now);
                d.valid[..words].fill(u64::MAX);
                d.fault[..words].fill(0);
            }),
            Ins::Host {
                host,
                dst,
                args,
                rep,
                fixed,
            } => Self::def(regs, *dst, |d, regs| {
                let views: SmallVec<[DV; 4]> =
                    args.iter().map(|(a, r)| self.dated(a, *r, regs)).collect();
                for w in 0..words {
                    d.valid[w] = u64::MAX;
                    d.fault[w] =
                        views.iter().fold(0, |acc, v| acc | v.bad(start, w)) & self.tail(w);
                }
                match rep {
                    Rep::Text => d.texts(n),
                    Rep::Date => d.stamps(),
                    _ => {}
                }
                let Some(builtin) = host.builtin() else {
                    for w in 0..words {
                        d.fault[w] = self.tail(w);
                    }
                    return;
                };
                let skip = d.fault;
                let mut broken = Vec::new();
                if let (Host::Construct, [DV::Text(text)]) = (host, views.as_slice()) {
                    d.stamps();
                    let mut last: Option<(&[u8], Date)> = None;
                    self.each(act, &skip, |i| {
                        let bytes = text.get(start, i);
                        let date = match (bytes, last) {
                            (Some(b), Some((prev, date))) if prev == b => Some(date),
                            (Some(b), _) => {
                                std::str::from_utf8(b).ok().map(|t| Date::text(t, None))
                            }
                            (None, _) => Some(Date(None)),
                        };
                        match date {
                            Some(date) => {
                                d.dates[i] = date;
                                last = bytes.map(|b| (b, date));
                            }
                            None => broken.push(i),
                        }
                    });
                    Self::broken(d, broken);
                    return;
                }
                if let (Host::Method(method), true) = (host, *fixed) {
                    if let Some(broken) = self.quick(*method, &views, *rep, d, act) {
                        Self::broken(d, broken);
                        return;
                    }
                }
                let mut given: SmallVec<[Given; 4]> = SmallVec::new();
                let mut seen: SmallVec<[Given; 4]> = SmallVec::new();
                let mut last: Option<Option<Out>> = None;
                self.each(act, &skip, |i| {
                    given.clear();
                    for v in &views {
                        match v.given(start, i) {
                            Some(g) => given.push(g),
                            None => {
                                broken.push(i);
                                return;
                            }
                        }
                    }
                    let repeated = last.is_some()
                        && seen.len() == given.len()
                        && seen.iter().zip(&given).all(|(a, b)| Self::same(a, b));
                    if !repeated {
                        last = Some(Builtins::raw(builtin, &given).ok());
                        seen.clear();
                        seen.extend(given.iter().copied());
                    }
                    let stored = last
                        .clone()
                        .flatten()
                        .is_some_and(|out| d.store(*rep, i, out));
                    if !stored {
                        broken.push(i);
                    }
                });
                Self::broken(d, broken);
            }),
            Ins::Clock { clock, dst, args } => Self::def(regs, *dst, |d, regs| {
                let views: SmallVec<[DV; 2]> =
                    args.iter().map(|(a, r)| self.dated(a, *r, regs)).collect();
                for w in 0..words {
                    d.valid[w] = u64::MAX;
                    d.fault[w] =
                        views.iter().fold(0, |acc, v| acc | v.bad(start, w)) & self.tail(w);
                }
                let skip = d.fault;
                let mut broken = Vec::new();
                match (clock, views.as_slice()) {
                    (Clock::Coerce, [text]) => {
                        d.stamps();
                        self.each(act, &skip, |i| match text.given(start, i) {
                            Some(Given::Str(t)) => match Date::from_text(t) {
                                date if date.is_valid() => d.dates[i] = date,
                                _ => broken.push(i),
                            },
                            _ => broken.push(i),
                        });
                    }
                    (Clock::Strict, [date]) => {
                        d.stamps();
                        self.each(act, &skip, |i| match date.given(start, i) {
                            Some(Given::Date(x)) if x.is_valid() => d.dates[i] = x,
                            _ => broken.push(i),
                        });
                    }
                    (Clock::Text, [date]) => {
                        d.texts(n);
                        let mut last = None;
                        self.each(act, &skip, |i| match date.given(start, i) {
                            Some(Given::Date(x)) => {
                                let key = (x, x.0.map(|at| at.timezone()));
                                if last.as_ref().is_none_or(|(seen, _)| *seen != key) {
                                    last = Some((key, x.rendered()));
                                }
                                match last.as_ref().and_then(|(_, text)| text.as_deref()) {
                                    Some(text) => d.push(i, text.as_bytes()),
                                    None => broken.push(i),
                                }
                            }
                            _ => Bits::set(&mut d.valid, i, false),
                        });
                    }
                    (Clock::Matches, [date, text]) => {
                        self.bools(act, &mut d.bits, |i| {
                            match (date.given(start, i), text.given(start, i)) {
                                (Some(Given::Date(x)), Some(Given::Str(t))) => {
                                    x.is_valid() && Date::from_text(t) == x
                                }
                                (Some(Given::Date(x)), Some(Given::Date(y))) => {
                                    x.is_valid() && x == y
                                }
                                _ => false,
                            }
                        });
                    }
                    _ => {
                        for w in 0..words {
                            d.fault[w] = self.tail(w);
                        }
                    }
                }
                Self::broken(d, broken);
            }),
            _ => {}
        }
    }

    fn quick(
        &self,
        method: DateMethod,
        views: &[DV],
        rep: Rep,
        d: &mut Reg,
        act: Option<&[u64; Reg::WORDS]>,
    ) -> Option<Vec<usize>> {
        let (start, skip) = (self.start, d.fault);
        let kind = MethodKind::DateMethod(method);
        let (this, rest) = views.split_first()?;
        let mut consts: SmallVec<[Given; 4]> = SmallVec::new();
        consts.push(Given::Null);
        for v in rest {
            consts.push(v.given(start, 0)?);
        }
        let mut broken = Vec::new();
        let part = matches!(
            method,
            DateMethod::Second
                | DateMethod::Minute
                | DateMethod::Hour
                | DateMethod::Day
                | DateMethod::Weekday
                | DateMethod::DayOfYear
                | DateMethod::Week
                | DateMethod::Month
                | DateMethod::Quarter
                | DateMethod::Year
                | DateMethod::Timestamp
        );
        match rep {
            Rep::Date => {
                let shift = Dates::shift(&kind, &consts)?;
                self.each(act, &skip, |i| match this.given(start, i) {
                    Some(Given::Date(x)) => d.dates[i] = shift.apply(&x),
                    _ => broken.push(i),
                });
            }
            Rep::Fix(_) | Rep::Dyn if part && rest.is_empty() => {
                self.each(act, &skip, |i| match this.given(start, i) {
                    Some(Given::Date(x)) => match Dates::part_of(&kind, &x) {
                        Some(n) => d.keep(rep, i, N::Dec(Dec::int(n))),
                        None => Bits::set(&mut d.valid, i, false),
                    },
                    _ => broken.push(i),
                });
            }
            _ => return None,
        }
        Some(broken)
    }

    pub(crate) fn blend_dates(&self, d: &mut Reg, m: &[u64], t: &Reg, e: &Reg) {
        d.stamps();
        for i in 0..self.n {
            d.dates[i] = match Bits::get(m, i) {
                true => t.date(i),
                false => e.date(i),
            };
        }
    }

    pub(crate) fn pick_dates(&self, d: &mut Reg, m: &[u64], a: &Reg) {
        for (w, &word) in m.iter().enumerate() {
            Bits::each(word & a.valid[w], w * 64, |i| d.dates[i] = a.date(i));
        }
    }

    pub(crate) fn gather_dates(&self, d: &mut Reg, b: &Buf, parent: &[u32]) {
        d.stamps();
        for (i, &p) in parent.iter().enumerate() {
            d.dates[i] = b.dates.get(p as usize).copied().unwrap_or(Date(None));
        }
    }

    pub(crate) fn fill_dates(&self, d: &mut Reg, b: &Buf) {
        d.stamps();
        for i in 0..self.n {
            d.dates[i] = b.dates.get(self.start + i).copied().unwrap_or(Date(None));
        }
    }

    pub(crate) fn spill_dates(&self, r: &Reg, b: &mut Buf) {
        b.stamps();
        for i in 0..self.n {
            if let Some(slot) = b.dates.get_mut(self.start + i) {
                *slot = r.date(i);
            }
        }
    }
}
