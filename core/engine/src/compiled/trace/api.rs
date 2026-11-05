use super::core::{self, check_doc_tags, open_envelope};
use super::value::{self, eq, Val};
use super::{collect, convert, graph, marks, native, policy};
use zen_types::variable::Variable;

pub use super::{PolicyTrace, TraceBatch, TraceDocument, TraceFormat};

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("{0}")]
    Invalid(String),
    #[error("this policy trace needs its documents to restore reads")]
    DocumentsRequired,
    #[error("{0}")]
    Json(String),
    #[error("malformed trace: {0}")]
    Malformed(String),
}

pub(crate) type R<T> = Result<T, TraceError>;

pub(crate) fn bad(msg: impl Into<String>) -> TraceError {
    TraceError::Invalid(msg.into())
}

pub(crate) fn malformed(what: impl Into<String>) -> TraceError {
    TraceError::Malformed(what.into())
}

pub(crate) trait Need<T> {
    fn need(self, what: &str) -> R<T>;
}

impl<T> Need<T> for Option<T> {
    #[inline]
    fn need(self, what: &str) -> R<T> {
        match self {
            Some(v) => Ok(v),
            None => Err(malformed(what)),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Input<'a> {
    Variable(&'a Variable),
    Json(&'a str),
}

#[derive(Clone, Copy)]
pub enum LegacyTrace<'a> {
    Variable(&'a Variable),
    Json(&'a str),
    Reference(&'a Variable),
    ReferenceString(&'a str),
}

#[derive(Clone, Copy)]
pub struct NodeError<'a> {
    pub node: &'a str,
    pub message: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evaluation {
    pub evaluated_at: Option<std::sync::Arc<str>>,
    pub timezone: Option<std::sync::Arc<str>>,
    pub seed: Option<u64>,
}

impl Evaluation {
    pub fn write(&self, out: &mut String) {
        if let Some(at) = &self.evaluated_at {
            out.push_str(",\"evaluatedAt\":");
            value::write_str(at, out);
        }
        if let Some(zone) = &self.timezone {
            out.push_str(",\"timezone\":");
            value::write_str(zone, out);
        }
        if let Some(seed) = self.seed {
            out.push_str(",\"seed\":");
            out.push_str(itoa::Buffer::new().format(seed));
        }
    }

    pub(crate) fn stamp(&self, doc: Val) -> Val {
        let Val::Obj(o) = &doc else { return doc };
        let mut out = value::Obj::with_capacity(o.len() + 3);
        let mut placed = false;
        let place = |out: &mut value::Obj| {
            if let Some(at) = &self.evaluated_at {
                out.insert(std::rc::Rc::from("evaluatedAt"), Val::str(at));
            }
            if let Some(zone) = &self.timezone {
                out.insert(std::rc::Rc::from("timezone"), Val::str(zone));
            }
            if let Some(seed) = self.seed {
                out.insert(
                    std::rc::Rc::from("seed"),
                    Val::Num(rust_decimal::Decimal::from(seed)),
                );
            }
        };
        for (k, v) in o.iter() {
            if matches!(&**k, "evaluatedAt" | "timezone" | "seed") {
                continue;
            }
            if !placed && !matches!(&**k, "version" | "kind" | "engine") {
                place(&mut out);
                placed = true;
            }
            out.insert(k.clone(), v.clone());
        }
        if !placed {
            place(&mut out);
        }
        Val::obj(out)
    }

    pub fn stamp_json(&self, json: &str) -> Option<String> {
        if self.evaluated_at.is_none() && self.timezone.is_none() && self.seed.is_none() {
            return Some(json.to_string());
        }
        let doc = value::parse(json).ok()?;
        doc.as_obj()?;
        Some(self.stamp(doc).to_json())
    }
}

pub struct ConvertArgs<'a> {
    pub trace: LegacyTrace<'a>,
    pub request: Input<'a>,
    pub error: Option<NodeError<'a>>,
    pub engine_version: Option<&'a str>,
    pub documents: Option<Input<'a>>,
    pub verify: bool,
    pub evaluation: Option<&'a Evaluation>,
}

impl<'a> ConvertArgs<'a> {
    pub fn new(trace: LegacyTrace<'a>, request: Input<'a>) -> Self {
        Self {
            trace,
            request,
            error: None,
            engine_version: None,
            documents: None,
            verify: true,
            evaluation: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Converted,
    Verbatim { reason: String },
}

#[derive(Debug, Clone)]
pub struct ConvertOutput {
    pub json: String,
    pub status: Status,
}

impl ConvertOutput {
    pub fn document(&self) -> Variable {
        value::parse(&self.json)
            .map(|v| convert::to_variable(&v))
            .unwrap_or(Variable::Null)
    }
}

pub fn parse_document(json: &str) -> Result<Variable, TraceError> {
    value::parse(json)
        .map(|v| convert::to_variable(&v))
        .map_err(|e| TraceError::Json(e.to_string()))
}

pub struct RestoreArgs<'a> {
    pub trace: Input<'a>,
    pub request: Input<'a>,
    pub documents: Option<Input<'a>>,
}

fn input(i: Input) -> R<Val> {
    match i {
        Input::Variable(v) => Ok(convert::from_variable(v)),
        Input::Json(s) => value::parse(s).map_err(|e| TraceError::Json(e.to_string())),
    }
}

fn legacy(t: LegacyTrace) -> R<Val> {
    match t {
        LegacyTrace::Variable(v) => Ok(convert::from_variable(v)),
        LegacyTrace::Json(s) => value::parse(s).map_err(|e| TraceError::Json(e.to_string())),
        LegacyTrace::Reference(v) => convert::from_reference(v),
        LegacyTrace::ReferenceString(s) => convert::from_reference_string(s),
    }
}

pub(crate) struct WriteArgs<'a> {
    pub level: &'a collect::Level,
    pub request: &'a Variable,
    pub error: Option<NodeError<'a>>,
    pub engine_version: Option<&'a str>,
    pub verify: bool,
    pub evaluation: Option<&'a Evaluation>,
}

pub(crate) fn write(args: WriteArgs) -> Result<ConvertOutput, &'static str> {
    let error = error_info(args.error);
    let engine = args.engine_version.map(Val::str);
    let _sizes = super::core::SizeMemo::start();
    let doc = native::encode_doc(
        args.level,
        args.request,
        error.as_ref(),
        engine.as_ref(),
        args.evaluation,
    )?;
    let (json, tree) = match doc {
        native::Doc::Text(t) => (t, None),
        native::Doc::Tree(v) => (v.to_json(), Some(v)),
    };
    if !args.verify {
        return Ok(ConvertOutput {
            json,
            status: Status::Converted,
        });
    }
    let tree = match tree {
        Some(v) => v,
        None => value::parse(&json).map_err(|_| "native document does not parse")?,
    };
    let mut shared = convert::Shared::default();
    let request = shared.val(args.request);
    let out = settle(
        Ok(tree),
        &mut || shared.val(&args.level.legacy()),
        &request,
        false,
        engine.as_ref(),
        None,
        true,
    );
    let doc = match (&out.status, args.evaluation) {
        (Status::Verbatim { .. }, Some(e)) => e.stamp(out.doc),
        _ => out.doc,
    };
    Ok(ConvertOutput {
        json: doc.to_json(),
        status: out.status,
    })
}

fn error_info(e: Option<NodeError>) -> Option<graph::ErrorInfo> {
    e.map(|e| graph::ErrorInfo {
        node: e.node.to_string(),
        message: e.message.map(|m| m.to_string()),
    })
}

pub(crate) struct Encoded {
    pub doc: Val,
    pub status: Status,
}

pub(crate) fn encode_val(
    trace: &Val,
    request: &Val,
    error: Option<&graph::ErrorInfo>,
    engine: Option<&Val>,
    documents: Option<&Val>,
    verify: bool,
) -> Encoded {
    let _sizes = super::core::SizeMemo::start();
    let is_policy = policy::is_policy(trace);
    let encoded = if is_policy {
        policy::encode_policy(trace, request, documents)
    } else {
        graph::encode_graph(trace, request, error, engine)
    };
    settle(
        encoded,
        &mut || trace.clone(),
        request,
        is_policy,
        engine,
        documents,
        verify,
    )
}

fn settle(
    encoded: R<Val>,
    trace: &mut dyn FnMut() -> Val,
    request: &Val,
    is_policy: bool,
    engine: Option<&Val>,
    documents: Option<&Val>,
    verify: bool,
) -> Encoded {
    let fallback = |reason: String, trace: &Val| Encoded {
        doc: if is_policy {
            policy::policy_fallback(trace)
        } else {
            graph::graph_fallback(trace, engine)
        },
        status: Status::Verbatim { reason },
    };
    let doc = match encoded {
        Ok(d) => d,
        Err(e) => return fallback(format!("encode failed: {e}"), &trace()),
    };
    if !verify {
        return Encoded {
            doc,
            status: Status::Converted,
        };
    }
    let trace = trace();
    let text = doc.to_json();
    let back = value::parse(&text)
        .map_err(|e| TraceError::Json(e.to_string()))
        .and_then(|d| decode_val(&d, request, if is_policy { documents } else { None }));
    match back {
        Ok(b) if eq(&b, &trace) => Encoded {
            doc,
            status: Status::Converted,
        },
        Ok(b) => fallback(
            format!(
                "restore differs at {}",
                first_diff(&trace, &b, "").unwrap_or_default()
            ),
            &trace,
        ),
        Err(e) => fallback(format!("restore failed: {e}"), &trace),
    }
}

pub(crate) fn decode_val(doc: &Val, request: &Val, documents: Option<&Val>) -> R<Val> {
    if matches!(doc.get("requires"), Some(Val::Arr(a)) if a.iter().any(|f| f.as_str() == Some("derived")))
    {
        return Err(bad(
            "draft documents (requires derived) read the release and are not supported",
        ));
    }
    let marks = check_doc_tags(doc)?;
    marks::envelope(doc)?;
    let env = open_envelope(&core::unpack_doc(doc)?)?;
    let (redacted, truncated) = (env.has(core::REDACTED), env.has(core::TRUNCATED));
    if marks.redacted && !redacted {
        return Err(bad("zen-trace v1: $redacted needs requires redacted"));
    }
    if marks.omitted && !truncated {
        return Err(bad("zen-trace v1: $omitted needs requires truncated"));
    }
    let _lenient = core::Lenient::enter(redacted || truncated);
    let env = match redacted || truncated {
        true => {
            let body = marks::settle(&Val::Obj(std::rc::Rc::new(env.body.clone())), &env.kind)?;
            core::Envelope {
                body: body.as_obj().cloned().unwrap_or_default(),
                ..env
            }
        }
        false => env,
    };
    if env.kind == "policy" {
        let derive = policy::needs_documents(&env.requires);
        return policy::decode_policy(&env.body, request, documents, env.engine.as_ref(), derive);
    }
    graph::decode_graph(&env.body, request, env.has(core::RESULT_ROWS))
}

fn first_diff(a: &Val, b: &Val, at: &str) -> Option<String> {
    match (a, b) {
        (Val::Obj(x), Val::Obj(y)) => {
            for (k, v) in x.iter() {
                match y.get(k) {
                    None => return Some(format!("{at}/{k} (missing)")),
                    Some(w) => {
                        if let Some(d) = first_diff(v, w, &format!("{at}/{k}")) {
                            return Some(d);
                        }
                    }
                }
            }
            y.keys()
                .find(|k| !x.has(k))
                .map(|k| format!("{at}/{k} (unexpected)"))
        }
        (Val::Arr(x), Val::Arr(y)) => {
            if x.len() != y.len() {
                return Some(format!("{at} (length {} vs {})", x.len(), y.len()));
            }
            x.iter()
                .zip(y.iter())
                .enumerate()
                .find_map(|(i, (p, q))| first_diff(p, q, &format!("{at}/{i}")))
        }
        _ if eq(a, b) => None,
        _ => Some(at.to_string()),
    }
}

pub fn convert(args: ConvertArgs) -> Result<ConvertOutput, TraceError> {
    let (trace, request) = match (args.trace, args.request) {
        (LegacyTrace::Variable(t), Input::Variable(r)) => {
            let policy = match t {
                Variable::Object(o) => {
                    matches!(o.borrow().get_str("executions"), Some(Variable::Array(_)))
                }
                _ => false,
            };
            match policy {
                true => (convert::from_variable(t), convert::from_variable(r)),
                false => {
                    let mut shared = convert::Shared::default();
                    let trace = shared.val(t);
                    (trace, shared.val(r))
                }
            }
        }
        (t, r) => (legacy(t)?, input(r)?),
    };
    let documents = args.documents.map(cached_documents).transpose()?;
    let error = args.error.map(|e| graph::ErrorInfo {
        node: e.node.to_string(),
        message: e.message.map(|m| m.to_string()),
    });
    let engine = args.engine_version.map(Val::str);
    let out = encode_val(
        &trace,
        &request,
        error.as_ref(),
        engine.as_ref(),
        documents.as_ref(),
        args.verify,
    );
    let doc = match args.evaluation {
        Some(e) => e.stamp(out.doc),
        None => out.doc,
    };
    Ok(ConvertOutput {
        json: doc.to_json(),
        status: out.status,
    })
}

fn cached_documents(i: Input) -> R<Val> {
    let Input::Json(text) = i else {
        return input(i);
    };
    thread_local! {
        static LAST: std::cell::RefCell<Option<(Box<str>, Val)>> = const { std::cell::RefCell::new(None) };
    }
    if let Some(v) = LAST.with(|l| {
        l.borrow()
            .as_ref()
            .filter(|(t, _)| **t == *text)
            .map(|(_, v)| v.clone())
    }) {
        return Ok(v);
    }
    let v = input(i)?;
    LAST.with(|l| *l.borrow_mut() = Some((Box::from(text), v.clone())));
    Ok(v)
}

fn restore_val(args: RestoreArgs) -> R<Val> {
    let doc = input(args.trace)?;
    let request = input(args.request)?;
    let documents = args.documents.map(input).transpose()?;
    decode_val(&doc, &request, documents.as_ref())
}

pub fn restore(args: RestoreArgs) -> Result<Variable, TraceError> {
    restore_val(args).map(|v| convert::to_variable(&v))
}

pub fn restore_json(args: RestoreArgs) -> Result<String, TraceError> {
    restore_val(args).map(|v| v.to_json())
}

#[doc(hidden)]
pub mod testing {
    pub use crate::compiled::trace::value::{eq, json_len, parse, Obj, Val};
    pub use crate::compiled::trace::{
        batch_writes, columnar_traced, graph_runs, native_misses, policy_pair, reevaluations,
    };

    pub fn repack(doc: &str) -> Option<String> {
        let v = parse(doc).ok()?;
        let plain = crate::compiled::trace::core::unpack(&v).ok()?;
        Some(crate::compiled::trace::core::pack(&plain).to_json())
    }

    pub fn unpacked(doc: &str) -> Option<Val> {
        let v = parse(doc).ok()?;
        crate::compiled::trace::core::unpack_doc(&v).ok()
    }

    pub fn first_diff(a: &Val, b: &Val) -> Option<String> {
        super::first_diff(a, b, "")
    }

    pub fn convert_val(
        trace: &Val,
        request: &Val,
        error: Option<(&str, Option<&str>)>,
        engine: Option<&str>,
        documents: Option<&Val>,
    ) -> (String, super::Status) {
        convert_val_with(trace, request, error, engine, documents, true)
    }

    pub fn convert_val_with(
        trace: &Val,
        request: &Val,
        error: Option<(&str, Option<&str>)>,
        engine: Option<&str>,
        documents: Option<&Val>,
        verify: bool,
    ) -> (String, super::Status) {
        let err = error.map(|(n, m)| crate::compiled::trace::graph::ErrorInfo {
            node: n.to_string(),
            message: m.map(|x| x.to_string()),
        });
        let engine = engine.map(Val::str);
        let out = super::encode_val(
            trace,
            request,
            err.as_ref(),
            engine.as_ref(),
            documents,
            verify,
        );
        (out.doc.to_json(), out.status)
    }

    pub fn restore_val(
        doc: &str,
        request: &Val,
        documents: Option<&Val>,
    ) -> Result<Val, super::TraceError> {
        let d = parse(doc).map_err(|e| super::TraceError::Json(e.to_string()))?;
        super::decode_val(&d, request, documents)
    }
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub kind: String,
    pub engine: Option<String>,
    pub requires: Vec<String>,
    pub evaluated_at: Option<String>,
    pub timezone: Option<String>,
    pub seed: Option<u64>,
    pub context: Option<Variable>,
    pub truncated: bool,
}

pub fn inspect(json: &str) -> Result<Inspection, TraceError> {
    let doc = value::parse(json).map_err(|e| TraceError::Json(e.to_string()))?;
    check_doc_tags(&doc)?;
    let env = open_envelope(&doc)?;
    let text = |v: &Option<Val>, name: &str| -> R<Option<String>> {
        match v {
            None => Ok(None),
            Some(Val::Str(s)) => Ok(Some(s.to_string())),
            Some(_) => Err(bad(format!("zen-trace v1: {name} must be a string"))),
        }
    };
    let seed = match &env.seed {
        None => None,
        Some(Val::Num(n)) => Some(
            rust_decimal::prelude::ToPrimitive::to_u64(n)
                .filter(|_| n.fract().is_zero())
                .ok_or_else(|| bad("zen-trace v1: seed must be an integer from 0 to 2^64 - 1"))?,
        ),
        Some(_) => return Err(bad("zen-trace v1: seed must be an integer")),
    };
    Ok(Inspection {
        evaluated_at: text(&env.evaluated_at, "evaluatedAt")?,
        timezone: text(&env.timezone, "timezone")?,
        engine: text(&env.engine, "engine")?,
        context: env.context.as_ref().map(convert::to_variable),
        seed,
        truncated: env.truncated,
        requires: env.requires,
        kind: env.kind,
    })
}

pub fn redact(json: &str, pointers: &[&str], reason: Option<&str>) -> Result<String, TraceError> {
    let doc = value::parse(json).map_err(|e| TraceError::Json(e.to_string()))?;
    let found = check_doc_tags(&doc)?;
    marks::envelope(&doc)?;
    let plain = core::unpack_doc(&doc)?;
    let env = open_envelope(&plain)?;
    if found.omitted && !env.has(core::TRUNCATED) {
        return Err(bad("zen-trace v1: $omitted needs requires truncated"));
    }
    if found.redacted || found.omitted {
        marks::settle(&Val::Obj(std::rc::Rc::new(env.body.clone())), &env.kind)?;
    }
    let root = marks::root(&env.kind);
    let mut marker = value::Obj::with_capacity(2);
    marker.insert(std::rc::Rc::from(core::REDACTED_TAG), Val::Bool(true));
    if let Some(r) = reason {
        marker.insert(std::rc::Rc::from("reason"), Val::str(r));
    }
    let marker = Val::obj(marker);
    let mut out = plain;
    for p in pointers {
        let segs = core::from_ptr(p)?;
        let Some(first) = segs.first() else {
            return Err(bad("redact: the whole document cannot be redacted"));
        };
        if core::ENVELOPE_KEYS.contains(&&*first.text()) {
            return Err(bad(format!("redact: {p} points into the envelope")));
        }
        let mut at = &out;
        let mut pos = root;
        for s in &segs {
            if core::is_literal(at) {
                return Err(bad(format!("redact: {p} points inside $literal")));
            }
            if !marks::keyed(pos) && marks::is_marker(at) {
                return Err(bad(format!("redact: {p} points inside a marker")));
            }
            pos = marks::child(pos, at, &s.text());
            at = core::child(at, s).ok_or_else(|| bad(format!("redact: {p} does not resolve")))?;
        }
        if pos != marks::Pos::Value {
            return Err(bad(format!(
                "redact: {p} is not a value position; only user data (node inputs and outputs, results, rule cells and references, logs, details, step values, writes, stored operands, verbatim traces) can be redacted"
            )));
        }
        out = core::replace_at(&out, &segs, marker.clone())?;
    }
    let Val::Obj(o) = &out else {
        return Err(bad("not a zen-trace v1 document"));
    };
    let mut o = (**o).clone();
    let mut requires: Vec<Val> = match o.get("requires") {
        Some(Val::Arr(a)) => a.to_vec(),
        _ => Vec::new(),
    };
    if !requires.iter().any(|f| f.as_str() == Some(core::REDACTED)) {
        requires.push(Val::str(core::REDACTED));
    }
    o.set("requires", Val::arr(requires));
    let mut body = o.clone();
    body.remove("context");
    let Val::Obj(packed) = core::pack(&Val::obj(body)) else {
        return Err(bad("not a zen-trace v1 document"));
    };
    let mut out = value::Obj::with_capacity(o.len());
    for (k, v) in o.iter() {
        let v = match &**k {
            "context" => v.clone(),
            _ => packed.get(k).cloned().unwrap_or(Val::Null),
        };
        out.insert(k.clone(), v);
    }
    Ok(Val::obj(out).to_json())
}
