use crate::parser::{Node, ParserError};
use nohash_hasher::BuildNoHashHasher;
use std::collections::HashMap;

#[derive(Debug)]
pub struct ParserResult<'a> {
    pub root: &'a Node<'a>,
    pub is_complete: bool,
    pub metadata: Option<HashMap<usize, NodeMetadata, BuildNoHashHasher<usize>>>,
}

#[derive(Debug, Clone)]
pub struct NodeMetadata {
    pub span: (u32, u32),
}

impl<'a> ParserResult<'a> {
    pub fn error(&self) -> Result<(), ParserError> {
        if !self.is_complete {
            return Err(ParserError::Incomplete);
        }

        match self.root.first_error() {
            None => Ok(()),
            Some(err) => Err(ParserError::NodeError(err.to_string())),
        }
    }
}
