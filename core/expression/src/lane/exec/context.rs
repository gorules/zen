use crate::lane::columns::{Column, Columns, Values};
use crate::lane::program::{Binding, Program, Reg};
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
    pub(super) views: Vec<(u64, Reg, Column<'a>)>,
    pub(super) texts: Vec<(*const u8, Option<&'a str>)>,
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
            views: Vec::new(),
            texts: Vec::new(),
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

    pub(super) fn view(&self, program: u64, reg: Reg) -> Option<Column<'a>> {
        self.views
            .iter()
            .rev()
            .find(|(p, r, _)| *p == program && *r == reg)
            .map(|(_, _, c)| *c)
    }

    pub(super) fn text(&mut self, column: Column<'a>) -> Column<'a> {
        let Values::Utf8 { offsets, data } = column.values else {
            return column;
        };
        let key = data.as_ptr();
        let text = match self.texts.iter().find(|(k, _)| *k == key) {
            Some((_, text)) => *text,
            None => {
                let text = std::str::from_utf8(data).ok();
                self.texts.push((key, text));
                text
            }
        };
        match text {
            Some(data) => Column {
                values: Values::Text { offsets, data },
                validity: column.validity,
            },
            None => column,
        }
    }

    pub(super) fn register(&mut self, program: u64, reg: Reg, column: Column<'a>) {
        match self.views.iter_mut().find(|(p, r, _)| *p == program && *r == reg) {
            Some(slot) => slot.2 = column,
            None => self.views.push((program, reg, column)),
        }
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
