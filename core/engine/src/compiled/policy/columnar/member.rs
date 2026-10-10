use crate::compiled::policy::schedule::{Demand, Event};
use crate::compiled::policy::{Fixed, Native, PolicyPlan};
use crate::compiled::graph::bound::Outs;
use crate::compiled::leaf::{Array, ColumnBuilder, Dict, Leaf};
use crate::policy::blocks::MatchSelection;
use crate::policy::host::Pick;
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding, Column, Columns, Dictionary, LaneProgram, Values};
use zen_expression::Variable;
use super::{Bind, Family, Pass, Sheet, Units};

pub(super) enum Member {
    Field(usize),
    Root(usize),
    Closure,
    Absent,
    Unsupported,
}

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn entity_of(&self, block: usize) -> Option<usize> {
        let path = self.owners.get(block)?.as_ref()?;
        self.sheet.entities.iter().position(|e| e.active && e.path == *path)
    }

    pub(super) fn kids(&mut self, at: usize, positions: &[usize]) -> (Rc<[usize]>, Rc<[usize]>) {
        let full = positions.len() == self.sheet.rows;
        if let Some((_, kids, parents, ..)) = self.kin.iter().find(|(a, ..)| full && *a == at) {
            return (kids.clone(), parents.clone());
        }
        if let Some((kids, parents)) = full.then(|| self.spans(at)).flatten() {
            self.kin.push((at, kids.clone(), parents.clone(), true, OnceCell::new()));
            return (kids, parents);
        }
        let (kids, parents) = self.gather(at, positions);
        if full {
            let identity = kids.len() == self.sheet.entities[at].children && kids.iter().enumerate().all(|(i, &k)| i == k);
            self.kin.push((at, kids.clone(), parents.clone(), identity, OnceCell::new()));
        }
        (kids, parents)
    }

    fn spans(&self, at: usize) -> Option<Family> {
        let entity = &self.sheet.entities[at];
        if entity.validity.is_some() {
            return None;
        }
        let offsets = entity.offsets.get(..=self.sheet.rows)?;
        let ordered = offsets.first() == Some(&0)
            && offsets.last().and_then(|&end| usize::try_from(end).ok()) == Some(entity.children)
            && offsets.windows(2).all(|pair| pair[0] <= pair[1]);
        if !ordered {
            return None;
        }
        let mut parents = Vec::with_capacity(entity.children);
        for (row, pair) in offsets.windows(2).enumerate() {
            parents.extend(std::iter::repeat_n(row, (pair[1] - pair[0]) as usize));
        }
        Some(((0..entity.children).collect(), parents.into()))
    }

    fn identity(&self, at: usize, kids: &Rc<[usize]>) -> bool {
        match self.kin.iter().find(|(a, k, ..)| *a == at && Rc::ptr_eq(k, kids)) {
            Some((_, _, _, identity, _)) => *identity,
            None => kids.len() == self.sheet.entities[at].children && kids.iter().enumerate().all(|(i, &k)| i == k),
        }
    }

    fn broadcast(&self, entry: usize, parents: &Rc<[usize]>) -> Option<Leaf<'a>> {
        let column = self.sheet.entries[entry].leaf.column();
        let (Values::Scaled { mant, scale }, None) = (column.values, column.validity) else {
            return None;
        };
        let (.., codes) = self.kin.iter().find(|(_, _, owners, ..)| Rc::ptr_eq(owners, parents))?;
        let codes = codes
            .get_or_init(|| parents.iter().map(|&row| i32::try_from(row).ok()).collect())
            .clone()?;
        let dict = Dict::Scaled {
            mant: mant.to_vec(),
            scale: scale.to_vec(),
        };
        Some(Leaf::typed(Array::coded(codes, Rc::new(dict))))
    }

    pub(super) fn gather(&self, at: usize, positions: &[usize]) -> (Rc<[usize]>, Rc<[usize]>) {
        let entity = &self.sheet.entities[at];
        let mut kids = Vec::with_capacity(entity.children);
        let mut parents = Vec::with_capacity(entity.children);
        for &row in positions {
            if let Some((a, b)) = entity.range(row) {
                kids.extend(a..b);
                parents.extend(std::iter::repeat_n(row, b.saturating_sub(a)));
            }
        }
        (kids.into(), parents.into())
    }

    pub(super) fn site(&self, at: usize, key: Option<&str>, rows: &[usize]) -> Member {
        let Some(key) = key else {
            return Member::Closure;
        };
        let entity = &self.sheet.entities[at];
        let owned = |field: &str| {
            entity
                .owner
                .as_deref()
                .is_some_and(|owner| field == owner || field.strip_prefix(owner).is_some_and(|rest| rest.starts_with('.')))
        };
        match key.strip_prefix(entity.name.as_ref()).and_then(|rest| rest.strip_prefix('.')) {
            Some(field) if owned(field) || entity.overlaps(field) => Member::Unsupported,
            Some(field) => entity.field(field).map_or(Member::Absent, Member::Field),
            None if key == entity.name.as_ref() || key.starts_with('$') => Member::Unsupported,
            None if self.sheet.entities.iter().any(|e| e.active && Sheet::related(key, &e.path).is_some()) => Member::Unsupported,
            None => match self.sheet.bind(key, rows, self.shared) {
                Bind::Column(entry) => Member::Root(entry),
                Bind::Absent => Member::Absent,
                Bind::Row => Member::Unsupported,
            },
        }
    }

    fn field_of(&self, at: usize, key: &Arc<str>) -> Option<Option<Arc<str>>> {
        if key.is_empty() {
            return Some(None);
        }
        let entity = &self.sheet.entities[at];
        let field = key.strip_prefix(entity.name.as_ref())?.strip_prefix('.')?;
        (!field.is_empty() && !entity.overlaps(field)).then(|| Some(Arc::from(field)))
    }

    pub(super) fn native(plan: &PolicyPlan, block: usize) -> Option<&Native> {
        plan.natives[block].as_ref().or(plan.members[block].as_ref())
    }

    pub(super) fn block(plan: &PolicyPlan, block: usize) -> Option<(Vec<&LaneProgram>, Vec<&Arc<str>>)> {
        match (Self::native(plan, block), &plan.tables[block]) {
            (Some(native), _) => {
                let key = match native {
                    Native::Expression { key, .. } => key,
                    Native::Assertion { ir, .. } => &ir.output,
                    Native::Match { ir, .. } => &ir.key,
                };
                Some((Self::programs(native), vec![key]))
            }
            (None, Some(table)) => Some((table.programs(), table.outputs.iter().map(|o| &o.field).collect())),
            (None, None) => None,
        }
    }

    pub(super) fn programs(native: &Native) -> Vec<&LaneProgram> {
        match native {
            Native::Expression { program, .. } => vec![program.as_ref()],
            Native::Assertion { conditions, .. } => conditions.iter().collect(),
            Native::Match { conditions, values, .. } => conditions.iter().chain(values.iter()).flatten().collect(),
        }
    }

    pub(super) fn supported(&self, at: usize, block: usize, rows: &[usize]) -> bool {
        let Some((programs, keys)) = Self::block(self.plan, block) else {
            return false;
        };
        let iterated = self.plan.members[block].is_some() || self.plan.tables[block].is_some();
        iterated
            && keys.iter().all(|key| self.field_of(at, key).is_some())
            && programs.into_iter().all(|program| {
                let p = program.program();
                !(p.writes_env || p.chain || p.opaque() || p.rows)
                    && p.site_keys.iter().all(|k| !matches!(self.site(at, k.as_deref(), rows), Member::Unsupported))
            })
    }

    fn member_run(&mut self, at: usize, program: &LaneProgram, kids: &Rc<[usize]>, parents: &Rc<[usize]>) -> (Leaf<'a>, Vec<bool>) {
        let (mut leaves, failed) = self.member_outs(at, program, kids, parents);
        let leaf = match leaves.is_empty() {
            true => Leaf::nulls(kids.len()),
            false => leaves.swap_remove(0),
        };
        (leaf, failed)
    }

    pub(super) fn member_outs(&mut self, at: usize, program: &LaneProgram, kids: &Rc<[usize]>, parents: &Rc<[usize]>) -> (Vec<Leaf<'a>>, Vec<bool>) {
        let p = program.program();
        let count = kids.len();
        let mut failed = vec![false; count];
        if count == 0 {
            return (Vec::new(), failed);
        }
        let mut views: Vec<(Arc<str>, Leaf<'a>)> = Vec::new();
        let identity = self.identity(at, kids);
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|key| {
                let name: Arc<str> = key.as_deref().map_or_else(|| Arc::from(""), Arc::from);
                if let Some(position) = views.iter().position(|(k, _)| *k == name) {
                    return Binding::Column(position);
                }
                let leaf = match self.site(at, key.as_deref(), parents) {
                    Member::Field(field) if identity => self.sheet.entities[at].view(field),
                    Member::Field(field) => self.sheet.entities[at].view(field).pick(kids),
                    Member::Root(entry) => self.broadcast(entry, parents).unwrap_or_else(|| self.sheet.entries[entry].leaf.pick(parents)),
                    Member::Closure => return Binding::Row,
                    Member::Absent | Member::Unsupported => return Binding::Absent,
                };
                views.push((name, leaf));
                Binding::Column(views.len() - 1)
            })
            .collect();
        let mut columns = Columns::new(count);
        for (name, view) in &views {
            columns = columns.column(name.as_ref(), view.column());
        }
        let scopes = crate::compiled::graph::bound::Bound::blank(count);
        let mut outs = Outs::take();
        let special = self.plan.special(program, &columns);
        let program = special.as_deref().unwrap_or(program);
        Self::prefer_codes(program, &mut outs);
        self.runner.evaluate_sites(program, scopes.slice(), &columns, &bound, None, &mut outs, |child, _, _| {
            if let Some(f) = failed.get_mut(child) {
                *f = true;
            }
        });
        let leaves = outs.iter_mut().map(|out| Leaf::typed(Array::from_output(out))).collect();
        outs.iter_mut().for_each(zen_expression::lane::Output::prefer_text);
        (leaves, failed)
    }

    pub(super) fn member_fail(&mut self, parents: &[usize], failed: &[bool]) {
        if !failed.contains(&true) {
            return;
        }
        for (&row, &f) in parents.iter().zip(failed) {
            if f {
                self.fall(row);
            }
        }
    }

    pub(super) fn member_write(&mut self, at: usize, key: &Arc<str>, leaf: Leaf<'a>, kids: &Rc<[usize]>) {
        if let Some(Some(field)) = self.field_of(at, key) {
            self.sheet.entities[at].write(field, leaf, kids);
        }
    }

    pub(super) fn member(&mut self, at: usize, block: usize, event: Event, positions: &[usize]) {
        let plan = self.plan;
        let (kids, parents) = self.kids(at, positions);
        if let (None, Some(table)) = (Self::native(plan, block), &plan.tables[block]) {
            let units = Units {
                member: Some(at),
                units: kids,
                owners: parents,
            };
            return match event {
                Event::Select(block) => self.table_select(block, table, &units),
                Event::Commit(block) => self.table_commit(block, table, &units),
            };
        }
        match (event, Self::native(plan, block)) {
            (Event::Commit(_), Some(Native::Expression { key, program })) => {
                let (leaf, failed) = self.member_run(at, program, &kids, &parents);
                self.member_fail(&parents, &failed);
                self.member_write(at, key, leaf, &kids);
            }
            (Event::Commit(_), Some(Native::Assertion { ir, conditions })) => {
                let mut truths: Vec<Vec<u64>> = Vec::with_capacity(conditions.len());
                for program in conditions {
                    let (leaf, failed) = self.member_run(at, program, &kids, &parents);
                    self.member_fail(&parents, &failed);
                    truths.push(leaf.truths(kids.len()));
                }
                let bits = Self::folded(ir, &truths, kids.len());
                let leaf = Leaf::typed(Array::parts(crate::compiled::leaf::Store::Bool(bits), None, kids.len()));
                self.member_write(at, &ir.output, leaf, &kids);
            }
            (Event::Select(_), Some(Native::Match { pick: Some(pick), .. })) => {
                let total = self.sheet.entities[at].children;
                let (leaf, failed) = self.member_run(at, pick, &kids, &parents);
                self.member_fail(&parents, &failed);
                let picked = Self::arms_of(&leaf, kids.len());
                let slot = self.members.iter().position(|(b, _)| *b == block);
                match slot {
                    None if self.identity(at, &kids) => self.members.push((block, picked)),
                    None => {
                        let mut chosen: Vec<Option<usize>> = vec![None; total];
                        kids.iter().zip(picked).for_each(|(&child, arm)| chosen[child] = arm);
                        self.members.push((block, chosen));
                    }
                    Some(at) => {
                        let arms = &mut self.members[at].1;
                        kids.iter().zip(picked).for_each(|(&child, arm)| arms[child] = arm);
                    }
                }
            }
            (Event::Select(_), Some(Native::Match { conditions, .. })) => {
                let total = self.sheet.entities[at].children;
                let mut chosen: Vec<Option<usize>> = vec![None; total];
                let mut open: Vec<usize> = (0..kids.len()).collect();
                for (arm, condition) in conditions.iter().enumerate() {
                    if open.is_empty() {
                        break;
                    }
                    let Some(program) = condition else {
                        open.drain(..).for_each(|local| chosen[kids[local]] = Some(arm));
                        break;
                    };
                    let subset: Rc<[usize]> = open.iter().map(|&local| kids[local]).collect();
                    let owners: Rc<[usize]> = open.iter().map(|&local| parents[local]).collect();
                    let (leaf, failed) = self.member_run(at, program, &subset, &owners);
                    self.member_fail(&owners, &failed);
                    let mut next = Vec::with_capacity(open.len());
                    for (i, &local) in open.iter().enumerate() {
                        match leaf.truthy(i) {
                            true => chosen[kids[local]] = Some(arm),
                            false => next.push(local),
                        }
                    }
                    open = next;
                }
                match self.members.iter_mut().find(|(b, _)| *b == block) {
                    Some((_, arms)) => kids.iter().for_each(|&child| arms[child] = chosen[child]),
                    None => self.members.push((block, chosen)),
                }
            }
            (Event::Commit(_), Some(Native::Match { ir, values, constants, .. })) if Self::constant(values, constants) => {
                let codes: Vec<i32> = match self.members.iter().find(|(b, _)| *b == block) {
                    Some((_, arms)) => kids.iter().map(|&child| Self::code(arms.get(child).copied().flatten(), values)).collect(),
                    None => vec![-1; kids.len()],
                };
                self.member_write(at, &ir.key, Self::coded(&Self::fixed(constants), codes), &kids);
            }
            (Event::Commit(_), Some(Native::Match { ir, values, constants, .. })) => {
                let arms = self.members.iter().find(|(b, _)| *b == block).map(|(_, arms)| arms.clone()).unwrap_or_default();
                let mut out: Vec<Option<(usize, usize)>> = vec![None; kids.len()];
                let mut leaves: Vec<Leaf<'a>> = Vec::new();
                let mut groups: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
                for (local, &child) in kids.iter().enumerate() {
                    if let Some(arm) = arms.get(child).copied().flatten().filter(|&arm| values[arm].is_some()) {
                        groups[arm].push(local);
                    }
                }
                if groups.iter().enumerate().all(|(arm, locals)| locals.is_empty() || constants.get(arm).is_some_and(Option::is_some)) {
                    let values: Vec<Variable> = constants.iter().map(|c| c.as_ref().map_or(Variable::Null, Fixed::variable)).collect();
                    let mut codes = vec![-1i32; kids.len()];
                    for (arm, locals) in groups.iter().enumerate() {
                        locals.iter().for_each(|&local| codes[local] = arm as i32);
                    }
                    self.member_write(at, &ir.key, Self::coded(&values, codes), &kids);
                    return;
                }
                let fixed: Vec<(usize, Variable)> = constants.iter().enumerate().filter_map(|(arm, c)| Some((arm, c.as_ref()?.variable()))).collect();
                if !fixed.is_empty() {
                    let mut builder = ColumnBuilder::with_capacity(fixed.len());
                    fixed.iter().for_each(|(_, value)| builder.push_variable(value.clone()));
                    let table = Leaf::typed(builder.finish());
                    for (slot, (arm, _)) in fixed.iter().enumerate() {
                        groups[*arm].drain(..).for_each(|local| out[local] = Some((leaves.len(), slot)));
                    }
                    leaves.push(table);
                }
                for (arm, locals) in groups.into_iter().enumerate() {
                    let Some(program) = values[arm].as_ref().filter(|_| !locals.is_empty()) else {
                        continue;
                    };
                    let subset: Rc<[usize]> = locals.iter().map(|&local| kids[local]).collect();
                    let owners: Rc<[usize]> = locals.iter().map(|&local| parents[local]).collect();
                    let (leaf, failed) = self.member_run(at, program, &subset, &owners);
                    self.member_fail(&owners, &failed);
                    for (i, &local) in locals.iter().enumerate() {
                        out[local] = Some((leaves.len(), i));
                    }
                    leaves.push(leaf);
                }
                let columns: Vec<Column> = leaves.iter().map(Leaf::column).collect();
                let mut builder = ColumnBuilder::with_capacity(out.len());
                for cell in &out {
                    match cell {
                        Some((leaf, i)) => builder.push_cell(&columns[*leaf], *i),
                        None => builder.push_null(),
                    }
                }
                self.member_write(at, &ir.key, Leaf::typed(builder.finish()), &kids);
            }
            _ => {}
        }
    }

    pub(super) fn member_demand(&self, block: usize, row: usize) -> Option<Demand> {
        let at = self.entity_of(block)?;
        let (_, arms) = self.members.iter().find(|(b, _)| *b == block)?;
        let Some(Native::Match { ir, .. }) = Self::native(self.plan, block) else {
            return None;
        };
        let plan = self.artifact.read_plans.get(&self.plan.blocks.refs[block])?;
        let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
        let mut demanded: Vec<Arc<str>> = Vec::new();
        for child in a..b {
            let pick = Pick::Match(MatchSelection {
                matched_arm: arms.get(child).copied().flatten().and_then(|arm| ir.arms.get(arm)).map(|arm| arm.id.clone()),
                arms: Vec::new(),
            });
            pick.collect_reads(plan, &mut demanded);
        }
        demanded.sort();
        demanded.dedup();
        Some(demanded.into())
    }

    fn plain(leaf: &Leaf) -> bool {
        matches!(
            leaf,
            Leaf::Input(_) | Leaf::Typed(_) | Leaf::Masked(_) | Leaf::Scattered(_) | Leaf::Picked(_)
        ) && matches!(
            leaf.column().values,
            Values::Scaled { .. }
                | Values::Dec(_)
                | Values::I64(_)
                | Values::Bool { .. }
                | Values::Utf8 { .. }
                | Values::Text { .. }
                | Values::Dict {
                    values: Dictionary::Scaled { .. } | Dictionary::Text { .. } | Dictionary::Bool { .. },
                    ..
                }
        )
    }

    pub(super) fn direct(&self, units: &Units, program: &LaneProgram) -> Option<Leaf<'a>> {
        let key = program.path()?;
        let leaf = match units.member {
            None => {
                if self.sheet.entities.iter().any(|e| e.active && Sheet::related(key, &e.path).is_some()) {
                    return None;
                }
                let Bind::Column(entry) = self.sheet.bind(key, &units.units, self.shared) else {
                    return None;
                };
                let leaf = &self.sheet.entries[entry].leaf;
                match units.units.len() == self.sheet.rows {
                    true => leaf.clone(),
                    false => leaf.pick(&units.units),
                }
            }
            Some(at) => match self.site(at, Some(key), &units.owners) {
                Member::Field(field) if self.identity(at, &units.units) => self.sheet.entities[at].view(field),
                Member::Field(field) => self.sheet.entities[at].view(field).pick(&units.units),
                Member::Root(entry) => self.sheet.entries[entry].leaf.pick(&units.owners),
                _ => return None,
            },
        };
        Self::plain(&leaf).then_some(leaf)
    }

    pub(super) fn evaluate(&mut self, units: &Units, program: &LaneProgram) -> Leaf<'a> {
        match units.member {
            None => {
                let (leaf, failed) = self.run(program, &units.units);
                self.fail(&units.units, &failed);
                leaf
            }
            Some(at) => {
                let (leaf, failed) = self.member_run(at, program, &units.units, &units.owners);
                self.member_fail(&units.owners, &failed);
                leaf
            }
        }
    }
}
