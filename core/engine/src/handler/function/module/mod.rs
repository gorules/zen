use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::DerefMut;
use std::rc::Rc;

use rquickjs::loader::{Bundle, Loader, ModuleLoader as MDLoader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{embed, Ctx, Error, Module};

use crate::handler::function::module::zen::js_zen_module;

pub(crate) mod console;
pub(crate) mod zen;

static JS_BUNDLE: Bundle = embed! {
    "dayjs": "js/dayjs.js",
    "big": "js/big.js",
    "internals": "js/internals.js"
};

#[derive(Clone)]
pub struct ModuleLoader(Rc<RefCell<BaseModuleLoader>>);

impl ModuleLoader {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(BaseModuleLoader::new())))
    }

    pub fn add_module(&self, module: String) {
        let reference = self.0.borrow_mut();
        reference.add_module(module);
    }
}

impl Resolver for ModuleLoader {
    fn resolve<'js>(&mut self, ctx: &Ctx<'js>, base: &str, name: &str) -> rquickjs::Result<String> {
        let mut inner = self.0.borrow_mut();
        inner.deref_mut().resolve(ctx, base, name)
    }
}

impl Loader for ModuleLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<Module<'js, Declared>> {
        let mut inner = self.0.borrow_mut();
        inner.deref_mut().load(ctx, name)
    }
}

struct BaseModuleLoader {
    bundle: Bundle,
    defined_modules: RefCell<HashSet<String>>,
    md_loader: MDLoader,
}

impl BaseModuleLoader {
    pub fn new() -> Self {
        let hs = HashSet::from(["zen".to_string()]);

        Self {
            bundle: JS_BUNDLE,
            defined_modules: RefCell::new(hs),
            md_loader: MDLoader::default().with_module("zen", js_zen_module),
        }
    }

    pub fn add_module(&self, value: String) {
        let mut modules = self.defined_modules.borrow_mut();
        modules.insert(value);
    }
}

impl Resolver for &mut BaseModuleLoader {
    fn resolve<'js>(&mut self, ctx: &Ctx<'js>, base: &str, name: &str) -> rquickjs::Result<String> {
        if let Ok(b) = self.bundle.resolve(ctx, base, name) {
            return Ok(b);
        }

        let defined_modules = self.defined_modules.borrow();
        if defined_modules.contains(name) {
            return Ok(name.to_string());
        }

        Err(Error::new_resolving(base, name))
    }
}

impl Loader for &mut BaseModuleLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<Module<'js, Declared>> {
        self.bundle
            .load(ctx, name)
            .or_else(|_| self.md_loader.load(ctx, name))
    }
}
