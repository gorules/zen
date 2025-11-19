use crate::nodes::function::v2::error::ResultExt;
use ahash::{HashMap, HashMapExt};
use nohash_hasher::BuildNoHashHasher;
use rquickjs::{Ctx, FromJs, IntoAtom, IntoJs, Type, Value as QValue};
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
                    js_arr.push(JsValue::from_js(ctx, x.or_throw(ctx)?).or_throw(ctx)?.0)
                }

                Variable::from_array(js_arr)
            }
            Type::Object => {
                let object = v
                    .into_object()
                    .or_throw_msg(ctx, "failed to convert to object")?;

                let mut js_object = HashMap::with_capacity(object.len());
                for p in object.props::<String, QValue>() {
                    let (k, v) = p.or_throw(ctx)?;
                    js_object.insert(
                        Rc::from(k.as_str()),
                        JsValue::from_js(ctx, v).or_throw(ctx)?.0,
                    );
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
        converter.convert(self.0)
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

    pub fn convert(mut self, var: Variable) -> rquickjs::Result<QValue<'js>> {
        self.convert_with_cache(var)
    }

    fn convert_with_cache(&mut self, var: Variable) -> rquickjs::Result<QValue<'js>> {
        match var {
            Variable::Null => Ok(QValue::new_null(self.ctx.clone())),
            Variable::Bool(b) => Ok(QValue::new_bool(self.ctx.clone(), b)),
            Variable::Number(n) => Ok(QValue::new_number(
                self.ctx.clone(),
                n.to_f64()
                    .or_throw_msg(self.ctx, "failed to convert float to number")?,
            )),
            Variable::String(str) => str.into_js(self.ctx),
            Variable::Array(a) => {
                let addr = Rc::as_ptr(&a) as *const () as usize;
                if let Some(cached) = self.cache.get(&addr) {
                    return Ok(cached.clone());
                }

                let qarr = rquickjs::Array::new(self.ctx.clone())?;
                let arr = a.borrow();
                for (idx, item) in arr.iter().enumerate() {
                    qarr.set(idx, self.convert_with_cache(item.clone())?)?;
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
                    let key_atom = key.into_atom(self.ctx)?;
                    qmap.set(key_atom, self.convert_with_cache(value.clone())?)?;
                }

                let val = qmap.into_value();
                self.cache.insert(addr, val.clone());
                Ok(val)
            }
            Variable::Dynamic(d) => d.to_string().into_js(self.ctx),
        }
    }
}
