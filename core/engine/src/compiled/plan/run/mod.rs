use crate::compiled::data::Data;
use zen_expression::Variable;
use crate::compiled::plan::census::Census;
use crate::compiled::plan::{Layout, Op, Program, SegEnd, SegPlan};
use crate::decision_graph::walker::GraphWalker;
use crate::nodes::NodeHandlerExtensions;
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::CompiledGraph;
use crate::model::GraphContent;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::LaneRunner;
use store::{Shredded, Store};

mod assemble;
mod fanout;
mod gather;
mod lane;
mod loops;
mod rows;
mod store;
mod table;
mod typed;

struct Host<'r> {
    content: &'r Arc<GraphContent>,
    extensions: &'r NodeHandlerExtensions,
    max_depth: u8,
}

struct Run<'r, 'a> {
    graph: &'r CompiledGraph,
    layout: &'r Layout,
    runner: LaneRunner,
    host: Host<'r>,
    store: Store<'a>,
    settled: bool,
    failures: Vec<(usize, usize)>,
    shreds: Vec<(Leaf<'a>, Shredded<'a>)>,
}

impl<'a> Run<'_, 'a> {
    fn alive(&self, positions: Option<Rc<[usize]>>, seen: &mut usize) -> Option<Rc<[usize]>> {
        if self.store.failed == *seen {
            return positions;
        }
        *seen = self.store.failed;
        let rows = self.store.rows;
        let errors = &self.store.errors;
        let total = positions.as_ref().map_or(rows, |p| p.len());
        let live = match &positions {
            Some(p) => p.iter().filter(|&&row| errors[row].is_none()).count(),
            None => errors.iter().filter(|error| error.is_none()).count(),
        };
        match (live + live / 7 < total, live == rows) {
            (true, _) => {
                let mut kept = Vec::with_capacity(live);
                match &positions {
                    Some(p) => kept.extend(p.iter().copied().filter(|&row| errors[row].is_none())),
                    None => kept.extend((0..rows).filter(|&row| errors[row].is_none())),
                }
                Some(kept.into())
            }
            (false, true) => None,
            (false, false) => positions,
        }
    }

    async fn walk(&mut self, root: Arc<SegPlan>) -> Result<(), String> {
        let mut stack: Vec<(Arc<SegPlan>, Option<Rc<[usize]>>, Option<(usize, usize)>)> = vec![(root, None, None)];
        let mut children = Vec::new();
        while let Some((seg, positions, parent)) = stack.pop() {
            if let Some(parent) = parent {
                self.store.rewind(parent);
            }
            self.segment(&seg, positions, &mut children).await?;
            let parent = Some((seg.slots.len(), seg.masks.exprs.len()));
            stack.extend(children.drain(..).rev().map(|(child, members)| (child, members, parent)));
        }
        Ok(())
    }

    async fn segment(&mut self, seg: &SegPlan, positions: Option<Rc<[usize]>>, children: &mut Vec<(Arc<SegPlan>, Option<Rc<[usize]>>)>) -> Result<(), String> {
        self.store.enter(seg);
        let mut seen = 0usize;
        let mut live = self.alive(positions, &mut seen);
        let mut positions = live.as_ref();
        for op in &seg.ops {
            if self.store.failed != seen {
                live = self.alive(live, &mut seen);
                positions = live.as_ref();
            }
            match op {
                Op::Lane(lane) => {
                    if matches!(lane.program, Program::Expression) {
                        self.store.counts.1 += 1;
                    }
                    self.lane(seg, lane, positions, true);
                }
                Op::Table(table) => {
                    self.store.counts.1 += 1;
                    self.table(seg, table, positions);
                }
                Op::Typed { op, units } => self.typed(seg, op, units, positions),
                Op::Deferred { op, units } => self.deferred(seg, op, units, positions),
                Op::Rows(op) => {
                    Census::begin();
                    self.rows(seg, op, positions).await;
                    if Census::on() {
                        self.counted(op, positions);
                    }
                }
                Op::Loop { node, list, out } => self.looped(seg, *node, *list, *out, positions),
                Op::Join { inputs, out, strict, ending } => {
                    let leaves: Vec<Leaf<'a>> = inputs.iter().map(|r| self.store.leaf(r.slot)).collect();
                    let mut values = vec![Variable::Null; self.store.rows];
                    let mut parts = Vec::with_capacity(leaves.len());
                    let mut arrays = false;
                    let at: Box<dyn Iterator<Item = usize>> = match positions {
                        Some(positions) => Box::new(positions.iter().copied()),
                        None => Box::new(0..self.store.rows),
                    };
                    for row in at {
                        parts.clear();
                        parts.extend(leaves.iter().map(|leaf| leaf.get(row)));
                        let value = match ending {
                            true => GraphWalker::merge_ending(parts.iter()),
                            false => GraphWalker::merge_values(parts.iter()),
                        };
                        arrays |= matches!(value, Variable::Array(_));
                        values[row] = value;
                    }
                    if *strict && arrays {
                        return Err("array input".into());
                    }
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
                Op::Select { a, b, take, out } => {
                    let (a, b) = (self.store.value(seg, *a), self.store.value(seg, *b));
                    let take = self.store.mask(seg, *take);
                    if Bits::none(&take) || Rc::ptr_eq(&take, &self.store.ones) || *take == *self.store.ones {
                        let chosen = match Bits::none(&take) {
                            true => a,
                            false => b,
                        };
                        self.store.slots[*out as usize] = Some(chosen);
                        continue;
                    }
                    self.store.slots[*out as usize] = Some(Self::chosen(&a, &b, &take, self.store.rows));
                }
                Op::Materialize { node, out } => {
                    let shape = self.store.shape(seg, node);
                    let values = Store::objects(&shape, self.store.rows);
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
                Op::MergeRows { a, b, parts, present, out } => {
                    let (sa, sb) = (self.store.shape(seg, a), self.store.shape(seg, b));
                    let [rr, rb, ra] = parts.map(|m| self.store.mask(seg, m));
                    let rows = self.store.rows;
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    let values: Vec<Variable> = (0..rows)
                        .map(|row| {
                            let value = Store::merged(&sa, &sb, Bits::get(&rr, row), Bits::get(&rb, row), Bits::get(&ra, row), row);
                            if value.is_some() {
                                Bits::set(&mut bits, row, true);
                            }
                            value.unwrap_or(Variable::Null)
                        })
                        .collect();
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                    self.store.produce(*present, bits.into());
                }
                Op::Null { out } => {
                    self.store.slots[*out as usize] = Some(Leaf::nulls(self.store.rows));
                }
                Op::Coalesce { from, value, out } => {
                    let leaf = self.store.value(seg, *from);
                    let array = Self::coalesced(&leaf.column(), value, self.store.rows);
                    self.store.slots[*out as usize] = Some(Leaf::typed(array));
                }
                Op::Extract { from, path, out } => {
                    let leaf = self.store.value(seg, *from);
                    let path = path.iter().map(|s| s.as_ref()).collect::<Vec<_>>().join(".");
                    let values: Vec<Variable> = (0..self.store.rows)
                        .map(|row| Data::lookup(&leaf.get(row), &path).unwrap_or(Variable::Null))
                        .collect();
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
            }
        }
        if self.store.failed != seen {
            live = self.alive(live, &mut seen);
            positions = live.as_ref();
        }
        match &seg.end {
            SegEnd::Finish(refs) if matches!(refs.as_slice(), [(path, _)] if path.is_empty()) => {
                let leaves = self.flattened(refs[0].1, positions);
                self.store.groups.push((positions.cloned(), leaves));
                Ok(())
            }
            SegEnd::Finish(refs) => {
                let leaves = refs
                    .iter()
                    .map(|(path, r)| {
                        let base = self.store.leaf(r.slot);
                        let presence = (!Store::implied(seg, *r)).then(|| self.store.mask(seg, r.present));
                        (path.clone(), base, presence)
                    })
                    .collect();
                self.store.groups.push((positions.cloned(), leaves));
                Ok(())
            }
            SegEnd::Switch { first, conditions, .. } => self.fan_out(seg, positions, *first, conditions, children),
        }
    }

    fn unit(&mut self, seg: &SegPlan, op: &Op, positions: Option<&Rc<[usize]>>) {
        match op {
            Op::Lane(lane) => self.lane(seg, lane, positions, true),
            Op::Table(table) => self.table(seg, table, positions),
            _ => {}
        }
    }
}
