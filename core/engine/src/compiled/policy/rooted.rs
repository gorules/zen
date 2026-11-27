use crate::compiled::policy::artifact::{Artifact as EvalArtifact, Iteration};
use crate::compiled::policy::{BlockPlan, Native, PolicyPlan, Refused};
use crate::compiled::table::{Table, TableIr};
use crate::policy::blocks::{Block, BlockKind, DecisionTableIr, ExpressionIr};
use std::sync::Arc;
use zen_expression::lane::{LaneProgram, SourceInfo};
use zen_types::path::DotPath;

pub(super) struct Rooted {
    pub(super) rule: Arc<Block>,
    pub(super) plan: BlockPlan,
    pub(super) gate: usize,
}

struct Alias<'r> {
    name: &'r str,
    path: &'r str,
}

impl Alias<'_> {
    fn key(&self, key: &Arc<str>) -> Option<Arc<str>> {
        if key.is_empty() {
            return Some(key.clone());
        }
        let rest = DotPath::tail(key, self.name).filter(|rest| !rest.is_empty())?;
        Some(Arc::from(format!("{}.{rest}", self.path)))
    }

    fn source(&self, source: &Arc<str>, unary: bool) -> Option<Arc<str>> {
        SourceInfo::renamed(source, unary, self.name, self.path).map(Arc::from)
    }

    fn table(&self, ir: &DecisionTableIr) -> Option<DecisionTableIr> {
        let mut renamed = ir.clone();
        for input in renamed.inputs.iter_mut() {
            if let Some(field) = input.field.as_ref() {
                input.field = Some(self.source(field, false)?);
            }
        }
        for output in renamed.outputs.iter_mut() {
            output.field = self.key(&output.field)?;
        }
        for rule in renamed.rules.iter_mut() {
            for input in &ir.inputs {
                if let Some(cell) = rule.get_mut(&input.id) {
                    *cell = self.source(cell, true)?;
                }
            }
            for output in &ir.outputs {
                if let Some(cell) = rule.get_mut(&output.id) {
                    *cell = self.source(cell, false)?;
                }
            }
        }
        Some(renamed)
    }

    fn keys(&self, kind: &BlockKind) -> Option<BlockKind> {
        Some(match kind {
            BlockKind::Expression(ir) => BlockKind::Expression(Arc::new(ExpressionIr {
                id: ir.id.clone(),
                key: self.key(&ir.key)?,
                value: ir.value.clone(),
            })),
            BlockKind::Assertion(ir) => {
                let mut renamed = ir.as_ref().clone();
                renamed.output = self.key(&ir.output)?;
                BlockKind::Assertion(Arc::new(renamed))
            }
            BlockKind::Match(ir) => {
                let mut renamed = ir.as_ref().clone();
                renamed.key = self.key(&ir.key)?;
                BlockKind::Match(Arc::new(renamed))
            }
            BlockKind::DecisionTable(ir) => {
                let mut renamed = ir.as_ref().clone();
                for output in renamed.outputs.iter_mut() {
                    output.field = self.key(&output.field)?;
                }
                BlockKind::DecisionTable(Arc::new(renamed))
            }
        })
    }

    fn kind(&self, kind: &BlockKind) -> Option<BlockKind> {
        Some(match kind {
            BlockKind::Expression(ir) => BlockKind::Expression(Arc::new(ExpressionIr {
                id: ir.id.clone(),
                key: self.key(&ir.key)?,
                value: self.source(&ir.value, false)?,
            })),
            BlockKind::Assertion(ir) => {
                let mut renamed = ir.as_ref().clone();
                renamed.output = self.key(&ir.output)?;
                for condition in renamed.conditions.iter_mut() {
                    condition.expression = self.source(&condition.expression, false)?;
                }
                BlockKind::Assertion(Arc::new(renamed))
            }
            BlockKind::Match(ir) => {
                let mut renamed = ir.as_ref().clone();
                renamed.key = self.key(&ir.key)?;
                for arm in renamed.arms.iter_mut() {
                    arm.condition = self.source(&arm.condition, false)?;
                    arm.value = self.source(&arm.value, false)?;
                }
                BlockKind::Match(Arc::new(renamed))
            }
            BlockKind::DecisionTable(ir) => BlockKind::DecisionTable(Arc::new(self.table(ir)?)),
        })
    }
}

impl Rooted {
    fn plain(program: &LaneProgram, path: &str, owner: Option<&str>) -> bool {
        let p = program.program();
        let owned = owner.map(|owner| format!("{path}.{owner}"));
        !(p.writes_env || p.chain || p.opaque() || p.rows)
            && p.site_keys.iter().flatten().all(|key| {
                key != path
                    && !key.starts_with('$')
                    && !key.contains('[')
                    && owned
                        .as_deref()
                        .is_none_or(|owned| !DotPath::overlaps(key, owned))
            })
    }

    fn programs(plan: &BlockPlan) -> Option<Vec<&LaneProgram>> {
        match (plan.native(), &plan.table) {
            (Some(native), _) => Some(match native {
                Native::Expression { program, .. } => vec![program.as_ref()],
                Native::Assertion { conditions, .. } => conditions.iter().collect(),
                Native::Match {
                    conditions, values, ..
                } => conditions.iter().chain(values.iter()).flatten().collect(),
            }),
            (None, Some(table)) => Some(table.programs()),
            (None, None) => None,
        }
    }

    fn native(rule: &Arc<Block>) -> BlockPlan {
        BlockPlan {
            native: Native::compile(rule),
            iterated: false,
            table: match &rule.kind {
                BlockKind::DecisionTable(ir) if !ir.draws() => {
                    Table::compile(TableIr::policy(ir)).ok()
                }
                _ => None,
            },
            member: None,
            iteration: None,
        }
    }

    fn block(rule: &Block, iteration: &Iteration) -> Option<(Arc<Block>, BlockPlan)> {
        let (name, path, owner) = iteration;
        let alias = Alias { name, path };
        let renamed = alias.kind(&rule.kind).map(|kind| {
            let rule = Arc::new(Block {
                id: rule.id.clone(),
                kind,
            });
            let plan = Self::native(&rule);
            (rule, plan)
        });
        if let Some((rule, plan)) = renamed {
            let plain = Self::programs(&plan).is_none_or(|programs| {
                programs
                    .iter()
                    .all(|program| Self::plain(program, path, owner.as_deref()))
            });
            if plain {
                return Some((rule, plan));
            }
        }
        let rule = Arc::new(Block {
            id: rule.id.clone(),
            kind: alias.keys(&rule.kind)?,
        });
        let plan = BlockPlan {
            native: Err(Refused::Iterated),
            iterated: false,
            table: None,
            member: None,
            iteration: None,
        };
        Some((rule, plan))
    }

    pub(super) fn compile(
        artifact: &EvalArtifact,
        rules: &[Arc<Block>],
        plans: &[BlockPlan],
    ) -> (Vec<Option<Rooted>>, Vec<Arc<str>>) {
        let related = |a: &str, b: &str| DotPath::overlaps(a, b);
        let single = |path: &str| {
            artifact.input_schema.single(path)
                && !artifact
                    .reference_fields
                    .iter()
                    .any(|field| related(&field.path, path) || related(&field.target, path))
        };
        let mut gates: Vec<Arc<str>> = Vec::new();
        let mut rooted: Vec<Option<Rooted>> = rules
            .iter()
            .zip(plans)
            .map(|(rule, plan)| {
                let iteration = plan
                    .iteration
                    .as_ref()
                    .filter(|(_, path, _)| single(path))?;
                let (rule, plan) = Self::block(rule, iteration)?;
                let gate = match gates.iter().position(|gate| *gate == iteration.1) {
                    Some(gate) => gate,
                    None => {
                        gates.push(iteration.1.clone());
                        gates.len() - 1
                    }
                };
                Some(Rooted { rule, plan, gate })
            })
            .collect();
        let foreign = |gate: usize, path: &str| {
            rules
                .iter()
                .zip(plans)
                .enumerate()
                .any(|(block, (rule, plan))| {
                    let own = rooted[block].as_ref().is_some_and(|r| r.gate == gate);
                    let listed = plan
                        .iteration
                        .as_ref()
                        .is_some_and(|(_, list, _)| !own && related(list, path));
                    let writes = !own
                        && plan.iteration.is_none()
                        && rule
                            .kind
                            .write_sites()
                            .iter()
                            .any(|site| related(&site.path, path));
                    listed || writes
                })
        };
        let clashing: Vec<bool> = gates
            .iter()
            .enumerate()
            .map(|(gate, path)| foreign(gate, path))
            .collect();
        for slot in rooted.iter_mut() {
            if slot.as_ref().is_some_and(|r| clashing[r.gate]) {
                *slot = None;
            }
        }
        (rooted, gates)
    }
}

impl PolicyPlan {
    pub(super) fn flat(&self, block: usize) -> &BlockPlan {
        match &self.rooted[block] {
            Some(rooted) => &rooted.plan,
            None => &self.plans[block],
        }
    }

    pub(super) fn rule(&self, block: usize) -> &Arc<Block> {
        match &self.rooted[block] {
            Some(rooted) => &rooted.rule,
            None => &self.blocks.rules[block],
        }
    }

    pub(super) fn flats(&self) -> impl Iterator<Item = &BlockPlan> {
        (0..self.plans.len()).map(|block| self.flat(block))
    }

    pub(super) fn gate(&self, block: usize) -> Option<usize> {
        self.rooted[block].as_ref().map(|rooted| rooted.gate)
    }
}
