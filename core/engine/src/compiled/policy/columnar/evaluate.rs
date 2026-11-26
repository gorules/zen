use super::output::{from_results, output};
use super::{ByBlock, Fallback, Pass, Sheet};
use crate::compiled::census::Census;
use crate::compiled::diagnostics::Paths;
use crate::compiled::policy::artifact::{Artifact as EvalArtifact, Iteration};
use crate::compiled::policy::entity::Entity;
use crate::compiled::policy::{BlockPlan, PolicyPlan, Rows};
use crate::compiled::shred::{Shred, Shredder};
use crate::compiled::ColumnarOutput;
use crate::workspace::{EvaluateRequest, EvaluationError};
use std::cell::{OnceCell, RefCell};
use std::sync::Arc;
use zen_expression::context::EvaluationContext;
use zen_expression::lane::{Columns, Kind as LaneKind, LaneProgram};
use zen_expression::Variable;

impl PolicyPlan {
    const DELEGATED_ROOTS: usize = 2;
    const DELEGATED_MEMBERS: usize = 3;

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
            for (name, path, owner) in self.flats().filter_map(|plan| plan.iteration.as_ref())
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

    fn entities<'a>(
        &self,
        artifact: &EvalArtifact,
        sheet: &Sheet<'a>,
    ) -> Vec<Entity<'a>> {
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

    #[cold]
    fn unentitied(&self, artifact: &EvalArtifact, sheet: &Sheet) {
        let safe = self.safe(artifact);
        let mut seen: Vec<&Arc<str>> = Vec::new();
        for (name, path, _) in self.flats().filter_map(|plan| plan.iteration.as_ref()) {
            if seen.contains(&path) || sheet.entities.iter().any(|e| e.path == *path) {
                continue;
            }
            seen.push(path);
            let why = if !artifact.reference_fields.is_empty() {
                "reference fields"
            } else if !safe.iter().any(|(_, p, _)| p == path) {
                match (
                    self.members_safe(name, path),
                    self.readers_safe(artifact, path),
                ) {
                    (false, _) => "members not safe",
                    (_, false) => "readers not safe",
                    _ => "?",
                }
            } else {
                match sheet
                    .entries
                    .iter()
                    .position(|e| e.input && e.path == *path)
                {
                    None if sheet.entries.iter().any(|e| {
                        Sheet::related(&e.path, path) == Some(std::cmp::Ordering::Greater)
                    }) =>
                    {
                        "path is an object of columns"
                    }
                    None => "no input column",
                    Some(_) => "nested columns or unshreddable list",
                }
            };
            Census::record(format!("policy no entity | {why}"), 0, 0);
        }
    }

    #[cold]
    fn refusals(&self) {
        for (block, plan) in self.flats().enumerate() {
            if let (Some(why), None, None) = (plan.refused(), &plan.table, &plan.member) {
                let table = match &self.rule(block).kind {
                    crate::policy::blocks::BlockKind::DecisionTable(ir) => {
                        crate::compiled::table::Table::compile(
                            crate::compiled::table::TableIr::policy(ir),
                        )
                        .err()
                    }
                    _ => None,
                };
                Census::record(
                    format!(
                        "policy block refused | {} | {why} | {}",
                        self.rule(block).id,
                        table.unwrap_or_default()
                    ),
                    0,
                    0,
                );
            }
        }
    }

    fn delegating(&self) -> bool {
        let refused =
            |plan: &&BlockPlan| plan.native().is_none() && plan.table.is_none() && plan.member.is_none();
        let roots = self.flats()
            .filter(refused)
            .filter(|plan| plan.iteration.is_none())
            .count();
        let members = self.flats()
            .filter(refused)
            .filter_map(|plan| plan.iteration.as_ref().map(|(_, path, _)| path))
            .map(|path| {
                self.plans
                    .iter()
                    .filter(|plan| {
                        plan.iteration
                            .as_ref()
                            .is_some_and(|(_, own, _)| own == path)
                    })
                    .count()
            })
            .max()
            .unwrap_or(0);
        roots >= Self::DELEGATED_ROOTS || members >= Self::DELEGATED_MEMBERS
    }

    fn members_safe(&self, name: &str, path: &str) -> bool {
        let prefix = format!("{name}.");
        self.flats().enumerate().all(|(block, plan)| {
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
                return self.rule(block)
                    .kind
                    .write_sites()
                    .iter()
                    .all(|site| {
                        site.path
                            .strip_prefix(prefix.as_str())
                            .is_some_and(|f| !f.is_empty())
                    });
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

    pub(super) fn readers_safe(&self, artifact: &EvalArtifact, path: &str) -> bool {
        (0..self.blocks.rules.len()).all(|block| {
            let rule = self.rule(block);
            if self.flat(block)
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
                        self.flat(block).native(),
                        self.flat(block).iterated,
                        &self.flat(block).table,
                        &self.flat(block).iteration
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
        context: &EvaluationContext,
    ) -> ColumnarOutput<'a> {
        let resolved = columns.resolved();
        let columns = resolved.as_ref().unwrap_or(columns);
        let rows = columns.rows;
        let request = |row: usize| EvaluateRequest {
            policy_path: policy_path.clone(),
            input: columns.row(row),
            goals: goals.to_vec(),
            trace: crate::compiled::trace::TraceFormat::Off,
        };
        let sheet = Sheet::new(columns);
        let Some(mut sheet) = sheet else {
            let requests: Vec<EvaluateRequest> = (0..rows).map(request).collect();
            return from_results(rows, self.evaluate(artifact, &requests, context));
        };
        let mut errors: Vec<Option<EvaluationError>> = (0..rows).map(|_| None).collect();
        let stores: Vec<OnceCell<Variable>> = (0..rows).map(|_| OnceCell::new()).collect();
        sheet.entities = self.entities(artifact, &sheet);
        let unresolved = self.references(artifact, &mut sheet, columns);
        let referenced = !artifact.reference_fields.is_empty() && unresolved.is_none();
        if referenced {
            sheet.entities.clear();
            sheet.refs = artifact
                .reference_fields
                .iter()
                .map(|field| field.path.clone())
                .collect();
        }
        let shared = referenced
            || self.delegating()
            || self.flats().any(|plan| match &plan.iteration {
                Some((_, path, _)) => !sheet.entities.iter().any(|e| e.path == *path),
                None => false,
            });
        if shared && Census::on() {
            self.unentitied(artifact, &sheet);
            self.refusals();
        }
        if shared {
            sheet.entities.clear();
            if Census::on() {
                let why = match (referenced, self.delegating()) {
                    (true, _) => "reference fields",
                    (false, true) => "refused blocks",
                    (false, false) => "iteration without entity",
                };
                Census::record(format!("policy rows | shared | {why}"), rows, 0);
            }
        }
        let converted = !shared;
        if converted {
            sheet.dates(&artifact.input_schema, context.timezone());
        }
        let mut dated: Option<Vec<bool>> = None;
        let mut fallback = vec![false; rows];
        let mut erred = false;
        let sure = match shared {
            true => None,
            false => Entity::sure(
                artifact,
                columns,
                &sheet.entities,
                goals,
                converted,
                unresolved.is_some(),
            )
            .map(|mut sure| {
                if let Some(unresolved) = &unresolved {
                    sure.iter_mut()
                        .zip(unresolved)
                        .for_each(|(sure, missing)| *sure &= !missing);
                }
                sure
            }),
        };
        sheet
            .entities
            .iter()
            .filter_map(|entity| entity.hosted.as_deref())
            .flatten()
            .for_each(|&row| fallback[row] = true);
        if Census::on() {
            let irregular = fallback.iter().filter(|&&f| f).count();
            if irregular > 0 {
                Census::record("policy hosted | irregular list".into(), irregular, 0);
            }
            let unsure = (0..rows)
                .filter(|&row| sure.as_ref().is_none_or(|sure| !sure[row]))
                .count();
            if unsure > 0 {
                let why = match (sure.is_some(), shared) {
                    (_, true) => "shared",
                    (false, false) => "no columnar validation",
                    (true, false) => "not validated by columns",
                };
                Census::record(format!("policy rows | prepared per row | {why}"), unsure, 0);
            }
        }
        for row in 0..rows {
            if sure.as_ref().is_some_and(|sure| sure[row]) {
                continue;
            }
            let req = request(row);
            let dated = dated.get_or_insert_with(|| match converted {
                true => sheet.dated(),
                false => vec![false; rows],
            });
            if !dated[row]
                && artifact
                    .input_schema
                    .convert_dates(&req.input, context.timezone())
                    .is_some()
            {
                fallback[row] = true;
                if Census::on() {
                    Census::record("policy hosted | date input".into(), 1, 0);
                }
                continue;
            }
            match artifact.prepare(&req, context.timezone()) {
                Err(error) => {
                    errors[row] = Some(error);
                    erred = true;
                }
                Ok((store, _)) if shared => {
                    let _ = stores[row].set(store);
                }
                Ok(_) if unresolved.as_ref().is_some_and(|unresolved| unresolved[row]) => {
                    fallback[row] = true;
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
                    context: context.clone(),
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
                present: self.gates.iter().map(|_| OnceCell::new()).collect(),
                owners: self.flats()
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
        if Census::on() {
            Census::record("policy hosted | rows".into(), hosted.len(), 0);
            Census::record("policy rows | objects after walk".into(), finished.len(), 0);
        }
        let requests: Vec<EvaluateRequest> = hosted.iter().map(|&row| request(row)).collect();
        context.restart(&hosted);
        let evaluated = self.evaluate(artifact, &requests, &context.rows(&hosted));
        if owned.iter().all(Option::is_none) && evaluated.iter().all(Result::is_err) {
            for (&row, result) in hosted.iter().zip(evaluated) {
                if let Err(error) = result {
                    errors[row] = Some(error);
                }
            }
            return output(
                rows,
                sheet.output(&finished),
                errors,
                Paths {
                    hosted: hosted.len(),
                    ..counts
                },
            );
        }
        let mut shredder = Shredder::new(rows);
        for &row in &finished {
            match owned.get(row) {
                Some(Some(store)) => shredder.visit(store, 0, row),
                _ => shredder.visit(&sheet.object(row), 0, row),
            }
        }
        for (&row, result) in hosted.iter().zip(evaluated) {
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
