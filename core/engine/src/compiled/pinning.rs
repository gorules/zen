mod calendar;
mod date;

use crate::engine::EvaluationOptions;
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

    pub(crate) fn install(&self, ctx: &rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
        date::install(ctx, self)
    }
}
