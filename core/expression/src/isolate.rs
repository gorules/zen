use ahash::AHasher;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use thiserror::Error;

use crate::arena::UnsafeArena;
use crate::compiler::{Compiler, CompilerError};
use crate::lexer::{Lexer, LexerError};
use crate::parser::{Parser, ParserError};
use crate::variable::Variable;
use crate::vm::{VMError, VM};

type ADefHasher = BuildHasherDefault<AHasher>;

/// Isolate is a component that encapsulates an isolated environment for executing expressions.
///
/// Rerunning the Isolate allows for efficient memory reuse through an arena allocator.
/// The arena allocator optimizes memory management by reusing memory blocks for subsequent evaluations,
/// contributing to improved performance and resource utilization in scenarios where the Isolate is reused multiple times.
#[derive(Debug)]
pub struct Isolate<'arena> {
    lexer: Lexer<'arena>,
    compiler: Compiler<'arena>,
    vm: VM,

    bump: UnsafeArena<'arena>,

    environment: Option<Variable>,
    references: HashMap<String, Variable, ADefHasher>,
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
        }
    }

    pub fn with_environment(variable: Variable) -> Self {
        let mut isolate = Isolate::new();
        isolate.set_environment(variable);

        isolate
    }

    pub fn set_environment(&mut self, variable: Variable) {
        self.environment.replace(variable);
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
        environment_object.insert("$".to_string(), reference_value);

        Ok(())
    }

    pub fn get_reference(&self, reference: &str) -> Option<Variable> {
        self.references.get(reference).cloned()
    }

    pub fn run_standard(&mut self, source: &'a str) -> Result<Variable, IsolateError> {
        self.bump.with_mut(|b| b.reset());
        let bump = self.bump.get();

        let tokens = self
            .lexer
            .tokenize(source)
            .map_err(|source| IsolateError::LexerError { source })?;

        let parser = Parser::try_new(tokens, bump)
            .map_err(|source| IsolateError::ParserError { source })?
            .standard();

        let parser_result = parser.parse();
        parser_result
            .error()
            .map_err(|source| IsolateError::ParserError { source })?;

        let bytecode = self
            .compiler
            .compile(parser_result.root)
            .map_err(|source| IsolateError::CompilerError { source })?;

        let result = self
            .vm
            .run(bytecode, self.environment.clone().unwrap_or(Variable::Null))
            .map_err(|source| IsolateError::VMError { source })?;

        Ok(result)
    }

    pub fn run_unary(&mut self, source: &'a str) -> Result<bool, IsolateError> {
        self.bump.with_mut(|b| b.reset());
        let bump = self.bump.get();

        let tokens = self
            .lexer
            .tokenize(source)
            .map_err(|source| IsolateError::LexerError { source })?;

        let parser = Parser::try_new(tokens, bump)
            .map_err(|source| IsolateError::ParserError { source })?
            .unary();

        let parser_result = parser.parse();
        parser_result
            .error()
            .map_err(|source| IsolateError::ParserError { source })?;

        let bytecode = self
            .compiler
            .compile(parser_result.root)
            .map_err(|source| IsolateError::CompilerError { source })?;

        let result = self
            .vm
            .run(bytecode, self.environment.clone().unwrap_or(Variable::Null))
            .map_err(|source| IsolateError::VMError { source })?;

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
