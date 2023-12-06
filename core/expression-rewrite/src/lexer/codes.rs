#[macro_export]
macro_rules! token_type {
    ("space") => { ' ' | '\n' | '\t' };
    ("quote") => { '\'' | '"' };
    ("digit") => { '0'..='9' };
    ("bracket") => { '(' | ')' | '[' | ']' };
    ("cmp_operator") => { '>' | '<' | '!' | '=' };
    ("operator") => { '#' | ',' | '?' | ':' | '+' | '-' | '/' | '*' | '^' | '%' };
    ("alpha") => { 'A'..='Z' | 'a'..='z' | '$' | '_' };
    ("alphanumeric") => { 'A'..='Z' | 'a'..='z' | '0'..='9' | '$' | '_' };
}

#[macro_export]
macro_rules! is_token_type {
    ($str: expr, $t: tt) => {
        matches!($str, crate::token_type!($t))
    };
}
