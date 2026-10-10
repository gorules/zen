use super::{Ctx, Row};
use crate::decision_graph::tracer::ExpressionNodeTraceItem;
use crate::nodes::expression::ExpressionNodeTrace;
use crate::nodes::NodeResult;
use std::rc::Rc;
use zen_expression::variable::Variable;
use zen_types::decision::ExpressionNodeContent;

impl Row<'_> {
    pub(crate) fn expression(self, content: &ExpressionNodeContent) -> NodeResult {
        let mut ctx = Ctx::<ExpressionNodeTrace>::new(self);
        let result = Variable::empty_object();
        let mut session = ctx.row.session();

        for expression in content.expressions.iter() {
            if expression.key.is_empty() || expression.value.is_empty() {
                continue;
            }

            let value = match session.run_standard(&expression.value) {
                Ok(value) => value,
                Err(_) => {
                    return Err(ctx.error(format!(r#"Failed to evaluate expression: "{}""#, expression.value)))
                }
            };
            let value = match expression.value.contains('$') {
                true => value.deep_clone(),
                false => value,
            };
            ctx.trace(|trace| {
                trace.insert(
                    Rc::from(&*expression.key),
                    ExpressionNodeTraceItem {
                        result: value.clone(),
                    },
                );
            });

            session.insert_dollar(&expression.key, value.clone());

            result.dot_insert(&expression.key, value);
        }

        ctx.success(result)
    }
}
