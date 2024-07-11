use napi::tokio::task::JoinHandle;
use std::future::Future;
use std::sync::OnceLock;
use std::thread::available_parallelism;
use tokio_util::task::LocalPoolHandle;

fn parallelism() -> usize {
    let available = available_parallelism().map(Into::into).unwrap_or(1);
    let additional = ((available as f64) * 1.5) as usize;

    available + additional
}

pub(crate) fn worker_pool() -> LocalPoolHandle {
    static LOCAL_POOL: OnceLock<LocalPoolHandle> = OnceLock::new();
    LOCAL_POOL
        .get_or_init(|| LocalPoolHandle::new(parallelism()))
        .clone()
}

pub(crate) fn spawn_worker<F, Fut>(create_task: F) -> JoinHandle<Fut::Output>
where
    F: FnOnce() -> Fut,
    F: Send + 'static,
    Fut: Future + 'static,
    Fut::Output: Send + 'static,
{
    worker_pool().spawn_pinned(create_task)
}
