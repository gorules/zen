use super::{Bits, ColumnBuilder, Leaf, Records};
use zen_expression::lane::{Column, Values};
use zen_expression::Variable;

pub(crate) enum Json<'c, 'a> {
    Text(Column<'c>),
    Number(Column<'c>),
    List(&'c Records<'a>, Vec<(&'c str, &'c [u64], Json<'c, 'a>)>),
    Other(&'c Leaf<'a>),
}

impl<'c, 'a> Json<'c, 'a> {
    pub fn of(leaf: &'c Leaf<'a>) -> Self {
        if let Leaf::Records(records) = leaf {
            if let Some(labels) = records.labels() {
                return Json::List(
                    records,
                    records
                        .fields
                        .iter()
                        .zip(labels)
                        .map(|((_, leaf, present), label)| (&**label, &**present, Json::of(leaf)))
                        .collect(),
                );
            }
        }
        match leaf.scalars() {
            Some((column, true)) => Json::Text(column),
            Some((column, false)) => Json::Number(column),
            None => Json::Other(leaf),
        }
    }

    pub fn write(&self, row: usize, out: &mut Vec<u8>) -> bool {
        let mark = out.len();
        let written = match self {
            Json::Text(column) => match column.valid(row) {
                false => None,
                true => column
                    .text(row)
                    .map(|text| serde_json::to_writer(&mut *out, text).is_ok()),
            },
            Json::Number(column) => match (column.valid(row), column.values) {
                (false, _) => None,
                (true, Values::Scaled { mant, scale }) if scale.get(row) == Some(&0) => {
                    mant.get(row).map(|whole| {
                        out.extend_from_slice(itoa::Buffer::new().format(*whole).as_bytes());
                        true
                    })
                }
                (true, _) => column.number(row).map(|number| {
                    serde_json::to_writer(&mut *out, &Variable::Number(number)).is_ok()
                }),
            },
            Json::List(records, fields) => records.range(row).map(|(start, end)| {
                out.push(b'[');
                for item in start..end {
                    if item > start {
                        out.push(b',');
                    }
                    if records.nulled.as_ref().is_some_and(|n| Bits::get(n, item)) {
                        out.extend_from_slice(b"null");
                        continue;
                    }
                    out.push(b'{');
                    let mut any = false;
                    for (label, present, field) in fields {
                        if !Bits::get(present, item) {
                            continue;
                        }
                        if any {
                            out.push(b',');
                        }
                        any = true;
                        out.extend_from_slice(label.as_bytes());
                        field.write(item, out);
                    }
                    out.push(b'}');
                }
                out.push(b']');
                true
            }),
            Json::Other(leaf) => match leaf.get(row) {
                Variable::Null => None,
                value => Some(serde_json::to_writer(&mut *out, &value).is_ok()),
            },
        };
        match written {
            Some(true) => true,
            _ => {
                out.truncate(mark);
                match (self, written) {
                    (Json::Text(_) | Json::Number(_), Some(false) | None) if !self.null(row) => {
                        self.slow(row, out)
                    }
                    _ => {
                        out.extend_from_slice(b"null");
                        false
                    }
                }
            }
        }
    }

    fn null(&self, row: usize) -> bool {
        match self {
            Json::Text(column) | Json::Number(column) => ColumnBuilder::null_at(column, row),
            _ => true,
        }
    }

    fn slow(&self, row: usize, out: &mut Vec<u8>) -> bool {
        let value = match self {
            Json::Text(column) | Json::Number(column) => column.variable(row),
            _ => Variable::Null,
        };
        match value {
            Variable::Null => {
                out.extend_from_slice(b"null");
                false
            }
            value => serde_json::to_writer(&mut *out, &value).is_ok(),
        }
    }
}
