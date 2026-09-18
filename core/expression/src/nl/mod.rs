pub(crate) mod project;
pub mod token;

pub use token::{EditHint, EnumOption, NlToken, NlTokenKind, OpChoice, OpSym, TypeTag, WordSym};

use serde::Serialize;
use std::rc::Rc;

use crate::intellisense::diagnostic::Diagnostic;
use crate::intellisense::NlLabelResolver;
use crate::variable::VariableType;

pub use crate::slot::encode_string;

pub(crate) fn enum_options(
    name: Option<&str>,
    values: &[Rc<str>],
    labels: Option<&NlLabelResolver>,
) -> Vec<EnumOption> {
    crate::slot::enum_options(name, values, labels)
        .into_iter()
        .map(|o| EnumOption {
            label: o.label,
            source: o.source,
        })
        .collect()
}

pub(crate) fn subject_enum_options(
    subject: &VariableType,
    labels: Option<&NlLabelResolver>,
) -> Option<Vec<EnumOption>> {
    match subject {
        VariableType::Enum(name, values) => Some(enum_options(name.as_deref(), values, labels)),
        VariableType::Nullable(inner) | VariableType::Array(inner) => {
            subject_enum_options(inner, labels)
        }
        _ => None,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_options: Option<Vec<EnumOption>>,
}
