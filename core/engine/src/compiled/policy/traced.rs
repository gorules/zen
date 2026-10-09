use std::sync::Arc;
use std::time::Instant;

use super::schedule::{Demand, End, Event};
use super::{Host, PolicyPlan};
use crate::policy::evaluator::{EvalArtifact, StoreOps};
use crate::policy::host::Picked;
use crate::workspace::types::{EvaluateRequest, EvaluationError, EvaluationResult, Trace};

impl PolicyPlan {
    pub(crate) fn traced(
        &self,
        artifact: &EvalArtifact,
        req: &EvaluateRequest,
        extras: bool,
    ) -> Result<EvaluationResult, EvaluationError> {
        let start = Instant::now();
        let (store, order_to_run) = artifact.prepare(req)?;
        let mut host = Host::new(artifact, &store, &req.policy_path, req.trace, extras, &self.programs);
        let outcome = self.replay(artifact, &mut host);

        let trace = req.trace.then(|| Trace {
            engine_version: Arc::from(crate::ENGINE_VERSION),
            properties: store.snapshot(&order_to_run),
            executions: host.executions(),
        });

        if let Err(error) = outcome {
            return Err(error.with_partial_trace(trace));
        }

        Ok(EvaluationResult {
            output: store,
            duration: start.elapsed(),
            trace,
        })
    }

    fn replay(&self, artifact: &EvalArtifact, host: &mut Host) -> Result<(), EvaluationError> {
        let mut segment = self.root.clone();
        let mut picks: Vec<(usize, Picked)> = Vec::new();
        loop {
            for event in &segment.events {
                match *event {
                    Event::Select(block) => {
                        let picked = host.select(&self.blocks.refs[block], &self.blocks.rules[block])?;
                        picks.push((block, picked));
                    }
                    Event::Commit(block) => {
                        let (owner, rule) = (&self.blocks.refs[block], &self.blocks.rules[block]);
                        let picked = match picks.iter().position(|(b, _)| *b == block) {
                            Some(at) => picks.swap_remove(at).1,
                            None => host.select(owner, rule)?,
                        };
                        host.commit(owner, rule, picked)?;
                    }
                }
            }
            let End::Branch { block, .. } = &segment.end else {
                return Ok(());
            };
            let owner = &self.blocks.refs[*block];
            let demand: Demand = match picks.iter().find(|(b, _)| b == block) {
                Some((_, picked)) => host.state.demanded(owner, picked).into(),
                None => Arc::from([]),
            };
            segment = self.child(artifact, &segment, demand);
        }
    }
}
