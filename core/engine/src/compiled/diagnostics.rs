use crate::compiled::ColumnarOutput;
use crate::Decision;
use zen_expression::lane::Columns;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    pub typed: usize,
    pub laned: usize,
    pub hosted: usize,
}

pub fn paths(output: &ColumnarOutput) -> Paths {
    output.paths
}

pub fn census() -> Vec<(String, u64, u64, u128)> {
    crate::compiled::CompiledGraph::census()
}

pub fn plan_verdict(decision: &Decision, columns: &Columns) -> Option<Result<(), String>> {
    decision.compiled().plan_verdict(columns)
}

pub fn compiled_verdict(decision: &Decision) -> Option<Result<(), String>> {
    decision.compiled().compiled_verdict()
}

pub fn degraded(decision: &Decision) -> Option<Vec<(String, String)>> {
    decision.compiled().degraded().map(|nodes| {
        nodes
            .into_iter()
            .map(|(node, why)| (node.to_string(), why.to_string()))
            .collect()
    })
}

pub fn set_census(on: bool) {
    crate::compiled::switches::SWITCHES.set_census(on);
}

pub fn set_typed(on: bool) {
    crate::compiled::switches::SWITCHES.set_typed(on);
}

pub fn compilations() -> usize {
    crate::compiled::switches::SWITCHES.compilations()
}

pub fn evaluation_diagnostics(
    workspace: &crate::Workspace,
    entry: &str,
) -> Vec<crate::workspace::Diagnostic> {
    workspace.evaluation_diagnostics(entry)
}
