use crate::lane::columns::{Column, Columns};
use crate::lane::program::{Binding, Program};
use crate::scope::Scope;

pub enum Envs<'a> {
    Shared(&'a [Scope]),
    Owned(Vec<Scope>),
}

impl Envs<'_> {
    #[inline]
    pub(super) fn get(&self, row: u32) -> &Scope {
        match self {
            Envs::Shared(s) => &s[row as usize],
            Envs::Owned(s) => &s[row as usize],
        }
    }

    pub(super) fn get_mut(&mut self, row: u32) -> Option<&mut Scope> {
        match self {
            Envs::Shared(_) => None,
            Envs::Owned(s) => s.get_mut(row as usize),
        }
    }
}

pub struct Context<'a> {
    pub roots: Envs<'a>,
    pub envs: Envs<'a>,
    pub columns: Option<&'a Columns<'a>>,
    pub bound: &'a [Binding],
    pub base: usize,
}

impl<'a> Context<'a> {
    pub fn new(program: &Program, roots: &'a [Scope]) -> Self {
        let envs = match program.writes_env {
            true => Envs::Owned(roots.to_vec()),
            false => Envs::Shared(roots),
        };
        let roots = match program.chain {
            true => Envs::Owned(roots.to_vec()),
            false => Envs::Shared(roots),
        };
        Self {
            roots,
            envs,
            columns: None,
            bound: &[],
            base: 0,
        }
    }

    pub fn columnar(
        program: &Program,
        roots: &'a [Scope],
        columns: &'a Columns<'a>,
        bound: &'a [Binding],
        base: usize,
    ) -> Self {
        let mut ctx = Self::new(program, roots);
        ctx.columns = Some(columns);
        ctx.bound = bound;
        ctx.base = base;
        ctx
    }

    #[inline]
    pub(super) fn column(&self, site: u16) -> Option<Option<&'a Column<'a>>> {
        let columns = self.columns?;
        match self.bound.get(site as usize)? {
            Binding::Column(index) => Some(columns.columns.get(*index).map(|(_, c)| c)),
            Binding::Absent => Some(None),
            Binding::Row => None,
        }
    }
}
