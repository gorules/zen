pub use super::migrate::api::{convert, inspect, redact, restore, restore_json, testing};
use super::value::{self, Val};
pub use super::{PolicyTrace, TraceBatch, TraceDocument, TraceFormat};
use zen_types::variable::Variable;

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
            .map(|v| value::to_variable(&v))
            .unwrap_or(Variable::Null)
    }
}

pub fn parse_document(json: &str) -> Result<Variable, TraceError> {
    value::parse(json)
        .map(|v| value::to_variable(&v))
        .map_err(|e| TraceError::Json(e.to_string()))
}

pub struct RestoreArgs<'a> {
    pub trace: Input<'a>,
    pub request: Input<'a>,
    pub documents: Option<Input<'a>>,
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
