use crate::engine::EvaluationOptions;
use chrono::{DateTime, Duration, LocalResult, Offset, TimeZone};
use rquickjs::loader::bundle::{Bundle, ScaBundleData};
use rquickjs::loader::Loader;
use rquickjs::prelude::Func;
use rquickjs::promise::PromiseState;
use rquickjs::{Module, WriteOptions};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;
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
    pub(crate) settings: crate::compiled::host::Settings,
    pub(crate) context: EvaluationContext,
}

impl Invocation {
    fn new(options: &EvaluationOptions, context: EvaluationContext) -> Self {
        Self {
            trace: options.trace,
            settings: crate::compiled::host::Settings::of(options),
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

const PIN_ENTRY: (&str, &str) = ("zen:pinning", include_str!("pinning.entry.js"));
const PIN_DATE: (&str, &str) = ("zen:pinning/date", include_str!("pinning.js"));

fn canonical_zone(name: &str) -> Option<Timezone> {
    static ZONES: OnceLock<HashMap<String, Timezone>> = OnceLock::new();
    parse_timezone(name).or_else(|| {
        ZONES
            .get_or_init(|| {
                chrono_tz::TZ_VARIANTS
                    .iter()
                    .map(|zone| (zone.name().to_ascii_lowercase(), *zone))
                    .collect()
            })
            .get(&name.to_ascii_lowercase())
            .copied()
    })
}

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
            Some(name) => canonical_zone(&name),
            None => Some(self.timezone()),
        }
    }

    fn modelled(ms: f64) -> Option<chrono::NaiveDateTime> {
        use chrono::Datelike;
        const CYCLE: f64 = 146_097.0 * 86_400_000.0;
        const SPAN: f64 = 250_000.0 * 365.2425 * 86_400_000.0;
        let cycles = match ms.abs() > SPAN {
            true => ((ms.abs() - SPAN) / CYCLE).ceil() * ms.signum(),
            false => 0.0,
        };
        let at = DateTime::from_timestamp_millis((ms - cycles * CYCLE) as i64)?.naive_utc();
        if at.year() <= 2099 {
            return Some(at);
        }
        let year = chrono::NaiveDate::from_yo_opt(at.year(), 1)?;
        let twin = (2072..=2099).find(|candidate| {
            chrono::NaiveDate::from_yo_opt(*candidate, 1).is_some_and(|first| {
                first.leap_year() == year.leap_year() && first.weekday() == year.weekday()
            })
        })?;
        Some(chrono::NaiveDate::from_yo_opt(twin, at.ordinal())?.and_time(at.time()))
    }

    fn offset(&self, ms: f64, name: Option<String>) -> Option<f64> {
        let zone = self.zone(name)?;
        if !ms.is_finite() {
            return None;
        }
        let at = Self::modelled(ms)?;
        let seconds = zone.offset_from_utc_datetime(&at).fix().local_minus_utc();
        Some(seconds as f64 / 60.0)
    }

    fn utc(&self, local: f64, name: Option<String>) -> Option<f64> {
        let zone = self.zone(name)?;
        if !local.is_finite() {
            return None;
        }
        let naive = Self::modelled(local)?;
        let offset = match zone.offset_from_local_datetime(&naive) {
            LocalResult::Single(offset) => offset.fix(),
            LocalResult::Ambiguous(earlier, _) => earlier.fix(),
            LocalResult::None => zone
                .offset_from_local_datetime(&(naive - Duration::hours(3)))
                .earliest()?
                .fix(),
        };
        Some(local - offset.local_minus_utc() as f64 * 1000.0)
    }

    fn bundle() -> Option<Bundle<ScaBundleData<&'static [u8]>>> {
        static CODE: OnceLock<Option<[Vec<u8>; 2]>> = OnceLock::new();
        static BUNDLE: OnceLock<Option<[(&'static str, &'static [u8]); 2]>> = OnceLock::new();
        BUNDLE
            .get_or_init(|| {
                let code = CODE.get_or_init(|| {
                    let runtime = rquickjs::Runtime::new().ok()?;
                    let context = rquickjs::Context::full(&runtime).ok()?;
                    context.with(|ctx| {
                        let compile = |name: &str, source: &str| {
                            Module::declare(ctx.clone(), name, source)
                                .and_then(|module| {
                                    module.write(WriteOptions {
                                        strip_source: true,
                                        ..Default::default()
                                    })
                                })
                                .ok()
                        };
                        Some([
                            compile(PIN_ENTRY.0, PIN_ENTRY.1)?,
                            compile(PIN_DATE.0, PIN_DATE.1)?,
                        ])
                    })
                });
                code.as_ref().map(|[entry, date]| {
                    [
                        (PIN_ENTRY.0, entry.as_slice()),
                        (PIN_DATE.0, date.as_slice()),
                    ]
                })
            })
            .as_ref()
            .map(|entries| Bundle(entries.as_slice()))
    }

    fn evaluate<'js>(
        ctx: &rquickjs::Ctx<'js>,
        (name, source): (&str, &str),
    ) -> rquickjs::Result<rquickjs::Function<'js>> {
        let declared = match Self::bundle() {
            Some(mut bundle) => bundle.load(ctx, name)?,
            None => Module::declare(ctx.clone(), name, source)?,
        };
        let (module, promise) = declared.eval()?;
        if promise.state() != PromiseState::Resolved {
            return Err(rquickjs::Error::new_loading(name));
        }
        module.get("default")
    }

    pub(crate) fn install<'js>(&self, ctx: &rquickjs::Ctx<'js>) -> rquickjs::Result<()> {
        let entry = Self::evaluate(ctx, PIN_ENTRY)?;
        let (now, random, slot) = (self.clone(), self.clone(), self.clone());
        entry.call((
            Func::from(move || now.now()),
            Func::from(move || random.random()),
            Func::from(
                move |ctx: rquickjs::Ctx<'js>,
                      native: rquickjs::Value<'js>,
                      pinned: rquickjs::Value<'js>| {
                    slot.load(&ctx, native, pinned)
                },
            ),
        ))
    }

    fn load<'js>(
        &self,
        ctx: &rquickjs::Ctx<'js>,
        native: rquickjs::Value<'js>,
        pinned: rquickjs::Value<'js>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
        let date = Self::evaluate(ctx, PIN_DATE)?;
        let (now, offset, utc) = (self.clone(), self.clone(), self.clone());
        date.call((
            native,
            pinned,
            Func::from(move || now.now()),
            Func::from(move |ms: f64, zone: Option<String>| offset.offset(ms, zone)),
            Func::from(move |local: f64, zone: Option<String>| utc.utc(local, zone)),
            Func::from(|name: String| canonical_zone(&name).map(|zone| zone.name().to_string())),
        ))
    }
}
