#[cfg(not(target_family = "wasm"))]
mod pool {
    use napi::tokio::task::JoinHandle;
    use std::future::Future;
    use std::sync::OnceLock;
    use std::thread::available_parallelism;
    use tokio_util::task::LocalPoolHandle;

    fn parallelism() -> usize {
        available_parallelism().map(Into::into).unwrap_or(1)
    }

    fn worker_pool() -> LocalPoolHandle {
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
}

#[cfg(target_family = "wasm")]
mod pool {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct AssertSend<Fut>(Pin<Box<Fut>>);

    unsafe impl<Fut> Send for AssertSend<Fut> {}

    impl<Fut: Future> Future for AssertSend<Fut> {
        type Output = Fut::Output;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.0.as_mut().poll(cx)
        }
    }

    pub(crate) fn spawn_worker<F, Fut>(
        create_task: F,
    ) -> napi_async_runtime::JoinHandle<Fut::Output>
    where
        F: FnOnce() -> Fut,
        F: Send + 'static,
        Fut: Future + 'static,
        Fut::Output: Send + 'static,
    {
        napi_async_runtime::spawn(AssertSend(Box::pin(create_task())))
    }

    #[napi_derive::module_init]
    fn install_async_runtime() {
        napi_async_runtime::install(napi_async_runtime::RuntimeOptions::default())
            .expect("install current-thread async runtime");
        zen_engine::nodes::function::sleep::set_sleep_provider(|duration| {
            Box::pin(napi_async_runtime::sleep_until(
                std::time::Instant::now() + duration,
            ))
        });
    }
}

pub(crate) use pool::spawn_worker;

#[cfg(not(target_family = "wasm"))]
pub(crate) async fn run_task<T, F>(task: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    napi::tokio::spawn(async move { task() })
        .await
        .map_err(|_| ())
}

#[cfg(target_family = "wasm")]
pub(crate) async fn run_task<T, F>(task: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Ok(task())
}
