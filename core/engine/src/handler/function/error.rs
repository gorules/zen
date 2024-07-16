use std::fmt::{Display, Formatter};

use rquickjs::{CaughtError, Ctx, Error, Exception};

pub type FunctionResult<Ok = ()> = Result<Ok, FunctionError>;

#[derive(Debug)]
pub enum FunctionError {
    Caught(String),
    Runtime(Error),
}

impl<'js> From<CaughtError<'js>> for FunctionError {
    fn from(value: CaughtError<'js>) -> Self {
        Self::Caught(value.to_string())
    }
}

impl From<Error> for FunctionError {
    fn from(value: Error) -> Self {
        Self::Runtime(value)
    }
}

impl Display for FunctionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionError::Caught(c) => f.write_str(c.as_str()),
            FunctionError::Runtime(rt) => rt.fmt(f),
        }
    }
}

pub trait ResultExt<T> {
    #[allow(dead_code)]
    fn or_throw_msg(self, ctx: &Ctx, msg: &str) -> rquickjs::Result<T>;
    fn or_throw(self, ctx: &Ctx) -> rquickjs::Result<T>;
}

impl<T, E: Display> ResultExt<T> for Result<T, E> {
    fn or_throw_msg(self, ctx: &Ctx, msg: &str) -> rquickjs::Result<T> {
        self.map_err(|_| {
            let mut message = String::with_capacity(100);
            message.push_str(msg);
            message.push_str(".");
            Exception::throw_message(ctx, &message)
        })
    }

    fn or_throw(self, ctx: &Ctx) -> rquickjs::Result<T> {
        self.map_err(|err| Exception::throw_message(ctx, &err.to_string()))
    }
}

impl<T> ResultExt<T> for Option<T> {
    fn or_throw_msg(self, ctx: &Ctx, msg: &str) -> rquickjs::Result<T> {
        self.ok_or(Exception::throw_message(ctx, msg))
    }

    fn or_throw(self, ctx: &Ctx) -> rquickjs::Result<T> {
        self.ok_or(Exception::throw_message(ctx, "Value is not present"))
    }
}
