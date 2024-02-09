use itertools::Itertools;
use rquickjs::{Ctx, Error as QError, FromJs, Type, Value as QValue};
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
        };

        Ok(JsValue(computed_value))
    }
}
