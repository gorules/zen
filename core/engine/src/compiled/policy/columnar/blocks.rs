use crate::compiled::policy::{Fixed, Native};
use crate::compiled::leaf::{Array, Bits, ColumnBuilder, Leaf};
use crate::policy::blocks::{AssertionIr, MatchIr};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{LaneProgram, Values};
use zen_expression::Variable;
use super::Pass;

impl<'s, 'a> Pass<'s, 'a> {
    pub(super) fn expression(&mut self, key: &Arc<str>, program: &LaneProgram, positions: &[usize]) {
        let (leaf, failed) = self.run(program, positions);
        self.fail(positions, &failed);
        self.write(key, leaf, positions);
    }

    pub(super) fn assertion(&mut self, ir: &AssertionIr, conditions: &[LaneProgram], positions: &[usize]) {
        let mut truths: Vec<Vec<u64>> = Vec::with_capacity(conditions.len());
        for program in conditions {
            let (leaf, failed) = self.run(program, positions);
            self.fail(positions, &failed);
            truths.push(leaf.truths(positions.len()));
        }
        let bits = Self::folded(ir, &truths, positions.len());
        let leaf = Leaf::typed(Array::parts(crate::compiled::leaf::Store::Bool(bits), None, positions.len()));
        self.write(&ir.output, leaf, positions);
    }

    pub(super) fn folded(ir: &AssertionIr, truths: &[Vec<u64>], count: usize) -> Vec<u64> {
        let width = truths.len();
        if width > 6 {
            let mut results = vec![false; width];
            return Bits::of(count, |at| {
                results.iter_mut().zip(truths).for_each(|(r, t)| *r = Bits::get(t, at));
                ir.fold(&results)
            });
        }
        let mut results = vec![false; width];
        let table = (0..1usize << width).fold(0u64, |table, combo| {
            results.iter_mut().enumerate().for_each(|(c, r)| *r = combo >> c & 1 == 1);
            table | u64::from(ir.fold(&results)) << combo
        });
        match table {
            0 => vec![0; count.div_ceil(64)],
            _ => Bits::of(count, |at| {
                let combo = truths.iter().enumerate().fold(0usize, |combo, (c, t)| combo | usize::from(Bits::get(t, at)) << c);
                table >> combo & 1 == 1
            }),
        }
    }

    fn dict(values: &[Variable]) -> crate::compiled::leaf::Dict {
        use crate::compiled::leaf::Dict;
        if values.iter().all(|v| matches!(v, Variable::String(_))) {
            let mut offsets = vec![0i32];
            let mut data = String::new();
            for value in values {
                data.push_str(value.as_str().unwrap_or_default());
                offsets.push(data.len() as i32);
            }
            return Dict::Text { offsets, data };
        }
        let scaled: Option<Vec<(i64, u8)>> = values
            .iter()
            .map(|v| match v {
                Variable::Number(n) => Some((i64::try_from(n.mantissa()).ok()?, u8::try_from(n.scale()).ok()?)),
                _ => None,
            })
            .collect();
        if let Some(parts) = scaled {
            return Dict::Scaled {
                mant: parts.iter().map(|p| p.0).collect(),
                scale: parts.iter().map(|p| p.1).collect(),
            };
        }
        if values.iter().all(|v| matches!(v, Variable::Bool(_))) {
            return Dict::Bool(Bits::of(values.len(), |i| matches!(values[i], Variable::Bool(true))));
        }
        Dict::Any(values.to_vec())
    }

    pub(super) fn coded(values: &[Variable], codes: Vec<i32>) -> Leaf<'a> {
        let mut valid: Vec<u64> = codes
            .chunks(64)
            .map(|chunk| chunk.iter().enumerate().fold(0u64, |word, (at, code)| word | u64::from(*code >= 0) << at))
            .collect();
        Bits::trim(&mut valid, codes.len());
        Leaf::typed(Array::coded_with(codes.into(), Rc::new(Self::dict(values)), valid))
    }

    pub(super) fn arms_of(leaf: &Leaf, count: usize) -> Vec<Option<usize>> {
        let column = leaf.column();
        if let Values::Scaled { mant, scale } = column.values {
            if mant.len() >= count && scale.len() >= count && scale[..count].iter().all(|s| *s == 0) {
                return mant[..count].iter().map(|m| usize::try_from(*m).ok()).collect();
            }
        }
        (0..count)
            .map(|at| column.number(at).and_then(|n| i64::try_from(n).ok()).and_then(|n| usize::try_from(n).ok()))
            .collect()
    }

    pub(super) fn select(&mut self, block: usize, conditions: &[Option<LaneProgram>], pick: Option<&LaneProgram>, positions: &[usize]) {
        let mut chosen: Vec<Option<usize>> = vec![None; self.sheet.rows];
        if let Some(pick) = pick {
            let (leaf, failed) = self.run(pick, positions);
            self.fail(positions, &failed);
            for (&row, arm) in positions.iter().zip(Self::arms_of(&leaf, positions.len())) {
                chosen[row] = arm;
            }
            match self.arms.iter_mut().find(|(b, _)| *b == block) {
                Some((_, arms)) => positions.iter().for_each(|&row| arms[row] = chosen[row]),
                None => self.arms.push((block, chosen)),
            }
            return;
        }
        let mut open: Vec<usize> = positions.to_vec();
        for (arm, condition) in conditions.iter().enumerate() {
            if open.is_empty() {
                break;
            }
            let Some(program) = condition else {
                open.drain(..).for_each(|row| chosen[row] = Some(arm));
                break;
            };
            let (leaf, failed) = self.run(program, &open);
            self.fail(&open, &failed);
            let mut next = Vec::with_capacity(open.len());
            for (at, &row) in open.iter().enumerate() {
                match leaf.truthy(at) {
                    true => chosen[row] = Some(arm),
                    false => next.push(row),
                }
            }
            open = next;
        }
        match self.arms.iter_mut().find(|(b, _)| *b == block) {
            Some((_, arms)) => positions.iter().for_each(|&row| arms[row] = chosen[row]),
            None => self.arms.push((block, chosen)),
        }
    }

    pub(super) fn arm(&self, block: usize, row: usize) -> Option<usize> {
        self.arms.iter().find(|(b, _)| *b == block).and_then(|(_, arms)| arms[row])
    }

    pub(super) fn constant(values: &[Option<LaneProgram>], constants: &[Option<Fixed>]) -> bool {
        values.iter().enumerate().all(|(arm, value)| value.is_none() || constants.get(arm).is_some_and(Option::is_some))
    }

    pub(super) fn fixed(constants: &[Option<Fixed>]) -> Vec<Variable> {
        constants.iter().map(|c| c.as_ref().map_or(Variable::Null, Fixed::variable)).collect()
    }

    pub(super) fn code(arm: Option<usize>, values: &[Option<LaneProgram>]) -> i32 {
        arm.filter(|&arm| values.get(arm).is_some_and(Option::is_some)).map_or(-1, |arm| arm as i32)
    }

    pub(super) fn commit(&mut self, block: usize, ir: &MatchIr, values: &[Option<LaneProgram>], constants: &[Option<Fixed>], positions: &[usize]) {
        if Self::constant(values, constants) {
            let codes: Vec<i32> = match self.arms.iter().find(|(b, _)| *b == block) {
                Some((_, arms)) => positions.iter().map(|&row| Self::code(arms[row], values)).collect(),
                None => vec![-1; positions.len()],
            };
            self.write(&ir.key, Self::coded(&Self::fixed(constants), codes), positions);
            return;
        }
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
        for (local, &row) in positions.iter().enumerate() {
            if let Some(arm) = self.arm(block, row).filter(|&arm| values[arm].is_some()) {
                groups[arm].push(local);
            }
        }
        if groups.iter().enumerate().all(|(arm, locals)| locals.is_empty() || constants.get(arm).is_some_and(Option::is_some)) {
            let values: Vec<Variable> = constants.iter().map(|c| c.as_ref().map_or(Variable::Null, Fixed::variable)).collect();
            let mut codes = vec![-1i32; positions.len()];
            for (arm, locals) in groups.iter().enumerate() {
                locals.iter().for_each(|&local| codes[local] = arm as i32);
            }
            self.write(&ir.key, Self::coded(&values, codes), positions);
            return;
        }
        let mut out: Vec<Variable> = vec![Variable::Null; positions.len()];
        for (arm, locals) in groups.iter_mut().enumerate() {
            if let Some(Some(value)) = constants.get(arm) {
                let value = value.variable();
                locals.drain(..).for_each(|local| out[local] = value.clone());
            }
        }
        for (arm, locals) in groups.into_iter().enumerate() {
            let Some(program) = values[arm].as_ref().filter(|_| !locals.is_empty()) else {
                continue;
            };
            let members: Vec<usize> = locals.iter().map(|&local| positions[local]).collect();
            let (leaf, failed) = self.run(program, &members);
            self.fail(&members, &failed);
            for (i, &local) in locals.iter().enumerate() {
                out[local] = leaf.get(i);
            }
        }
        let mut builder = ColumnBuilder::with_capacity(out.len());
        out.into_iter().for_each(|value| builder.push_variable(value));
        self.write(&ir.key, Leaf::typed(builder.finish()), positions);
    }

    pub(super) fn selection(&self, block: usize, row: usize) -> Option<Vec<u32>> {
        let code = |arm: Option<usize>| arm.map_or(0, |arm| arm as u32 + 1);
        if let Some(at) = self.entity_of(block) {
            if let (Some((_, arms)), Some(Native::Match { .. })) = (self.members.iter().find(|(b, _)| *b == block), Self::native(self.plan, block)) {
                let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
                let mut key: Vec<u32> = (a..b).map(|child| code(arms.get(child).copied().flatten())).collect();
                key.sort_unstable();
                key.dedup();
                key.insert(0, 1);
                return Some(key);
            }
        }
        if let (Some(table), false) = (&self.plan.tables[block], self.hosted(block, row)) {
            if let Some((_, chosen)) = self.chosen.iter().find(|(b, _)| *b == block) {
                let outputs = table.outputs.len().max(1);
                let unit = |unit: usize| -> Vec<u32> { (0..outputs).map(|c| chosen.get(unit * outputs + c).copied().unwrap_or(u32::MAX)).collect() };
                let mut units: Vec<Vec<u32>> = match self.entity_of(block) {
                    None => vec![unit(row)],
                    Some(at) => {
                        let (a, b) = self.sheet.entities[at].range(row).unwrap_or((0, 0));
                        (a..b).map(unit).collect()
                    }
                };
                units.sort_unstable();
                units.dedup();
                let mut key = vec![2];
                key.extend(units.into_iter().flatten());
                return Some(key);
            }
        }
        if self.state.picked(row, block).is_some() {
            return None;
        }
        match &self.plan.natives[block] {
            Some(Native::Match { .. }) => Some(vec![3, code(self.arm(block, row))]),
            _ => None,
        }
    }
}
