use crate::variable::RcCell;
use crate::Variable;
use ahash::HashMap;
use anyhow::Context;
use rust_decimal::Decimal;
use std::ops::Deref;

pub struct Arguments<'a>(pub &'a [Variable]);

impl<'a> Deref for Arguments<'a> {
    type Target = [Variable];

    fn deref(&self) -> &'a Self::Target {
        &self.0
    }
}

// TODO: Optional vars need to crash if different type is provided (instead of Option, we should do Result<Option<_>, _>
impl<'a> Arguments<'a> {
    pub fn ovar(&self, pos: usize) -> Option<&'a Variable> {
        self.0.get(pos)
    }

    pub fn var(&self, pos: usize) -> anyhow::Result<&'a Variable> {
        self.ovar(pos)
            .with_context(|| format!("Argument on {pos} position out of bounds"))
    }

    pub fn obool(&self, pos: usize) -> Option<bool> {
        self.ovar(pos).and_then(|v| v.as_bool())
    }

    pub fn bool(&self, pos: usize) -> anyhow::Result<bool> {
        self.obool(pos)
            .with_context(|| format!("Argument on {pos} position is not a valid bool"))
    }

    pub fn ostr(&self, pos: usize) -> Option<&'a str> {
        self.ovar(pos).and_then(|v| v.as_str())
    }

    pub fn str(&self, pos: usize) -> anyhow::Result<&'a str> {
        self.ostr(pos)
            .with_context(|| format!("Argument on {pos} position is not a valid string"))
    }

    pub fn onumber(&self, pos: usize) -> Option<Decimal> {
        self.ovar(pos).and_then(|v| v.as_number())
    }

    pub fn number(&self, pos: usize) -> anyhow::Result<Decimal> {
        self.onumber(pos)
            .with_context(|| format!("Argument on {pos} position is not a valid number"))
    }

    pub fn oarray(&self, pos: usize) -> Option<RcCell<Vec<Variable>>> {
        self.ovar(pos).and_then(|v| v.as_array())
    }

    pub fn array(&self, pos: usize) -> anyhow::Result<RcCell<Vec<Variable>>> {
        self.oarray(pos)
            .with_context(|| format!("Argument on {pos} position is not a valid array"))
    }

    pub fn oobject(&self, pos: usize) -> Option<RcCell<HashMap<String, Variable>>> {
        self.ovar(pos).and_then(|v| v.as_object())
    }

    pub fn object(&self, pos: usize) -> anyhow::Result<RcCell<HashMap<String, Variable>>> {
        self.oobject(pos)
            .with_context(|| format!("Argument on {pos} position is not a valid object"))
    }
}
