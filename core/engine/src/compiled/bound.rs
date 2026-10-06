use ahash::HashMap;
use std::cell::RefCell;
use std::rc::Rc;
use zen_expression::lane::{Binding, Columns, Fault, Kind, LaneProgram, LaneRunner, Output};
use zen_expression::{IsolateError, Scope, Variable};

pub(crate) enum Scopes {
    Owned(Vec<Scope>),
    Blank(Rc<[Scope]>, usize),
}

impl Scopes {
    pub fn slice(&self) -> &[Scope] {
        match self {
            Scopes::Owned(scopes) => scopes,
            Scopes::Blank(scopes, rows) => &scopes[..*rows],
        }
    }
}

pub(crate) struct Bound<'a> {
    pub scopes: Scopes,
    pub columns: Columns<'a>,
    pub bind: Box<dyn Fn(&str) -> Binding + 'a>,
}

pub(crate) struct Outs(Vec<Output>);

impl Outs {
    const KEEP: usize = 32;

    thread_local! {
        static POOL: RefCell<Vec<Vec<Output>>> = const { RefCell::new(Vec::new()) };
    }

    pub fn take() -> Outs {
        Outs(Self::POOL.with_borrow_mut(Vec::pop).unwrap_or_default())
    }
}

impl std::ops::Deref for Outs {
    type Target = Vec<Output>;

    fn deref(&self) -> &Vec<Output> {
        &self.0
    }
}

impl std::ops::DerefMut for Outs {
    fn deref_mut(&mut self) -> &mut Vec<Output> {
        &mut self.0
    }
}

impl Drop for Outs {
    fn drop(&mut self) {
        let outs = std::mem::take(&mut self.0);
        Self::POOL.with_borrow_mut(|pool| {
            if pool.len() < Self::KEEP {
                pool.push(outs);
            }
        });
    }
}

pub(crate) type Many = Result<Vec<Variable>, (usize, IsolateError)>;

type Specialized = HashMap<(u64, Vec<Option<Kind>>), Option<Rc<LaneProgram>>>;

impl<'a> Bound<'a> {
    thread_local! {
        static SPECIALIZED: RefCell<Specialized> = RefCell::new(HashMap::default());
        static BLANK: RefCell<Rc<[Scope]>> = RefCell::new(Rc::from([]));
    }

    pub fn blank(rows: usize) -> Scopes {
        Self::BLANK.with_borrow_mut(|blank| {
            if blank.len() < rows {
                *blank = (0..rows.next_power_of_two()).map(|_| Scope::default()).collect();
            }
            Scopes::Blank(blank.clone(), rows)
        })
    }

    pub fn scopes(&self) -> &[Scope] {
        match &self.scopes {
            Scopes::Owned(scopes) => scopes,
            Scopes::Blank(scopes, rows) => &scopes[..*rows],
        }
    }

    pub fn plain(scopes: Vec<Scope>) -> Self {
        let rows = scopes.len();
        Self {
            scopes: Scopes::Owned(scopes),
            columns: Columns::new(rows),
            bind: Box::new(|_| Binding::Row),
        }
    }

    pub fn rows(&self) -> usize {
        self.scopes().len()
    }

    fn specialized(&self, program: &LaneProgram, bind: &dyn Fn(&str) -> Binding) -> Option<Rc<LaneProgram>> {
        let p = program.program();
        let signature: Vec<Option<Kind>> = p
            .keys
            .iter()
            .map(|key| match bind(key) {
                Binding::Column(index) => self.columns.columns.get(index).map(|(_, c)| c.kind()),
                _ => None,
            })
            .collect();
        if signature.iter().all(|k| matches!(k, None | Some(Kind::Dyn))) {
            return None;
        }
        Self::SPECIALIZED.with_borrow_mut(|cache| {
            cache
                .entry((p.id, signature))
                .or_insert_with(|| program.specialize_bound(&self.columns, bind).ok().map(Rc::new))
                .clone()
        })
    }

    pub fn many(&self, runner: &mut LaneRunner, program: &LaneProgram, subset: Option<&[usize]>, mut sink: impl FnMut(usize, Many)) {
        let specialized = self.specialized(program, &*self.bind);
        let program = specialized.as_deref().unwrap_or(program);
        runner.evaluate_bound(program, self.scopes(), &self.columns, &*self.bind, subset, &mut sink);
    }

    pub fn export(
        &self,
        runner: &mut LaneRunner,
        program: &LaneProgram,
        subset: Option<&[usize]>,
        outs: &mut Vec<Output>,
        failure: impl FnMut(usize, usize, Fault),
    ) {
        self.export_with(runner, program, subset, &*self.bind, outs, failure)
    }

    pub fn export_with(
        &self,
        runner: &mut LaneRunner,
        program: &LaneProgram,
        subset: Option<&[usize]>,
        bind: &dyn Fn(&str) -> Binding,
        outs: &mut Vec<Output>,
        failure: impl FnMut(usize, usize, Fault),
    ) {
        let specialized = self.specialized(program, bind);
        let program = specialized.as_deref().unwrap_or(program);
        runner.evaluate_bound_into(program, self.scopes(), &self.columns, bind, subset, outs, failure);
    }
}
