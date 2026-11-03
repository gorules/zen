pub(crate) mod graph;
pub(super) mod policy;
mod table;

pub(crate) use table::RowTable;
use zen_expression::Variable;

pub(crate) struct Failure;

impl Failure {
    pub const EXPRESSION: &'static str = "Failed to evaluate expression";

    pub fn expression(source: &str) -> String {
        format!(r#"{}: "{source}""#, Self::EXPRESSION)
    }

    pub fn input(error: impl std::fmt::Display) -> String {
        format!("{}: {error}", Self::EXPRESSION)
    }
}

pub(crate) struct Attributes;

impl Attributes {
    pub fn item(pass_through: bool, item: &Variable, output: Variable) -> Variable {
        match pass_through {
            true => item.clone().merge_clone(&output),
            false => output,
        }
    }

    pub fn finish(
        output_path: Option<&str>,
        pass_through: bool,
        input: &Variable,
        output: Variable,
    ) -> Variable {
        let output = match output_path {
            Some(path) => {
                let wrapped = Variable::empty_object();
                wrapped.dot_insert(path, output);
                wrapped
            }
            None => output,
        };
        match pass_through {
            true => input.clone().merge_clone(&output),
            false => output,
        }
    }
}
