use std::sync::{Arc, OnceLock};

use tokio::runtime;
use tokio::runtime::Runtime;

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
