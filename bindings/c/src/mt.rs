use std::sync::{Arc, OnceLock};
use std::thread::available_parallelism;

use tokio::runtime;
use tokio::runtime::Runtime;
use tokio_util::task::LocalPoolHandle;

pub(crate) fn worker_pool() -> LocalPoolHandle {
    static LOCAL_POOL: OnceLock<LocalPoolHandle> = OnceLock::new();
    LOCAL_POOL
        .get_or_init(|| LocalPoolHandle::new(available_parallelism().map(Into::into).unwrap_or(1)))
        .clone()
}

pub(crate) fn tokio_runtime() -> Arc<Runtime> {
    static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            Arc::new(
                runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build tokio runtime"),
            )
        })
        .clone()
}
