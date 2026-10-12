use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

pub type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
pub type SleepProvider = fn(Duration) -> SleepFuture;

static PROVIDER: OnceLock<SleepProvider> = OnceLock::new();

pub fn set_sleep_provider(provider: SleepProvider) {
    let _ = PROVIDER.set(provider);
}

pub(crate) async fn sleep(duration: Duration) {
    match PROVIDER.get() {
        Some(provider) => provider(duration).await,
        None => tokio::time::sleep(duration).await,
    }
}
