use std::sync::Arc;

use zen_expression::variable::Variable;

use super::Exact;
use crate::policy::blocks::{ExecutionError, MatchIr, MatchSelection};
use crate::workspace::types::{BlockTrace, ConditionTrace};

impl Exact<'_> {
    pub(crate) fn select_match(&mut self, ir: &MatchIr) -> Result<MatchSelection, ExecutionError> {
        let mut arms: Vec<ConditionTrace> = Vec::new();
        let mut matched_arm: Option<Arc<str>> = None;

        for arm in &ir.arms {
            if matched_arm.is_some() && !self.site.extras {
                break;
            }
            let matches = if arm.condition.is_empty() {
                true
            } else {
                match self.session.run_standard(&arm.condition) {
                    Ok(value) => value.as_bool().unwrap_or(false),
                    Err(_) if self.site.extras && matched_arm.is_some() => false,
                    Err(e) => return Err(self.site.expression_error(&arm.condition, e)),
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

    pub(crate) fn commit_match(
        &mut self,
        ir: &MatchIr,
        selection: &MatchSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        let matched = selection
            .matched_arm
            .as_ref()
            .and_then(|id| ir.arms.iter().find(|a| &a.id == id));
        let value = match matched {
            Some(arm) if !arm.value.is_empty() => self
                .session
                .run_standard(&arm.value)
                .map_err(|e| self.site.expression_error(&arm.value, e))?,
            _ => Variable::Null,
        };

        let traced = self.site.trace.then(|| value.deep_clone());
        if !ir.key.is_empty() {
            self.site.write(&ir.key, value);
        }

        Ok(BlockTrace::Match {
            matched_arm: selection.matched_arm.clone(),
            value: traced.unwrap_or(Variable::Null),
            arms: if self.site.trace {
                selection.arms.clone()
            } else {
                Vec::new()
            },
        })
    }

    pub(crate) fn execute_match(&mut self, ir: &MatchIr) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select_match(ir)?;
        self.commit_match(ir, &selection)
    }
}
