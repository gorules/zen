pub mod api;
pub(crate) mod batch;
pub(crate) mod collect;
mod format;
pub(crate) mod migrate;
pub(crate) mod record;
mod v1;

pub(crate) use format::{core, transcode, value};
pub(crate) use v1::{native, policy_ser, rows};

pub(crate) use api::Evaluation;
use api::{
    bad, malformed, ConvertArgs, ConvertOutput, Input, LegacyTrace, Need, NodeError, RestoreArgs,
    Status, TraceError, R,
};
pub(crate) use format::graph::plain_path;
use policy_ser::PolicyDocument;
use transcode::JsonText;

use crate::decision_graph::graph::{DecisionGraphResponse, EvaluationTrace};
use crate::decision_graph::DecisionGraphTrace;
use crate::workspace::types::Trace;
use crate::EvaluationError;
use ahash::HashMap;
use serde::{Serialize, Serializer};
use serde_json::value::RawValue;
use std::sync::{Arc, OnceLock};
use strum::{EnumString, IntoStaticStr};
use zen_expression::variable::{ToVariable, Variable};

/// Trace format requested from an evaluation.
///
/// `V1` and `Legacy` apply to the compiled engine, the default. The interpreted engine always
/// produces legacy traces: any format other than `Off` returns the legacy trace, exactly as
/// before format v1 existed, and the `now`, `timezone` and `seed` evaluation options are ignored
/// by it (it reads the real clock and draws unseeded random numbers).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, EnumString, IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum TraceFormat {
    #[default]
    Off,
    V1,
    Legacy,
}

impl TraceFormat {
    pub fn is_on(self) -> bool {
        self != TraceFormat::Off
    }
}

impl From<bool> for TraceFormat {
    fn from(value: bool) -> Self {
        match value {
            true => TraceFormat::V1,
            false => TraceFormat::Off,
        }
    }
}

#[derive(Debug)]
struct Document {
    json: OnceLock<String>,
    policy: Option<PolicyDocument>,
    verbatim: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct TraceDocument {
    inner: Arc<Document>,
}

impl TraceDocument {
    pub(crate) fn new(json: String, verbatim: Option<Arc<str>>) -> Self {
        Self {
            inner: Arc::new(Document {
                json: OnceLock::from(json),
                policy: None,
                verbatim,
            }),
        }
    }

    pub(crate) fn policy(policy: PolicyDocument) -> Self {
        Self {
            inner: Arc::new(Document {
                json: OnceLock::new(),
                policy: Some(policy),
                verbatim: None,
            }),
        }
    }

    pub(crate) fn failed(reason: String) -> Self {
        Self::new("null".to_string(), Some(Arc::from(reason)))
    }

    pub(crate) fn from_output(output: ConvertOutput) -> Self {
        let verbatim = match output.status {
            Status::Converted => None,
            Status::Verbatim { reason } => Some(Arc::from(reason)),
        };
        Self::new(output.json, verbatim)
    }

    pub fn json(&self) -> &str {
        self.inner.json.get_or_init(|| match &self.inner.policy {
            Some(policy) => policy.json(),
            None => "null".to_string(),
        })
    }

    pub fn raw(&self) -> Option<&RawValue> {
        serde_json::from_str(self.json()).ok()
    }

    pub fn verbatim_reason(&self) -> Option<&str> {
        self.inner.verbatim.as_deref()
    }

    pub fn to_variable(&self) -> Variable {
        api::parse_document(self.json()).unwrap_or(Variable::Null)
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::from_str(self.json()).unwrap_or_default()
    }
}

fn json_serializer<S>() -> bool {
    std::any::type_name::<S>().starts_with("&mut serde_json::ser::Serializer<")
}

struct Unchecked<'a>(&'a str);

impl Serialize for Unchecked<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        const TOKEN: &str = "$serde_json::private::RawValue";
        let mut s = serializer.serialize_struct(TOKEN, 1)?;
        s.serialize_field(TOKEN, self.0)?;
        s.end()
    }
}

impl Serialize for TraceDocument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match (
            json_serializer::<S>(),
            self.inner.json.get(),
            &self.inner.policy,
        ) {
            (true, None, Some(policy)) => policy.serialize(serializer),
            (true, _, _) => Unchecked(self.json()).serialize(serializer),
            (false, _, _) => JsonText(self.json()).serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PolicyTrace {
    Legacy(Trace),
    V1(TraceDocument),
}

impl PolicyTrace {
    pub fn as_legacy(&self) -> Option<&Trace> {
        match self {
            Self::Legacy(t) => Some(t),
            Self::V1(_) => None,
        }
    }

    pub fn into_legacy(self) -> Option<Trace> {
        match self {
            Self::Legacy(t) => Some(t),
            Self::V1(_) => None,
        }
    }

    pub fn as_v1(&self) -> Option<&TraceDocument> {
        match self {
            Self::V1(d) => Some(d),
            Self::Legacy(_) => None,
        }
    }
}

pub(crate) fn verify() -> bool {
    static VERIFY: OnceLock<bool> = OnceLock::new();
    *VERIFY.get_or_init(|| std::env::var("ZEN_TRACE_VERIFY").is_ok_and(|v| v != "0"))
}

pub(crate) type GraphMap = HashMap<Arc<str>, DecisionGraphTrace>;

pub(crate) fn graph_v1_at(
    legacy: &Variable,
    request: &Variable,
    error: Option<(&str, &str)>,
    evaluation: Option<&Evaluation>,
) -> TraceDocument {
    let args = ConvertArgs {
        trace: LegacyTrace::Variable(legacy),
        request: Input::Variable(request),
        error: error.map(|(node, message)| NodeError {
            node,
            message: Some(message),
        }),
        engine_version: Some(crate::ENGINE_VERSION),
        documents: None,
        verify: verify(),
        evaluation,
    };
    match api::convert(args) {
        Ok(output) => TraceDocument::from_output(output),
        Err(error) => TraceDocument::failed(error.to_string()),
    }
}

pub(crate) fn restore(
    doc: &TraceDocument,
    request: &Variable,
    documents: Option<&str>,
) -> Option<Variable> {
    api::restore(RestoreArgs {
        trace: Input::Json(doc.json()),
        request: Input::Variable(request),
        documents: documents.map(Input::Json),
    })
    .ok()
}

pub(crate) fn policy_variable(trace: &Trace) -> Variable {
    Variable::from(serde_json::to_value(trace).unwrap_or_default())
}

fn plain(trace: &Trace) -> bool {
    trace
        .executions
        .iter()
        .all(|e| e.writes.is_empty() && e.reads.is_empty() && e.operand_values.is_empty())
}

fn policy_level(trace: Trace) -> collect::PolicyLevel {
    use crate::workspace::types::BlockTrace;
    use collect::{PolicyExecution, PolicyFact, PolicyLevel};
    let sorted = |map: HashMap<Arc<str>, Variable>| {
        let mut entries: Vec<(Arc<str>, Variable)> = map.into_iter().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        entries
    };
    let executions = trace
        .executions
        .into_iter()
        .map(|e| PolicyExecution {
            block: e.block_id,
            policy: e.policy_path,
            member: e.instance_path,
            fact: match e.trace {
                BlockTrace::Expression { property, value } => {
                    PolicyFact::Expression { property, value }
                }
                BlockTrace::Match {
                    matched_arm,
                    value,
                    arms,
                } => PolicyFact::Match {
                    arms: arms.into_iter().map(|a| (a.id, a.result)).collect(),
                    matched: matched_arm,
                    value,
                },
                BlockTrace::Assertion { result, conditions } => PolicyFact::Assertion {
                    conditions: conditions.into_iter().map(|c| (c.id, c.result)).collect(),
                    result,
                },
                BlockTrace::DecisionTable {
                    matched_rows,
                    matched_ids,
                    evaluations,
                    extras,
                } => PolicyFact::Table {
                    ids: matched_ids,
                    rows: matched_rows,
                    evaluations: evaluations.into_iter().map(sorted).collect(),
                    input_pass: extras.map(|x| x.input_pass),
                },
            },
            operands: Vec::new(),
            writes: Vec::new(),
            reads: Vec::new(),
        })
        .collect();
    PolicyLevel {
        engine_version: Some(trace.engine_version),
        executions,
        properties: Some(sorted(trace.properties)),
    }
}

fn policy_native(
    trace: Trace,
    request: &Variable,
    evaluation: Option<&Evaluation>,
) -> Result<TraceDocument, Trace> {
    if !plain(&trace) {
        return Err(trace);
    }
    let members = trace.executions.iter().any(|e| e.instance_path.is_some());
    if members && !PolicyDocument::accepts(request) {
        return Err(trace);
    }
    let legacy = verify().then(|| policy_variable(&trace));
    let fallback = (legacy.is_some() || members).then(|| trace.clone());
    let document = match PolicyDocument::new(policy_level(trace), request) {
        Ok(document) => TraceDocument::policy(document.with_evaluation(evaluation.cloned())),
        Err(miss) => {
            if std::env::var("ZEN_TRACE_NATIVE_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("native policy trace fell back: {miss}");
            }
            return match fallback {
                Some(trace) => Err(trace),
                None => Ok(TraceDocument::failed(miss.to_string())),
            };
        }
    };
    if let (Some(legacy), Some(fallback)) = (legacy, fallback) {
        let back = api::restore(RestoreArgs {
            trace: Input::Json(document.json()),
            request: Input::Variable(request),
            documents: None,
        })
        .ok();
        let same = back.is_some_and(|back| {
            serde_json::to_value(&back).ok() == serde_json::to_value(&legacy).ok()
        });
        if !same {
            return Err(fallback);
        }
    }
    Ok(document)
}

#[doc(hidden)]
pub fn policy_pair(
    trace: &Trace,
    request: &Variable,
    documents: &str,
) -> (Option<TraceDocument>, TraceDocument) {
    (
        policy_native(trace.clone(), request, None).ok(),
        policy_converted(trace, request, documents, None),
    )
}

pub(crate) fn policy_v1(
    trace: Trace,
    request: &Variable,
    documents: &str,
    evaluation: Option<&Evaluation>,
) -> TraceDocument {
    match policy_native(trace, request, evaluation) {
        Ok(doc) => doc,
        Err(trace) => policy_converted(&trace, request, documents, evaluation),
    }
}

fn policy_converted(
    trace: &Trace,
    request: &Variable,
    documents: &str,
    evaluation: Option<&Evaluation>,
) -> TraceDocument {
    let legacy = policy_variable(trace);
    let args = ConvertArgs {
        trace: LegacyTrace::Variable(&legacy),
        request: Input::Variable(request),
        error: None,
        engine_version: None,
        documents: Some(Input::Json(documents)),
        verify: verify(),
        evaluation,
    };
    match api::convert(args) {
        Ok(output) => TraceDocument::from_output(output),
        Err(error) => TraceDocument::failed(error.to_string()),
    }
}

pub(crate) type GraphResult = Result<DecisionGraphResponse, Box<EvaluationError>>;

pub(crate) struct Request(Variable);

impl Request {
    pub(crate) fn take(context: &Variable) -> Self {
        Request(context.deep_clone())
    }

    pub(crate) fn wanted(format: TraceFormat, context: &Variable) -> Option<Self> {
        (format != TraceFormat::Legacy).then(|| Self::take(context))
    }
}

#[doc(hidden)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    pub misses: usize,
    pub columnar: usize,
    pub rowwise: usize,
    pub planned: usize,
    pub batched: usize,
    pub runs: usize,
    pub reevaluated: std::collections::BTreeMap<&'static str, usize>,
}

impl Counters {
    pub fn add(&mut self, other: &Counters) {
        self.misses += other.misses;
        self.columnar += other.columnar;
        self.rowwise += other.rowwise;
        self.planned += other.planned;
        self.batched += other.batched;
        self.runs += other.runs;
        for (reason, rows) in &other.reevaluated {
            *self.reevaluated.entry(reason).or_default() += rows;
        }
    }
}

thread_local! {
    static COUNTERS: std::cell::RefCell<Counters> = std::cell::RefCell::new(Counters::default());
}

fn count(update: impl FnOnce(&mut Counters)) {
    COUNTERS.with_borrow_mut(update);
}

#[doc(hidden)]
pub fn counters() -> Counters {
    COUNTERS.with_borrow(Clone::clone)
}

pub(crate) fn count_run() {
    count(|c| c.runs += 1);
}

#[doc(hidden)]
pub fn graph_runs() -> usize {
    COUNTERS.with_borrow(|c| c.runs)
}

pub(crate) fn count_columnar(planned: bool, rows: usize) {
    count(|c| match planned {
        true => c.planned += rows,
        false => c.batched += rows,
    });
}

#[doc(hidden)]
pub fn columnar_traced() -> (usize, usize) {
    COUNTERS.with_borrow(|c| (c.planned, c.batched))
}

#[doc(hidden)]
pub fn batch_writes() -> (usize, usize) {
    COUNTERS.with_borrow(|c| (c.columnar, c.rowwise))
}

pub(crate) fn reevaluated(reason: &'static str, rows: usize) {
    count(|c| *c.reevaluated.entry(reason).or_default() += rows);
}

#[doc(hidden)]
pub fn reevaluations() -> Vec<(&'static str, usize)> {
    COUNTERS.with_borrow(|c| c.reevaluated.iter().map(|(k, v)| (*k, *v)).collect())
}

#[doc(hidden)]
pub fn native_misses() -> usize {
    COUNTERS.with_borrow(|c| c.misses)
}

pub(crate) fn written_at(
    level: &collect::Level,
    request: &Variable,
    error: Option<(&str, &str)>,
    evaluation: Option<&Evaluation>,
) -> TraceDocument {
    let args = v1::write::WriteArgs {
        level,
        request,
        error: error.map(|(node, message)| NodeError {
            node,
            message: Some(message),
        }),
        engine_version: Some(crate::ENGINE_VERSION),
        verify: verify(),
        evaluation,
    };
    match v1::write::write(args) {
        Ok(output) => TraceDocument::from_output(output),
        Err(miss) => {
            count(|c| c.misses += 1);
            if std::env::var("ZEN_TRACE_NATIVE_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("native trace fell back: {miss}");
            }
            graph_v1_at(&level.legacy(), request, error, evaluation)
        }
    }
}

fn cleaned(run: crate::compiled::traced::Run) -> crate::compiled::traced::Run {
    let crate::compiled::traced::Run {
        result,
        recording: mut level,
    } = run;
    for key in crate::decision_graph::cleaner::ZEN_RESERVED_PROPERTIES {
        level.strip(key);
    }
    let mut cleaner = crate::decision_graph::cleaner::VariableCleaner::new();
    if let Ok(result) = &result {
        cleaner.clean(result);
    }
    level.values(&mut |v| cleaner.clean(v));
    crate::compiled::traced::Run {
        result,
        recording: level,
    }
}

pub(crate) fn failure(
    result: &Result<Variable, Box<EvaluationError>>,
) -> Option<(Arc<str>, String)> {
    match result.as_ref().err()?.as_ref() {
        EvaluationError::NodeError {
            node_id, source, ..
        } => Some((node_id.clone(), source.to_string())),
        _ => None,
    }
}

pub(crate) fn pair(
    request: Request,
    run: crate::compiled::traced::Run,
    evaluation: Option<&Evaluation>,
) -> Option<(TraceDocument, TraceDocument)> {
    let run = cleaned(run);
    let failed = failure(&run.result);
    if run.result.is_err() && failed.is_none() {
        return None;
    }
    let error = failed.as_ref().map(|(n, m)| (&**n, m.as_str()));
    Some((
        written_at(&run.recording, &request.0, error, evaluation),
        graph_v1_at(&run.recording.legacy(), &request.0, error, evaluation),
    ))
}

pub(crate) fn collected(
    format: TraceFormat,
    request: Option<Request>,
    run: crate::compiled::traced::Run,
    start: std::time::Instant,
    evaluation: Option<&Evaluation>,
) -> GraphResult {
    let crate::compiled::traced::Run {
        result,
        recording: level,
    } = cleaned(run);
    let performance = format!("{:.1?}", start.elapsed());
    let request = request.map(|request| request.0).unwrap_or(Variable::Null);
    match result {
        Ok(result) => {
            let trace = match format {
                TraceFormat::Legacy => EvaluationTrace::Graph(level.graph_map()),
                _ => EvaluationTrace::V1(written_at(&level, &request, None, evaluation)),
            };
            Ok(DecisionGraphResponse {
                performance,
                result,
                trace: Some(trace),
            })
        }
        Err(error) => match *error {
            EvaluationError::NodeError {
                node_id, source, ..
            } => {
                let message = source.to_string();
                let trace = match format {
                    TraceFormat::Legacy => level.legacy(),
                    _ => written_at(&level, &request, Some((&node_id, &message)), evaluation)
                        .to_variable(),
                };
                Err(Box::new(EvaluationError::NodeError {
                    node_id,
                    trace: Some(trace),
                    source,
                }))
            }
            other => Err(Box::new(other)),
        },
    }
}

pub(crate) enum Requests<'a> {
    Owned(Vec<Variable>),
    Derived {
        data: std::rc::Rc<crate::compiled::graph::data::Data<'a>>,
        rows: Vec<std::cell::OnceCell<Variable>>,
    },
}

impl<'a> Requests<'a> {
    pub(crate) fn derived(data: std::rc::Rc<crate::compiled::graph::data::Data<'a>>) -> Self {
        let rows = (0..data.len()).map(|_| Default::default()).collect();
        Requests::Derived { data, rows }
    }

    pub(crate) fn get(&self, row: usize) -> Option<Variable> {
        match self {
            Requests::Owned(rows) => rows.get(row).cloned(),
            Requests::Derived { data, rows } => rows
                .get(row)
                .map(|cell| cell.get_or_init(|| data.materialize_row(row)).clone()),
        }
    }

    fn has(&self, row: usize) -> bool {
        match self {
            Requests::Owned(rows) => row < rows.len(),
            Requests::Derived { rows, .. } => row < rows.len(),
        }
    }
}

pub struct TraceBatch<'a> {
    hint: std::cell::Cell<usize>,
    traces: batch::Traces<'a>,
    requests: Requests<'a>,
    owned: Vec<Variable>,
    failures: Vec<Option<(Arc<str>, String)>>,
    format: TraceFormat,
    context: zen_expression::context::EvaluationContext,
}

impl<'a> TraceBatch<'a> {
    pub(crate) fn new(
        traces: batch::Traces<'a>,
        requests: Vec<Variable>,
        owned: Vec<Variable>,
        results: &[Result<Variable, Box<EvaluationError>>],
        format: TraceFormat,
        context: &zen_expression::context::EvaluationContext,
    ) -> Self {
        Self {
            hint: std::cell::Cell::new(256),
            traces,
            requests: Requests::Owned(requests),
            owned,
            failures: results.iter().map(failure).collect(),
            format,
            context: context.clone(),
        }
    }

    pub(crate) fn with_failures(
        traces: batch::Traces<'a>,
        requests: Requests<'a>,
        owned: Vec<Variable>,
        failures: Vec<Option<(Arc<str>, String)>>,
        format: TraceFormat,
        context: &zen_expression::context::EvaluationContext,
    ) -> Self {
        Self {
            hint: std::cell::Cell::new(256),
            traces,
            requests,
            owned,
            failures,
            format,
            context: context.clone(),
        }
    }

    fn evaluation(&self, row: usize) -> Option<Evaluation> {
        Some(crate::compiled::pinning::row_evaluation(&self.context, row))
    }

    fn columnar(&self, row: usize, out: &mut String) -> bool {
        let (true, Some(None)) = (self.requests.has(row), self.failures.get(row)) else {
            return false;
        };
        let traces = &self.traces;
        let mark = out.len();
        let make = || self.requests.get(row).unwrap_or(Variable::Null);
        let asked = batch::Asked::new(&make);
        let written = traces.write_row(
            row,
            &asked,
            self.owned.get(row),
            Some(crate::ENGINE_VERSION),
            self.evaluation(row).as_ref(),
            out,
        );
        if let Err(reason) = written {
            if std::env::var("ZEN_TRACE_COLUMNAR_DEBUG").is_ok_and(|v| v != "0") {
                eprintln!("columnar fallback: {reason}");
            }
            out.truncate(mark);
            count(|c| c.rowwise += 1);
            return false;
        }
        if verify() {
            let request = asked.get();
            let fresh = TraceDocument::new(out[mark..].to_string(), None);
            let legacy = traces.level(row).legacy();
            let back = restore(&fresh, request, None);
            let want = graph_v1_at(&legacy, request, None, None);
            let want = restore(&want, request, None);
            let same = match (back, want) {
                (Some(a), Some(b)) => {
                    serde_json::to_value(&a).ok() == serde_json::to_value(&b).ok()
                }
                _ => false,
            };
            if !same {
                if std::env::var("ZEN_TRACE_NATIVE_DEBUG").is_ok_and(|v| v != "0") {
                    eprintln!("columnar trace row {row} differs: {}", &out[mark..]);
                }
                out.truncate(mark);
                count(|c| c.rowwise += 1);
                return false;
            }
        }
        count(|c| c.columnar += 1);
        true
    }

    pub fn len(&self) -> usize {
        self.traces.rows()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn format(&self) -> TraceFormat {
        self.format
    }

    fn level(&self, row: usize) -> Option<collect::Level> {
        self.cleaned_level(self.traces.level(row))
    }

    fn cleaned_level(&self, mut level: collect::Level) -> Option<collect::Level> {
        for key in crate::decision_graph::cleaner::ZEN_RESERVED_PROPERTIES {
            level.strip(key);
        }
        let mut cleaner = crate::decision_graph::cleaner::VariableCleaner::new();
        level.values_mut(&mut |v| {
            if rows::reserved(v) {
                *v = v.deep_clone();
                cleaner.clean(v);
            }
        });
        Some(level)
    }

    pub fn document(&self, row: usize) -> Option<TraceDocument> {
        let mut out = String::with_capacity(self.hint.get());
        if self.columnar(row, &mut out) {
            self.hint.set(out.len() + out.len() / 8);
            return Some(TraceDocument::new(out, None));
        }
        self.written(row)
    }

    fn written(&self, row: usize) -> Option<TraceDocument> {
        let request = &self.requests.get(row)?;
        let failure = self.failures.get(row)?;
        let error = failure.as_ref().map(|(n, m)| (&**n, m.as_str()));
        Some(written_at(
            &self.level(row)?,
            request,
            error,
            self.evaluation(row).as_ref(),
        ))
    }

    pub fn trace(&self, row: usize) -> Option<EvaluationTrace> {
        if row >= self.len() {
            return None;
        }
        Some(match self.format {
            TraceFormat::Legacy => EvaluationTrace::Graph(self.level(row)?.graph_map()),
            _ => EvaluationTrace::V1(self.document(row)?),
        })
    }

    pub fn json(&self, row: usize) -> Option<String> {
        self.document(row).map(|doc| doc.json().to_string())
    }

    pub fn write_all(&self, out: &mut String) -> Vec<usize> {
        let mut ends = Vec::with_capacity(self.len());
        let start = out.len();
        for row in 0..self.len() {
            if !self.columnar(row, out) {
                match self.written(row) {
                    Some(doc) => out.push_str(doc.json()),
                    None => out.push_str("null"),
                }
            }
            if row == 0 {
                let first = out.len() - start;
                out.reserve((first + first / 16 + 16).saturating_mul(self.len() - 1));
            }
            ends.push(out.len());
            out.push('\n');
        }
        ends
    }

    pub(crate) fn responses(
        &self,
        results: Vec<Result<Variable, Box<EvaluationError>>>,
        performance: &str,
    ) -> Vec<GraphResult> {
        results
            .into_iter()
            .enumerate()
            .map(|(row, result)| match result {
                Ok(result) => Ok(DecisionGraphResponse {
                    performance: performance.to_string(),
                    result,
                    trace: self.trace(row),
                }),
                Err(error) => match *error {
                    EvaluationError::NodeError {
                        node_id, source, ..
                    } => {
                        let trace = self.trace(row).map(|t| match t {
                            EvaluationTrace::Graph(map) => map.to_variable(),
                            other => other.to_variable(),
                        });
                        Err(Box::new(EvaluationError::NodeError {
                            node_id,
                            trace,
                            source,
                        }))
                    }
                    other => Err(Box::new(other)),
                },
            })
            .collect()
    }
}

impl crate::workspace::types::EvaluateRequest<TraceFormat> {
    pub(crate) fn legacy(&self) -> crate::workspace::types::EvaluateRequest {
        crate::workspace::types::EvaluateRequest {
            policy_path: self.policy_path.clone(),
            input: self.input.clone(),
            goals: self.goals.clone(),
            trace: self.trace.is_on(),
        }
    }
}

impl From<crate::workspace::types::EvaluationResult> for crate::workspace::EvaluationResult {
    fn from(result: crate::workspace::types::EvaluationResult) -> Self {
        Self {
            output: result.output,
            duration: result.duration,
            trace: result.trace.map(PolicyTrace::Legacy),
        }
    }
}

impl From<crate::workspace::types::EvaluationError> for crate::workspace::EvaluationError {
    fn from(error: crate::workspace::types::EvaluationError) -> Self {
        use crate::workspace::types::EvaluationError as Legacy;
        match error {
            Legacy::PolicyNotFound(path) => Self::PolicyNotFound(path),
            Legacy::GraphNotEvaluable(path) => Self::GraphNotEvaluable(path),
            Legacy::ImportNotFound {
                policy_path,
                import,
            } => Self::ImportNotFound {
                policy_path,
                import,
            },
            Legacy::CompilationErrors { policy_path } => Self::CompilationErrors { policy_path },
            Legacy::GoalNotFound(goal) => Self::GoalNotFound(goal),
            Legacy::MissingRequiredInputs { goals, missing } => {
                Self::MissingRequiredInputs { goals, missing }
            }
            Legacy::InputValidationFailed { errors } => Self::InputValidationFailed { errors },
            Legacy::ExpressionFailed {
                policy_path,
                block_id,
                expression,
                source,
                partial_trace,
            } => Self::ExpressionFailed {
                policy_path,
                block_id,
                expression,
                source,
                partial_trace: partial_trace.map(|trace| Box::new(PolicyTrace::Legacy(*trace))),
            },
        }
    }
}

impl From<crate::workspace::EvaluationResult> for DecisionGraphResponse {
    fn from(result: crate::workspace::EvaluationResult) -> Self {
        Self {
            performance: format!("{:.1?}", result.duration),
            result: result.output,
            trace: result.trace.map(|trace| match trace {
                PolicyTrace::V1(document) => EvaluationTrace::V1(document),
                legacy => EvaluationTrace::Policy(legacy),
            }),
        }
    }
}

impl EvaluationTrace {
    pub fn as_policy(&self) -> Option<&Trace> {
        match self {
            Self::Policy(trace) => trace.as_legacy(),
            _ => None,
        }
    }

    pub fn as_v1(&self) -> Option<&TraceDocument> {
        match self {
            Self::V1(document) => Some(document),
            Self::Policy(trace) => trace.as_v1(),
            Self::Graph(_) => None,
        }
    }

    pub fn to_variable(&self) -> Variable {
        match self {
            Self::Graph(map) => map.to_variable(),
            Self::Policy(PolicyTrace::Legacy(trace)) => policy_variable(trace),
            Self::Policy(PolicyTrace::V1(document)) | Self::V1(document) => document.to_variable(),
        }
    }
}

impl DecisionGraphResponse {
    pub(crate) fn serialized(
        response: Result<Self, Box<EvaluationError>>,
    ) -> Result<serde_json::Value, serde_json::Value> {
        match response {
            Ok(ok) => Ok(serde_json::to_value(&ok).unwrap_or_default()),
            Err(err) => Err(serde_json::to_value(&*err).unwrap_or_default()),
        }
    }
}

impl From<crate::workspace::types::EvaluationResult> for DecisionGraphResponse {
    fn from(result: crate::workspace::types::EvaluationResult) -> Self {
        crate::workspace::EvaluationResult::from(result).into()
    }
}
