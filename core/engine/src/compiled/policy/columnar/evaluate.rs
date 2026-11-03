use super::output::{from_results, output};
use super::{ByBlock, Fallback, Pass, Sheet};
use crate::compiled::diagnostics::Paths;
use crate::compiled::policy::artifact::{Artifact as EvalArtifact, Iteration};
use crate::compiled::policy::entity::Entity;
use crate::compiled::policy::{PolicyPlan, Rows};
use crate::compiled::shred::{Shred, Shredder};
use crate::compiled::ColumnarOutput;
use crate::workspace::types::{EvaluateRequest, EvaluationError};
use std::cell::{OnceCell, RefCell};
use std::sync::Arc;
use zen_expression::lane::{Columns, Kind as LaneKind, LaneProgram};
use zen_expression::Variable;
use zen_types::path::DotPath;

impl PolicyPlan {
    pub(super) fn scalar(program: &LaneProgram) -> bool {
        let p = program.program();
        let regs = match p.outputs.is_empty() {
            true => std::slice::from_ref(&p.out),
            false => p.outputs.as_slice(),
        };
        regs.iter().all(|r| {
            matches!(
                p.kinds.get(*r as usize),
                Some(LaneKind::Num | LaneKind::Bool | LaneKind::Str)
            )
        })
    }

    fn safe(&self, artifact: &EvalArtifact) -> &[Iteration] {
        self.safe.get_or_init(|| {
            let mut found: Vec<Iteration> = Vec::new();
            for (name, path, owner) in self.plans.iter().filter_map(|plan| plan.iteration.as_ref())
            {
                if !found.iter().any(|(_, p, _)| p == path) {
                    found.push((name.clone(), path.clone(), owner.clone()));
                }
            }
            found.retain(|(name, path, _)| {
                self.members_safe(name, path) && self.readers_safe(artifact, path)
            });
            found
        })
    }

    fn entities<'a>(&self, artifact: &EvalArtifact, sheet: &Sheet<'a>) -> Vec<Entity<'a>> {
        if !artifact.reference_fields.is_empty() {
            return Vec::new();
        }
        self.safe(artifact)
            .iter()
            .filter_map(|(name, path, owner)| {
                let (name, path, owner) = (name.clone(), path.clone(), owner.clone());
                let entry = sheet
                    .entries
                    .iter()
                    .position(|e| e.input && e.path == path)?;
                if sheet
                    .entries
                    .iter()
                    .any(|e| Sheet::related(&e.path, &path) == Some(std::cmp::Ordering::Greater))
                {
                    return None;
                }
                Entity::new(
                    name.clone(),
                    path.clone(),
                    owner.clone(),
                    entry,
                    sheet.inputs[entry],
                )
                .or_else(|| {
                    Shred::of(&sheet.inputs[entry], sheet.rows)
                        .map(|shred| Entity::shredded(name, path, owner, entry, shred))
                })
            })
            .collect()
    }

    fn dated_tables(&self, artifact: &EvalArtifact) -> bool {
        *self.dated_tables.get_or_init(|| {
            let schema = &artifact.input_schema;
            schema.has_dates()
                && self.plans.iter().any(|plan| {
                    let Some(table) = &plan.table else {
                        return false;
                    };
                    let alias = plan.iteration.as_ref().map(|(name, path, _)| (name, path));
                    table.fields().any(|field| {
                        field.program().site_keys.iter().flatten().any(|key| {
                            let key = match alias.and_then(|(name, path)| {
                                DotPath::tail(key, name).map(|rest| format!("{path}.{rest}"))
                            }) {
                                Some(mapped) => mapped,
                                None => key.clone(),
                            };
                            schema.dates_at(&key).is_some()
                        })
                    })
                })
        })
    }

    fn members_safe(&self, name: &str, path: &str) -> bool {
        let prefix = format!("{name}.");
        self.plans.iter().enumerate().all(|(block, plan)| {
            let Some((_, own, _)) = &plan.iteration else {
                return true;
            };
            if Sheet::related(own, path) == Some(std::cmp::Ordering::Greater) {
                return false;
            }
            if own.as_ref() != path {
                return true;
            }
            if plan.member.is_none() && plan.table.is_none() {
                return false;
            }
            let Some((programs, keys)) = Pass::block(self, block) else {
                return false;
            };
            let keyed = keys.iter().all(|key| {
                key.is_empty()
                    || key
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|f| !f.is_empty())
            });
            keyed
                && programs.iter().all(|program| {
                    program
                        .program()
                        .site_keys
                        .iter()
                        .all(|k| match k.as_deref() {
                            None => true,
                            Some(k) if k == name || k.starts_with('$') => false,
                            Some(k) if k == path => Pass::siblings(program, path, name, &keys),
                            Some(k) => match k.strip_prefix(prefix.as_str()) {
                                Some(field) => !field.is_empty(),
                                None => Sheet::related(k, path).is_none(),
                            },
                        })
                })
        })
    }

    fn readers_safe(&self, artifact: &EvalArtifact, path: &str) -> bool {
        self.blocks.rules.iter().enumerate().all(|(block, rule)| {
            if self.plans[block]
                .iteration
                .as_ref()
                .is_some_and(|(_, own, _)| own.as_ref() == path)
            {
                return true;
            }
            let writes = rule
                .kind
                .write_sites()
                .iter()
                .any(|site| Sheet::related(&site.path, path).is_some());
            let reads = artifact
                .reads
                .get(&self.blocks.refs[block])
                .map_or(Vec::new(), |reads| {
                    reads
                        .iter()
                        .filter_map(|read| Sheet::related(&read.path, path))
                        .collect::<Vec<_>>()
                });
            if writes || reads.contains(&std::cmp::Ordering::Less) {
                return false;
            }
            reads.is_empty()
                || matches!(
                    (
                        self.plans[block].native(),
                        self.plans[block].iterated,
                        &self.plans[block].table,
                        &self.plans[block].iteration
                    ),
                    (Some(_), false, _, None) | (None, _, Some(_), None)
                )
        })
    }

    pub(crate) fn evaluate_columns<'a>(
        &self,
        artifact: &EvalArtifact,
        policy_path: &Arc<str>,
        goals: &[Arc<str>],
        columns: &'a Columns<'a>,
    ) -> ColumnarOutput<'a> {
        let rows = columns.rows;
        let request = |row: usize| EvaluateRequest {
            policy_path: policy_path.clone(),
            input: columns.row(row),
            goals: goals.to_vec(),
            trace: false,
        };
        let sheet = Sheet::new(columns);
        let Some(mut sheet) = sheet else {
            let requests: Vec<EvaluateRequest> = (0..rows).map(request).collect();
            return from_results(rows, self.evaluate(artifact, &requests));
        };
        let mut errors: Vec<Option<EvaluationError>> = (0..rows).map(|_| None).collect();
        let stores: Vec<OnceCell<Variable>> = (0..rows).map(|_| OnceCell::new()).collect();
        sheet.entities = self.entities(artifact, &sheet);
        sheet.refs = artifact
            .reference_fields
            .iter()
            .map(|field| field.path.clone())
            .collect();
        let shared = !artifact.reference_fields.is_empty()
            || self.plans.iter().any(|plan| match &plan.iteration {
                Some((_, path, _)) => !sheet.entities.iter().any(|e| e.path == *path),
                None => plan.native().is_none() && plan.table.is_none(),
            });
        if shared {
            sheet.entities.clear();
        }
        let dated = match shared || self.dated_tables(artifact) {
            true => vec![false; rows],
            false => sheet.dates(&artifact.input_schema),
        };
        let mut fallback = vec![false; rows];
        let mut erred = false;
        let sure = match shared {
            true => None,
            false => Entity::sure(artifact, columns, &sheet.entities, goals),
        };
        sheet
            .entities
            .iter()
            .filter_map(|entity| entity.hosted.as_deref())
            .flatten()
            .for_each(|&row| fallback[row] = true);
        for row in 0..rows {
            if !dated[row] && sure.as_ref().is_some_and(|sure| sure[row]) {
                continue;
            }
            let req = request(row);
            if !dated[row] && artifact.input_schema.convert_dates(&req.input).is_some() {
                fallback[row] = true;
                continue;
            }
            match artifact.prepare(&req) {
                Err(error) => {
                    errors[row] = Some(error);
                    erred = true;
                }
                Ok((store, _)) if shared => {
                    let _ = stores[row].set(store);
                }
                Ok(_) => {}
            }
        }
        let base = sheet.entries.len();
        let settled = !fallback.contains(&true);
        let live: Vec<usize> = match (erred, settled) {
            (true, _) => (0..rows)
                .filter(|&row| errors[row].is_none() && !fallback[row])
                .collect(),
            (false, true) => (0..rows).collect(),
            (false, false) => (0..rows).filter(|&row| !fallback[row]).collect(),
        };
        let mut runner = Self::RUNNER.with_borrow_mut(std::mem::take);
        let (sheet, fallback, errors, owned, fell, counts) = {
            let runner = &mut runner;
            let mut pass = Pass {
                plan: self,
                artifact,
                entry: policy_path,
                sheet,
                runner,
                fallback: Fallback::new(fallback),
                arms: ByBlock::default(),
                stores: &stores,
                state: Rows {
                    hosts: Vec::new(),
                    picks: Vec::new(),
                    failures: errors,
                },
                synced: vec![0; rows],
                shared,
                base,
                chosen: ByBlock::default(),
                members: ByBlock::default(),
                hosting: ByBlock::default(),
                kin: Vec::new(),
                delegated: false,
                counts: Paths::default(),
                listed: RefCell::new(Vec::new()),
                owners: self
                    .plans
                    .iter()
                    .map(|plan| plan.iteration.as_ref().map(|(_, path, _)| path.clone()))
                    .collect(),
            };
            pass.walk(self.root.clone(), live);
            let owning = |row: usize| {
                pass.state.hosts.get(row).is_some_and(Option::is_some)
                    || pass.stores[row].get().is_some()
            };
            let owns = (0..rows).any(owning);
            if owns {
                (0..pass.sheet.entities.len()).for_each(|at| pass.sheet.retire(at));
            }
            let owned: Vec<Option<Variable>> = match owns {
                false => Vec::new(),
                true => (0..rows)
                    .map(|row| {
                        match pass.state.hosts.get(row).is_some_and(Option::is_some)
                            || pass.stores[row].get().is_some()
                        {
                            true => pass
                                .host(row)
                                .map(|host| host.state.env().shallow_clone())
                                .and(pass.stores[row].get().cloned()),
                            false => None,
                        }
                    })
                    .collect(),
            };
            erred |= pass.delegated;
            let fell = pass.fallback.fell();
            (
                pass.sheet,
                pass.fallback.rows(),
                pass.state.failures,
                owned,
                fell,
                pass.counts,
            )
        };
        Self::RUNNER.with_borrow_mut(|slot| *slot = runner);
        let mut errors = errors;
        let clean = |row: usize| !erred || errors[row].is_none();
        let (hosted, finished): (Vec<usize>, Vec<usize>) = match settled && fell == 0 && !erred {
            true => (Vec::new(), (0..rows).collect()),
            false => (
                (0..rows)
                    .filter(|&row| fallback[row] && clean(row))
                    .collect(),
                (0..rows)
                    .filter(|&row| !fallback[row] && clean(row))
                    .collect(),
            ),
        };
        if hosted.is_empty() && owned.iter().all(Option::is_none) {
            let columns = sheet.output(&finished);
            return output(
                rows,
                columns,
                errors,
                Paths {
                    hosted: 0,
                    ..counts
                },
            );
        }
        let requests: Vec<EvaluateRequest> = hosted.iter().map(|&row| request(row)).collect();
        let mut shredder = Shredder::new(rows);
        for &row in &finished {
            match owned.get(row) {
                Some(Some(store)) => shredder.visit(store, 0, row),
                _ => shredder.visit(&sheet.object(row), 0, row),
            }
        }
        for (&row, result) in hosted.iter().zip(self.evaluate(artifact, &requests)) {
            match result {
                Ok(result) => shredder.visit(&result.output, 0, row),
                Err(error) => errors[row] = Some(error),
            }
        }
        output(
            rows,
            shredder.output(),
            errors,
            Paths {
                hosted: hosted.len(),
                ..counts
            },
        )
    }
}
