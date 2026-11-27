use crate::context::EvaluationContext;
use crate::functions::InternalFunction;
use crate::lane::{Bits, Column, Values};
use crate::typed::batch::{Batch, Owned, OwnedData};
use crate::typed::hir::{
    Clock, ColId, ColumnDef, Func, Hir, Host, Kind, Node, NodeId, Op, Output, Schema, Scope,
};
use crate::typed::kernel::K;
use crate::typed::output::{Data, Slot};
use crate::typed::program::Program;
use ahash::{AHashMap, AHashSet};

pub(crate) struct Derived {
    pub(crate) source: ColId,
    pub(crate) program: Program,
    pub(crate) columns: Vec<ColId>,
    heavy: bool,
}

pub(crate) struct Split {
    pub(crate) main: Program,
    pub(crate) derived: Vec<Derived>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Src {
    Free,
    One(ColId),
    Many,
}

struct Entries {
    keys: Vec<i32>,
    offsets: Vec<i32>,
    data: Vec<u8>,
    valid: Vec<u64>,
}

impl Split {
    const ROWS: usize = 32;
    const SAMPLE: usize = 128;
    const WIDE: usize = 512;

    pub(crate) fn of(hir: &Hir, build: impl Fn(Hir) -> Option<Program>) -> Option<Self> {
        if hir.nodes.is_empty() {
            return None;
        }
        let n = hir.nodes.len();
        let mut src = vec![Src::Free; n];
        let mut costly = vec![false; n];
        let mut heavy = vec![false; n];
        for (i, node) in hir.nodes.iter().enumerate() {
            let inputs = node.op.inputs();
            let own = match &node.op {
                _ if node.scope != Scope::Row => Src::Many,
                Op::Column(c) => match hir.schema.columns.get(*c as usize) {
                    Some(def) if def.ty.kind == Kind::Text => Src::One(*c),
                    _ => Src::Many,
                },
                Op::Const(_) => Src::Free,
                Op::Field(..)
                | Op::Parent(_)
                | Op::Fold { .. }
                | Op::Clock(Clock::Now, _)
                | Op::Host(Host::Internal(InternalFunction::Rand), _) => Src::Many,
                _ => inputs.iter().fold(Src::Free, |acc, x| {
                    match (acc, src.get(*x as usize).copied().unwrap_or(Src::Many)) {
                        (Src::Many, _) | (_, Src::Many) => Src::Many,
                        (Src::Free, other) | (other, Src::Free) => other,
                        (Src::One(a), Src::One(b)) if a == b => Src::One(a),
                        _ => Src::Many,
                    }
                }),
            };
            src[i] = own;
            let weighty = matches!(
                node.op,
                Op::Host(..) | Op::Clock(..) | Op::Call(Func::Matches, _)
            );
            let priced = weighty || matches!(node.op, Op::Call(..) | Op::Str(_) | Op::Concat(_));
            costly[i] = priced || inputs.iter().any(|x| costly[*x as usize]);
            heavy[i] = weighty || inputs.iter().any(|x| heavy[*x as usize]);
        }
        let pickable: Vec<bool> = (0..n)
            .map(|i| {
                let node = &hir.nodes[i];
                matches!(src[i], Src::One(_))
                    && costly[i]
                    && !matches!(node.op, Op::Column(_))
                    && matches!(
                        node.ty.kind,
                        Kind::Bool | Kind::Num(_) | Kind::Text | Kind::Code(_)
                    )
            })
            .collect();
        let mut external = vec![false; n];
        for o in hir.outputs.iter().chain(&hir.parts) {
            external[o.node as usize] = true;
            if let Some(c) = o.collect {
                external[c.item as usize] = true;
                if let Some(k) = c.keep {
                    external[k as usize] = true;
                }
            }
        }
        for (at, node) in hir.nodes.iter().enumerate() {
            for x in node.op.inputs() {
                if !pickable[at] || src[at] != src[x as usize] {
                    external[x as usize] = true;
                }
            }
        }
        let picked: Vec<NodeId> = (0..n)
            .filter(|&i| pickable[i] && external[i])
            .map(|i| i as NodeId)
            .collect();
        if picked.is_empty() {
            return None;
        }
        let mut sources: Vec<ColId> = Vec::new();
        for &i in &picked {
            if let Src::One(c) = src[i as usize] {
                if !sources.contains(&c) {
                    sources.push(c);
                }
            }
        }
        let mut main = hir.clone();
        let mut derived = Vec::with_capacity(sources.len());
        for source in sources {
            let mine: Vec<NodeId> = picked
                .iter()
                .copied()
                .filter(|&i| src[i as usize] == Src::One(source))
                .collect();
            let sub = Self::sub(hir, source, &mine)?;
            let program = build(sub)?;
            let mut columns = Vec::with_capacity(mine.len());
            for &i in &mine {
                let c = ColId::try_from(main.schema.columns.len()).ok()?;
                main.schema.columns.push(ColumnDef {
                    path: format!("\u{0}{i}"),
                    ty: hir.nodes[i as usize].ty,
                });
                main.nodes[i as usize].op = Op::Column(c);
                columns.push(c);
            }
            derived.push(Derived {
                source,
                program,
                columns,
                heavy: mine.iter().any(|&i| heavy[i as usize]),
            });
        }
        let main = build(main.pruned())?;
        Some(Self { main, derived })
    }

    fn sub(hir: &Hir, source: ColId, picked: &[NodeId]) -> Option<Hir> {
        let n = hir.nodes.len();
        let mut live = vec![false; n];
        for &i in picked {
            live[i as usize] = true;
        }
        for i in (0..n).rev() {
            if live[i] {
                for x in hir.nodes[i].op.inputs() {
                    live[x as usize] = true;
                }
            }
        }
        let mut remap = vec![0 as NodeId; n];
        let mut nodes = Vec::new();
        for (i, node) in hir.nodes.iter().enumerate().filter(|(i, _)| live[*i]) {
            remap[i] = NodeId::try_from(nodes.len()).ok()?;
            let op = match &node.op {
                Op::Column(c) if *c == source => Op::Column(0),
                Op::Column(_) => return None,
                op => op.remap(|x| remap[x as usize]),
            };
            nodes.push(Node {
                op,
                ty: node.ty,
                scope: node.scope,
            });
        }
        let outputs = picked
            .iter()
            .map(|&i| Output {
                name: String::new(),
                node: remap[i as usize],
                scope: Scope::Row,
                ty: hir.nodes[i as usize].ty,
                collect: None,
                shape: None,
            })
            .collect();
        Some(Hir {
            schema: Schema {
                columns: vec![hir.schema.columns.get(source as usize)?.clone()],
                lists: Vec::new(),
                dicts: hir.schema.dicts.clone(),
            },
            nodes,
            outputs,
            parts: Vec::new(),
        })
    }

    pub(crate) fn derive(
        &self,
        batch: &Batch,
        select: Option<&[u64]>,
        context: &EvaluationContext,
    ) -> Option<Vec<Owned>> {
        let rows = batch.rows();
        if rows < Self::ROWS {
            return None;
        }
        let coded: Vec<Option<Entries>> = self
            .derived
            .iter()
            .map(|d| {
                let column = batch.column_at(d.source as usize)?;
                let limit = match d.heavy {
                    true => rows / 2,
                    false => rows / 4,
                };
                let sample = match d.heavy {
                    true => (Self::WIDE, Self::WIDE / 16),
                    false => (Self::SAMPLE, Self::SAMPLE / 2),
                };
                Entries::code(column, rows, limit, sample)
            })
            .collect();
        if coded.iter().all(Option::is_none) {
            return None;
        }
        let mut owned = Vec::new();
        for (d, entries) in self.derived.iter().zip(coded) {
            let mut out = d.program.outputs();
            match &entries {
                Some(entries) => {
                    let len = entries.offsets.len() - 1;
                    let column = Column::with_validity(
                        Values::Utf8 {
                            offsets: &entries.offsets,
                            data: &entries.data,
                        },
                        &entries.valid,
                        0,
                    );
                    let sub = Batch::new(len).column(column);
                    d.program.run(&sub, &mut out, None, context).ok()?;
                }
                None => {
                    let column = batch.column_at(d.source as usize)?;
                    let sub = Batch::new(rows).column(column);
                    d.program.run(&sub, &mut out, select, context).ok()?;
                }
            }
            let keys = entries.as_ref().map(|e| e.keys.as_slice());
            for (slot, column) in out.slots().iter().zip(&d.columns) {
                owned.push(Owned::gathered(*column, slot, keys, rows)?);
            }
        }
        Some(owned)
    }
}

impl Entries {
    fn code(column: Column, rows: usize, limit: usize, sample: (usize, usize)) -> Option<Self> {
        let mut entries = Self {
            keys: Vec::with_capacity(rows),
            offsets: vec![0],
            data: Vec::new(),
            valid: Vec::new(),
        };
        let mut null: Option<i32> = None;
        let push = |entries: &mut Self, bytes: Option<&[u8]>| -> Option<i32> {
            let at = i32::try_from(entries.offsets.len() - 1).ok()?;
            if let Some(bytes) = bytes {
                entries.data.extend_from_slice(bytes);
            }
            entries
                .offsets
                .push(i32::try_from(entries.data.len()).ok()?);
            if entries.valid.len() * 64 <= at as usize {
                entries.valid.push(0);
            }
            if bytes.is_some() {
                Bits::set(&mut entries.valid, at as usize, true);
            }
            Some(at)
        };
        match column.values {
            Values::Dict { keys, values } => {
                let dict = values.column();
                let size = dict.len();
                if size >= limit || keys.len() < rows {
                    return None;
                }
                for at in 0..size {
                    let text = match dict.valid(at) {
                        true => Some(dict.bytes(at)?),
                        false => None,
                    };
                    push(&mut entries, text)?;
                }
                for (row, key) in keys[..rows].iter().enumerate() {
                    let key = match column.valid(row) {
                        false => match null {
                            Some(k) => k,
                            None => {
                                let k = push(&mut entries, None)?;
                                null = Some(k);
                                k
                            }
                        },
                        true => match usize::try_from(*key).ok().filter(|k| *k < size) {
                            Some(k) if dict.valid(k) => k as i32,
                            _ => return None,
                        },
                    };
                    entries.keys.push(key);
                }
            }
            Values::Text { .. } | Values::Utf8 { .. } => {
                if column.len() < rows {
                    return None;
                }
                let (size, spare) = sample;
                let step = (rows / size).max(1);
                let mut seen: AHashSet<&[u8]> = AHashSet::with_capacity(size);
                let mut taken = 0;
                for row in (0..rows).step_by(step).take(size) {
                    taken += 1;
                    if let Some(bytes) = column.valid(row).then(|| column.bytes(row)).flatten() {
                        seen.insert(bytes);
                    }
                }
                if seen.len() + spare > taken {
                    return None;
                }
                let mut index: AHashMap<&[u8], i32> = AHashMap::with_capacity(64);
                for row in 0..rows {
                    let bytes = match column.valid(row) {
                        true => column.bytes(row),
                        false => None,
                    };
                    let key = match bytes {
                        None => match null {
                            Some(k) => k,
                            None => {
                                let k = push(&mut entries, None)?;
                                null = Some(k);
                                k
                            }
                        },
                        Some(bytes) => match index.get(bytes) {
                            Some(k) => *k,
                            None => {
                                let k = push(&mut entries, Some(bytes))?;
                                index.insert(bytes, k);
                                k
                            }
                        },
                    };
                    if entries.offsets.len() > limit {
                        return None;
                    }
                    entries.keys.push(key);
                }
            }
            _ => return None,
        }
        entries.valid.resize(K::words(entries.offsets.len() - 1), 0);
        Some(entries)
    }
}

impl Owned {
    fn gathered(column: ColId, slot: &Slot, keys: Option<&[i32]>, rows: usize) -> Option<Self> {
        let at = |row: usize| -> usize {
            match keys {
                Some(keys) => keys[row] as usize,
                None => row,
            }
        };
        let len = slot.len;
        if keys.is_some_and(|keys| keys.len() < rows || keys.iter().any(|k| *k as usize >= len)) {
            return None;
        }
        let bits = |source: &[u64]| -> Vec<u64> {
            let mut out = vec![0u64; K::words(rows)];
            let lit = |k: usize| (source.get(k >> 6).copied().unwrap_or(0) >> (k & 63)) & 1;
            match keys {
                Some(keys) => {
                    for (word, chunk) in out.iter_mut().zip(keys[..rows].chunks(64)) {
                        *word = chunk
                            .iter()
                            .enumerate()
                            .fold(0u64, |acc, (j, k)| acc | lit(*k as usize) << j);
                    }
                }
                None => {
                    let n = out.len();
                    out.copy_from_slice(&source[..n]);
                    if !rows.is_multiple_of(64) {
                        out[n - 1] &= (1u64 << (rows % 64)) - 1;
                    }
                }
            }
            out
        };
        let valid = bits(&slot.valid);
        let fault = match slot.fault.any() {
            true => bits(slot.fault.words()),
            false => Vec::new(),
        };
        let data = match &slot.data {
            Data::Num { mant, scale } => OwnedData::Num {
                mant: (0..rows).map(|row| mant[at(row)]).collect(),
                scale: (0..rows).map(|row| scale[at(row)]).collect(),
            },
            Data::Bool(source) => OwnedData::Bool(bits(source)),
            Data::Code(codes) => OwnedData::Codes((0..rows).map(|row| codes[at(row)]).collect()),
            Data::Text { span, data } => {
                let mut offsets = Vec::with_capacity(len + 1);
                let mut packed = Vec::new();
                offsets.push(0i32);
                for (entry, (lo, hi)) in span.iter().take(len).enumerate() {
                    if Bits::get(&slot.valid, entry) {
                        packed.extend_from_slice(data.get(*lo as usize..*hi as usize)?);
                    }
                    offsets.push(i32::try_from(packed.len()).ok()?);
                }
                OwnedData::Text {
                    keys: (0..rows).map(|row| at(row) as i32).collect(),
                    offsets,
                    data: packed,
                }
            }
            Data::Date(_) => return None,
        };
        let wide = match slot.wide.is_empty() {
            true => None,
            false => {
                let mut marks = vec![0u64; K::words(rows)];
                let mut values = Vec::new();
                for row in 0..rows {
                    if let Ok(found) = slot.wide.binary_search_by_key(&at(row), |(i, _)| *i) {
                        Bits::set(&mut marks, row, true);
                        values.push((row, slot.wide[found].1));
                    }
                }
                Some((marks, values))
            }
        };
        Some(Owned {
            column,
            data,
            valid,
            fault,
            wide,
        })
    }
}
