#[macro_export]
macro_rules! is_token_type {
    ($str: expr, "space") => {
        matches!($str, ' ' | '\n' | '\t')
    };
    ($str: expr, "quote") => {
        matches!($str, '\'' | '"')
    };
    ($str: expr, "digit") => {
        matches!($str, '0'..='9')
    };
    ($str: expr, "bracket") => {
        matches!($str, '(' | ')' | '[' | ']')
    };
    ($str: expr, "cmp_operator") => {
        matches!($str, '>' | '<' | '!' | '=')
    };
    ($str: expr, "operator") => {
        matches!($str, '#' | ',' | '?' | ':' | '+' | '-' | '/' | '*' | '^' | '%')
    };
    ($str: expr, "alpha") => {
        matches!($str, 'A'..='Z' | 'a'..='z' | '$' | '_')
    };
    ($str: expr, "alphanumeric") => {
        matches!($str, 'A'..='Z' | 'a'..='z' | '0'..='9' | '$' | '_')
    };
}
