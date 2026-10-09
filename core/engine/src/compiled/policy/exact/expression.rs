use zen_expression::variable::Variable;

use super::Exact;
use crate::policy::blocks::{ExecutionError, ExpressionIr};
use crate::workspace::types::BlockTrace;

impl Exact<'_> {
    pub(crate) fn execute_expression(&mut self, ir: &ExpressionIr) -> Result<BlockTrace, ExecutionError> {
        if ir.key.is_empty() || ir.value.is_empty() {
            return Ok(BlockTrace::Expression {
                property: ir.key.clone(),
                value: Variable::Null,
            });
        }

        let result = self
            .session
            .run_standard(&ir.value)
            .map_err(|e| self.site.expression_error(&ir.value, e))?;
        let traced = self.site.trace.then(|| result.deep_clone());
        self.site.write(&ir.key, result);

        Ok(BlockTrace::Expression {
            property: ir.key.clone(),
            value: traced.unwrap_or(Variable::Null),
        })
    }
}
