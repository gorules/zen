use std::sync::Arc;

use ahash::HashSet;
use serde::{Deserialize, Serialize};
use zen_expression::intellisense::IntelliSense;
use zen_expression::variable::{Variable, VariableType};
use zen_expression::Isolate;

use crate::policy::types::{
    BlockTrace, ConditionTrace, Cursor, CursorTarget, Diagnostic, DiagnosticCode, ExpressionKind,
    NlExpression,
};

use crate::policy::ArcStrTrim;

use super::context::{AnalysisContext, ExecutionContext, ExecutionError};
use super::{
    Block, BlockKind, BlockReadPlan, ConditionalReads, ExpressionLocation, ParseContext,
    ReadFlattenFn, WriteSite, WriteTarget,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionDoc {
    pub output: Arc<str>,
    pub conditions: Vec<ConditionDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionDoc {
    pub id: Arc<str>,
    pub expression: Arc<str>,
    pub operator: ConditionOperatorDoc,
    #[serde(default)]
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConditionOperatorDoc {
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct AssertionIr {
    pub output: Arc<str>,
    pub conditions: Vec<AssertionCondition>,
}

#[derive(Debug, Clone)]
pub struct AssertionCondition {
    pub id: Arc<str>,
    pub expression: Arc<str>,
    pub operator: ConditionOperator,
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionOperator {
    And,
    Or,
}

use crate::policy::MAX_RECURSION_DEPTH;

impl ConditionOperator {
    fn apply(&self, left: bool, right: bool) -> bool {
        match self {
            ConditionOperator::And => left && right,
            ConditionOperator::Or => left || right,
        }
    }
}

impl AssertionIr {
    pub(crate) fn parse(
        id: &Arc<str>,
        doc: &AssertionDoc,
        policy_path: &Arc<str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Block {
        let mut cx = ParseContext {
            policy_path,
            block_id: id,
            diagnostics,
        };
        let mut output = doc.output.trimmed();

        if output.is_empty() {
            cx.block_warning(
                DiagnosticCode::InvalidWritePath,
                "assertion has no output property path",
            );
        } else if let Err(reason) = WriteTarget::validate_path(&output) {
            cx.block_error(
                DiagnosticCode::InvalidWritePath,
                format!("invalid write path '{output}': {reason}"),
            );
            output = Arc::from("");
        }

        if doc.conditions.is_empty() {
            cx.block_error(DiagnosticCode::EmptyBlock, "assertion has no conditions");
        }

        let conditions: Vec<_> = doc
            .conditions
            .iter()
            .filter_map(|c| {
                let expr = c.expression.trimmed();
                if expr.is_empty() {
                    cx.expression_warning(
                        &c.id,
                        DiagnosticCode::EmptyBlock,
                        "assertion condition is empty",
                    );
                    return None;
                }
                if (c.depth as usize) >= MAX_RECURSION_DEPTH {
                    cx.expression_error(
                        &c.id,
                        DiagnosticCode::MaxDepthExceeded,
                        format!(
                            "assertion condition depth {} exceeds maximum of {}",
                            c.depth, MAX_RECURSION_DEPTH
                        ),
                    );
                    return None;
                }
                Some(AssertionCondition {
                    id: c.id.clone(),
                    expression: expr,
                    operator: match c.operator {
                        ConditionOperatorDoc::And => ConditionOperator::And,
                        ConditionOperatorDoc::Or => ConditionOperator::Or,
                    },
                    depth: c.depth,
                })
            })
            .collect();

        Block {
            id: id.clone(),
            kind: BlockKind::Assertion(Arc::new(AssertionIr { output, conditions })),
        }
    }

    pub(super) fn expressions(&self, block_id: &Arc<str>) -> Vec<ExpressionLocation> {
        self.conditions
            .iter()
            .filter_map(|c| {
                ExpressionLocation::try_new(
                    block_id.clone(),
                    c.id.clone(),
                    ExpressionKind::Standard,
                    c.expression.clone(),
                )
            })
            .collect()
    }

    pub(super) fn write_sites(&self) -> Vec<WriteSite> {
        if self.output.is_empty() {
            return Vec::new();
        }
        let contributing: HashSet<Arc<str>> =
            self.conditions.iter().map(|c| c.id.clone()).collect();
        vec![WriteSite {
            path: self.output.clone(),
            expression_id: None,
            resolved_type: VariableType::Bool,
            contributing_expr_ids: contributing,
        }]
    }

    pub(super) fn analyze(&self, cx: &mut AnalysisContext) {
        for condition in &self.conditions {
            cx.analyze_standard(&condition.expression, Some(condition.id.clone()));
        }
        if !self.output.is_empty() {
            cx.record_write(
                self.output.clone(),
                VariableType::Bool,
                None,
                Some(CursorTarget::AssertionOutput),
            );
        }
    }

    pub(super) fn write_target(&self, path: &str) -> Option<CursorTarget> {
        (!self.output.is_empty() && self.output.as_ref() == path)
            .then_some(CursorTarget::AssertionOutput)
    }

    pub(super) fn read_plan(&self, flatten: &mut ReadFlattenFn) -> BlockReadPlan {
        let mut unconditional = Vec::new();
        for condition in &self.conditions {
            unconditional.extend(flatten(&condition.expression, ExpressionKind::Standard));
        }
        BlockReadPlan {
            unconditional: BlockReadPlan::dedup(unconditional),
            conditional: ConditionalReads::None,
        }
    }

    pub(super) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();
        let mut traces: Vec<ConditionTrace> = Vec::new();
        let result = self.evaluate_conditions(&mut isolate, cx, &mut traces)?;

        if !self.output.is_empty() {
            cx.write(&self.output, Variable::Bool(result));
        }

        Ok(BlockTrace::Assertion {
            result,
            conditions: traces,
        })
    }

    pub(super) fn nl(
        &self,
        policy_path: &Arc<str>,
        block_id: &Arc<str>,
        scope: &VariableType,
        is: &mut IntelliSense,
    ) -> Vec<NlExpression> {
        self.conditions
            .iter()
            .filter(|condition| !condition.expression.is_empty())
            .map(|condition| {
                NlExpression::project(
                    is,
                    policy_path,
                    block_id,
                    CursorTarget::Expression {
                        id: condition.id.clone(),
                    },
                    ExpressionKind::Standard,
                    condition.expression.as_ref(),
                    scope,
                )
            })
            .collect()
    }

    pub(super) fn resolve_cursor(
        &self,
        cursor: &Cursor,
        scope: VariableType,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        match &cursor.target {
            CursorTarget::AssertionOutput => {
                if self.output.is_empty() {
                    return None;
                }
                Some((self.output.clone(), ExpressionKind::Standard, scope))
            }
            CursorTarget::Expression { id } => {
                let cond = self.conditions.iter().find(|c| c.id == *id)?;
                Some((cond.expression.clone(), ExpressionKind::Unary, scope))
            }
            _ => None,
        }
    }

    fn evaluate_conditions(
        &self,
        isolate: &mut Isolate,
        cx: &ExecutionContext,
        traces: &mut Vec<ConditionTrace>,
    ) -> Result<bool, ExecutionError> {
        if self.conditions.is_empty() {
            return Ok(false);
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut acc = false;
        let mut started = false;
        let mut depth = 0u32;
        let mut combine_next = ConditionOperator::And;

        for condition in &self.conditions {
            while condition.depth > depth {
                stack.push(Frame {
                    acc,
                    started,
                    combine: combine_next,
                });
                acc = false;
                started = false;
                combine_next = ConditionOperator::And;
                depth += 1;
            }
            while condition.depth < depth {
                if let Some(frame) = stack.pop() {
                    acc = frame.close(acc);
                    started = true;
                }
                depth -= 1;
            }

            let cond_result = isolate
                .run_standard(&condition.expression)
                .map_err(|e| cx.expression_error(&condition.expression, e))?
                .as_bool()
                .unwrap_or(false);

            traces.push(ConditionTrace {
                id: condition.id.clone(),
                result: cond_result,
            });

            acc = if started {
                combine_next.apply(acc, cond_result)
            } else {
                cond_result
            };
            started = true;
            combine_next = condition.operator;
        }

        while let Some(frame) = stack.pop() {
            acc = frame.close(acc);
        }

        Ok(acc)
    }
}

struct Frame {
    acc: bool,
    started: bool,
    combine: ConditionOperator,
}

impl Frame {
    fn close(&self, group: bool) -> bool {
        if self.started {
            self.combine.apply(self.acc, group)
        } else {
            group
        }
    }
}
