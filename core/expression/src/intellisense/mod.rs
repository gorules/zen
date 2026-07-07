use crate::compiler::Compiler;
use crate::intellisense::completion::Completions;
use crate::intellisense::dependency::DependencyResolutionWalker;
use crate::intellisense::diagnostic::{
    collect_parser_diagnostics, collect_type_diagnostics, compiler_error_to_diagnostic,
    lexer_error_to_diagnostic, Diagnostic, DiagnosticSource, Severity,
};
use crate::intellisense::inspection::{inspect_at, InspectionResult};
use crate::intellisense::scope::IntelliSenseScope;
use crate::intellisense::type_provider::TypesProvider;
use crate::lexer::Lexer;
use crate::nl::project::Projector;
use crate::nl::{NlRequest, NlResult};
use crate::parser::{Node, NodeMetadata, Parser};
use crate::variable::VariableType;
use bumpalo::Bump;
use nohash_hasher::BuildNoHashHasher;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub mod completion;
pub mod dependency;
pub mod diagnostic;
mod discriminant;
mod entity_flow;
mod inspection;
mod scope;
pub(crate) mod type_provider;

pub use dependency::{DependencyResult, ReadDependency, Reference};
pub use discriminant::{ArmTest, NumberCover};
pub use entity_flow::FlowSource;

pub type AstMetadata = HashMap<usize, NodeMetadata, BuildNoHashHasher<usize>>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntelliSenseToken {
    pub span: (u32, u32),
    pub kind: VariableType,
    pub node_kind: &'static str,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionAnalysis {
    pub return_type: VariableType,
    pub reads: Vec<ReadDependency>,
    pub references: Vec<Reference>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct IntelliSense {
    arena: Bump,
    lexer: Lexer,
    strict: bool,
}

impl IntelliSense {
    pub fn new() -> Self {
        Self {
            arena: Bump::new(),
            lexer: Lexer::new(),
            strict: false,
        }
    }

    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    pub fn completions(
        &mut self,
        source: &str,
        pos: u32,
        data: &VariableType,
    ) -> Vec<completion::Completion> {
        let tokens = match self.type_check(source, data) {
            Some(t) => t,
            None => return Completions::build_scope(data),
        };

        Completions::build(source, pos, data, &tokens)
    }

    pub fn inspect(
        &mut self,
        source: &str,
        pos: u32,
        data: &VariableType,
    ) -> Option<InspectionResult> {
        let tokens = self.type_check(source, data)?;
        inspect_at(source, pos, &tokens)
    }

    pub fn analyze(&mut self, source: &str, data: &VariableType) -> Rc<ExpressionAnalysis> {
        Rc::new(self.analyze_standard_inner(source, data))
    }

    fn analyze_standard_inner(&mut self, source: &str, data: &VariableType) -> ExpressionAnalysis {
        self.arena.reset();
        let arena = &self.arena;
        let mut diagnostics = Vec::new();

        let tokens = match self.lexer.tokenize(arena, source) {
            Ok(tokens) => tokens,
            Err(err) => {
                diagnostics.push(lexer_error_to_diagnostic(&err));
                return ExpressionAnalysis {
                    return_type: VariableType::Any,
                    reads: Vec::new(),
                    references: Vec::new(),
                    diagnostics,
                };
            }
        };

        let Ok(parser) = Parser::try_new(&tokens, arena) else {
            return ExpressionAnalysis {
                return_type: VariableType::Any,
                reads: Vec::new(),
                references: Vec::new(),
                diagnostics,
            };
        };

        let parser = parser.standard().with_metadata();
        let parser_result = parser.parse();
        let ast = parser_result.root;

        if !parser_result.is_complete || ast.has_error() {
            if !parser_result.is_complete {
                diagnostics.push(Diagnostic {
                    span: (0, 0),
                    message: "Incomplete expression".to_string(),
                    severity: Severity::Error,
                    source: DiagnosticSource::Parser,
                });
            }
            collect_parser_diagnostics(ast, &mut diagnostics);
            return ExpressionAnalysis {
                return_type: VariableType::Any,
                reads: Vec::new(),
                references: Vec::new(),
                diagnostics,
            };
        }

        let metadata = parser_result.metadata.unwrap_or_default();

        let scope = IntelliSenseScope {
            pointer_data: data.shallow_clone(),
            root_data: data.shallow_clone(),
            current_data: data.shallow_clone(),
            ..Default::default()
        };

        let type_data = TypesProvider::generate(ast, scope, self.strict);

        let return_type = type_data
            .get_type(ast)
            .map(|t| t.kind.clone())
            .unwrap_or(VariableType::Any);

        collect_type_diagnostics(ast, &type_data, &metadata, &mut diagnostics);

        let dep_result = DependencyResolutionWalker::walk(ast, &metadata);

        let mut compiler = Compiler::new();
        if let Err(err) = compiler.compile(ast) {
            diagnostics.push(compiler_error_to_diagnostic(&err));
        }

        ExpressionAnalysis {
            return_type,
            reads: dep_result.reads,
            references: dep_result.references,
            diagnostics,
        }
    }

    pub fn nl_tokenize_batch(
        &mut self,
        requests: &[NlRequest],
        root_type: &VariableType,
    ) -> Vec<NlResult> {
        requests
            .iter()
            .map(|request| self.nl_tokenize(request, root_type))
            .collect()
    }

    pub fn nl_tokenize(&mut self, request: &NlRequest, root_type: &VariableType) -> NlResult {
        let scope = if request.unary {
            Self::unary_scope(root_type, request.subject_type.as_ref())
        } else {
            root_type.shallow_clone()
        };
        let mut result =
            self.nl_tokenize_scoped(&request.id, &request.expression, request.unary, &scope);
        if request.unary {
            result.subject_type = Some(scope.get("$"));
        }
        result
    }

    pub fn nl_tokenize_scoped(
        &mut self,
        id: &str,
        source: &str,
        unary: bool,
        scope_type: &VariableType,
    ) -> NlResult {
        let mut result = NlResult {
            id: id.to_string(),
            tokens: Vec::new(),
            enums: Vec::new(),
            diagnostics: Vec::new(),
            subject_type: None,
        };

        self.arena.reset();
        let arena = &self.arena;

        let tokens = match self.lexer.tokenize(arena, source) {
            Ok(tokens) => tokens,
            Err(err) => {
                result.diagnostics.push(lexer_error_to_diagnostic(&err));
                return result;
            }
        };

        let Ok(parser) = Parser::try_new(&tokens, arena) else {
            return result;
        };

        let parser_result = if unary {
            parser.unary().with_metadata().parse()
        } else {
            parser.standard().with_metadata().parse()
        };
        let ast = parser_result.root;

        if !parser_result.is_complete || ast.has_error() {
            if !parser_result.is_complete {
                result.diagnostics.push(Diagnostic {
                    span: (0, 0),
                    message: "Incomplete expression".to_string(),
                    severity: Severity::Error,
                    source: DiagnosticSource::Parser,
                });
            }
            collect_parser_diagnostics(ast, &mut result.diagnostics);
            return result;
        }

        let metadata = parser_result.metadata.unwrap_or_default();

        let scope = IntelliSenseScope {
            pointer_data: scope_type.shallow_clone(),
            root_data: scope_type.shallow_clone(),
            current_data: scope_type.shallow_clone(),
            ..Default::default()
        };

        let type_data = TypesProvider::generate(ast, scope, self.strict);
        collect_type_diagnostics(ast, &type_data, &metadata, &mut result.diagnostics);

        let (tokens, enums) = Projector::new(source, &type_data, &metadata, unary).run(ast);
        result.tokens = tokens;
        result.enums = enums;
        result
    }

    fn unary_scope(root_type: &VariableType, subject_type: Option<&VariableType>) -> VariableType {
        let subject = subject_type
            .map(|s| s.shallow_clone())
            .unwrap_or(VariableType::Any);

        let object = VariableType::empty_object();
        if let VariableType::Object(target) = &object {
            if let VariableType::Object(source) = root_type {
                for (key, value) in source.borrow().iter() {
                    target
                        .borrow_mut()
                        .insert(key.clone(), value.shallow_clone());
                }
            }
            target.borrow_mut().insert(Rc::from("$"), subject);
        }
        object
    }

    pub fn with_ast<T>(
        &mut self,
        source: &str,
        unary: bool,
        f: impl for<'arena> FnOnce(&'arena Node<'arena>, &AstMetadata) -> T,
    ) -> Option<T> {
        self.arena.reset();
        let arena = &self.arena;
        let tokens = self.lexer.tokenize(arena, source).ok()?;
        let parser = Parser::try_new(&tokens, arena).ok()?;
        let parser_result = if unary {
            parser.unary().with_metadata().parse()
        } else {
            parser.standard().with_metadata().parse()
        };
        let ast = parser_result.root;
        if !parser_result.is_complete || ast.has_error() {
            return None;
        }
        let metadata = parser_result.metadata.unwrap_or_default();
        Some(f(ast, &metadata))
    }

    pub fn field_reads(
        &mut self,
        source: &str,
        field_path: &[&str],
    ) -> Option<Vec<ReadDependency>> {
        self.arena.reset();
        let arena = &self.arena;
        let tokens = self.lexer.tokenize(arena, source).ok()?;
        let parser = Parser::try_new(&tokens, arena)
            .ok()?
            .standard()
            .with_metadata();
        let parser_result = parser.parse();
        let ast = parser_result.root;
        if !parser_result.is_complete || ast.has_error() {
            return None;
        }
        let metadata = parser_result.metadata.unwrap_or_default();
        DependencyResolutionWalker::field_dependencies(ast, &metadata, field_path)
    }

    pub fn arm_test(&mut self, source: &str) -> ArmTest {
        if source.trim().is_empty() {
            return ArmTest::Default;
        }
        self.arena.reset();
        let arena = &self.arena;
        let result = (|| {
            let tokens = self.lexer.tokenize(arena, source).ok()?;
            let parser = Parser::try_new(&tokens, arena).ok()?;
            let parser_result = parser.standard().with_metadata().parse();
            let ast = parser_result.root;
            if !parser_result.is_complete || ast.has_error() {
                return None;
            }
            Some(ArmTest::from_node(ast))
        })();
        result.unwrap_or(ArmTest::Unrecognized)
    }

    pub fn cell_test(&mut self, source: &str) -> ArmTest {
        if source.trim().is_empty() {
            return ArmTest::Default;
        }
        self.arena.reset();
        let arena = &self.arena;
        let result = (|| {
            let tokens = self.lexer.tokenize(arena, source).ok()?;
            let parser = Parser::try_new(&tokens, arena).ok()?;
            let parser_result = parser.unary().with_metadata().parse();
            let ast = parser_result.root;
            if !parser_result.is_complete || ast.has_error() {
                return None;
            }
            let test = ArmTest::from_node(ast);
            let on_reference = match &test {
                ArmTest::Enum { path, .. }
                | ArmTest::Bool { path, .. }
                | ArmTest::Number { path, .. } => {
                    matches!(path.as_slice(), [p] if p.as_ref() == "$")
                }
                _ => true,
            };
            on_reference.then_some(test)
        })();
        result.unwrap_or(ArmTest::Unrecognized)
    }

    pub fn flow_source(&mut self, source: &str) -> Option<FlowSource> {
        if source.trim().is_empty() {
            return None;
        }
        self.arena.reset();
        let arena = &self.arena;
        let tokens = self.lexer.tokenize(arena, source).ok()?;
        let parser = Parser::try_new(&tokens, arena).ok()?;
        let parser_result = parser.standard().parse();
        let ast = parser_result.root;
        if !parser_result.is_complete || ast.has_error() {
            return None;
        }
        FlowSource::from_node(ast)
    }

    pub fn reads(&mut self, source: &str) -> Vec<ReadDependency> {
        self.reads_inner(source, false)
    }

    pub fn reads_unary(&mut self, source: &str) -> Vec<ReadDependency> {
        self.reads_inner(source, true)
    }

    fn reads_inner(&mut self, source: &str, unary: bool) -> Vec<ReadDependency> {
        self.arena.reset();
        let arena = &self.arena;
        let result = (|| {
            let tokens = self.lexer.tokenize(arena, source).ok()?;
            let parser = Parser::try_new(&tokens, arena).ok()?;
            let parser_result = if unary {
                parser.unary().with_metadata().parse()
            } else {
                parser.standard().with_metadata().parse()
            };
            let ast = parser_result.root;
            if !parser_result.is_complete || ast.has_error() {
                return None;
            }
            let metadata = parser_result.metadata.unwrap_or_default();
            let dep = if unary {
                DependencyResolutionWalker::walk_with_locals(ast, &metadata, &["$"])
            } else {
                DependencyResolutionWalker::walk(ast, &metadata)
            };
            Some(dep.reads)
        })();
        result.unwrap_or_default()
    }

    pub fn analyze_unary(&mut self, source: &str, data: &VariableType) -> Rc<ExpressionAnalysis> {
        Rc::new(self.analyze_unary_inner(source, data))
    }

    fn analyze_unary_inner(&mut self, source: &str, data: &VariableType) -> ExpressionAnalysis {
        self.arena.reset();
        let arena = &self.arena;
        let mut diagnostics = Vec::new();

        let tokens = match self.lexer.tokenize(arena, source) {
            Ok(tokens) => tokens,
            Err(err) => {
                diagnostics.push(lexer_error_to_diagnostic(&err));
                return ExpressionAnalysis {
                    return_type: VariableType::Bool,
                    reads: Vec::new(),
                    references: Vec::new(),
                    diagnostics,
                };
            }
        };

        let Ok(parser) = Parser::try_new(&tokens, arena) else {
            return ExpressionAnalysis {
                return_type: VariableType::Bool,
                reads: Vec::new(),
                references: Vec::new(),
                diagnostics,
            };
        };

        let parser = parser.unary().with_metadata();
        let parser_result = parser.parse();
        let ast = parser_result.root;

        if !parser_result.is_complete || ast.has_error() {
            if !parser_result.is_complete {
                diagnostics.push(Diagnostic {
                    span: (0, 0),
                    message: "Incomplete expression".to_string(),
                    severity: Severity::Error,
                    source: DiagnosticSource::Parser,
                });
            }
            collect_parser_diagnostics(ast, &mut diagnostics);
            return ExpressionAnalysis {
                return_type: VariableType::Bool,
                reads: Vec::new(),
                references: Vec::new(),
                diagnostics,
            };
        }

        let metadata = parser_result.metadata.unwrap_or_default();

        let scope = IntelliSenseScope {
            pointer_data: data.shallow_clone(),
            root_data: data.shallow_clone(),
            current_data: data.shallow_clone(),
            ..Default::default()
        };

        let type_data = TypesProvider::generate(ast, scope, self.strict);
        collect_type_diagnostics(ast, &type_data, &metadata, &mut diagnostics);

        let dep_result = DependencyResolutionWalker::walk_with_locals(ast, &metadata, &["$"]);

        let mut compiler = Compiler::new();
        if let Err(err) = compiler.compile(ast) {
            diagnostics.push(compiler_error_to_diagnostic(&err));
        }

        ExpressionAnalysis {
            return_type: VariableType::Bool,
            reads: dep_result.reads,
            references: dep_result.references,
            diagnostics,
        }
    }

    pub fn type_check(
        &mut self,
        source: &str,
        data: &VariableType,
    ) -> Option<Vec<IntelliSenseToken>> {
        self.arena.reset();
        let arena = &self.arena;

        let tokens = self.lexer.tokenize(arena, source).ok()?;
        let parser = Parser::try_new(&tokens, arena).map(|p| p.standard()).ok()?;

        let parser_result = parser.with_metadata().parse();
        let ast = parser_result.root;
        let metadata = parser_result.metadata?;

        let type_data = TypesProvider::generate(
            ast,
            IntelliSenseScope {
                pointer_data: data.shallow_clone(),
                root_data: data.shallow_clone(),
                current_data: data.shallow_clone(),
                ..Default::default()
            },
            self.strict,
        );

        let results = RefCell::new(Vec::new());
        ast.walk(|node| {
            let addr = node as *const Node as usize;
            let mut r = results.borrow_mut();
            let typ = type_data.get_type(node);

            r.push(IntelliSenseToken {
                span: node
                    .span()
                    .or_else(|| metadata.get(&addr).map(|s| s.span))
                    .unwrap_or_default(),
                node_kind: node.into(),
                error: typ.map(|t| t.error.clone()).flatten(),
                kind: typ
                    .map(|t| t.kind.clone())
                    .unwrap_or_else(|| VariableType::Any),
            });
        });
        Some(results.into_inner())
    }

    pub fn type_check_unary(
        &mut self,
        source: &str,
        data: &VariableType,
    ) -> Option<Vec<IntelliSenseToken>> {
        self.arena.reset();
        let arena = &self.arena;

        let tokens = self.lexer.tokenize(arena, source).ok()?;
        let parser = Parser::try_new(&tokens, arena).map(|p| p.unary()).ok()?;

        let parser_result = parser.with_metadata().parse();
        let ast = parser_result.root;
        let metadata = parser_result.metadata?;

        let type_data = TypesProvider::generate(
            ast,
            IntelliSenseScope {
                pointer_data: data.shallow_clone(),
                root_data: data.shallow_clone(),
                current_data: data.shallow_clone(),
                ..Default::default()
            },
            self.strict,
        );

        let results = RefCell::new(Vec::new());
        ast.walk(|node| {
            let addr = node as *const Node as usize;
            let mut r = results.borrow_mut();
            let typ = type_data.get_type(node);

            r.push(IntelliSenseToken {
                span: metadata.get(&addr).map(|s| s.span).unwrap_or_default(),
                node_kind: node.into(),
                error: typ.map(|t| t.error.clone()).flatten(),
                kind: typ
                    .map(|t| t.kind.clone())
                    .unwrap_or_else(|| VariableType::Any),
            });
        });
        Some(results.into_inner())
    }
}
