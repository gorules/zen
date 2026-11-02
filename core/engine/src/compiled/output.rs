use crate::compiled::leaf::Leaf;
use std::sync::Arc;
use zen_expression::lane::Column;
use zen_expression::Variable;

pub struct OutputColumn<'a>(pub(crate) Leaf<'a>);

pub struct RecordsView<'c> {
    pub offsets: &'c [i32],
    pub valid: Vec<u64>,
    pub fields: Vec<(&'c str, Column<'c>, &'c [u64])>,
}

impl OutputColumn<'_> {
    pub fn column(&self) -> Column<'_> {
        self.0.column()
    }

    pub fn records(&self) -> Option<RecordsView<'_>> {
        let records = match &self.0 {
            Leaf::Records(records) => records,
            _ => return None,
        };
        Some(RecordsView {
            offsets: records.offsets(),
            valid: records.validity().to_vec(),
            fields: records
                .fields()
                .iter()
                .map(|(key, leaf, present)| (key.as_str(), leaf.column(), present.as_ref()))
                .collect(),
        })
    }

    pub fn get(&self, row: usize) -> Variable {
        self.0.get(row)
    }

    pub(crate) fn row(columns: &[(Arc<str>, OutputColumn)], row: usize) -> Variable {
        let object = Variable::empty_object();
        for (path, column) in columns {
            match (path.is_empty(), column.get(row)) {
                (_, Variable::Null) => {}
                (true, value) => return value,
                (false, value) => {
                    object.dot_insert(path, value);
                }
            }
        }
        object
    }
}
