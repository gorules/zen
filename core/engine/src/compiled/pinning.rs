use crate::engine::EvaluationOptions;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use zen_expression::clock::Clock;

pub(crate) struct Recycled(Option<Arc<Clock>>);

impl Drop for Recycled {
    fn drop(&mut self) {
        if let Some(clock) = self.0.take() {
            Clock::recycle(clock);
        }
    }
}

pub(crate) async fn run<F: Future>(clock: Option<Arc<Clock>>, mut inner: Pin<&mut F>) -> F::Output {
    let clock = Recycled(clock);
    std::future::poll_fn(move |cx: &mut Context<'_>| -> Poll<F::Output> {
        let _origin = zen_expression::clock::enter_origin(None);
        let _scope = clock.0.as_ref().map(|clock| clock.enter());
        inner.as_mut().poll(cx)
    })
    .await
}

pub(crate) type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub(crate) fn originated<'a, F: Future + 'a>(
    origin: Option<Rc<zen_expression::clock::Origin>>,
    inner: F,
) -> Boxed<'a, F::Output> {
    let Some(origin) = origin else {
        return Box::pin(inner);
    };
    Box::pin(async move {
        let mut inner = std::pin::pin!(inner);
        std::future::poll_fn(move |cx: &mut Context<'_>| -> Poll<F::Output> {
            let _origin = zen_expression::clock::enter_origin(Some(origin.clone()));
            inner.as_mut().poll(cx)
        })
        .await
    })
}

pub(crate) fn rows_origin(rows: &Rc<[usize]>) -> Option<Rc<zen_expression::clock::Origin>> {
    zen_expression::clock::streamed().then(|| {
        Rc::new(zen_expression::clock::Origin::Rows(
            rows.clone(),
            zen_expression::clock::origin(),
        ))
    })
}

pub(crate) fn rows_scope(rows: &[usize]) -> Option<zen_expression::clock::Origins> {
    zen_expression::clock::streamed().then(|| zen_expression::clock::rows(rows.into()))
}

pub(crate) fn restart(rows: &[usize]) {
    if zen_expression::clock::origin().is_some() {
        return;
    }
    if let Some(clock) = zen_expression::clock::current().filter(|clock| clock.is_batch()) {
        for &row in rows {
            clock.fresh_stream(row);
        }
    }
}

pub(crate) fn at_row_now<R>(row: usize, f: impl FnOnce() -> R) -> R {
    match zen_expression::clock::streamed() {
        true => match zen_expression::clock::stream_at(row) {
            Some(stream) => {
                let _origin = zen_expression::clock::enter_origin(None);
                stream.scope(f)
            }
            None => f(),
        },
        false => f(),
    }
}

pub(crate) fn item<'a, F: Future + 'a>(index: usize, inner: F) -> Boxed<'a, F::Output> {
    let origin = zen_expression::clock::Origin::Items(
        Rc::from([(0, index)]),
        zen_expression::clock::origin(),
    );
    originated(Some(Rc::new(origin)), inner)
}

pub(crate) fn at_row<'a, F: Future + 'a>(row: usize, inner: F) -> Boxed<'a, F::Output> {
    match zen_expression::clock::streamed() {
        true => match zen_expression::clock::stream_at(row) {
            Some(stream) => Box::pin(with(stream, inner)),
            None => Box::pin(inner),
        },
        false => Box::pin(inner),
    }
}

pub(crate) fn scope_for(options: &EvaluationOptions) -> Option<Arc<Clock>> {
    match nested(options) {
        true => None,
        false => Some(clock_for(options)),
    }
}

pub(crate) fn supplies(options: &EvaluationOptions) -> bool {
    options.now.is_some() || options.timezone.is_some() || options.seed.is_some()
}

pub(crate) fn clock_for(options: &EvaluationOptions) -> Arc<Clock> {
    let clock = match (zen_expression::clock::current(), supplies(options)) {
        (Some(current), false) => current,
        _ => Clock::new(options.now, options.timezone, options.seed),
    };
    if options.trace.is_on() {
        clock.now();
    }
    clock
}

pub(crate) fn batch_for(options: &EvaluationOptions) -> Arc<Clock> {
    clock_for(options).batch()
}

pub(crate) fn nested(options: &EvaluationOptions) -> bool {
    !supplies(options) && zen_expression::clock::current_is_set()
}

pub(crate) fn inner(options: EvaluationOptions) -> EvaluationOptions {
    EvaluationOptions {
        now: None,
        timezone: None,
        seed: None,
        ..options
    }
}

pub(crate) async fn with<F: Future>(clock: Arc<Clock>, inner: F) -> F::Output {
    let inner = std::pin::pin!(inner);
    run(Some(clock), inner).await
}

pub(crate) fn scoped<R>(f: impl FnOnce() -> R) -> R {
    match zen_expression::clock::current() {
        Some(_) => f(),
        None => Clock::new(None, None, None).scope(f),
    }
}

pub(crate) fn batched<R>(f: impl FnOnce() -> R) -> R {
    match zen_expression::clock::current() {
        Some(clock) => clock.batch().scope(f),
        None => Clock::new(None, None, None).batch().scope(f),
    }
}

pub(crate) fn evaluation_of(clock: &Clock) -> crate::compiled::trace::Evaluation {
    crate::compiled::trace::Evaluation {
        evaluated_at: Some(clock.evaluated_at()),
        timezone: Some(clock.zone()),
        seed: clock.seed(),
    }
}

pub(crate) fn row_evaluation(clock: &Arc<Clock>, row: usize) -> crate::compiled::trace::Evaluation {
    crate::compiled::trace::Evaluation {
        evaluated_at: Some(clock.evaluated_at()),
        timezone: Some(clock.zone()),
        seed: match clock.is_batch() {
            true => clock.row_seed(row),
            false => clock.seed(),
        },
    }
}

pub(crate) fn evaluation() -> Option<crate::compiled::trace::Evaluation> {
    zen_expression::clock::current().map(|clock| evaluation_of(&clock))
}

const PIN_JS: &str = r#"(() => {
  if (globalThis.__zenPinned) return;
  const Native = Date;
  const now = globalThis.__zenNow;
  const random = globalThis.__zenRandom;
  function PinnedDate(...args) {
    if (!new.target) return new Native(now()).toString();
    return args.length === 0 ? new Native(now()) : new Native(...args);
  }
  PinnedDate.prototype = Native.prototype;
  PinnedDate.now = () => now();
  PinnedDate.parse = Native.parse;
  PinnedDate.UTC = Native.UTC;
  Object.defineProperty(Native.prototype, 'constructor', { value: PinnedDate, writable: true, configurable: true });
  globalThis.Date = PinnedDate;
  Math.random = () => random();
  globalThis.__zenPinned = true;
})();"#;

pub(crate) fn install_js(ctx: &rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    if globals.get::<_, bool>("__zenPinned").unwrap_or(false) {
        return Ok(());
    }
    globals.set(
        "__zenNow",
        rquickjs::prelude::Func::from(|| zen_expression::clock::now().timestamp_millis() as f64),
    )?;
    globals.set(
        "__zenRandom",
        rquickjs::prelude::Func::from(zen_expression::clock::random_f64),
    )?;
    ctx.eval::<(), _>(PIN_JS)
}
