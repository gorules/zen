pub mod zen;

use zen_expression::variable::Variable;
use zen_expression::ToVariable;

#[derive(Debug, Clone, ToVariable)]
pub(crate) enum RowOutputKind {
    Variable(Variable),
}

#[derive(Debug, Default, ToVariable)]
pub(crate) struct RowOutput {
    output: OutputMap,
}

type OutputMap = Vec<(String, RowOutputKind)>;

impl RowOutput {
    pub fn push<K: Into<String>>(&mut self, key: K, value: RowOutputKind) {
        self.output.push((key.into(), value))
    }

    pub async fn to_json(&self) -> Variable {
        let object = Variable::empty_object();

        for (key, kind) in &self.output {
            match kind {
                RowOutputKind::Variable(variable) => {
                    object.dot_insert(key.as_str(), variable.clone());
                }
            }
        }

        object
    }
}
