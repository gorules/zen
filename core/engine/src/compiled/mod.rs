pub(crate) mod bound;
pub(crate) mod census;
pub(crate) mod content;
mod core;
pub(crate) mod decision;
pub mod diagnostics;
pub(crate) mod engine;
pub(crate) mod exact;
mod graph;
pub(crate) mod host;
mod leaf;
mod lowered;
mod output;
pub(crate) mod pinning;
pub(crate) mod policy;
pub(crate) mod prepared;
mod shred;
pub(crate) mod switches;
pub(crate) mod table;
pub(crate) mod trace;

pub(crate) use graph::traces as graph_traces;
pub use graph::ColumnarOutput;
pub(crate) use graph::{call, traced, CompiledGraph, CompiledPlan, Invalid};
pub use output::{OutputColumn, RecordsView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    Configured,
    Interpreted,
    Compiled,
}

thread_local! {
    static INTERPRETING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Engine {
    pub(crate) fn compiled(self) -> bool {
        match self {
            Engine::Configured => {
                !INTERPRETING.get()
                    && crate::ZEN_CONFIG
                        .compiled
                        .load(std::sync::atomic::Ordering::Relaxed)
            }
            Engine::Interpreted => false,
            Engine::Compiled => true,
        }
    }

    pub(crate) fn scopes(self) -> bool {
        self == Engine::Interpreted && !INTERPRETING.get()
    }

    pub(crate) fn interpreting<F: std::future::Future>(future: F) -> Interpreting<F> {
        Interpreting(Box::pin(future))
    }
}

pub(crate) struct Interpreting<F>(std::pin::Pin<Box<F>>);

struct Restore(bool);

impl Drop for Restore {
    fn drop(&mut self) {
        INTERPRETING.set(self.0);
    }
}

impl<F: std::future::Future> std::future::Future for Interpreting<F> {
    type Output = F::Output;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<F::Output> {
        let _restore = Restore(INTERPRETING.replace(true));
        self.0.as_mut().poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::Engine;

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn the_interpreting_scope_overrides_only_the_configured_engine() {
        let inside = run(Engine::interpreting(async {
            (
                Engine::Configured.compiled(),
                Engine::Compiled.compiled(),
                Engine::Interpreted.scopes(),
            )
        }));
        assert_eq!(inside, (false, true, false));
        assert!(Engine::Interpreted.scopes());
    }

    #[test]
    fn the_interpreting_scope_is_restored_after_a_panic() {
        let panicked = std::panic::catch_unwind(|| {
            run(Engine::interpreting(async { panic!("inside the scope") }))
        });
        assert!(panicked.is_err());
        assert!(Engine::Interpreted.scopes());
    }
}
