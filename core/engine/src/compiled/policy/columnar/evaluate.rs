use crate::compiled::policy::entity::Entity;
use crate::compiled::policy::{PolicyPlan, Rows};
use crate::compiled::shred::{Shred, Shredder};
use crate::policy::evaluator::{EvalArtifact, Iteration};
use std::cell::{OnceCell, RefCell};
use crate::workspace::types::{EvaluateRequest, EvaluationError};
use std::sync::Arc;
use zen_expression::lane::{Columns, Kind as LaneKind, LaneProgram};
use zen_expression::Variable;
use super::{Pass, PolicyColumnarOutput, Sheet};

impl PolicyPlan {
    pub(super) fn scalar(program: &LaneProgram) -> bool {
        let p = program.program();
        let regs = match p.outputs.is_empty() {
            true => std::slice::from_ref(&p.out),
            false => p.outputs.as_slice(),
        };
        regs.iter().all(|r| matches!(p.kinds.get(*r as usize), Some(LaneKind::Num | LaneKind::Bool | LaneKind::Str)))
    }

    fn safe(&self, artifact: &EvalArtifact) -> &[Iteration] {
        self.safe.get_or_init(|| {
            let mut found: Vec<Iteration> = Vec::new();
            for (name, path, owner) in self.iterations.iter().flatten() {
                if !found.iter().any(|(_, p, _)| p == path) {
                    found.push((name.clone(), path.clone(), owner.clone()));
                }
            }
            found.retain(|(name, path, _)| self.members_safe(name, path) && self.readers_safe(artifact, path));
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
                let entry = sheet.entries.iter().position(|e| e.input && e.path == path)?;
                if sheet.entries.iter().any(|e| Sheet::related(&e.path, &path) == Some(std::cmp::Ordering::Greater)) {
                    return None;
                }
                Entity::new(name.clone(), path.clone(), owner.clone(), entry, sheet.inputs[entry])
                    .or_else(|| Shred::of(&sheet.inputs[entry], sheet.rows).map(|shred| Entity::shredded(name, path, owner, entry, shred)))
            })
            .collect()
    }

    fn members_safe(&self, name: &str, path: &str) -> bool {
        let iterations = self.iterations.iter();
        let prefix = format!("{name}.");
        iterations.enumerate().all(|(block, iteration)| {
            let Some((_, own, _)) = iteration else {
                return true;
            };
            if Sheet::related(own, path) == Some(std::cmp::Ordering::Greater) {
                return false;
            }
            if own.as_ref() != path {
                return true;
            }
            if self.members[block].is_none() && self.tables[block].is_none() {
                return false;
            }
            let Some((programs, keys)) = Pass::block(self, block) else {
                return false;
            };
            let keyed = keys.iter().all(|key| key.is_empty() || key.strip_prefix(prefix.as_str()).is_some_and(|f| !f.is_empty()));
            keyed
                && programs.into_iter().all(|program| {
                    program.program().site_keys.iter().all(|k| match k.as_deref() {
                        None => true,
                        Some(k) if k == name || k.starts_with('$') => false,
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
            if self.iterations[block].as_ref().is_some_and(|(_, own, _)| own.as_ref() == path) {
                return true;
            }
            let writes = rule.kind.write_sites().iter().any(|site| Sheet::related(&site.path, path).is_some());
            let reads = artifact.reads.get(&self.blocks.refs[block]).map_or(Vec::new(), |reads| {
                reads.iter().filter_map(|read| Sheet::related(&read.path, path)).collect::<Vec<_>>()
            });
            if writes || reads.contains(&std::cmp::Ordering::Less) {
                return false;
            }
            reads.is_empty()
                || matches!(
                    (&self.natives[block], self.iterated[block], &self.tables[block], &self.iterations[block]),
                    (Some(_), false, _, None) | (None, _, Some(_), None)
                )
        })
    }

    /// The policy over `original`, as given or `prepared` (see
    /// [`super::prepare`]): the prepared columns evaluated, the rows forced
    /// (and any row hosted) evaluated as `original`'s rows.
    pub(crate) fn evaluate_columns<'a>(
        &self,
        artifact: &EvalArtifact,
        policy_path: &Arc<str>,
        goals: &[Arc<str>],
        original: &'a Columns<'a>,
        prepared: Option<&'a super::prepare::Prepared<'a>>,
        reasons: bool,
    ) -> PolicyColumnarOutput<'a> {
        let columns = prepared.map_or(original, |prepared| &prepared.hydrated);
        let rows = columns.rows;
        let request = |row: usize| EvaluateRequest {
            policy_path: policy_path.clone(),
            input: original.row(row),
            goals: goals.to_vec(),
            trace: false,
        };
        let sheet = Sheet::new(columns);
        let Some(mut sheet) = sheet else {
            let requests: Vec<EvaluateRequest> = (0..rows).map(request).collect();
            return PolicyColumnarOutput::from_results(rows, self.evaluate(artifact, &requests));
        };
        let mut errors: Vec<Option<EvaluationError>> = (0..rows).map(|_| None).collect();
        let stores: Vec<OnceCell<Variable>> = (0..rows).map(|_| OnceCell::new()).collect();
        sheet.entities = self.entities(artifact, &sheet);
        // Prepared references are resolved: read as any other column.
        if prepared.is_none() {
            sheet.refs = artifact.reference_fields.iter().map(|field| field.path.clone()).collect();
        }
        let shared = (!artifact.reference_fields.is_empty() && prepared.is_none())
            || self.iterations.iter().enumerate().any(|(block, iteration)| match iteration {
                Some((_, path, _)) => !sheet.entities.iter().any(|e| e.path == *path),
                None => self.natives[block].is_none() && self.tables[block].is_none(),
            });
        let mut fallback = vec![false; rows];
        let mut erred = false;
        let sure = match (shared, prepared) {
            (true, _) => None,
            (false, Some(prepared)) => artifact.sure_prepared(&prepared.converted, columns, goals),
            (false, None) => Entity::sure(artifact, columns, &sheet.entities, goals),
        };
        let mut why = super::Fallbacks::default();
        for &row in sheet.entities.iter().filter_map(|entity| entity.hosted.as_deref()).flatten() {
            if !fallback[row] {
                fallback[row] = true;
                why.add(super::FallbackReason::Entity);
            }
        }
        if let Some(prepared) = prepared {
            for (row, reason) in prepared.forced.iter().enumerate() {
                if let (Some(reason), false) = (reason, fallback[row]) {
                    fallback[row] = true;
                    why.add(reason.clone());
                }
            }
        }
        for row in 0..rows {
            if sure.as_ref().is_some_and(|sure| sure[row]) || (prepared.is_some() && fallback[row]) {
                continue;
            }
            let req = request(row);
            // Prepared columns hold the row converted: checked here only.
            if prepared.is_none() && artifact.input_schema.convert_dates(&req.input).is_some() {
                fallback[row] = true;
                why.add(super::FallbackReason::Converted);
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
            (true, _) => (0..rows).filter(|&row| errors[row].is_none() && !fallback[row]).collect(),
            (false, true) => (0..rows).collect(),
            (false, false) => (0..rows).filter(|&row| !fallback[row]).collect(),
        };
        let mut runner = Self::RUNNER.with_borrow_mut(std::mem::take);
        let (sheet, fallback, errors, owned, fell, counts, why, reasons) = {
            let runner = &mut runner;
            let mut pass = Pass {
                plan: self,
                artifact,
                entry: policy_path,
                sheet,
                runner,
                fallback,
                arms: Vec::new(),
                stores: &stores,
                state: Rows {
                    hosts: Vec::new(),
                    picks: Vec::new(),
                    failures: errors,
                },
                synced: vec![0; rows],
                shared,
                base,
                chosen: Vec::new(),
                members: Vec::new(),
                hosting: Vec::new(),
                kin: Vec::new(),
                delegated: false,
                fell: 0,
                why,
                at: None,
                reasons,
                counts: (0, 0),
                listed: RefCell::new(Vec::new()),
                owners: self.iterations.iter().map(|iteration| iteration.as_ref().map(|(_, path, _)| path.clone())).collect(),
            };
            pass.walk(self.root.clone(), live);
            let owning = |row: usize| pass.state.hosts.get(row).is_some_and(Option::is_some) || pass.stores[row].get().is_some();
            let owns = (0..rows).any(owning);
            if owns {
                (0..pass.sheet.entities.len()).for_each(|at| pass.sheet.retire(at));
            }
            let owned: Vec<Option<Variable>> = match owns {
                false => Vec::new(),
                true => (0..rows)
                    .map(|row| match pass.state.hosts.get(row).is_some_and(Option::is_some) || pass.stores[row].get().is_some() {
                        true => pass.host(row).map(|host| host.state.env().shallow_clone()).and(pass.stores[row].get().cloned()),
                        false => None,
                    })
                    .collect(),
            };
            erred |= pass.delegated;
            let reasons = match reasons {
                true => pass.reasons(),
                false => Vec::new(),
            };
            (pass.sheet, pass.fallback, pass.state.failures, owned, pass.fell, pass.counts, pass.why, reasons)
        };
        Self::RUNNER.with_borrow_mut(|slot| *slot = runner);
        let mut errors = errors;
        let clean = |row: usize| !erred || errors[row].is_none();
        let (hosted, finished): (Vec<usize>, Vec<usize>) = match settled && fell == 0 && !erred {
            true => (Vec::new(), (0..rows).collect()),
            false => (
                (0..rows).filter(|&row| fallback[row] && clean(row)).collect(),
                (0..rows).filter(|&row| !fallback[row] && clean(row)).collect(),
            ),
        };
        if hosted.is_empty() && owned.iter().all(Option::is_none) {
            let columns = sheet.output(&finished);
            let exact = sheet.exact(&finished);
            return PolicyColumnarOutput {
                rows,
                columns,
                errors,
                hosted: 0,
                fallbacks: Vec::new(),
                reasons,
                typed: counts.0,
                laned: counts.1,
                exact,
            };
        }
        let requests: Vec<EvaluateRequest> = hosted.iter().map(|&row| request(row)).collect();
        let mut shredder = Shredder::new(rows);
        for &row in &finished {
            match owned.get(row) {
                Some(Some(store)) => shredder.row(store, row),
                // As a row: a written null (which columns drop) kept whole.
                _ => shredder.row(&sheet.object(row), row),
            }
        }
        for (&row, result) in hosted.iter().zip(self.evaluate(artifact, &requests)) {
            match result {
                Ok(result) => shredder.row(&result.output, row),
                Err(error) => errors[row] = Some(error),
            }
        }
        let exact = shredder.exact();
        PolicyColumnarOutput {
            rows,
            columns: Sheet::columns(shredder.finish()),
            errors,
            hosted: hosted.len(),
            fallbacks: why.0,
            reasons,
            typed: counts.0,
            laned: counts.1,
            exact,
        }
    }
}
