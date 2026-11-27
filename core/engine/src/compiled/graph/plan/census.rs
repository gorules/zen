use super::{Op, Program};
use crate::compiled::census::Census;

impl Op {
    pub(crate) fn refuse(&self, graph: &crate::compiled::graph::CompiledGraph) {
        if Census::on() {
            if let Some(key) = self.census(graph) {
                Census::reject(key);
            }
        }
    }

    pub(crate) fn census(
        &self,
        graph: &crate::compiled::graph::CompiledGraph,
    ) -> Option<(usize, String)> {
        let base = graph.census;
        match self {
            Op::Lane(lane) => Some((
                base,
                format!("{}:{}", lane.node.index(), lane.program.tag()),
            )),
            Op::Table(table) => Some((base, format!("{}:table", table.node.index()))),
            _ => None,
        }
    }
}

impl Program {
    fn tag(&self) -> String {
        match self {
            Program::Expression => "expression".into(),
            Program::Condition(at) => format!("condition{at}"),
            Program::Field => "field".into(),
        }
    }
}
