use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zen_expression::variable::VariableMap;
use zen_expression::Variable;

use crate::decision::Decision;
use crate::engine::{DecisionEngine, EvaluationOptions};
use crate::error::ContentKindError;
use crate::loader::LoaderError;
use crate::EvaluationError;

/// Compares evaluation outcomes between two engines — candidate vs baseline.
/// The engines may differ in loaders, documents and configuration; the
/// comparison itself is engine-agnostic: evaluate both arms per input, compare
/// the result variables structurally, and materialise outputs only when they
/// differ (or fail).
pub struct ImpactAnalysis {
    candidate: Arc<DecisionEngine>,
    baseline: Arc<DecisionEngine>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImpactError {
    #[error("failed to load '{key}': {source}")]
    Load { key: String, source: LoaderError },
    #[error("'{key}' is not an evaluable graph: {source}")]
    ContentKind {
        key: String,
        source: ContentKindError,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactRow {
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Variable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Variable>,
    /// Carrying mode only: the single shared output of an UNCHANGED row —
    /// consumers that aggregate over every record read it as both arms while
    /// the payload stays at one copy instead of two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Variable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_error: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_error: Option<serde_json::Value>,
}

impl ImpactRow {
    pub fn failed(&self) -> bool {
        self.before_error.is_some() || self.after_error.is_some()
    }

    /// The aggregation context for this row — `{input, changed, before, after,
    /// errors?}`. Unchanged carrying rows expose the single shared result as
    /// both arms; `changed` is the engine's exact structural comparison (ZEN
    /// `!=` does not deep-compare objects, so specs must not recompute it).
    pub fn into_aggregate_context(self, input: Variable) -> Variable {
        let mut context = VariableMap::new();
        context.insert("input".into(), input);
        context.insert("changed".into(), Variable::Bool(self.changed));
        if self.failed() {
            let mut errors = VariableMap::new();
            if let Some(error) = &self.before_error {
                errors.insert("before".into(), Variable::from(error));
            }
            if let Some(error) = &self.after_error {
                errors.insert("after".into(), Variable::from(error));
            }
            context.insert("errors".into(), Variable::from_object(errors));
        }
        let (before, after) = match self.result {
            Some(result) => (Some(result.clone()), Some(result)),
            None => (self.before, self.after),
        };
        context.insert("before".into(), before.unwrap_or(Variable::Null));
        context.insert("after".into(), after.unwrap_or(Variable::Null));
        Variable::from_object(context)
    }
}

/// Headline counts over impact rows. `changed` counts rows where both arms
/// succeeded with differing results; rows with any failure count under
/// `failed` instead, so `total = unchanged + changed + failed`. Summaries
/// from separate batches merge additively, which is what makes incremental
/// runs exact rather than approximate.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactSummary {
    pub total: u64,
    pub changed: u64,
    pub failed: u64,
    pub before_errors: u64,
    pub after_errors: u64,
}

impl ImpactSummary {
    pub fn record(&mut self, row: &ImpactRow) {
        self.total += 1;
        if row.failed() {
            self.failed += 1;
            self.before_errors += u64::from(row.before_error.is_some());
            self.after_errors += u64::from(row.after_error.is_some());
        } else if row.changed {
            self.changed += 1;
        }
    }

    pub fn merge(&mut self, other: &ImpactSummary) {
        self.total += other.total;
        self.changed += other.changed;
        self.failed += other.failed;
        self.before_errors += other.before_errors;
        self.after_errors += other.after_errors;
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactBatch {
    pub rows: Vec<ImpactRow>,
    pub summary: ImpactSummary,
}

impl ImpactAnalysis {
    pub fn new(candidate: Arc<DecisionEngine>, baseline: Arc<DecisionEngine>) -> Self {
        Self {
            candidate,
            baseline,
        }
    }

    /// Resolves both decision handles once — every evaluation in the
    /// comparison reuses them, so decision content is parsed per comparison,
    /// never per record.
    pub async fn compare(
        &self,
        candidate_key: &str,
        baseline_key: &str,
    ) -> Result<ImpactComparison, ImpactError> {
        let candidate = resolve(&self.candidate, candidate_key).await?;
        let baseline = resolve(&self.baseline, baseline_key).await?;
        Ok(ImpactComparison {
            candidate,
            baseline,
        })
    }
}

async fn resolve(engine: &DecisionEngine, key: &str) -> Result<Decision, ImpactError> {
    engine
        .get_decision(key)
        .await
        .map_err(|source| ImpactError::Load {
            key: key.to_string(),
            source,
        })?
        .map_err(|source| ImpactError::ContentKind {
            key: key.to_string(),
            source,
        })
}

pub struct ImpactComparison {
    candidate: Decision,
    baseline: Decision,
}

impl ImpactComparison {
    /// Each input evaluates through both arms off the same variable (an Rc
    /// bump, not a copy). Per-arm failures land in the row — a bad record
    /// never aborts the batch.
    pub async fn run_batch(
        &self,
        inputs: Vec<Variable>,
        options: EvaluationOptions,
    ) -> ImpactBatch {
        self.run_batch_with(inputs, options, false).await
    }

    /// Like [`Self::run_batch`], but unchanged rows carry their single shared
    /// output in `result` — for consumers that aggregate over every record and
    /// would otherwise need both full outputs per row.
    pub async fn run_batch_carrying(
        &self,
        inputs: Vec<Variable>,
        options: EvaluationOptions,
    ) -> ImpactBatch {
        self.run_batch_with(inputs, options, true).await
    }

    async fn run_batch_with(
        &self,
        inputs: Vec<Variable>,
        options: EvaluationOptions,
        carry_unchanged: bool,
    ) -> ImpactBatch {
        let mut rows = Vec::with_capacity(inputs.len());
        let mut summary = ImpactSummary::default();
        for input in inputs {
            let row = self.run_one_with(input, options, carry_unchanged).await;
            summary.record(&row);
            rows.push(row);
        }
        ImpactBatch { rows, summary }
    }

    pub async fn run_one_carrying(&self, input: Variable, options: EvaluationOptions) -> ImpactRow {
        self.run_one_with(input, options, true).await
    }

    async fn run_one_with(
        &self,
        input: Variable,
        options: EvaluationOptions,
        carry_unchanged: bool,
    ) -> ImpactRow {
        let after = self
            .candidate
            .evaluate_with_opts(input.clone(), options)
            .await;
        let before = self.baseline.evaluate_with_opts(input, options).await;

        match (before, after) {
            (Ok(before), Ok(after)) => {
                let changed = before.result != after.result;
                ImpactRow {
                    changed,
                    before: changed.then_some(before.result),
                    after: changed.then(|| after.result.clone()),
                    result: (!changed && carry_unchanged).then_some(after.result),
                    before_error: None,
                    after_error: None,
                }
            }
            (before, after) => {
                let (before_value, before_error) = split(before);
                let (after_value, after_error) = split(after);
                ImpactRow {
                    changed: true,
                    before: before_value,
                    after: after_value,
                    result: None,
                    before_error,
                    after_error,
                }
            }
        }
    }
}

fn split(
    outcome: Result<crate::DecisionGraphResponse, Box<EvaluationError>>,
) -> (Option<Variable>, Option<serde_json::Value>) {
    match outcome {
        Ok(response) => (Some(response.result), None),
        Err(error) => (
            None,
            Some(
                error
                    .serialize_with_mode(
                        serde_json::value::Serializer,
                        crate::engine::EvaluationTraceKind::None,
                    )
                    .unwrap_or_default(),
            ),
        ),
    }
}
