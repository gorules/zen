mod blocks;
mod evaluate;
mod fuse;
mod host;
mod lane;
mod member;
mod output;
mod sheet;
mod table;
mod typed;

pub(super) use fuse::Fusions;
pub use output::PolicyColumnarOutput;
use member::Member;
use sheet::{Bind, Sheet, Slot, Units};
use super::schedule::{Demand, End, Event, Segment};
use super::{Native, PolicyPlan, Rows};
use crate::compiled::core::list::Kin;
use crate::compiled::shred::Listing;
use crate::compiled::leaf::Leaf;
use crate::policy::blocks::MatchSelection;
use crate::policy::evaluator::EvalArtifact;
use crate::policy::host::Pick;
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::LaneRunner;
use zen_expression::{Scope, Variable};
use zen_types::symbol::Symbol;

type Listed<'a> = (Rc<str>, bool, Option<Rc<Listing<'a>>>);
type Fields<'a> = (usize, Vec<(Symbol, Leaf<'a>)>);
type Extent<'p> = (usize, &'p [i32], Option<(&'p [u64], usize)>);
type Present<'a> = (Arc<str>, Leaf<'a>, Option<Rc<[u64]>>);

struct Pass<'s, 'a> {
    plan: &'s PolicyPlan,
    artifact: &'s EvalArtifact,
    entry: &'s Arc<str>,
    sheet: Sheet<'a>,
    runner: &'s mut LaneRunner,
    fallback: Vec<bool>,
    arms: Vec<(usize, Vec<Option<usize>>)>,
    stores: &'s [OnceCell<Variable>],
    state: Rows<'s>,
    synced: Vec<usize>,
    shared: bool,
    base: usize,
    chosen: Vec<(usize, Vec<u32>)>,
    members: Vec<(usize, Vec<Option<usize>>)>,
    hosting: Vec<(usize, Vec<bool>)>,
    owners: Vec<Option<Arc<str>>>,
    kin: Vec<Kin>,
    delegated: bool,
    fell: usize,
    counts: (usize, usize),
    listed: RefCell<Vec<Listed<'a>>>,
}

impl<'s, 'a> Pass<'s, 'a> {
    fn scope(&mut self, row: usize) -> Scope {
        if !self.shared {
            return Scope::new(self.sheet.object(row));
        }
        match self.host(row) {
            Some(host) => {
                host.state.clear_dollar();
                Scope::new(host.state.env().shallow_clone())
            }
            None => Scope::default(),
        }
    }

    fn scopes(&mut self, positions: &[usize]) -> Vec<Scope> {
        let mut scopes: Vec<Scope> = (0..self.sheet.rows).map(|_| Scope::default()).collect();
        for &row in positions {
            scopes[row] = self.scope(row);
        }
        scopes
    }

    fn fall(&mut self, row: usize) {
        if !self.fallback[row] {
            self.fallback[row] = true;
            self.fell += 1;
        }
    }

    fn fail(&mut self, positions: &[usize], failed: &[bool]) {
        if !failed.contains(&true) {
            return;
        }
        for (&row, &f) in positions.iter().zip(failed) {
            if f {
                self.fall(row);
            }
        }
    }

    fn write(&mut self, key: &Arc<str>, leaf: Leaf<'a>, positions: &[usize]) {
        if !key.is_empty() {
            self.sheet.write(key.clone(), leaf, positions);
        }
    }

    fn demand(&self, block: usize, row: usize) -> Demand {
        if let Some(demand) = self.member_demand(block, row).or_else(|| self.table_demand(block, row)) {
            return demand;
        }
        let owner = &self.plan.blocks.refs[block];
        if let (Some(picked), Some(host)) = (self.state.picked(row, block), self.state.hosts.get(row).and_then(Option::as_ref)) {
            return host.state.demanded(owner, picked).into();
        }
        let Some(plan) = self.artifact.read_plans.get(owner) else {
            return Arc::from([]);
        };
        let Some(Native::Match { ir, .. }) = &self.plan.natives[block] else {
            return Arc::from([]);
        };
        let pick = Pick::Match(MatchSelection {
            matched_arm: self.arm(block, row).and_then(|arm| ir.arms.get(arm)).map(|arm| arm.id.clone()),
            arms: Vec::new(),
        });
        let mut demanded = Vec::new();
        pick.collect_reads(plan, &mut demanded);
        demanded.into()
    }

    fn event(&mut self, event: Event, positions: &[usize]) {
        let plan = self.plan;
        let block = match event {
            Event::Select(block) | Event::Commit(block) => block,
        };
        if self.owners[block].is_some() {
            let selected = self.members.iter().any(|(b, _)| *b == block) || self.chosen.iter().any(|(b, _)| *b == block);
            if let Some(at) = self.entity_of(block) {
                if self.supported(at, block, positions) {
                    return self.member(at, block, event, positions);
                }
                if selected || self.sheet.entities[at].cached() {
                    positions.iter().for_each(|&row| self.fall(row));
                    return;
                }
                self.sheet.retire(at);
            }
            return self.delegate(event, positions);
        }
        if let Some(table) = &plan.tables[block] {
            let units = |rows: &[usize]| Units {
                member: None,
                units: rows.into(),
                owners: rows.into(),
            };
            return match event {
                Event::Select(block) => self.table_select(block, table, &units(positions)),
                Event::Commit(block) if !self.hosting.iter().any(|(b, _)| *b == block) => self.table_commit(block, table, &units(positions)),
                Event::Commit(block) => {
                    let (hosted, native): (Vec<usize>, Vec<usize>) = positions.iter().partition(|&&row| self.hosted(block, row));
                    if !native.is_empty() {
                        self.table_commit(block, table, &units(&native));
                    }
                    if !hosted.is_empty() {
                        self.delegate(event, &hosted);
                    }
                }
            };
        }
        match (event, &plan.natives[block]) {
            (Event::Select(block), Some(Native::Match { conditions, pick, .. })) => self.select(block, conditions, pick.as_deref(), positions),
            (Event::Commit(block), Some(Native::Match { ir, values, constants, .. })) => self.commit(block, ir, values, constants, positions),
            (Event::Commit(_), Some(Native::Expression { key, program })) => self.expression(key, program, positions),
            (Event::Commit(_), Some(Native::Assertion { ir, conditions })) => self.assertion(ir, conditions, positions),
            _ => self.delegate(event, positions),
        }
    }

    fn walk(&mut self, root: Arc<Segment>, live: Vec<usize>) {
        let mut stack: Vec<(Arc<Segment>, Vec<usize>)> = vec![(root, live)];
        while let Some((segment, mut rows)) = stack.pop() {
            let segment = self.events(segment, &mut rows);
            let End::Branch { block, .. } = &segment.end else {
                continue;
            };
            if rows.is_empty() {
                continue;
            }
            if self.quiet(*block) {
                stack.push((self.plan.child(self.artifact, &segment, Arc::from([])), rows));
                continue;
            }
            let mut groups: Vec<(Demand, Vec<usize>)> = Vec::new();
            let mut memo: ahash::HashMap<Vec<u32>, usize> = ahash::HashMap::default();
            for &row in &rows {
                let key = self.selection(*block, row);
                if let Some(&at) = key.as_ref().and_then(|key| memo.get(key)) {
                    groups[at].1.push(row);
                    continue;
                }
                let demand = self.demand(*block, row);
                let at = match groups.iter().position(|(d, _)| *d == demand) {
                    Some(at) => {
                        groups[at].1.push(row);
                        at
                    }
                    None => {
                        groups.push((demand, vec![row]));
                        groups.len() - 1
                    }
                };
                if let Some(key) = key {
                    memo.insert(key, at);
                }
            }
            for (demand, members) in groups.into_iter().rev() {
                stack.push((self.plan.child(self.artifact, &segment, demand), members));
            }
        }
    }
}
