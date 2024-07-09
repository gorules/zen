#[rquickjs::module(rename_vars = "camelCase")]
pub mod zen_module {
    use rquickjs::Ctx;

    use crate::handler::function::module::throw_js_error;
    use crate::handler::function::serde::JsValue;

    #[rquickjs::function]
    pub fn evaluateExpression<'js>(
        ctx: Ctx<'js>,
        expression: String,
        context: JsValue,
    ) -> rquickjs::Result<JsValue> {
        let s = zen_expression::evaluate_expression(expression.as_str(), &context.0)
            .map_err(|err| throw_js_error(ctx, err.to_string()))?;

        Ok(JsValue(s))
    }

    #[rquickjs::function]
    pub fn evaluateUnaryExpression<'js>(
        ctx: Ctx<'js>,
        expression: String,
        context: JsValue,
    ) -> rquickjs::Result<bool> {
        let s = zen_expression::evaluate_unary_expression(expression.as_str(), &context.0)
            .map_err(|err| throw_js_error(ctx, err.to_string()))?;

        Ok(s)
    }
}
