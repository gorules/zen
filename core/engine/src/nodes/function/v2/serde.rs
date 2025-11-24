use crate::decision_graph::cleaner::ZEN_RESERVED_PROPERTIES;
use crate::nodes::function::v2::error::ResultExt;
use ahash::{HashMap, HashMapExt};
use nohash_hasher::BuildNoHashHasher;
use rquickjs::{Ctx, FromJs, IntoJs, Type, Value as QValue};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::HashMap as StdHashMap;
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
                    js_arr.push(JsValue::from_js(ctx, x?)?.0)
                }

                Variable::from_array(js_arr)
            }
            Type::Object => {
                let object = v
                    .into_object()
                    .or_throw_msg(ctx, "failed to convert to object")?;

                let mut js_object = HashMap::with_capacity(object.len());
                for p in object.props::<String, QValue>() {
                    let (k, v) = p?;
                    if ZEN_RESERVED_PROPERTIES.contains(&k.as_str()) {
                        continue;
                    }

                    js_object.insert(Rc::from(k.as_str()), JsValue::from_js(ctx, v)?.0);
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
        let converter = JsConverter::new(ctx);
        converter.convert(&self.0)
    }
}

pub struct JsValueWithNodes(pub JsValue);

impl<'js> IntoJs<'js> for JsValueWithNodes {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<QValue<'js>> {
        let base_js = self.0.into_js(ctx)?;
        if !base_js.is_object() {
            return Ok(base_js);
        }

        let obj = base_js.into_object().or_throw(ctx)?;
        let nodes_proxy: QValue<'js> = ctx.eval(
            r#"
            (() => {
              const _data = { loaded: false, inner: null };
              const data = () => {
                if (!_data.loaded) {
                  _data.loaded = true;
                  _data.inner = __getNodesData();
                }

                return _data.inner;
              };

              return new Proxy({}, {
                get: (target, prop) => data()[prop],
                has: (target, prop) => prop in data(),
                ownKeys: () => Object.keys(data()),
                getOwnPropertyDescriptor: (target, prop) => Object.getOwnPropertyDescriptor(data(), prop),
              });
            })();
        "#,
        )?;

        obj.set("$nodes", nodes_proxy)?;
        Ok(obj.into_value())
    }
}

pub(crate) struct JsConverter<'r, 'js> {
    ctx: &'r Ctx<'js>,
    cache: StdHashMap<usize, QValue<'js>, BuildNoHashHasher<usize>>,
}

impl<'r, 'js> JsConverter<'r, 'js> {
    pub fn new(ctx: &'r Ctx<'js>) -> Self {
        Self {
            ctx,
            cache: StdHashMap::default(),
        }
    }

    pub fn convert(mut self, var: &Variable) -> rquickjs::Result<QValue<'js>> {
        self.convert_with_cache(var)
    }

    fn convert_with_cache(&mut self, var: &Variable) -> rquickjs::Result<QValue<'js>> {
        match var {
            Variable::Null => Ok(QValue::new_null(self.ctx.clone())),
            Variable::Bool(b) => b.into_js(self.ctx),
            Variable::Number(n) => n.to_f64().into_js(self.ctx),
            Variable::String(str) => str.into_js(self.ctx),
            Variable::Array(a) => {
                let addr = Rc::as_ptr(&a) as *const () as usize;
                if let Some(cached) = self.cache.get(&addr) {
                    return Ok(cached.clone());
                }

                let qarr = rquickjs::Array::new(self.ctx.clone())?;
                let arr = a.borrow();
                for (idx, item) in arr.iter().enumerate() {
                    qarr.set(idx, self.convert_with_cache(item)?)?;
                }

                let val = qarr.into_value();
                self.cache.insert(addr, val.clone());
                Ok(val)
            }
            Variable::Object(o) => {
                let addr = Rc::as_ptr(&o) as *const () as usize;
                if let Some(cached) = self.cache.get(&addr) {
                    return Ok(cached.clone());
                }

                let qmap = rquickjs::Object::new(self.ctx.clone())?;
                let obj = o.borrow();
                for (key, value) in obj.iter() {
                    qmap.set(key.as_ref(), self.convert_with_cache(value)?)?;
                }

                let val = qmap.into_value();
                self.cache.insert(addr, val.clone());
                Ok(val)
            }
            Variable::Dynamic(d) => d.to_string().into_js(self.ctx),
        }
    }
}

pub(crate) mod rquickjs_conv {
    use crate::nodes::function::v2::error::ResultExt;
    use crate::nodes::function::v2::serde::QValue;
    use rquickjs::{Ctx, FromJs, IntoJs, Type};

    #[derive(Debug, Clone)]
    struct JsonValue(serde_json::Value);

    impl<'js> FromJs<'js> for JsonValue {
        fn from_js(ctx: &Ctx<'js>, value: QValue<'js>) -> rquickjs::Result<Self> {
            let json_value = match value.type_of() {
                Type::Uninitialized | Type::Undefined | Type::Null => serde_json::Value::Null,
                Type::Bool => serde_json::Value::Bool(
                    value
                        .as_bool()
                        .or_throw_msg(ctx, "failed to convert to bool")?,
                ),
                Type::Int => {
                    let int_val = value
                        .as_int()
                        .or_throw_msg(ctx, "failed to convert to int")?;
                    serde_json::Value::Number(int_val.into())
                }
                Type::Float => {
                    let float_val = value
                        .as_float()
                        .or_throw_msg(ctx, "failed to convert to float")?;
                    serde_json::Number::from_f64(float_val)
                        .map(serde_json::Value::Number)
                        .or_throw_msg(ctx, "failed to convert float to number")?
                }
                Type::BigInt => {
                    let big_int = value
                        .into_big_int()
                        .or_throw_msg(ctx, "failed to convert bigint")?
                        .to_i64()
                        .or_throw_msg(ctx, "failed to convert bigint to i64")?;
                    serde_json::Value::Number(big_int.into())
                }
                Type::String => {
                    let str_val = value
                        .into_string()
                        .or_throw_msg(ctx, "failed to convert to string")?
                        .to_string()
                        .or_throw_msg(ctx, "failed to get string value")?;
                    serde_json::Value::String(str_val)
                }
                Type::Array => {
                    let arr = value
                        .into_array()
                        .or_throw_msg(ctx, "failed to convert to array")?;
                    let mut json_arr = Vec::with_capacity(arr.len());
                    for item in arr.into_iter() {
                        let item_val = item.or_throw(ctx)?;
                        json_arr.push(JsonValue::from_js(ctx, item_val).or_throw(ctx)?.0);
                    }
                    serde_json::Value::Array(json_arr)
                }
                Type::Object => {
                    let obj = value
                        .into_object()
                        .or_throw_msg(ctx, "failed to convert to object")?;
                    let mut json_obj = serde_json::Map::new();
                    for prop in obj.props::<String, QValue>() {
                        let (key, val) = prop.or_throw(ctx)?;
                        json_obj.insert(key, JsonValue::from_js(ctx, val).or_throw(ctx)?.0);
                    }
                    serde_json::Value::Object(json_obj)
                }
                Type::Promise => {
                    let promise = value.into_promise().or_throw(ctx)?;
                    let val: JsonValue = promise.finish()?;
                    val.0
                }
                _ => serde_json::Value::Null,
            };

            Ok(JsonValue(json_value))
        }
    }

    impl<'js> IntoJs<'js> for JsonValue {
        fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<QValue<'js>> {
            match self.0 {
                serde_json::Value::Null => Ok(QValue::new_null(ctx.clone())),
                serde_json::Value::Bool(b) => b.into_js(ctx),
                serde_json::Value::Number(n) => {
                    let number = n.as_i64().map(|i| i as f64).or(n.as_f64());
                    match number {
                        Some(num) => num.into_js(ctx),
                        None => Ok(QValue::new_null(ctx.clone())),
                    }
                }
                serde_json::Value::String(s) => s.into_js(ctx),
                serde_json::Value::Array(arr) => {
                    let qarr = rquickjs::Array::new(ctx.clone())?;
                    for (idx, item) in arr.into_iter().enumerate() {
                        qarr.set(idx, JsonValue(item).into_js(ctx)?)?;
                    }

                    Ok(qarr.into_value())
                }
                serde_json::Value::Object(obj) => {
                    let qobj = rquickjs::Object::new(ctx.clone())?;
                    for (key, value) in obj.into_iter() {
                        qobj.set(key, JsonValue(value).into_js(ctx)?)?;
                    }

                    Ok(qobj.into_value())
                }
            }
        }
    }

    pub(crate) fn from_value<T>(value: QValue) -> rquickjs::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let ctx = value.ctx().clone();
        let js_value = JsonValue::from_js(&ctx, value)?;
        serde_json::from_value(js_value.0).map_err(|e| {
            rquickjs::Error::new_from_js_message("serde_json::Value", "rust type", e.to_string())
        })
    }

    pub(crate) fn to_value<T>(ctx: Ctx, value: T) -> rquickjs::Result<QValue>
    where
        T: serde::Serialize,
    {
        let json_value = serde_json::to_value(value).map_err(|e| {
            rquickjs::Error::new_from_js_message("rust type", "serde_json::Value", e.to_string())
        })?;
        JsonValue(json_value).into_js(&ctx)
    }
}
