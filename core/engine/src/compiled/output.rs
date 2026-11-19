use crate::compiled::leaf::Leaf;
use std::sync::Arc;
use zen_expression::lane::Column;
use zen_expression::Variable;

pub struct OutputColumn<'a>(pub(crate) Leaf<'a>);

enum Part {
    Leaf(Box<str>, usize),
    Branch(Box<str>, Vec<Part>),
}

pub(crate) struct Layout(Option<Vec<Part>>);

impl Layout {
    pub(crate) fn new(columns: &[(Arc<str>, OutputColumn)]) -> Self {
        let paths: Vec<Vec<&str>> = columns
            .iter()
            .map(|(path, _)| path.split('.').collect())
            .collect();
        let plain = columns.iter().all(|(path, _)| !path.is_empty());
        let all: Vec<usize> = (0..paths.len()).collect();
        Layout(match plain {
            true => Self::parts(&paths, &all, 0),
            false => None,
        })
    }

    fn label(key: &str) -> Box<str> {
        let mut text = serde_json::to_string(key).unwrap_or_default();
        text.push(':');
        text.into_boxed_str()
    }

    fn parts(paths: &[Vec<&str>], items: &[usize], depth: usize) -> Option<Vec<Part>> {
        let mut parts: Vec<Part> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        let mut start = 0;
        while start < items.len() {
            let key = *paths[items[start]].get(depth)?;
            let end = (start..items.len())
                .find(|&at| paths[items[at]].get(depth) != Some(&key))
                .unwrap_or(items.len());
            if seen.contains(&key) {
                return None;
            }
            seen.push(key);
            let group = &items[start..end];
            let leaf = group.iter().any(|&index| paths[index].len() == depth + 1);
            match (leaf, group) {
                (true, [index]) => parts.push(Part::Leaf(Self::label(key), *index)),
                (true, _) => return None,
                (false, _) => parts.push(Part::Branch(
                    Self::label(key),
                    Self::parts(paths, group, depth + 1)?,
                )),
            }
            start = end;
        }
        Some(parts)
    }

    fn fill(
        parts: &[Part],
        columns: &[(Arc<str>, OutputColumn)],
        row: usize,
        out: &mut Vec<u8>,
    ) -> bool {
        let mut any = false;
        for part in parts {
            let mark = out.len();
            if any {
                out.push(b',');
            }
            let written = match part {
                Part::Leaf(label, index) => {
                    out.extend_from_slice(label.as_bytes());
                    columns[*index].1 .0.write_json(row, out)
                }
                Part::Branch(label, children) => {
                    out.extend_from_slice(label.as_bytes());
                    out.push(b'{');
                    let filled = Self::fill(children, columns, row, out);
                    out.push(b'}');
                    filled
                }
            };
            match written {
                true => any = true,
                false => out.truncate(mark),
            }
        }
        any
    }

    pub(crate) fn write(
        &self,
        columns: &[(Arc<str>, OutputColumn)],
        row: usize,
        out: &mut Vec<u8>,
    ) {
        match &self.0 {
            Some(parts) => {
                out.push(b'{');
                Self::fill(parts, columns, row, out);
                out.push(b'}');
            }
            None => {
                let _ = serde_json::to_writer(&mut *out, &OutputColumn::row(columns, row));
            }
        }
    }
}

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
