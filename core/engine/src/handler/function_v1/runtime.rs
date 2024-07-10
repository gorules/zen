use anyhow::Context;
use rquickjs::loader::Bundle;
use rquickjs::{embed, Runtime};

static JS_BUNDLE: Bundle = embed! {
    "dayjs": "js/v1/dayjs.js",
    "big": "js/v1/big.js",
    "internals": "js/v1/internals.js"
};

pub(crate) fn create_runtime() -> anyhow::Result<Runtime> {
    let runtime = Runtime::new().context("Failed to create runtime")?;
    runtime.set_loader(JS_BUNDLE, JS_BUNDLE);

    Ok(runtime)
}
