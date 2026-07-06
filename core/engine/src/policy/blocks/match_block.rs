use std::sync::Arc;

use ahash::HashSet;
use serde::{Deserialize, Serialize};
use zen_expression::intellisense::{ArmTest, IntelliSense, NumberCover};
use zen_expression::variable::{Variable, VariableType};

use crate::policy::queries::scope::VariableTypeScope;

use crate::policy::types::{
    BlockTrace, ConditionTrace, Cursor, CursorTarget, Diagnostic, DiagnosticCode, ExpressionKind,
    NlExpression,
};

use crate::policy::ArcStrTrim;

use super::context::{AnalysisContext, ExecutionContext, ExecutionError, InstanceSource};
use super::{
    ArmReads, Block, BlockKind, BlockReadPlan, ConditionalReads, ExpressionLocation, ParseContext,
    ReadFlattenFn, WriteSite, WriteTarget,
};

pub(crate) struct MatchSelection {
    pub(crate) matched_arm: Option<Arc<str>>,
    arms: Vec<ConditionTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchDoc {
    #[serde(default)]
    pub key: Arc<str>,
    #[serde(default)]
    pub arms: Vec<MatchArmDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchArmDoc {
    pub id: Arc<str>,
    #[serde(default)]
    pub condition: Arc<str>,
    #[serde(default)]
    pub value: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct MatchIr {
    pub key: Arc<str>,
    pub arms: Vec<MatchArm>,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub id: Arc<str>,
    pub condition: Arc<str>,
    pub value: Arc<str>,
}

impl MatchIr {
    pub(crate) fn parse(
        id: &Arc<str>,
        doc: &MatchDoc,
        policy_path: &Arc<str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Block {
        let mut cx = ParseContext {
            policy_path,
            block_id: id,
            diagnostics,
        };
        let mut key = doc.key.trimmed();

        if key.is_empty() {
            cx.block_warning(
                DiagnosticCode::InvalidWritePath,
                "match has no target property path",
            );
        } else if let Err(reason) = WriteTarget::validate_path(&key) {
            cx.target_error(
                id,
                CursorTarget::MatchTarget,
                Some((0, key.chars().count() as u32)),
                DiagnosticCode::InvalidWritePath,
                format!("invalid write path '{key}': {reason}"),
            );
            key = Arc::from("");
        }

        if doc.arms.is_empty() {
            cx.block_warning(DiagnosticCode::EmptyBlock, "match has no arms");
        }

        let mut arms = Vec::with_capacity(doc.arms.len());
        for arm in &doc.arms {
            let value = arm.value.trimmed();
            if value.is_empty() {
                cx.expression_warning(
                    &arm.id,
                    DiagnosticCode::EmptyBlock,
                    "match arm has no value; it writes null when selected",
                );
            }
            arms.push(MatchArm {
                id: arm.id.clone(),
                condition: arm.condition.trimmed(),
                value,
            });
        }

        Block {
            id: id.clone(),
            kind: BlockKind::Match(Arc::new(MatchIr { key, arms })),
        }
    }

    pub(super) fn expressions(&self, block_id: &Arc<str>) -> Vec<ExpressionLocation> {
        let mut out = Vec::new();
        for arm in &self.arms {
            if let Some(loc) = ExpressionLocation::try_new(
                block_id.clone(),
                arm.id.clone(),
                ExpressionKind::Standard,
                arm.condition.clone(),
            ) {
                out.push(loc);
            }
            if let Some(loc) = ExpressionLocation::try_new(
                block_id.clone(),
                arm.id.clone(),
                ExpressionKind::Standard,
                arm.value.clone(),
            ) {
                out.push(loc);
            }
        }
        out
    }

    pub(super) fn write_sites(&self) -> Vec<WriteSite> {
        if self.key.is_empty() {
            return Vec::new();
        }
        let contributing: HashSet<Arc<str>> = self.arms.iter().map(|a| a.id.clone()).collect();
        vec![WriteSite {
            path: self.key.clone(),
            expression_id: None,
            resolved_type: VariableType::Any,
            contributing_expr_ids: contributing,
        }]
    }

    pub(super) fn write_value_expressions(&self, key: &str) -> Vec<Arc<str>> {
        if self.key.as_ref() != key {
            return Vec::new();
        }
        self.arms
            .iter()
            .filter(|a| !a.value.is_empty())
            .map(|a| a.value.clone())
            .collect()
    }

    pub(super) fn read_plan(&self, flatten: &mut ReadFlattenFn) -> BlockReadPlan {
        let mut unconditional = Vec::new();
        let mut arms = Vec::new();
        for arm in &self.arms {
            if !arm.condition.is_empty() {
                unconditional.extend(flatten(&arm.condition, ExpressionKind::Standard));
            }
            let value_reads = if arm.value.is_empty() {
                Vec::new()
            } else {
                flatten(&arm.value, ExpressionKind::Standard)
            };
            arms.push(ArmReads {
                arm_id: arm.id.clone(),
                value_reads: BlockReadPlan::dedup(value_reads),
            });
        }
        BlockReadPlan {
            unconditional: BlockReadPlan::dedup(unconditional),
            conditional: ConditionalReads::Match(Arc::from(arms)),
        }
    }

    pub(super) fn write_target(&self, path: &str) -> Option<CursorTarget> {
        (!self.key.is_empty() && self.key.as_ref() == path).then_some(CursorTarget::MatchTarget)
    }

    pub(super) fn analyze(&self, cx: &mut AnalysisContext) {
        let mut value_types: Vec<VariableType> = Vec::new();
        let mut has_default = false;
        let mut tests: Vec<ArmTest> = Vec::new();

        for arm in &self.arms {
            if arm.condition.is_empty() {
                has_default = true;
            } else {
                let analysis = cx.analyze_standard(&arm.condition, Some(arm.id.clone()));
                if !matches!(analysis.return_type, VariableType::Bool | VariableType::Any) {
                    cx.error(
                        DiagnosticCode::TypeMismatch,
                        Some(arm.id.clone()),
                        None,
                        format!(
                            "match arm condition must return a boolean, got `{}`",
                            analysis.return_type
                        ),
                    );
                }
                tests.push(cx.arm_test(&arm.condition));
            }
            if arm.value.is_empty() {
                value_types.push(VariableType::Null);
            } else {
                let analysis = cx.analyze_standard(&arm.value, Some(arm.id.clone()));
                value_types.push(analysis.return_type.clone());
            }
        }

        if self.key.is_empty() {
            return;
        }

        if !has_default && !Self::discriminant_covered(&tests, cx.scope()) {
            value_types.push(VariableType::Null);
            if cx.is_enriched() && !self.arms.is_empty() {
                cx.error_with_target(
                    DiagnosticCode::MissingDefaultBranch,
                    None,
                    None,
                    Some(CursorTarget::MatchTarget),
                    "match is not exhaustive: add a default `_` arm, or make the arms a provable discriminated union (every enum value, both booleans, or a gap-free numeric range)",
                );
            }
        }

        let resolved = cx.merge_types(
            &value_types,
            &self.key,
            None,
            Some(CursorTarget::MatchTarget),
        );
        let instance_source = self.arm_instance_source(cx);
        cx.record_write_sourced(
            self.key.clone(),
            resolved,
            None,
            Some(CursorTarget::MatchTarget),
            instance_source,
        );
    }

    fn arm_instance_source(&self, cx: &mut AnalysisContext) -> Option<InstanceSource> {
        let mut shared: Option<InstanceSource> = None;
        for arm in &self.arms {
            if arm.value.is_empty() {
                continue;
            }
            let source = cx.flow_source(&arm.value)?;
            match &shared {
                None => shared = Some(source),
                Some(existing) if *existing == source => {}
                Some(_) => return None,
            }
        }
        shared
    }

    fn discriminant_covered(tests: &[ArmTest], scope: &VariableType) -> bool {
        if tests.is_empty() {
            return false;
        }
        let mut shared_path: Option<&Vec<std::rc::Rc<str>>> = None;
        for test in tests {
            let path = match test {
                ArmTest::Enum { path, .. }
                | ArmTest::Bool { path, .. }
                | ArmTest::Number { path, .. } => path,
                _ => return false,
            };
            match shared_path {
                None => shared_path = Some(path),
                Some(p) if p == path => {}
                Some(_) => return false,
            }
        }
        let Some(path) = shared_path else {
            return false;
        };
        let dotted = path
            .iter()
            .map(|s| s.as_ref())
            .collect::<Vec<&str>>()
            .join(".");
        let resolved_type = scope.resolve_at(&dotted);
        let (resolved, nullable) = resolved_type.unwrap_nullable();
        if nullable {
            return false;
        }
        match resolved {
            VariableType::Enum(_, declared) => {
                let mut collected: HashSet<&str> = HashSet::default();
                for test in tests {
                    let ArmTest::Enum { values, .. } = test else {
                        return false;
                    };
                    collected.extend(values.iter().map(|v| v.as_ref()));
                }
                declared.iter().all(|d| collected.contains(d.as_ref()))
            }
            VariableType::Bool => {
                let (mut seen_true, mut seen_false) = (false, false);
                for test in tests {
                    let ArmTest::Bool { values, .. } = test else {
                        return false;
                    };
                    for value in values {
                        if *value {
                            seen_true = true;
                        } else {
                            seen_false = true;
                        }
                    }
                }
                seen_true && seen_false
            }
            VariableType::Number => {
                let mut cover: Option<NumberCover> = None;
                for test in tests {
                    let ArmTest::Number { cover: segment, .. } = test else {
                        return false;
                    };
                    match &mut cover {
                        Some(acc) => acc.merged_with(segment),
                        None => cover = Some(segment.clone()),
                    }
                }
                cover.is_some_and(|c| c.is_total())
            }
            _ => false,
        }
    }

    pub(crate) fn select(&self, cx: &ExecutionContext) -> Result<MatchSelection, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();

        let mut arms: Vec<ConditionTrace> = Vec::new();
        let mut matched_arm: Option<Arc<str>> = None;

        for arm in &self.arms {
            if matched_arm.is_some() && !cx.extras {
                break;
            }
            let matches = if arm.condition.is_empty() {
                true
            } else {
                match isolate.run_standard(&arm.condition) {
                    Ok(value) => value.as_bool().unwrap_or(false),
                    Err(_) if cx.extras && matched_arm.is_some() => false,
                    Err(e) => return Err(cx.expression_error(&arm.condition, e)),
                }
            };
            arms.push(ConditionTrace {
                id: arm.id.clone(),
                result: matches,
            });
            if matches && matched_arm.is_none() {
                matched_arm = Some(arm.id.clone());
            }
        }

        Ok(MatchSelection { matched_arm, arms })
    }

    pub(crate) fn commit(
        &self,
        cx: &ExecutionContext,
        selection: &MatchSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        let matched = selection
            .matched_arm
            .as_ref()
            .and_then(|id| self.arms.iter().find(|a| &a.id == id));
        let value = match matched {
            Some(arm) if !arm.value.is_empty() => {
                let mut isolate = cx.isolate.borrow_mut();
                isolate
                    .run_standard(&arm.value)
                    .map_err(|e| cx.expression_error(&arm.value, e))?
            }
            _ => Variable::Null,
        };

        let traced = cx.trace.then(|| value.deep_clone());
        if !self.key.is_empty() {
            cx.write(&self.key, value);
        }

        Ok(BlockTrace::Match {
            matched_arm: selection.matched_arm.clone(),
            value: traced.unwrap_or(Variable::Null),
            arms: if cx.trace {
                selection.arms.clone()
            } else {
                Vec::new()
            },
        })
    }

    pub(super) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select(cx)?;
        self.commit(cx, &selection)
    }

    pub(super) fn nl(
        &self,
        policy_path: &Arc<str>,
        block_id: &Arc<str>,
        scope: &VariableType,
        is: &mut IntelliSense,
    ) -> Vec<NlExpression> {
        let mut out = Vec::new();
        if !self.key.is_empty() {
            out.push(NlExpression::project(
                is,
                policy_path,
                block_id,
                CursorTarget::MatchTarget,
                ExpressionKind::Standard,
                self.key.as_ref(),
                scope,
            ));
        }
        for arm in &self.arms {
            if !arm.condition.is_empty() {
                out.push(NlExpression::project(
                    is,
                    policy_path,
                    block_id,
                    CursorTarget::Expression {
                        id: arm.id.clone(),
                    },
                    ExpressionKind::Standard,
                    arm.condition.as_ref(),
                    scope,
                ));
            }
            if !arm.value.is_empty() {
                out.push(NlExpression::project(
                    is,
                    policy_path,
                    block_id,
                    CursorTarget::MatchValue {
                        id: arm.id.clone(),
                    },
                    ExpressionKind::Standard,
                    arm.value.as_ref(),
                    scope,
                ));
            }
        }
        out
    }

    pub(super) fn resolve_cursor(
        &self,
        cursor: &Cursor,
        scope: VariableType,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        match &cursor.target {
            CursorTarget::MatchTarget => {
                (!self.key.is_empty()).then(|| (self.key.clone(), ExpressionKind::Standard, scope))
            }
            CursorTarget::Expression { id } => {
                let arm = self.arms.iter().find(|a| a.id == *id)?;
                (!arm.condition.is_empty())
                    .then(|| (arm.condition.clone(), ExpressionKind::Standard, scope))
            }
            CursorTarget::MatchValue { id } => {
                let arm = self.arms.iter().find(|a| a.id == *id)?;
                (!arm.value.is_empty())
                    .then(|| (arm.value.clone(), ExpressionKind::Standard, scope))
            }
            _ => None,
        }
    }
}
