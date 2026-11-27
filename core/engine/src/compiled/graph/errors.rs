use crate::compiled::host::NodeError;
use crate::EvaluationError;
use ahash::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use zen_types::decision::DecisionNode;

pub(crate) struct RowErrors<E> {
    errors: Vec<Option<E>>,
    failed: usize,
}

impl<E> RowErrors<E> {
    const COMPACT: usize = 7;

    pub fn new(rows: usize) -> Self {
        Self {
            errors: (0..rows).map(|_| None).collect(),
            failed: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.errors.len()
    }

    pub fn ok(&self, row: usize) -> bool {
        self.errors.get(row).is_some_and(Option::is_none)
    }

    pub fn get(&self, row: usize) -> Option<&E> {
        self.errors.get(row)?.as_ref()
    }

    pub fn generation(&self) -> usize {
        self.failed
    }

    pub fn set(&mut self, row: usize, error: E) {
        if let Some(slot) = self.errors.get_mut(row) {
            if slot.is_none() {
                self.failed += 1;
            }
            *slot = Some(error);
        }
    }

    pub fn fill(&mut self, error: impl Fn() -> E) {
        for row in 0..self.errors.len() {
            self.set(row, error());
        }
    }

    pub fn into_vec(self) -> Vec<Option<E>> {
        self.errors
    }

    pub fn take(&mut self) -> Vec<Option<E>> {
        self.failed = 0;
        std::mem::take(&mut self.errors)
    }

    pub fn live(&self, rows: &Rc<[usize]>) -> Rc<[usize]> {
        match self.failed == 0 || rows.iter().all(|&row| self.ok(row)) {
            true => rows.clone(),
            false => rows.iter().copied().filter(|&row| self.ok(row)).collect(),
        }
    }

    pub fn alive(&self, positions: Option<Rc<[usize]>>, seen: &mut usize) -> Option<Rc<[usize]>> {
        if self.failed == *seen {
            return positions;
        }
        *seen = self.failed;
        let rows = self.errors.len();
        let total = positions.as_ref().map_or(rows, |p| p.len());
        let live = match &positions {
            Some(p) => p.iter().filter(|&&row| self.ok(row)).count(),
            None => self.errors.iter().filter(|error| error.is_none()).count(),
        };
        match (live + live / Self::COMPACT < total, live == rows) {
            (true, _) => {
                let mut kept = Vec::with_capacity(live);
                match &positions {
                    Some(p) => kept.extend(p.iter().copied().filter(|&row| self.ok(row))),
                    None => kept.extend((0..rows).filter(|&row| self.ok(row))),
                }
                Some(kept.into())
            }
            (false, true) => None,
            (false, false) => positions,
        }
    }
}

impl RowErrors<Box<EvaluationError>> {
    pub fn fault(&mut self, row: usize, error: NodeError) {
        self.set(
            row,
            Box::new(NodeError::new(error.node_id, error.source).failure()),
        );
    }

    pub fn fail(&mut self, row: usize, node: &DecisionNode, message: String) {
        self.set(
            row,
            Box::new(NodeError::new(node.id.clone(), message).failure()),
        );
    }
}

struct Failure(Rc<str>);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}

impl std::error::Error for Failure {}

struct Known {
    node: Arc<str>,
    message: Rc<str>,
    error: Rc<EvaluationError>,
}

pub(crate) struct SharedErrors {
    rows: RowErrors<Rc<EvaluationError>>,
    known: HashMap<(Arc<str>, Rc<str>), Rc<EvaluationError>>,
    last: Option<Known>,
    deferral: Rc<EvaluationError>,
}

impl std::ops::Deref for SharedErrors {
    type Target = RowErrors<Rc<EvaluationError>>;

    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl std::ops::DerefMut for SharedErrors {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rows
    }
}

impl SharedErrors {
    pub fn new(rows: usize) -> Self {
        Self {
            rows: RowErrors::new(rows),
            known: HashMap::default(),
            last: None,
            deferral: Rc::new(NodeError::failure(NodeError::new(
                Arc::from(""),
                Failure(Rc::from("deferred")),
            ))),
        }
    }

    pub fn fail(&mut self, row: usize, node: &DecisionNode, message: Rc<str>) {
        if !self.ok(row) {
            return;
        }
        if let Some(last) = &self.last {
            if Arc::ptr_eq(&last.node, &node.id) && Rc::ptr_eq(&last.message, &message) {
                let error = last.error.clone();
                self.rows.set(row, error);
                return;
            }
        }
        let error = self
            .known
            .entry((node.id.clone(), message.clone()))
            .or_insert_with(|| {
                Rc::new(NodeError::failure(NodeError::new(
                    node.id.clone(),
                    Failure(message.clone()),
                )))
            })
            .clone();
        self.last = Some(Known {
            node: node.id.clone(),
            message,
            error: error.clone(),
        });
        self.rows.set(row, error);
    }

    pub fn fault(&mut self, row: usize, error: NodeError) {
        if self.ok(row) {
            self.rows.set(
                row,
                Rc::new(NodeError::new(error.node_id, error.source).failure()),
            );
        }
    }

    pub fn share(&mut self, row: usize, error: &Rc<EvaluationError>) {
        if self.ok(row) {
            self.rows.set(row, error.clone());
        }
    }

    pub fn defer(&mut self, row: usize) {
        let deferral = self.deferral.clone();
        self.share(row, &deferral);
    }

    pub fn deferred(&self) -> Vec<usize> {
        self.rows
            .errors
            .iter()
            .enumerate()
            .filter(|(_, error)| {
                error
                    .as_ref()
                    .is_some_and(|e| Rc::ptr_eq(e, &self.deferral))
            })
            .map(|(row, _)| row)
            .collect()
    }
}
