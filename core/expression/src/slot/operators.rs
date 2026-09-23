use crate::variable::VariableType;

pub(crate) struct Operators;

impl Operators {
    const ORDERED: &[&str] = &["==", "!=", "<", "<=", ">", ">=", "in", "not in"];
    const ORDERED_UNARY: &[&str] = &[">", ">=", "<", "<=", "==", "!=", "in", "not in"];
    const TEXT: &[&str] = &["==", "!=", "in", "not in"];
    const TEXT_UNARY: &[&str] = &["!=", "in", "not in"];
    const MEMBERSHIP: &[&str] = &["in", "not in"];
    pub(crate) const EQUALITY: &[&str] = &["==", "!="];
    pub(crate) const LOGICAL: &[&str] = &["and", "or"];

    pub(crate) fn nullable_extras(unary: bool) -> &'static [&'static str] {
        if unary {
            Self::EQUALITY
        } else {
            &["==", "!=", "??"]
        }
    }

    pub(crate) fn for_type(t: &VariableType, unary: bool) -> Vec<&'static str> {
        if let VariableType::Nullable(inner) = t {
            let mut list = Self::for_type(inner, unary);
            let mut front = 0;
            for op in Self::nullable_extras(unary) {
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
        let list: &[&str] = match (t, unary) {
            (VariableType::Number | VariableType::Date | VariableType::Any, true) => {
                Self::ORDERED_UNARY
            }
            (VariableType::Number | VariableType::Date | VariableType::Any, false) => Self::ORDERED,
            (VariableType::Enum(..) | VariableType::Const(_) | VariableType::String, true) => {
                Self::TEXT_UNARY
            }
            (VariableType::Enum(..) | VariableType::Const(_) | VariableType::String, false) => {
                Self::TEXT
            }
            (VariableType::Array(_), _) => Self::MEMBERSHIP,
            (
                VariableType::Bool
                | VariableType::Null
                | VariableType::Object(_)
                | VariableType::Interval
                | VariableType::Nullable(_),
                _,
            ) => Self::EQUALITY,
        };
        list.to_vec()
    }
}
