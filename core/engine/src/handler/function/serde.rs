use itertools::Itertools;
use rquickjs::{Ctx, Error as QError, FromJs, IntoAtom, IntoJs, Type, Value as QValue};
use serde_json::{json, Map, Number, Value};

#[derive(Debug)]
pub(crate) struct JsValue(pub(crate) Value);

impl<'js> FromJs<'js> for JsValue {
    fn from_js(ctx: &Ctx<'js>, v: QValue<'js>) -> rquickjs::Result<Self> {
        let computed_value = match v.type_of() {
            Type::Uninitialized | Type::Undefined | Type::Null => Value::Null,
            Type::Bool => Value::Bool(
                v.as_bool()
                    .ok_or_else(|| QError::new_from_js("bool", "bool"))?,
            ),
            Type::Int => Value::Number(Number::from(
                v.as_int()
                    .ok_or_else(|| QError::new_from_js("int", "number"))?,
            )),
            Type::BigInt => Value::Number(Number::from(
                v.as_big_int()
                    .map(|b| b.clone().to_i64().ok())
                    .flatten()
                    .ok_or_else(|| QError::new_from_js("bigint", "number"))?,
            )),
            Type::Float => Value::Number(
                v.as_float()
                    .map(|n| Number::from_f64(n))
                    .flatten()
                    .ok_or_else(|| QError::new_from_js("float", "number"))?,
            ),
            Type::String => Value::String(
                v.as_string()
                    .map(|s| s.to_string().ok())
                    .flatten()
                    .ok_or_else(|| QError::new_from_js("string", "string"))?,
            ),
            Type::Array => {
                let arr = v
                    .as_array()
                    .ok_or_else(|| QError::new_from_js("array", "array"))?;

                let js_arr: Vec<Value> = arr
                    .iter::<QValue>()
                    .map_ok(|n| JsValue::from_js(ctx, n.clone()).map(|js_val| js_val.0))
                    .flatten()
                    .try_collect()
                    .map_err(|_| QError::new_from_js("array", "array"))?;

                Value::Array(js_arr)
            }
            Type::Object => {
                let object = v
                    .as_object()
                    .ok_or_else(|| QError::new_from_js("object", "object"))?;

                let js_object: Map<String, Value> = object
                    .props::<String, QValue>()
                    .map_ok(|(key, value)| {
                        JsValue::from_js(ctx, value.clone()).map(|js_val| (key, js_val.0))
                    })
                    .flatten()
                    .try_collect()
                    .map_err(|_| QError::new_from_js("object", "object"))?;

                Value::Object(js_object)
            }
            Type::Exception => {
                let exception = v
                    .as_exception()
                    .ok_or_else(|| QError::new_from_js("exception", "object"))?;

                let message = exception
                    .message()
                    .ok_or_else(|| QError::new_from_js("exception", "object"))?;
                let description = exception.to_string();

                json!({ "message": message, "description": description })
            }
            Type::Function => json!("[Function]"),
            Type::Module => json!("[Module]"),
            Type::Constructor => json!("[Constructor]"),
            Type::Symbol => json!("[Symbol]"),
            Type::Unknown => json!("[Unknown]"),
            Type::Promise => {
                let promise = v.as_promise().unwrap();
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
                n.as_f64().ok_or_else(|| rquickjs::Error::IntoJs {
                    from: "serde::Number",
                    to: "Number",
                    message: Some("Number is not finite".to_string()),
                })?,
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
