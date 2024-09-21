use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use ahash::AHasher;
use bumpalo::Bump;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::arena::UnsafeArena;
use crate::compiler::{Compiler, CompilerError};
use crate::lexer::{Lexer, LexerError};
use crate::parser::{Parser, ParserError};
use crate::variable::{ToVariable, Variable};
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
    vm: VM<'arena>,

    bump: UnsafeArena<'arena>,
    reference_bump: UnsafeArena<'arena>,

    environment: Option<&'arena mut Variable<'arena>>,
    references: HashMap<&'arena str, &'arena Variable<'arena>, ADefHasher>,
}

impl<'a> Isolate<'a> {
    pub fn new() -> Self {
        Self {
            lexer: Lexer::new(),
            compiler: Compiler::new(),
            vm: VM::new(),

            bump: UnsafeArena::new(),
            reference_bump: UnsafeArena::new(),

            environment: None,
            references: Default::default(),
        }
    }

    pub fn with_environment(value: &Value) -> Self {
        let mut isolate = Isolate::new();
        isolate.set_environment(value);

        isolate
    }

    pub fn set_environment(&mut self, value: &Value) {
        let bump = self.reference_bump.get();
        let new_environment = value.to_variable(bump).unwrap();

        self.environment.replace(bump.alloc(new_environment));
    }

    pub fn update_environment<F>(&mut self, mut updater: F)
    where
        F: FnMut(&'a Bump, &mut Option<&'a mut Variable<'a>>),
    {
        let bump = self.reference_bump.get();
        updater(bump, &mut self.environment);
    }

    pub fn set_reference(&mut self, reference: &'a str) -> Result<(), IsolateError> {
        let bump = self.reference_bump.get();
        let reference_value = match self.references.get(reference) {
            Some(value) => value,
            None => {
                let result = self.run_standard(reference)?;
                let value = &*bump.alloc(result.to_variable(bump).unwrap());
                self.references.insert(reference, value);
                value
            }
        };

        if !matches!(&mut self.environment, Some(Variable::Object(_))) {
            self.environment
                .replace(bump.alloc(Variable::empty_object(bump)));
        }

        let Some(Variable::Object(environment_object)) = self.environment else {
            return Err(IsolateError::ReferenceError);
        };

        environment_object.insert("$", reference_value.clone_in(bump));
        Ok(())
    }

    pub fn get_reference(&self, reference: &str) -> Option<Value> {
        let reference_variable = self.references.get(reference)?;

        Some(reference_variable.to_value())
    }

    pub fn run_standard(&mut self, source: &'a str) -> Result<Value, IsolateError> {
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
            .run(
                bytecode,
                bump,
                self.environment.as_deref().unwrap_or(&Variable::Null),
            )
            .map_err(|source| IsolateError::VMError { source })?;

        Ok(result.to_value())
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
            .run(
                bytecode,
                bump,
                self.environment.as_deref().unwrap_or(&Variable::Null),
            )
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
