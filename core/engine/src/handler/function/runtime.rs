use anyhow::Context;
use rquickjs::loader::Bundle;
use rquickjs::{embed, Runtime};

static JS_BUNDLE: Bundle = embed! {
    "dayjs": "js/dayjs.js",
    "big": "js/big.js",
    "internals": "js/internals.js"
};

pub(crate) fn create_runtime() -> anyhow::Result<Runtime> {
    let runtime = Runtime::new().context("Failed to create runtime")?;
    runtime.set_loader(JS_BUNDLE, JS_BUNDLE);
    
    Ok(runtime)
}
