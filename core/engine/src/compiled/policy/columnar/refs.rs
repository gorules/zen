use super::Sheet;
use crate::compiled::census::Census;
use crate::compiled::core::list::Frame;
use crate::compiled::leaf::{Array, Bits, Leaf};
use crate::compiled::policy::artifact::Artifact as EvalArtifact;
use crate::compiled::policy::entity::{Entity, Field};
use crate::compiled::policy::PolicyPlan;
use crate::compiled::shred::Normalizer;
use crate::policy::queries::scope::ReferenceField;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;

type Items<'a> = (Column<'a>, &'a [(&'a str, Column<'a>)]);

struct Listed<'a> {
    path: Arc<str>,
    target: Arc<str>,
    entry: usize,
    frame: Frame,
    index: Rc<[usize]>,
    items: Option<Items<'a>>,
}

fn refused<T>(why: &str) -> Option<T> {
    if Census::on() {
        Census::record(format!("policy references | {why}"), 0, 0);
    }
    None
}

impl PolicyPlan {
    fn referable(&self, artifact: &EvalArtifact) -> bool {
        let fields = &artifact.reference_fields;
        let schema = &artifact.input_schema;
        let flat = fields.iter().all(|field| {
            !schema.has_dated(&field.target)
                && field
                    .path
                    .split_once('.')
                    .is_some_and(|(root, rest)| schema.root(root) && !rest.contains('.'))
        });
        let related = |a: &str, b: &str| Sheet::related(a, b).is_some();
        let free = |field: &ReferenceField| {
            (0..self.blocks.rules.len()).all(|block| {
                let iteration = self.flat(block).iteration.as_ref().map(|(_, path, _)| path);
                let pooled = field.array && iteration.is_some_and(|path| *path == field.target);
                let iterated = iteration.is_some_and(|path| {
                    related(path, &field.path) || (!pooled && related(path, &field.target))
                });
                let written = self.rule(block).kind.write_sites().iter().any(|site| {
                    related(&site.path, &field.path)
                        || (!pooled && related(&site.path, &field.target))
                });
                !iterated && !written
            })
        };
        let safe = |field: &ReferenceField| !field.array || self.readers_safe(artifact, &field.path);
        if Census::on() {
            let why = match (flat, fields.iter().all(free), fields.iter().all(safe)) {
                (false, _, _) => "nested or dated",
                (_, false, _) => "blocks on the pool or the reference",
                (_, _, false) => "readers not safe",
                _ => "",
            };
            if !why.is_empty() {
                Census::record(format!("policy references | {why}"), 0, 0);
            }
        }
        flat && fields.iter().all(|field| free(field) && safe(field))
    }

    fn pool_items<'a>(columns: &Columns<'a>, target: &str) -> Option<Option<Items<'a>>> {
        let Some(pool) = columns.find(target).map(|at| columns.columns[at].1) else {
            return Some(None);
        };
        match pool.values {
            Values::List {
                child: Dictionary::Column(child),
                ..
            } => match child.values {
                Values::Struct { fields, .. } => Some(Some((*child, fields))),
                _ => refused("pool items are not records"),
            },
            _ => refused("pool is not a list"),
        }
    }

    fn located(pool: Option<Column>, items: Option<Items>, row: usize, text: &str) -> Option<usize> {
        let (pool, (child, fields)) = (pool?, items?);
        let ids = fields.iter().find(|(name, _)| *name == "id").map(|(_, c)| *c)?;
        let (a, b) = pool.range(row)?;
        (a..b).rev().find(|&item| {
            child.valid(item)
                && ids.valid(item)
                && match ids.text(item) {
                    Some(id) => id == text,
                    None => matches!(ids.variable(item), Variable::String(id) if *id == *text),
                }
        })
    }

    fn listed<'a>(
        field: &ReferenceField,
        entry: usize,
        columns: &Columns<'a>,
        items: Option<Items<'a>>,
        unresolved: &mut [bool],
    ) -> Option<Listed<'a>> {
        let key = columns.columns[entry].1;
        let pool = columns.find(&field.target).map(|at| columns.columns[at].1);
        let Values::List { offsets, child } = key.values else {
            return refused("references are not a list");
        };
        let child = match child {
            Dictionary::Column(child) => *child,
            other => other.column(),
        };
        let rows = columns.rows;
        let children = child.len();
        let mut index = vec![0usize; children];
        for (row, missing) in unresolved.iter_mut().enumerate() {
            let Some((a, b)) = key.range(row) else {
                continue;
            };
            for element in a..b.min(children) {
                let found = match child.valid(element) {
                    false => None,
                    true => match child.text(element) {
                        Some(text) => Self::located(pool, items, row, text),
                        None => match child.variable(element) {
                            Variable::String(text) => Self::located(pool, items, row, &text),
                            _ => None,
                        },
                    },
                };
                match found {
                    Some(item) => index[element] = item,
                    None => *missing = true,
                }
            }
        }
        let validity = Bits::partial(key.validity, rows)
            .map(|(bits, offset)| Rc::from(Bits::window(bits, offset, rows)));
        let frame = Frame {
            offsets: Rc::from(offsets),
            validity,
            children,
        };
        if frame.span(rows).is_none() {
            return refused("reference offsets");
        }
        Some(Listed {
            path: field.path.clone(),
            target: field.target.clone(),
            entry,
            frame,
            index: index.into(),
            items,
        })
    }

    fn view<'a>(&self, sheet: &Sheet<'a>, listed: Listed<'a>) -> Option<Entity<'a>> {
        let Listed {
            path,
            target,
            entry,
            frame,
            index,
            items,
        } = listed;
        let pool = sheet
            .entities
            .iter()
            .position(|entity| entity.active && entity.path == target);
        if let Some(pool) = pool {
            if frame.children > 0 && sheet.entities[pool].frame.children == 0 {
                return refused("empty pool");
            }
            let fields = sheet.entities[pool].gathered(&index);
            return Some(Entity::linked(path, entry, frame, fields, Some((pool, index))));
        }
        if self
            .flats()
            .any(|plan| plan.iteration.as_ref().is_some_and(|(_, own, _)| *own == target))
        {
            return refused("pool without entity");
        }
        let Some((child, fields)) = items else {
            return (frame.children == 0)
                .then(|| Entity::linked(path, entry, frame, Vec::new(), None));
        };
        if Bits::partial(child.validity, child.len()).is_some()
            || (frame.children > 0 && child.len() == 0)
        {
            return refused("pool items missing");
        }
        let fields = fields
            .iter()
            .map(|(name, column)| {
                Normalizer::class(&column.values)?;
                Some(Field {
                    name: Arc::from(*name),
                    leaf: Normalizer::lean(*column, child.len()).pick(&index),
                    present: None,
                    input: true,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Entity::linked(path, entry, frame, fields, None))
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
        let mut lists: Vec<Listed<'a>> = Vec::new();
        for field in fields {
            let Some(entry) = columns.find(&field.path) else {
                continue;
            };
            let key = columns.columns[entry].1;
            let pool = columns.find(&field.target).map(|at| columns.columns[at].1);
            let items = Self::pool_items(columns, &field.target)?;
            if field.array {
                lists.push(Self::listed(field, entry, columns, items, &mut unresolved)?);
                continue;
            }
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
                match Self::located(pool, items, row, &text) {
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
        let views: Vec<Entity<'a>> = lists
            .into_iter()
            .map(|listed| self.view(sheet, listed))
            .collect::<Option<_>>()?;
        sheet.resolve(&keys, leaves);
        sheet.entities.extend(views);
        Some(unresolved)
    }
}
