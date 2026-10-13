//! Columnar input made what each row's evaluation sees once prepared, for
//! every row at once: declared dates converted, missing or null properties
//! given their default, and references replaced by the pool items they name.
//!
//! A row this cannot prepare exactly as the row path would (a date that
//! does not parse, a reference to an item not in its pool, a reference that
//! is null or not a string) is forced onto the row path, which reports or
//! keeps it as it always has.

use bumpalo::Bump;
use typed_arena::Arena;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::{DateValue, Variable};

use crate::policy::evaluator::EvalArtifact;
use crate::policy::ir::{Property, PropertyTypeIr};
use crate::policy::validator::InputSchema;

/// What prepared columns are made of, held for as long as they (and the
/// output read from them) are.
pub struct ColumnStore<'a> {
    bump: Bump,
    values: Arena<Vec<Variable>>,
    prepared: Arena<Prepared<'a>>,
}

impl Default for ColumnStore<'_> {
    fn default() -> Self {
        Self {
            bump: Bump::new(),
            values: Arena::new(),
            prepared: Arena::new(),
        }
    }
}

impl<'a> ColumnStore<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    fn values(&'a self, values: Vec<Variable>) -> &'a [Variable] {
        self.values.alloc(values).as_slice()
    }

    fn bits(&'a self, set: impl Iterator<Item = bool>, rows: usize) -> &'a [u64] {
        let words = self.bump.alloc_slice_fill_copy(rows.div_ceil(64).max(1), 0u64);
        for (row, set) in set.enumerate() {
            if set {
                words[row / 64] |= 1 << (row % 64);
            }
        }
        words
    }

    pub(crate) fn keep(&'a self, prepared: Prepared<'a>) -> &'a Prepared<'a> {
        self.prepared.alloc(prepared)
    }
}

pub(crate) struct Prepared<'a> {
    /// Dates converted and defaults filled: what a row is validated as.
    pub converted: Columns<'a>,
    /// And references replaced by their items: what a row is evaluated as.
    pub hydrated: Columns<'a>,
    /// Rows left to the row path.
    pub forced: Vec<bool>,
}

/// The prepared columns of `columns`, or `None` when their shape is one
/// this does not prepare (the row path then prepares every row).
pub(crate) fn prepare<'a>(artifact: &EvalArtifact, columns: &'a Columns<'a>, store: &'a ColumnStore<'a>) -> Option<Prepared<'a>> {
    let schema = &artifact.input_schema;
    if schema.converts_globals() {
        return None;
    }
    let rows = columns.rows;
    let mut forced = vec![false; rows];
    let mut converted = Columns::new(rows);
    for (path, column) in &columns.columns {
        let column = match path.split_once('.') {
            None if schema.is_pool(path) => pool(schema, path, column, store, &mut forced)?,
            None if schema.is_root(path) && schema.is_dated(path) => return None,
            None => *column,
            Some((root, rest)) if schema.is_root(root) => match schema.is_dated(root) {
                false => *column,
                true if rest.contains('.') => return None,
                true => match schema.model(root)?.properties.iter().find(|p| p.name.as_ref() == rest) {
                    None => *column,
                    Some(property) => property_column(schema, property, column, store, &mut forced, |row| row)?,
                },
            },
            Some((root, _)) if schema.is_pool(root) => return None,
            Some(_) => *column,
        };
        converted = converted.column(path, column);
    }
    defaults(schema, &mut converted, store);
    let mut hydrated = converted.clone();
    for field in &artifact.reference_fields {
        if field.array {
            return None;
        }
        hydrated = hydrate(&hydrated, &field.path, &field.target, store, &mut forced)?;
    }
    Some(Prepared {
        converted,
        hydrated,
        forced,
    })
}

/// A root's property column as converted: dates parsed (a row whose date
/// does not parse forced), other kinds as given. `row_of` maps a value's
/// index to its row (an item's to the row of its pool).
fn property_column<'a>(
    schema: &InputSchema,
    property: &Property,
    column: &Column<'a>,
    store: &'a ColumnStore<'a>,
    forced: &mut [bool],
    row_of: impl Fn(usize) -> usize,
) -> Option<Column<'a>> {
    match (&property.kind, property.array) {
        (PropertyTypeIr::Date, false) => {
            let values = (0..column.len())
                .map(|at| match column.variable(at) {
                    Variable::Null => Variable::Null,
                    // As the row path: parsed, else kept (an empty text is
                    // a valid date left as is; any other fails there).
                    Variable::String(text) => DateValue::from_text(&text).unwrap_or_else(|| {
                        if !text.is_empty() {
                            forced[row_of(at)] = true;
                        }
                        Variable::String(text)
                    }),
                    other => {
                        forced[row_of(at)] = true;
                        other
                    }
                })
                .collect();
            Some(Column {
                values: Values::Any(store.values(values)),
                validity: column.validity,
            })
        }
        (PropertyTypeIr::Date, true) => None,
        (PropertyTypeIr::Relationship { target }, _) if schema.is_dated(target) => None,
        _ => Some(*column),
    }
}

/// A pool (a list of items per row) with its items converted and their
/// defaults filled.
fn pool<'a>(schema: &InputSchema, target: &str, column: &Column<'a>, store: &'a ColumnStore<'a>, forced: &mut [bool]) -> Option<Column<'a>> {
    if !schema.is_dated(target) {
        return Some(*column);
    }
    let Values::List { offsets, child } = column.values else {
        return None;
    };
    let item = child.column();
    let Values::Struct { fields, len } = item.values else {
        return None;
    };
    let model = schema.model(target)?;
    // Each item's row, for the rows a conversion forces.
    let mut owner = vec![0usize; len];
    for row in 0..column.len() {
        let (a, b) = (offsets[row].max(0) as usize, offsets[row + 1].max(0) as usize);
        owner[a.min(len)..b.min(len)].iter_mut().for_each(|o| *o = row);
    }
    let mut converted: Vec<(&'a str, Column<'a>)> = Vec::with_capacity(fields.len());
    for (name, field) in fields {
        let field = match model.properties.iter().find(|p| p.name.as_ref() == *name) {
            Some(property) => property_column(schema, property, field, store, forced, |at| owner[at])?,
            None => *field,
        };
        converted.push((name, field));
    }
    for property in schema.defaults_of(target) {
        let default = schema.default_value(property)?;
        match converted.iter_mut().find(|(name, _)| *name == property.name.as_ref()) {
            Some((_, field)) => *field = filled(field, len, &default, None, store),
            None => {
                let name = store.bump.alloc_str(&property.name);
                let values = vec![default; len];
                converted.push((name, Column::new(Values::Any(store.values(values)))));
            }
        }
    }
    let fields = store.bump.alloc_slice_copy(&converted);
    let item = store.bump.alloc(Column {
        values: Values::Struct { fields, len },
        validity: item.validity,
    });
    Some(Column {
        values: Values::List {
            offsets,
            child: Dictionary::Column(item),
        },
        validity: column.validity,
    })
}

/// `column` with its missing (where `present`) and null values set to
/// `default`.
fn filled<'a>(column: &Column<'a>, len: usize, default: &Variable, present: Option<&[bool]>, store: &'a ColumnStore<'a>) -> Column<'a> {
    let values = (0..len)
        .map(|at| match column.variable(at) {
            Variable::Null if present.is_none_or(|p| p[at]) => default.clone(),
            value => value,
        })
        .collect();
    let validity = present.map(|present| {
        let bits = store.bits((0..len).map(|at| present[at] || column.valid(at)), len);
        (bits, 0)
    });
    Column {
        values: Values::Any(store.values(values)),
        validity,
    }
}

/// Each root entity's properties with a default, filled where the row has
/// the entity and the property is missing or null.
fn defaults<'a>(schema: &InputSchema, columns: &mut Columns<'a>, store: &'a ColumnStore<'a>) {
    let rows = columns.rows;
    let mut roots: Vec<&'a str> = columns
        .columns
        .iter()
        .map(|(path, _)| path.split_once('.').map_or(*path, |(root, _)| root))
        .filter(|root| schema.is_root(root))
        .collect();
    roots.sort_unstable();
    roots.dedup();
    for root in roots {
        let properties = schema.defaults_of(root);
        if properties.is_empty() {
            continue;
        }
        // A row has the entity when a column under it is given there.
        let present: Vec<bool> = (0..rows)
            .map(|row| {
                columns.columns.iter().any(|(path, column)| {
                    path.strip_prefix(root).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')) && column.valid(row)
                })
            })
            .collect();
        for property in properties {
            let Some(default) = schema.default_value(property) else {
                continue;
            };
            let path = store.bump.alloc_str(&format!("{root}.{}", property.name));
            let given = columns.find(path).map(|at| columns.columns[at].1);
            let column = match given {
                Some(column) => filled(&column, rows, &default, Some(&present), store),
                None => filled(&Column::new(Values::Any(store.values(vec![Variable::Null; rows]))), rows, &default, Some(&present), store),
            };
            match columns.find(path) {
                Some(at) => columns.columns[at].1 = column,
                None => columns.columns.push((path, column)),
            }
        }
    }
}

/// The columns with the reference at `path` replaced by the fields of the
/// `target` pool's item it names (its `id`; the last such item, as the row
/// path indexes them). A row whose reference is given but not found, not
/// a string, or null is forced.
fn hydrate<'a>(columns: &Columns<'a>, path: &str, target: &str, store: &'a ColumnStore<'a>, forced: &mut [bool]) -> Option<Columns<'a>> {
    let under = |p: &str| p.strip_prefix(path).is_some_and(|rest| rest.starts_with('.'));
    if columns.columns.iter().any(|(p, _)| under(p)) {
        return None;
    }
    let Some(at) = columns.find(path) else {
        // No row gives the reference.
        return Some(columns.clone());
    };
    let key = columns.columns[at].1;
    let pool = columns.find(target).map(|at| columns.columns[at].1);
    let items = pool.and_then(|pool| match pool.values {
        Values::List { child, .. } => match child {
            Dictionary::Column(item) => match item.values {
                Values::Struct { fields, .. } => Some((pool, item, fields)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    });
    if pool.is_some() && items.is_none() {
        return None;
    }
    let rows = columns.rows;
    let mut codes = vec![-1i32; rows];
    for row in 0..rows {
        // No reference: nothing to replace (nor to find).
        if !key.valid(row) {
            continue;
        }
        let found = match (key.variable(row), items) {
            (Variable::String(id), Some((pool, item, fields))) => {
                let ids = fields.iter().find(|(name, _)| *name == "id").map(|(_, c)| c);
                pool.range(row).and_then(|(a, b)| {
                    (a..b).rev().find(|&i| item.valid(i) && ids.is_some_and(|ids| ids.text(i) == Some(id.as_str())))
                })
            }
            _ => None,
        };
        match found.and_then(|i| i32::try_from(i).ok()) {
            Some(code) => codes[row] = code,
            None => forced[row] = true,
        }
    }
    let mut out = Columns::new(rows);
    for (i, (p, column)) in columns.columns.iter().enumerate() {
        if i != at {
            out = out.column(p, *column);
        }
    }
    let Some((_, _, fields)) = items else {
        return Some(out);
    };
    let keys = store.bump.alloc_slice_copy(&codes);
    for (name, field) in fields.iter() {
        let bits = store.bits(codes.iter().map(|&code| code >= 0 && field.valid(code as usize)), rows);
        let name = store.bump.alloc_str(&format!("{path}.{name}"));
        out = out.column(
            name,
            Column {
                values: Values::Dict {
                    keys,
                    values: Dictionary::Column(field),
                },
                validity: Some((bits, 0)),
            },
        );
    }
    Some(out)
}
