mod assertion;
mod expression;
mod matching;
mod table;

use zen_expression::lane::Session;

use crate::policy::blocks::{Block, BlockKind, ExecutionError};
use crate::policy::host::Site;
use crate::workspace::types::BlockTrace;

pub(crate) struct Exact<'e> {
    pub(crate) session: &'e mut Session,
    pub(crate) site: &'e Site<'e>,
}

impl Exact<'_> {
    pub(crate) fn execute(&mut self, rule: &Block) -> Result<BlockTrace, ExecutionError> {
        match &rule.kind {
            BlockKind::Assertion(a) => self.execute_assertion(a),
            BlockKind::DecisionTable(d) => self.execute_table(d),
            BlockKind::Expression(e) => self.execute_expression(e),
            BlockKind::Match(m) => self.execute_match(m),
        }
    }
}
