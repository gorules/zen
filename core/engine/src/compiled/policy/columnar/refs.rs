use super::Sheet;
use crate::compiled::leaf::{Array, Bits, Leaf};
use crate::compiled::policy::artifact::Artifact as EvalArtifact;
use crate::compiled::policy::PolicyPlan;
use crate::compiled::shred::Normalizer;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;

impl PolicyPlan {
    fn referable(&self, artifact: &EvalArtifact) -> bool {
        let fields = &artifact.reference_fields;
        let schema = &artifact.input_schema;
        let flat = fields.iter().all(|field| {
            !field.array
                && !schema.has_dated(&field.target)
                && field
                    .path
                    .split_once('.')
                    .is_some_and(|(root, rest)| schema.root(root) && !rest.contains('.'))
        });
        let touched = |path: &str| {
            fields.iter().any(|field| {
                Sheet::related(path, &field.target).is_some()
                    || Sheet::related(path, &field.path).is_some()
            })
        };
        flat && !self
            .plans
            .iter()
            .any(|plan| plan.iteration.as_ref().is_some_and(|(_, path, _)| touched(path)))
            && !self
                .blocks
                .rules
                .iter()
                .any(|rule| rule.kind.write_sites().iter().any(|site| touched(&site.path)))
    }

    pub(super) fn references<'a>(
        &self,
        artifact: &EvalArtifact,
        sheet: &mut Sheet<'a>,
        columns: &Columns<'a>,
    ) -> Option<Vec<bool>> {
        let fields = &artifact.reference_fields;
        if fields.is_empty() || !self.referable(artifact) {
            return None;
        }
        let rows = columns.rows;
        let mut unresolved = vec![false; rows];
        let mut keys: Vec<Arc<str>> = Vec::new();
        let mut leaves: Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)> = Vec::new();
        for field in fields {
            let Some(key) = columns.find(&field.path).map(|at| columns.columns[at].1) else {
                continue;
            };
            let pool = columns.find(&field.target).map(|at| columns.columns[at].1);
            let items: Option<(&Column<'a>, &'a [(&'a str, Column<'a>)])> = match pool {
                None => None,
                Some(pool) => match pool.values {
                    Values::List {
                        child: Dictionary::Column(child),
                        ..
                    } => match child.values {
                        Values::Struct { fields, .. } => Some((child, fields)),
                        _ => return None,
                    },
                    _ => return None,
                },
            };
            let ids = items.and_then(|(_, fields)| {
                fields
                    .iter()
                    .find(|(name, _)| *name == "id")
                    .map(|(_, column)| *column)
            });
            let mut found: Vec<Option<usize>> = vec![None; rows];
            for (row, slot) in found.iter_mut().enumerate() {
                if !key.valid(row) {
                    continue;
                }
                let text = match key.variable(row) {
                    Variable::Null => continue,
                    Variable::String(text) => text,
                    _ => {
                        unresolved[row] = true;
                        continue;
                    }
                };
                let hit = match (pool, items, ids) {
                    (Some(pool), Some((child, _)), Some(ids)) => pool.range(row).and_then(|(a, b)| {
                        (a..b).rev().find(|&item| {
                            child.valid(item)
                                && matches!(ids.variable(item), Variable::String(id) if id == text)
                        })
                    }),
                    _ => None,
                };
                match hit {
                    Some(item) => *slot = Some(item),
                    None => unresolved[row] = true,
                }
            }
            keys.push(field.path.clone());
            let Some((child, struct_fields)) = items else {
                continue;
            };
            if found.iter().all(Option::is_none) {
                continue;
            }
            let picks: Vec<usize> = found.iter().map(|item| item.unwrap_or(0)).collect();
            for (name, column) in struct_fields {
                Normalizer::class(&column.values)?;
                let mut valid = vec![0u64; rows.div_ceil(64)];
                for (row, item) in found.iter().enumerate() {
                    if item.is_some_and(|item| child.valid(item) && column.valid(item)) {
                        Bits::set(&mut valid, row, true);
                    }
                }
                let gathered = Leaf::typed(Array::gather(column, &picks));
                let full = valid.iter().map(|w| w.count_ones() as usize).sum::<usize>() == rows;
                let (leaf, present) = match full {
                    true => (gathered, None),
                    false => {
                        let valid: Rc<[u64]> = Rc::from(valid);
                        (Leaf::masked(gathered, &valid), Some(valid))
                    }
                };
                leaves.push((Arc::from(format!("{}.{name}", field.path)), leaf, present));
            }
        }
        sheet.resolve(&keys, leaves);
        Some(unresolved)
    }
}
