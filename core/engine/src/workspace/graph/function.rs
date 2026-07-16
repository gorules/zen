use std::hash::{Hash, Hasher};
use std::sync::Arc;

use zen_expression::variable::VariableType;

use crate::workspace::db::Db;
use crate::workspace::graph::ts_type::TsTypeParser;

pub type FunctionTypeResolver = dyn Fn(&str, &VariableType) -> Option<String>;

pub(crate) type FunctionKey = (u64, u64);

#[derive(Debug, Clone)]
pub struct FunctionResolutionRequest {
    pub source: Arc<str>,
    pub input: VariableType,
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedFunction {
    Type(VariableType),
    Unresolved,
}

pub(crate) enum FunctionTypeOutcome {
    Typed(VariableType),
    Unresolved,
    Unknown,
}

impl Db {
    pub(crate) fn function_output_type(
        &self,
        source: &Arc<str>,
        input: &VariableType,
    ) -> FunctionTypeOutcome {
        let key = Self::function_key(source, input);
        let outcome = self.function_outcome(key, source, input);
        self.graph_fn_record(key, self.function_state(key));
        outcome
    }

    fn function_outcome(
        &self,
        key: FunctionKey,
        source: &Arc<str>,
        input: &VariableType,
    ) -> FunctionTypeOutcome {
        if let Some(entry) = self.function_types().borrow().get(&key) {
            return match entry {
                ResolvedFunction::Type(t) => FunctionTypeOutcome::Typed(t.shallow_clone()),
                ResolvedFunction::Unresolved => FunctionTypeOutcome::Unresolved,
            };
        }

        if let Some(resolver) = self.function_resolver().borrow().as_ref() {
            let entry = match resolver(source.as_ref(), input) {
                Some(ts) => match TsTypeParser::variable_type(&ts) {
                    Some(resolved) => ResolvedFunction::Type(resolved),
                    None => ResolvedFunction::Unresolved,
                },
                None => ResolvedFunction::Unresolved,
            };
            self.function_types()
                .borrow_mut()
                .insert(key, entry.clone());
            return match entry {
                ResolvedFunction::Type(t) => FunctionTypeOutcome::Typed(t),
                ResolvedFunction::Unresolved => FunctionTypeOutcome::Unresolved,
            };
        }

        if self.function_requested().borrow_mut().insert(key) {
            self.function_requests()
                .borrow_mut()
                .push(FunctionResolutionRequest {
                    source: source.clone(),
                    input: input.shallow_clone(),
                });
        }
        FunctionTypeOutcome::Unknown
    }

    pub fn function_resolution_requests(&self) -> Vec<FunctionResolutionRequest> {
        let snap = self.snapshot();
        let mut paths: Vec<Arc<str>> = snap.graphs.keys().cloned().collect();
        paths.sort();
        for path in &paths {
            let _ = self.graph_analysis(path);
        }
        std::mem::take(&mut *self.function_requests().borrow_mut())
    }

    pub fn set_function_type(&self, source: &str, input: &VariableType, ts_type: Option<&str>) {
        let key = Self::function_key_str(source, input);
        let entry = match ts_type.and_then(TsTypeParser::variable_type) {
            Some(resolved) => ResolvedFunction::Type(resolved),
            None => ResolvedFunction::Unresolved,
        };
        self.function_types().borrow_mut().insert(key, entry);
        self.invalidate_snapshot();
    }

    pub(crate) fn function_state(&self, key: FunctionKey) -> u64 {
        match self.function_types().borrow().get(&key) {
            None => 0,
            Some(ResolvedFunction::Unresolved) => 1,
            Some(ResolvedFunction::Type(t)) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                2u8.hash(&mut hasher);
                t.hash(&mut hasher);
                hasher.finish()
            }
        }
    }

    pub(crate) fn function_key(source: &Arc<str>, input: &VariableType) -> FunctionKey {
        Self::function_key_str(source.as_ref(), input)
    }

    fn function_key_str(source: &str, input: &VariableType) -> FunctionKey {
        let mut source_hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut source_hasher);
        let mut input_hasher = std::collections::hash_map::DefaultHasher::new();
        input.hash(&mut input_hasher);
        (source_hasher.finish(), input_hasher.finish())
    }
}
