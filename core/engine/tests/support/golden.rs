use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zen_engine::policy::{EvaluateRequest, EvaluationError as PolicyError, EvaluationResult, PolicyWorkspace};
use zen_engine::{Decision, DecisionGraphResponse, EvaluationError, EvaluationTraceKind};
use zen_expression::variable::{RcValue, VariableMap};
use zen_expression::Variable;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Ok(Value),
    Err { debug: String, value: Value },
}

impl Outcome {
    pub fn missing(key: &str) -> Outcome {
        Outcome::Err {
            debug: format!("missing golden entry {key}"),
            value: Value::Null,
        }
    }

    pub fn graph(result: &Result<DecisionGraphResponse, Box<EvaluationError>>) -> Outcome {
        match result {
            Ok(response) => Outcome::Ok(Canon::variable(&response.result)),
            Err(error) => Outcome::Err {
                debug: error.to_string(),
                value: Canon::value(serde_json::to_value(&**error).unwrap_or(Value::Null)),
            },
        }
    }

    pub fn graph_traced(result: &Result<DecisionGraphResponse, Box<EvaluationError>>, mode: EvaluationTraceKind) -> Outcome {
        match result {
            Ok(response) => Outcome::Ok(Canon::redacted(response.serialize_with_mode(serde_json::value::Serializer, mode).unwrap_or(Value::Null))),
            Err(error) => Outcome::Err {
                debug: error.to_string(),
                value: Canon::redacted(error.serialize_with_mode(serde_json::value::Serializer, mode).unwrap_or(Value::Null)),
            },
        }
    }

    pub fn serialized(result: Result<Value, Value>) -> Outcome {
        match result {
            Ok(value) => Outcome::Ok(Canon::redacted(value)),
            Err(value) => Outcome::Err {
                debug: String::new(),
                value: Canon::redacted(value),
            },
        }
    }

    pub fn policy(result: Result<EvaluationResult, PolicyError>) -> Outcome {
        match result {
            Ok(result) => Outcome::Ok(Canon::variable(&result.output)),
            Err(error) => {
                let error = Self::ordered(error);
                Outcome::Err {
                    debug: format!("{error:?}"),
                    value: Canon::unordered(serde_json::to_value(&EvaluationError::Policy(error)).unwrap_or(Value::Null), false),
                }
            }
        }
    }

    pub fn policy_traced(result: Result<EvaluationResult, PolicyError>) -> Outcome {
        match result {
            Ok(result) => Outcome::Ok(Canon::unordered(serde_json::to_value(&result).unwrap_or(Value::Null), true)),
            Err(error) => {
                let error = Self::ordered(error);
                Outcome::Err {
                    debug: error.to_string(),
                    value: Canon::unordered(serde_json::to_value(&EvaluationError::Policy(error)).unwrap_or(Value::Null), true),
                }
            }
        }
    }

    pub fn debug(error: &PolicyError) -> String {
        match error {
            PolicyError::InputValidationFailed { errors } => format!("{:?}", Self::ordered(PolicyError::InputValidationFailed { errors: errors.clone() })),
            other => format!("{other:?}"),
        }
    }

    pub fn ordered(error: PolicyError) -> PolicyError {
        match error {
            PolicyError::InputValidationFailed { mut errors } => {
                errors.sort_by(|a, b| (&a.path, &a.expected, &a.got).cmp(&(&b.path, &b.expected, &b.got)));
                PolicyError::InputValidationFailed { errors }
            }
            other => other,
        }
    }

    pub fn value(&self) -> &Value {
        match self {
            Outcome::Ok(value) => value,
            Outcome::Err { value, .. } => value,
        }
    }

    pub fn normalized(&self, normalize: impl Fn(Value) -> Value) -> Outcome {
        match self {
            Outcome::Ok(value) => Outcome::Ok(Canon::value(normalize(value.clone()))),
            other => other.clone(),
        }
    }

    pub fn without_trace(&self) -> Outcome {
        match self {
            Outcome::Err { debug, value } => Outcome::Err {
                debug: debug.clone(),
                value: match value {
                    Value::Object(map) => Value::Object(map.iter().filter(|(k, _)| k.as_str() != "trace").map(|(k, v)| (k.clone(), v.clone())).collect()),
                    other => other.clone(),
                },
            },
            other => other.clone(),
        }
    }

    pub fn show(&self) -> String {
        match self {
            Outcome::Ok(value) => value.to_string(),
            Outcome::Err { debug, value } => format!("!error {debug} {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub key: String,
    pub suite: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<String>,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub plain: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traced: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhanced: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ordered: bool,
}

impl Entry {
    pub fn new(suite: &str, goals: &[String], input: &Variable, plain: Outcome) -> Entry {
        Entry {
            key: Canon::key(suite, goals, input),
            suite: suite.to_string(),
            goals: goals.to_vec(),
            input: Canon::variable(input),
            fingerprint: None,
            plain,
            traced: None,
            enhanced: None,
            reference: None,
            ordered: false,
        }
    }

    pub fn fingerprint(mut self, fingerprint: Option<String>) -> Entry {
        self.fingerprint = fingerprint;
        self
    }

    pub fn traced(mut self, traced: Outcome, reference: Option<Value>) -> Entry {
        self.traced = Some(traced);
        self.reference = reference;
        self
    }

    pub fn enhanced(mut self, enhanced: Outcome) -> Entry {
        self.enhanced = Some(enhanced);
        self
    }

    pub fn outcome(&self, mode: Mode) -> Option<&Outcome> {
        match mode {
            Mode::Plain => Some(&self.plain),
            Mode::Traced => self.traced.as_ref(),
            Mode::Enhanced => self.enhanced.as_ref(),
        }
    }

    pub fn merge(&mut self, other: Entry) -> Result<(), String> {
        let (traced, enhanced, reference) = (self.traced.take().or(other.traced.clone()), self.enhanced.take().or(other.enhanced.clone()), self.reference.take().or(other.reference.clone()));
        self.traced = traced;
        self.enhanced = enhanced;
        self.reference = reference;
        let consistent = self.plain == other.plain
            && other.traced.as_ref().is_none_or(|t| self.traced.as_ref() == Some(t))
            && other.enhanced.as_ref().is_none_or(|t| self.enhanced.as_ref() == Some(t))
            && other.reference.as_ref().is_none_or(|t| self.reference.as_ref() == Some(t));
        self.ordered |= !consistent || other.ordered;
        match self.fingerprint == other.fingerprint {
            true => Ok(()),
            false => Err(format!("{}: fingerprints differ between duplicates", self.key)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Plain,
    Traced,
    Enhanced,
}

pub struct Canon;

impl Canon {
    pub const REDACTED: [&'static str; 4] = ["performance", "msSinceRun", "engineVersion", "duration"];

    pub fn value(value: Value) -> Value {
        Self::sorted(value, false)
    }

    pub fn redacted(value: Value) -> Value {
        Self::sorted(value, true)
    }

    pub fn variable(variable: &Variable) -> Value {
        Self::value(Self::typed(variable))
    }

    fn typed(variable: &Variable) -> Value {
        match variable {
            Variable::Dynamic(d) => serde_json::json!({ "$type": d.type_name(), "$value": d.to_value() }),
            Variable::Array(items) => Value::Array(items.borrow().iter().map(Self::typed).collect()),
            Variable::Object(map) => Value::Object(map.borrow().iter().map(|(k, v)| (k.to_string(), Self::typed(v))).collect()),
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        }
    }

    pub fn restore(value: &Value) -> Variable {
        match value {
            Value::Object(map) if map.len() == 2 && map.get("$type").and_then(Value::as_str) == Some("date") => {
                let text = map.get("$value").and_then(Value::as_str).unwrap_or_default();
                zen_expression::DateValue::from_text(text).unwrap_or_else(|| Variable::String(text.into()))
            }
            Value::Object(map) if map.len() == 2 && map.contains_key("$type") => map.get("$value").map(Self::restore).unwrap_or(Variable::Null),
            Value::Array(items) => Variable::from_array(items.iter().map(Self::restore).collect()),
            Value::Object(map) => {
                let mut object = VariableMap::new();
                for (key, item) in map {
                    object.insert(Variable::key(key), Self::restore(item));
                }
                Variable::from_object(object)
            }
            other => Variable::from(other.clone()),
        }
    }

    pub fn unordered(value: Value, redact: bool) -> Value {
        Self::sorted(Self::sets(value), redact)
    }

    fn sets(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| match (k.as_str(), v) {
                        ("writes", Value::Array(mut items)) => {
                            items.sort_by_key(|item| item.get("path").map(Value::to_string).unwrap_or_default());
                            let items = items.into_iter().map(Self::sets).collect();
                            (k, Value::Array(items))
                        }
                        (_, v) => (k, Self::sets(v)),
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(Self::sets).collect()),
            other => other,
        }
    }

    pub const DEPTH: usize = 96;

    fn sorted(value: Value, redact: bool) -> Value {
        Self::bounded(value, redact, 0)
    }

    fn bounded(value: Value, redact: bool, depth: usize) -> Value {
        if depth >= Self::DEPTH && (value.is_object() || value.is_array()) {
            let flat = Self::bounded(value, redact, 0);
            return serde_json::json!({ "$digest": Self::hash(&serde_json::to_string(&flat).unwrap_or_default()) });
        }
        match value {
            Value::Object(map) => {
                let ordered: BTreeMap<String, Value> = map
                    .into_iter()
                    .map(|(k, v)| match redact && Self::REDACTED.contains(&k.as_str()) {
                        true => (k, Value::String("~".to_string())),
                        false => (k, Self::bounded(v, redact, depth + 1)),
                    })
                    .collect();
                Value::Object(ordered.into_iter().collect::<Map<String, Value>>())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(|v| Self::bounded(v, redact, depth + 1)).collect()),
            other => other,
        }
    }

    pub fn tagged(variable: &Variable, out: &mut String) {
        match variable {
            Variable::Null => out.push('z'),
            Variable::Bool(true) => out.push('t'),
            Variable::Bool(false) => out.push('f'),
            Variable::Number(n) => {
                out.push('n');
                out.push_str(&n.to_string());
            }
            Variable::String(s) => {
                out.push('s');
                out.push_str(&serde_json::to_string(s.as_str()).unwrap_or_default());
            }
            Variable::Dynamic(d) => {
                out.push('d');
                out.push_str(d.type_name());
                out.push_str(&serde_json::to_string(&d.to_string()).unwrap_or_default());
            }
            Variable::Array(items) => {
                out.push('[');
                for (at, item) in items.borrow().iter().enumerate() {
                    if at > 0 {
                        out.push(',');
                    }
                    Self::tagged(item, out);
                }
                out.push(']');
            }
            Variable::Object(map) => {
                let map = map.borrow();
                let mut entries: Vec<(&str, &Variable)> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                out.push('{');
                for (at, (key, value)) in entries.into_iter().enumerate() {
                    if at > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    Self::tagged(value, out);
                }
                out.push('}');
            }
        }
    }

    pub fn hash(text: &str) -> String {
        let hash = text
            .bytes()
            .fold(0x6c62_272e_07bb_0142_62b8_2175_6295_c58du128, |h, b| (h ^ b as u128).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b));
        format!("{hash:032x}")
    }

    pub fn key(suite: &str, goals: &[String], input: &Variable) -> String {
        let mut text = String::new();
        Self::tagged(input, &mut text);
        format!("{suite} | {} | {}", goals.join(","), Self::hash(&text))
    }

    pub fn sharing(value: &Value) -> (usize, usize) {
        let refs = value.get("$refs").and_then(Value::as_array).cloned().unwrap_or_default();
        let shared: BTreeSet<usize> = refs.iter().enumerate().filter(|(_, v)| v.is_object() || v.is_array()).map(|(at, _)| at).collect();
        (shared.len(), Self::sites(value, &shared))
    }

    fn sites(value: &Value, shared: &BTreeSet<usize>) -> usize {
        let site = |s: &str| s.strip_prefix('@').and_then(|id| id.parse::<usize>().ok()).is_some_and(|id| shared.contains(&id));
        match value {
            Value::String(s) => usize::from(site(s)),
            Value::Array(items) => items.iter().map(|v| Self::sites(v, shared)).sum(),
            Value::Object(map) => map.values().map(|v| Self::sites(v, shared)).sum(),
            _ => 0,
        }
    }

    pub fn reference(traced: &Outcome, serialized: Result<Value, Value>) -> Value {
        let (ok, raw) = match serialized {
            Ok(value) => (true, value),
            Err(value) => (false, value),
        };
        let Value::Object(mut map) = raw else {
            return serde_json::json!({"malformed": true});
        };
        let trace = map.remove("trace");
        let (refs, sites) = trace.as_ref().map_or((0, 0), Self::sharing);
        if let Some(trace) = trace {
            let dereferenced = Variable::deserialize_ref(RcValue::from(&trace)).map(|v| serde_json::to_value(&v).unwrap_or(Value::Null)).unwrap_or(Value::String("undecodable".to_string()));
            map.insert("trace".to_string(), dereferenced);
        }
        let response = Canon::redacted(Value::Object(map));
        let same = match traced {
            Outcome::Ok(value) => ok && value == &response,
            Outcome::Err { value, .. } => !ok && value == &response,
        };
        let mut summary = serde_json::json!({"refs": refs, "sites": sites});
        if !same {
            summary["response"] = response;
        }
        summary
    }
}

impl Canon {
    pub fn mode(result: Result<Value, Value>, reference: bool) -> Outcome {
        let (ok, raw) = match result {
            Ok(value) => (true, value),
            Err(value) => (false, value),
        };
        let value = match raw {
            Value::Object(mut map) => {
                if let Some(trace) = map.remove("trace") {
                    let trace = match trace {
                        Value::String(text) => serde_json::from_str(&text).unwrap_or_else(|_| Self::opaque(&text)),
                        other => other,
                    };
                    match reference {
                        true => {
                            map.insert("$shared".to_string(), Self::shared(&trace));
                            let dereferenced = Variable::deserialize_ref(RcValue::from(&trace)).map(|v| serde_json::to_value(&v).unwrap_or(Value::Null)).unwrap_or(Value::String("undecodable".to_string()));
                            map.insert("trace".to_string(), dereferenced);
                        }
                        false => {
                            map.insert("trace".to_string(), trace);
                        }
                    }
                }
                Self::redacted(Value::Object(map))
            }
            other => Self::redacted(other),
        };
        match ok {
            true => Outcome::Ok(value),
            false => Outcome::Err { debug: String::new(), value },
        }
    }

    fn opaque(text: &str) -> Value {
        let marker = "\"performance\":\"";
        let mut rest = text;
        let mut kept = String::with_capacity(text.len());
        while let Some(at) = rest.find(marker) {
            kept.push_str(&rest[..at]);
            let tail = &rest[at + marker.len()..];
            rest = tail.find('"').map_or("", |end| &tail[end + 1..]);
        }
        kept.push_str(rest);
        let mut chars: Vec<char> = kept.chars().collect();
        chars.sort_unstable();
        serde_json::json!({ "$unparsed": Self::hash(&chars.into_iter().collect::<String>()) })
    }

    pub fn shared(value: &Value) -> Value {
        let refs = value.get("$refs").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut sites = vec![0usize; refs.len()];
        Self::count(value, &mut sites);
        let mut shared: Vec<(String, usize)> = refs
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_object() || v.is_array())
            .map(|(at, v)| (serde_json::to_string(&Self::redacted(Self::deref(v, &refs))).unwrap_or_default(), sites[at]))
            .collect();
        shared.sort();
        serde_json::json!(shared)
    }

    fn ref_id(text: &str) -> Option<usize> {
        text.strip_prefix('@').and_then(|id| id.parse::<usize>().ok())
    }

    fn count(value: &Value, sites: &mut [usize]) {
        match value {
            Value::String(text) => {
                if let Some(site) = Self::ref_id(text).and_then(|id| sites.get_mut(id)) {
                    *site += 1;
                }
            }
            Value::Array(items) => items.iter().for_each(|v| Self::count(v, sites)),
            Value::Object(map) => map.values().for_each(|v| Self::count(v, sites)),
            _ => {}
        }
    }

    fn deref(value: &Value, refs: &[Value]) -> Value {
        match value {
            Value::String(text) => match Self::ref_id(text).and_then(|id| refs.get(id)) {
                Some(target) => Self::deref(target, refs),
                None => value.clone(),
            },
            Value::Array(items) => Value::Array(items.iter().map(|v| Self::deref(v, refs)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        let key = match Self::ref_id(k).and_then(|id| refs.get(id)) {
                            Some(Value::String(text)) => text.clone(),
                            _ => k.clone(),
                        };
                        (key, Self::deref(v, refs))
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

pub struct Written {
    pub path: PathBuf,
    pub entries: usize,
    pub raw: u64,
    pub stored: u64,
}

pub struct Golden {
    pub name: String,
    entries: BTreeMap<String, Entry>,
    seen: Mutex<BTreeSet<String>>,
    missing: Mutex<BTreeSet<String>>,
    pub conflicts: Vec<String>,
}

impl Golden {
    pub const LIMIT: u64 = 15 * 1024 * 1024;
    pub const CLOCK: &'static str = "2025-03-20T10:15:30Z";

    pub fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
    }

    pub fn prepare() {
        std::env::set_var("TZ", "UTC");
        std::env::set_var("__ZEN_MOCK_UTC_TIME", Self::CLOCK);
    }

    pub fn new(name: &str) -> Golden {
        Golden {
            name: name.to_string(),
            entries: BTreeMap::new(),
            seen: Mutex::new(BTreeSet::new()),
            missing: Mutex::new(BTreeSet::new()),
            conflicts: Vec::new(),
        }
    }

    pub fn insert(&mut self, entry: Entry) {
        match self.entries.get_mut(&entry.key) {
            Some(existing) => {
                if let Err(conflict) = existing.merge(entry) {
                    self.conflicts.push(conflict);
                }
            }
            None => {
                self.entries.insert(entry.key.clone(), entry);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    fn files(&self) -> Vec<PathBuf> {
        let base = Self::root().join(&self.name);
        let mut files: Vec<PathBuf> = match base.is_dir() {
            true => std::fs::read_dir(&base).into_iter().flatten().flatten().map(|e| e.path()).collect(),
            false => ["jsonl", "jsonl.gz"].iter().map(|ext| Self::root().join(format!("{}.{ext}", self.name))).collect(),
        };
        files.retain(|p| p.is_file() && p.to_string_lossy().contains(".jsonl"));
        files.sort();
        files
    }

    pub fn open(name: &str) -> Golden {
        let mut golden = Golden::new(name);
        let files = golden.files();
        assert!(!files.is_empty(), "no golden files for {name} under {}", Self::root().display());
        for path in files {
            let file = std::fs::File::open(&path).expect("golden file");
            let reader: Box<dyn Read> = match path.extension().is_some_and(|e| e == "gz") {
                true => Box::new(GzDecoder::new(file)),
                false => Box::new(file),
            };
            for line in BufReader::new(reader).lines() {
                let line = line.expect("golden line");
                let entry: Entry = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                golden.insert(entry);
            }
        }
        golden
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        match self.entries.get(key) {
            Some(entry) => {
                self.seen.lock().expect("seen").insert(key.to_string());
                Some(entry)
            }
            None => {
                self.missing.lock().expect("missing").insert(key.to_string());
                None
            }
        }
    }

    pub fn outcome(&self, key: &str, mode: Mode) -> Outcome {
        match self.get(key).and_then(|e| e.outcome(mode)) {
            Some(outcome) => outcome.clone(),
            None => Outcome::missing(key),
        }
    }

    pub fn missing(&self) -> Vec<String> {
        self.missing.lock().expect("missing").iter().cloned().collect()
    }

    pub fn unvisited(&self) -> Vec<String> {
        let seen = self.seen.lock().expect("seen");
        self.entries.keys().filter(|k| !seen.contains(*k)).cloned().collect()
    }

    pub fn compare(&self, recorded: &Golden) -> Vec<String> {
        let mut out = Vec::new();
        for (key, entry) in &self.entries {
            match recorded.entries.get(key) {
                None => out.push(format!("{key}: not in recorded goldens")),
                Some(want) if want != entry => out.push(format!("{key}: differs\n  recorded {}\n  live     {}", serde_json::to_string(want).unwrap_or_default(), serde_json::to_string(entry).unwrap_or_default())),
                Some(_) => {}
            }
        }
        out.extend(recorded.entries.keys().filter(|k| !self.entries.contains_key(*k)).map(|k| format!("{k}: recorded but no longer produced")));
        out
    }

    pub fn write(&self, shard: impl Fn(&Entry) -> Option<String>) -> Vec<Written> {
        let mut shards: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
        for entry in self.entries.values() {
            shards.entry(shard(entry)).or_default().push(serde_json::to_string(entry).expect("entry json"));
        }
        let raw: u64 = shards.values().flatten().map(|l| l.len() as u64 + 1).sum();
        let gzip = raw > Self::LIMIT || std::env::var("GOLDEN_GZIP").is_ok_and(|v| v == "1");
        for stale in self.files() {
            std::fs::remove_file(stale).expect("remove stale golden");
        }
        let extension = match gzip {
            true => "jsonl.gz",
            false => "jsonl",
        };
        let mut written = Vec::new();
        for (shard, lines) in shards {
            let path = match shard {
                Some(name) => Self::root().join(&self.name).join(format!("{name}.{extension}")),
                None => Self::root().join(format!("{}.{extension}", self.name)),
            };
            std::fs::create_dir_all(path.parent().expect("parent")).expect("golden dir");
            let mut text = lines.join("\n");
            text.push('\n');
            let bytes = match gzip {
                true => {
                    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(9));
                    encoder.write_all(text.as_bytes()).expect("gzip");
                    encoder.finish().expect("gzip")
                }
                false => text.clone().into_bytes(),
            };
            std::fs::write(&path, &bytes).expect("write golden");
            written.push(Written {
                path,
                entries: lines.len(),
                raw: text.len() as u64,
                stored: bytes.len() as u64,
            });
        }
        written
    }
}

pub struct Live;

impl Live {
    pub const GRAPH: &'static str = "live graph references need the driver feature";

    #[cfg(feature = "driver")]
    pub fn walker(decision: &Decision) -> Decision {
        decision.interpreted()
    }

    #[cfg(not(feature = "driver"))]
    pub fn walker(decision: &Decision) -> Decision {
        decision.clone()
    }

    pub fn require_walker() {
        assert!(cfg!(feature = "driver"), "{}", Self::GRAPH);
    }

    #[cfg(feature = "driver")]
    pub fn policy(ws: &PolicyWorkspace, req: &EvaluateRequest) -> Result<EvaluationResult, PolicyError> {
        ws.evaluate_with_driver(req)
    }

    #[cfg(feature = "driver")]
    pub fn enhanced(ws: &PolicyWorkspace, req: &EvaluateRequest) -> Result<EvaluationResult, PolicyError> {
        ws.enhance_trace_with_driver(req)
    }

    #[cfg(not(feature = "driver"))]
    pub fn policy(_: &PolicyWorkspace, _: &EvaluateRequest) -> Result<EvaluationResult, PolicyError> {
        panic!("live policy references need the driver feature")
    }

    #[cfg(not(feature = "driver"))]
    pub fn enhanced(_: &PolicyWorkspace, _: &EvaluateRequest) -> Result<EvaluationResult, PolicyError> {
        panic!("live policy references need the driver feature")
    }
}

pub enum Reference {
    Live,
    Recorded(Golden),
}

impl Reference {
    const DEFAULT: &'static str = if cfg!(feature = "driver") || !cfg!(feature = "arbitrary_precision") { "live" } else { "recorded" };
    pub fn unavailable(corpus: &str) -> bool {
        let unavailable = !cfg!(feature = "driver") && !Self::recorded();
        if unavailable {
            eprintln!("{corpus}: skipped, references need the driver feature (live) or arbitrary_precision (recorded goldens)");
        }
        unavailable
    }

    pub fn recorded() -> bool {
        std::env::var("GOLDEN_REFERENCE").unwrap_or_else(|_| Self::DEFAULT.to_string()) == "recorded"
    }

    pub fn open(corpus: &str) -> Reference {
        match Self::recorded() {
            true => {
                assert!(std::env::var("SPEC_TZ").map_or(true, |tz| tz == "UTC"), "recorded goldens are UTC only");
                Golden::prepare();
                Reference::Recorded(Golden::open(corpus))
            }
            false => Reference::Live,
        }
    }

    pub fn prepare(live: &str) {
        match Self::recorded() {
            true => Golden::prepare(),
            false => std::env::set_var("__ZEN_MOCK_UTC_TIME", live),
        }
    }

    pub fn debug(error: &PolicyError) -> String {
        match Self::recorded() {
            true => Outcome::debug(error),
            false => format!("{error:?}"),
        }
    }

    pub fn ordered(&self, key: impl FnOnce() -> String) -> bool {
        match self {
            Reference::Live => false,
            Reference::Recorded(golden) => golden.get(&key()).is_some_and(|e| e.ordered),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self, Reference::Live)
    }

    pub async fn graph<T, F>(&self, key: impl FnOnce() -> String, live: impl FnOnce() -> F, recorded: impl FnOnce(&Outcome) -> T) -> T
    where
        F: Future<Output = T>,
    {
        match self {
            Reference::Live => live().await,
            Reference::Recorded(golden) => recorded(&golden.outcome(&key(), Mode::Plain)),
        }
    }

    pub fn policy<T>(&self, key: impl FnOnce() -> String, live: impl FnOnce() -> T, recorded: impl FnOnce(&Outcome) -> T) -> T {
        match self {
            Reference::Live => live(),
            Reference::Recorded(golden) => recorded(&golden.outcome(&key(), Mode::Plain)),
        }
    }

    pub fn stale(&self, key: impl FnOnce() -> String, fingerprint: &str) -> bool {
        match self {
            Reference::Live => false,
            Reference::Recorded(golden) => golden.get(&key()).is_some_and(|e| e.fingerprint.as_deref() != Some(fingerprint)),
        }
    }

    pub fn missing(&self) -> Vec<String> {
        match self {
            Reference::Live => Vec::new(),
            Reference::Recorded(golden) => golden.missing().into_iter().map(|k| format!("missing golden entry {k}")).collect(),
        }
    }
}
