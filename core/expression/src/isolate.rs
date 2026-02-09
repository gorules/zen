use ahash::HashMap;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;

use crate::arena::UnsafeArena;
use crate::compiler::{Compiler, CompilerError, Opcode};
use crate::expression::{OpcodeCache, Standard, Unary};
use crate::lexer::{Lexer, LexerError};
use crate::parser::{Parser, ParserError};
use crate::variable::Variable;
use crate::vm::{VMError, VM};
use crate::{Expression, ExpressionKind};

/// Isolate is a component that encapsulates an isolated environment for executing expressions.
///
/// Rerunning the Isolate allows for efficient memory reuse through an arena allocator.
/// The arena allocator optimizes memory management by reusing memory blocks for subsequent evaluations,
/// contributing to improved performance and resource utilization in scenarios where the Isolate is reused multiple times.
#[derive(Debug)]
pub struct Isolate<'arena> {
    lexer: Lexer<'arena>,
    compiler: Compiler,
    vm: VM,

    bump: UnsafeArena<'arena>,

    environment: Option<Variable>,
    references: HashMap<String, Variable>,
    cache: Option<Arc<OpcodeCache>>,
}

impl<'a> Isolate<'a> {
    pub fn new() -> Self {
        Self {
            lexer: Lexer::new(),
            compiler: Compiler::new(),
            vm: VM::new(),

            bump: UnsafeArena::new(),

            environment: None,
            references: Default::default(),
            cache: None,
        }
    }

    pub fn with_environment(variable: Variable) -> Self {
        let mut isolate = Isolate::new();
        isolate.set_environment(variable);

        isolate
    }

    pub fn with_cache(mut self, cache: Option<Arc<OpcodeCache>>) -> Self {
        self.cache = cache;
        self
    }

    pub fn set_environment(&mut self, variable: Variable) {
        self.environment.replace(variable);
    }

    pub fn set_cache(&mut self, cache: Arc<OpcodeCache>) {
        self.cache = Some(cache);
    }

    pub fn update_environment<F>(&mut self, mut updater: F)
    where
        F: FnMut(Option<&mut Variable>),
    {
        updater(self.environment.as_mut());
    }

    pub fn set_reference(&mut self, reference: &'a str) -> Result<(), IsolateError> {
        let reference_value = match self.references.get(reference) {
            Some(value) => value.clone(),
            None => {
                let result = self.run_standard(reference)?;
                self.references
                    .insert(reference.to_string(), result.clone());
                result
            }
        };

        if !matches!(&mut self.environment, Some(Variable::Object(_))) {
            self.environment.replace(Variable::empty_object());
        }

        let Some(Variable::Object(environment_object_ref)) = &self.environment else {
            return Err(IsolateError::ReferenceError);
        };

        let mut environment_object = environment_object_ref.borrow_mut();
        environment_object.insert(Rc::from("$"), reference_value);

        Ok(())
    }

    pub fn get_reference(&self, reference: &str) -> Option<Variable> {
        self.references.get(reference).cloned()
    }

    pub fn clear_references(&mut self) {
        self.references.clear();
    }

    fn run_internal(&mut self, source: &'a str, kind: ExpressionKind) -> Result<(), IsolateError> {
        self.bump.with_mut(|b| b.reset());
        let bump = self.bump.get();

        let tokens = self.lexer.tokenize(source)?;

        let base_parser = Parser::try_new(tokens, bump)?;
        let parser_result = match kind {
            ExpressionKind::Unary => base_parser.unary().parse(),
            ExpressionKind::Standard => base_parser.standard().parse(),
        };

        parser_result.error()?;

        self.compiler.compile(parser_result.root)?;

        Ok(())
    }

    pub fn compile_standard(
        &mut self,
        source: &'a str,
    ) -> Result<Expression<Standard>, IsolateError> {
        self.run_internal(source, ExpressionKind::Standard)?;
        let bytecode = self.compiler.get_bytecode().to_vec();

        Ok(Expression::new_standard(Arc::from(bytecode)))
    }

    pub fn run_standard(&mut self, source: &'a str) -> Result<Variable, IsolateError> {
        let cached = self
            .cache
            .as_ref()
            .and_then(|c| c.standard.get(source).cloned());
        if let Some(codes) = cached {
            return self.run_compiled(codes.as_ref());
        }

        self.run_internal(source, ExpressionKind::Standard)?;

        let bytecode = self.compiler.get_bytecode();
        let result = self
            .vm
            .run(bytecode, self.environment.clone().unwrap_or(Variable::Null))?;

        Ok(result)
    }
    pub fn run_compiled(&mut self, source: &[Opcode]) -> Result<Variable, IsolateError> {
        let result = self
            .vm
            .run(source, self.environment.clone().unwrap_or(Variable::Null))?;

        Ok(result)
    }

    pub fn compile_unary(&mut self, source: &'a str) -> Result<Expression<Unary>, IsolateError> {
        self.run_internal(source, ExpressionKind::Unary)?;
        let bytecode = self.compiler.get_bytecode().to_vec();

        Ok(Expression::new_unary(Arc::from(bytecode)))
    }

    pub fn run_unary(&mut self, source: &'a str) -> Result<bool, IsolateError> {
        let cached = self
            .cache
            .as_ref()
            .and_then(|c| c.unary.get(source).cloned());
        if let Some(codes) = cached {
            return self.run_unary_compiled(codes.as_ref());
        }

        self.run_internal(source, ExpressionKind::Unary)?;

        let bytecode = self.compiler.get_bytecode();
        let result = self
            .vm
            .run(bytecode, self.environment.clone().unwrap_or(Variable::Null))?;

        result.as_bool().ok_or_else(|| IsolateError::ValueCastError)
    }

    pub fn run_unary_compiled(&mut self, code: &[Opcode]) -> Result<bool, IsolateError> {
        let result = self
            .vm
            .run(code, self.environment.clone().unwrap_or(Variable::Null))?;

        result.as_bool().ok_or_else(|| IsolateError::ValueCastError)
    }
}

/// Errors which happen within isolate or during evaluation
#[derive(Debug, Error)]
pub enum IsolateError {
    #[error("Lexer error: {source}")]
    LexerError { source: LexerError },

    #[error("Parser error: {source}")]
    ParserError { source: ParserError },

    #[error("Compiler error: {source}")]
    CompilerError { source: CompilerError },

    #[error("VM error: {source}")]
    VMError { source: VMError },

    #[error("Value cast error")]
    ValueCastError,

    #[error("Failed to compute reference")]
    ReferenceError,

    #[error("Missing context reference")]
    MissingContextReference,
}

impl Serialize for IsolateError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;

        match &self {
            IsolateError::ReferenceError => {
                map.serialize_entry("type", "referenceError")?;
            }
            IsolateError::MissingContextReference => {
                map.serialize_entry("type", "missingContextReference")?;
            }
            IsolateError::ValueCastError => {
                map.serialize_entry("type", "valueCastError")?;
            }
            IsolateError::LexerError { source } => {
                map.serialize_entry("type", "lexerError")?;
                map.serialize_entry("source", source.to_string().as_str())?;
            }
            IsolateError::ParserError { source } => {
                map.serialize_entry("type", "parserError")?;
                map.serialize_entry("source", source.to_string().as_str())?;
            }
            IsolateError::CompilerError { source } => {
                map.serialize_entry("type", "compilerError")?;
                map.serialize_entry("source", source.to_string().as_str())?;
            }
            IsolateError::VMError { source } => {
                map.serialize_entry("type", "vmError")?;
                map.serialize_entry("source", source.to_string().as_str())?;
            }
        }

        map.end()
    }
}

impl From<LexerError> for IsolateError {
    fn from(source: LexerError) -> Self {
        IsolateError::LexerError { source }
    }
}

impl From<ParserError> for IsolateError {
    fn from(source: ParserError) -> Self {
        IsolateError::ParserError { source }
    }
}

impl From<VMError> for IsolateError {
    fn from(source: VMError) -> Self {
        IsolateError::VMError { source }
    }
}

impl From<CompilerError> for IsolateError {
    fn from(source: CompilerError) -> Self {
        IsolateError::CompilerError { source }
    }
}
