use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};

use ahash::AHasher;
use bumpalo::Bump;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::compiler::{Compiler, CompilerError};
use crate::lexer::error::LexerError;
use crate::lexer::token::TokenKind;
use crate::lexer::Lexer;
use crate::opcodes::Variable;
use crate::parser::error::ParserError;
use crate::parser::parser::Parser;
use crate::vm::{VMError, VM};

type ADefHasher = BuildHasherDefault<AHasher>;

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

#[derive(Debug)]
pub struct Isolate<'arena> {
    lexer: Lexer<'arena>,
    compiler: Compiler<'arena>,
    vm: VM<'arena>,

    bump: Bump,
    reference_bump: Bump,

    environment: UnsafeCell<ManuallyDrop<Variable<'arena>>>,
    references: HashMap<&'arena str, &'arena Variable<'arena>, ADefHasher>,
}

impl<'a> Default for Isolate<'a> {
    fn default() -> Self {
        Self {
            lexer: Lexer::new(),
            compiler: Compiler::new(),
            vm: VM::new(),

            bump: Default::default(),
            reference_bump: Default::default(),

            environment: UnsafeCell::new(ManuallyDrop::new(Variable::Null)),
            references: Default::default(),
        }
    }
}

impl<'a> Isolate<'a> {
    pub fn inject_env(&mut self, value: &Value) {
        let new_environment = Variable::from_serde(value, self.get_reference_bump());
        let current_environment = self.environment.get_mut();

        *current_environment = ManuallyDrop::new(new_environment);
    }

    fn get_bump(&self) -> &'a Bump {
        unsafe { std::mem::transmute::<&Bump, &'a Bump>(&self.bump) }
    }

    fn get_reference_bump(&self) -> &'a Bump {
        unsafe { std::mem::transmute::<&Bump, &'a Bump>(&self.reference_bump) }
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

        let environment = self.environment.get_mut();
        let environment_ref = ManuallyDrop::deref(environment);
        if !matches!(environment_ref, Variable::Object(_)) {
            let _ = std::mem::replace(
                environment,
                ManuallyDrop::new(Variable::empty_object_in(bump)),
            );
        }

        let environment_mut_ref = ManuallyDrop::deref_mut(environment);
        let Variable::Object(environment_object) = environment_mut_ref else {
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
            .run(bytecode, bump, self.environment.get_mut())
            .map_err(|source| IsolateError::VMError { source })?;

        result.try_into().map_err(|_| IsolateError::ValueCastError)
    }

    pub fn run_unary(&mut self, source: &'a str) -> Result<Value, IsolateError> {
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
            .run(bytecode, bump, &self.environment.get_mut())
            .map_err(|source| IsolateError::VMError { source })?;

        result.try_into().map_err(|_| IsolateError::ValueCastError)
    }
}
