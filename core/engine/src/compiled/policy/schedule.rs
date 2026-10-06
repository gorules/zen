use crate::policy::blocks::{Block, BlockKind, ConditionalReads};
use crate::policy::evaluator::EvalArtifact;
use crate::workspace::types::BlockRef;
use ahash::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

pub(crate) type Demand = Arc<[Arc<str>]>;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Event {
    Select(usize),
    Commit(usize),
}

pub(crate) enum End {
    Finish,
    Branch {
        block: usize,
        children: RwLock<HashMap<Demand, Arc<Segment>>>,
    },
}

pub(crate) struct Segment {
    pub(crate) path: Arc<[Demand]>,
    pub(crate) events: Vec<Event>,
    pub(crate) end: End,
}

impl Segment {
    pub(crate) fn finished() -> Self {
        Self {
            path: Arc::from([]),
            events: Vec::new(),
            end: End::Finish,
        }
    }
}

pub(crate) struct Blocks {
    pub(crate) refs: Vec<BlockRef>,
    pub(crate) rules: Vec<Arc<Block>>,
    index: HashMap<BlockRef, usize>,
}

impl Blocks {
    pub(crate) fn new(artifact: &EvalArtifact) -> Self {
        let mut entries: Vec<(&BlockRef, &Arc<Block>)> = artifact.rule_by_ref.iter().collect();
        entries.sort_by(|(a, _), (b, _)| {
            (a.policy_path.as_ref(), a.block_id.as_ref()).cmp(&(b.policy_path.as_ref(), b.block_id.as_ref()))
        });
        let refs: Vec<BlockRef> = entries.iter().map(|(r, _)| (*r).clone()).collect();
        let rules = entries.iter().map(|(_, b)| (*b).clone()).collect();
        let index = refs.iter().enumerate().map(|(i, r)| (r.clone(), i)).collect();
        Self { refs, rules, index }
    }

    pub(crate) fn selects(rule: &Block) -> bool {
        matches!(rule.kind, BlockKind::Match(_) | BlockKind::DecisionTable(_))
    }
}

struct Stop;

struct Sim<'a> {
    artifact: &'a EvalArtifact,
    blocks: &'a Blocks,
    script: &'a [Demand],
    used: usize,
    start: usize,
    ran: HashSet<usize>,
    in_progress: HashSet<usize>,
    events: Vec<Event>,
    stopped: Option<usize>,
}

impl Sim<'_> {
    fn demand(&mut self, prop: &str) -> Result<(), Stop> {
        for owner in self.artifact.writers_of_longest_prefix(prop) {
            self.run_block(owner)?;
        }
        Ok(())
    }

    fn run_block(&mut self, owner: &BlockRef) -> Result<(), Stop> {
        let Some(&block) = self.blocks.index.get(owner) else {
            return Ok(());
        };
        if self.ran.contains(&block) || !self.in_progress.insert(block) {
            return Ok(());
        }
        let result = self.inner(owner, block);
        self.in_progress.remove(&block);
        if result.is_ok() {
            self.ran.insert(block);
        }
        result
    }

    fn inner(&mut self, owner: &BlockRef, block: usize) -> Result<(), Stop> {
        let plan = self.artifact.read_plans.get(owner);
        if let Some(plan) = plan {
            for path in plan.unconditional.iter() {
                self.demand(path)?;
            }
        }
        let rule = &self.blocks.rules[block];
        if Blocks::selects(rule) {
            self.events.push(Event::Select(block));
            let conditional = plan.is_some_and(|p| !matches!(p.conditional, ConditionalReads::None));
            if conditional {
                let Some(demand) = self.script.get(self.used) else {
                    self.stopped = Some(block);
                    return Err(Stop);
                };
                self.used += 1;
                if self.used == self.script.len() {
                    self.start = self.events.len();
                }
                for path in demand.iter() {
                    self.demand(path)?;
                }
            }
        }
        self.events.push(Event::Commit(block));
        Ok(())
    }
}

pub(crate) struct Replay<'a> {
    pub(crate) artifact: &'a EvalArtifact,
    pub(crate) blocks: &'a Blocks,
    pub(crate) roots: &'a [Arc<str>],
}

impl Replay<'_> {
    pub(crate) fn segment(&self, script: &[Demand]) -> Segment {
        let mut sim = Sim {
            artifact: self.artifact,
            blocks: self.blocks,
            script,
            used: 0,
            start: 0,
            ran: HashSet::default(),
            in_progress: HashSet::default(),
            events: Vec::new(),
            stopped: None,
        };
        let finished = self.roots.iter().try_for_each(|root| sim.demand(root)).is_ok();
        let events = sim.events.split_off(sim.start.min(sim.events.len()));
        let end = match (finished, sim.stopped) {
            (false, Some(block)) => End::Branch {
                block,
                children: RwLock::new(HashMap::default()),
            },
            _ => End::Finish,
        };
        Segment {
            path: script.into(),
            events,
            end,
        }
    }
}
