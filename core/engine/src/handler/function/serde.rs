use crate::handler::function::error::ResultExt;
use ahash::{HashMap, HashMapExt};
use rquickjs::{Ctx, FromJs, IntoAtom, IntoJs, Type, Value as QValue};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::json;
use std::rc::Rc;
use zen_expression::variable::Variable;

#[derive(Debug)]
pub(crate) struct JsValue(pub(crate) Variable);

impl<'js> FromJs<'js> for JsValue {
    fn from_js(ctx: &Ctx<'js>, v: QValue<'js>) -> rquickjs::Result<Self> {
        let computed_value = match v.type_of() {
            Type::Uninitialized | Type::Undefined | Type::Null => Variable::Null,
            Type::Bool => {
                Variable::Bool(v.as_bool().or_throw_msg(ctx, "failed to convert to bool")?)
            }
            Type::Int => Variable::Number(Decimal::from(
                v.as_int().or_throw_msg(ctx, "failed to convert to int")?,
            )),
            Type::BigInt => Variable::Number(Decimal::from(
                v.into_big_int()
                    .map(|b| b.clone().to_i64().ok())
                    .flatten()
                    .or_throw_msg(ctx, "failed to convert to number")?,
            )),
            Type::Float => Variable::Number(
                Decimal::try_from(
                    v.as_float()
                        .or_throw_msg(ctx, "failed to convert to number")?,
                )
                .or_throw_msg(ctx, "failed to convert to number")?,
            ),
            Type::String => Variable::String(
                v.into_string()
                    .map(|s| s.to_string().ok())
                    .flatten()
                    .map(|s| Rc::from(s.as_str()))
                    .or_throw_msg(ctx, "failed to convert to string")?,
            ),
            Type::Array => {
                let arr = v
                    .into_array()
                    .or_throw_msg(ctx, "failed to convert to array")?;

                let mut js_arr = Vec::with_capacity(arr.len());
                for x in arr.into_iter() {
                    js_arr.push(JsValue::from_js(ctx, x.or_throw(ctx)?).or_throw(ctx)?.0)
                }

                Variable::from_array(js_arr)
            }
            Type::Object => {
                let object = v
                    .into_object()
                    .or_throw_msg(ctx, "failed to convert to object")?;

                let mut js_object = HashMap::with_capacity(object.len());
                for p in object.props() {
                    let (k, v) = p.or_throw(ctx)?;
                    js_object.insert(k, JsValue::from_js(ctx, v).or_throw(ctx)?.0);
                }

                Variable::from_object(js_object)
            }
            Type::Exception => {
                let exception = v
                    .into_exception()
                    .or_throw_msg(ctx, "failed to convert to exception")?;

                let message = exception.message().unwrap_or_default();
                let description = exception.to_string();

                json!({ "message": message, "description": description }).into()
            }
            Type::Function => json!("[Function]").into(),
            Type::Module => json!("[Module]").into(),
            Type::Constructor => json!("[Constructor]").into(),
            Type::Symbol => json!("[Symbol]").into(),
            Type::Unknown => json!("[Unknown]").into(),
            Type::Promise => {
                let promise = v.into_promise().or_throw(ctx)?;
                let val: JsValue = promise.finish()?;
                val.0
            }
        };

        Ok(JsValue(computed_value))
    }
}

impl<'js> IntoJs<'js> for JsValue {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<QValue<'js>> {
        let res = match self.0 {
            Variable::Null => QValue::new_null(ctx.clone()),
            Variable::Bool(b) => QValue::new_bool(ctx.clone(), b),
            Variable::Number(n) => QValue::new_number(
                ctx.clone(),
                n.to_f64()
                    .or_throw_msg(ctx, "failed to convert float to number")?,
            ),
            Variable::String(str) => str.into_js(ctx)?,
            Variable::Array(a) => {
                let qarr = rquickjs::Array::new(ctx.clone())?;

                let arr = a.borrow();
                for (idx, item) in arr.iter().enumerate() {
                    qarr.set(idx, JsValue(item.clone()))?;
                }

                qarr.into_value()
            }
            Variable::Object(o) => {
                let qmap = rquickjs::Object::new(ctx.clone())?;

                let obj = o.borrow();
                for (key, value) in obj.iter() {
                    qmap.set(key.into_atom(ctx)?, JsValue(value.clone()))?;
                }

                qmap.into_value()
            }
        };

        Ok(res)
    }
}
