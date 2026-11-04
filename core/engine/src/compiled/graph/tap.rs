use crate::compiled::leaf::Leaf;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::Variable;

thread_local! {
    static TAP: RefCell<Option<Tap>> = const { RefCell::new(None) };
}

pub(crate) type Results = (Arc<[Arc<str>]>, Vec<Leaf<'static>>);
pub(crate) type Objects = (Arc<[Arc<str>]>, Rc<[Variable]>);
pub(crate) type Hits = Vec<SmallVec<[u32; 4]>>;

pub(crate) struct Field {
    pub(crate) name: Arc<str>,
    pub(crate) values: Vec<Variable>,
    pub(crate) failed: Vec<bool>,
}

#[derive(Default)]
pub(crate) struct Tap {
    pub(crate) first: Option<Vec<Option<u32>>>,
    pub(crate) hits: Option<Hits>,
    pub(crate) results: Option<Results>,
    pub(crate) objects: Option<Objects>,
    pub(crate) fields: Option<Vec<Field>>,
    pub(crate) calls: usize,
    wants_fields: bool,
}

impl Tap {
    pub(crate) fn arm(fields: bool) {
        TAP.with(|t| {
            *t.borrow_mut() = Some(Tap {
                wants_fields: fields,
                ..Tap::default()
            })
        });
    }

    pub(crate) fn wants_fields() -> bool {
        TAP.with(|t| t.borrow().as_ref().is_some_and(|tap| tap.wants_fields))
    }

    pub(crate) fn fields(fields: Vec<Field>) {
        TAP.with(|t| {
            if let Some(tap) = t.borrow_mut().as_mut() {
                tap.fields = Some(fields);
            }
        });
    }

    pub(crate) fn hits(hits: Option<Hits>) {
        if !Self::armed() {
            return;
        }
        TAP.with(|t| {
            if let Some(tap) = t.borrow_mut().as_mut() {
                tap.calls += 1;
                tap.hits = hits;
            }
        });
    }

    pub(crate) fn take() -> Option<Tap> {
        TAP.with(|t| t.borrow_mut().take())
    }

    pub(crate) fn armed() -> bool {
        TAP.with(|t| t.borrow().is_some())
    }

    pub(crate) fn first(
        table: &crate::compiled::table::Table,
        first: &crate::compiled::table::First,
    ) {
        if !Self::armed() {
            return;
        }
        let chosen = table.origins_of(first);
        TAP.with(|t| {
            if let Some(tap) = t.borrow_mut().as_mut() {
                tap.calls += 1;
                tap.first = Some(chosen);
            }
        });
    }
}

impl Tap {
    pub(crate) fn results(keys: &Arc<[Arc<str>]>, leaves: &[Leaf<'_>]) {
        if !Self::armed() {
            return;
        }
        let owned: Option<Vec<Leaf<'static>>> = leaves
            .iter()
            .map(|leaf| match leaf {
                Leaf::Typed(array) => Some(Leaf::Typed(array.clone())),
                Leaf::Any(values) => Some(Leaf::Any(values.clone())),
                _ => None,
            })
            .collect();
        TAP.with(|t| {
            if let Some(tap) = t.borrow_mut().as_mut() {
                tap.calls += 1;
                tap.results = owned.map(|leaves| (keys.clone(), leaves));
            }
        });
    }

    pub(crate) fn objects(keys: &Arc<[Arc<str>]>, values: &Rc<[Variable]>) {
        if !Self::armed() {
            return;
        }
        TAP.with(|t| {
            if let Some(tap) = t.borrow_mut().as_mut() {
                tap.calls += 1;
                tap.objects = Some((keys.clone(), values.clone()));
            }
        });
    }
}
