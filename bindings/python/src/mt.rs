use std::future::Future;
use std::sync::OnceLock;
use std::thread::available_parallelism;

use ::tokio::task;
use ::tokio::runtime::Handle;
use pyo3::{IntoPy, PyAny, PyObject, PyResult, Python};
use pyo3_asyncio::tokio;
use pyo3_asyncio::TaskLocals;
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
    task::block_in_place(move || {
        Handle::current().block_on(async move {
            worker_pool()
                .spawn_pinned(create_task)
                .await
                .expect("Thread panicked")
        })
    })
}

pub(crate) fn async_block_on_into_py<F, T>(
    py: Python,
    locals: TaskLocals,
    task: F,
) -> PyResult<&PyAny>
where
    F: Future<Output = PyResult<T>> + 'static,
    T: IntoPy<PyObject>,
{
    task::LocalSet::new().block_on(tokio::get_runtime(), async {
        #[allow(deprecated)]
        tokio::local_future_into_py_with_locals(py, locals.clone(), task)
    })
}

pub(crate) fn async_block_on<F, R>(locals: TaskLocals, task: F) -> R
where
    F: Future<Output = R> + 'static,
{
    task::LocalSet::new().block_on(
        tokio::get_runtime(),
        tokio::scope_local(locals.clone(), task),
    )
}
