use crate::compiled::bound::Bound;
use crate::compiled::data::Col;
use crate::compiled::data::Mask;
use crate::compiled::leaf::{Array, Bits, ColumnBuilder, Dict, Leaf, Literal, Records};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::LaneRunner;
use zen_expression::Variable;
use super::{First, Hit, Kind, Table};

pub(crate) struct Collected<'a> {
    pub paths: Arc<[Arc<str>]>,
    pub columns: Vec<Leaf<'a>>,
    pub present: Vec<Mask<'a>>,
    pub unmatched: Vec<u64>,
}

impl Table {
    pub fn collects(&self) -> bool {
        self.split.is_some()
    }

    fn literal_lists<'a>(
        first: &First,
        candidates: &[u64],
        words: usize,
        lists: usize,
        targets: &[Vec<Option<usize>>],
        templates: &[Vec<Option<Variable>>],
    ) -> Vec<Leaf<'a>> {
        let mut keys: std::collections::HashMap<Vec<u64>, i32> = std::collections::HashMap::new();
        let mut entries: Vec<Vec<u64>> = Vec::new();
        let codes: Rc<[i32]> = (0..first.codes.len())
            .map(|row| {
                let Some(chosen) = first.chosen(row) else {
                    return -1;
                };
                let mut key: Vec<u64> = candidates[row * words..(row + 1) * words].to_vec();
                for (word, bits) in key.iter_mut().enumerate() {
                    let base = word * 64;
                    *bits &= match chosen.checked_sub(base) {
                        Some(skip) if skip >= 64 => 0,
                        Some(skip) => u64::MAX << skip,
                        None => u64::MAX,
                    };
                }
                key[chosen / 64] |= 1 << (chosen % 64);
                *keys.entry(key).or_insert_with_key(|key| {
                    entries.push(key.clone());
                    entries.len() as i32 - 1
                })
            })
            .collect();
        (0..lists)
            .map(|list| {
                let values: Vec<Variable> = entries
                    .iter()
                    .map(|key| {
                        let items: Vec<Variable> = Self::ones(key)
                            .flat_map(|rule| targets[rule].iter().zip(&templates[rule]))
                            .filter(|(target, _)| **target == Some(list))
                            .filter_map(|(_, template)| template.clone())
                            .collect();
                        Variable::from_array(items)
                    })
                    .collect();
                Leaf::typed(Array::coded(codes.clone(), Rc::new(Dict::Any(values))))
            })
            .collect()
    }

    pub fn first_collect<'a>(&self, runner: &mut LaneRunner, bound: &Bound) -> Collected<'a> {
        let paths = self.split.clone().unwrap_or_default();
        let rows = bound.rows();
        let words = self.words;
        let (candidates, deferred) = self.candidates(runner, bound, false);
        let first = self.choose(runner, bound, &candidates, deferred);
        let lists = self.collect_columns.len();
        let mut collected: Vec<Vec<Vec<Variable>>> = vec![vec![Vec::new(); rows]; lists];
        let targets: Vec<Vec<Option<usize>>> = self
            .outputs
            .iter()
            .map(|outputs| {
                outputs
                    .paths
                    .iter()
                    .map(|(_, c, column)| match c {
                        true => self.collect_columns.iter().position(|(k, _)| k == column),
                        false => None,
                    })
                    .collect()
            })
            .collect();
        let templates: Vec<Vec<Option<Variable>>> = self
            .outputs
            .iter()
            .zip(&targets)
            .map(|(outputs, targets)| match &outputs.literals {
                Some(literals) => literals
                    .iter()
                    .zip(targets)
                    .map(|(literal, target)| target.map(|_| literal.variable()))
                    .collect(),
                None => Vec::new(),
            })
            .collect();
        let literal = first.batches.is_empty()
            && self.outputs.iter().zip(&targets).all(|(outputs, targets)| match &outputs.literals {
                Some(literals) => !literals.iter().any(Literal::deep),
                None => !targets.iter().any(Option::is_some),
            });
        let lists_leaves: Vec<Leaf<'a>> = match literal {
            true => Self::literal_lists(&first, &candidates, words, lists, &targets, &templates),
            false => {
            for (row, &code) in first.codes.iter().enumerate() {
                let Some(rule) = first.rule(code) else {
                    continue;
                };
                match first.computed(code).is_some() {
                    true => {
                        for (index, target) in targets[rule].iter().enumerate() {
                            if let Some(list) = target {
                                collected[*list][row].push(first.value(row, index));
                            }
                        }
                    }
                    false => {
                        for (target, template) in targets[rule].iter().zip(&templates[rule]) {
                            if let (Some(list), Some(template)) = (target, template) {
                                collected[*list][row].push(template.deep_clone());
                            }
                        }
                    }
                }
            }
            let mut any = vec![0u64; words];
            for row in 0..rows {
                any.iter_mut()
                    .zip(&candidates[row * words..(row + 1) * words])
                    .for_each(|(a, c)| *a |= c);
            }
            for rule in Self::ones(&any) {
                let outputs = &self.outputs[rule];
                if !targets[rule].iter().any(Option::is_some) {
                    continue;
                }
                let lanes: Vec<usize> = (0..rows)
                    .filter(|&row| candidates[row * words + rule / 64] >> (rule % 64) & 1 == 1)
                    .filter(|&row| first.chosen(row).is_some_and(|c| c < rule))
                    .collect();
                if lanes.is_empty() {
                    continue;
                }
                match outputs.literals {
                    Some(_) => {
                        for &row in &lanes {
                            for (target, template) in targets[rule].iter().zip(&templates[rule]) {
                                if let (Some(list), Some(template)) = (target, template) {
                                    collected[*list][row].push(template.deep_clone());
                                }
                            }
                        }
                    }
                    None => {
                        let order: Vec<usize> = targets[rule].iter().filter_map(|t| *t).collect();
                        Self::run_rule(runner, outputs.collect.as_ref(), bound, &lanes, &mut |row, values| {
                            for (list, value) in order.iter().zip(values.iter().flatten()) {
                                collected[*list][row].push(value.deep_clone());
                            }
                        });
                    }
                }
            }
                collected
                    .into_iter()
                    .map(|lists| {
                        Leaf::Any(
                            lists
                                .into_iter()
                                .enumerate()
                                .map(|(row, items)| match first.codes[row] < 0 {
                                    true => Variable::Null,
                                    false => Variable::from_array(items),
                                })
                                .collect::<Col>(),
                        )
                    })
                    .collect()
            }
        };
        let collect_slots: Vec<bool> = self
            .all_paths
            .iter()
            .map(|p| self.collect_columns.iter().any(|(_, c)| c == p))
            .collect();
        let (scalars, masks, unmatched) = self.columns(first);
        let mut columns = Vec::with_capacity(paths.len());
        let mut present = Vec::with_capacity(paths.len());
        for ((column, mask), skip) in scalars.into_iter().zip(masks).zip(&collect_slots) {
            if !skip {
                columns.push(column);
                present.push(mask);
            }
        }
        let matched: Rc<[u64]> = {
            let mut bits: Vec<u64> = unmatched.iter().map(|w| !w).collect();
            Bits::trim(&mut bits, rows);
            bits.into()
        };
        for leaf in lists_leaves {
            columns.push(leaf);
            present.push(Mask::Bits(matched.clone()));
        }
        Collected {
            paths,
            columns,
            present,
            unmatched,
        }
    }

    pub fn collect_keyed(&self, runner: &mut LaneRunner, bound: &Bound) -> Option<(Vec<Variable>, Vec<i32>, usize)> {
        let literal = self.outputs.iter().all(|o| o.paths.is_empty() || o.literals.is_some());
        if self.hit != Hit::Collect || !literal || !self.stable {
            return None;
        }
        let rows = bound.rows();
        let words = self.words;
        let (candidates, _) = self.candidates(runner, bound, false);
        let mut seen: ahash::HashMap<&[u64], i32> = ahash::HashMap::default();
        let mut sets: Vec<&[u64]> = Vec::new();
        let codes: Vec<i32> = (0..rows)
            .map(|row| {
                let key = &candidates[row * words..(row + 1) * words];
                *seen.entry(key).or_insert_with(|| {
                    sets.push(key);
                    (sets.len() - 1) as i32
                })
            })
            .collect();
        let templates: Vec<Vec<Variable>> = sets
            .iter()
            .map(|set| {
                Self::ones(set)
                    .map(|rule| {
                        let outputs = &self.outputs[rule];
                        match &outputs.literals {
                            Some(literals) if !outputs.paths.is_empty() => {
                                let values: Vec<Variable> = literals.iter().map(Literal::variable).collect();
                                self.built(self.builder(rule, None).as_ref(), rule, None, &values)
                            }
                            _ => Variable::empty_object(),
                        }
                    })
                    .collect()
            })
            .collect();
        let values = codes
            .iter()
            .map(|&code| Variable::from_array(templates[code as usize].iter().map(Variable::deep_clone).collect()))
            .collect();
        Some((values, codes, sets.len()))
    }

    fn record_names(&self) -> Option<Vec<Arc<str>>> {
        let mut names: Vec<Arc<str>> = Vec::new();
        for outputs in &self.outputs {
            let mut last: Option<usize> = None;
            for (path, _, _) in &outputs.paths {
                if path.is_empty() || path.contains('.') || path.starts_with('$') {
                    return None;
                }
                let at = match names.iter().position(|name| name == path) {
                    Some(at) => at,
                    None => {
                        names.push(path.clone());
                        names.len() - 1
                    }
                };
                if last.is_some_and(|last| at <= last) {
                    return None;
                }
                last = Some(at);
            }
        }
        Some(names)
    }

    fn record_dict(cells: &[Vec<Option<&Literal>>], field: usize) -> Option<Dict> {
        let column: Vec<Option<&Literal>> = cells.iter().map(|rule| rule.get(field).copied().flatten().filter(|l| !matches!(l, Literal::Null))).collect();
        let kinds: Vec<Kind> = column.iter().flatten().map(|literal| Kind::of_literal(literal)).collect::<Option<Vec<_>>>()?;
        let kind = *kinds.first()?;
        if kinds.iter().any(|k| *k != kind) {
            return None;
        }
        Some(match kind {
            Kind::Text => {
                let mut offsets = Vec::with_capacity(column.len() + 1);
                let mut data = String::new();
                offsets.push(0i32);
                for literal in &column {
                    if let Some(Literal::Str(text)) = literal {
                        data.push_str(text);
                    }
                    offsets.push(i32::try_from(data.len()).ok()?);
                }
                Dict::Text { offsets, data }
            }
            Kind::Num => {
                let parts: Vec<(i64, u8)> = column.iter().map(|literal| literal.and_then(Literal::scaled).unwrap_or((0, 0))).collect();
                Dict::Scaled {
                    mant: parts.iter().map(|p| p.0).collect(),
                    scale: parts.iter().map(|p| p.1).collect(),
                }
            }
            Kind::Bool => {
                let mut bits = vec![0u64; column.len().div_ceil(64).max(1)];
                column.iter().enumerate().filter(|(_, l)| matches!(l, Some(Literal::Bool(true)))).for_each(|(at, _)| Bits::set(&mut bits, at, true));
                Dict::Bool(bits)
            }
            Kind::Any => return None,
        })
    }

    pub fn collect_records<'a>(&self, runner: &mut LaneRunner, bound: &Bound) -> Option<(Records<'a>, Vec<i32>, usize)> {
        let literal = self.outputs.iter().all(|o| o.paths.is_empty() || o.literals.is_some());
        if self.hit != Hit::Collect || !literal || !self.stable {
            return None;
        }
        let names = self.record_names()?;
        let rows = bound.rows();
        let words = self.words;
        let (candidates, _) = self.candidates(runner, bound, false);
        let mut seen: ahash::HashMap<&[u64], i32> = ahash::HashMap::default();
        let mut sets: Vec<Vec<usize>> = Vec::new();
        let codes: Vec<i32> = (0..rows)
            .map(|row| {
                let key = &candidates[row * words..(row + 1) * words];
                *seen.entry(key).or_insert_with(|| {
                    sets.push(Self::ones(key).collect());
                    (sets.len() - 1) as i32
                })
            })
            .collect();
        let cells: Vec<Vec<Option<&Literal>>> = (0..self.outputs.len())
            .map(|rule| {
                let outputs = &self.outputs[rule];
                names
                    .iter()
                    .map(|name| {
                        let index = outputs.paths.iter().position(|(path, _, _)| path == name)?;
                        outputs.literals.as_ref()?.get(index)
                    })
                    .collect()
            })
            .collect();
        let mut offsets = Vec::with_capacity(rows + 1);
        offsets.push(0i32);
        let mut total = 0usize;
        for &code in &codes {
            total += sets[code as usize].len();
            offsets.push(i32::try_from(total).ok()?);
        }
        let dicts: Vec<Option<Rc<Dict>>> = (0..names.len()).map(|field| Self::record_dict(&cells, field).map(Rc::new)).collect();
        let mut builders: Vec<Option<ColumnBuilder>> = dicts.iter().map(|dict| dict.is_none().then(|| ColumnBuilder::with_capacity(total))).collect();
        let mut keys: Vec<Vec<i32>> = dicts.iter().map(|dict| match dict {
            Some(_) => Vec::with_capacity(total),
            None => Vec::new(),
        }).collect();
        let mut present: Vec<Vec<u64>> = names.iter().map(|_| vec![0u64; total.div_ceil(64)]).collect();
        let mut item = 0usize;
        for &code in &codes {
            for &rule in &sets[code as usize] {
                for (((builder, keys), bits), cell) in builders.iter_mut().zip(keys.iter_mut()).zip(present.iter_mut()).zip(&cells[rule]) {
                    if cell.is_some() {
                        Bits::set(bits, item, true);
                    }
                    match builder {
                        Some(builder) => match cell {
                            Some(literal) => builder.push_literal(literal),
                            None => builder.push_null(),
                        },
                        None => keys.push(match cell {
                            Some(Literal::Null) | None => -1,
                            Some(_) => rule as i32,
                        }),
                    }
                }
                item += 1;
            }
        }
        let fields = names
            .iter()
            .zip(builders.into_iter().zip(keys).zip(dicts))
            .zip(present)
            .map(|((name, ((builder, keys), dict)), bits)| {
                let leaf = match (builder, dict) {
                    (Some(builder), _) => Leaf::typed(builder.finish()),
                    (None, Some(dict)) => Leaf::typed(Array::coded(keys.into(), dict)),
                    (None, None) => Leaf::nulls(total),
                };
                (zen_types::symbol::Symbol::from(name.as_ref()), leaf, Rc::from(bits))
            })
            .collect();
        let size = sets.len();
        Some((Records::new(rows, offsets, Bits::ones(rows), fields, None), codes, size))
    }
}
