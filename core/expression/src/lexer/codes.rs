macro_rules! token_type {
    ("space") => { ' ' | '\n' | '\t' };
    ("digit") => { '0'..='9' };
    ("bracket") => { '(' | ')' | '[' | ']' | '{' | '}' };
    ("cmp_operator") => { '>' | '<' | '!' | '=' };
    ("operator") => { ',' | ':' | '+' | '-' | '/' | '*' | '^' | '%' };
    ("alpha") => { 'A'..='Z' | 'a'..='z' | '$' | '_' | '#' };
    ("alphanumeric") => { 'A'..='Z' | 'a'..='z' | '0'..='9' | '$' | '_' | '#' };
    ("question_mark") => { '?' }
}

macro_rules! is_token_type {
    ($str: expr, $t: tt) => {
        matches!($str, crate::lexer::codes::token_type!($t))
    };
}

pub(crate) use is_token_type;
pub(crate) use token_type;
