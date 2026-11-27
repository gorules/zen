mod assertion;
mod expression;
mod matching;

use zen_expression::lane::Session;

use crate::compiled::exact::RowTable;
use crate::compiled::policy::blocks::TableSelection;
use crate::compiled::policy::site::Site;
use crate::policy::blocks::{Block, BlockKind, DecisionTableIr, ExecutionError};
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

impl Exact<'_> {
    pub(crate) fn select_table(
        &mut self,
        ir: &DecisionTableIr,
    ) -> Result<TableSelection, ExecutionError> {
        RowTable::new(
            self.session,
            &ir.inputs,
            self.site.block_id,
            self.site.indexes.of(ir).as_ref().as_ref(),
            Some(self.site),
        )
        .select(ir, self.site.extras)
    }

    pub(crate) fn commit_table(
        &mut self,
        ir: &DecisionTableIr,
        selection: &TableSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        RowTable::new(
            self.session,
            &ir.inputs,
            self.site.block_id,
            None,
            Some(self.site),
        )
        .commit(ir, selection, self.site)
    }

    fn execute_table(&mut self, ir: &DecisionTableIr) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select_table(ir)?;
        self.commit_table(ir, &selection)
    }
}
