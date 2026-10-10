use zen_expression::variable::Variable;

use super::Exact;
use crate::policy::blocks::{AssertionIr, ExecutionError};
use crate::workspace::types::{BlockTrace, ConditionTrace};

impl Exact<'_> {
    pub(crate) fn execute_assertion(&mut self, ir: &AssertionIr) -> Result<BlockTrace, ExecutionError> {
        let mut traces: Vec<ConditionTrace> = Vec::new();
        let result = self.evaluate_conditions(ir, &mut traces)?;

        if !ir.output.is_empty() {
            self.site.write(&ir.output, Variable::Bool(result));
        }

        Ok(BlockTrace::Assertion {
            result,
            conditions: traces,
        })
    }

    fn evaluate_conditions(
        &mut self,
        ir: &AssertionIr,
        traces: &mut Vec<ConditionTrace>,
    ) -> Result<bool, ExecutionError> {
        let mut results = Vec::with_capacity(ir.conditions.len());
        for condition in &ir.conditions {
            let cond_result = self
                .session
                .run_standard(&condition.expression)
                .map_err(|e| self.site.expression_error(&condition.expression, e))?
                .as_bool()
                .unwrap_or(false);
            traces.push(ConditionTrace {
                id: condition.id.clone(),
                result: cond_result,
            });
            results.push(cond_result);
        }
        Ok(ir.fold(&results))
    }
}
