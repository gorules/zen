use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use zen_expression::intellisense::{ArmTest, ExpressionAnalysis, IntelliSense, ReadDependency};
use zen_expression::variable::{Variable, VariableType};
use zen_expression::{Isolate, IsolateError};

use super::property_read::ReadFlattener;
use super::type_check::TypeCheck;
use crate::policy::db::AnalysisPass;
use crate::policy::ir::PropertyPath;
use crate::policy::queries::scope::VariableTypeScope;
use crate::policy::types::{
    CursorTarget, Diagnostic, DiagnosticCode, DiagnosticLocation, ExpressionKind, Severity,
    WriteTrace,
};

pub type SharedIntelliSense = Rc<RefCell<IntelliSense>>;

#[derive(Debug, Clone)]
pub struct ExpressionLocation {
    pub block_id: Arc<str>,
    pub expression_id: Arc<str>,
    pub kind: ExpressionKind,
    pub source: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct WriteTarget {
    pub path: PropertyPath,
    pub resolved_type: VariableType,
    pub instance_source: Option<InstanceSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSource {
    pub path: Arc<str>,
    pub element: bool,
}

#[derive(Debug, Clone)]
pub struct PropertyRead {
    pub path: PropertyPath,
    pub expression_id: Option<Arc<str>>,
    pub span: Option<(u32, u32)>,
    pub via_alias: bool,
    pub unresolved: bool,
}

#[derive(Debug)]
pub struct ExecutionError {
    pub block_id: Arc<str>,
    pub policy_path: Arc<str>,
    pub expression: Arc<str>,
    pub source: IsolateError,
}

pub type SharedDictionaryTypes = Rc<ahash::HashMap<Arc<str>, VariableType>>;

pub struct AnalysisContext {
    scope: VariableType,
    policy_path: Arc<str>,
    block_id: Arc<str>,
    reads: Vec<PropertyRead>,
    writes: Vec<WriteTarget>,
    diagnostics: Vec<Diagnostic>,
    pass: AnalysisPass,
    intellisense: SharedIntelliSense,
    dictionary_types: SharedDictionaryTypes,
}

impl AnalysisContext {
    pub fn new(
        scope: VariableType,
        policy_path: Arc<str>,
        block_id: Arc<str>,
        intellisense: SharedIntelliSense,
        pass: AnalysisPass,
        dictionary_types: SharedDictionaryTypes,
    ) -> Self {
        Self {
            scope,
            policy_path,
            block_id,
            reads: Vec::new(),
            writes: Vec::new(),
            diagnostics: Vec::new(),
            pass,
            intellisense,
            dictionary_types,
        }
    }

    pub(super) fn scope(&self) -> &VariableType {
        &self.scope
    }

    pub(super) fn dictionary_types(&self) -> &ahash::HashMap<Arc<str>, VariableType> {
        &self.dictionary_types
    }

    pub fn analyze_standard(
        &mut self,
        source: &Arc<str>,
        expression_id: Option<Arc<str>>,
    ) -> Rc<ExpressionAnalysis> {
        self.analyze_with(source, ExpressionKind::Standard, expression_id)
    }

    fn analyze_with(
        &mut self,
        source: &Arc<str>,
        kind: ExpressionKind,
        expression_id: Option<Arc<str>>,
    ) -> Rc<ExpressionAnalysis> {
        let analysis = match self.pass {
            AnalysisPass::Shallow => {
                IntelliSenseSource::reads_only(&mut self.intellisense.borrow_mut(), source, kind)
            }
            AnalysisPass::Enriched => IntelliSenseSource::analyze(
                &mut self.intellisense.borrow_mut(),
                source,
                kind,
                &self.scope,
            ),
        };
        self.record_reads(&analysis, &expression_id);
        self.absorb_diagnostics(&analysis, &expression_id);
        analysis
    }

    pub(super) fn arm_test(&mut self, source: &Arc<str>) -> ArmTest {
        IntelliSenseSource::arm_test(&mut self.intellisense.borrow_mut(), source)
    }

    pub(super) fn cell_test(&mut self, source: &Arc<str>) -> ArmTest {
        IntelliSenseSource::cell_test(&mut self.intellisense.borrow_mut(), source)
    }

    pub(super) fn is_enriched(&self) -> bool {
        matches!(self.pass, AnalysisPass::Enriched)
    }

    pub fn analyze_unary_in_scope(
        &mut self,
        source: &Arc<str>,
        scope: &VariableType,
        expression_id: Option<Arc<str>>,
    ) -> Rc<ExpressionAnalysis> {
        let analysis = match self.pass {
            AnalysisPass::Shallow => IntelliSenseSource::reads_only(
                &mut self.intellisense.borrow_mut(),
                source,
                ExpressionKind::Unary,
            ),
            AnalysisPass::Enriched => IntelliSenseSource::analyze(
                &mut self.intellisense.borrow_mut(),
                source,
                ExpressionKind::Unary,
                scope,
            ),
        };
        self.record_reads(&analysis, &expression_id);
        self.absorb_diagnostics(&analysis, &expression_id);
        analysis
    }

    pub fn record_write(
        &mut self,
        path: PropertyPath,
        resolved_type: VariableType,
        expression_id: Option<Arc<str>>,
        target: Option<CursorTarget>,
    ) {
        self.record_write_sourced(path, resolved_type, expression_id, target, None);
    }

    pub fn record_write_sourced(
        &mut self,
        path: PropertyPath,
        resolved_type: VariableType,
        expression_id: Option<Arc<str>>,
        target: Option<CursorTarget>,
        instance_source: Option<InstanceSource>,
    ) {
        TypeCheck::check_no_any(self, &resolved_type, expression_id, target, &path);
        if matches!(self.pass, AnalysisPass::Enriched) {
            self.scope.insert_at_path(&path, &resolved_type, true);
        }
        self.writes.push(WriteTarget {
            path,
            resolved_type,
            instance_source,
        });
    }

    pub(super) fn flow_source(&mut self, source: &Arc<str>) -> Option<InstanceSource> {
        if matches!(self.pass, AnalysisPass::Enriched) {
            return None;
        }
        let flow = self.intellisense.borrow_mut().flow_source(source)?;
        let path: Vec<&str> = flow.path.iter().map(|s| s.as_ref()).collect();
        Some(InstanceSource {
            path: Arc::from(path.join(".")),
            element: flow.element,
        })
    }

    pub fn error(
        &mut self,
        code: DiagnosticCode,
        expression_id: Option<Arc<str>>,
        span: Option<(u32, u32)>,
        message: impl Into<String>,
    ) {
        self.error_with_target(code, expression_id, span, None, message);
    }

    pub fn error_with_target(
        &mut self,
        code: DiagnosticCode,
        expression_id: Option<Arc<str>>,
        span: Option<(u32, u32)>,
        target: Option<CursorTarget>,
        message: impl Into<String>,
    ) {
        let mut location = self.location_with(expression_id, span);
        if let Some(t) = target {
            location = location.with_target(t);
        }
        self.diagnostics
            .push(Diagnostic::error(code, location, message));
    }

    fn location_with(
        &self,
        expression_id: Option<Arc<str>>,
        span: Option<(u32, u32)>,
    ) -> DiagnosticLocation {
        DiagnosticLocation {
            policy_path: self.policy_path.clone(),
            block_id: Some(self.block_id.clone()),
            expression_id,
            span,
            target: None,
        }
    }

    pub fn merge_types(
        &mut self,
        types: &[VariableType],
        label: &str,
        expression_id: Option<Arc<str>>,
        target: Option<CursorTarget>,
    ) -> VariableType {
        let Some(result) = types
            .iter()
            .map(|t| t.shallow_clone())
            .reduce(|acc, t| acc.merge(&t))
        else {
            return VariableType::Null;
        };

        if matches!(result, VariableType::Any)
            && types.len() > 1
            && !types.iter().any(|t| matches!(t, VariableType::Any))
        {
            let span = target.as_ref().map(|_| Self::write_label_span(label));
            self.error_with_target(
                DiagnosticCode::TypeMismatch,
                expression_id,
                span,
                target,
                format!(
                    "'{}' has incompatible types: {}",
                    label,
                    types
                        .iter()
                        .map(|t| format!("`{}`", t))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        result
    }

    fn write_label_span(label: &str) -> (u32, u32) {
        (0, label.chars().count() as u32)
    }

    pub fn finish(mut self) -> AnalysisSummary {
        self.reads.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.expression_id.cmp(&b.expression_id))
        });
        self.reads
            .dedup_by(|a, b| a.path == b.path && a.expression_id == b.expression_id);
        self.validate_reads();
        AnalysisSummary {
            reads: self.reads,
            writes: self.writes,
            diagnostics: self.diagnostics,
        }
    }

    fn validate_reads(&mut self) {
        let mut problems: Vec<(Option<Arc<str>>, Option<(u32, u32)>, String)> = Vec::new();
        for read in &self.reads {
            if matches!(read.path.as_ref(), "$" | "$root") || read.via_alias || read.unresolved {
                continue;
            }
            let resolved = self.scope.resolve_at(&read.path);
            let unknown = match resolved {
                VariableType::Any => true,
                VariableType::Null => !Self::path_declared(&self.scope, &read.path),
                _ => false,
            };
            if !unknown {
                continue;
            }
            if self.read_already_diagnosed(read) {
                continue;
            }
            problems.push((
                read.expression_id.clone(),
                read.span,
                format!("Unknown property '{}'", read.path),
            ));
        }
        for (expr_id, span, msg) in problems {
            self.error(DiagnosticCode::UndefinedVariable, expr_id, span, msg);
        }
    }

    fn path_declared(scope: &VariableType, path: &str) -> bool {
        match path.rsplit_once('.') {
            Some((parent, key)) => Self::scope_has_key(&scope.resolve_at(parent), key),
            None => Self::scope_has_key(scope, path),
        }
    }

    fn scope_has_key(scope: &VariableType, key: &str) -> bool {
        match scope {
            VariableType::Object(obj) => obj.borrow().contains_key(key),
            VariableType::Nullable(inner) => Self::scope_has_key(inner, key),
            _ => false,
        }
    }

    fn read_already_diagnosed(&self, read: &PropertyRead) -> bool {
        let Some(read_span) = read.span else {
            return false;
        };
        self.diagnostics.iter().any(|d| {
            d.severity == Severity::Error
                && d.location.expression_id == read.expression_id
                && d.location
                    .span
                    .is_some_and(|s| s.0 < read_span.1 && read_span.0 < s.1)
        })
    }

    fn record_reads(&mut self, analysis: &ExpressionAnalysis, expression_id: &Option<Arc<str>>) {
        ReadFlattener::extend_from_deps(&analysis.reads, expression_id, &mut self.reads);
    }

    fn absorb_diagnostics(
        &mut self,
        analysis: &ExpressionAnalysis,
        expression_id: &Option<Arc<str>>,
    ) {
        for diag in &analysis.diagnostics {
            let location = DiagnosticLocation {
                policy_path: self.policy_path.clone(),
                block_id: Some(self.block_id.clone()),
                expression_id: expression_id.clone(),
                span: Some(diag.span),
                target: None,
            };
            self.diagnostics
                .push(Diagnostic::from_expression(diag, location));
        }
    }
}

#[derive(Clone)]
pub struct AnalysisSummary {
    pub reads: Vec<PropertyRead>,
    pub writes: Vec<WriteTarget>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct ExecutionContext<'a> {
    pub store: &'a Variable,
    pub policy_path: &'a Arc<str>,
    pub block_id: &'a Arc<str>,
    pub trace: bool,
    pub extras: bool,
    pub write_log: Option<&'a RefCell<Vec<WriteTrace>>>,
    pub env_mirror: Option<&'a Variable>,
    pub isolate: &'a RefCell<Isolate>,
}

impl ExecutionContext<'_> {
    pub fn expression_error(&self, expression: &Arc<str>, source: IsolateError) -> ExecutionError {
        ExecutionError {
            block_id: self.block_id.clone(),
            policy_path: self.policy_path.clone(),
            expression: expression.clone(),
            source,
        }
    }

    pub fn write(&self, path: &Arc<str>, value: Variable) {
        if let Some(log) = self.write_log {
            log.borrow_mut().push(WriteTrace {
                path: path.clone(),
                value: value.deep_clone(),
            });
        }
        self.store.dot_insert(path, value);
        self.mirror_top_level(path);
    }

    fn mirror_top_level(&self, path: &str) {
        let Some(env) = self.env_mirror else {
            return;
        };
        let (Some(store_fields), Some(env_fields)) = (self.store.as_object(), env.as_object())
        else {
            return;
        };
        let segment = &path[..path.find('.').unwrap_or(path.len())];
        let store_fields = store_fields.borrow();
        let Some((key, value)) = store_fields.get_key_value(segment) else {
            return;
        };
        env_fields
            .borrow_mut()
            .insert(key.clone(), value.shallow_clone());
    }
}

pub(crate) struct IntelliSenseSource;

impl IntelliSenseSource {
    pub(crate) fn analyze(
        is: &mut IntelliSense,
        source: &Arc<str>,
        kind: ExpressionKind,
        scope: &VariableType,
    ) -> Rc<ExpressionAnalysis> {
        match kind {
            ExpressionKind::Standard => is.analyze(source, scope),
            ExpressionKind::Unary => is.analyze_unary(source, scope),
        }
    }

    pub(crate) fn reads_only(
        is: &mut IntelliSense,
        source: &Arc<str>,
        kind: ExpressionKind,
    ) -> Rc<ExpressionAnalysis> {
        let (reads, return_type) = match kind {
            ExpressionKind::Standard => (is.reads(source), VariableType::Any),
            ExpressionKind::Unary => (is.reads_unary(source), VariableType::Bool),
        };
        Rc::new(ExpressionAnalysis {
            return_type,
            reads,
            references: Vec::new(),
            diagnostics: Vec::new(),
        })
    }

    pub(crate) fn arm_test(is: &mut IntelliSense, source: &Arc<str>) -> ArmTest {
        is.arm_test(source)
    }

    pub(crate) fn cell_test(is: &mut IntelliSense, source: &Arc<str>) -> ArmTest {
        is.cell_test(source)
    }

    pub(crate) fn field_reads(
        is: &mut IntelliSense,
        source: &Arc<str>,
        field_path: &[&str],
    ) -> Option<Vec<ReadDependency>> {
        is.field_reads(source, field_path)
    }
}
