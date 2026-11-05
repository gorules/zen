use super::core::canon;
use super::value::{json_len, Key, Obj, Val};
use super::{bad, malformed, Need, R};
use ahash::HashMap;
use std::rc::Rc;

pub const MAX_ITERATIONS: usize = 10_000_000;
pub const MAX_DEPTH: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NestKind {
    Sub,
    Loop,
}

impl NestKind {
    pub fn field(self) -> &'static str {
        match self {
            NestKind::Sub => "sub",
            NestKind::Loop => "loop",
        }
    }
}

pub struct Nest {
    pub kind: NestKind,
    pub ids: Vec<usize>,
    pub timings: Vec<Vec<Val>>,
}

pub struct EncRec {
    pub obj: Obj,
    pub nest: Option<Nest>,
}

pub enum EncTrace {
    Verbatim(Val),
    Nodes(Vec<EncRec>),
}

#[derive(Default)]
pub struct Shapes {
    pub paths: Vec<Val>,
    index: HashMap<String, usize>,
    pub depth: usize,
}

pub const REC_ORDER: [&str; 21] = [
    "id",
    "name",
    "type",
    "perf",
    "failed",
    "error",
    "input",
    "output",
    "results",
    "failedKey",
    "resultsAt",
    "scope",
    "items",
    "rule",
    "rules",
    "rulesPerItem",
    "branches",
    "log",
    "details",
    "sub",
    "loop",
];

pub fn rec_rank(k: &str) -> usize {
    REC_ORDER
        .iter()
        .position(|x| *x == k)
        .unwrap_or(REC_ORDER.len())
}

fn ids_val(ids: &[usize]) -> Val {
    Val::arr(ids.iter().map(|&i| Val::int(i as i64)).collect())
}

impl Nest {
    fn first(&self) -> R<usize> {
        self.ids
            .first()
            .copied()
            .need("a nested trace needs at least one run")
    }

    fn uniform(&self) -> bool {
        self.ids.windows(2).all(|w| w[0] == w[1])
    }

    fn shape(&self) -> R<Obj> {
        let first = self.first()?;
        let mut o = Obj::new();
        if self.kind == NestKind::Sub {
            o.insert(Rc::from("path"), Val::int(first as i64));
            return Ok(o);
        }
        if self.uniform() {
            o.insert(Rc::from("path"), Val::int(first as i64));
            o.insert(Rc::from("iterations"), Val::int(self.ids.len() as i64));
        } else {
            o.insert(Rc::from("iterationPaths"), ids_val(&self.ids));
        }
        Ok(o)
    }
}

pub fn nodes_val(nodes: Vec<Val>) -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("nodes"), Val::arr(nodes));
    Val::obj(o)
}

fn verbatim_val(v: Val) -> Val {
    let mut o = Obj::with_capacity(1);
    o.insert(Rc::from("verbatim"), v);
    Val::obj(o)
}

pub fn is_verbatim(t: &Val) -> bool {
    t.has("verbatim")
}

fn with_nodes(t: &Val, nodes: Vec<Val>) -> Val {
    let mut o = t.as_obj().cloned().unwrap_or_default();
    o.set("nodes", Val::arr(nodes));
    Val::obj(o)
}

fn records(t: &Val) -> &[Val] {
    match t.get("nodes") {
        Some(Val::Arr(a)) => a,
        _ => &[],
    }
}

fn skel_records(skel: &Val) -> &[Val] {
    if !skel.is_obj() || is_verbatim(skel) {
        return &[];
    }
    records(skel)
}

pub fn with_field(r: &Obj, key: &str, v: Val) -> Obj {
    if r.has(key) {
        let mut c = r.clone();
        c.set(key, v);
        return c;
    }
    let rank = rec_rank(key);
    let mut c = Obj::with_capacity(r.len() + 1);
    let mut placed = false;
    for (k, x) in r.iter() {
        if !placed && rec_rank(k) > rank {
            c.insert(Rc::from(key), v.clone());
            placed = true;
        }
        c.insert(k.clone(), x.clone());
    }
    if !placed {
        c.insert(Rc::from(key), v);
    }
    c
}

fn without(r: &Obj, key: &str) -> Obj {
    let mut c = r.clone();
    c.remove(key);
    c
}

fn hoist(t: EncTrace) -> R<(Val, Vec<Val>)> {
    match t {
        EncTrace::Verbatim(v) => Ok((verbatim_val(v), Vec::new())),
        EncTrace::Nodes(nodes) => {
            let mut timings = Vec::new();
            let mut out = Vec::with_capacity(nodes.len());
            for r in nodes {
                let mut c = r.obj;
                if let Some(p @ Val::Num(_)) = c.get("perf").cloned() {
                    c.remove("perf");
                    timings.push(p);
                }
                if let Some(n) = r.nest {
                    c = with_field(&c, n.kind.field(), Val::obj(n.shape()?));
                    let ps = &n.timings;
                    let width = ps.first().map_or(0, |p| p.len());
                    if n.kind == NestKind::Loop
                        && ps.len() > 1
                        && width > 0
                        && ps.iter().all(|p| p.len() == width)
                    {
                        for j in 0..width {
                            for p in ps {
                                timings.extend(p.get(j).cloned());
                            }
                        }
                    } else {
                        for p in ps {
                            timings.extend(p.iter().cloned());
                        }
                    }
                }
                out.push(Val::obj(c));
            }
            Ok((nodes_val(out), timings))
        }
    }
}

fn intern(d: &mut Shapes, skel: Val) -> usize {
    let key = skel.to_json();
    if let Some(&id) = d.index.get(&key) {
        return id;
    }
    let id = d.paths.len();
    d.paths.push(skel);
    d.index.insert(key, id);
    id
}

pub fn nest(d: &mut Shapes, kind: NestKind, subs: Vec<EncTrace>) -> R<Nest> {
    if subs.is_empty() {
        return Err(malformed("a nested trace needs at least one run"));
    }
    let mut ids = Vec::with_capacity(subs.len());
    let mut timings = Vec::with_capacity(subs.len());
    for s in subs {
        let (skel, t) = hoist(s)?;
        ids.push(intern(d, skel));
        timings.push(t);
    }
    Ok(Nest { kind, ids, timings })
}

#[derive(Clone)]
enum ShapeId {
    At(usize),
    Inline(Val),
}

fn shape_id(v: &Val) -> R<ShapeId> {
    match v {
        Val::Num(_) => Ok(ShapeId::At(
            v.as_usize().ok_or_else(|| bad("bad path index"))?,
        )),
        _ => Ok(ShapeId::Inline(v.clone())),
    }
}

fn resolve<'a>(paths: &'a [Val], id: &'a ShapeId) -> R<&'a Val> {
    match id {
        ShapeId::At(i) => paths.get(*i).ok_or_else(|| bad("path index out of range")),
        ShapeId::Inline(v) => Ok(v),
    }
}

fn shape_of(r: &Val) -> Option<(NestKind, &Val, &Obj)> {
    match r.get("loop") {
        Some(v @ Val::Obj(o)) if !o.has("paths") => return Some((NestKind::Loop, v, o)),
        _ => {}
    }
    match r.get("sub") {
        Some(v @ Val::Obj(o)) if o.has("path") => Some((NestKind::Sub, v, o)),
        _ => None,
    }
}

fn sub_path(s: &Val) -> R<ShapeId> {
    shape_id(s.get("path").need("a sub shape needs path")?)
}

fn deeper(depth: usize) -> R<usize> {
    match depth < MAX_DEPTH {
        true => Ok(depth + 1),
        false => Err(malformed("nested shapes are too deep")),
    }
}

fn shape_ids(s: &Val) -> R<Vec<ShapeId>> {
    match s.get("iterationPaths") {
        Some(Val::Arr(a)) => a.iter().map(shape_id).collect(),
        Some(_) => Err(bad("iterationPaths must be a list")),
        None => Ok(vec![shape_id(
            s.get("path").ok_or_else(|| bad("shape needs path"))?,
        )?]),
    }
}

fn iteration_ids(s: &Val) -> R<Vec<ShapeId>> {
    if s.has("iterationPaths") {
        return shape_ids(s);
    }
    let n = s
        .get("iterations")
        .and_then(|x| x.as_usize())
        .filter(|n| *n <= MAX_ITERATIONS)
        .ok_or_else(|| bad("shape needs iterations"))?;
    let p = shape_id(s.get("path").ok_or_else(|| bad("shape needs path"))?)?;
    Ok(vec![p; n])
}

fn written_ids(o: &Obj, n: usize) -> Vec<ShapeId> {
    let one = |v: &Val| shape_id(v).unwrap_or(ShapeId::Inline(v.clone()));
    match (o.get("iterationPaths"), o.get("path")) {
        (Some(Val::Arr(a)), _) => a.iter().map(one).collect(),
        (_, Some(p)) => vec![one(p); n],
        _ => Vec::new(),
    }
}

fn group_key(id: &ShapeId) -> String {
    match id {
        ShapeId::At(i) => format!("#{i}"),
        ShapeId::Inline(v) => {
            let mut s = String::new();
            canon(v, &mut s);
            s
        }
    }
}

fn to_cols(ids: &[ShapeId], rows: &[Vec<Val>]) -> Vec<Vec<Vec<Val>>> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&Vec<Val>>> = HashMap::default();
    for (id, row) in ids.iter().zip(rows) {
        let k = group_key(id);
        if !groups.contains_key(&k) {
            order.push(k.clone());
        }
        groups.entry(k).or_default().push(row);
    }
    order
        .iter()
        .filter_map(|k| groups.get(k))
        .map(|g| {
            (0..g.first().map_or(0, |r| r.len()))
                .map(|j| {
                    g.iter()
                        .map(|r| r.get(j).cloned().unwrap_or(Val::Null))
                        .collect()
                })
                .collect()
        })
        .collect()
}

fn from_cols(ids: &[ShapeId], groups: &Val) -> R<Vec<Vec<Val>>> {
    let groups = groups
        .as_arr()
        .ok_or_else(|| bad("timingColumns must be a list"))?;
    let mut gi: HashMap<String, usize> = HashMap::default();
    let keys: Vec<String> = ids.iter().map(group_key).collect();
    for k in &keys {
        let n = gi.len();
        gi.entry(k.clone()).or_insert(n);
    }
    let mut seen = vec![0usize; gi.len()];
    let mut out = Vec::with_capacity(ids.len());
    for k in &keys {
        let g = *gi.get(k).need("timingColumns group")?;
        let slot = seen.get_mut(g).need("timingColumns group")?;
        let t = *slot;
        *slot += 1;
        let cols = groups
            .get(g)
            .and_then(|x| x.as_arr())
            .ok_or_else(|| bad("timingColumns group"))?;
        let mut row = Vec::with_capacity(cols.len());
        for c in cols.iter() {
            let c = c.as_arr().ok_or_else(|| bad("timingColumns column"))?;
            row.push(
                c.get(t)
                    .cloned()
                    .ok_or_else(|| bad("timingColumns too short"))?,
            );
        }
        out.push(row);
    }
    Ok(out)
}

fn rows_val(rows: &[Vec<Val>]) -> Val {
    Val::arr(rows.iter().map(|r| Val::arr(r.clone())).collect())
}

fn cols_val(cols: &[Vec<Vec<Val>>]) -> Val {
    Val::arr(
        cols.iter()
            .map(|g| Val::arr(g.iter().map(|c| Val::arr(c.clone())).collect()))
            .collect(),
    )
}

pub const COLUMNS_FROM: usize = 16;

fn timing_table(ids: &[ShapeId], rows: &[Vec<Val>]) -> (&'static str, Val) {
    let cols = to_cols(ids, rows);
    if cols
        .iter()
        .any(|g| g.first().map_or(0, |c| c.len()) >= COLUMNS_FROM)
    {
        ("timingColumns", cols_val(&cols))
    } else {
        ("timings", rows_val(rows))
    }
}

fn rows_of(s: &Val, ids: &[ShapeId]) -> R<Vec<Vec<Val>>> {
    if let Some(c) = s.get("timingColumns") {
        return from_cols(ids, c);
    }
    let t = s
        .get("timings")
        .and_then(|x| x.as_arr())
        .ok_or_else(|| bad("loop needs timings"))?;
    t.iter()
        .map(|r| {
            r.as_arr()
                .map(|a| a.to_vec())
                .ok_or_else(|| bad("timing row must be a list"))
        })
        .collect()
}

struct Cursor {
    row: Vec<Val>,
    at: usize,
}

impl Cursor {
    fn new(row: Vec<Val>) -> Self {
        Self { row, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.row.len().saturating_sub(self.at)
    }

    fn next(&mut self) -> R<Val> {
        let v = self
            .row
            .get(self.at)
            .cloned()
            .ok_or_else(|| bad("timing row too short"))?;
        self.at += 1;
        Ok(v)
    }
}

fn strip_timings(t: &Val) -> Option<(Val, Vec<Val>)> {
    if !t.is_obj() || is_verbatim(t) {
        return None;
    }
    let mut timings = Vec::new();
    let nodes = records(t)
        .iter()
        .map(|r| match (r, r.get("perf")) {
            (Val::Obj(o), Some(p @ Val::Num(_))) => {
                timings.push(p.clone());
                Val::obj(without(o, "perf"))
            }
            _ => r.clone(),
        })
        .collect();
    Some((with_nodes(t, nodes), timings))
}

fn fill_timings(shape: &Val, row: Vec<Val>) -> R<Val> {
    let mut next = Cursor::new(row);
    let mut nodes = Vec::new();
    for r in records(shape) {
        if r.has("perf") {
            nodes.push(r.clone());
        } else {
            let o = r.as_obj().ok_or_else(|| bad("record must be an object"))?;
            nodes.push(Val::obj(with_field(o, "perf", next.next()?)));
        }
    }
    Ok(with_nodes(shape, nodes))
}

pub fn loop_pack(runs: Vec<Val>) -> Val {
    if runs.len() < 2 {
        return Val::arr(runs);
    }
    let mut parts = Vec::with_capacity(runs.len());
    for r in &runs {
        match strip_timings(r) {
            Some(p) => parts.push(p),
            None => return Val::arr(runs),
        }
    }
    let mut paths = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::default();
    let mut ids = Vec::with_capacity(parts.len());
    for (shape, _) in &parts {
        let key = shape.to_json();
        let id = match index.get(&key) {
            Some(&i) => i,
            None => {
                index.insert(key, paths.len());
                paths.push(shape.clone());
                paths.len() - 1
            }
        };
        ids.push(id);
    }
    let mut packed = Obj::new();
    let single = paths.len() == 1;
    packed.insert(Rc::from("paths"), Val::arr(paths));
    if single {
        packed.insert(Rc::from("iterations"), Val::int(runs.len() as i64));
    } else {
        packed.insert(Rc::from("iterationPaths"), ids_val(&ids));
    }
    let sids: Vec<ShapeId> = ids.iter().map(|&i| ShapeId::At(i)).collect();
    let rows: Vec<Vec<Val>> = parts.into_iter().map(|(_, t)| t).collect();
    let (k, v) = timing_table(&sids, &rows);
    packed.insert(Rc::from(k), v);
    let packed = Val::obj(packed);
    let runs = Val::arr(runs);
    if json_len(&packed) < json_len(&runs) {
        packed
    } else {
        runs
    }
}

fn loop_unpack(x: &Val) -> R<Vec<Val>> {
    let paths = x
        .get("paths")
        .and_then(|p| p.as_arr())
        .ok_or_else(|| bad("loop paths must be a list"))?;
    let ids: Vec<usize> = match x.get("iterationPaths") {
        Some(Val::Arr(a)) => a
            .iter()
            .map(|i| i.as_usize().ok_or_else(|| bad("bad iteration path")))
            .collect::<R<_>>()?,
        Some(_) => return Err(bad("iterationPaths must be a list")),
        None => vec![
            0;
            x.get("iterations")
                .and_then(|n| n.as_usize())
                .filter(|n| *n <= MAX_ITERATIONS)
                .ok_or_else(|| bad("loop needs iterations"))?
        ],
    };
    let sids: Vec<ShapeId> = ids.iter().map(|&i| ShapeId::At(i)).collect();
    let rows = rows_of(x, &sids)?;
    let mut out = Vec::with_capacity(ids.len());
    for (i, id) in ids.iter().enumerate() {
        let shape = paths
            .get(*id)
            .ok_or_else(|| bad("iteration path out of range"))?;
        let row = rows
            .get(i)
            .cloned()
            .ok_or_else(|| bad("timings too short"))?;
        out.push(fill_timings(shape, row)?);
    }
    Ok(out)
}

fn timing_count(paths: &[Val], id: &ShapeId, depth: usize) -> R<usize> {
    let depth = deeper(depth)?;
    let skel = resolve(paths, id)?;
    let mut n: usize = 0;
    for r in skel_records(skel) {
        if !r.has("perf") {
            n = n.saturating_add(1);
        }
        match shape_of(r) {
            Some((NestKind::Loop, s, _)) => {
                for c in iteration_ids(s)? {
                    n = n.saturating_add(timing_count(paths, &c, depth)?);
                }
            }
            Some((NestKind::Sub, s, _)) => {
                n = n.saturating_add(timing_count(paths, &sub_path(s)?, depth)?);
            }
            None => {}
        }
    }
    Ok(n)
}

fn expand_nested(paths: &[Val], s: &Val, next: &mut Cursor, depth: usize) -> R<Vec<Val>> {
    let ids = iteration_ids(s)?;
    let lens: Vec<usize> = ids
        .iter()
        .map(|id| timing_count(paths, id, depth))
        .collect::<R<_>>()?;
    let width = lens.first().copied().unwrap_or(0);
    if ids.len() < 2 || width == 0 || lens.iter().any(|&l| l != width) {
        return ids
            .iter()
            .map(|id| rebuild(paths, id, next, depth))
            .collect();
    }
    if width.saturating_mul(ids.len()) > next.remaining() {
        return Err(bad("timing row too short"));
    }
    let mut rows: Vec<Vec<Val>> = vec![Vec::with_capacity(width); ids.len()];
    for _ in 0..width {
        for r in rows.iter_mut() {
            r.push(next.next()?);
        }
    }
    ids.iter()
        .zip(rows)
        .map(|(id, row)| rebuild(paths, id, &mut Cursor::new(row), depth))
        .collect()
}

fn rebuild(paths: &[Val], id: &ShapeId, next: &mut Cursor, depth: usize) -> R<Val> {
    let depth = deeper(depth)?;
    let skel = resolve(paths, id)?;
    if !skel.is_obj() || is_verbatim(skel) {
        return Ok(skel.clone());
    }
    let recs = records(skel);
    let mut nodes = Vec::with_capacity(recs.len());
    for r in recs {
        let o = r.as_obj().ok_or_else(|| bad("record must be an object"))?;
        let mut c = if o.has("perf") {
            o.clone()
        } else {
            with_field(o, "perf", next.next()?)
        };
        match shape_of(r) {
            Some((NestKind::Loop, s, _)) => {
                let l = expand_nested(paths, s, next, depth)?;
                c.set("loop", Val::arr(l));
            }
            Some((NestKind::Sub, s, _)) => {
                let p = sub_path(s)?;
                c.set("sub", rebuild(paths, &p, next, depth)?);
            }
            None => {}
        }
        nodes.push(Val::obj(c));
    }
    Ok(with_nodes(skel, nodes))
}

fn flatten(t: Val) -> Val {
    if !t.is_obj() || is_verbatim(&t) {
        return t;
    }
    let nodes = records(&t)
        .iter()
        .map(|r| {
            let Val::Obj(ro) = r else {
                return r.clone();
            };
            if let Some(Val::Arr(l)) = ro.get("loop") {
                let packed = loop_pack(l.iter().cloned().map(flatten).collect());
                return Val::obj(with_field(ro, "loop", packed));
            }
            if let Some(s @ Val::Obj(_)) = ro.get("sub") {
                let f = flatten(s.clone());
                return Val::obj(with_field(ro, "sub", f));
            }
            r.clone()
        })
        .collect();
    with_nodes(&t, nodes)
}

fn flat_form(d: &Shapes, n: &Nest) -> (&'static str, Val) {
    let run = |id: usize, t: Option<&Vec<Val>>| {
        let mut c = Cursor::new(t.cloned().unwrap_or_default());
        flatten(rebuild(&d.paths, &ShapeId::At(id), &mut c, 0).unwrap_or(Val::Null))
    };
    if n.kind == NestKind::Sub {
        let t = match n.ids.first() {
            Some(&id) => run(id, n.timings.first()),
            None => Val::Null,
        };
        return ("sub", t);
    }
    let runs = n
        .ids
        .iter()
        .enumerate()
        .map(|(i, &id)| run(id, n.timings.get(i)))
        .collect();
    ("loop", loop_pack(runs))
}

struct Finalizer<'a> {
    d: &'a Shapes,
    uses: Vec<usize>,
    shared: Vec<Option<Val>>,
    slot: HashMap<usize, usize>,
    hit_shared: bool,
}

impl<'a> Finalizer<'a> {
    fn path(&self, id: usize) -> R<&'a Val> {
        self.d.paths.get(id).need("shape path out of range")
    }

    fn visit(&mut self, id: usize, reach: &mut [bool]) -> R<()> {
        let skel = self.path(id)?;
        if let Some(u) = self.uses.get_mut(id) {
            *u += 1;
        }
        match reach.get_mut(id) {
            Some(seen) if !*seen => *seen = true,
            _ => return Ok(()),
        }
        for r in skel_records(skel) {
            if let Some((_, s, _)) = shape_of(r) {
                for c in shape_ids(s).unwrap_or_default() {
                    if let ShapeId::At(c) = c {
                        self.visit(c, reach)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn reference(&mut self, id: usize, depth: usize) -> R<Val> {
        let depth = deeper(depth)?;
        let uses = self.uses.get(id).copied().unwrap_or(0);
        if uses <= 1 || json_len(self.path(id)?) * (uses - 1) <= 2 * uses + 12 {
            return self.materialize(id, depth);
        }
        self.hit_shared = true;
        if let Some(&s) = self.slot.get(&id) {
            return Ok(Val::int(s as i64));
        }
        let s = self.shared.len();
        self.slot.insert(id, s);
        self.shared.push(None);
        let m = self.materialize(id, depth)?;
        if let Some(slot) = self.shared.get_mut(s) {
            *slot = Some(m);
        }
        Ok(Val::int(s as i64))
    }

    fn shape_out(&mut self, s: &Obj, depth: usize) -> R<Obj> {
        let mut out = s.clone();
        if let Some(Val::Arr(ip)) = s.get("iterationPaths") {
            let mapped = ip
                .iter()
                .map(|i| self.reference(i.as_usize().unwrap_or(0), depth))
                .collect::<R<_>>()?;
            out.set("iterationPaths", Val::arr(mapped));
        } else if let Some(p) = s.get("path") {
            let r = self.reference(p.as_usize().unwrap_or(0), depth)?;
            out.set("path", r);
        }
        Ok(out)
    }

    fn materialize(&mut self, id: usize, depth: usize) -> R<Val> {
        let skel = self.path(id)?;
        if !skel.is_obj() || is_verbatim(skel) {
            return Ok(skel.clone());
        }
        let nodes = records(skel)
            .iter()
            .map(|r| match (r, shape_of(r)) {
                (Val::Obj(ro), Some((k, _, s))) => {
                    let so = self.shape_out(s, depth)?;
                    Ok(Val::obj(with_field(ro, k.field(), Val::obj(so))))
                }
                _ => Ok(r.clone()),
            })
            .collect::<R<_>>()?;
        Ok(with_nodes(skel, nodes))
    }
}

fn finalize(nodes: &[EncRec], d: &Shapes, force_flat: bool) -> R<(Vec<Val>, Vec<Val>)> {
    let mut flats: HashMap<usize, (&'static str, Val)> = HashMap::default();
    if force_flat {
        for (i, r) in nodes.iter().enumerate() {
            if let Some(n) = &r.nest {
                flats.insert(i, flat_form(d, n));
            }
        }
    }
    let mut f = Finalizer {
        d,
        uses: vec![0; d.paths.len()],
        shared: Vec::new(),
        slot: HashMap::default(),
        hit_shared: false,
    };
    let mut reach = vec![false; d.paths.len()];
    for (i, r) in nodes.iter().enumerate() {
        if let Some(n) = &r.nest {
            if !flats.contains_key(&i) {
                let uniform = n.uniform();
                for id in n.ids.iter() {
                    if n.kind == NestKind::Sub || !uniform {
                        f.visit(*id, &mut reach)?;
                    }
                }
                if n.kind == NestKind::Loop && uniform {
                    f.visit(n.first()?, &mut reach)?;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(nodes.len());
    for (i, r) in nodes.iter().enumerate() {
        let Some(n) = &r.nest else {
            out.push(Val::obj(r.obj.clone()));
            continue;
        };
        let (field, value) = if let Some((k, v)) = flats.remove(&i) {
            (k, v)
        } else {
            f.hit_shared = false;
            let mut o = f.shape_out(&n.shape()?, 0)?;
            if n.kind == NestKind::Sub {
                o.insert(Rc::from("timings"), rows_val(&n.timings));
                ("sub", Val::obj(o))
            } else {
                let (k, v) = timing_table(&written_ids(&o, n.ids.len()), &n.timings);
                o.insert(Rc::from(k), v);
                let o = Val::obj(o);
                if f.hit_shared {
                    ("loop", o)
                } else {
                    let (_, flat) = flat_form(d, n);
                    if json_len(&flat) < json_len(&o) {
                        ("loop", flat)
                    } else {
                        ("loop", o)
                    }
                }
            }
        };
        out.push(Val::obj(with_field(&r.obj, field, value)));
    }
    let shared = f
        .shared
        .into_iter()
        .map(|x| x.unwrap_or(Val::Null))
        .collect();
    Ok((out, shared))
}

fn body_len(nodes: &[Val], shared: &[Val]) -> usize {
    let mut n = 2 + 8 + json_len(&Val::arr(nodes.to_vec()));
    if !shared.is_empty() {
        n += 9 + json_len(&Val::arr(shared.to_vec()));
    }
    n
}

pub fn deep_or_flat(nodes: Vec<EncRec>, d: &Shapes) -> R<(Vec<Val>, Vec<Val>)> {
    if d.paths.is_empty() {
        return Ok((
            nodes.into_iter().map(|r| Val::obj(r.obj)).collect(),
            Vec::new(),
        ));
    }
    let deep = finalize(&nodes, d, false)?;
    let flat = finalize(&nodes, d, true)?;
    let fl = body_len(&flat.0, &flat.1);
    let dl = body_len(&deep.0, &deep.1);
    if dl + 64 < fl && (dl as f64) < fl as f64 * 0.95 {
        Ok(deep)
    } else {
        Ok(flat)
    }
}

pub fn loop_in(x: &Val, paths: &[Val]) -> R<Vec<Val>> {
    match x {
        Val::Arr(a) => Ok(a.to_vec()),
        Val::Obj(o) if o.has("paths") => loop_unpack(x),
        Val::Obj(_) => {
            let ids = iteration_ids(x)?;
            let rows = rows_of(x, &ids)?;
            ids.iter()
                .enumerate()
                .map(|(i, id)| {
                    rebuild(
                        paths,
                        id,
                        &mut Cursor::new(rows.get(i).cloned().unwrap_or_default()),
                        0,
                    )
                })
                .collect()
        }
        _ => Err(bad("loop must be a list or a shape")),
    }
}

pub fn sub_in(x: &Val, paths: &[Val]) -> R<Val> {
    let Some(path) = x.get("path") else {
        return Ok(x.clone());
    };
    let id = shape_id(path)?;
    let row = x
        .get("timings")
        .and_then(|t| t.as_arr())
        .and_then(|t| t.first())
        .and_then(|r| r.as_arr())
        .map(|r| r.to_vec())
        .ok_or_else(|| bad("sub needs timings"))?;
    rebuild(paths, &id, &mut Cursor::new(row), 0)
}

pub fn tidy_trace(t: &Val) -> Val {
    match t {
        Val::Obj(o) => {
            let mut out = Obj::with_capacity(o.len());
            for (k, v) in o.iter() {
                let nv = match (&**k, v) {
                    ("nodes", Val::Arr(a)) => Val::arr(a.iter().map(tidy_record).collect()),
                    ("sub" | "loop" | "paths" | "path" | "iterationPaths", _) => tidy_trace(v),
                    _ => v.clone(),
                };
                out.insert(k.clone(), nv);
            }
            Val::obj(out)
        }
        Val::Arr(a) => Val::arr(a.iter().map(tidy_trace).collect()),
        _ => t.clone(),
    }
}

pub fn tidy_record(r: &Val) -> Val {
    let Val::Obj(o) = r else { return r.clone() };
    let tidy = o
        .keys()
        .zip(o.keys().skip(1))
        .all(|(a, b)| rec_rank(a) <= rec_rank(b))
        && !o.has("sub")
        && !o.has("loop");
    if tidy {
        return r.clone();
    }
    let mut keys: Vec<(&Key, &Val)> = o.iter().collect();
    keys.sort_by_key(|(k, _)| rec_rank(k));
    let mut out = Obj::with_capacity(o.len());
    for (k, v) in keys {
        let nv = if &**k == "sub" || &**k == "loop" {
            tidy_trace(v)
        } else {
            v.clone()
        };
        out.insert(k.clone(), nv);
    }
    Val::obj(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(fields: Vec<(&str, Val)>) -> Obj {
        let mut o = Obj::new();
        for (k, v) in fields {
            o.insert(Rc::from(k), v);
        }
        o
    }

    fn node(id: &str, kind: &str, perf: i64, extra: Vec<(&'static str, Val)>) -> Obj {
        let mut fields = vec![
            ("id", Val::str(id)),
            ("type", Val::str(kind)),
            ("perf", Val::int(perf)),
        ];
        fields.extend(extra);
        rec(fields)
    }

    fn plain(nodes: &[Obj]) -> EncTrace {
        EncTrace::Nodes(
            nodes
                .iter()
                .map(|o| EncRec {
                    obj: o.clone(),
                    nest: None,
                })
                .collect(),
        )
    }

    fn permuted(i: usize) -> Obj {
        let mut keys = vec!["p", "q", "r", "s"];
        let mut out = Vec::new();
        let mut i = i;
        while !keys.is_empty() {
            let k = keys.remove(i % keys.len());
            i /= keys.len() + 1;
            out.push((k, Val::int(k.len() as i64)));
        }
        rec(out)
    }

    fn iteration(d: &mut Shapes, order: usize, t: &mut impl FnMut() -> i64) -> (EncTrace, Val) {
        let screened = || Val::obj(rec(vec![("care.screened", Val::Bool(true))]));
        let inner = [
            node("a", "input", t(), vec![]),
            node("b", "expression", t(), vec![("results", screened())]),
        ];
        let results = permuted(order);
        let row = node("row", "input", t(), vec![]);
        let out = node(
            "out",
            "expression",
            t(),
            vec![("results", Val::obj(results))],
        );
        let call = node("call", "decision", t(), vec![]);
        let inner_val = nodes_val(inner.iter().cloned().map(Val::obj).collect());
        let want = nodes_val(vec![
            Val::obj(row.clone()),
            Val::obj(out.clone()),
            Val::obj(with_field(&call, "sub", inner_val)),
        ]);
        let sub = nest(d, NestKind::Sub, vec![plain(&inner)]).unwrap();
        let enc = EncTrace::Nodes(vec![
            EncRec {
                obj: row,
                nest: None,
            },
            EncRec {
                obj: out,
                nest: None,
            },
            EncRec {
                obj: call,
                nest: Some(sub),
            },
        ]);
        (enc, want)
    }

    #[test]
    fn timing_columns_group_iterations_as_written() {
        for timings in [[7i64, 7, 7, 7, 7], [250, 2200, 9600, 1000, 625]] {
            let mut d = Shapes::default();
            let mut at = 0usize;
            let mut t = || {
                at += 1;
                timings[at % timings.len()] + (at as i64 % 3)
            };
            let mut subs = Vec::new();
            let mut want = Vec::new();
            for i in 0..COLUMNS_FROM + 2 {
                let (enc, v) = iteration(&mut d, i, &mut t);
                subs.push(enc);
                want.push(v);
            }
            let top = EncRec {
                obj: node("loop", "decision", 1, vec![]),
                nest: Some(nest(&mut d, NestKind::Loop, subs).unwrap()),
            };
            let (out, shared) = finalize(&[top], &d, false).unwrap();
            let lp = out[0].get("loop").unwrap();
            assert!(lp.has("timingColumns"), "{}", lp.to_json());
            let paths = lp.get("iterationPaths").and_then(|p| p.as_arr()).unwrap();
            assert!(paths.iter().all(|p| p.is_obj()));
            let back = loop_in(lp, &shared).unwrap();
            assert_eq!(
                Val::arr(back).to_json(),
                Val::arr(want).to_json(),
                "{}",
                lp.to_json()
            );
        }
    }
}
