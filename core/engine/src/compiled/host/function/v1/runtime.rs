use anyhow::Context;
use rquickjs::loader::Bundle;
use rquickjs::{embed, Runtime};
use std::cell::RefCell;

static JS_BUNDLE: Bundle = embed! {
    "dayjs": "js/v1/dayjs.js",
    "big": "js/v1/big.js",
    "internals": "js/v1/internals.js"
};

thread_local! {
    static IDLE: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

pub(crate) fn create_runtime() -> anyhow::Result<Runtime> {
    let runtime = Runtime::new().context("Failed to create runtime")?;
    runtime.set_loader(JS_BUNDLE, JS_BUNDLE);

    Ok(runtime)
}

pub(crate) fn acquire_runtime() -> anyhow::Result<Runtime> {
    match IDLE.with_borrow_mut(Option::take) {
        Some(runtime) => Ok(runtime),
        None => create_runtime(),
    }
}

pub(crate) fn release_runtime(runtime: Runtime) {
    if !runtime.is_job_pending() {
        IDLE.with_borrow_mut(|idle| *idle = Some(runtime));
    }
}
