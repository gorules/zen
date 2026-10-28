use crate::lane::scaled::Scaled;
use crate::lane::Bits;
use crate::typed::batch::{Feed, Text};
use crate::typed::exec::{Buf, Exec, Others, Reg};
use crate::typed::hir::Func;
use crate::typed::ins::{Arg, Ins, Rep};
use crate::typed::kernel::K;

#[derive(Clone, Copy)]
pub(crate) enum TV<'a, 'x> {
    Lit(&'a [u8]),
    Col(&'a Text<'x>, &'a Feed<'x>),
    Reg(&'a Reg),
    Missing,
}

impl<'a, 'x> TV<'a, 'x> {
    #[inline]
    pub(crate) fn get(self, start: usize, i: usize) -> Option<&'a [u8]> {
        match self {
            TV::Lit(b) => Some(b),
            TV::Col(text, feed) => match feed.valid(start + i)
                && !Bits::get(feed.faults.as_deref().unwrap_or_default(), start + i)
            {
                true => text.get(start + i),
                false => None,
            },
            TV::Reg(r) => (Bits::get(&r.valid, i) && !Bits::get(&r.fault, i)).then(|| r.str(i)),
            TV::Missing => None,
        }
    }

    #[inline]
    pub(crate) fn ok(self, start: usize, w: usize) -> u64 {
        match self {
            TV::Lit(_) => u64::MAX,
            TV::Col(_, feed) => {
                feed.valid_word(start + w * 64, 64) & !feed.fault_word(start / 64 + w)
            }
            TV::Reg(r) => r.valid[w] & !r.fault[w],
            TV::Missing => 0,
        }
    }

    #[inline]
    pub(crate) fn bad(self, start: usize, w: usize) -> u64 {
        match self {
            TV::Col(_, feed) => feed.fault_word(start / 64 + w),
            TV::Reg(r) => r.fault[w],
            TV::Lit(_) | TV::Missing => 0,
        }
    }
}

impl Reg {
    pub(crate) fn texts(&mut self, n: usize) {
        self.text.clear();
        if self.span.len() < Reg::LANES {
            self.span.resize(Reg::LANES, (0, 0));
        }
        self.span[..n].fill((0, 0));
    }

    #[inline]
    pub(crate) fn str(&self, i: usize) -> &[u8] {
        let (lo, hi) = self.span.get(i).copied().unwrap_or_default();
        self.text.get(lo as usize..hi as usize).unwrap_or_default()
    }

    #[inline]
    pub(crate) fn push(&mut self, i: usize, bytes: &[u8]) {
        let lo = self.text.len() as u32;
        self.text.extend_from_slice(bytes);
        self.span[i] = (lo, self.text.len() as u32);
    }

    pub(crate) fn literal(&mut self, text: &str) {
        self.text.clear();
        self.text.extend_from_slice(text.as_bytes());
        self.span.clear();
        self.span.resize(Reg::LANES, (0, text.len() as u32));
    }
}

impl Buf {
    #[inline]
    pub(crate) fn str(&self, i: usize) -> &[u8] {
        let (lo, hi) = self.span.get(i).copied().unwrap_or_default();
        self.text.get(lo as usize..hi as usize).unwrap_or_default()
    }

    pub(crate) fn spill(&mut self, at: usize, bytes: &[u8]) {
        if self.span.len() < self.mant.len() {
            self.span.resize(self.mant.len(), (0, 0));
        }
        let lo = self.text.len() as u32;
        self.text.extend_from_slice(bytes);
        if let Some(s) = self.span.get_mut(at) {
            *s = (lo, self.text.len() as u32);
        }
    }
}

impl<'e, 'x> Exec<'e, 'x> {
    pub(crate) fn view<'a>(&'a self, arg: &'a Arg, regs: &'a Others) -> TV<'a, 'x> {
        match arg {
            Arg::Lit(s) => TV::Lit(s.as_bytes()),
            Arg::Reg(r) => TV::Reg(&regs[*r as usize]),
            Arg::Src(src) => match self.feed(*src) {
                Some(feed) => match feed.text() {
                    Some(text) => TV::Col(text, feed),
                    None => TV::Missing,
                },
                None => TV::Missing,
            },
        }
    }

    fn find(hay: &[u8], needle: &[u8]) -> bool {
        needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
    }

    fn mapped(func: Func, bytes: &[u8], d: &mut Reg, i: usize) -> bool {
        if bytes.is_ascii() && func != Func::Trim {
            let lo = d.text.len();
            d.text.extend_from_slice(bytes);
            let tail = &mut d.text[lo..];
            match func {
                Func::Upper => tail.make_ascii_uppercase(),
                _ => tail.make_ascii_lowercase(),
            }
            d.span[i] = (lo as u32, d.text.len() as u32);
            return true;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return false;
        };
        match func {
            Func::Upper => d.push(i, text.to_uppercase().as_bytes()),
            Func::Lower => d.push(i, text.to_lowercase().as_bytes()),
            _ => d.push(i, text.trim().as_bytes()),
        }
        true
    }

    fn faults(&self, d: &mut Reg, views: &[TV]) {
        for w in 0..self.words.min(Reg::WORDS) {
            d.valid[w] = u64::MAX;
            d.fault[w] = views.iter().fold(0, |acc, v| acc | !v.ok(self.start, w)) & self.tail(w);
        }
    }

    pub(crate) fn text(&self, ins: &Ins, regs: &mut [Reg], act: Option<&[u64; Reg::WORDS]>) {
        let (n, words, start) = (self.n, self.words.min(Reg::WORDS), self.start);
        match ins {
            Ins::EqText { ne, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let (x, y) = (self.view(a, regs), self.view(b, regs));
                for w in 0..words {
                    d.valid[w] = u64::MAX;
                    d.fault[w] = (x.bad(start, w) | y.bad(start, w)) & self.tail(w);
                }
                let ne = *ne;
                self.bools(act, &mut d.bits, |i| {
                    (x.get(start, i) == y.get(start, i)) != ne
                });
            }),
            Ins::Text { func, dst, a, b } => Self::def(regs, *dst, |d, regs| {
                let x = self.view(a, regs);
                let y = b.as_ref().map_or(TV::Lit(b""), |b| self.view(b, regs));
                self.faults(d, &[x, y]);
                match (func, x) {
                    (Func::Len, TV::Col(Text::Str { offsets, .. }, _)) => {
                        for (m, p) in d.mant[..n]
                            .iter_mut()
                            .zip(offsets[start..=start + n].windows(2))
                        {
                            *m = (p[1] - p[0]) as i64;
                        }
                    }
                    (Func::Len, TV::Lit(l)) => d.mant[..n].fill(l.len() as i64),
                    (Func::Len, x) => {
                        for (i, m) in d.mant[..n].iter_mut().enumerate() {
                            *m = x.get(start, i).map_or(0, |t| t.len() as i64);
                        }
                    }
                    (Func::StartsWith, TV::Col(Text::Str { offsets, data }, _))
                        if act.is_none() && matches!(y, TV::Lit(_)) =>
                    {
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
                    (Func::StartsWith | Func::EndsWith | Func::Contains, _) => {
                        let func = *func;
                        self.bools(act, &mut d.bits, |i| {
                            match (x.get(start, i), y.get(start, i)) {
                                (Some(a), Some(b)) => match func {
                                    Func::StartsWith => a.starts_with(b),
                                    Func::EndsWith => a.ends_with(b),
                                    _ => Self::find(a, b),
                                },
                                _ => false,
                            }
                        })
                    }
                    _ => {}
                }
            }),
            Ins::Matches { dst, a, pattern } => Self::def(regs, *dst, |d, regs| {
                let x = self.view(a, regs);
                self.faults(d, &[x]);
                d.bits[..words].fill(0);
                let skip = d.fault;
                let mut broken = Vec::new();
                self.each(act, &skip, |i| {
                    match x.get(start, i).map(std::str::from_utf8) {
                        Some(Ok(t)) => Bits::set(&mut d.bits, i, pattern.is_match(t)),
                        _ => broken.push(i),
                    }
                });
                broken
                    .into_iter()
                    .for_each(|i| Bits::set(&mut d.fault, i, true));
            }),
            Ins::Map { func, dst, a } => Self::def(regs, *dst, |d, regs| {
                let x = self.view(a, regs);
                self.faults(d, &[x]);
                d.texts(n);
                let skip = d.fault;
                let mut broken = Vec::new();
                self.each(act, &skip, |i| {
                    let ok = x
                        .get(start, i)
                        .is_some_and(|bytes| Self::mapped(*func, bytes, d, i));
                    if !ok {
                        broken.push(i);
                    }
                });
                broken
                    .into_iter()
                    .for_each(|i| Bits::set(&mut d.fault, i, true));
            }),
            Ins::Concat { dst, parts } => Self::def(regs, *dst, |d, regs| {
                let views: Vec<TV> = parts.iter().map(|p| self.view(p, regs)).collect();
                self.faults(d, &views);
                d.texts(n);
                let skip = d.fault;
                self.each(act, &skip, |i| {
                    let lo = d.text.len() as u32;
                    for v in &views {
                        d.text
                            .extend_from_slice(v.get(start, i).unwrap_or_default());
                    }
                    d.span[i] = (lo, d.text.len() as u32);
                });
            }),
            Ins::Str { dst, a, rep } => Self::def(regs, *dst, |d, regs| {
                let x = self.view(a, regs);
                for w in 0..words {
                    d.valid[w] = u64::MAX;
                    d.fault[w] = x.bad(start, w) & self.tail(w);
                }
                d.texts(n);
                let skip = d.fault;
                let mut scratch = String::new();
                let mut broken = Vec::new();
                match (rep, x) {
                    (Rep::Text, x) => self.each(act, &skip, |i| {
                        d.push(i, x.get(start, i).unwrap_or(b"null"))
                    }),
                    (Rep::Fix(_) | Rep::Dyn, TV::Reg(r)) => self.each(act, &skip, |i| {
                        scratch.clear();
                        match (Bits::get(&r.valid, i), Bits::get(&r.wide, i)) {
                            (false, _) => scratch.push_str("null"),
                            (true, true) => scratch.push_str(&r.num(*rep, i).decimal().to_string()),
                            (true, false) => match rep.scale_at(r, i) {
                                s if s <= 28 => Scaled::write(r.mant[i], s, &mut scratch),
                                _ => broken.push(i),
                            },
                        }
                        d.push(i, scratch.as_bytes());
                    }),
                    (Rep::Bool, TV::Reg(r)) => self.each(act, &skip, |i| {
                        let text: &[u8] = match (Bits::get(&r.valid, i), Bits::get(&r.bits, i)) {
                            (false, _) => b"null",
                            (true, true) => b"true",
                            (true, false) => b"false",
                        };
                        d.push(i, text);
                    }),
                    (Rep::Date, TV::Reg(r)) => {
                        self.each(act, &skip, |i| match Bits::get(&r.valid, i) {
                            true => d.push(i, r.date(i).to_string().as_bytes()),
                            false => d.push(i, b"null"),
                        })
                    }
                    (Rep::Null, _) => self.each(act, &skip, |i| d.push(i, b"null")),
                    _ => d.fault[..words].fill(u64::MAX),
                }
                broken
                    .into_iter()
                    .for_each(|i| Bits::set(&mut d.fault, i, true));
            }),
            Ins::Decode { dst, a, values } => Self::def(regs, *dst, |d, regs| {
                let a = &regs[*a as usize];
                d.valid[..words].copy_from_slice(&a.valid[..words]);
                d.fault[..words].copy_from_slice(&a.fault[..words]);
                d.texts(n);
                let mut skip = [0u64; Reg::WORDS];
                for (w, s) in skip.iter_mut().enumerate().take(words) {
                    *s = !a.valid[w] | a.fault[w];
                }
                let mut broken = Vec::new();
                self.each(act, &skip, |i| {
                    match usize::try_from(a.codes[i]).ok().and_then(|c| values.get(c)) {
                        Some(text) => d.push(i, text.as_bytes()),
                        None => broken.push(i),
                    }
                });
                broken
                    .into_iter()
                    .for_each(|i| Bits::set(&mut d.fault, i, true));
            }),
            Ins::Class { dst, a, matcher } => Self::def(regs, *dst, |d, regs| {
                let x = self.view(a, regs);
                for w in 0..words {
                    d.valid[w] = x.ok(start, w);
                    d.fault[w] = x.bad(start, w) & self.tail(w);
                }
                for (i, c) in d.codes[..n].iter_mut().enumerate() {
                    *c = matcher.code(x.get(start, i));
                }
            }),
            _ => {}
        }
    }

    pub(crate) fn load_text(&self, d: &mut Reg, text: &Text) {
        let (n, start) = (self.n, self.start);
        d.texts(n);
        match text {
            Text::Str { offsets, data } => {
                let (lo, hi) = (offsets[start] as usize, offsets[start + n] as usize);
                d.text
                    .extend_from_slice(data.get(lo..hi).unwrap_or_default());
                for (s, p) in d.span[..n]
                    .iter_mut()
                    .zip(offsets[start..=start + n].windows(2))
                {
                    *s = (
                        (p[0] as usize).saturating_sub(lo) as u32,
                        (p[1] as usize).saturating_sub(lo) as u32,
                    );
                }
            }
            Text::Dict { .. } => {
                for i in 0..n {
                    if Bits::get(&d.valid, i) {
                        d.push(i, text.get(start + i).unwrap_or_default());
                    }
                }
            }
        }
    }

    pub(crate) fn gather_text(&self, d: &mut Reg, b: &Buf, parent: &[u32]) {
        d.texts(self.n);
        for (i, &p) in parent.iter().enumerate() {
            d.push(i, b.str(p as usize));
        }
    }

    pub(crate) fn fill_text(&self, d: &mut Reg, b: &Buf) {
        d.texts(self.n);
        for i in 0..self.n {
            d.push(i, b.str(self.start + i));
        }
    }

    pub(crate) fn spill_text(&self, r: &Reg, b: &mut Buf) {
        for i in 0..self.n {
            b.spill(self.start + i, r.str(i));
        }
    }

    pub(crate) fn blend_text(&self, d: &mut Reg, m: &[u64], t: &Reg, e: &Reg) {
        d.texts(self.n);
        for i in 0..self.n {
            let src = match Bits::get(m, i) {
                true => t,
                false => e,
            };
            if Bits::get(&src.valid, i) {
                d.push(i, src.str(i));
            }
        }
    }

    pub(crate) fn pick_text(&self, d: &mut Reg, m: &[u64], a: &Reg) {
        for (w, &word) in m.iter().enumerate() {
            Bits::each(word & a.valid[w], w * 64, |i| d.push(i, a.str(i)));
        }
    }

    pub(crate) fn out_text(&self, r: &Reg, span: &mut [(u32, u32)], data: &mut Vec<u8>) {
        for i in 0..self.n {
            let lo = data.len() as u32;
            if Bits::get(&r.valid, i) && !Bits::get(&r.fault, i) {
                data.extend_from_slice(r.str(i));
            }
            if let Some(s) = span.get_mut(self.start + i) {
                *s = (lo, data.len() as u32);
            }
        }
    }
}
