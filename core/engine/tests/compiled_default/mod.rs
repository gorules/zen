#[ctor::ctor(unsafe)]
fn compiled_by_default() {
    zen_engine::ZEN_CONFIG
        .compiled
        .store(true, std::sync::atomic::Ordering::Relaxed);
}
