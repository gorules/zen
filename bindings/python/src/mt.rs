use pyo3_async_runtimes::tokio::re_exports::runtime::Runtime;
use std::sync::OnceLock;
use std::thread::available_parallelism;
use tokio_util::task::LocalPoolHandle;

fn parallelism() -> usize {
    available_parallelism().map(Into::into).unwrap_or(1)
}

pub(crate) fn worker_pool() -> LocalPoolHandle {
    static LOCAL_POOL: OnceLock<LocalPoolHandle> = OnceLock::new();
    LOCAL_POOL
        .get_or_init(|| LocalPoolHandle::new(parallelism()))
        .clone()
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().unwrap())
}

pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    get_runtime().block_on(future)
}
