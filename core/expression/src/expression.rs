use crate::compiler::Opcode;
use crate::vm::VM;
use crate::{IsolateError, Variable};
use std::marker::PhantomData;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Standard;

#[derive(Debug, Clone)]
pub struct Unary;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ExpressionKind {
    Standard,
    Unary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpcodeCache {
    pub standard: ahash::HashMap<Arc<str>, Arc<[Opcode]>>,
    pub unary: ahash::HashMap<Arc<str>, Arc<[Opcode]>>,
}

impl OpcodeCache {
    pub fn new() -> Self {
        Self {
            standard: Default::default(),
            unary: Default::default(),
        }
    }
}

/// Compiled expression
#[derive(Debug, Clone)]
pub struct Expression<Kind> {
    bytecode: Arc<[Opcode]>,
    _marker: PhantomData<Kind>,
}

impl<Kind> Expression<Kind> {
    pub fn bytecode(&self) -> &Arc<[Opcode]> {
        &self.bytecode
    }
}

impl Expression<Standard> {
    pub fn new_standard(bytecode: Arc<[Opcode]>) -> Self {
        Expression {
            bytecode,
            _marker: PhantomData,
        }
    }

    pub fn kind(&self) -> ExpressionKind {
        ExpressionKind::Standard
    }

    pub fn evaluate(&self, context: Variable) -> Result<Variable, IsolateError> {
        let mut vm = VM::new();
        self.evaluate_with(context, &mut vm)
    }

    pub fn evaluate_with(&self, context: Variable, vm: &mut VM) -> Result<Variable, IsolateError> {
        let output = vm.run(self.bytecode.as_ref(), context)?;
        Ok(output)
    }
}

impl Expression<Unary> {
    pub fn new_unary(bytecode: Arc<[Opcode]>) -> Self {
        Expression {
            bytecode,
            _marker: PhantomData,
        }
    }

    pub fn kind(&self) -> ExpressionKind {
        ExpressionKind::Unary
    }

    pub fn evaluate(&self, context: Variable) -> Result<bool, IsolateError> {
        let mut vm = VM::new();
        self.evaluate_with(context, &mut vm)
    }

    pub fn evaluate_with(&self, context: Variable, vm: &mut VM) -> Result<bool, IsolateError> {
        let Some(context_object_ref) = context.as_object() else {
            return Err(IsolateError::MissingContextReference);
        };

        let context_object = context_object_ref.borrow();
        if !context_object.contains_key("$") {
            return Err(IsolateError::MissingContextReference);
        }

        let output = vm
            .run(self.bytecode.as_ref(), context)?
            .as_bool()
            .ok_or_else(|| IsolateError::ValueCastError)?;
        Ok(output)
    }
}
