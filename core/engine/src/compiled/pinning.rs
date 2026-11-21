use crate::engine::EvaluationOptions;
use chrono::{DateTime, Duration, LocalResult, Offset, TimeZone};
use rquickjs::prelude::Func;
use std::cell::RefCell;
use std::rc::Rc;
use zen_expression::context::{
    location, parse_timezone, system_now, system_timezone, EvaluationContext, Site, Timezone,
};

pub(crate) fn single(options: &EvaluationOptions) -> EvaluationContext {
    let context = EvaluationContext::new(options.pinned());
    if options.trace.is_on() {
        context.now();
    }
    context
}

pub(crate) fn batch(options: &EvaluationOptions) -> EvaluationContext {
    EvaluationContext::batch(options.pinned())
}

#[derive(Debug, Clone)]
pub(crate) struct Invocation {
    pub(crate) trace: crate::compiled::trace::TraceFormat,
    pub(crate) limits: crate::compiled::host::Limits,
    pub(crate) context: EvaluationContext,
}

impl Invocation {
    fn new(options: &EvaluationOptions, context: EvaluationContext) -> Self {
        Self {
            trace: options.trace,
            limits: crate::compiled::host::Limits::of(options),
            context,
        }
    }

    pub(crate) fn single(options: &EvaluationOptions) -> Self {
        Self::new(options, single(options))
    }

    pub(crate) fn batch(options: &EvaluationOptions) -> Self {
        Self::new(options, batch(options))
    }

    pub(crate) fn at(&self, row: usize) -> Self {
        Self {
            context: self.context.at(row),
            ..self.clone()
        }
    }

    pub(crate) fn untraced(&self) -> Self {
        Self {
            trace: crate::compiled::trace::TraceFormat::Off,
            ..self.clone()
        }
    }
}

pub(crate) fn evaluation(context: &EvaluationContext) -> crate::compiled::trace::Evaluation {
    crate::compiled::trace::Evaluation {
        evaluated_at: Some(context.evaluated_at()),
        timezone: Some(context.zone()),
        seed: context.seed(),
    }
}

pub(crate) fn row_evaluation(
    context: &EvaluationContext,
    row: usize,
) -> crate::compiled::trace::Evaluation {
    crate::compiled::trace::Evaluation {
        evaluated_at: Some(context.evaluated_at()),
        timezone: Some(context.zone()),
        seed: context.row_seed(row),
    }
}

pub(crate) struct Located;

impl Located {
    pub(crate) fn expression(node: &str, key: &str) -> u64 {
        location(&["expression", node, key])
    }

    pub(crate) fn input(node: &str) -> u64 {
        location(&["input", node])
    }

    pub(crate) fn statement(node: &str, statement: &str) -> u64 {
        location(&["switch", node, statement])
    }

    pub(crate) fn cell(node: &str, rule: &str, column: &str) -> u64 {
        location(&["cell", node, rule, column])
    }

    pub(crate) fn field(node: &str, column: &str) -> u64 {
        location(&["field", node, column])
    }

    pub(crate) fn function(node: &str) -> u64 {
        location(&["function", node])
    }

    pub(crate) fn decision(node: &str, key: &str) -> u64 {
        location(&["decision", node, key])
    }

    pub(crate) fn block(block: &str) -> u64 {
        location(&["block", block])
    }

    pub(crate) fn condition(block: &str, condition: &str) -> u64 {
        location(&["condition", block, condition])
    }

    pub(crate) fn arm(block: &str, arm: &str) -> u64 {
        location(&["arm", block, arm])
    }

    pub(crate) fn value(block: &str, arm: &str) -> u64 {
        location(&["value", block, arm])
    }
}

const PIN_JS: &str = include_str!("pinning.js");

#[derive(Debug, Clone, Default)]
pub(crate) struct JsSlot(Rc<RefCell<Option<EvaluationContext>>>);

pub(crate) struct Entered {
    slot: JsSlot,
    previous: Option<EvaluationContext>,
}

impl Drop for Entered {
    fn drop(&mut self) {
        *self.slot.0.borrow_mut() = self.previous.take();
    }
}

impl JsSlot {
    pub(crate) fn enter(&self, context: EvaluationContext) -> Entered {
        Entered {
            slot: self.clone(),
            previous: self.0.borrow_mut().replace(context),
        }
    }

    pub(crate) fn current(&self) -> Option<EvaluationContext> {
        self.0.borrow().clone()
    }

    fn now(&self) -> f64 {
        let now = match self.0.borrow().as_ref() {
            Some(context) => context.now(),
            None => system_now(),
        };
        now.timestamp_millis() as f64
    }

    fn random(&self) -> f64 {
        match self.0.borrow().as_ref() {
            Some(context) => context.random_f64(0, Site::SCRIPT),
            None => EvaluationContext::default().random_f64(0, Site::SCRIPT),
        }
    }

    fn timezone(&self) -> Timezone {
        match self.0.borrow().as_ref() {
            Some(context) => context.timezone(),
            None => system_timezone(),
        }
    }

    fn zone(&self, name: Option<String>) -> Option<Timezone> {
        match name {
            Some(name) => parse_timezone(&name),
            None => Some(self.timezone()),
        }
    }

    fn offset(&self, ms: f64, name: Option<String>) -> Option<f64> {
        let zone = self.zone(name)?;
        if !ms.is_finite() {
            return None;
        }
        let at = DateTime::from_timestamp_millis(ms as i64)?.naive_utc();
        let seconds = zone.offset_from_utc_datetime(&at).fix().local_minus_utc();
        Some(seconds as f64 / 60.0)
    }

    fn utc(&self, local: f64, name: Option<String>) -> Option<f64> {
        let zone = self.zone(name)?;
        if !local.is_finite() {
            return None;
        }
        let naive = DateTime::from_timestamp_millis(local as i64)?.naive_utc();
        let offset = match zone.offset_from_local_datetime(&naive) {
            LocalResult::Single(offset) => offset.fix(),
            LocalResult::Ambiguous(earlier, _) => earlier.fix(),
            LocalResult::None => zone
                .offset_from_local_datetime(&(naive - Duration::hours(3)))
                .earliest()?
                .fix(),
        };
        let utc = naive - Duration::seconds(offset.local_minus_utc() as i64);
        Some(utc.and_utc().timestamp_millis() as f64)
    }

    pub(crate) fn install(&self, ctx: &rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
        let globals = ctx.globals();
        if globals.get::<_, bool>("__zenPinned").unwrap_or(false) {
            return Ok(());
        }
        let slot = self.clone();
        globals.set("__zenNow", Func::from(move || slot.now()))?;
        let slot = self.clone();
        globals.set("__zenRandom", Func::from(move || slot.random()))?;
        let slot = self.clone();
        globals.set(
            "__zenOffset",
            Func::from(move |ms: f64, zone: Option<String>| slot.offset(ms, zone)),
        )?;
        let slot = self.clone();
        globals.set(
            "__zenUtc",
            Func::from(move |local: f64, zone: Option<String>| slot.utc(local, zone)),
        )?;
        let slot = self.clone();
        globals.set(
            "__zenZone",
            Func::from(move || slot.timezone().name().to_string()),
        )?;
        globals.set(
            "__zenCanonicalZone",
            Func::from(|name: String| parse_timezone(&name).map(|zone| zone.name().to_string())),
        )?;
        ctx.eval::<(), _>(PIN_JS)
    }
}
