use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use thiserror::Error;
use zen_expression::IsolateError;

#[derive(Debug, Error)]
pub enum TemplateRenderError {
    #[error("isolate error: {0}")]
    IsolateError(IsolateError),

    #[error("parser error: {0}")]
    ParserError(ParserError),
}

impl From<IsolateError> for TemplateRenderError {
    fn from(value: IsolateError) -> Self {
        Self::IsolateError(value)
    }
}

impl From<ParserError> for TemplateRenderError {
    fn from(value: ParserError) -> Self {
        Self::ParserError(value)
    }
}

impl Serialize for TemplateRenderError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            TemplateRenderError::IsolateError(isolate) => isolate.serialize(serializer),
            TemplateRenderError::ParserError(parser) => parser.serialize(serializer),
        }
    }
}

#[derive(Debug, Error)]
pub enum ParserError {
    #[error("Open bracket")]
    OpenBracket,

    #[error("Close bracket")]
    CloseBracket,
}

impl Serialize for ParserError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;

        map.serialize_entry("type", "templateParserError")?;

        match self {
            ParserError::OpenBracket => map.serialize_entry("value", "openBracket")?,
            ParserError::CloseBracket => map.serialize_entry("value", "closeBracket")?,
        }

        map.end()
    }
}
