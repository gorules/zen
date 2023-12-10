use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use ahash::AHasher;
use bumpalo::Bump;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::compiler::{Compiler, CompilerError};
use crate::lexer::{Lexer, LexerError};
use crate::parser::{Parser, ParserError};
use crate::vm::{VMError, Variable, VM};

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

    bump: Bump,
    reference_bump: Bump,

    environment: Option<&'arena mut Variable<'arena>>,
    references: HashMap<&'arena str, &'arena Variable<'arena>, ADefHasher>,
}

impl<'a> Isolate<'a> {
    pub fn new() -> Self {
        Self {
            lexer: Lexer::new(),
            compiler: Compiler::new(),
            vm: VM::new(),

            bump: Default::default(),
            reference_bump: Default::default(),

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
        let bump = self.get_reference_bump();
        let new_environment = Variable::from_serde(value, self.get_reference_bump());

        self.environment.replace(bump.alloc(new_environment));
    }

    pub fn set_reference(&mut self, reference: &'a str) -> Result<(), IsolateError> {
        let bump = self.get_reference_bump();

        let reference_value = match self.references.get(reference) {
            Some(value) => value,
            None => {
                let result = self.run_standard(reference)?;
                let value = &*bump.alloc(Variable::from_serde(&result, bump));
                self.references.insert(reference, value);
                value
            }
        };

        let environment = self.environment.as_deref().unwrap_or(&Variable::Null);
        if !matches!(environment, Variable::Object(_)) {
            let new_environment = bump.alloc(Variable::empty_object_in(bump));
            self.environment.replace(new_environment);
        }

        let Some(Variable::Object(environment_object)) = self.environment else {
            return Err(IsolateError::ReferenceError);
        };

        environment_object.insert("$", reference_value);
        Ok(())
    }

    pub fn get_reference(&self, reference: &str) -> Option<Value> {
        let reference_variable = self.references.get(reference)?;
        (*reference_variable).try_into().ok()
    }

    pub fn run_standard(&mut self, source: &'a str) -> Result<Value, IsolateError> {
        self.bump.reset();
        let bump = self.get_bump();

        let tokens = self
            .lexer
            .tokenize(source)
            .map_err(|source| IsolateError::LexerError { source })?;

        let parser = Parser::try_new(tokens, bump)
            .map_err(|source| IsolateError::ParserError { source })?
            .standard();

        let ast = parser
            .parse()
            .map_err(|source| IsolateError::ParserError { source })?;

        let bytecode = self
            .compiler
            .compile(ast)
            .map_err(|source| IsolateError::CompilerError { source })?;

        let result = self
            .vm
            .run(
                bytecode,
                bump,
                self.environment.as_deref().unwrap_or(&Variable::Null),
            )
            .map_err(|source| IsolateError::VMError { source })?;

        result.try_into().map_err(|_| IsolateError::ValueCastError)
    }

    pub fn run_unary(&mut self, source: &'a str) -> Result<bool, IsolateError> {
        self.bump.reset();
        let bump = self.get_bump();

        let tokens = self
            .lexer
            .tokenize(source)
            .map_err(|source| IsolateError::LexerError { source })?;

        let parser = Parser::try_new(tokens, bump)
            .map_err(|source| IsolateError::ParserError { source })?
            .unary();

        let ast = parser
            .parse()
            .map_err(|source| IsolateError::ParserError { source })?;

        let bytecode = self
            .compiler
            .compile(ast)
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

    fn get_bump(&self) -> &'a Bump {
        unsafe { std::mem::transmute::<&Bump, &'a Bump>(&self.bump) }
    }

    fn get_reference_bump(&self) -> &'a Bump {
        unsafe { std::mem::transmute::<&Bump, &'a Bump>(&self.reference_bump) }
    }
}

/// Errors which happen within isolate or during evaluation
#[derive(Debug, Error)]
pub enum IsolateError {
    #[error("Lexer error")]
    LexerError { source: LexerError },

    #[error("Parser error")]
    ParserError { source: ParserError },

    #[error("Compiler error")]
    CompilerError { source: CompilerError },

    #[error("VM error")]
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
