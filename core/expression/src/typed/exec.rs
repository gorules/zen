use crate::lane::Column;
use crate::typed::batch::{Data, Feed, Feeds, Lists, Scales, Text};
use crate::typed::hir::{Arith, Cmp, Fold, Func, Lit, Scope};
use crate::typed::ins::{Arg, FixOp, Guard, Ins, Rep, Src, Tab, R};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use crate::typed::output::{Data as SlotData, Outputs};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq)]
enum V {
    Null,
    Num(Dec),
    Bool(bool),
    Code(i32),
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
    pub(crate) masks: Vec<u64>,
}

#[derive(Clone, Default)]
pub(crate) struct Buf {
    pub(crate) mant: Vec<i64>,
    pub(crate) scale: Vec<u8>,
    pub(crate) codes: Vec<i32>,
    pub(crate) bits: Vec<u64>,
    pub(crate) valid: Vec<u64>,
    pub(crate) fault: Vec<u64>,
}

struct Others<'r> {
    lo: &'r [Reg],
    hi: &'r [Reg],
    at: usize,
}

#[derive(Clone, Copy)]
enum TV<'a, 'x> {
    Lit(&'a [u8]),
    Col(&'a Text<'x>, &'a Feed<'x>),
    Missing,
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
        }
    }

    fn same(self, other: V) -> bool {
        match (self, other) {
            (V::Num(x), V::Num(y)) => x.eq_value(y),
            (V::Other, _) | (_, V::Other) => false,
            (x, y) => x == y,
        }
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
            masks: Vec::new(),
        }
    }

    pub(crate) fn fill(&mut self, rep: Rep, lit: &Lit) {
        self.valid.fill(u64::MAX);
        self.fault.fill(0);
        match lit {
            Lit::Num(d) => {
                self.mant.fill(d.mant);
                self.scale.fill(d.scale);
            }
            Lit::Bool(b) => self.bits.fill(if *b { u64::MAX } else { 0 }),
            Lit::Code(_, c) => self.codes.fill(*c),
            Lit::Null | Lit::Text(_) => self.valid.fill(0),
        }
        if rep == Rep::Null {
            self.valid.fill(0);
        }
    }

    #[inline]
    fn ok(&self, i: usize) -> bool {
        K::get(&self.valid, i)
    }

    #[inline]
    fn bad(&self, i: usize) -> bool {
        K::get(&self.fault, i)
    }

    #[inline]
    fn v(&self, rep: Rep, i: usize) -> V {
        if !self.ok(i) {
            return V::Null;
        }
        match rep {
            Rep::Fix(s) => V::Num(Dec::new(self.mant[i], s)),
            Rep::Dyn => V::Num(Dec::new(self.mant[i], self.scale[i])),
            Rep::Bool => V::Bool(K::get(&self.bits, i)),
            Rep::Code => V::Code(self.codes[i]),
            Rep::Text | Rep::Null => V::Null,
        }
    }

    #[inline]
    fn put(&mut self, i: usize, d: Dec) {
        self.mant[i] = d.mant;
        self.scale[i] = d.scale;
    }
}

impl<'a, 'x> TV<'a, 'x> {
    #[inline]
    fn get(self, at: usize) -> Option<&'a [u8]> {
        match self {
            TV::Lit(b) => Some(b),
            TV::Col(text, feed) => match feed.valid(at) {
                true => text.get(at),
                false => None,
            },
            TV::Missing => None,
        }
    }

    fn valid_word(self, at: usize) -> u64 {
        match self {
            TV::Lit(_) => u64::MAX,
            TV::Col(_, feed) => feed.valid_word(at, 64) & !feed.fault_word(at / 64),
            TV::Missing => 0,
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

    fn feed(&self, src: Src) -> Option<&'e Feed<'x>> {
        match src {
            Src::Col(c) => self.feeds.columns.get(c as usize),
            Src::Field(l, f) => self.feeds.fields.get(l as usize).and_then(|l| l.get(f as usize)),
            Src::Class(k) => self.feeds.classes.get(k as usize),
        }
    }

    fn view<'a>(&'a self, arg: &'a Arg) -> TV<'a, 'x> {
        match arg {
            Arg::Lit(s) => TV::Lit(s.as_bytes()),
            Arg::Src(src) => match self.feed(*src) {
                Some(feed) => match feed.text() {
                    Some(text) => TV::Col(text, feed),
                    None => TV::Missing,
                },
                None => TV::Missing,
            },
        }
    }

    #[inline(always)]
    fn num(&self, op: Arith, d: &mut Reg, act: Option<&[u64; Reg::WORDS]>, x: impl Fn(usize) -> Dec, y: impl Fn(usize) -> Dec) {
        let words = self.words.min(Reg::WORDS);
        if act.is_none() && op != Arith::Div {
            let mut bad = [0u64; Reg::WORDS];
            let (mant, scale) = (&mut d.mant, &mut d.scale);
            K::pack_idx(self.n, &mut bad[..words], |i| {
                let (p, q) = (x(i), y(i));
                let (r, s, o) = match op {
                    Arith::Mul => K::mul(p.mant, q.mant, p.scale, q.scale),
                    _ => K::add(op == Arith::Sub, p.mant, q.mant, p.scale, q.scale),
                };
                mant[i] = r;
                scale[i] = s;
                o
            });
            for (f, b) in d.fault[..words].iter_mut().zip(&bad) {
                *f |= b;
            }
            return;
        }
        let skip = d.fault;
        self.each(act, &skip, |i| {
            let r = match op {
                Arith::Add => x(i).checked_add(y(i)).map(Some),
                Arith::Sub => x(i).checked_sub(y(i)).map(Some),
                Arith::Mul => x(i).checked_mul(y(i)).map(Some),
                Arith::Div => x(i).checked_div(y(i)),
            };
            match r {
                Ok(Some(q)) => d.put(i, q),
                Ok(None) => K::set(&mut d.valid, i, false),
                Err(_) => K::set(&mut d.fault, i, true),
            }
        });
    }

    #[inline]
    fn def(regs: &mut [Reg], dst: R, f: impl FnOnce(&mut Reg, &Others)) {
        let at = dst as usize;
        let (lo, rest) = regs.split_at_mut(at);
        if let Some((d, hi)) = rest.split_first_mut() {
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

    pub(crate) fn mask_of(&self, guard: &Guard, regs: &[Reg], parent: Option<&[u64; Reg::WORDS]>) -> [u64; Reg::WORDS] {
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
                    idx.iter().fold(0, |acc, k| acc | s.masks.get(*k as usize * Reg::WORDS + w).copied().unwrap_or(0))
                }
            };
            *o = m & self.tail(w) & parent.map_or(u64::MAX, |p| p[w]);
        }
        out
    }

    #[inline(always)]
    fn bools(&self, act: Option<&[u64; Reg::WORDS]>, bits: &mut [u64; Reg::WORDS], mut f: impl FnMut(usize) -> bool) {
        let words = self.words.min(Reg::WORDS);
        match act {
            None => K::pack_idx(self.n, &mut bits[..words], f),
            Some(_) => {
                bits[..words].fill(0);
                let none = [0u64; Reg::WORDS];
                self.each(act, &none, |i| {
                    if f(i) {
                        K::set(bits, i, true);
                    }
                });
            }
        }
    }

    #[inline(always)]
    fn each(&self, act: Option<&[u64; Reg::WORDS]>, skip: &[u64; Reg::WORDS], mut f: impl FnMut(usize)) {
        for w in 0..self.words.min(Reg::WORDS) {
            let mut m = act.map_or(u64::MAX, |a| a[w]) & self.tail(w) & !skip[w];
            while m != 0 {
                let i = w * 64 + m.trailing_zeros() as usize;
                m &= m - 1;
                f(i);
            }
        }
    }

    pub(crate) fn run(&self, code: &[Ins], regs: &mut [Reg], bufs: &mut [Buf], scratch: &mut [i64], stack: &mut Stack, out: &mut Outputs) {
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
                            let name = format!("{ins:?}").split([' ', '{']).next().unwrap_or("").to_string();
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
    pub(crate) fn step(&self, ins: &Ins, regs: &mut [Reg], bufs: &mut [Buf], scratch: &mut [i64], act: Option<&[u64; Reg::WORDS]>, out: &mut Outputs) {
        let (n, words, start) = (self.n, self.words.min(Reg::WORDS), self.start);
        match ins {
            Ins::Skip { .. } => {}
            Ins::Load { dst, src, rep } => {
                let Some(feed) = self.feed(*src) else {
                    return;
                };
                Self::def(regs, *dst, |d, _| self.load(d, feed, *rep));
            }
            Ins::Gather { dst, buf, list, rep } => {
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
                        Rep::Bool => K::pack1(parent, &mut d.bits[..words], |p| K::get(&b.bits, p as usize)),
                        Rep::Text | Rep::Null => {}
                    }
                    let (r0, r1) = (parent.first().map_or(0, |p| *p as usize), parent.last().map_or(0, |p| *p as usize + 1));
                    if K::range(&b.valid, r0, r1, true) && K::range(&b.fault, r0, r1, false) {
                        d.valid[..words].fill(u64::MAX);
                        d.fault[..words].fill(0);
                    } else {
                        K::pack1(parent, &mut d.valid[..words], |p| K::get(&b.valid, p as usize));
                        K::pack1(parent, &mut d.fault[..words], |p| K::get(&b.fault, p as usize));
                    }
                });
            }
            Ins::Fill { dst, buf, rep } => {
                let b = &bufs[*buf as usize];
                let w0 = start / 64;
                Self::def(regs, *dst, |d, _| {
                    d.valid[..words].copy_from_slice(&b.valid[w0..w0 + words]);
                    d.fault[..words].copy_from_slice(&b.fault[w0..w0 + words]);
                    match rep {
                        Rep::Fix(_) => d.mant[..n].copy_from_slice(&b.mant[start..start + n]),
                        Rep::Dyn => {
                            d.mant[..n].copy_from_slice(&b.mant[start..start + n]);
                            d.scale[..n].copy_from_slice(&b.scale[start..start + n]);
                        }
                        Rep::Code => d.codes[..n].copy_from_slice(&b.codes[start..start + n]),
                        Rep::Bool => d.bits[..words].copy_from_slice(&b.bits[w0..w0 + words]),
                        Rep::Text | Rep::Null => {}
                    }
                });
            }
            Ins::Spill { src, buf, rep } => {
                let r = &regs[*src as usize];
                let b = &mut bufs[*buf as usize];
                let w0 = start / 64;
                b.valid[w0..w0 + words].copy_from_slice(&r.valid[..words]);
                b.fault[w0..w0 + words].copy_from_slice(&r.fault[..words]);
                match rep {
                    Rep::Fix(_) => b.mant[start..start + n].copy_from_slice(&r.mant[..n]),
                    Rep::Dyn => {
                        b.mant[start..start + n].copy_from_slice(&r.mant[..n]);
                        b.scale[start..start + n].copy_from_slice(&r.scale[..n]);
                    }
                    Rep::Code => b.codes[start..start + n].copy_from_slice(&r.codes[..n]),
                    Rep::Bool => b.bits[w0..w0 + words].copy_from_slice(&r.bits[..words]),
                    Rep::Text | Rep::Null => {}
                }
            }
            Ins::Fix { op, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                self.strict2(d, a, b);
                let (x, y, o, f) = (&a.mant[..n], &b.mant[..n], &mut d.mant[..n], &mut d.fault[..words]);
                match op {
                    FixOp::Add => K::ovf2(x, y, o, f, i64::overflowing_add),
                    FixOp::Sub => K::ovf2(x, y, o, f, i64::overflowing_sub),
                    FixOp::Mul => K::ovf2(x, y, o, f, i64::overflowing_mul),
                }
            }),
            Ins::Neg { dst, a, rep } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                K::ovf1(&a.mant[..n], &mut d.mant[..n], &mut d.fault[..words], |x| (x.wrapping_neg(), (x == 0) | (x == i64::MIN)));
                if *rep == Rep::Dyn {
                    d.scale[..n].copy_from_slice(&a.scale[..n]);
                }
            }),
            Ins::Num { op, dst, a, ra, b, rb } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                self.strict2(d, a, b);
                let (am, bm) = (&a.mant[..n], &b.mant[..n]);
                if act.is_none() && *op != Arith::Div {
                    let (mut pa, mut pb) = ([0u8; Reg::LANES], [0u8; Reg::LANES]);
                    let (asc, bsc) = (Self::scales(*ra, a, &mut pa, n), Self::scales(*rb, b, &mut pb, n));
                    let (sub, mul) = (*op == Arith::Sub, *op == Arith::Mul);
                    let (dm, ds, df) = (&mut d.mant[..n], &mut d.scale[..n], &mut d.fault[..words]);
                    match mul {
                        true => K::dyn2(am, bm, asc, bsc, dm, ds, df, K::mul),
                        false => K::dyn2(am, bm, asc, bsc, dm, ds, df, |x, y, p, q| K::add(sub, x, y, p, q)),
                    }
                    return;
                }
                let (ra, rb) = (*ra, *rb);
                self.num(*op, d, act, |i| Dec::new(am[i], ra.scale_at(a, i)), |i| Dec::new(bm[i], rb.scale_at(b, i)));
            }),
            Ins::DivPow { dst, a, ra, k, by } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                let (k, by, ra) = (*k, *by, *ra);
                let slow_lane = |i: usize, d: &mut Reg| match Dec::new(a.mant[i], ra.scale_at(a, i)).div_power(k, by) {
                    Ok(q) => d.put(i, q),
                    Err(_) => K::set(&mut d.fault, i, true),
                };
                let mut slow = [0u64; Reg::WORDS];
                let shift = match ra {
                    Rep::Fix(sa) => sa as i32 - by as i32,
                    _ => -1,
                };
                let fast = act.is_none() && k >= 2 && (0..=19).contains(&shift);
                let mut even = [0u64; Reg::WORDS];
                if fast {
                    K::pack1(&a.mant[..n], &mut even[..words], |m| (m % 10 != 0) & (m != i64::MIN));
                    d.mant[..n].copy_from_slice(&a.mant[..n]);
                    d.scale[..n].fill((shift + k as i32) as u8);
                    for (w, word) in even.iter_mut().enumerate().take(words) {
                        *word |= d.fault[w];
                    }
                }
                let (mant, scale) = (&mut d.mant, &mut d.scale);
                let mut quick = |i: usize| {
                    let (m, t, bad) = K::div_pow(a.mant[i], ra.scale_at(a, i) as i32 - by as i32, k);
                    mant[i] = m;
                    scale[i] = t;
                    bad
                };
                match (fast, act) {
                    (true, _) => self.each(None, &even, |i| K::set(&mut slow, i, quick(i))),
                    (false, None) => K::pack_idx(n, &mut slow[..words], quick),
                    (false, Some(_)) => {
                        let skip = d.fault;
                        self.each(act, &skip, |i| K::set(&mut slow, i, quick(i)));
                    }
                }
                for (w, word) in slow.iter_mut().enumerate().take(words) {
                    *word = !(*word & !d.fault[w]);
                }
                self.each(None, &slow, |i| slow_lane(i, d));
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
                        for ((o, c), &x) in d.mant[..n].iter_mut().zip(d.scale[..n].iter_mut()).zip(sm) {
                            *o = tab.mant[(x as usize) & 255];
                            *c = tab.scale[(x as usize) & 255];
                        }
                    }
                    Rep::Code => {
                        for (o, &x) in d.codes[..n].iter_mut().zip(sm) {
                            *o = tab.codes[(x as usize) & 255];
                        }
                    }
                    Rep::Bool => K::pack1(sm, &mut d.bits[..words], |x| Tab::bit(&tab.flag, x as usize)),
                    Rep::Text | Rep::Null => {}
                }
                K::pack1(sm, &mut d.valid[..words], |x| Tab::bit(&tab.ok, x as usize));
                d.fault[..words].copy_from_slice(&s.fault[..words]);
            }),
            Ins::CmpFix { op, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                let (x, y, o) = (&a.mant[..n], &b.mant[..n], &mut d.bits[..words]);
                match op {
                    Cmp::Eq | Cmp::Ne => K::pack2(x, y, o, |x, y| x == y),
                    Cmp::Lt => K::pack2(x, y, o, |x, y| x < y),
                    Cmp::Le => K::pack2(x, y, o, |x, y| x <= y),
                    Cmp::Gt => K::pack2(x, y, o, |x, y| x > y),
                    Cmp::Ge => K::pack2(x, y, o, |x, y| x >= y),
                }
                match op {
                    Cmp::Eq | Cmp::Ne => self.eq_meta(d, a, b, *op == Cmp::Ne),
                    _ => self.strict2(d, a, b),
                }
            }),
            Ins::CmpNum { op, dst, a, ra, b, rb } => Self::def(regs, *dst, |d, regs| {
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
                match op {
                    Cmp::Eq | Cmp::Ne => self.eq_meta(d, a, b, op == Cmp::Ne),
                    _ => self.strict2(d, a, b),
                }
            }),
            Ins::CmpNumK { op, dst, a, ra, k, tab, fits } => Self::def(regs, *dst, |d, regs| {
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
            Ins::CmpGen { op, dst, a, ra, b, rb } if matches!((ra, rb), (Rep::Null, _) | (_, Rep::Null)) && *ra != Rep::Text && *rb != Rep::Text => Self::def(regs, *dst, |d, regs| {
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
            }),
            Ins::CmpGen { op, dst, a, ra, b, rb } if matches!((ra, rb), (Rep::Fix(_) | Rep::Dyn, Rep::Fix(_) | Rep::Dyn)) => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                let equality = matches!(op, Cmp::Eq | Cmp::Ne);
                for w in 0..words {
                    let tail = self.tail(w);
                    let mut hit = 0u64;
                    for j in 0..(n - w * 64).min(64) {
                        let i = w * 64 + j;
                        let order = Dec::new(a.mant[i], ra.scale_at(a, i)).compare(Dec::new(b.mant[i], rb.scale_at(b, i)));
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
                    d.fault[w] = a.fault[w] | b.fault[w] | if equality { 0 } else { !both & tail };
                }
            }),
            Ins::CmpGen { op, dst, a, ra, b, rb } => Self::def(regs, *dst, |d, regs| {
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
                        (_, V::Num(x), V::Num(y)) => Some(K::ord(*op, x.compare(y))),
                        _ => None,
                    };
                    match r {
                        Some(v) => K::set(&mut d.bits, i, v),
                        None => K::set(&mut d.fault, i, true),
                    }
                }
            }),
            Ins::EqCode { ne, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                K::pack2(&a.codes[..n], &b.codes[..n], &mut d.bits[..words], |x, y| x == y);
                self.eq_meta(d, a, b, *ne);
            }),
            Ins::EqBool { ne, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                for w in 0..words {
                    d.bits[w] = !(a.bits[w] ^ b.bits[w]);
                }
                self.eq_meta(d, a, b, *ne);
            }),
            Ins::EqText { ne, dst, a, b } => {
                let (x, y) = (self.view(a), self.view(b));
                Self::def(regs, *dst, |d, _| {
                    d.valid[..words].fill(u64::MAX);
                    d.fault[..words].fill(0);
                    let ne = *ne;
                    self.bools(act, &mut d.bits, |i| (x.get(start + i) == y.get(start + i)) != ne);
                })
            }
            Ins::InCode { dst, a, set, null } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                let set = *set;
                K::pack1(&a.codes[..n], &mut d.bits[..words], |c| ((set >> (c & 63)) & 1 != 0) & ((c as u32) < 64));
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
                    K::set(&mut d.bits, i, set.iter().any(|l| x.same(V::lit(l))));
                }
            }),
            Ins::InRangeFix { dst, a, lo, span, empty } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                let (lo, span) = (*lo, *span);
                match empty {
                    true => d.bits[..words].fill(0),
                    false => K::pack1(&a.mant[..n], &mut d.bits[..words], |x| (x.wrapping_sub(lo) as u64) <= span),
                }
                self.strict1(d, a);
            }),
            Ins::InRangeGen { dst, a, ra, range } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                self.strict1(d, a);
                for w in 0..words {
                    let mut hit = 0u64;
                    for j in 0..(n - w * 64).min(64) {
                        let i = w * 64 + j;
                        let x = Dec::new(a.mant[i], ra.scale_at(a, i));
                        let (lo, hi) = (x.compare(range.lo), x.compare(range.hi));
                        let above = lo == Ordering::Greater || (range.lo_closed && lo == Ordering::Equal);
                        let below = hi == Ordering::Less || (range.hi_closed && hi == Ordering::Equal);
                        hit |= u64::from(above && below) << j;
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
            Ins::Blend { dst, c, t, rt, e, re, rep } => Self::def(regs, *dst, |d, regs| {
                let (c, t, e) = (&regs[*c as usize], &regs[*t as usize], &regs[*e as usize]);
                let mut ct = [0u64; Reg::WORDS];
                for w in 0..words {
                    ct[w] = c.bits[w] & c.valid[w];
                    let cf = !c.bits[w] & c.valid[w];
                    d.valid[w] = (ct[w] & t.valid[w]) | (cf & e.valid[w]);
                    d.fault[w] = c.fault[w] | !c.valid[w] | (ct[w] & t.fault[w]) | (cf & e.fault[w]);
                }
                self.values(d, &ct[..words], (t, *rt), (e, *re), *rep);
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
                        let (tb, eb) = (if t != 0 { u64::MAX } else { 0 }, if e != 0 { u64::MAX } else { 0 });
                        for w in 0..words {
                            d.bits[w] = (ct[w] & tb) | (!ct[w] & eb);
                        }
                    }
                    _ => {}
                }
            }),
            Ins::Coalesce { dst, a, ra, b, rb, rep } => Self::def(regs, *dst, |d, regs| {
                let (a, b) = (&regs[*a as usize], &regs[*b as usize]);
                let mut m = [0u64; Reg::WORDS];
                for w in 0..words {
                    m[w] = a.valid[w];
                    d.valid[w] = a.valid[w] | b.valid[w];
                    d.fault[w] = a.fault[w] | (!a.valid[w] & b.fault[w]);
                }
                self.values(d, &m[..words], (a, *ra), (b, *rb), *rep);
            }),
            Ins::First { dst, conds, sel, strict } => Self::def(regs, *dst, |d, regs| {
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
                            false => c.fault[w] & rem[w],
                        };
                        bad[w] |= fail;
                        rem[w] &= !(hit | fail);
                    }
                }
                d.masks[k * Reg::WORDS..k * Reg::WORDS + words].copy_from_slice(&rem[..words]);
                if *sel {
                    for ci in 0..=k {
                        let (masks, mant) = (&d.masks[ci * Reg::WORDS..ci * Reg::WORDS + words], &mut d.mant[..n]);
                        K::fill_masked(masks, ci as i64, mant);
                    }
                }
                d.valid[..words].fill(u64::MAX);
                d.fault[..words].copy_from_slice(&bad[..words]);
            }),
            Ins::Pick { dst, sel, arms, rep } => Self::def(regs, *dst, |d, regs| {
                let s = &regs[*sel as usize];
                for w in 0..words {
                    d.valid[w] = 0;
                    d.fault[w] = s.fault[w];
                }
                let mut m = [0u64; Reg::WORDS];
                for (arm, ra, idx) in arms {
                    let a = &regs[*arm as usize];
                    for (w, mw) in m.iter_mut().enumerate().take(words) {
                        *mw = idx.iter().fold(0, |acc, k| acc | s.masks.get(*k as usize * Reg::WORDS + w).copied().unwrap_or(0));
                        d.valid[w] |= *mw & a.valid[w];
                        d.fault[w] |= *mw & a.fault[w];
                    }
                    let m = &m[..words];
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
                        Rep::Text | Rep::Null => {}
                    }
                }
            }),
            Ins::Fold { fold, buf, list, filter, value } => self.fold(*fold, &mut bufs[*buf as usize], *list, (*filter, *value), regs, scratch),
            Ins::Text { func, dst, a, b } => {
                let x = self.view(a);
                let y = b.as_ref().map_or(TV::Lit(b""), |b| self.view(b));
                Self::def(regs, *dst, |d, _| {
                    for w in 0..words {
                        d.valid[w] = u64::MAX;
                        d.fault[w] = !(x.valid_word(start + w * 64) & y.valid_word(start + w * 64)) & self.tail(w);
                    }
                    match (func, x) {
                        (Func::Len, TV::Col(Text::Str { offsets, .. }, _)) => {
                            for (m, p) in d.mant[..n].iter_mut().zip(offsets[start..=start + n].windows(2)) {
                                *m = (p[1] - p[0]) as i64;
                            }
                        }
                        (Func::Len, TV::Lit(l)) => d.mant[..n].fill(l.len() as i64),
                        (Func::Len, TV::Col(text, _)) => {
                            for (i, m) in d.mant[..n].iter_mut().enumerate() {
                                *m = text.get(start + i).map_or(0, |t| t.len() as i64);
                            }
                        }
                        (Func::Len, TV::Missing) => d.fault[..words].fill(u64::MAX),
                        (Func::StartsWith, TV::Col(Text::Str { offsets, data }, _)) if act.is_none() && matches!(y, TV::Lit(_)) => {
                            let p = match y {
                                TV::Lit(p) => p,
                                _ => b"",
                            };
                            let offs = &offsets[start..=start + n];
                            K::pack_idx(n, &mut d.bits[..words], |i| {
                                let (lo, hi) = (offs[i] as usize, offs[i + 1] as usize);
                                hi >= lo + p.len() && data.get(lo..lo + p.len()).is_some_and(|h| h == p)
                            });
                        }
                        (Func::StartsWith, _) => self.bools(act, &mut d.bits, |i| match (x.get(start + i), y.get(start + i)) {
                            (Some(a), Some(b)) => a.starts_with(b),
                            _ => false,
                        }),
                        (Func::EndsWith, _) => self.bools(act, &mut d.bits, |i| match (x.get(start + i), y.get(start + i)) {
                            (Some(a), Some(b)) => a.ends_with(b),
                            _ => false,
                        }),
                    }
                })
            }
            Ins::Out { slot, src, rep, scope } => self.out(*slot, &regs[*src as usize], *rep, *scope, out),
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
            (Data::Num { mant, .. }, Rep::Fix(_)) => d.mant[..n].copy_from_slice(&mant[start..start + n]),
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
                    d.bits[w] = Column::word(bits, offset + start + w * 64, take);
                }
            }
            (Data::Codes(codes), _) => d.codes[..n].copy_from_slice(&codes[start..start + n]),
            (Data::Text(_), _) => {}
        }
    }

    fn values(&self, d: &mut Reg, m: &[u64], (t, rt): (&Reg, Rep), (e, re): (&Reg, Rep), rep: Rep) {
        let n = self.n;
        match rep {
            Rep::Fix(_) => K::blend(m, &t.mant[..n], &e.mant[..n], &mut d.mant[..n]),
            Rep::Dyn => {
                K::blend(m, &t.mant[..n], &e.mant[..n], &mut d.mant[..n]);
                match (rt, re) {
                    (Rep::Dyn, Rep::Dyn) => K::blend(m, &t.scale[..n], &e.scale[..n], &mut d.scale[..n]),
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
            Rep::Text | Rep::Null => {}
        }
    }

    fn fold(&self, fold: Fold, b: &mut Buf, list: u16, (filter, value): (Option<R>, Option<(R, Rep)>), regs: &[Reg], scratch: &mut [i64]) {
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
        let fast = matches!((fold, value), (Fold::Count, _) | (Fold::Sum, Some((_, Rep::Fix(_)))));
        if let (Fold::Sum, Some((v, Rep::Dyn))) = (fold, value) {
            return self.sum_dyn(b, l, filter, v);
        }
        if fast {
            let mark = match (fold, value) {
                (Fold::Sum, Some((_, Rep::Fix(s)))) if s > 0 => Some(s),
                _ => None,
            };
            let sum = fold == Fold::Sum;
            let words = self.words.min(Reg::WORDS);
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
            let safe = big.checked_mul(n as u64).is_some_and(|t| t < i64::MAX as u64);
            for (w, &bw) in bad.iter().enumerate().take(words) {
                let mut m = bw;
                while m != 0 {
                    let i = w * 64 + m.trailing_zeros() as usize;
                    m &= m - 1;
                    K::set(&mut b.fault, l.parent[s0 + i] as usize, true);
                }
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
                    None => K::set(&mut b.fault, r, true),
                }
                if let Some(s) = mark {
                    if filter.is_none() || !K::range(&keep, lo, hi, false) {
                        b.scale[r] = s;
                    }
                }
            }
            return;
        }
        for r in r0..=r1 {
            if K::get(&b.fault, r) {
                continue;
            }
            let lo = (l.offsets[r] as usize).max(s0);
            let hi = (l.offsets[r + 1] as usize).min(s1);
            let mut bad = false;
            for e in lo..hi {
                let i = e - s0;
                if let Some(f) = filter {
                    if !f.ok(i) || f.bad(i) {
                        bad = true;
                        break;
                    }
                    if !K::get(&f.bits, i) {
                        continue;
                    }
                }
                match (fold, value) {
                    (Fold::Count, _) => b.mant[r] += 1,
                    (Fold::Sum, Some((v, rep))) => {
                        if !v.ok(i) || v.bad(i) {
                            bad = true;
                            break;
                        }
                        match Dec::new(b.mant[r], b.scale[r]).checked_add(Dec::new(v.mant[i], rep.scale_at(v, i))) {
                            Ok(s) => {
                                b.mant[r] = s.mant;
                                b.scale[r] = s.scale;
                            }
                            Err(_) => {
                                bad = true;
                                break;
                            }
                        }
                    }
                    (Fold::Sum, None) => {
                        bad = true;
                        break;
                    }
                }
            }
            if bad {
                K::set(&mut b.fault, r, true);
            }
        }
    }

    fn kept(&self, filter: Option<&Reg>, value: Option<&Reg>) -> ([u64; Reg::WORDS], [u64; Reg::WORDS]) {
        let words = self.words.min(Reg::WORDS);
        let mut keep = [0u64; Reg::WORDS];
        let mut bad = [0u64; Reg::WORDS];
        for w in 0..words {
            let (k, x) = match filter {
                Some(f) => (f.bits[w] & f.valid[w] & !f.fault[w], !f.valid[w] | f.fault[w]),
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

    fn sum_dyn(&self, b: &mut Buf, l: &Lists, filter: Option<&Reg>, v: &Reg) {
        let (n, s0) = (self.n, self.start);
        let s1 = s0 + n;
        let (r0, r1) = (l.parent[s0] as usize, l.parent[s1 - 1] as usize);
        let (keep, bad) = self.kept(filter, Some(v));
        let (mant, scale) = (&v.mant[..n], &v.scale[..n]);
        for (w, &bw) in bad.iter().enumerate().take(self.words.min(Reg::WORDS)) {
            let mut m = bw;
            while m != 0 {
                let i = w * 64 + m.trailing_zeros() as usize;
                m &= m - 1;
                K::set(&mut b.fault, l.parent[s0 + i] as usize, true);
            }
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
            b.mant[r] = m;
            b.scale[r] = s;
            if o {
                K::set(&mut b.fault, r, true);
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
        }
        match rep {
            Rep::Null => s.valid[w0..w0 + words].fill(0),
            _ => s.valid[w0..w0 + words].copy_from_slice(&r.valid[..words]),
        }
        let parent = match scope {
            Scope::Row => None,
            Scope::Elem(l) => self.feeds.lists.get(l as usize).map(|l| &l.parent),
        };
        for w in 0..words {
            let mut f = r.fault[w] & self.tail(w);
            while f != 0 {
                let i = start + w * 64 + f.trailing_zeros() as usize;
                f &= f - 1;
                let row = match parent {
                    Some(p) => p[i] as usize,
                    None => i,
                };
                if self.select.is_none_or(|sel| K::get(sel, row)) {
                    faults.set(row);
                    s.fault.set(row);
                }
            }
        }
    }
}
