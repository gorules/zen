use crate::compiler::Opcode;
use crate::vm::VM;
use crate::{IsolateError, Variable};
use std::marker::PhantomData;
use std::sync::Arc;

pub struct Standard;
pub struct Unary;

pub enum ExpressionKind {
    Standard,
    Unary,
}

/// Compiled expression
pub struct Expression<Kind> {
    bytecode: Arc<Vec<Opcode>>,
    _marker: PhantomData<Kind>,
}

impl<Kind> Expression<Kind> {
    pub fn bytecode(&self) -> &Arc<Vec<Opcode>> {
        &self.bytecode
    }
}

impl Expression<Standard> {
    pub fn new_standard(bytecode: Arc<Vec<Opcode>>) -> Self {
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
        self.evaluate_in(context, &mut vm)
    }

    pub fn evaluate_in(&self, context: Variable, vm: &mut VM) -> Result<Variable, IsolateError> {
        let output = vm.run(self.bytecode.as_slice(), context)?;
        Ok(output)
    }
}

impl Expression<Unary> {
    pub fn new_unary(bytecode: Arc<Vec<Opcode>>) -> Self {
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
        self.evaluate_in(context, &mut vm)
    }

    pub fn evaluate_in(&self, context: Variable, vm: &mut VM) -> Result<bool, IsolateError> {
        let Some(context_object_ref) = context.as_object() else {
            return Err(IsolateError::MissingContextReference);
        };

        let context_object = context_object_ref.borrow();
        if !context_object.contains_key("$") {
            return Err(IsolateError::MissingContextReference);
        }

        let output = vm
            .run(self.bytecode.as_slice(), context)?
            .as_bool()
            .ok_or_else(|| IsolateError::ValueCastError)?;
        Ok(output)
    }
}
