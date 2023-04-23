#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Token<'a> {
    pub span: (usize, usize),
    pub kind: TokenKind,
    pub value: &'a str,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum TokenKind {
    Identifier,
    Number,
    String,
    Operator,
    Bracket,
}
