use super::Row;
use crate::compiled::host::NodeResult;
use crate::compiled::trace::collect::Fact;
use std::sync::Arc;
use zen_expression::variable::Variable;
use zen_types::decision::ExpressionNodeContent;

impl Row<'_> {
    pub(crate) fn expression(self, content: &ExpressionNodeContent) -> NodeResult {
        let result = Variable::empty_object();
        let mut session = self.session();
        let mut results: Option<Vec<(Arc<str>, Variable)>> = self.traced().then(Vec::new);

        for expression in content.expressions.iter() {
            if expression.key.is_empty() || expression.value.is_empty() {
                continue;
            }

            let value = match session.run_standard(&expression.value) {
                Ok(value) => value,
                Err(_) => {
                    if let Some(results) = results {
                        self.push(Fact::Results(results));
                    }
                    return Err(self.error(crate::compiled::exact::Failure::expression(
                        &expression.value,
                    )));
                }
            };
            let value = match expression.value.contains('$') {
                true => value.deep_clone(),
                false => value,
            };
            if let Some(results) = results.as_mut() {
                results.push((expression.key.clone(), value.clone()));
            }

            session.insert_dollar(&expression.key, value.clone());

            result.dot_insert(&expression.key, value);
        }

        if let Some(results) = results {
            self.push(Fact::Results(results));
        }
        self.success(result)
    }
}
