use std::cell::{RefCell, UnsafeCell};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::mem::ManuallyDrop;
use std::ops::DerefMut;
use std::rc::Rc;

use ahash::AHasher;
use bumpalo::Bump;
use serde_json::Value;
use thiserror::Error;

use crate::lexer::error::LexerError;
use crate::lexer::Lexer;
use crate::parser::error::ParserError;
use crate::parser::{StandardParser, UnaryParser};

use crate::compiler::{Compiler, CompilerError};
use crate::lexer::token::TokenKind;
use crate::opcodes::{Opcode, Variable};
use crate::vm::{Scope, VMError, VM};

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

#[derive(Debug)]
pub struct Isolate<'a> {
    lexer: Lexer<'a>,
    bump: RefCell<Bump>,
    reference_bump: Bump,
    bytecode: Rc<UnsafeCell<Vec<&'a Opcode<'a>>>>,
    stack: UnsafeCell<Vec<&'a Variable<'a>>>,
    scopes: UnsafeCell<Vec<Scope<'a>>>,
    environment: UnsafeCell<ManuallyDrop<Variable<'a>>>,
    references: RefCell<HashMap<&'a str, &'a Variable<'a>, ADefHasher>>,
}

impl<'a> Default for Isolate<'a> {
    fn default() -> Self {
        Self {
            lexer: Lexer::new(),
            bump: Default::default(),
            reference_bump: Default::default(),
            bytecode: Default::default(),
            stack: Default::default(),
            scopes: Default::default(),
            environment: UnsafeCell::new(ManuallyDrop::new(Variable::Null)),
            references: Default::default(),
        }
    }
}

impl<'a> Isolate<'a> {
    pub fn inject_env(&self, value: &Value) {
        let new_env = Variable::from_serde(value, self.get_reference_bump());
        let env = unsafe { &mut (*self.environment.get()) };
        *env = ManuallyDrop::new(new_env);
    }

    fn get_bump(&self) -> &'a Bump {
        unsafe { std::mem::transmute::<&Bump, &'a Bump>(&self.bump.borrow()) }
    }

    fn get_reference_bump(&self) -> &'a Bump {
        unsafe { std::mem::transmute(&self.reference_bump) }
    }

    pub fn set_reference(&self, reference: &'a str) -> Result<(), IsolateError> {
        let mut references = self.references.borrow_mut();
        let bump = self.get_reference_bump();

        if !references.contains_key(reference) {
            let result = self.run_standard(reference)?;
            let value = bump.alloc(Variable::from_serde(&result, bump));

            references.insert(reference, value);
        }

        let value = references
            .get(reference)
            .ok_or(IsolateError::ReferenceError)?;

        let env_w = unsafe { &mut (*self.environment.get()) };
        let env = env_w.deref_mut();
        match env {
            Variable::Object(..) => {
                //
            }
            _ => {
                *env = Variable::empty_object_in(bump);
            }
        }

        if let Variable::Object(obj) = env {
            obj.insert("$", value);
        }

        Ok(())
    }

    pub fn get_reference(&self, reference: &str) -> Option<Value> {
        let refs = self.references.borrow();
        let var = refs.get(reference)?;

        (*var).try_into().ok()
    }

    pub fn run_standard(&self, source: &'a str) -> Result<Value, IsolateError> {
        self.clear();

        let tokens = self
            .lexer
            .tokenize(source)
            .map_err(|source| IsolateError::LexerError { source })?;

        let tkn = tokens.borrow();
        let bump = self.get_bump();

        let parser = StandardParser::try_new(tkn.as_ref(), bump)
            .map_err(|source| IsolateError::ParserError { source })?;

        let ast = parser
            .parse()
            .map_err(|source| IsolateError::ParserError { source })?;

        let compiler = Compiler::new(ast, self.bytecode.clone(), bump);
        compiler
            .compile()
            .map_err(|source| IsolateError::CompilerError { source })?;

        let mut vm = unsafe {
            VM::new(
                &*self.bytecode.get(),
                &mut *self.stack.get(),
                &mut *self.scopes.get(),
                bump,
            )
        };

        let res = vm
            .run(unsafe { &*self.environment.get() })
            .map_err(|source| IsolateError::VMError { source })?;

        res.try_into().map_err(|_| IsolateError::ValueCastError)
    }

    /// Runs unary test
    /// If reference identifier is present ($) it will use standard parser
    pub fn run_unary(&self, source: &'a str) -> Result<Value, IsolateError> {
        self.clear();

        let tokens = self
            .lexer
            .tokenize(source)
            .map_err(|source| IsolateError::LexerError { source })?;

        let tkn = tokens.borrow();
        let unary_disallowed = tkn
            .iter()
            .any(|token| token.kind == TokenKind::Identifier && token.value == "$");

        let bump = self.get_bump();
        let ast = match unary_disallowed {
            true => {
                let parser = StandardParser::try_new(tkn.as_ref(), bump)
                    .map_err(|source| IsolateError::ParserError { source })?;

                parser
                    .parse()
                    .map_err(|source| IsolateError::ParserError { source })?
            }
            false => {
                let parser = UnaryParser::try_new(tkn.as_ref(), bump)
                    .map_err(|source| IsolateError::ParserError { source })?;

                parser
                    .parse()
                    .map_err(|source| IsolateError::ParserError { source })?
            }
        };

        let compiler = Compiler::new(ast, self.bytecode.clone(), bump);
        compiler
            .compile()
            .map_err(|source| IsolateError::CompilerError { source })?;

        let mut vm = unsafe {
            VM::new(
                &*self.bytecode.get(),
                &mut *self.stack.get(),
                &mut *self.scopes.get(),
                bump,
            )
        };

        let res = vm
            .run(unsafe { &*self.environment.get() })
            .map_err(|source| IsolateError::VMError { source })?;

        res.try_into().map_err(|_| IsolateError::ValueCastError)
    }

    fn clear(&self) {
        self.bump.borrow_mut().reset();
        unsafe {
            (*self.bytecode.get()).clear();
            (*self.stack.get()).clear();
            (*self.scopes.get()).clear();
        }
    }
}
