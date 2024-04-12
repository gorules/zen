use napi::bindgen_prelude::{ToNapiValue, TypeName};
use napi::sys::{napi_env, napi_value};
use napi::ValueType;

pub struct SafeResult<Data, Error = napi::Error>(Result<Data, Error>);

impl<T, E> TypeName for SafeResult<T, E> {
    fn type_name() -> &'static str {
        "SafeResult"
    }

    fn value_type() -> ValueType {
        ValueType::Object
    }
}

impl<T, E> ToNapiValue for SafeResult<T, E>
where
    T: ToNapiValue,
    E: ToNapiValue,
{
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        let env_wrapper = napi::bindgen_prelude::Env::from(env);
        let mut obj = env_wrapper.create_object()?;

        match val.0 {
            Ok(data) => {
                obj.set("success", true)?;
                obj.set("data", data)?;
            }
            Err(error) => {
                obj.set("success", false)?;
                obj.set("error", error)?;
            }
        }

        napi::bindgen_prelude::Object::to_napi_value(env, obj)
    }
}

impl<Data> From<napi::Result<Data>> for SafeResult<Data, napi::Error> {
    fn from(value: napi::Result<Data>) -> Self {
        Self(value)
    }
}
