use crate::variable::VariableType;

const ORDERED: &[&str] = &["==", "!=", "<", "<=", ">", ">=", "in", "not in"];
const ORDERED_UNARY: &[&str] = &[">", ">=", "<", "<=", "==", "!=", "in", "not in"];
const TEXT: &[&str] = &["==", "!=", "in", "not in"];
const TEXT_UNARY: &[&str] = &["!=", "in", "not in"];
pub(crate) const EQUALITY: &[&str] = &["==", "!="];
const MEMBERSHIP: &[&str] = &["in", "not in"];
pub(crate) fn nullable_extras(unary: bool) -> &'static [&'static str] {
    if unary {
        &["==", "!="]
    } else {
        &["==", "!=", "??"]
    }
}
pub(crate) fn operators_for(t: &VariableType, unary: bool) -> Vec<&'static str> {
    if let VariableType::Nullable(inner) = t {
        let mut list = operators_for(inner, unary);
        let mut front = 0;
        for op in nullable_extras(unary) {
            if list.contains(op) {
                continue;
            }
            if *op == "??" {
                list.push(op);
            } else {
                list.insert(front, op);
                front += 1;
            }
        }
        return list;
    }
    let list: &[&str] = match t {
        VariableType::Number | VariableType::Date => {
            if unary {
                ORDERED_UNARY
            } else {
                ORDERED
            }
        }
        VariableType::Enum(_, _) | VariableType::Const(_) | VariableType::String => {
            if unary {
                TEXT_UNARY
            } else {
                TEXT
            }
        }
        VariableType::Bool => EQUALITY,
        VariableType::Array(_) => MEMBERSHIP,
        VariableType::Any => {
            if unary {
                ORDERED_UNARY
            } else {
                ORDERED
            }
        }
        VariableType::Null
        | VariableType::Object(_)
        | VariableType::Interval
        | VariableType::Nullable(_) => EQUALITY,
    };
    list.to_vec()
}

pub(crate) const LOGICAL: &[&str] = &["and", "or"];
