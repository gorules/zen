use crate::typed::hir::{Arith, Cmp, Fold, Hir, Kind, Lit, NodeId, Op, Scope, Unsupported};
use crate::typed::ins::{Arg, Arm, BufDef, FixOp, Guard, Ins, Rep, Src, Tab, R};
use crate::typed::kernel::K;
use crate::typed::num::Dec;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Passes {
    pub(crate) fold: bool,
    pub(crate) factor: bool,
    pub(crate) skip: bool,
    pub(crate) reuse: bool,
    pub(crate) elide: bool,
    pub(crate) skip_min: u32,
}

pub(crate) struct Stage {
    pub(crate) scope: Scope,
    pub(crate) code: Vec<Ins>,
}

pub(crate) struct Built {
    pub(crate) stages: Vec<Stage>,
    pub(crate) nregs: usize,
    pub(crate) consts: Vec<(R, Rep, Lit)>,
    pub(crate) defs: Vec<BufDef>,
}

pub(crate) struct Build<'h> {
    hir: &'h Hir,
    sig: &'h [Option<u8>],
    classes: &'h HashMap<NodeId, u16>,
    bases: Vec<usize>,
    passes: Passes,
    rep: Vec<Rep>,
    level: Vec<u32>,
    stage: Vec<usize>,
    keys: Vec<(u32, Scope)>,
    users: Vec<Vec<NodeId>>,
    outs: Vec<Vec<usize>>,
    cross: Vec<bool>,
    acc: Vec<usize>,
    code: Vec<Vec<Ins>>,
    cur: usize,
    vreg: HashMap<(usize, NodeId), R>,
    next: usize,
    pinned: Vec<bool>,
    consts: Vec<(R, Rep, Lit)>,
    const_of: HashMap<(Lit, Rep), R>,
    defs: Vec<BufDef>,
    buf_of: Vec<Option<u16>>,
    sel_need: Vec<bool>,
    picks: HashMap<NodeId, Vec<(Arm, Vec<u16>)>>,
}

impl Passes {
    pub(crate) const ALL: Passes = Passes { fold: true, factor: true, skip: true, reuse: true, elide: true, skip_min: 2 };
}

impl Rep {
    fn of_kind(kind: Kind, fixed: Option<u8>) -> Rep {
        match kind {
            Kind::Num(Some(s)) => Rep::Fix(s),
            Kind::Num(None) => fixed.map_or(Rep::Dyn, Rep::Fix),
            Kind::Bool => Rep::Bool,
            Kind::Code(_) => Rep::Code,
            Kind::Text => Rep::Text,
            Kind::Null => Rep::Null,
        }
    }
}

impl<'h> Build<'h> {
    pub(crate) fn plan(hir: &'h Hir, sig: &'h [Option<u8>], classes: &'h HashMap<NodeId, u16>, passes: Passes) -> Result<Built, Unsupported> {
        let mut b = Build::new(hir, sig, classes, passes)?;
        b.emit_all()?;
        b.finish()
    }

    fn new(hir: &'h Hir, sig: &'h [Option<u8>], classes: &'h HashMap<NodeId, u16>, passes: Passes) -> Result<Self, Unsupported> {
        let n = hir.nodes.len();
        let mut bases = Vec::with_capacity(hir.schema.lists.len());
        let mut base = hir.schema.columns.len();
        for l in &hir.schema.lists {
            bases.push(base);
            base += l.fields.len();
        }
        let mut users: Vec<Vec<NodeId>> = vec![Vec::new(); n];
        for (i, node) in hir.nodes.iter().enumerate() {
            for inp in node.op.inputs() {
                let u = users.get_mut(inp as usize).ok_or_else(|| Unsupported::new("typed: bad input"))?;
                if u.last() != Some(&(i as NodeId)) {
                    u.push(i as NodeId);
                }
            }
        }
        let mut outs = vec![Vec::new(); n];
        for (k, o) in hir.outputs.iter().enumerate() {
            outs.get_mut(o.node as usize).ok_or_else(|| Unsupported::new("typed: bad output"))?.push(k);
        }
        let mut b = Build {
            hir,
            sig,
            classes,
            bases,
            passes,
            rep: Vec::with_capacity(n),
            level: vec![0; n],
            stage: vec![usize::MAX; n],
            keys: Vec::new(),
            users,
            outs,
            cross: vec![false; n],
            acc: vec![usize::MAX; n],
            code: Vec::new(),
            cur: 0,
            vreg: HashMap::new(),
            next: 0,
            pinned: Vec::new(),
            consts: Vec::new(),
            const_of: HashMap::new(),
            defs: Vec::new(),
            buf_of: vec![None; n],
            sel_need: vec![false; n],
            picks: HashMap::new(),
        };
        for i in 0..n {
            let r = b.rep_of(i)?;
            b.rep.push(r);
        }
        for (i, node) in hir.nodes.iter().enumerate() {
            if let Op::Pick(sel, arms) = &node.op {
                let plan = b.pick_plan(arms);
                if plan.iter().any(|(a, _)| !matches!(a, Arm::Node(_))) {
                    if let Some(x) = b.sel_need.get_mut(*sel as usize) {
                        *x = true;
                    }
                }
                b.picks.insert(i as NodeId, plan);
            }
        }
        b.levels()?;
        b.stages()?;
        Ok(b)
    }

    fn is_const(&self, i: NodeId) -> bool {
        matches!(self.hir.nodes[i as usize].op, Op::Const(_))
    }

    fn konst_num(&self, i: NodeId) -> Option<Dec> {
        match &self.hir.nodes[i as usize].op {
            Op::Const(Lit::Num(d)) => Some(*d),
            _ => None,
        }
    }

    fn rep_of(&self, i: usize) -> Result<Rep, Unsupported> {
        let hir = self.hir;
        let node = &hir.nodes[i];
        let r = |x: &NodeId| self.rep[*x as usize];
        Ok(match &node.op {
            Op::Column(c) => {
                let def = hir.schema.columns.get(*c as usize).ok_or_else(|| Unsupported::new("typed: column"))?;
                Rep::of_kind(def.ty.kind, self.sig.get(*c as usize).copied().flatten())
            }
            Op::Field(l, f) => {
                let def = hir.schema.lists.get(*l as usize).and_then(|x| x.fields.get(*f as usize)).ok_or_else(|| Unsupported::new("typed: field"))?;
                let at = self.bases.get(*l as usize).map(|b| b + *f as usize);
                Rep::of_kind(def.ty.kind, at.and_then(|a| self.sig.get(a).copied().flatten()))
            }
            Op::Parent(a) | Op::Neg(a) => r(a),
            Op::Const(lit) => Rep::of_lit(lit),
            Op::Classify(..) => Rep::Code,
            Op::Not(_) | Op::Cmp(..) | Op::And(..) | Op::Or(..) | Op::InSet(..) | Op::InRange(..) => Rep::Bool,
            Op::Arith(op, a, b) => match (op, r(a), r(b)) {
                (Arith::Add | Arith::Sub, Rep::Fix(x), Rep::Fix(y)) if x == y => Rep::Fix(x),
                (Arith::Mul, Rep::Fix(0), Rep::Fix(0)) => Rep::Fix(0),
                _ => Rep::Dyn,
            },
            Op::If(_, t, e) => Rep::join(r(t), r(e))?,
            Op::Coalesce(a, b) => Rep::join(r(a), r(b))?,
            Op::Pick(_, arms) => arms.iter().try_fold(Rep::Null, |acc, a| Rep::join(acc, r(a)))?,
            Op::Fold { fold: Fold::Sum, value: Some(v), .. } => match r(v) {
                Rep::Fix(0) => Rep::Fix(0),
                _ => Rep::Dyn,
            },
            Op::Fold { fold: Fold::Sum, value: None, .. } => return Err(Unsupported::new("typed: sum without value")),
            Op::Fold { .. } | Op::FirstMatch { .. } | Op::Call(crate::typed::hir::Func::Len, _) => Rep::Fix(0),
            Op::Call(..) => Rep::Bool,
        })
    }

    fn levels(&mut self) -> Result<(), Unsupported> {
        let hir = self.hir;
        for (i, node) in hir.nodes.iter().enumerate() {
            if matches!(node.op, Op::Const(_)) {
                continue;
            }
            let mut l = match node.op {
                Op::Field(..) => 1,
                Op::Fold { .. } => 2,
                _ => 0,
            };
            for inp in node.op.inputs() {
                if self.is_const(inp) {
                    continue;
                }
                let s = hir.nodes[inp as usize].scope;
                let bump = match (node.scope, s, &node.op) {
                    (Scope::Row, Scope::Row, _) => 0,
                    (Scope::Elem(a), Scope::Elem(b), _) if a == b => 0,
                    (Scope::Row, Scope::Elem(_), Op::Fold { .. }) => 1,
                    (Scope::Elem(_), Scope::Row, Op::Parent(_)) => 1,
                    _ => return Err(Unsupported::new("typed: scope crossing")),
                };
                l = l.max(self.level[inp as usize] + bump);
            }
            self.level[i] = match node.scope {
                Scope::Row => l + (l & 1),
                Scope::Elem(_) => l | 1,
            };
        }
        for i in (0..hir.nodes.len()).rev() {
            let node = &hir.nodes[i];
            if matches!(node.op, Op::Const(_)) || node.scope != Scope::Row || self.users[i].is_empty() {
                continue;
            }
            let mut latest = u32::MAX;
            for &u in &self.users[i] {
                let lu = match hir.nodes[u as usize].scope {
                    Scope::Row => self.level[u as usize],
                    Scope::Elem(_) => self.level[u as usize].saturating_sub(1),
                };
                latest = latest.min(lu);
            }
            let l = latest & !1;
            if l > self.level[i] && latest != u32::MAX {
                self.level[i] = l;
            }
        }
        Ok(())
    }

    fn scope_ord(s: Scope) -> u32 {
        match s {
            Scope::Row => 0,
            Scope::Elem(l) => 1 + l as u32,
        }
    }

    fn const_level(scope: Scope) -> u32 {
        match scope {
            Scope::Row => 0,
            Scope::Elem(_) => 1,
        }
    }

    fn stages(&mut self) -> Result<(), Unsupported> {
        let hir = self.hir;
        let mut keys: Vec<(u32, Scope)> = Vec::new();
        let mut add = |k: (u32, Scope)| {
            if !keys.contains(&k) {
                keys.push(k);
            }
        };
        for (i, node) in hir.nodes.iter().enumerate() {
            if matches!(node.op, Op::Const(_)) {
                continue;
            }
            add((self.level[i], node.scope));
            if let Op::Fold { list, .. } = node.op {
                add((self.level[i] - 1, Scope::Elem(list)));
            }
        }
        for o in &hir.outputs {
            if self.is_const(o.node) {
                add((Self::const_level(o.scope), o.scope));
            }
        }
        keys.sort_by_key(|(l, s)| (*l, Self::scope_ord(*s)));
        let find = |k: (u32, Scope)| keys.iter().position(|x| *x == k).ok_or_else(|| Unsupported::new("typed: stage"));
        for (i, node) in hir.nodes.iter().enumerate() {
            if matches!(node.op, Op::Const(_)) {
                continue;
            }
            self.stage[i] = find((self.level[i], node.scope))?;
            if let Op::Fold { list, .. } = node.op {
                self.acc[i] = find((self.level[i] - 1, Scope::Elem(list)))?;
            }
        }
        for (i, node) in hir.nodes.iter().enumerate() {
            if matches!(node.op, Op::Const(_)) || self.rep[i] == Rep::Text {
                continue;
            }
            for &u in &self.users[i] {
                let cs = match hir.nodes[u as usize].op {
                    Op::Fold { .. } => self.acc[u as usize],
                    _ => self.stage[u as usize],
                };
                if cs != self.stage[i] {
                    self.cross[i] = true;
                }
            }
            match node.op {
                Op::Fold { list, .. } => {
                    self.buf_of[i] = Some(self.new_buf(BufDef { scope: Scope::Row, fold: Some(list) })?);
                }
                _ if self.cross[i] => {
                    self.buf_of[i] = Some(self.new_buf(BufDef { scope: node.scope, fold: None })?);
                }
                _ => {}
            }
        }
        self.keys = keys;
        Ok(())
    }

    fn new_buf(&mut self, def: BufDef) -> Result<u16, Unsupported> {
        let id = u16::try_from(self.defs.len()).map_err(|_| Unsupported::new("typed: buffers"))?;
        self.defs.push(def);
        Ok(id)
    }

    fn fresh(&mut self) -> Result<R, Unsupported> {
        let r = R::try_from(self.next).map_err(|_| Unsupported::new("typed: registers"))?;
        self.next += 1;
        self.pinned.push(false);
        Ok(r)
    }

    fn emit(&mut self, ins: Ins) {
        self.code[self.cur].push(ins);
    }

    fn konst(&mut self, lit: Lit, rep: Rep) -> Result<R, Unsupported> {
        if rep == Rep::Text {
            return Err(Unsupported::new("typed: text constant as value"));
        }
        if let Some(r) = self.const_of.get(&(lit.clone(), rep)) {
            return Ok(*r);
        }
        let r = self.fresh()?;
        self.pinned[r as usize] = true;
        self.consts.push((r, rep, lit.clone()));
        self.const_of.insert((lit, rep), r);
        Ok(r)
    }

    fn konst_node(&mut self, i: NodeId) -> Result<R, Unsupported> {
        match &self.hir.nodes[i as usize].op {
            Op::Const(lit) => self.konst(lit.clone(), self.rep[i as usize]),
            _ => Err(Unsupported::new("typed: not a constant")),
        }
    }

    fn buf(&self, i: NodeId) -> Result<u16, Unsupported> {
        self.buf_of[i as usize].ok_or_else(|| Unsupported::new("typed: missing buffer"))
    }

    fn slot(k: usize) -> Result<u16, Unsupported> {
        u16::try_from(k).map_err(|_| Unsupported::new("typed: slots"))
    }

    fn emit_all(&mut self) -> Result<(), Unsupported> {
        let hir = self.hir;
        for s in 0..self.keys.len() {
            self.cur = s;
            self.code.push(Vec::new());
            let (level, scope) = self.keys[s];
            for (k, o) in hir.outputs.iter().enumerate() {
                if self.is_const(o.node) && o.scope == scope && Self::const_level(scope) == level {
                    let src = self.konst_node(o.node)?;
                    self.emit(Ins::Out { slot: Self::slot(k)?, src, rep: self.rep[o.node as usize], scope: o.scope });
                }
            }
            for i in 0..hir.nodes.len() {
                if self.stage[i] != s {
                    continue;
                }
                let feeds = self.users[i].iter().any(|&u| self.acc[u as usize] == s);
                if !self.outs[i].is_empty() || self.cross[i] || feeds {
                    self.ensure(i as NodeId)?;
                }
            }
            for f in 0..hir.nodes.len() {
                if self.acc[f] != s {
                    continue;
                }
                let Op::Fold { fold, list, filter, value } = hir.nodes[f].op else {
                    continue;
                };
                let filter = filter.map(|x| self.val(x)).transpose()?;
                let value = match (fold, value) {
                    (Fold::Sum, Some(v)) => Some((self.val(v)?, self.rep[v as usize])),
                    _ => None,
                };
                let buf = self.buf(f as NodeId)?;
                self.emit(Ins::Fold { fold, buf, list, filter, value });
            }
        }
        Ok(())
    }

    fn val(&mut self, i: NodeId) -> Result<R, Unsupported> {
        let at = i as usize;
        if self.is_const(i) {
            return self.konst_node(i);
        }
        if let Some(r) = self.vreg.get(&(self.cur, i)) {
            return Ok(*r);
        }
        if self.stage[at] == self.cur {
            self.ensure(i)?;
            return self.vreg.get(&(self.cur, i)).copied().ok_or_else(|| Unsupported::new("typed: ensure"));
        }
        if self.hir.nodes[at].scope != self.keys[self.cur].1 {
            return Err(Unsupported::new("typed: cross-scope read"));
        }
        let buf = self.buf(i)?;
        let d = self.fresh()?;
        self.emit(Ins::Fill { dst: d, buf, rep: self.rep[at] });
        self.vreg.insert((self.cur, i), d);
        Ok(d)
    }

    fn arg(&self, i: NodeId) -> Result<Arg, Unsupported> {
        let scope = self.keys[self.cur].1;
        match &self.hir.nodes[i as usize].op {
            Op::Column(c) if self.rep[i as usize] == Rep::Text && scope == Scope::Row => Ok(Arg::Src(Src::Col(*c))),
            Op::Field(l, f) if self.rep[i as usize] == Rep::Text && scope == Scope::Elem(*l) => Ok(Arg::Src(Src::Field(*l, *f))),
            Op::Const(Lit::Text(s)) => Ok(Arg::Lit(s.clone())),
            _ => Err(Unsupported::new("typed: text argument")),
        }
    }

    fn cost(&self, i: usize) -> u32 {
        let rep = self.rep[i];
        match &self.hir.nodes[i].op {
            Op::Const(_) => 0,
            Op::Arith(Arith::Div, _, b) => match self.konst_num(*b).and_then(Dec::power) {
                Some(_) => 4,
                None => 30,
            },
            Op::Arith(Arith::Mul, ..) if rep != Rep::Dyn => 2,
            Op::Arith(..) | Op::Neg(_) if rep == Rep::Dyn => 6,
            Op::Call(..) => 4,
            _ => 1,
        }
    }

    fn excl(&self, x: NodeId, r: NodeId) -> Vec<NodeId> {
        let ok = |m: usize| {
            !matches!(self.hir.nodes[m].op, Op::Const(_))
                && self.stage[m] == self.cur
                && self.outs[m].is_empty()
                && !self.cross[m]
                && self.rep[m] != Rep::Text
                && !self.vreg.contains_key(&(self.cur, m as NodeId))
                && !self.users[m].is_empty()
        };
        let r = r as usize;
        if !ok(r) || self.users[r].iter().any(|&u| u != x) {
            return Vec::new();
        }
        let mut mark = vec![false; self.hir.nodes.len()];
        mark[r] = true;
        let mut set = vec![r as NodeId];
        for m in (0..r).rev() {
            if ok(m) && self.users[m].iter().all(|&u| mark[u as usize]) {
                mark[m] = true;
                set.push(m as NodeId);
            }
        }
        set.reverse();
        set
    }

    fn arms(&mut self, x: NodeId, guards: Vec<(Guard, NodeId)>) -> Result<Vec<R>, Unsupported> {
        let mut plans = Vec::with_capacity(guards.len());
        for (g, r) in guards {
            let set = match self.passes.skip {
                true => self.excl(x, r),
                false => Vec::new(),
            };
            let cost: u32 = set.iter().map(|m| self.cost(*m as usize)).sum();
            let skip = !set.is_empty() && cost >= self.passes.skip_min;
            plans.push((g, r, set, skip));
        }
        for (_, _, set, skip) in &plans {
            if !*skip {
                continue;
            }
            for &m in set {
                for inp in self.hir.nodes[m as usize].op.inputs() {
                    if !set.contains(&inp) && !self.is_const(inp) && self.rep[inp as usize] != Rep::Text {
                        self.val(inp)?;
                    }
                }
            }
        }
        let mut regs = Vec::with_capacity(plans.len());
        for (g, r, _, skip) in plans {
            if skip {
                let at = self.code[self.cur].len();
                self.emit(Ins::Skip { guard: g, len: 0 });
                let v = self.val(r)?;
                let len = u32::try_from(self.code[self.cur].len() - at - 1).map_err(|_| Unsupported::new("typed: skip"))?;
                if let Some(Ins::Skip { len: l, .. }) = self.code[self.cur].get_mut(at) {
                    *l = len;
                }
                regs.push(v);
            } else {
                regs.push(self.val(r)?);
            }
        }
        Ok(regs)
    }

    fn mul_const(&self, arm: NodeId) -> Option<(NodeId, Dec)> {
        let (x, c) = match self.hir.nodes[arm as usize].op {
            Op::Arith(Arith::Mul, a, b) => match (self.konst_num(a), self.konst_num(b)) {
                (None, Some(c)) => (a, c),
                (Some(c), None) => (b, c),
                _ => return None,
            },
            _ => return None,
        };
        matches!(self.rep[x as usize], Rep::Fix(_)).then_some((x, c))
    }

    fn pick_plan(&self, arms: &[NodeId]) -> Vec<(Arm, Vec<u16>)> {
        let mut nodes: Vec<(NodeId, Vec<u16>)> = Vec::new();
        for (k, a) in arms.iter().enumerate() {
            let k = k as u16;
            match nodes.iter_mut().find(|g| g.0 == *a) {
                Some(g) => g.1.push(k),
                None => nodes.push((*a, vec![k])),
            }
        }
        if !self.passes.factor || arms.len() >= Tab::MAX {
            return nodes.into_iter().map(|(n, idx)| (Arm::Node(n), idx)).collect();
        }
        let lit = |n: NodeId| matches!(&self.hir.nodes[n as usize].op, Op::Const(l) if !matches!(l, Lit::Text(_)));
        let mut plan = Vec::new();
        if nodes.iter().filter(|(n, _)| lit(*n)).count() >= 2 {
            let mut idx: Vec<u16> = nodes.iter().filter(|(n, _)| lit(*n)).flat_map(|(_, i)| i.clone()).collect();
            idx.sort_unstable();
            nodes.retain(|(n, _)| !lit(*n));
            plan.push((Arm::Tab, idx));
        }
        let mut xs: Vec<NodeId> = Vec::new();
        for (n, _) in &nodes {
            if let Some((x, _)) = self.mul_const(*n) {
                if !xs.contains(&x) && nodes.iter().filter(|(m, _)| self.mul_const(*m).map(|p| p.0) == Some(x)).count() >= 2 {
                    xs.push(x);
                }
            }
        }
        for x in xs {
            let mut idx: Vec<u16> = nodes.iter().filter(|(m, _)| self.mul_const(*m).map(|p| p.0) == Some(x)).flat_map(|(_, i)| i.clone()).collect();
            idx.sort_unstable();
            nodes.retain(|(m, _)| self.mul_const(*m).map(|p| p.0) != Some(x));
            plan.push((Arm::Mul(x), idx));
        }
        nodes.into_iter().map(|(n, idx)| (Arm::Node(n), idx)).chain(plan).collect()
    }

    fn table(&self, arms: &[NodeId], idx: &[u16], filler: bool) -> (Tab, Rep) {
        let mut entries: Vec<Option<Lit>> = vec![None; arms.len() + 1];
        let mut rep = Rep::Null;
        let mut first = None;
        for &k in idx {
            let lit = match &self.hir.nodes[arms[k as usize] as usize].op {
                Op::Const(l) => Some(l.clone()),
                _ => self.mul_const(arms[k as usize]).map(|(_, c)| Lit::Num(c)),
            };
            if let Some(l) = &lit {
                rep = Rep::join(rep, Rep::of_lit(l)).unwrap_or(Rep::Dyn);
                first.get_or_insert_with(|| l.clone());
            }
            entries[k as usize] = lit;
        }
        if filler {
            for e in entries.iter_mut().take(arms.len()) {
                if e.is_none() {
                    *e = first.clone();
                }
            }
        }
        (Tab::new(&entries), rep)
    }

    fn needs_sel(&self, i: NodeId) -> bool {
        let at = i as usize;
        self.sel_need[at]
            || !self.passes.elide
            || !self.outs[at].is_empty()
            || self.cross[at]
            || self.users[at].iter().any(|&u| match &self.hir.nodes[u as usize].op {
                Op::Pick(s, arms) => *s != i || arms.contains(&i),
                _ => true,
            })
    }

    fn arith(&mut self, op: Arith, a: NodeId, b: NodeId, rep: Rep) -> Result<R, Unsupported> {
        let (ra, rb) = (self.rep[a as usize], self.rep[b as usize]);
        let power = match op {
            Arith::Div => self.konst_num(b).and_then(|d| d.power().map(|k| (k, d.scale))),
            _ => None,
        };
        let va = self.val(a)?;
        let d = self.fresh()?;
        let ins = match (op, rep, power) {
            (Arith::Add, Rep::Fix(_), _) => Ins::Fix { op: FixOp::Add, dst: d, a: va, b: self.val(b)? },
            (Arith::Sub, Rep::Fix(_), _) => Ins::Fix { op: FixOp::Sub, dst: d, a: va, b: self.val(b)? },
            (Arith::Mul, Rep::Fix(_), _) => Ins::Fix { op: FixOp::Mul, dst: d, a: va, b: self.val(b)? },
            (Arith::Div, _, Some((k, by))) => Ins::DivPow { dst: d, a: va, ra, k, by },
            _ => Ins::Num { op, dst: d, a: va, ra, b: self.val(b)?, rb },
        };
        self.emit(ins);
        Ok(d)
    }

    fn select(&self, t: NodeId, e: NodeId, rep: Rep) -> Option<(i64, i64)> {
        let lit = |n: NodeId| match (&self.hir.nodes[n as usize].op, rep) {
            (Op::Const(Lit::Num(d)), Rep::Fix(s)) if d.scale == s => Some(d.mant),
            (Op::Const(Lit::Code(_, c)), Rep::Code) => Some(*c as i64),
            (Op::Const(Lit::Bool(b)), Rep::Bool) => Some(*b as i64),
            _ => None,
        };
        lit(t).zip(lit(e))
    }

    fn flip(op: Cmp) -> Cmp {
        match op {
            Cmp::Lt => Cmp::Gt,
            Cmp::Le => Cmp::Ge,
            Cmp::Gt => Cmp::Lt,
            Cmp::Ge => Cmp::Le,
            other => other,
        }
    }

    fn cmp_num(&mut self, op: Cmp, a: NodeId, b: NodeId) -> Result<Ins, Unsupported> {
        let (ra, rb) = (self.rep[a as usize], self.rep[b as usize]);
        if self.passes.fold {
            if let Some(k) = self.konst_num(b) {
                let va = self.val(a)?;
                let (tab, fits) = K::table(k);
                return Ok(Ins::CmpNumK { op, dst: self.fresh()?, a: va, ra, k, tab, fits });
            }
            if let Some(k) = self.konst_num(a) {
                let vb = self.val(b)?;
                let (tab, fits) = K::table(k);
                return Ok(Ins::CmpNumK { op: Self::flip(op), dst: self.fresh()?, a: vb, ra: rb, k, tab, fits });
            }
        }
        let (va, vb) = (self.val(a)?, self.val(b)?);
        Ok(Ins::CmpNum { op, dst: self.fresh()?, a: va, ra, b: vb, rb })
    }

    fn cmp(&mut self, op: Cmp, a: NodeId, b: NodeId) -> Result<R, Unsupported> {
        let (ra, rb) = (self.rep[a as usize], self.rep[b as usize]);
        let eq = matches!(op, Cmp::Eq | Cmp::Ne);
        let ne = op == Cmp::Ne;
        let up = |x: Option<Dec>, s: u8| x.and_then(|d| d.upscale(s));
        let ins = match (ra, rb) {
            (Rep::Fix(x), Rep::Fix(y)) if x == y => {
                let (va, vb) = (self.val(a)?, self.val(b)?);
                Ins::CmpFix { op, dst: self.fresh()?, a: va, b: vb }
            }
            (Rep::Fix(x), Rep::Fix(y)) if self.passes.fold && x < y && up(self.konst_num(a), y).is_some() => {
                let u = up(self.konst_num(a), y).ok_or_else(|| Unsupported::new("typed: upscale"))?;
                let k = self.konst(Lit::Num(u), Rep::Fix(y))?;
                let vb = self.val(b)?;
                Ins::CmpFix { op, dst: self.fresh()?, a: k, b: vb }
            }
            (Rep::Fix(x), Rep::Fix(y)) if self.passes.fold && y < x && up(self.konst_num(b), x).is_some() => {
                let u = up(self.konst_num(b), x).ok_or_else(|| Unsupported::new("typed: upscale"))?;
                let k = self.konst(Lit::Num(u), Rep::Fix(x))?;
                let va = self.val(a)?;
                Ins::CmpFix { op, dst: self.fresh()?, a: va, b: k }
            }
            (x, y) if x.num() && y.num() => self.cmp_num(op, a, b)?,
            (Rep::Code, Rep::Code) if eq => {
                let (va, vb) = (self.val(a)?, self.val(b)?);
                Ins::EqCode { ne, dst: self.fresh()?, a: va, b: vb }
            }
            (Rep::Bool, Rep::Bool) if eq => {
                let (va, vb) = (self.val(a)?, self.val(b)?);
                Ins::EqBool { ne, dst: self.fresh()?, a: va, b: vb }
            }
            (Rep::Text, Rep::Text) if eq => Ins::EqText { ne, dst: self.fresh()?, a: self.arg(a)?, b: self.arg(b)? },
            (Rep::Text, _) | (_, Rep::Text) => return Err(Unsupported::new("typed: text compare")),
            _ => {
                let (va, vb) = (self.val(a)?, self.val(b)?);
                Ins::CmpGen { op, dst: self.fresh()?, a: va, ra, b: vb, rb }
            }
        };
        let d = ins.dst().ok_or_else(|| Unsupported::new("typed: compare"))?;
        self.emit(ins);
        Ok(d)
    }

    fn pick(&mut self, i: NodeId, sel: NodeId, arms: &[NodeId], rep: Rep) -> Result<R, Unsupported> {
        if !matches!(self.hir.nodes[sel as usize].op, Op::FirstMatch { .. }) || self.stage[sel as usize] != self.cur {
            return Err(Unsupported::new("typed: pick without local firstmatch"));
        }
        let vs = self.val(sel)?;
        let plan = self.picks.get(&i).cloned().unwrap_or_default();
        if let [(Arm::Tab, idx)] = plan.as_slice() {
            if idx.len() == arms.len() {
                let (tab, _) = self.table(arms, idx, false);
                let d = self.fresh()?;
                self.emit(Ins::PickTab { dst: d, sel: vs, tab, rep });
                return Ok(d);
            }
        }
        let mut out: Vec<(R, Rep, Vec<u16>)> = Vec::new();
        let mut guards = Vec::new();
        for (arm, idx) in &plan {
            match arm {
                Arm::Node(n) => guards.push((Guard::Masks(vs, idx.clone()), *n, idx.clone())),
                Arm::Tab => {
                    let (tab, tr) = self.table(arms, idx, false);
                    let t = self.fresh()?;
                    self.emit(Ins::PickTab { dst: t, sel: vs, tab, rep: tr });
                    out.push((t, tr, idx.clone()));
                }
                Arm::Mul(x) => {
                    let (tab, tr) = self.table(arms, idx, true);
                    let t = self.fresh()?;
                    self.emit(Ins::PickTab { dst: t, sel: vs, tab, rep: tr });
                    let vx = self.val(*x)?;
                    let d = self.fresh()?;
                    let mr = match (self.rep[*x as usize], tr) {
                        (Rep::Fix(0), Rep::Fix(0)) => {
                            self.emit(Ins::Fix { op: FixOp::Mul, dst: d, a: vx, b: t });
                            Rep::Fix(0)
                        }
                        (rx, _) => {
                            self.emit(Ins::Num { op: Arith::Mul, dst: d, a: vx, ra: rx, b: t, rb: tr });
                            Rep::Dyn
                        }
                    };
                    out.push((d, mr, idx.clone()));
                }
            }
        }
        let regs = self.arms(i, guards.iter().map(|(g, n, _)| (g.clone(), *n)).collect())?;
        for ((_, n, idx), r) in guards.into_iter().zip(regs) {
            out.push((r, self.rep[n as usize], idx));
        }
        let d = self.fresh()?;
        self.emit(Ins::Pick { dst: d, sel: vs, arms: out, rep });
        Ok(d)
    }

    fn ensure(&mut self, i: NodeId) -> Result<(), Unsupported> {
        let cur = self.cur;
        if self.vreg.contains_key(&(cur, i)) {
            return Ok(());
        }
        let hir = self.hir;
        let at = i as usize;
        let node = &hir.nodes[at];
        let rep = self.rep[at];
        let rof = |b: &Build, x: &NodeId| b.rep[*x as usize];
        let v = match &node.op {
            Op::Const(_) => self.konst_node(i)?,
            Op::Column(_) | Op::Field(..) if rep == Rep::Text => return Err(Unsupported::new("typed: text value")),
            Op::Column(c) => {
                let d = self.fresh()?;
                self.emit(Ins::Load { dst: d, src: Src::Col(*c), rep });
                d
            }
            Op::Field(l, f) => {
                let d = self.fresh()?;
                self.emit(Ins::Load { dst: d, src: Src::Field(*l, *f), rep });
                d
            }
            Op::Classify(..) => {
                let k = self.classes.get(&i).copied().ok_or_else(|| Unsupported::new("typed: classify"))?;
                let d = self.fresh()?;
                self.emit(Ins::Load { dst: d, src: Src::Class(k), rep });
                d
            }
            Op::Parent(a) if self.is_const(*a) => self.konst_node(*a)?,
            Op::Parent(a) => {
                let Scope::Elem(list) = node.scope else {
                    return Err(Unsupported::new("typed: parent outside element scope"));
                };
                let buf = self.buf(*a)?;
                let d = self.fresh()?;
                self.emit(Ins::Gather { dst: d, buf, list, rep });
                d
            }
            Op::Fold { .. } => {
                let buf = self.buf(i)?;
                let d = self.fresh()?;
                self.emit(Ins::Fill { dst: d, buf, rep });
                d
            }
            Op::Neg(a) => {
                let va = self.val(*a)?;
                let d = self.fresh()?;
                self.emit(Ins::Neg { dst: d, a: va, rep });
                d
            }
            Op::Not(a) => {
                let va = self.val(*a)?;
                let d = self.fresh()?;
                self.emit(Ins::Not { dst: d, a: va });
                d
            }
            Op::Arith(op, a, b) => self.arith(*op, *a, *b, rep)?,
            Op::Cmp(op, a, b) => self.cmp(*op, *a, *b)?,
            Op::And(a, b) | Op::Or(a, b) => {
                let and = matches!(node.op, Op::And(..));
                let va = self.val(*a)?;
                let g = if and { Guard::True(va) } else { Guard::False(va) };
                let vb = self.arms(i, vec![(g, *b)])?.first().copied().ok_or_else(|| Unsupported::new("typed: arm"))?;
                let d = self.fresh()?;
                self.emit(if and { Ins::And { dst: d, a: va, b: vb } } else { Ins::Or { dst: d, a: va, b: vb } });
                d
            }
            Op::If(c, t, e) if self.passes.fold && self.select(*t, *e, rep).is_some() => {
                let (tv, ev) = self.select(*t, *e, rep).ok_or_else(|| Unsupported::new("typed: select"))?;
                let vc = self.val(*c)?;
                let d = self.fresh()?;
                self.emit(Ins::SelK { dst: d, c: vc, t: tv, e: ev, rep });
                d
            }
            Op::If(c, t, e) => {
                let vc = self.val(*c)?;
                let vs = self.arms(i, vec![(Guard::True(vc), *t), (Guard::False(vc), *e)])?;
                let (rt, re) = (rof(self, t), rof(self, e));
                let d = self.fresh()?;
                self.emit(Ins::Blend { dst: d, c: vc, t: vs[0], rt, e: vs[1], re, rep });
                d
            }
            Op::Coalesce(a, b) => {
                let va = self.val(*a)?;
                let vb = self.arms(i, vec![(Guard::Null(va), *b)])?.first().copied().ok_or_else(|| Unsupported::new("typed: arm"))?;
                let (ra, rb) = (rof(self, a), rof(self, b));
                let d = self.fresh()?;
                self.emit(Ins::Coalesce { dst: d, a: va, ra, b: vb, rb, rep });
                d
            }
            Op::InSet(a, set) => {
                let ra = rof(self, a);
                let va = self.val(*a)?;
                let mut bits = 0u64;
                let mut null = false;
                let mut fast = ra == Rep::Code;
                for lit in set {
                    match lit {
                        Lit::Code(_, c) if (0..64).contains(c) => bits |= 1u64 << c,
                        Lit::Null => null = true,
                        _ => fast = false,
                    }
                }
                let d = self.fresh()?;
                match fast {
                    true => self.emit(Ins::InCode { dst: d, a: va, set: bits, null }),
                    false if ra == Rep::Text => return Err(Unsupported::new("typed: text in-set")),
                    false => self.emit(Ins::InGen { dst: d, a: va, ra, set: set.clone() }),
                }
                d
            }
            Op::InRange(a, r) => {
                let ra = rof(self, a);
                let va = self.val(*a)?;
                let d = self.fresh()?;
                let fixed = match (ra, self.passes.fold) {
                    (Rep::Fix(s), true) => r.lo.upscale(s).zip(r.hi.upscale(s)),
                    _ => None,
                };
                match fixed {
                    Some((lo, hi)) => {
                        let lo = if r.lo_closed { Some(lo.mant) } else { lo.mant.checked_add(1) };
                        let hi = if r.hi_closed { Some(hi.mant) } else { hi.mant.checked_sub(1) };
                        let (lo, span, empty) = match lo.zip(hi) {
                            Some((l, h)) if h >= l => (l, h.wrapping_sub(l) as u64, false),
                            _ => (0, 0, true),
                        };
                        self.emit(Ins::InRangeFix { dst: d, a: va, lo, span, empty })
                    }
                    None => self.emit(Ins::InRangeGen { dst: d, a: va, ra, range: *r }),
                }
                d
            }
            Op::FirstMatch { conds, strict } => {
                let mut vs = Vec::with_capacity(conds.len());
                for c in conds {
                    vs.push(self.val(*c)?);
                }
                let sel = self.needs_sel(i);
                let d = self.fresh()?;
                self.emit(Ins::First { dst: d, conds: vs, sel, strict: *strict });
                d
            }
            Op::Pick(sel, arms) => self.pick(i, *sel, arms, rep)?,
            Op::Call(func, args) => {
                let a = self.arg(*args.first().ok_or_else(|| Unsupported::new("typed: call"))?)?;
                let b = args.get(1).map(|x| self.arg(*x)).transpose()?;
                let d = self.fresh()?;
                self.emit(Ins::Text { func: *func, dst: d, a, b });
                d
            }
        };
        self.vreg.insert((cur, i), v);
        if self.cross[at] && !matches!(node.op, Op::Fold { .. }) {
            let buf = self.buf(i)?;
            self.emit(Ins::Spill { src: v, buf, rep });
        }
        for k in self.outs[at].clone() {
            self.emit(Ins::Out { slot: Self::slot(k)?, src: v, rep, scope: hir.outputs[k].scope });
        }
        Ok(())
    }

    fn finish(self) -> Result<Built, Unsupported> {
        let Build { mut code, keys, next, pinned, consts, defs, passes, .. } = self;
        let mut phys: Vec<R> = (0..next).map(|v| v as R).collect();
        let mut nphys = next;
        if passes.reuse {
            let mut top = 0usize;
            for (v, p) in pinned.iter().enumerate() {
                if *p {
                    phys[v] = top as R;
                    top += 1;
                }
            }
            let base = top;
            let mut most = base;
            for stage in &code {
                let mut last = vec![usize::MAX; next];
                for (pc, ins) in stage.iter().enumerate() {
                    let mut copy = ins.clone();
                    copy.regs(&mut |r, def| {
                        if !def {
                            last[*r as usize] = pc;
                        }
                    });
                }
                let mut free: Vec<R> = Vec::new();
                let mut top = base;
                for (pc, ins) in stage.iter().enumerate() {
                    let mut uses = Vec::new();
                    let mut defs = Vec::new();
                    let mut copy = ins.clone();
                    copy.regs(&mut |r, def| if def { defs.push(*r) } else { uses.push(*r) });
                    for d in defs {
                        let p = match free.pop() {
                            Some(p) => p,
                            None => {
                                top += 1;
                                R::try_from(top - 1).map_err(|_| Unsupported::new("typed: registers"))?
                            }
                        };
                        phys[d as usize] = p;
                        if last[d as usize] == usize::MAX {
                            free.push(p);
                        }
                    }
                    uses.sort_unstable();
                    uses.dedup();
                    for u in uses {
                        if !pinned[u as usize] && last[u as usize] == pc {
                            free.push(phys[u as usize]);
                        }
                    }
                }
                most = most.max(top);
            }
            nphys = most;
        }
        for stage in &mut code {
            for ins in stage.iter_mut() {
                ins.regs(&mut |r, _| *r = phys[*r as usize]);
            }
        }
        let consts = consts.into_iter().map(|(v, rep, lit)| (phys[v as usize], rep, lit)).collect();
        let stages = code.into_iter().zip(keys).map(|(code, (_, scope))| Stage { scope, code }).collect();
        Ok(Built { stages, nregs: nphys, consts, defs })
    }
}
