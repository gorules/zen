use crate::variable::{DynamicVariable, RcCell};
use crate::Variable;
use ahash::HashMap;
use anyhow::Context;
use rust_decimal::Decimal;
use std::ops::Deref;
use std::rc::Rc;

pub struct Arguments<'a>(pub &'a [Variable]);

impl<'a> Deref for Arguments<'a> {
    type Target = [Variable];

    fn deref(&self) -> &'a Self::Target {
        &self.0
    }
}

impl<'a> Arguments<'a> {
    pub fn ovar(&self, pos: usize) -> Option<&'a Variable> {
        self.0.get(pos)
    }

    pub fn var(&self, pos: usize) -> anyhow::Result<&'a Variable> {
        self.ovar(pos)
            .with_context(|| format!("Argument on {pos} position out of bounds"))
    }

    pub fn obool(&self, pos: usize) -> anyhow::Result<Option<bool>> {
        match self.ovar(pos) {
            Some(v) => v
                .as_bool()
                .map(Some)
                .with_context(|| format!("Argument on {pos} is not a bool")),
            None => Ok(None),
        }
    }

    pub fn bool(&self, pos: usize) -> anyhow::Result<bool> {
        self.obool(pos)?
            .with_context(|| format!("Argument on {pos} position is not a valid bool"))
    }

    pub fn ostr(&self, pos: usize) -> anyhow::Result<Option<&'a str>> {
        match self.ovar(pos) {
            Some(v) => v
                .as_str()
                .map(Some)
                .with_context(|| format!("Argument on {pos} is not a string")),
            None => Ok(None),
        }
    }

    pub fn str(&self, pos: usize) -> anyhow::Result<&'a str> {
        self.ostr(pos)?
            .with_context(|| format!("Argument on {pos} position is not a valid string"))
    }

    pub fn onumber(&self, pos: usize) -> anyhow::Result<Option<Decimal>> {
        match self.ovar(pos) {
            Some(v) => v
                .as_number()
                .map(Some)
                .with_context(|| format!("Argument on {pos} is not a number")),
            None => Ok(None),
        }
    }

    pub fn number(&self, pos: usize) -> anyhow::Result<Decimal> {
        self.onumber(pos)?
            .with_context(|| format!("Argument on {pos} position is not a valid number"))
    }

    pub fn oarray(&self, pos: usize) -> anyhow::Result<Option<RcCell<Vec<Variable>>>> {
        match self.ovar(pos) {
            Some(v) => v
                .as_array()
                .map(Some)
                .with_context(|| format!("Argument on {pos} is not a array")),
            None => Ok(None),
        }
    }

    pub fn array(&self, pos: usize) -> anyhow::Result<RcCell<Vec<Variable>>> {
        self.oarray(pos)?
            .with_context(|| format!("Argument on {pos} position is not a valid array"))
    }

    pub fn oobject(
        &self,
        pos: usize,
    ) -> anyhow::Result<Option<RcCell<HashMap<Rc<str>, Variable>>>> {
        match self.ovar(pos) {
            Some(v) => v
                .as_object()
                .map(Some)
                .with_context(|| format!("Argument on {pos} is not a object")),
            None => Ok(None),
        }
    }

    pub fn object(&self, pos: usize) -> anyhow::Result<RcCell<HashMap<Rc<str>, Variable>>> {
        self.oobject(pos)?
            .with_context(|| format!("Argument on {pos} position is not a valid object"))
    }

    pub fn odynamic<T: DynamicVariable + 'static>(&self, pos: usize) -> anyhow::Result<Option<&T>> {
        match self.ovar(pos) {
            None => Ok(None),
            Some(s) => Ok(s.dynamic::<T>()),
        }
    }

    pub fn dynamic<T: DynamicVariable + 'static>(&self, pos: usize) -> anyhow::Result<&T> {
        self.odynamic(pos)?
            .with_context(|| format!("Argument on {pos} position is not a valid dynamic"))
    }
}
