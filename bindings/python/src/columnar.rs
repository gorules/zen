use std::sync::Arc;

use anyhow::anyhow;
use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, Int8Type};
use arrow_array::{Array, RecordBatch};
use arrow_schema::DataType;
use rust_decimal::Decimal;
use zen_engine::Variable;
use zen_expression::variable::VariableMap;

use crate::data::insert_path;

pub fn inputs_from_batch(batch: &RecordBatch) -> Result<Vec<Variable>, anyhow::Error> {
    let rows = batch.num_rows();
    let schema = batch.schema();

    let mut columns: Vec<(Vec<String>, Vec<Option<Variable>>)> = Vec::new();
    for (index, field) in schema.fields().iter().enumerate() {
        let name = field.name();
        let segments = name.split('.').map(str::to_string).collect();
        columns.push((segments, column_values(batch.column(index), name)?));
    }

    Ok((0..rows)
        .map(|row| {
            let mut map = VariableMap::default();
            for (segments, values) in &columns {
                if let Some(value) = &values[row] {
                    insert_path(&mut map, segments, value.clone());
                }
            }
            Variable::from_object(map)
        })
        .collect())
}

fn column_values(
    column: &Arc<dyn Array>,
    name: &str,
) -> Result<Vec<Option<Variable>>, anyhow::Error> {
    let rows = column.len();
    let values = match column.data_type() {
        DataType::Null => vec![None; rows],
        DataType::Boolean => {
            let array = column.as_boolean();
            (0..rows)
                .map(|row| (!array.is_null(row)).then(|| Variable::Bool(array.value(row))))
                .collect()
        }
        DataType::Int8 => int_values(column.as_primitive::<Int8Type>(), |v| v as i64),
        DataType::Int16 => int_values(column.as_primitive::<Int16Type>(), |v| v as i64),
        DataType::Int32 => int_values(column.as_primitive::<Int32Type>(), |v| v as i64),
        DataType::Int64 => int_values(column.as_primitive::<Int64Type>(), |v| v),
        DataType::Float32 => float_values(column.as_primitive::<Float32Type>(), |v| v as f64),
        DataType::Float64 => float_values(column.as_primitive::<Float64Type>(), |v| v),
        DataType::Utf8 => {
            let array = column.as_string::<i32>();
            (0..rows)
                .map(|row| {
                    (!array.is_null(row)).then(|| Variable::String(array.value(row).into()))
                })
                .collect()
        }
        DataType::LargeUtf8 => {
            let array = column.as_string::<i64>();
            (0..rows)
                .map(|row| {
                    (!array.is_null(row)).then(|| Variable::String(array.value(row).into()))
                })
                .collect()
        }
        DataType::Dictionary(_, value_type) if **value_type == DataType::Utf8 => {
            let array = column.as_any_dictionary();
            let dictionary = array
                .values()
                .as_string_opt::<i32>()
                .ok_or_else(|| anyhow!("column '{name}': unsupported dictionary values"))?;
            let interned: Vec<Variable> = (0..dictionary.len())
                .map(|index| Variable::String(dictionary.value(index).into()))
                .collect();
            let keys = array.normalized_keys();
            (0..rows)
                .map(|row| (!array.is_null(row)).then(|| interned[keys[row]].clone()))
                .collect()
        }
        DataType::Struct(fields) => {
            let array = column.as_struct();
            let mut children: Vec<(String, Vec<Option<Variable>>)> =
                Vec::with_capacity(fields.len());
            for (field, child) in fields.iter().zip(array.columns()) {
                children.push((field.name().clone(), column_values(child, field.name())?));
            }
            (0..rows)
                .map(|row| {
                    if array.is_null(row) {
                        return None;
                    }
                    let mut map = VariableMap::with_capacity(children.len());
                    for (key, values) in &children {
                        if let Some(value) = &values[row] {
                            map.insert(key.as_str().into(), value.clone());
                        }
                    }
                    Some(Variable::from_object(map))
                })
                .collect()
        }
        DataType::List(_) => {
            let array = column.as_list::<i32>();
            let values = column_values(array.values(), name)?;
            let offsets = array.offsets();
            (0..rows)
                .map(|row| {
                    if array.is_null(row) {
                        return None;
                    }
                    let start = offsets[row] as usize;
                    let end = offsets[row + 1] as usize;
                    let items: Vec<Variable> = values[start..end]
                        .iter()
                        .map(|value| value.clone().unwrap_or(Variable::Null))
                        .collect();
                    Some(Variable::from_array(items))
                })
                .collect()
        }
        other => return Err(anyhow!("column '{name}': unsupported arrow type {other}")),
    };
    Ok(values)
}

fn int_values<T: arrow_array::ArrowPrimitiveType>(
    array: &arrow_array::PrimitiveArray<T>,
    to_i64: impl Fn(T::Native) -> i64,
) -> Vec<Option<Variable>> {
    (0..array.len())
        .map(|row| {
            (!array.is_null(row)).then(|| Variable::Number(Decimal::from(to_i64(array.value(row)))))
        })
        .collect()
}

fn float_values<T: arrow_array::ArrowPrimitiveType>(
    array: &arrow_array::PrimitiveArray<T>,
    to_f64: impl Fn(T::Native) -> f64,
) -> Vec<Option<Variable>> {
    (0..array.len())
        .map(|row| {
            if array.is_null(row) {
                return None;
            }
            Decimal::try_from(to_f64(array.value(row)))
                .ok()
                .map(Variable::Number)
        })
        .collect()
}
