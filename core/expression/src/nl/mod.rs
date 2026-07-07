pub(crate) mod project;
pub mod token;

pub use token::{EditHint, EnumOption, NlToken, NlTokenKind, OpChoice, OpSym, TypeTag, WordSym};

use serde::Serialize;

use crate::intellisense::diagnostic::Diagnostic;
use crate::variable::VariableType;

pub fn encode_string(value: &str) -> Option<String> {
    if !value.contains('"') {
        Some(format!("\"{value}\""))
    } else if !value.contains('\'') {
        Some(format!("'{value}'"))
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct NlRequest {
    pub id: String,
    pub expression: String,
    pub unary: bool,
    pub subject_type: Option<VariableType>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NlResult {
    pub id: String,
    pub tokens: Vec<NlToken>,
    pub enums: Vec<Vec<EnumOption>>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip)]
    pub subject_type: Option<VariableType>,
}
