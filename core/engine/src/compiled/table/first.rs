use crate::compiled::bound::{Bound, Outs};
use crate::compiled::leaf::Array;
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;
use super::{Batch, Deferred, Hit, Table};

pub(crate) struct First {
    pub(super) rules: usize,
    pub(super) codes: Vec<i32>,
    pub(super) batches: Vec<Batch>,
    pub(super) starts: Vec<usize>,
}

impl First {
    pub fn coded(rules: usize, codes: Vec<i32>) -> First {
        First {
            rules,
            codes,
            batches: Vec::new(),
            starts: vec![0],
        }
    }

    pub(super) fn entries(&self) -> usize {
        self.rules + self.starts.last().copied().unwrap_or(0)
    }

    pub(super) fn computed(&self, code: i32) -> Option<(usize, usize)> {
        let entry = usize::try_from(code).ok()?.checked_sub(self.rules)?;
        let batch = self.starts.partition_point(|&start| start <= entry).checked_sub(1)?;
        Some((batch, entry - self.starts[batch]))
    }

    pub(super) fn rule(&self, code: i32) -> Option<usize> {
        let code = usize::try_from(code).ok()?;
        match code < self.rules {
            true => Some(code),
            false => self.computed(code as i32).and_then(|(batch, _)| self.batches.get(batch)).map(|b| b.rule),
        }
    }

    pub(super) fn chosen(&self, row: usize) -> Option<usize> {
        self.codes.get(row).and_then(|&code| self.rule(code))
    }

    pub(super) fn value(&self, row: usize, index: usize) -> Variable {
        self.codes
            .get(row)
            .and_then(|&code| self.computed(code))
            .and_then(|(batch, pos)| Some(self.batches.get(batch)?.outputs.get(index)?.column().variable(pos).deep_clone()))
            .unwrap_or(Variable::Null)
    }
}

impl Table {
    pub fn first_hit(&self) -> bool {
        self.hit == Hit::First
    }

    fn next(candidates: &[u64], from: usize) -> Option<usize> {
        let (mut word, bit) = (from / 64, from % 64);
        let mut current = candidates.get(word)? & (u64::MAX << bit);
        loop {
            if current != 0 {
                return Some(word * 64 + current.trailing_zeros() as usize);
            }
            word += 1;
            current = *candidates.get(word)?;
        }
    }

    pub fn first(&self, runner: &mut LaneRunner, bound: &Bound) -> First {
        if let (Some(dispatch), true) = (&self.dispatch, self.literal) {
            if let (Some(keys), Some(chosen)) = (self.keyed(dispatch, runner, bound), &dispatch.chosen) {
                return First {
                    rules: self.outputs.len(),
                    codes: keys.iter().map(|key| chosen.get(*key as usize).copied().unwrap_or(-1)).collect(),
                    batches: Vec::new(),
                    starts: vec![0],
                };
            }
        }
        let (candidates, deferred) = self.candidates(runner, bound, true);
        self.choose(runner, bound, &candidates, deferred)
    }

    pub(super) fn choose(&self, runner: &mut LaneRunner, bound: &Bound, candidates: &[u64], mut deferred: Vec<Deferred>) -> First {
        let rows = bound.rows();
        let words = self.words;
        let rules = self.outputs.len();
        if deferred.is_empty() && self.literal {
            return First {
                rules,
                codes: candidates
                    .chunks_exact(words)
                    .map(|row| Self::next(row, 0).map_or(-1, |rule| rule as i32))
                    .collect(),
                batches: Vec::new(),
                starts: vec![0],
            };
        }
        let mut cursor = vec![0usize; rows];
        let mut codes: Vec<i32> = vec![-1; rows];
        let mut batches: Vec<Batch> = Vec::new();
        let mut starts: Vec<usize> = vec![0];
        let mut open: Vec<usize> = (0..rows).collect();
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); self.rules];
        let mut touched: Vec<usize> = Vec::new();
        let mut kept = vec![false; rows];
        let mut before: Vec<usize> = Vec::new();
        let mut outs = Outs::take();
        while !open.is_empty() {
            for &row in &open {
                if let Some(rule) = Self::next(&candidates[row * words..(row + 1) * words], cursor[row]) {
                    if buckets[rule].is_empty() {
                        touched.push(rule);
                    }
                    buckets[rule].push(row);
                }
            }
            open.clear();
            for rule in std::mem::take(&mut touched) {
                let mut lanes = std::mem::take(&mut buckets[rule]);
                if !deferred.is_empty() {
                    before.clear();
                    before.extend_from_slice(&lanes);
                    self.verify(runner, bound, &mut deferred, rule, &mut lanes);
                    lanes.iter().for_each(|&row| kept[row] = true);
                    for &row in before.iter().filter(|&&r| !kept[r]) {
                        cursor[row] = rule + 1;
                        open.push(row);
                    }
                    lanes.iter().for_each(|&row| kept[row] = false);
                }
                if lanes.is_empty() {
                    buckets[rule] = lanes;
                    continue;
                }
                let outputs = &self.outputs[rule];
                let direct = match (outputs.paths.is_empty(), &outputs.literals) {
                    (false, None) => Self::direct(outputs, bound, &lanes),
                    _ => None,
                };
                match (outputs.paths.is_empty(), &outputs.literals, &outputs.full) {
                    (true, _, _) | (false, Some(_), _) => lanes.iter().for_each(|&row| codes[row] = rule as i32),
                    (false, None, _) if direct.is_some() => {
                        let start = starts.last().copied().unwrap_or(0);
                        for (pos, &row) in lanes.iter().enumerate() {
                            codes[row] = (rules + start + pos) as i32;
                        }
                        starts.push(start + lanes.len());
                        batches.push(Batch {
                            rule,
                            outputs: direct.unwrap_or_default(),
                        });
                    }
                    (false, None, Some(program)) => {
                        let mut failed = vec![false; lanes.len()];
                        bound.export(runner, program, Some(&lanes), &mut outs, |pos, _, _| {
                            if let Some(f) = failed.get_mut(pos) {
                                *f = true;
                            }
                        });
                        let start = starts.last().copied().unwrap_or(0);
                        for (pos, &row) in lanes.iter().enumerate() {
                            match failed[pos] {
                                true => {
                                    cursor[row] = rule + 1;
                                    open.push(row);
                                }
                                false => codes[row] = (rules + start + pos) as i32,
                            }
                        }
                        starts.push(start + lanes.len());
                        batches.push(Batch {
                            rule,
                            outputs: outs.iter_mut().map(Array::from_output).collect(),
                        });
                    }
                    (false, None, None) => {
                        for &row in &lanes {
                            cursor[row] = rule + 1;
                            open.push(row);
                        }
                    }
                }
                lanes.clear();
                buckets[rule] = lanes;
            }
        }
        First {
            rules,
            codes,
            batches,
            starts,
        }
    }
}
