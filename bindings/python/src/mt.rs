use std::future::Future;
use std::sync::OnceLock;
use std::thread::available_parallelism;
use tokio::runtime::Handle;
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

pub(crate) fn spawn_worker<F, Fut>(create_task: F) -> impl Future<Output = Fut::Output>
where
    F: FnOnce() -> Fut,
    F: Send + 'static,
    Fut: Future + 'static,
    Fut::Output: Send + 'static,
{
    async move {
        worker_pool()
            .spawn_pinned(create_task)
            .await
            .expect("Thread panicked")
    }
}

pub(crate) fn spawn_worker_blocking<F, Fut>(create_task: F) -> Fut::Output
where
    F: FnOnce() -> Fut,
    F: Send + 'static,
    Fut: Future + 'static,
    Fut::Output: Send + 'static,
{
    tokio::task::block_in_place(move || {
        Handle::current().block_on(async move {
            worker_pool()
                .spawn_pinned(create_task)
                .await
                .expect("Thread panicked")
        })
    })
}
