use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use zen_expression::expression::Standard;
use zen_expression::vm::VM;
use zen_expression::{compile_expression, Expression, Variable};

/// Declarative, mergeable aggregation over impact records — the SQL
/// `GROUP BY` + `FILTER (WHERE …)` vocabulary compiled to ZEN expressions.
/// Every metric is distributive or algebraic, so shard states merge exactly;
/// holistic metrics (quantiles, distinct) are deliberately out of scope.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateSpec {
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
    pub metrics: BTreeMap<String, MetricSpec>,
}

/// One raw metric declaration — classified by which field is present.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricSpec {
    /// `true` counts every record; an expression counts strict-`true` results.
    #[serde(default)]
    pub count: Option<Value>,
    #[serde(default)]
    pub sum: Option<String>,
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub max: Option<String>,
    /// Group key expression; `metrics` nest one level (no groups in groups).
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub metrics: Option<BTreeMap<String, MetricSpec>>,
    /// Bounded exemplar collection: keeps the first N matching records
    /// (by record index, exact across shard merges).
    #[serde(default)]
    pub sample: Option<usize>,
    /// Sample payload expression (an object literal, usually).
    #[serde(default)]
    pub value: Option<String>,
    /// Ranked samples: keep the N records with the LARGEST value of this
    /// expression (numeric), instead of the first N. Mergeable exactly.
    #[serde(default)]
    pub rank_by: Option<String>,
    /// SQL FILTER clause: the metric sees only records where this is `true`.
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AggregateError {
    #[error("metric '{name}': {message}")]
    Spec { name: String, message: String },
    #[error("expression '{source_text}' failed to compile: {message}")]
    Compile {
        source_text: String,
        message: String,
    },
    #[error("state does not match the spec: {0}")]
    State(String),
}

enum CompiledKind {
    Count(Option<Expression<Standard>>),
    Sum(Expression<Standard>),
    Min(Expression<Standard>),
    Max(Expression<Standard>),
    Group {
        by: Expression<Standard>,
        metrics: Vec<CompiledMetric>,
    },
    Sample {
        limit: usize,
        value: Expression<Standard>,
        rank_by: Option<Expression<Standard>>,
    },
}

struct CompiledMetric {
    name: String,
    when: Option<Expression<Standard>>,
    kind: CompiledKind,
}

pub struct Aggregator {
    facts: Vec<(String, Expression<Standard>)>,
    metrics: Vec<CompiledMetric>,
    state: Vec<MetricState>,
    index: u64,
    /// One VM reused across every expression evaluation — a fresh VM per call
    /// costs more than the evaluations themselves at ~19 expressions/record.
    vm: VM,
}

#[derive(Debug, Clone)]
enum MetricState {
    Count(u64),
    Sum(Decimal),
    Min(Option<Decimal>),
    Max(Option<Decimal>),
    Group(BTreeMap<String, Vec<MetricState>>),
    /// (order key, payload): record index for first-N samples, negated rank
    /// for ranked ones — ascending sort works for both.
    Sample(Vec<(Decimal, Variable)>),
}

fn compile(source: &str) -> Result<Expression<Standard>, AggregateError> {
    compile_expression(source).map_err(|e| AggregateError::Compile {
        source_text: source.to_string(),
        message: e.to_string(),
    })
}

fn compile_metric(
    name: &str,
    spec: &MetricSpec,
    nested: bool,
) -> Result<CompiledMetric, AggregateError> {
    let err = |message: &str| AggregateError::Spec {
        name: name.to_string(),
        message: message.to_string(),
    };

    let when = spec.when.as_deref().map(compile).transpose()?;
    let kind = if let Some(count) = &spec.count {
        match count {
            Value::Bool(true) => CompiledKind::Count(None),
            Value::String(expression) => CompiledKind::Count(Some(compile(expression)?)),
            _ => return Err(err("count must be true or an expression")),
        }
    } else if let Some(sum) = &spec.sum {
        CompiledKind::Sum(compile(sum)?)
    } else if let Some(min) = &spec.min {
        CompiledKind::Min(compile(min)?)
    } else if let Some(max) = &spec.max {
        CompiledKind::Max(compile(max)?)
    } else if let Some(by) = &spec.by {
        if nested {
            return Err(err("groups cannot nest"));
        }
        let Some(metric_specs) = &spec.metrics else {
            return Err(err("a group needs `metrics`"));
        };
        let mut metrics = Vec::with_capacity(metric_specs.len());
        for (child_name, child) in metric_specs {
            metrics.push(compile_metric(child_name, child, true)?);
        }
        CompiledKind::Group {
            by: compile(by)?,
            metrics,
        }
    } else if let Some(limit) = spec.sample {
        let Some(value) = &spec.value else {
            return Err(err("a sample needs `value`"));
        };
        CompiledKind::Sample {
            limit,
            value: compile(value)?,
            rank_by: spec.rank_by.as_deref().map(compile).transpose()?,
        }
    } else {
        return Err(err("declare one of count/sum/min/max/by/sample"));
    };

    Ok(CompiledMetric {
        name: name.to_string(),
        when,
        kind,
    })
}

fn with_facts(
    facts: &[(String, Expression<Standard>)],
    context: Variable,
    vm: &mut VM,
) -> Variable {
    if facts.is_empty() {
        return context;
    }
    let augmented = context.depth_clone(1);
    if let Variable::Object(object) = &augmented {
        let mut computed = Vec::with_capacity(facts.len());
        for (name, expression) in facts {
            if let Ok(value) = eval(expression, &augmented, vm) {
                computed.push((name.as_str(), value));
            }
        }
        let mut borrowed = object.borrow_mut();
        for (name, value) in computed {
            borrowed.insert(name.into(), value);
        }
    }
    augmented
}

fn initial_state(metric: &CompiledMetric) -> MetricState {
    match &metric.kind {
        CompiledKind::Count(_) => MetricState::Count(0),
        CompiledKind::Sum(_) => MetricState::Sum(Decimal::ZERO),
        CompiledKind::Min(_) => MetricState::Min(None),
        CompiledKind::Max(_) => MetricState::Max(None),
        CompiledKind::Group { .. } => MetricState::Group(BTreeMap::new()),
        CompiledKind::Sample { .. } => MetricState::Sample(Vec::new()),
    }
}

fn is_true(value: Result<Variable, zen_expression::IsolateError>) -> bool {
    matches!(value, Ok(Variable::Bool(true)))
}

fn eval(
    expression: &Expression<Standard>,
    context: &Variable,
    vm: &mut VM,
) -> Result<Variable, zen_expression::IsolateError> {
    expression.evaluate_with(context.clone(), vm)
}

fn as_decimal(value: Result<Variable, zen_expression::IsolateError>) -> Option<Decimal> {
    match value {
        Ok(Variable::Number(number)) => Some(number),
        _ => None,
    }
}

fn group_key(value: Result<Variable, zen_expression::IsolateError>) -> Option<String> {
    match value {
        Ok(Variable::String(text)) => Some(text.to_string()),
        Ok(Variable::Number(number)) => Some(number.to_string()),
        Ok(Variable::Bool(flag)) => Some(flag.to_string()),
        _ => None,
    }
}

fn apply(
    metric: &CompiledMetric,
    state: &mut MetricState,
    context: &Variable,
    index: u64,
    vm: &mut VM,
) {
    if let Some(when) = &metric.when {
        if !is_true(eval(when, context, vm)) {
            return;
        }
    }
    match (&metric.kind, state) {
        (CompiledKind::Count(condition), MetricState::Count(count)) => {
            let matched = match condition {
                None => true,
                Some(expression) => is_true(eval(expression, context, vm)),
            };
            if matched {
                *count += 1;
            }
        }
        (CompiledKind::Sum(expression), MetricState::Sum(total)) => {
            if let Some(number) = as_decimal(eval(expression, context, vm)) {
                *total += number;
            }
        }
        (CompiledKind::Min(expression), MetricState::Min(slot)) => {
            if let Some(number) = as_decimal(eval(expression, context, vm)) {
                *slot = Some(slot.map_or(number, |current| current.min(number)));
            }
        }
        (CompiledKind::Max(expression), MetricState::Max(slot)) => {
            if let Some(number) = as_decimal(eval(expression, context, vm)) {
                *slot = Some(slot.map_or(number, |current| current.max(number)));
            }
        }
        (CompiledKind::Group { by, metrics }, MetricState::Group(groups)) => {
            let Some(key) = group_key(eval(by, context, vm)) else {
                return;
            };
            let states = groups
                .entry(key)
                .or_insert_with(|| metrics.iter().map(initial_state).collect());
            for (child, child_state) in metrics.iter().zip(states.iter_mut()) {
                apply(child, child_state, context, index, vm);
            }
        }
        (
            CompiledKind::Sample {
                limit,
                value,
                rank_by,
            },
            MetricState::Sample(items),
        ) => {
            let key = match rank_by {
                None => Decimal::from(index),
                Some(expression) => match as_decimal(eval(expression, context, vm)) {
                    Some(rank) => -rank,
                    None => return,
                },
            };
            if items.len() >= *limit {
                let Some((worst, _)) = items.last() else {
                    return;
                };
                if key >= *worst {
                    return;
                }
            }
            if let Ok(payload) = eval(value, context, vm) {
                items.push((key, payload));
                items.sort_by(|a, b| a.0.cmp(&b.0));
                items.truncate(*limit);
            }
        }
        _ => {}
    }
}

fn merge_state(metric: &CompiledMetric, into: &mut MetricState, from: MetricState) {
    match (into, from) {
        (MetricState::Count(a), MetricState::Count(b)) => *a += b,
        (MetricState::Sum(a), MetricState::Sum(b)) => *a += b,
        (MetricState::Min(a), MetricState::Min(b)) => {
            *a = match (*a, b) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            }
        }
        (MetricState::Max(a), MetricState::Max(b)) => {
            *a = match (*a, b) {
                (Some(x), Some(y)) => Some(x.max(y)),
                (x, y) => x.or(y),
            }
        }
        (MetricState::Group(a), MetricState::Group(b)) => {
            let CompiledKind::Group { metrics, .. } = &metric.kind else {
                return;
            };
            for (key, states) in b {
                match a.remove(&key) {
                    None => {
                        a.insert(key, states);
                    }
                    Some(mut existing) => {
                        for ((child, into_state), from_state) in
                            metrics.iter().zip(existing.iter_mut()).zip(states)
                        {
                            merge_state(child, into_state, from_state);
                        }
                        a.insert(key, existing);
                    }
                }
            }
        }
        (MetricState::Sample(a), MetricState::Sample(b)) => {
            let CompiledKind::Sample { limit, .. } = &metric.kind else {
                return;
            };
            a.extend(b);
            a.sort_by(|x, y| x.0.cmp(&y.0));
            a.truncate(*limit);
        }
        _ => {}
    }
}

fn state_to_value(state: &MetricState) -> Value {
    match state {
        MetricState::Count(count) => Value::from(*count),
        MetricState::Sum(total) => decimal_value(*total),
        MetricState::Min(slot) | MetricState::Max(slot) => {
            slot.map(decimal_value).unwrap_or(Value::Null)
        }
        MetricState::Group(groups) => Value::Object(
            groups
                .iter()
                .map(|(key, states)| {
                    (key.clone(), Value::Array(states.iter().map(state_to_value).collect()))
                })
                .collect(),
        ),
        MetricState::Sample(items) => Value::Array(
            items
                .iter()
                .map(|(key, value)| serde_json::json!({ "i": key.to_string(), "v": value.to_value() }))
                .collect(),
        ),
    }
}

fn decimal_value(number: Decimal) -> Value {
    serde_json::from_str::<serde_json::Number>(&number.normalize().to_string())
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn state_from_value(metric: &CompiledMetric, value: Value) -> Result<MetricState, AggregateError> {
    let bad = |what: &str| AggregateError::State(format!("metric '{}': {what}", metric.name));
    let number = |value: Value, what: &str| -> Result<Decimal, AggregateError> {
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_f64().map(|f| f.to_string()))
            .or_else(|| Some(value.to_string()))
            .and_then(|text| text.parse::<Decimal>().ok())
            .ok_or_else(|| bad(what))
    };
    Ok(match &metric.kind {
        CompiledKind::Count(_) => MetricState::Count(value.as_u64().ok_or_else(|| bad("count"))?),
        CompiledKind::Sum(_) => MetricState::Sum(number(value, "sum")?),
        CompiledKind::Min(_) => MetricState::Min(match value {
            Value::Null => None,
            other => Some(number(other, "min")?),
        }),
        CompiledKind::Max(_) => MetricState::Max(match value {
            Value::Null => None,
            other => Some(number(other, "max")?),
        }),
        CompiledKind::Group { metrics, .. } => {
            let Value::Object(map) = value else {
                return Err(bad("group"));
            };
            let mut groups = BTreeMap::new();
            for (key, entry) in map {
                let Value::Array(items) = entry else {
                    return Err(bad("group entry"));
                };
                if items.len() != metrics.len() {
                    return Err(bad("group arity"));
                }
                let states = metrics
                    .iter()
                    .zip(items)
                    .map(|(child, item)| state_from_value(child, item))
                    .collect::<Result<Vec<_>, _>>()?;
                groups.insert(key, states);
            }
            MetricState::Group(groups)
        }
        CompiledKind::Sample { .. } => {
            let Value::Array(items) = value else {
                return Err(bad("sample"));
            };
            let mut collected = Vec::with_capacity(items.len());
            for item in items {
                let key = item
                    .get("i")
                    .and_then(|raw| match raw {
                        Value::String(text) => text.parse::<Decimal>().ok(),
                        Value::Number(number) => number.to_string().parse::<Decimal>().ok(),
                        _ => None,
                    })
                    .ok_or_else(|| bad("sample key"))?;
                let payload = item.get("v").cloned().unwrap_or(Value::Null);
                collected.push((key, Variable::from(&payload)));
            }
            MetricState::Sample(collected)
        }
    })
}

impl Aggregator {
    pub fn compile(spec: &AggregateSpec) -> Result<Self, AggregateError> {
        let mut facts = Vec::with_capacity(spec.facts.len());
        for (name, source) in &spec.facts {
            facts.push((name.clone(), compile(source)?));
        }
        let mut metrics = Vec::with_capacity(spec.metrics.len());
        for (name, metric) in &spec.metrics {
            metrics.push(compile_metric(name, metric, false)?);
        }
        let state = metrics.iter().map(initial_state).collect();
        Ok(Self {
            facts,
            metrics,
            state,
            index: 0,
            vm: VM::new(),
        })
    }

    /// Records one impact context — `{input, before, after, errors?}` — after
    /// augmenting it with the computed facts at top level (facts win on name
    /// clashes, mirroring how the canonical filter context shadows).
    pub fn record(&mut self, context: Variable) {
        let index = self.index;
        self.record_at(context, index);
    }

    /// Like [`Self::record`] with an explicit GLOBAL record index — sharded
    /// hosts pass the dataset position so sample ordering merges exactly.
    pub fn record_at(&mut self, context: Variable, index: u64) {
        let context = with_facts(&self.facts, context, &mut self.vm);
        self.index += 1;
        for (metric, state) in self.metrics.iter().zip(self.state.iter_mut()) {
            apply(metric, state, &context, index, &mut self.vm);
        }
    }

    /// Opaque, additive state — ship it across any boundary and merge exactly.
    pub fn state(&self) -> Value {
        serde_json::json!({
            "index": self.index,
            "metrics": self
                .metrics
                .iter()
                .zip(self.state.iter())
                .map(|(metric, state)| (metric.name.clone(), state_to_value(state)))
                .collect::<serde_json::Map<String, Value>>(),
        })
    }

    pub fn merge_value(&mut self, state: Value) -> Result<(), AggregateError> {
        let index = state
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| AggregateError::State("missing index".to_string()))?;
        let Some(Value::Object(map)) = state.get("metrics").cloned().into() else {
            return Err(AggregateError::State("missing metrics".to_string()));
        };
        self.index += index;
        for (metric, into) in self.metrics.iter().zip(self.state.iter_mut()) {
            let Some(entry) = map.get(&metric.name).cloned() else {
                return Err(AggregateError::State(format!(
                    "missing metric '{}'",
                    metric.name
                )));
            };
            let from = state_from_value(metric, entry)?;
            merge_state(metric, into, from);
        }
        Ok(())
    }

    /// The aggregate results as a report-template context: scalars for
    /// counts/sums, `[{key, …metrics}]` arrays for groups (key-ordered),
    /// index-ordered payload arrays for samples.
    pub fn finalize(&self) -> Value {
        let mut out = serde_json::Map::new();
        out.insert("recordCount".to_string(), Value::from(self.index));
        for (metric, state) in self.metrics.iter().zip(self.state.iter()) {
            out.insert(metric.name.clone(), finalize_state(metric, state));
        }
        Value::Object(out)
    }

    /// Renders a report template against the finalized aggregate: any object
    /// of the exact shape `{"$": "expression"}` evaluates in place; every
    /// other node passes through untouched. Iteration, arithmetic and string
    /// assembly all live in the expressions themselves (`map(bands, {...})`).
    pub fn render(&self, template: &Value) -> Value {
        render_node(template, &Variable::from(&self.finalize()))
    }
}

fn finalize_state(metric: &CompiledMetric, state: &MetricState) -> Value {
    match state {
        MetricState::Count(count) => Value::from(*count),
        MetricState::Sum(total) => decimal_value(*total),
        MetricState::Min(slot) | MetricState::Max(slot) => {
            slot.map(decimal_value).unwrap_or(Value::Null)
        }
        MetricState::Group(groups) => {
            let CompiledKind::Group { metrics, .. } = &metric.kind else {
                return Value::Null;
            };
            Value::Array(
                groups
                    .iter()
                    .map(|(key, states)| {
                        let mut object = serde_json::Map::new();
                        object.insert("key".to_string(), Value::String(key.clone()));
                        for (child, child_state) in metrics.iter().zip(states.iter()) {
                            object.insert(child.name.clone(), finalize_state(child, child_state));
                        }
                        Value::Object(object)
                    })
                    .collect(),
            )
        }
        MetricState::Sample(items) => {
            Value::Array(items.iter().map(|(_, value)| value.to_value()).collect())
        }
    }
}

fn render_node(template: &Value, context: &Variable) -> Value {
    match template {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(Value::String(expression)) = map.get("$") {
                    return zen_expression::evaluate_expression(expression, context.clone())
                        .map(|value| value.to_value())
                        .unwrap_or(Value::Null);
                }
            }
            Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), render_node(value, context)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_node(item, context))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_drilldown() {
        let parsed: AggregateSpec = serde_json::from_value(serde_json::json!({
            "facts": {
                "band": "input.applicant.creditScore >= 760 ? '760+' : input.applicant.creditScore >= 700 ? '700-759' : input.applicant.creditScore >= 660 ? '660-699' : input.applicant.creditScore >= 620 ? '620-659' : 'under 620'",
                "approvedBoth": "before.approved == true and after.approved == true",
            },
            "metrics": {
                "total": { "count": true },
                "failed": { "count": "errors != null" },
                "approvedBefore": { "count": "before.approved == true" },
                "approvedAfter": { "count": "after.approved == true" },
                "rateSumBefore": { "sum": "before.interestRate", "when": "approvedBoth" },
                "rateSumAfter": { "sum": "after.interestRate", "when": "approvedBoth" },
                "rateCount": { "count": "approvedBoth" },
                "limitBefore": { "sum": "before.creditLimit" },
                "limitAfter": { "sum": "after.creditLimit" },
                "newlyApproved": { "count": "before.approved != true and after.approved == true" },
                "newlyDeclined": { "count": "before.approved == true and after.approved != true" },
                "bands": { "by": "band", "metrics": {
                    "total": { "count": true },
                    "approvedBefore": { "count": "before.approved == true" },
                    "approvedAfter": { "count": "after.approved == true" },
                    "limitBefore": { "sum": "before.creditLimit" },
                    "limitAfter": { "sum": "after.creditLimit" },
                } },
                "movers": { "sample": 8, "when": "before.approved != after.approved", "value": "{id: input.evaluationId}" },
            },
        })).unwrap();
        let mut aggregator = Aggregator::compile(&parsed).unwrap();
        let contexts: Vec<Variable> = (0..20_000u64).map(|i| Variable::from(&serde_json::json!({
            "input": { "evaluationId": format!("e{i}"), "applicant": { "creditScore": 500 + (i % 400) } },
            "before": { "approved": i % 4 == 0, "interestRate": 7.49, "creditLimit": if i % 4 == 0 { 5000 } else { 0 } },
            "after": { "approved": i % 4 == 0, "interestRate": 7.49, "creditLimit": if i % 4 == 0 { 5000 } else { 0 } },
        }))).collect();
        let started = std::time::Instant::now();
        for context in &contexts {
            aggregator.record(context.clone());
        }
        let elapsed = started.elapsed();
        eprintln!(
            "record(): {:.2} us/rec over {} records",
            elapsed.as_micros() as f64 / contexts.len() as f64,
            contexts.len()
        );
    }

    fn spec() -> AggregateSpec {
        serde_json::from_value(serde_json::json!({
            "facts": {
                "band": "input.score >= 700 ? 'high' : 'low'",
                "flip": "before.approved != after.approved",
            },
            "metrics": {
                "total": { "count": true },
                "approvedAfter": { "count": "after.approved == true" },
                "limitAfter": { "sum": "after.limit" },
                "bestRate": { "min": "after.rate", "when": "after.approved == true" },
                "bands": {
                    "by": "band",
                    "metrics": {
                        "total": { "count": true },
                        "approvedAfter": { "count": "after.approved == true" },
                    },
                },
                "movers": {
                    "sample": 2,
                    "when": "flip",
                    "value": "{id: input.id, after: after.approved}",
                },
            },
        }))
        .unwrap()
    }

    fn record(id: u64, score: i64, before: bool, after: bool, limit: i64, rate: f64) -> Variable {
        Variable::from(&serde_json::json!({
            "input": { "id": id, "score": score },
            "before": { "approved": before },
            "after": { "approved": after, "limit": limit, "rate": rate },
        }))
    }

    #[test]
    fn aggregates_and_merges() {
        let parsed = spec();
        let mut a = Aggregator::compile(&parsed).unwrap();
        let mut b = Aggregator::compile(&parsed).unwrap();

        a.record(record(1, 720, true, true, 5000, 7.5));
        a.record(record(2, 650, true, false, 0, 9.0));
        b.record_at(record(3, 710, false, true, 3000, 6.5), 2);
        b.record_at(record(4, 600, false, false, 0, 0.0), 3);

        let mut merged = Aggregator::compile(&parsed).unwrap();
        merged.merge_value(a.state()).unwrap();
        merged.merge_value(b.state()).unwrap();
        let out = merged.finalize();

        assert_eq!(out["recordCount"], 4);
        assert_eq!(out["total"], Value::from(4u64));
        assert_eq!(out["approvedAfter"], Value::from(2u64));
        assert_eq!(out["limitAfter"].to_string(), "8000");
        assert_eq!(out["bestRate"].to_string(), "6.5");
        let bands = out["bands"].as_array().unwrap();
        assert_eq!(bands.len(), 2);
        assert_eq!(bands[0]["key"], "high");
        assert_eq!(bands[0]["total"], Value::from(2u64));
        assert_eq!(bands[0]["approvedAfter"], Value::from(2u64));
        let movers = out["movers"].as_array().unwrap();
        assert_eq!(movers.len(), 2);
        assert_eq!(movers[0]["id"], Value::from(2u64));

        let report = merged.render(&serde_json::json!({
            "summary": { "$": "approvedAfter == 2 ? 'ok' : 'bad'" },
            "series": { "$": "map(bands, {label: #.key, value: #.approvedAfter / #.total})" },
            "static": "unchanged",
        }));
        assert_eq!(report["summary"], "ok");
        assert_eq!(report["static"], "unchanged");
        assert_eq!(report["series"].as_array().unwrap().len(), 2);
    }
}
