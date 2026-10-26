use crate::lane::date::Date;
use crate::lane::Bits;
use crate::typed::batch::{Data, Feed, Feeds, Lists, Scales};
use crate::typed::hir::{Arith, Cmp, Fold, Func, Lit, Range, Scope};
use crate::typed::ins::{FixOp, Guard, Ins, Rep, Src, Tab, R};
use crate::typed::kernel::K;
use crate::typed::lookup::{Domain, Lookup};
use crate::typed::num::{Dec, N};
use crate::typed::output::{Data as SlotData, Outputs};
use rust_decimal::Decimal;
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq)]
enum V {
    Null,
    Num(Dec),
    Big(Decimal),
    Bool(bool),
    Code(i32),
    Date(Date),
    Other,
}

#[derive(Clone)]
pub(crate) struct Reg {
    pub(crate) mant: Vec<i64>,
    pub(crate) scale: Vec<u8>,
    pub(crate) codes: Vec<i32>,
    pub(crate) bits: [u64; Reg::WORDS],
    pub(crate) valid: [u64; Reg::WORDS],
    pub(crate) fault: [u64; Reg::WORDS],
    pub(crate) wide: [u64; Reg::WORDS],
    pub(crate) big: Vec<Decimal>,
    pub(crate) masks: Vec<u64>,
    pub(crate) text: Vec<u8>,
    pub(crate) span: Vec<(u32, u32)>,
    pub(crate) dates: Vec<Date>,
}

#[derive(Clone, Default)]
pub(crate) struct Buf {
    pub(crate) mant: Vec<i64>,
    pub(crate) scale: Vec<u8>,
    pub(crate) codes: Vec<i32>,
    pub(crate) bits: Vec<u64>,
    pub(crate) valid: Vec<u64>,
    pub(crate) fault: Vec<u64>,
    pub(crate) wide: Vec<u64>,
    pub(crate) big: Vec<Decimal>,
    pub(crate) any: bool,
    pub(crate) text: Vec<u8>,
    pub(crate) span: Vec<(u32, u32)>,
    pub(crate) dates: Vec<Date>,
}

pub(crate) struct Others<'r> {
    lo: &'r [Reg],
    hi: &'r [Reg],
    at: usize,
}

pub(crate) struct Exec<'e, 'x> {
    pub(crate) feeds: &'e Feeds<'x>,
    pub(crate) select: Option<&'e [u64]>,
    pub(crate) start: usize,
    pub(crate) n: usize,
    pub(crate) words: usize,
}

pub(crate) type Stack = Vec<(usize, [u64; Reg::WORDS])>;

impl V {
    fn lit(lit: &Lit) -> V {
        match lit {
            Lit::Null => V::Null,
            Lit::Num(d) => V::Num(*d),
            Lit::Bool(b) => V::Bool(*b),
            Lit::Code(_, c) => V::Code(*c),
            Lit::Text(_) => V::Other,
            Lit::Date(d) => V::Date(d.date()),
        }
    }

    fn number(self) -> Option<N> {
        match self {
            V::Num(d) => Some(N::Dec(d)),
            V::Big(d) => Some(N::Big(d)),
            _ => None,
        }
    }

    fn same(self, other: V) -> bool {
        if let (Some(x), Some(y)) = (self.number(), other.number()) {
            return x.compare(y) == Ordering::Equal;
        }
        match (self, other) {
            (V::Other, _) | (_, V::Other) => false,
            (x, y) => x == y,
        }
    }
}

impl Range {
    #[inline]
    fn holds(&self, x: N) -> bool {
        let (lo, hi) = (x.compare(N::Dec(self.lo)), x.compare(N::Dec(self.hi)));
        let above = lo == Ordering::Greater || (self.lo_closed && lo == Ordering::Equal);
        let below = hi == Ordering::Less || (self.hi_closed && hi == Ordering::Equal);
        above && below
    }
}

impl Rep {
    #[inline]
    pub(crate) fn scale_at(self, reg: &Reg, i: usize) -> u8 {
        match self {
            Rep::Fix(s) => s,
            Rep::Dyn => reg.scale[i],
            _ => 0,
        }
    }
}

impl std::ops::Index<usize> for Others<'_> {
    type Output = Reg;

    #[inline(always)]
    fn index(&self, i: usize) -> &Reg {
        if i < self.at {
            &self.lo[i]
        } else {
            &self.hi[i - self.at - 1]
        }
    }
}

impl Buf {
    pub(crate) fn sized(&mut self, len: usize) {
        let words = K::words(len);
        self.mant.resize(len, 0);
        self.scale.resize(len, 0);
        self.codes.resize(len, 0);
        self.bits.resize(words, 0);
        self.valid.resize(words, 0);
        self.fault.resize(words, 0);
        self.wide.clear();
        self.wide.resize(words, 0);
        self.any = false;
        self.text.clear();
    }

    #[inline]
    pub(crate) fn num(&self, rep: Rep, r: usize) -> N {
        match self.any && Bits::get(&self.wide, r) {
            true => N::Big(self.big.get(r).copied().unwrap_or_default()),
            false => N::Dec(Dec::new(
                self.mant[r],
                match rep {
                    Rep::Fix(s) => s,
                    _ => self.scale[r],
                },
            )),
        }
    }

    #[inline]
    pub(crate) fn keep(&mut self, rep: Rep, r: usize, n: N) {
        match n {
            N::Dec(d) if rep == Rep::Dyn || rep == Rep::Fix(d.scale) => {
                self.mant[r] = d.mant;
                self.scale[r] = d.scale;
                if self.any {
                    Bits::set(&mut self.wide, r, false);
                }
            }
            n => {
                if self.big.len() < self.mant.len() {
                    self.big.resize(self.mant.len(), Decimal::ZERO);
                }
                self.big[r] = n.decimal();
                self.mant[r] = 0;
                self.scale[r] = 0;
                self.any = true;
                Bits::set(&mut self.wide, r, true);
            }
        }
    }

    pub(crate) fn finish(&mut self, fold: Fold, rep: Rep) {
        let len = self.mant.len();
        for r in 0..len {
            if Bits::get(&self.fault, r) {
                continue;
            }
            let seen = self.codes[r];
            match fold {
                Fold::Any if seen == 0 => Bits::set(&mut self.bits, r, false),
                Fold::All | Fold::None if seen == 0 => Bits::set(&mut self.bits, r, true),
                Fold::One => Bits::set(&mut self.bits, r, self.mant[r] == 1),
                Fold::Min | Fold::Max if seen == 0 => Bits::set(&mut self.fault, r, true),
                Fold::Avg if seen == 0 => Bits::set(&mut self.fault, r, true),
                Fold::Avg => match self
                    .num(rep, r)
                    .arith(Arith::Div, N::Dec(Dec::int(i64::from(seen))))
                {
                    Ok(Some(q)) => self.keep(rep, r, q),
                    _ => Bits::set(&mut self.fault, r, true),
                },
                _ => {}
            }
        }
    }
}

impl Reg {
    pub(crate) const LANES: usize = 1024;
    pub(crate) const WORDS: usize = 16;

    pub(crate) fn new() -> Self {
        Reg {
            mant: vec![0; Self::LANES],
            scale: vec![0; Self::LANES],
            codes: vec![0; Self::LANES],
            bits: [0; Self::WORDS],
            valid: [0; Self::WORDS],
            fault: [0; Self::WORDS],
            wide: [0; Self::WORDS],
            big: Vec::new(),
            masks: Vec::new(),
            text: Vec::new(),
            span: Vec::new(),
            dates: Vec::new(),
        }
    }

    pub(crate) fn fill(&mut self, rep: Rep, lit: &Lit) {
        self.valid.fill(u64::MAX);
        self.fault.fill(0);
        self.wide.fill(0);
        match lit {
            Lit::Num(d) => {
                self.mant.fill(d.mant);
                self.scale.fill(d.scale);
            }
            Lit::Bool(b) => self.bits.fill(if *b { u64::MAX } else { 0 }),
            Lit::Code(_, c) => self.codes.fill(*c),
            Lit::Text(t) => self.literal(t),
            Lit::Date(d) => self.dates = vec![d.date(); Self::LANES],
            Lit::Null => self.valid.fill(0),
        }
        if rep == Rep::Null {
            self.valid.fill(0);
        }
    }

    #[inline]
    fn ok(&self, i: usize) -> bool {
        Bits::get(&self.valid, i)
    }

    #[inline]
    fn bad(&self, i: usize) -> bool {
        Bits::get(&self.fault, i)
    }

    #[inline]
    fn v(&self, rep: Rep, i: usize) -> V {
        if !self.ok(i) {
            return V::Null;
        }
        if self.wide(i) {
            return V::Big(self.big.get(i).copied().unwrap_or_default());
        }
        match rep {
            Rep::Fix(s) => V::Num(Dec::new(self.mant[i], s)),
            Rep::Dyn => V::Num(Dec::new(self.mant[i], self.scale[i])),
            Rep::Bool => V::Bool(Bits::get(&self.bits, i)),
            Rep::Code => V::Code(self.codes[i]),
            Rep::Date => V::Date(self.date(i)),
            Rep::Text | Rep::Null => V::Null,
        }
    }

    #[inline]
    fn put(&mut self, i: usize, d: Dec) {
        self.mant[i] = d.mant;
        self.scale[i] = d.scale;
    }

    #[inline]
    fn wide(&self, i: usize) -> bool {
        Bits::get(&self.wide, i)
    }

    #[inline]
    pub(crate) fn num(&self, rep: Rep, i: usize) -> N {
        match self.wide(i) {
            true => N::Big(self.big.get(i).copied().unwrap_or_default()),
            false => N::Dec(Dec::new(self.mant[i], rep.scale_at(self, i))),
        }
    }

    #[inline]
    pub(crate) fn keep(&mut self, rep: Rep, i: usize, n: N) {
        match n {
            N::Dec(d) if rep == Rep::Dyn || rep == Rep::Fix(d.scale) => {
                self.put(i, d);
                Bits::set(&mut self.wide, i, false);
            }
            n => {
                if self.big.len() < Self::LANES {
                    self.big.resize(Self::LANES, Decimal::ZERO);
                }
                self.big[i] = n.decimal();
                self.put(i, Dec::ZERO);
                Bits::set(&mut self.wide, i, true);
            }
        }
    }

    #[inline]
    fn widened(&self, words: usize) -> bool {
        self.wide[..words].iter().any(|w| *w != 0)
    }

    #[inline]
    fn adopt(&mut self, from: &Reg, mask: &[u64]) {
        for (w, &m) in mask.iter().enumerate() {
            let bits = m & from.wide[w];
            self.wide[w] |= bits;
            Bits::each(bits, w * 64, |i| {
                if self.big.len() < Self::LANES {
                    self.big.resize(Self::LANES, Decimal::ZERO);
                }
                self.big[i] = from.big.get(i).copied().unwrap_or_default();
            });
        }
    }
}

impl<'e, 'x> Exec<'e, 'x> {
    #[cfg(test)]
    thread_local! {
        pub(crate) static PROFILE: std::cell::RefCell<Option<std::collections::BTreeMap<String, (u128, usize)>>> = const { std::cell::RefCell::new(None) };
    }

    #[inline]
    pub(crate) fn tail(&self, w: usize) -> u64 {
        let rem = self.n - w * 64;
        if rem >= 64 {
            u64::MAX
        } else {
            (1u64 << rem) - 1
        }
    }

    pub(crate) fn feed(&self, src: Src) -> Option<&'e Feed<'x>> {
        match src {
            Src::Col(c) => self.feeds.columns.get(c as usize),
            Src::Field(l, f) => self
                .feeds
                .fields
                .get(l as usize)
                .and_then(|l| l.get(f as usize)),
            Src::Class(k) => self.feeds.classes.get(k as usize),
        }
    }

    #[inline(always)]
    fn num(
        &self,
        op: Arith,
        d: &mut Reg,
        act: Option<&[u64; Reg::WORDS]>,
        ovf: &mut [u64; Reg::WORDS],
        x: impl Fn(usize) -> Dec,
        y: impl Fn(usize) -> Dec,
    ) {
        let words = self.words.min(Reg::WORDS);
        if act.is_none() && matches!(op, Arith::Add | Arith::Sub | Arith::Mul) {
            let (mant, scale) = (&mut d.mant, &mut d.scale);
            K::pack_idx(self.n, &mut ovf[..words], |i| {
                let (p, q) = (x(i), y(i));
                let (r, s, o) = match op {
                    Arith::Mul => K::mul(p.mant, q.mant, p.scale, q.scale),
                    _ => K::add(op == Arith::Sub, p.mant, q.mant, p.scale, q.scale),
                };
                mant[i] = r;
                scale[i] = s;
                o
            });
            return;
        }
        let skip = d.fault;
        self.each(act, &skip, |i| match x(i).arith(op, y(i)) {
            Ok(Some(q)) => d.put(i, q),
            Ok(None) => Bits::set(&mut d.valid, i, false),
            Err(_) => Bits::set(ovf, i, true),
        });
    }

    fn redo(
        &self,
        d: &mut Reg,
        act: Option<&[u64; Reg::WORDS]>,
        ovf: &[u64; Reg::WORDS],
        rep: Rep,
        mut f: impl FnMut(usize) -> Result<Option<N>, ()>,
    ) {
        let words = self.words.min(Reg::WORDS);
        let mut m = [0u64; Reg::WORDS];
        for w in 0..words {
            m[w] = ovf[w] & !d.fault[w];
        }
        if m[..words].iter().all(|w| *w == 0) {
            return;
        }
        self.over(act, &m, |i| match f(i) {
            Ok(Some(v)) => {
                Bits::set(&mut d.valid, i, true);
                d.keep(rep, i, v)
            }
            Ok(None) => Bits::set(&mut d.valid, i, false),
            Err(()) => Bits::set(&mut d.fault, i, true),
        });
    }

    #[inline(always)]
    fn over(
        &self,
        act: Option<&[u64; Reg::WORDS]>,
        mask: &[u64; Reg::WORDS],
        mut f: impl FnMut(usize),
    ) {
        for w in 0..self.words.min(Reg::WORDS) {
            Bits::each(
                act.map_or(u64::MAX, |a| a[w]) & self.tail(w) & mask[w],
                w * 64,
                &mut f,
            );
        }
    }

    fn patch(&self, d: &mut Reg, a: &Reg, b: Option<&Reg>, mut f: impl FnMut(usize) -> bool) {
        for w in 0..self.words.min(Reg::WORDS) {
            let (bw, bv) = b.map_or((0, u64::MAX), |b| (b.wide[w], b.valid[w]));
            Bits::each((a.wide[w] | bw) & a.valid[w] & bv & self.tail(w), 0, |j| {
                let bit = 1u64 << j;
                match f(w * 64 + j) {
                    true => d.bits[w] |= bit,
                    false => d.bits[w] &= !bit,
                }
            });
        }
    }

    #[inline]
    pub(crate) fn def(regs: &mut [Reg], dst: R, f: impl FnOnce(&mut Reg, &Others)) {
        let at = dst as usize;
        let (lo, rest) = regs.split_at_mut(at);
        if let Some((d, hi)) = rest.split_first_mut() {
            d.wide = [0; Reg::WORDS];
            f(d, &Others { lo, hi, at });
        }
    }

    fn strict1(&self, d: &mut Reg, a: &Reg) {
        for w in 0..self.words.min(Reg::WORDS) {
            d.fault[w] = a.fault[w] | !a.valid[w];
            d.valid[w] = u64::MAX;
        }
    }

    fn strict2(&self, d: &mut Reg, a: &Reg, b: &Reg) {
        for w in 0..self.words.min(Reg::WORDS) {
            d.fault[w] = a.fault[w] | b.fault[w] | !a.valid[w] | !b.valid[w];
            d.valid[w] = u64::MAX;
        }
    }

    fn eq_meta(&self, d: &mut Reg, a: &Reg, b: &Reg, ne: bool) {
        for w in 0..self.words.min(Reg::WORDS) {
            let (va, vb) = (a.valid[w], b.valid[w]);
            let eq = (va & vb & d.bits[w]) | (!va & !vb);
            d.bits[w] = if ne { !eq } else { eq };
            d.fault[w] = a.fault[w] | b.fault[w];
            d.valid[w] = u64::MAX;
        }
    }

    pub(crate) fn mask_of(
        &self,
        guard: &Guard,
        regs: &[Reg],
        parent: Option<&[u64; Reg::WORDS]>,
    ) -> [u64; Reg::WORDS] {
        let mut out = [0u64; Reg::WORDS];
        for (w, o) in out.iter_mut().enumerate().take(self.words.min(Reg::WORDS)) {
            let m = match guard {
                Guard::True(c) => {
                    let c = &regs[*c as usize];
                    c.bits[w] & c.valid[w] & !c.fault[w]
                }
                Guard::False(c) => {
                    let c = &regs[*c as usize];
                    !c.bits[w] & c.valid[w] & !c.fault[w]
                }
                Guard::Null(c) => {
                    let c = &regs[*c as usize];
                    !c.valid[w] & !c.fault[w]
                }
                Guard::Masks(s, idx) => {
                    let s = &regs[*s as usize];
                    idx.iter().fold(0, |acc, k| {
                        acc | s
                            .masks
                            .get(*k as usize * Reg::WORDS + w)
                            .copied()
                            .unwrap_or(0)
                    })
                }
            };
            *o = m & self.tail(w) & parent.map_or(u64::MAX, |p| p[w]);
        }
        out
    }

    #[inline(always)]
    pub(crate) fn bools(
        &self,
        act: Option<&[u64; Reg::WORDS]>,
        bits: &mut [u64; Reg::WORDS],
        mut f: impl FnMut(usize) -> bool,
    ) {
        let words = self.words.min(Reg::WORDS);
        match act {
            None => K::pack_idx(self.n, &mut bits[..words], f),
            Some(_) => {
                bits[..words].fill(0);
                let none = [0u64; Reg::WORDS];
                self.each(act, &none, |i| {
                    if f(i) {
                        Bits::set(bits, i, true);
                    }
                });
            }
        }
    }

    #[inline(always)]
    pub(crate) fn each(
        &self,
        act: Option<&[u64; Reg::WORDS]>,
        skip: &[u64; Reg::WORDS],
        mut f: impl FnMut(usize),
    ) {
        for w in 0..self.words.min(Reg::WORDS) {
            Bits::each(
                act.map_or(u64::MAX, |a| a[w]) & self.tail(w) & !skip[w],
                w * 64,
                &mut f,
            );
        }
    }

    pub(crate) fn run(
        &self,
        code: &[Ins],
        regs: &mut [Reg],
        bufs: &mut [Buf],
        scratch: &mut [i64],
        stack: &mut Stack,
        out: &mut Outputs,
    ) {
        stack.clear();
        let mut pc = 0;
        while pc < code.len() {
            while stack.last().is_some_and(|top| pc >= top.0) {
                stack.pop();
            }
            match &code[pc] {
                Ins::Skip { guard, len } => {
                    let m = self.mask_of(guard, regs, stack.last().map(|t| &t.1));
                    if m.iter().all(|w| *w == 0) {
                        pc += *len as usize;
                    } else {
                        stack.push((pc + 1 + *len as usize, m));
                    }
                }
                ins => {
                    #[cfg(test)]
                    let t = std::time::Instant::now();
                    self.step(ins, regs, bufs, scratch, stack.last().map(|t| &t.1), out);
                    #[cfg(test)]
                    let took = t.elapsed().as_nanos();
                    #[cfg(test)]
                    Self::PROFILE.with_borrow_mut(|p| {
                        if let Some(p) = p {
                            let name = format!("{ins:?}")
                                .split([' ', '{'])
                                .next()
                                .unwrap_or("")
                                .to_string();
                            let e = p.entry(name).or_insert((0, 0));
                            e.0 += took;
                            e.1 += 1;
                        }
                    });
                }
            }
            pc += 1;
        }
    }

    #[allow(clippy::needless_range_loop)]
    pub(crate) fn step(
        &self,
        ins: &Ins,
        regs: &mut [Reg],
        bufs: &mut [Buf],
        scratch: &mut [i64],
        act: Option<&[u64; Reg::WORDS]>,
        out: &mut Outputs,
    ) {
        let (n, words, start) = (self.n, self.words.min(Reg::WORDS), self.start);
        match ins {
            Ins::Skip { .. } => {}
            Ins::Load { dst, src, rep } => {
                let Some(feed) = self.feed(*src) else {
                    return;
                };
                Self::def(regs, *dst, |d, _| self.load(d, feed, *rep));
            }
            Ins::Gather {
                dst,
                buf,
                list,
                rep,
            } => {
                let Some(l) = self.feeds.lists.get(*list as usize) else {
                    return;
                };
                let parent = &l.parent[start..start + n];
                let b = &bufs[*buf as usize];
                Self::def(regs, *dst, |d, _| {
                    match rep {
                        Rep::Fix(_) => {
                            for (o, &p) in d.mant[..n].iter_mut().zip(parent) {
                                *o = b.mant[p as usize];
                            }
                        }
                        Rep::Dyn => {
                            for (i, &p) in parent.iter().enumerate() {
                                d.mant[i] = b.mant[p as usize];
                                d.scale[i] = b.scale[p as usize];
                            }
                        }
                        Rep::Code => {
                            for (o, &p) in d.codes[..n].iter_mut().zip(parent) {
                                *o = b.codes[p as usize];
                            }
                        }
                        Rep::Bool => K::pack1(parent, &mut d.bits[..words], |p| {
                            Bits::get(&b.bits, p as usize)
                        }),
                        Rep::Text => self.gather_text(d, b, parent),
                        Rep::Date => self.gather_dates(d, b, parent),
                        Rep::Null => {}
                    }
                    let (r0, r1) = (
                        parent.first().map_or(0, |p| *p as usize),
                        parent.last().map_or(0, |p| *p as usize + 1),
                    );
                    if Bits::range(&b.valid, r0, r1, true) && Bits::range(&b.fault, r0, r1, false) {
                        d.valid[..words].fill(u64::MAX);
                        d.fault[..words].fill(0);
                    } else {
                        K::pack1(parent, &mut d.valid[..words], |p| {
                            Bits::get(&b.valid, p as usize)
                        });
                        K::pack1(parent, &mut d.fault[..words], |p| {
                            Bits::get(&b.fault, p as usize)
                        });
                    }
                    if b.any && !Bits::range(&b.wide, r0, r1, false) {
                        for (i, &p) in parent.iter().enumerate() {
                            if Bits::get(&b.wide, p as usize) {
                                d.keep(
                                    Rep::Dyn,
                                    i,
                                    N::Big(b.big.get(p as usize).copied().unwrap_or_default()),
                                );
                            }
                        }
                    }
                });
            }
            Ins::Fill { dst, buf, rep } => {
                let b = &bufs[*buf as usize];
                let w0 = start / 64;
                Self::def(regs, *dst, |d, _| {
                    d.valid[..words].copy_from_slice(&b.valid[w0..w0 + words]);
                    d.fault[..words].copy_from_slice(&b.fault[w0..w0 + words]);
                    if b.any && !Bits::range(&b.wide, start, start + n, false) {
                        for i in 0..n {
                            if Bits::get(&b.wide, start + i) {
                                d.keep(
                                    Rep::Dyn,
                                    i,
                                    N::Big(b.big.get(start + i).copied().unwrap_or_default()),
                                );
                            }
                        }
                    }
                    match rep {
                        Rep::Fix(_) => d.mant[..n].copy_from_slice(&b.mant[start..start + n]),
                        Rep::Dyn => {
                            d.mant[..n].copy_from_slice(&b.mant[start..start + n]);
                            d.scale[..n].copy_from_slice(&b.scale[start..start + n]);
                        }
                        Rep::Code => d.codes[..n].copy_from_slice(&b.codes[start..start + n]),
                        Rep::Bool => d.bits[..words].copy_from_slice(&b.bits[w0..w0 + words]),
                        Rep::Text => self.fill_text(d, b),
                        Rep::Date => self.fill_dates(d, b),
                        Rep::Null => {}
                    }
                });
            }
            Ins::Spill { src, buf, rep } => {
                let r = &regs[*src as usize];
                let b = &mut bufs[*buf as usize];
                let w0 = start / 64;
                b.valid[w0..w0 + words].copy_from_slice(&r.valid[..words]);
                b.fault[w0..w0 + words].copy_from_slice(&r.fault[..words]);
                b.wide[w0..w0 + words].copy_from_slice(&r.wide[..words]);
                if r.widened(words) {
                    if b.big.len() < b.mant.len() {
                        b.big.resize(b.mant.len(), Decimal::ZERO);
                    }
                    b.any = true;
                    for i in 0..n {
                        if r.wide(i) {
                            b.big[start + i] = r.big.get(i).copied().unwrap_or_default();
                        }
                    }
                }
                match rep {
                    Rep::Fix(_) => b.mant[start..start + n].copy_from_slice(&r.mant[..n]),
                    Rep::Dyn => {
                        b.mant[start..start + n].copy_from_slice(&r.mant[..n]);
                        b.scale[start..start + n].copy_from_slice(&r.scale[..n]);
                    }
                    Rep::Code => b.codes[start..start + n].copy_from_slice(&r.codes[..n]),
                    Rep::Bool => b.bits[w0..w0 + words].copy_from_slice(&r.bits[..words]),
                    Rep::Text => self.spill_text(r, b),
                    Rep::Date => self.spill_dates(r, b),
                    Rep::Null => {}
                }
            }
            Ins::Fix { op, dst, a, b, rep } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                self.strict2(d, a, b);
                let mut ovf = [0u64; Reg::WORDS];
                let (x, y, o, f) = (
                    &a.mant[..n],
                    &b.mant[..n],
                    &mut d.mant[..n],
                    &mut ovf[..words],
                );
                match op {
                    FixOp::Add => K::ovf2(x, y, o, f, i64::overflowing_add),
                    FixOp::Sub => K::ovf2(x, y, o, f, i64::overflowing_sub),
                    FixOp::Mul => K::ovf2(x, y, o, f, i64::overflowing_mul),
                }
                for w in 0..words {
                    ovf[w] |= a.wide[w] | b.wide[w];
                }
                let op = match op {
                    FixOp::Add => Arith::Add,
                    FixOp::Sub => Arith::Sub,
                    FixOp::Mul => Arith::Mul,
                };
                let rep = *rep;
                self.redo(d, act, &ovf, rep, |i| {
                    a.num(rep, i).arith(op, b.num(rep, i)).map_err(|_| ())
                });
            }),
            Ins::Neg { dst, a, rep } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                let mut ovf = [0u64; Reg::WORDS];
                K::ovf1(&a.mant[..n], &mut d.mant[..n], &mut ovf[..words], |x| {
                    (x.wrapping_neg(), (x == 0) | (x == i64::MIN))
                });
                if *rep == Rep::Dyn {
                    d.scale[..n].copy_from_slice(&a.scale[..n]);
                }
                for w in 0..words {
                    ovf[w] |= a.wide[w];
                }
                let rep = *rep;
                self.redo(d, act, &ovf, rep, |i| Ok(Some(a.num(rep, i).neg())));
            }),
            Ins::Num {
                op,
                dst,
                a,
                ra,
                b,
                rb,
            } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                self.strict2(d, a, b);
                let (am, bm) = (&a.mant[..n], &b.mant[..n]);
                let mut ovf = [0u64; Reg::WORDS];
                let (op, ra, rb) = (*op, *ra, *rb);
                if act.is_none() && matches!(op, Arith::Add | Arith::Sub | Arith::Mul) {
                    let (mut pa, mut pb) = ([0u8; Reg::LANES], [0u8; Reg::LANES]);
                    let (asc, bsc) = (
                        Self::scales(ra, a, &mut pa, n),
                        Self::scales(rb, b, &mut pb, n),
                    );
                    let (sub, mul) = (op == Arith::Sub, op == Arith::Mul);
                    let (dm, ds, df) = (&mut d.mant[..n], &mut d.scale[..n], &mut ovf[..words]);
                    match mul {
                        true => K::dyn2(am, bm, asc, bsc, dm, ds, df, K::mul),
                        false => K::dyn2(am, bm, asc, bsc, dm, ds, df, |x, y, p, q| {
                            K::add(sub, x, y, p, q)
                        }),
                    }
                } else {
                    self.num(
                        op,
                        d,
                        act,
                        &mut ovf,
                        |i| Dec::new(am[i], ra.scale_at(a, i)),
                        |i| Dec::new(bm[i], rb.scale_at(b, i)),
                    );
                }
                for w in 0..words {
                    ovf[w] |= a.wide[w] | b.wide[w];
                }
                self.redo(d, act, &ovf, Rep::Dyn, |i| {
                    a.num(ra, i).arith(op, b.num(rb, i)).map_err(|_| ())
                });
            }),
            Ins::DivPow { dst, a, ra, k, by } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                let (k, by, ra) = (*k, *by, *ra);
                let mut ovf = [0u64; Reg::WORDS];
                let mut slow_lane =
                    |i: usize, d: &mut Reg| match Dec::new(a.mant[i], ra.scale_at(a, i))
                        .div_power(k, by)
                    {
                        Ok(q) => d.put(i, q),
                        Err(_) => Bits::set(&mut ovf, i, true),
                    };
                let mut slow = [0u64; Reg::WORDS];
                let shift = match ra {
                    Rep::Fix(sa) => sa as i32 - by as i32,
                    _ => -1,
                };
                let fast = act.is_none() && k >= 2 && (0..=19).contains(&shift);
                let mut even = [0u64; Reg::WORDS];
                if fast {
                    K::pack1(&a.mant[..n], &mut even[..words], |m| {
                        (m % 10 != 0) & (m != i64::MIN)
                    });
                    d.mant[..n].copy_from_slice(&a.mant[..n]);
                    d.scale[..n].fill((shift + k as i32) as u8);
                    for (w, word) in even.iter_mut().enumerate().take(words) {
                        *word |= d.fault[w];
                    }
                }
                let (mant, scale) = (&mut d.mant, &mut d.scale);
                let mut quick = |i: usize| {
                    let (m, t, bad) =
                        K::div_pow(a.mant[i], ra.scale_at(a, i) as i32 - by as i32, k);
                    mant[i] = m;
                    scale[i] = t;
                    bad
                };
                match (fast, act) {
                    (true, _) => self.each(None, &even, |i| Bits::set(&mut slow, i, quick(i))),
                    (false, None) => K::pack_idx(n, &mut slow[..words], quick),
                    (false, Some(_)) => {
                        let skip = d.fault;
                        self.each(act, &skip, |i| Bits::set(&mut slow, i, quick(i)));
                    }
                }
                for (w, word) in slow.iter_mut().enumerate().take(words) {
                    *word = !(*word & !d.fault[w]);
                }
                self.each(None, &slow, |i| slow_lane(i, d));
                for w in 0..words {
                    ovf[w] |= a.wide[w];
                }
                let divisor = N::Dec(Dec::new(Dec::POW10[k as usize], by));
                self.redo(d, act, &ovf, Rep::Dyn, |i| {
                    a.num(ra, i).arith(Arith::Div, divisor).map_err(|_| ())
                });
            }),
            Ins::PickTab { dst, sel, tab, rep } => Self::def(regs, *dst, |d, regs| {
                let s = &regs[*sel as usize];
                let sm = &s.mant[..n];
                match rep {
                    Rep::Fix(_) => {
                        for (o, &x) in d.mant[..n].iter_mut().zip(sm) {
                            *o = tab.mant[(x as usize) & 255];
                        }
                    }
                    Rep::Dyn => {
                        for ((o, c), &x) in
                            d.mant[..n].iter_mut().zip(d.scale[..n].iter_mut()).zip(sm)
                        {
                            *o = tab.mant[(x as usize) & 255];
                            *c = tab.scale[(x as usize) & 255];
                        }
                    }
                    Rep::Code => {
                        for (o, &x) in d.codes[..n].iter_mut().zip(sm) {
                            *o = tab.codes[(x as usize) & 255];
                        }
                    }
                    Rep::Bool => K::pack1(sm, &mut d.bits[..words], |x| {
                        Tab::bit(&tab.flag, x as usize)
                    }),
                    Rep::Text | Rep::Date | Rep::Null => {}
                }
                K::pack1(sm, &mut d.valid[..words], |x| Tab::bit(&tab.ok, x as usize));
                d.fault[..words].copy_from_slice(&s.fault[..words]);
            }),
            Ins::CmpFix { op, dst, a, b, rep } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                let (x, y, o) = (&a.mant[..n], &b.mant[..n], &mut d.bits[..words]);
                match op {
                    Cmp::Eq | Cmp::Ne => K::pack2(x, y, o, |x, y| x == y),
                    Cmp::Lt => K::pack2(x, y, o, |x, y| x < y),
                    Cmp::Le => K::pack2(x, y, o, |x, y| x <= y),
                    Cmp::Gt => K::pack2(x, y, o, |x, y| x > y),
                    Cmp::Ge => K::pack2(x, y, o, |x, y| x >= y),
                }
                let (op, rep) = (*op, *rep);
                self.patch(d, a, Some(b), |i| {
                    K::ord(op, a.num(rep, i).compare(b.num(rep, i)))
                });
                match op {
                    Cmp::Eq | Cmp::Ne => self.eq_meta(d, a, b, op == Cmp::Ne),
                    _ => self.strict2(d, a, b),
                }
            }),
            Ins::CmpNum {
                op,
                dst,
                a,
                ra,
                b,
                rb,
            } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                let (am, bm, asc, bsc) = (&a.mant[..n], &b.mant[..n], &a.scale[..n], &b.scale[..n]);
                let (op, ra, rb) = (*op, *ra, *rb);
                let f = |i: usize| {
                    let x = match ra {
                        Rep::Fix(p) => Dec::new(am[i], p),
                        _ => Dec::new(am[i], asc[i]),
                    };
                    let y = match rb {
                        Rep::Fix(q) => Dec::new(bm[i], q),
                        _ => Dec::new(bm[i], bsc[i]),
                    };
                    K::ord(op, x.compare(y))
                };
                self.bools(act, &mut d.bits, f);
                self.patch(d, a, Some(b), |i| {
                    K::ord(op, a.num(ra, i).compare(b.num(rb, i)))
                });
                match op {
                    Cmp::Eq | Cmp::Ne => self.eq_meta(d, a, b, op == Cmp::Ne),
                    _ => self.strict2(d, a, b),
                }
            }),
            Ins::CmpNumK {
                op,
                dst,
                a,
                ra,
                k,
                tab,
                fits,
            } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                let (am, asc) = (&a.mant[..n], &a.scale[..n]);
                let (op, fits, k) = (*op, *fits, *k);
                let o = &mut d.bits[..words];
                let slow = |m: i64, s: u8| match tab.get(s as usize) {
                    Some(c) if (fits >> s) & 1 != 0 => K::ord(op, m.cmp(c)),
                    _ => K::ord(op, Dec::new(m, s).compare(k)),
                };
                match *ra {
                    Rep::Fix(s) if (fits >> s) & 1 != 0 => K::cmp_k(op, am, tab[s as usize], o),
                    Rep::Fix(s) => K::pack1(am, o, |m| slow(m, s)),
                    _ => K::pack2(am, asc, o, slow),
                }
                let ra = *ra;
                self.patch(d, a, None, |i| K::ord(op, a.num(ra, i).compare(N::Dec(k))));
                for w in 0..words {
                    match op {
                        Cmp::Eq | Cmp::Ne => {
                            let eq = a.valid[w] & d.bits[w];
                            d.bits[w] = if op == Cmp::Ne { !eq } else { eq };
                            d.fault[w] = a.fault[w];
                        }
                        _ => d.fault[w] = a.fault[w] | !a.valid[w],
                    }
                    d.valid[w] = u64::MAX;
                }
            }),
            Ins::CmpGen {
                op,
                dst,
                a,
                ra,
                b,
                rb,
            } if matches!((ra, rb), (Rep::Null, _) | (_, Rep::Null)) => {
                Self::def(regs, *dst, |d, regs| {
                    let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                    for w in 0..words {
                        let va = if *ra == Rep::Null { 0 } else { a.valid[w] };
                        let vb = if *rb == Rep::Null { 0 } else { b.valid[w] };
                        let (both, none) = (va & vb, !(va | vb));
                        d.valid[w] = u64::MAX;
                        d.fault[w] = a.fault[w] | b.fault[w];
                        d.bits[w] = match op {
                            Cmp::Eq => none,
                            Cmp::Ne => !none,
                            _ => 0,
                        };
                        if !matches!(op, Cmp::Eq | Cmp::Ne) {
                            d.fault[w] |= !both & self.tail(w);
                        }
                    }
                })
            }
            Ins::CmpGen {
                op,
                dst,
                a,
                ra,
                b,
                rb,
            } if matches!((ra, rb), (Rep::Fix(_) | Rep::Dyn, Rep::Fix(_) | Rep::Dyn)) => {
                Self::def(regs, *dst, |d, regs| {
                    let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                    let equality = matches!(op, Cmp::Eq | Cmp::Ne);
                    for w in 0..words {
                        let tail = self.tail(w);
                        let mut hit = 0u64;
                        for j in 0..(n - w * 64).min(64) {
                            let i = w * 64 + j;
                            let order = a.num(*ra, i).compare(b.num(*rb, i));
                            hit |= u64::from(K::ord(*op, order)) << j;
                        }
                        let both = a.valid[w] & b.valid[w];
                        let none = !(a.valid[w] | b.valid[w]);
                        d.bits[w] = match op {
                            Cmp::Eq => (hit & both) | none,
                            Cmp::Ne => !(hit & both) & !none,
                            _ => hit & both,
                        };
                        d.valid[w] = u64::MAX;
                        d.fault[w] =
                            a.fault[w] | b.fault[w] | if equality { 0 } else { !both & tail };
                    }
                })
            }
            Ins::CmpGen {
                op,
                dst,
                a,
                ra,
                b,
                rb,
            } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                for w in 0..words {
                    d.fault[w] = a.fault[w] | b.fault[w];
                    d.valid[w] = u64::MAX;
                    d.bits[w] = 0;
                }
                for i in 0..n {
                    let (x, y) = (a.v(*ra, i), b.v(*rb, i));
                    let r = match (op, x, y) {
                        (Cmp::Eq, x, y) => Some(x.same(y)),
                        (Cmp::Ne, x, y) => Some(!x.same(y)),
                        (_, V::Date(x), V::Date(y)) => Some(K::ord(*op, x.cmp(&y))),
                        (_, x, y) => x
                            .number()
                            .zip(y.number())
                            .map(|(x, y)| K::ord(*op, x.compare(y))),
                    };
                    match r {
                        Some(v) => Bits::set(&mut d.bits, i, v),
                        None => Bits::set(&mut d.fault, i, true),
                    }
                }
            }),
            Ins::EqCode { ne, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                K::pack2(
                    &a.codes[..n],
                    &b.codes[..n],
                    &mut d.bits[..words],
                    |x, y| x == y,
                );
                self.eq_meta(d, a, b, *ne);
            }),
            Ins::EqBool { ne, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                for w in 0..words {
                    d.bits[w] = !(a.bits[w] ^ b.bits[w]);
                }
                self.eq_meta(d, a, b, *ne);
            }),
            Ins::EqText { .. }
            | Ins::Text { .. }
            | Ins::Matches { .. }
            | Ins::Map { .. }
            | Ins::Concat { .. }
            | Ins::Str { .. }
            | Ins::Decode { .. }
            | Ins::Class { .. } => self.text(ins, regs, act),
            Ins::Now { .. } | Ins::Host { .. } | Ins::Clock { .. } => self.date(ins, regs, act),
            Ins::InCode { dst, a, set, null } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                let set = *set;
                K::pack1(&a.codes[..n], &mut d.bits[..words], |c| {
                    ((set >> (c & 63)) & 1 != 0) & ((c as u32) < 64)
                });
                let nm = if *null { u64::MAX } else { 0 };
                for w in 0..words {
                    d.bits[w] = (a.valid[w] & d.bits[w]) | (!a.valid[w] & nm);
                    d.fault[w] = a.fault[w];
                    d.valid[w] = u64::MAX;
                }
            }),
            Ins::InGen { dst, a, ra, set } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                for w in 0..words {
                    d.fault[w] = a.fault[w];
                    d.valid[w] = u64::MAX;
                    d.bits[w] = 0;
                }
                for i in 0..n {
                    let x = a.v(*ra, i);
                    Bits::set(&mut d.bits, i, set.iter().any(|l| x.same(V::lit(l))));
                }
            }),
            Ins::InRangeFix {
                dst,
                a,
                lo,
                span,
                empty,
                range,
            } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                let (lo, span) = (*lo, *span);
                match empty {
                    true => d.bits[..words].fill(0),
                    false => K::pack1(&a.mant[..n], &mut d.bits[..words], |x| {
                        (x.wrapping_sub(lo) as u64) <= span
                    }),
                }
                self.patch(d, a, None, |i| range.holds(a.num(Rep::Dyn, i)));
                self.strict1(d, a);
            }),
            Ins::InRangeGen { dst, a, ra, range } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                for w in 0..words {
                    let mut hit = 0u64;
                    for j in 0..(n - w * 64).min(64) {
                        let i = w * 64 + j;
                        hit |= u64::from(range.holds(a.num(*ra, i))) << j;
                    }
                    d.bits[w] = hit & a.valid[w];
                }
            }),
            Ins::Not { dst, a } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                for w in 0..words {
                    d.bits[w] = !a.bits[w];
                }
            }),
            Ins::And { dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                for w in 0..words {
                    let at = a.bits[w] & a.valid[w];
                    let af = !a.bits[w] & a.valid[w];
                    d.bits[w] = at & b.bits[w];
                    d.valid[w] = af | (at & b.valid[w]);
                    d.fault[w] = a.fault[w] | !a.valid[w] | (at & b.fault[w]);
                }
            }),
            Ins::Or { dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                for w in 0..words {
                    let at = a.bits[w] & a.valid[w];
                    let af = !a.bits[w] & a.valid[w];
                    d.bits[w] = at | (af & b.bits[w]);
                    d.valid[w] = at | (af & b.valid[w]);
                    d.fault[w] = a.fault[w] | !a.valid[w] | (af & b.fault[w]);
                }
            }),
            Ins::Blend {
                dst,
                c,
                t,
                rt,
                e,
                re,
                rep,
            } => Self::def(regs, *dst, |d, regs| {
                let (c, t, e) = (&regs[*c as usize], &regs[*t as usize], &regs[*e as usize]);
                let mut ct = [0u64; Reg::WORDS];
                for w in 0..words {
                    ct[w] = c.bits[w] & c.valid[w];
                    let cf = !c.bits[w] & c.valid[w];
                    d.valid[w] = (ct[w] & t.valid[w]) | (cf & e.valid[w]);
                    d.fault[w] =
                        c.fault[w] | !c.valid[w] | (ct[w] & t.fault[w]) | (cf & e.fault[w]);
                }
                self.values(d, &ct[..words], (t, *rt), (e, *re), *rep);
                if rep.num() && (t.widened(words) || e.widened(words)) {
                    let mut cf = [0u64; Reg::WORDS];
                    for w in 0..words {
                        cf[w] = !c.bits[w] & c.valid[w];
                    }
                    d.adopt(t, &ct[..words]);
                    d.adopt(e, &cf[..words]);
                }
            }),
            Ins::SelK { dst, c, t, e, rep } => Self::def(regs, *dst, |d, regs| {
                let c = &regs[*c as usize];
                let mut ct = [0u64; Reg::WORDS];
                for w in 0..words {
                    ct[w] = c.bits[w] & c.valid[w];
                    d.valid[w] = c.valid[w];
                    d.fault[w] = c.fault[w] | !c.valid[w];
                }
                let (t, e) = (*t, *e);
                match rep {
                    Rep::Fix(_) => K::select(&ct[..words], t, e, &mut d.mant[..n]),
                    Rep::Code => K::select(&ct[..words], t as i32, e as i32, &mut d.codes[..n]),
                    Rep::Bool => {
                        let (tb, eb) = (
                            if t != 0 { u64::MAX } else { 0 },
                            if e != 0 { u64::MAX } else { 0 },
                        );
                        for w in 0..words {
                            d.bits[w] = (ct[w] & tb) | (!ct[w] & eb);
                        }
                    }
                    _ => {}
                }
            }),
            Ins::Coalesce {
                dst,
                a,
                ra,
                b,
                rb,
                rep,
            } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                let mut m = [0u64; Reg::WORDS];
                for w in 0..words {
                    m[w] = a.valid[w];
                    d.valid[w] = a.valid[w] | b.valid[w];
                    d.fault[w] = a.fault[w] | (!a.valid[w] & b.fault[w]);
                }
                self.values(d, &m[..words], (a, *ra), (b, *rb), *rep);
                if rep.num() && (a.widened(words) || b.widened(words)) {
                    let mut e = [0u64; Reg::WORDS];
                    for w in 0..words {
                        e[w] = !a.valid[w];
                    }
                    d.adopt(a, &m[..words]);
                    d.adopt(b, &e[..words]);
                }
            }),
            Ins::Lookup { dst, keys, tab } => Self::def(regs, *dst, |d, regs| {
                let (rules, rw) = (tab.rules, tab.words);
                if d.masks.len() < (rules + 1) * Reg::WORDS {
                    d.masks.resize((rules + 1) * Reg::WORDS, 0);
                }
                for ci in 0..=rules {
                    d.masks[ci * Reg::WORDS..ci * Reg::WORDS + words].fill(0);
                }
                d.valid[..words].fill(u64::MAX);
                d.fault[..words].fill(0);
                for (r, _) in keys.iter().flatten() {
                    let x = &regs[*r as usize];
                    for w in 0..words {
                        d.fault[w] |= x.fault[w];
                    }
                }
                let mut full = [0u64; Lookup::WORDS];
                for (w, f) in full.iter_mut().enumerate().take(rw) {
                    *f = match rules - w * 64 {
                        left if left >= 64 => u64::MAX,
                        left => (1u64 << left) - 1,
                    };
                }
                if rw == 1 {
                    let (mut acc, mut bad, mut at) = (
                        [full[0]; Reg::LANES],
                        [0u64; Reg::LANES],
                        [0usize; Reg::LANES],
                    );
                    for (key, reg) in tab.keys.iter().zip(keys) {
                        Self::indexes(
                            key,
                            reg.map(|(r, rep)| (&regs[r as usize], rep)),
                            &mut at[..n],
                        );
                        if key.bad.iter().any(|b| *b != 0) {
                            for ((a, b), &v) in
                                acc[..n].iter_mut().zip(bad[..n].iter_mut()).zip(&at[..n])
                            {
                                *b |= *a & key.bad[v];
                            }
                        }
                        let hit = &key.hit[..];
                        for (a, &v) in acc[..n].iter_mut().zip(&at[..n]) {
                            *a &= hit.get(v).copied().unwrap_or(0);
                        }
                    }
                    for i in 0..n {
                        let m = acc[i] | bad[i];
                        let sel = match m {
                            0 => rules,
                            m => m.trailing_zeros() as usize,
                        };
                        if (bad[i] & m & m.wrapping_neg()) != 0 {
                            Bits::set(&mut d.fault, i, true);
                        }
                        d.mant[i] = sel as i64;
                        d.masks[sel * Reg::WORDS + i / 64] |= 1u64 << (i % 64);
                    }
                    return;
                }
                let mut at = [0usize; Reg::LANES];
                let mut acc = vec![[0u64; Lookup::WORDS]; n];
                let mut bad = vec![[0u64; Lookup::WORDS]; n];
                acc.iter_mut().for_each(|a| *a = full);
                for (key, reg) in tab.keys.iter().zip(keys) {
                    Self::indexes(
                        key,
                        reg.map(|(r, rep)| (&regs[r as usize], rep)),
                        &mut at[..n],
                    );
                    for i in 0..n {
                        let v = at[i];
                        let (h, b) = (
                            &key.hit[v * rw..(v + 1) * rw],
                            &key.bad[v * rw..(v + 1) * rw],
                        );
                        for w in 0..rw {
                            bad[i][w] |= acc[i][w] & b[w];
                            acc[i][w] &= h[w];
                        }
                    }
                }
                for i in 0..n {
                    let first = (0..rw).find_map(|w| {
                        let m = acc[i][w] | bad[i][w];
                        (m != 0).then(|| {
                            (
                                w * 64 + m.trailing_zeros() as usize,
                                (bad[i][w] >> m.trailing_zeros()) & 1 == 1,
                            )
                        })
                    });
                    let sel = match first {
                        Some((rule, true)) => {
                            Bits::set(&mut d.fault, i, true);
                            rule
                        }
                        Some((rule, false)) => rule,
                        None => rules,
                    };
                    d.mant[i] = sel as i64;
                    d.masks[sel * Reg::WORDS + i / 64] |= 1u64 << (i % 64);
                }
            }),
            Ins::First {
                dst,
                conds,
                sel,
                strict,
            } => Self::def(regs, *dst, |d, regs| {
                let k = conds.len();
                if d.masks.len() < (k + 1) * Reg::WORDS {
                    d.masks.resize((k + 1) * Reg::WORDS, 0);
                }
                let mut rem = [0u64; Reg::WORDS];
                let mut bad = [0u64; Reg::WORDS];
                for (w, r) in rem.iter_mut().enumerate().take(words) {
                    *r = self.tail(w);
                }
                for (ci, c) in conds.iter().enumerate() {
                    let c = &regs[*c as usize];
                    let m = &mut d.masks[ci * Reg::WORDS..ci * Reg::WORDS + words];
                    for w in 0..words {
                        let hit = c.bits[w] & c.valid[w] & !c.fault[w] & rem[w];
                        m[w] = hit;
                        let fail = match strict {
                            true => (!c.valid[w] | c.fault[w]) & rem[w],
                            false => 0,
                        };
                        bad[w] |= fail;
                        rem[w] &= !(hit | fail);
                    }
                }
                d.masks[k * Reg::WORDS..k * Reg::WORDS + words].copy_from_slice(&rem[..words]);
                if *sel {
                    for ci in 0..=k {
                        let (masks, mant) = (
                            &d.masks[ci * Reg::WORDS..ci * Reg::WORDS + words],
                            &mut d.mant[..n],
                        );
                        K::fill_masked(masks, ci as i64, mant);
                    }
                }
                d.valid[..words].fill(u64::MAX);
                d.fault[..words].copy_from_slice(&bad[..words]);
            }),
            Ins::Pick {
                dst,
                sel,
                arms,
                rep,
            } => Self::def(regs, *dst, |d, regs| {
                let s = &regs[*sel as usize];
                for w in 0..words {
                    d.valid[w] = 0;
                    d.fault[w] = s.fault[w];
                }
                match rep {
                    Rep::Text => d.texts(n),
                    Rep::Date => d.stamps(),
                    _ => {}
                }
                let mut m = [0u64; Reg::WORDS];
                for (arm, ra, idx) in arms {
                    let a = &regs[*arm as usize];
                    for (w, mw) in m.iter_mut().enumerate().take(words) {
                        *mw = idx.iter().fold(0, |acc, k| {
                            acc | s
                                .masks
                                .get(*k as usize * Reg::WORDS + w)
                                .copied()
                                .unwrap_or(0)
                        });
                        d.valid[w] |= *mw & a.valid[w];
                        d.fault[w] |= *mw & a.fault[w];
                    }
                    let m = &m[..words];
                    if rep.num() && a.widened(words) {
                        d.adopt(a, m);
                    }
                    match rep {
                        Rep::Fix(_) => K::copy_masked(m, &a.mant[..n], &mut d.mant[..n]),
                        Rep::Dyn => {
                            K::copy_masked(m, &a.mant[..n], &mut d.mant[..n]);
                            match ra {
                                Rep::Dyn => K::copy_masked(m, &a.scale[..n], &mut d.scale[..n]),
                                other => K::fill_masked(m, other.scale_at(a, 0), &mut d.scale[..n]),
                            }
                        }
                        Rep::Code => K::copy_masked(m, &a.codes[..n], &mut d.codes[..n]),
                        Rep::Bool => {
                            for (w, mw) in m.iter().enumerate() {
                                d.bits[w] = (d.bits[w] & !mw) | (a.bits[w] & mw);
                            }
                        }
                        Rep::Text => self.pick_text(d, m, a),
                        Rep::Date => self.pick_dates(d, m, a),
                        Rep::Null => {}
                    }
                }
            }),
            Ins::Fold {
                fold,
                buf,
                list,
                filter,
                value,
            } => self.fold(
                *fold,
                &mut bufs[*buf as usize],
                *list,
                (*filter, *value),
                regs,
                scratch,
            ),
            Ins::Math {
                func,
                dst,
                args,
                rep,
            } => Self::def(regs, *dst, |d, regs| {
                let (func, rep) = (*func, *rep);
                d.valid[..words].fill(u64::MAX);
                d.fault[..words].fill(0);
                for (a, _) in args {
                    let a = &regs[*a as usize];
                    for w in 0..words {
                        d.fault[w] |= a.fault[w] | !a.valid[w];
                    }
                }
                let skip = d.fault;
                match (func, args.as_slice()) {
                    (Func::Min | Func::Max, _) if rep == Rep::Date => {
                        d.stamps();
                        self.each(act, &skip, |i| {
                            let mut best: Option<Date> = None;
                            for (a, _) in args {
                                let x = regs[*a as usize].date(i);
                                let take = best.is_none_or(|b| match func {
                                    Func::Min => x < b,
                                    _ => x >= b,
                                });
                                if take {
                                    best = Some(x);
                                }
                            }
                            if let Some(b) = best {
                                d.dates[i] = b;
                            }
                        });
                    }
                    (Func::Min | Func::Max, _) => self.each(act, &skip, |i| {
                        let mut best: Option<N> = None;
                        for (a, ra) in args {
                            let x = regs[*a as usize].num(*ra, i);
                            best = Some(match best {
                                None => x,
                                Some(b) => {
                                    let order = x.compare(b);
                                    let take = match func {
                                        Func::Min => order == Ordering::Less,
                                        _ => order != Ordering::Less,
                                    };
                                    if take {
                                        x
                                    } else {
                                        b
                                    }
                                }
                            });
                        }
                        if let Some(b) = best {
                            d.keep(rep, i, b);
                        }
                    }),
                    (_, [(a, ra)]) => {
                        let (a, ra) = (&regs[*a as usize], *ra);
                        self.each(act, &skip, |i| {
                            let fast = match a.wide(i) {
                                false => Dec::new(a.mant[i], ra.scale_at(a, i))
                                    .func(func)
                                    .filter(|r| rep == Rep::Dyn || rep == Rep::Fix(r.scale)),
                                true => None,
                            };
                            match fast {
                                Some(r) => d.put(i, r),
                                None => d.keep(rep, i, a.num(ra, i).func(func)),
                            }
                        });
                    }
                    _ => {}
                }
            }),
            Ins::Out {
                slot,
                src,
                rep,
                scope,
            } => self.out(*slot, &regs[*src as usize], *rep, *scope, out),
            Ins::Keep { slot, src } => {
                let r = &regs[*src as usize];
                let w0 = start / 64;
                if let Some(s) = out.slot_mut(*slot as usize) {
                    for w in 0..words {
                        if let Some(k) = s.keep.get_mut(w0 + w) {
                            *k = r.bits[w] & r.valid[w] & !r.fault[w] & self.tail(w);
                        }
                    }
                }
            }
        }
    }

    fn indexes(key: &crate::typed::lookup::Key, reg: Option<(&Reg, Rep)>, at: &mut [usize]) {
        let Some((x, rep)) = reg else {
            at.fill(0);
            return;
        };
        let null = key.null();
        match &key.domain {
            Domain::Codes(_) => at
                .iter_mut()
                .zip(&x.codes)
                .for_each(|(a, c)| *a = key.code(*c)),
            Domain::Bools => at
                .iter_mut()
                .enumerate()
                .for_each(|(i, a)| *a = usize::from(Bits::get(&x.bits, i))),
            Domain::Unit => at.fill(0),
            Domain::Points(_) => match &key.fixed {
                Some((_, mants)) if mants.len() <= 32 => {
                    let n = at.len();
                    at.fill(0);
                    for p in mants {
                        for (a, m) in at.iter_mut().zip(&x.mant[..n]) {
                            *a += usize::from(*p < *m) * 2 + usize::from(*p == *m);
                        }
                    }
                }
                _ => at.iter_mut().enumerate().for_each(|(i, a)| {
                    *a = key
                        .mant(x.mant[i])
                        .unwrap_or_else(|| key.num(x.num(rep, i)))
                }),
            },
        }
        for (w, chunk) in at.chunks_mut(64).enumerate() {
            let odd = !x.valid[w] | x.wide[w];
            if odd == 0 {
                continue;
            }
            for (j, a) in chunk.iter_mut().enumerate() {
                if (odd >> j) & 1 == 1 {
                    *a = match x.ok(w * 64 + j) {
                        false => null,
                        true => key.num(x.num(rep, w * 64 + j)),
                    };
                }
            }
        }
    }

    #[inline(always)]
    fn scales<'r>(rep: Rep, reg: &'r Reg, buf: &'r mut [u8; Reg::LANES], n: usize) -> &'r [u8] {
        match rep {
            Rep::Fix(s) => {
                buf[..n].fill(s);
                &buf[..n]
            }
            _ => &reg.scale[..n],
        }
    }

    fn load(&self, d: &mut Reg, feed: &Feed, rep: Rep) {
        let (n, words, start) = (self.n, self.words.min(Reg::WORDS), self.start);
        let w0 = start / 64;
        for w in 0..words {
            let take = (n - w * 64).min(64);
            d.valid[w] = feed.valid_word(start + w * 64, take);
            d.fault[w] = feed.fault_word(w0 + w);
        }
        match (&feed.data, rep) {
            (Data::Num { mant, .. }, Rep::Fix(_)) => {
                d.mant[..n].copy_from_slice(&mant[start..start + n])
            }
            (Data::Num { mant, scale }, _) => {
                d.mant[..n].copy_from_slice(&mant[start..start + n]);
                match scale {
                    Scales::Fixed(s) => d.scale[..n].fill(*s),
                    Scales::Each(each) => d.scale[..n].copy_from_slice(&each[start..start + n]),
                }
            }
            (Data::Bool { bits, offset }, _) => {
                for w in 0..words {
                    let take = (n - w * 64).min(64);
                    d.bits[w] = Bits::word(bits, offset + start + w * 64, take);
                }
            }
            (Data::Codes(codes), _) => d.codes[..n].copy_from_slice(&codes[start..start + n]),
            (Data::Text(text), _) => self.load_text(d, text),
        }
        if let Some((bits, values)) = &feed.wide {
            for w in 0..words {
                let take = (n - w * 64).min(64);
                Bits::each(
                    Bits::word(bits, start + w * 64, take) & d.valid[w] & !d.fault[w],
                    w * 64,
                    |i| {
                        if let Ok(at) = values.binary_search_by_key(&(start + i), |(row, _)| *row) {
                            d.keep(Rep::Dyn, i, N::Big(values[at].1));
                        }
                    },
                );
            }
        }
    }

    fn values(&self, d: &mut Reg, m: &[u64], (t, rt): (&Reg, Rep), (e, re): (&Reg, Rep), rep: Rep) {
        let n = self.n;
        match rep {
            Rep::Fix(_) => K::blend(m, &t.mant[..n], &e.mant[..n], &mut d.mant[..n]),
            Rep::Dyn => {
                K::blend(m, &t.mant[..n], &e.mant[..n], &mut d.mant[..n]);
                match (rt, re) {
                    (Rep::Dyn, Rep::Dyn) => {
                        K::blend(m, &t.scale[..n], &e.scale[..n], &mut d.scale[..n])
                    }
                    (Rep::Dyn, x) => {
                        d.scale[..n].fill(x.scale_at(e, 0));
                        K::copy_masked(m, &t.scale[..n], &mut d.scale[..n]);
                    }
                    (x, Rep::Dyn) => {
                        d.scale[..n].copy_from_slice(&e.scale[..n]);
                        K::fill_masked(m, x.scale_at(t, 0), &mut d.scale[..n]);
                    }
                    (x, y) => {
                        d.scale[..n].fill(y.scale_at(e, 0));
                        K::fill_masked(m, x.scale_at(t, 0), &mut d.scale[..n]);
                    }
                }
            }
            Rep::Code => K::blend(m, &t.codes[..n], &e.codes[..n], &mut d.codes[..n]),
            Rep::Bool => {
                for (w, mw) in m.iter().enumerate() {
                    d.bits[w] = (t.bits[w] & mw) | (e.bits[w] & !mw);
                }
            }
            Rep::Text => self.blend_text(d, m, t, e),
            Rep::Date => self.blend_dates(d, m, t, e),
            Rep::Null => {}
        }
    }

    fn fold(
        &self,
        fold: Fold,
        b: &mut Buf,
        list: u16,
        (filter, value): (Option<R>, Option<(R, Rep)>),
        regs: &[Reg],
        scratch: &mut [i64],
    ) {
        let Some(l) = self.feeds.lists.get(list as usize) else {
            return;
        };
        if self.n == 0 {
            return;
        }
        let (s0, s1) = (self.start, self.start + self.n);
        let (r0, r1) = (l.parent[s0] as usize, l.parent[s1 - 1] as usize);
        let filter = filter.map(|f| &regs[f as usize]);
        let value = value.map(|(v, rep)| (&regs[v as usize], rep));
        let words = self.words.min(Reg::WORDS);
        let wide = b.any || value.is_some_and(|(v, _)| v.widened(words));
        if wide || !matches!(fold, Fold::Sum | Fold::Count) {
            for r in r0..=r1 {
                let lo = (l.offsets[r] as usize).max(s0) - s0;
                let hi = (l.offsets[r + 1] as usize).min(s1) - s0;
                self.tally(fold, b, r, lo..hi, filter, value);
            }
            return;
        }
        let mark = match (fold, value) {
            (Fold::Sum, Some((v, Rep::Dyn))) => return self.sum_dyn(b, l, filter, value, v),
            (Fold::Sum, Some((_, Rep::Fix(s)))) if s > 0 => Some(s),
            (Fold::Sum, Some((_, Rep::Fix(_)))) | (Fold::Count, _) => None,
            _ => return,
        };
        let sum = fold == Fold::Sum;
        let (keep, bad) = self.kept(filter, value.filter(|_| sum).map(|(v, _)| v));
        let n = self.n;
        if scratch.len() < 2 * Reg::LANES + 1 {
            return;
        }
        let (vals, pre) = scratch.split_at_mut(Reg::LANES);
        match (sum, value) {
            (true, Some((v, _))) => K::mask(&keep[..words], &v.mant[..n], &mut vals[..n]),
            _ => {
                for (&w, co) in keep[..words].iter().zip(vals[..n].chunks_mut(64)) {
                    for (j, o) in co.iter_mut().enumerate() {
                        *o = ((w >> j) & 1) as i64;
                    }
                }
            }
        }
        let big = vals[..n].iter().fold(0u64, |m, v| m.max(v.unsigned_abs()));
        let safe = big
            .checked_mul(n as u64)
            .is_some_and(|t| t < i64::MAX as u64);
        for (w, &bw) in bad.iter().enumerate().take(words) {
            Bits::each(bw, w * 64, |i| {
                Bits::set(&mut b.fault, l.parent[s0 + i] as usize, true)
            });
        }
        if safe {
            pre[0] = 0;
            let mut acc = 0i64;
            for (p, v) in pre[1..=n].iter_mut().zip(&vals[..n]) {
                acc = acc.wrapping_add(*v);
                *p = acc;
            }
        }
        for r in r0..=r1 {
            let lo = (l.offsets[r] as usize).max(s0) - s0;
            let hi = (l.offsets[r + 1] as usize).min(s1) - s0;
            if lo >= hi {
                continue;
            }
            let part = match safe {
                true => Some(pre[hi] - pre[lo]),
                false => {
                    let (mut acc, mut ovf) = (0i64, false);
                    for v in &vals[lo..hi] {
                        let (x, o) = acc.overflowing_add(*v);
                        acc = x;
                        ovf |= o;
                    }
                    (!ovf).then_some(acc)
                }
            };
            match part.and_then(|p| b.mant[r].checked_add(p)) {
                Some(x) => b.mant[r] = x,
                None => {
                    self.tally(fold, b, r, lo..hi, filter, value);
                    continue;
                }
            }
            if let Some(s) = mark {
                if filter.is_none() || !Bits::range(&keep, lo, hi, false) {
                    b.scale[r] = s;
                }
            }
        }
    }

    fn tally(
        &self,
        fold: Fold,
        b: &mut Buf,
        r: usize,
        range: std::ops::Range<usize>,
        filter: Option<&Reg>,
        value: Option<(&Reg, Rep)>,
    ) {
        let rep = match (fold, value) {
            (Fold::Sum, Some((_, Rep::Fix(0)))) => Rep::Fix(0),
            (Fold::Min | Fold::Max, Some((_, rep))) => rep,
            _ => Rep::Dyn,
        };
        for i in range {
            if Bits::get(&b.fault, r) {
                return;
            }
            if let Some(f) = filter {
                if !f.ok(i) || f.bad(i) {
                    Bits::set(&mut b.fault, r, true);
                    return;
                }
                if !Bits::get(&f.bits, i) {
                    continue;
                }
            }
            if fold == Fold::Count {
                b.mant[r] += 1;
                continue;
            }
            let Some((v, vr)) = value else {
                Bits::set(&mut b.fault, r, true);
                return;
            };
            if matches!(fold, Fold::Any | Fold::All | Fold::None) && b.codes[r] != 0 {
                match filter {
                    Some(_) => continue,
                    None => return,
                }
            }
            if !v.ok(i) || v.bad(i) {
                Bits::set(&mut b.fault, r, true);
                return;
            }
            match fold {
                Fold::Any | Fold::All | Fold::None => {
                    let bit = Bits::get(&v.bits, i);
                    if bit != (fold == Fold::All) {
                        b.codes[r] = 1;
                        Bits::set(&mut b.bits, r, fold == Fold::Any);
                    }
                }
                Fold::One => b.mant[r] += i64::from(Bits::get(&v.bits, i)),
                Fold::Min | Fold::Max if vr == Rep::Date => {
                    b.stamps();
                    let x = v.date(i);
                    let best = b.dates.get(r).copied().unwrap_or(Date(None));
                    let take = b.codes[r] == 0
                        || match fold {
                            Fold::Min => x < best,
                            _ => x >= best,
                        };
                    if take {
                        if let Some(slot) = b.dates.get_mut(r) {
                            *slot = x;
                        }
                    }
                    b.codes[r] = 1;
                }
                Fold::Min | Fold::Max => {
                    let x = v.num(vr, i);
                    let take = b.codes[r] == 0 || {
                        let order = x.compare(b.num(rep, r));
                        match fold {
                            Fold::Min => order == Ordering::Less,
                            _ => order != Ordering::Less,
                        }
                    };
                    if take {
                        b.keep(rep, r, x);
                    }
                    b.codes[r] = 1;
                }
                _ => match b.num(rep, r).arith(Arith::Add, v.num(vr, i)) {
                    Ok(Some(x)) => {
                        b.keep(rep, r, x);
                        b.codes[r] = b.codes[r].saturating_add(1);
                    }
                    _ => {
                        Bits::set(&mut b.fault, r, true);
                        return;
                    }
                },
            }
        }
    }

    fn kept(
        &self,
        filter: Option<&Reg>,
        value: Option<&Reg>,
    ) -> ([u64; Reg::WORDS], [u64; Reg::WORDS]) {
        let words = self.words.min(Reg::WORDS);
        let mut keep = [0u64; Reg::WORDS];
        let mut bad = [0u64; Reg::WORDS];
        for w in 0..words {
            let (k, x) = match filter {
                Some(f) => (
                    f.bits[w] & f.valid[w] & !f.fault[w],
                    !f.valid[w] | f.fault[w],
                ),
                None => (u64::MAX, 0),
            };
            let y = match value {
                Some(v) => k & (!v.valid[w] | v.fault[w]),
                None => 0,
            };
            keep[w] = k & self.tail(w);
            bad[w] = (x | y) & self.tail(w);
        }
        (keep, bad)
    }

    fn sum_dyn(
        &self,
        b: &mut Buf,
        l: &Lists,
        filter: Option<&Reg>,
        value: Option<(&Reg, Rep)>,
        v: &Reg,
    ) {
        let (n, s0) = (self.n, self.start);
        let s1 = s0 + n;
        let (r0, r1) = (l.parent[s0] as usize, l.parent[s1 - 1] as usize);
        let (keep, bad) = self.kept(filter, Some(v));
        let (mant, scale) = (&v.mant[..n], &v.scale[..n]);
        for (w, &bw) in bad.iter().enumerate().take(self.words.min(Reg::WORDS)) {
            Bits::each(bw, w * 64, |i| {
                Bits::set(&mut b.fault, l.parent[s0 + i] as usize, true)
            });
        }
        for r in r0..=r1 {
            let lo = (l.offsets[r] as usize).max(s0) - s0;
            let hi = (l.offsets[r + 1] as usize).min(s1) - s0;
            let (mut m, mut s, mut o) = (b.mant[r], b.scale[r], false);
            for j in lo..hi {
                let k = (keep[j / 64] >> (j % 64)) & 1 == 1;
                let (x, t, f) = K::add(false, m, mant[j], s, scale[j]);
                m = if k { x } else { m };
                s = if k { t } else { s };
                o |= k & f;
            }
            match o {
                true => self.tally(Fold::Sum, b, r, lo..hi, filter, value),
                false => {
                    b.mant[r] = m;
                    b.scale[r] = s;
                }
            }
        }
    }

    fn out(&self, slot: u16, r: &Reg, rep: Rep, scope: Scope, out: &mut Outputs) {
        let (n, words, start) = (self.n, self.words.min(Reg::WORDS), self.start);
        let w0 = start / 64;
        let (slots, faults) = out.parts();
        let Some(s) = slots.get_mut(slot as usize) else {
            return;
        };
        match &mut s.data {
            SlotData::Num { mant, scale } => match rep {
                Rep::Fix(x) => {
                    mant[start..start + n].copy_from_slice(&r.mant[..n]);
                    scale[start..start + n].fill(x);
                }
                Rep::Dyn => {
                    mant[start..start + n].copy_from_slice(&r.mant[..n]);
                    scale[start..start + n].copy_from_slice(&r.scale[..n]);
                }
                _ => {}
            },
            SlotData::Bool(bits) => bits[w0..w0 + words].copy_from_slice(&r.bits[..words]),
            SlotData::Code(codes) => codes[start..start + n].copy_from_slice(&r.codes[..n]),
            SlotData::Text { span, data } => self.out_text(r, span, data),
            SlotData::Date(dates) => {
                for (o, i) in dates[start..start + n].iter_mut().zip(0..n) {
                    *o = r.date(i).0;
                }
            }
        }
        match rep {
            Rep::Null => s.valid[w0..w0 + words].fill(0),
            _ => s.valid[w0..w0 + words].copy_from_slice(&r.valid[..words]),
        }
        if rep.num() && r.widened(words) {
            for w in 0..words {
                Bits::each(
                    r.wide[w] & r.valid[w] & !r.fault[w] & self.tail(w),
                    w * 64,
                    |i| {
                        s.wide
                            .push((start + i, r.big.get(i).copied().unwrap_or_default()))
                    },
                );
            }
        }
        let parent = match scope {
            Scope::Row => None,
            Scope::Elem(l) => self.feeds.lists.get(l as usize).map(|l| &l.root),
        };
        for w in 0..words {
            Bits::each(r.fault[w] & self.tail(w), start + w * 64, |i| {
                let row = match parent {
                    Some(p) => p[i] as usize,
                    None => i,
                };
                if self.select.is_none_or(|sel| Bits::get(sel, row)) {
                    faults.set(row);
                    s.fault.set(row);
                }
            });
        }
    }
}
