use crate::compiled::census::Census;
use crate::compiled::graph::data::Data;
use crate::compiled::graph::plan::typed::TypedOp;
use crate::compiled::graph::plan::{JoinMode, Layout, Op, Program, SegEnd, SegPlan};
use crate::compiled::graph::Host;
use crate::compiled::graph::{CompiledGraph, Completed};
use crate::compiled::leaf::{Bits, Leaf};
use crate::decision_graph::walker::GraphWalker;
use std::rc::Rc;
use std::sync::Arc;
use store::{Branch, Group, Mark, Pending, Shredded, Store};
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;

mod assemble;
pub(crate) use assemble::PlanOutcome;
mod child;
mod fanout;
mod gather;
mod lane;
mod loops;
mod rows;
mod store;
mod table;
mod typed;

type Parents<'r> = &'r dyn Fn(usize) -> Option<Variable>;

struct Run<'r, 'a> {
    graph: &'r CompiledGraph,
    layout: &'r Layout,
    runner: LaneRunner,
    host: Host<'r>,
    store: Store<'a>,
    settled: bool,
    failures: Vec<(usize, usize)>,
    shreds: Vec<(Leaf<'a>, Shredded<'a>)>,
    completed: Completed,
    iteration: u8,
    parents: Option<Parents<'r>>,
}

impl<'a> Run<'_, 'a> {
    fn alive(&self, positions: Option<Rc<[usize]>>, seen: &mut usize) -> Option<Rc<[usize]>> {
        self.store.errors.alive(positions, seen)
    }

    async fn walk(&mut self, root: Arc<SegPlan>) {
        let mut stack: Vec<Pending> = vec![Pending {
            branch: Branch {
                seg: root,
                positions: None,
            },
            parent: None,
        }];
        let mut children = Vec::new();
        while let Some(Pending { branch, parent }) = stack.pop() {
            if let Some(parent) = parent {
                self.store.rewind(parent);
            }
            let Branch { seg, positions } = branch;
            self.segment(&seg, positions, &mut children).await;
            let parent = Some(Mark {
                slots: seg.slots.len(),
                masks: seg.masks.exprs.len(),
            });
            stack.extend(
                children
                    .drain(..)
                    .rev()
                    .map(|branch| Pending { branch, parent }),
            );
        }
    }

    async fn segment(
        &mut self,
        seg: &SegPlan,
        positions: Option<Rc<[usize]>>,
        children: &mut Vec<Branch>,
    ) {
        self.store.enter(seg);
        let mut seen = 0usize;
        let mut live = self.alive(positions, &mut seen);
        let mut positions = live.as_ref();
        for op in &seg.ops {
            if self.store.errors.generation() != seen {
                live = self.alive(live, &mut seen);
                positions = live.as_ref();
            }
            match op {
                Op::Lane(lane) => {
                    if matches!(lane.program, Program::Expression) {
                        self.store.counts.1 += 1;
                    }
                    match Census::on() {
                        true => self.timed(seg, op, positions),
                        false => self.lane(seg, lane, positions, true),
                    }
                }
                Op::Table(table) => {
                    self.store.counts.1 += 1;
                    match Census::on() {
                        true => self.timed(seg, op, positions),
                        false => self.table(seg, table, positions),
                    }
                }
                Op::Typed { op, units } => match Census::on() {
                    true => self.grouped(seg, op, units, positions),
                    false => self.typed(seg, op, units, positions),
                },
                Op::Deferred { op, units } => self.deferred(seg, op, units, positions),
                Op::Rows(op) => {
                    Census::begin();
                    self.rows(seg, op, positions).await;
                    if Census::on() {
                        self.counted(op, positions);
                    }
                }
                Op::Loop { node, list, out } => self.looped(seg, *node, *list, *out, positions),
                Op::Child(op) => {
                    Census::begin();
                    self.child(seg, op, positions).await;
                    if Census::on() {
                        self.measured(positions);
                    }
                }
                Op::Join { inputs, out, mode } => {
                    let leaves: Vec<Leaf<'a>> =
                        inputs.iter().map(|r| self.store.leaf(r.slot)).collect();
                    let mut values = vec![Variable::Null; self.store.rows];
                    let mut parts = Vec::with_capacity(leaves.len());
                    let mut arrays = Vec::new();
                    let at: Box<dyn Iterator<Item = usize>> = match positions {
                        Some(positions) => Box::new(positions.iter().copied()),
                        None => Box::new(0..self.store.rows),
                    };
                    for row in at {
                        parts.clear();
                        parts.extend(leaves.iter().map(|leaf| leaf.get(row)));
                        let value = match mode {
                            JoinMode::Ending => GraphWalker::merge_ending(parts.iter()),
                            JoinMode::Strict | JoinMode::Lenient => {
                                GraphWalker::merge_values(parts.iter())
                            }
                        };
                        if *mode == JoinMode::Strict && matches!(value, Variable::Array(_)) {
                            arrays.push(row);
                        }
                        values[row] = value;
                    }
                    for row in arrays {
                        self.store.errors.defer(row);
                    }
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
                Op::Select { a, b, take, out } => {
                    let (a, b) = (self.store.value(seg, *a), self.store.value(seg, *b));
                    let take = self.store.mask(seg, *take);
                    if Bits::none(&take)
                        || Rc::ptr_eq(&take, &self.store.ones)
                        || *take == *self.store.ones
                    {
                        let chosen = match Bits::none(&take) {
                            true => a,
                            false => b,
                        };
                        self.store.slots[*out as usize] = Some(chosen);
                        continue;
                    }
                    self.store.slots[*out as usize] =
                        Some(Self::chosen(&a, &b, &take, self.store.rows));
                }
                Op::Materialize { node, out } => {
                    let shape = self.store.shape(seg, node);
                    let values = Store::objects(&shape, self.store.rows);
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
                Op::MergeRows {
                    a,
                    b,
                    parts,
                    present,
                    out,
                } => {
                    let (sa, sb) = (self.store.shape(seg, a), self.store.shape(seg, b));
                    let [rr, rb, ra] = parts.map(|m| self.store.mask(seg, m));
                    let rows = self.store.rows;
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    let values: Vec<Variable> = (0..rows)
                        .map(|row| {
                            let value = Store::merged(
                                &sa,
                                &sb,
                                Bits::get(&rr, row),
                                Bits::get(&rb, row),
                                Bits::get(&ra, row),
                                row,
                            );
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
                    let path = path
                        .iter()
                        .map(|s| s.as_ref())
                        .collect::<Vec<_>>()
                        .join(".");
                    let values: Vec<Variable> = (0..self.store.rows)
                        .map(|row| Data::lookup(&leaf.get(row), &path).unwrap_or(Variable::Null))
                        .collect();
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
            }
        }
        if self.store.errors.generation() != seen {
            live = self.alive(live, &mut seen);
            positions = live.as_ref();
        }
        match &seg.end {
            SegEnd::Finish(_) if self.iteration > 0 => {}
            SegEnd::Finish(refs) if matches!(refs.as_slice(), [(path, _)] if path.is_empty()) => {
                let leaves = self.flattened(refs[0].1, positions);
                self.store.groups.push(Group {
                    positions: positions.cloned(),
                    leaves,
                });
            }
            SegEnd::Finish(refs) => {
                let leaves = refs
                    .iter()
                    .map(|(path, r)| {
                        let base = self.store.leaf(r.slot);
                        let presence =
                            (!Store::implied(seg, *r)).then(|| self.store.mask(seg, r.present));
                        (path.clone(), base, presence)
                    })
                    .collect();
                self.store.groups.push(Group {
                    positions: positions.cloned(),
                    leaves,
                });
            }
            SegEnd::Switch {
                first, conditions, ..
            } => self.fan_out(seg, positions, *first, conditions, children),
        }
    }

    #[cold]
    #[inline(never)]
    fn timed(&mut self, seg: &SegPlan, op: &Op, positions: Option<&Rc<[usize]>>) {
        let start = std::time::Instant::now();
        self.unit(seg, op, positions);
        let rows = positions.map_or(self.store.rows, |p| p.len());
        if let Some(key) = op.census(self.graph) {
            let what = match op {
                Op::Table(_) => "table",
                _ => "lane",
            };
            Census::fallback(key, what, rows, start.elapsed().as_nanos());
        }
    }

    #[cold]
    #[inline(never)]
    fn grouped(
        &mut self,
        seg: &SegPlan,
        op: &TypedOp,
        units: &[Op],
        positions: Option<&Rc<[usize]>>,
    ) {
        let start = std::time::Instant::now();
        self.typed(seg, op, units, positions);
        Census::record(
            "typed | group".into(),
            positions.map_or(self.store.rows, |p| p.len()),
            start.elapsed().as_nanos(),
        );
    }

    fn unit(&mut self, seg: &SegPlan, op: &Op, positions: Option<&Rc<[usize]>>) {
        match op {
            Op::Lane(lane) => self.lane(seg, lane, positions, true),
            Op::Table(table) => self.table(seg, table, positions),
            _ => {}
        }
    }
}
