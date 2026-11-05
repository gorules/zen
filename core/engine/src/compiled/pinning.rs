use crate::engine::EvaluationOptions;
use std::cell::RefCell;
use std::rc::Rc;
use zen_expression::clock::{Clock, EvalContext};

pub(crate) fn single(options: &EvaluationOptions) -> EvalContext {
    let eval = EvalContext::new(Clock::new(options.now, options.timezone, options.seed));
    if options.trace.is_on() {
        eval.now();
    }
    eval
}

pub(crate) fn batch(options: &EvaluationOptions) -> EvalContext {
    EvalContext::new(Clock::batch(options.now, options.timezone, options.seed))
}

pub(crate) fn evaluation(eval: &EvalContext) -> crate::compiled::trace::Evaluation {
    crate::compiled::trace::Evaluation {
        evaluated_at: Some(eval.clock().evaluated_at()),
        timezone: Some(eval.clock().zone()),
        seed: eval.seed(),
    }
}

pub(crate) fn row_evaluation(eval: &EvalContext, row: usize) -> crate::compiled::trace::Evaluation {
    crate::compiled::trace::Evaluation {
        evaluated_at: Some(eval.clock().evaluated_at()),
        timezone: Some(eval.clock().zone()),
        seed: eval.row_seed(row),
    }
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

#[derive(Debug, Clone, Default)]
pub(crate) struct JsClock(Rc<RefCell<Option<EvalContext>>>);

pub(crate) struct Entered {
    clock: JsClock,
    previous: Option<EvalContext>,
}

impl Drop for Entered {
    fn drop(&mut self) {
        *self.clock.0.borrow_mut() = self.previous.take();
    }
}

impl JsClock {
    pub(crate) fn enter(&self, eval: EvalContext) -> Entered {
        Entered {
            clock: self.clone(),
            previous: self.0.borrow_mut().replace(eval),
        }
    }

    pub(crate) fn current(&self) -> Option<EvalContext> {
        self.0.borrow().clone()
    }

    fn now(&self) -> f64 {
        let now = match self.0.borrow().as_ref() {
            Some(eval) => eval.now(),
            None => zen_expression::clock::system_now(),
        };
        now.timestamp_millis() as f64
    }

    fn random(&self) -> f64 {
        match self.0.borrow().as_ref() {
            Some(eval) => eval.random_f64(0),
            None => EvalContext::default().random_f64(0),
        }
    }

    pub(crate) fn install(&self, ctx: &rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
        let globals = ctx.globals();
        if globals.get::<_, bool>("__zenPinned").unwrap_or(false) {
            return Ok(());
        }
        let clock = self.clone();
        globals.set(
            "__zenNow",
            rquickjs::prelude::Func::from(move || clock.now()),
        )?;
        let clock = self.clone();
        globals.set(
            "__zenRandom",
            rquickjs::prelude::Func::from(move || clock.random()),
        )?;
        ctx.eval::<(), _>(PIN_JS)
    }
}
