use crate::handler::function::error::ResultExt;
use itertools::Itertools;
use rquickjs::{Ctx, FromJs, IntoAtom, IntoJs, Type, Value as QValue};
use serde_json::{json, Map, Number, Value};

#[derive(Debug)]
pub(crate) struct JsValue(pub(crate) Value);

impl<'js> FromJs<'js> for JsValue {
    fn from_js(ctx: &Ctx<'js>, v: QValue<'js>) -> rquickjs::Result<Self> {
        let computed_value = match v.type_of() {
            Type::Uninitialized | Type::Undefined | Type::Null => Value::Null,
            Type::Bool => Value::Bool(v.as_bool().or_throw_msg(ctx, "failed to convert to bool")?),
            Type::Int => Value::Number(Number::from(
                v.as_int().or_throw_msg(ctx, "failed to convert to int")?,
            )),
            Type::BigInt => Value::Number(Number::from(
                v.as_big_int()
                    .map(|b| b.clone().to_i64().ok())
                    .flatten()
                    .or_throw_msg(ctx, "failed to convert to number")?,
            )),
            Type::Float => Value::Number(
                v.as_float()
                    .map(|n| Number::from_f64(n))
                    .flatten()
                    .or_throw_msg(ctx, "failed to convert to number")?,
            ),
            Type::String => Value::String(
                v.as_string()
                    .map(|s| s.to_string().ok())
                    .flatten()
                    .or_throw_msg(ctx, "failed to convert to string")?,
            ),
            Type::Array => {
                let arr = v
                    .as_array()
                    .or_throw_msg(ctx, "failed to convert to array")?;

                let js_arr: Vec<Value> = arr
                    .iter::<QValue>()
                    .map_ok(|n| JsValue::from_js(ctx, n.clone()).map(|js_val| js_val.0))
                    .flatten()
                    .try_collect()
                    .or_throw(ctx)?;

                Value::Array(js_arr)
            }
            Type::Object => {
                let object = v
                    .as_object()
                    .or_throw_msg(ctx, "failed to convert to object")?;

                let js_object: Map<String, Value> = object
                    .props::<String, QValue>()
                    .map_ok(|(key, value)| {
                        JsValue::from_js(ctx, value.clone()).map(|js_val| (key, js_val.0))
                    })
                    .flatten()
                    .try_collect()
                    .or_throw(ctx)?;

                Value::Object(js_object)
            }
            Type::Exception => {
                let exception = v
                    .as_exception()
                    .or_throw_msg(ctx, "failed to convert to exception")?;

                let message = exception.message().unwrap_or_default();
                let description = exception.to_string();

                json!({ "message": message, "description": description })
            }
            Type::Function => json!("[Function]"),
            Type::Module => json!("[Module]"),
            Type::Constructor => json!("[Constructor]"),
            Type::Symbol => json!("[Symbol]"),
            Type::Unknown => json!("[Unknown]"),
            Type::Promise => {
                let promise = v.as_promise().or_throw(ctx)?;
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
            Value::Null => QValue::new_null(ctx.clone()),
            Value::Bool(b) => QValue::new_bool(ctx.clone(), b),
            Value::Number(n) => QValue::new_number(
                ctx.clone(),
                n.as_f64()
                    .or_throw_msg(ctx, "failed to convert float to number")?,
            ),
            Value::String(str) => str.into_js(ctx)?,
            Value::Array(arr) => {
                let qarr = rquickjs::Array::new(ctx.clone())?;
                for (idx, item) in arr.into_iter().enumerate() {
                    qarr.set(idx, JsValue(item))?;
                }

                qarr.into_value()
            }
            Value::Object(map) => {
                let qmap = rquickjs::Object::new(ctx.clone())?;
                for (key, value) in map.into_iter() {
                    qmap.set(key.into_atom(ctx)?, JsValue(value))?;
                }

                qmap.into_value()
            }
        };

        Ok(res)
    }
}
