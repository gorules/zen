use std::collections::HashSet;

use swc_common::input::StringInput;
use swc_common::source_map::SmallPos;
use swc_common::{BytePos, GLOBALS};
use swc_ecma_ast::{
    ArrowExpr, AssignExpr, AssignTarget, BindingIdent, CallExpr, Callee, Decl, Expr, FnDecl, Function, Ident,
    ImportSpecifier, Lit, MemberExpr, MemberProp, Module, ModuleDecl, ModuleItem, OptChainBase, Pat,
    SimpleAssignTarget, Stmt, ThisExpr, UnaryExpr, UnaryOp, UpdateExpr, VarDeclKind, VarDeclarator, WithStmt,
};
use swc_ecma_parser::{lexer::Lexer, EsSyntax, Parser, Syntax};
use swc_ecma_visit::{Visit, VisitWith};

pub(crate) struct Isolation;

impl Isolation {
    pub(crate) fn shareable(source: &str) -> bool {
        GLOBALS.set(&Default::default(), || {
            let lexer = Lexer::new(
                Syntax::Es(EsSyntax::default()),
                Default::default(),
                StringInput::new(source, BytePos::from_usize(0), BytePos::from_usize(source.len())),
                None,
            );
            let mut parser = Parser::new_from(lexer);
            match parser.parse_module() {
                Ok(module) if parser.take_errors().is_empty() => Self::module(&module),
                _ => false,
            }
        })
    }

    fn module(module: &Module) -> bool {
        let mut names = HashSet::new();
        for item in &module.body {
            let decl = match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                    names.extend(import.specifiers.iter().map(Self::specifier));
                    continue;
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
                ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(named)) if named.src.is_none() => continue,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(_)) => continue,
                ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
                ModuleItem::Stmt(Stmt::Empty(_)) => continue,
                _ => return false,
            };
            match decl {
                Decl::Fn(function) => {
                    names.insert(function.ident.sym.to_string());
                }
                Decl::Var(var) if var.kind == VarDeclKind::Const => {
                    for declarator in &var.decls {
                        let (Pat::Ident(binding), Some(init)) = (&declarator.name, &declarator.init) else {
                            return false;
                        };
                        if !Self::inert(init) {
                            return false;
                        }
                        names.insert(binding.id.sym.to_string());
                    }
                }
                _ => return false,
            }
        }
        let mut scan = Scan::default();
        module.visit_with(&mut scan);
        let mut tainted: HashSet<String> = HashSet::new();
        loop {
            let before = tainted.len();
            for (bound, root) in &scan.aliases {
                if !scan.locals.contains(root) || names.contains(root) || tainted.contains(root) {
                    tainted.extend(bound.iter().cloned());
                }
            }
            if tainted.len() == before {
                break;
            }
        }
        let locals: HashSet<String> = scan
            .locals
            .into_iter()
            .filter(|name| !names.contains(name) && !tainted.contains(name))
            .collect();
        let mut check = Check { locals, shared: false };
        module.visit_with(&mut check);
        !check.shared
    }

    fn specifier(specifier: &ImportSpecifier) -> String {
        match specifier {
            ImportSpecifier::Named(named) => named.local.sym.to_string(),
            ImportSpecifier::Default(default) => default.local.sym.to_string(),
            ImportSpecifier::Namespace(namespace) => namespace.local.sym.to_string(),
        }
    }

    fn inert(expr: &Expr) -> bool {
        match expr {
            Expr::Lit(Lit::Regex(_)) => false,
            Expr::Lit(_) => true,
            Expr::Tpl(template) => template.exprs.is_empty(),
            Expr::Arrow(_) | Expr::Fn(_) => true,
            Expr::Paren(inner) => Self::inert(&inner.expr),
            Expr::Unary(unary) if matches!(unary.op, UnaryOp::Minus | UnaryOp::Plus) => Self::inert(&unary.arg),
            _ => false,
        }
    }

    fn root(expr: &Expr) -> Root {
        match expr {
            Expr::Ident(ident) => Root::Name(ident.sym.to_string()),
            Expr::Member(member) => Self::root(&member.obj),
            Expr::Paren(inner) => Self::root(&inner.expr),
            Expr::OptChain(chain) => match &*chain.base {
                OptChainBase::Member(member) => Self::root(&member.obj),
                OptChainBase::Call(call) => Self::root(&call.callee),
            },
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => Self::root(callee),
                _ => Root::Unknown,
            },
            Expr::This(_) | Expr::SuperProp(_) => Root::Unknown,
            _ => Root::Fresh,
        }
    }
}

enum Root {
    Name(String),
    Fresh,
    Unknown,
}

#[derive(Default)]
struct Scan {
    depth: usize,
    locals: HashSet<String>,
    aliases: Vec<(Vec<String>, String)>,
}

struct Names<'a>(&'a mut Vec<String>);

impl Visit for Names<'_> {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.0.push(ident.id.sym.to_string());
    }
}

impl Visit for Scan {
    fn visit_function(&mut self, function: &Function) {
        self.depth += 1;
        function.visit_children_with(self);
        self.depth -= 1;
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        self.depth += 1;
        arrow.visit_children_with(self);
        self.depth -= 1;
    }

    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        if self.depth > 0 {
            self.locals.insert(ident.id.sym.to_string());
        }
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        if self.depth > 0 {
            self.locals.insert(decl.ident.sym.to_string());
        }
        decl.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if self.depth > 0 {
            if let Some(Root::Name(root)) = declarator.init.as_deref().map(Isolation::root) {
                let mut bound = Vec::new();
                declarator.name.visit_with(&mut Names(&mut bound));
                self.aliases.push((bound, root));
            }
        }
        declarator.visit_children_with(self);
    }
}

struct Check {
    locals: HashSet<String>,
    shared: bool,
}

struct Targets<'a> {
    check: &'a Check,
    owned: bool,
}

impl Visit for Targets<'_> {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.owned &= self.check.locals.contains(ident.id.sym.as_ref());
    }

    fn visit_member_expr(&mut self, member: &MemberExpr) {
        self.owned &= self.check.owned(&member.obj);
    }
}

impl Check {
    const FORBIDDEN: &'static [&'static str] = &["globalThis", "global", "self", "window", "eval", "Function"];
    const PROTOTYPE: &'static [&'static str] = &["__proto__", "prototype", "constructor", "getPrototypeOf", "setPrototypeOf"];
    const MUTATORS: &'static [&'static str] = &[
        "extend", "locale", "set", "push", "pop", "shift", "unshift", "splice", "sort", "reverse", "fill", "copyWithin",
        "add", "delete", "clear", "assign", "defineProperty", "defineProperties", "setPrototypeOf", "freeze", "seal",
        "preventExtensions", "updateLocale", "tz", "config", "use",
    ];
    const OBJECT: &'static [&'static str] =
        &["assign", "defineProperty", "defineProperties", "setPrototypeOf", "freeze", "seal", "preventExtensions"];

    fn owned(&self, expr: &Expr) -> bool {
        match Isolation::root(expr) {
            Root::Name(name) => self.locals.contains(&name),
            Root::Fresh => true,
            Root::Unknown => false,
        }
    }

    fn name(prop: &MemberProp) -> Option<String> {
        match prop {
            MemberProp::Ident(ident) => Some(ident.sym.to_string()),
            MemberProp::Computed(computed) => match &*computed.expr {
                Expr::Lit(Lit::Str(text)) => Some(text.value.to_string_lossy().into_owned()),
                _ => None,
            },
            MemberProp::PrivateName(_) => None,
        }
    }

    fn global(expr: &Expr, name: &str) -> bool {
        matches!(expr, Expr::Ident(ident) if ident.sym == name)
    }
}

impl Visit for Check {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        let owned = match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(binding)) => self.locals.contains(binding.id.sym.as_ref()),
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => self.owned(&member.obj),
            AssignTarget::Simple(_) => false,
            AssignTarget::Pat(pat) => {
                let mut targets = Targets { check: self, owned: true };
                pat.visit_with(&mut targets);
                targets.owned
            }
        };
        self.shared |= !owned;
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        let owned = match &*update.arg {
            Expr::Ident(ident) => self.locals.contains(ident.sym.as_ref()),
            Expr::Member(member) => self.owned(&member.obj),
            _ => false,
        };
        self.shared |= !owned;
        update.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        if unary.op == UnaryOp::Delete {
            self.shared |= match &*unary.arg {
                Expr::Member(member) => !self.owned(&member.obj),
                _ => true,
            };
        }
        unary.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Import(_) | Callee::Super(_) => self.shared = true,
            Callee::Expr(callee) => {
                if let Expr::Member(member) = &**callee {
                    let method = Self::name(&member.prop).unwrap_or_default();
                    let reflect = Self::global(&member.obj, "Reflect");
                    let object = Self::global(&member.obj, "Object") && Self::OBJECT.contains(&method.as_str());
                    if reflect || object {
                        self.shared |= call.args.first().is_none_or(|first| !self.owned(&first.expr));
                    } else if Self::MUTATORS.contains(&method.as_str()) {
                        self.shared |= !self.owned(&member.obj);
                    }
                }
            }
        }
        call.visit_children_with(self);
    }

    fn visit_ident(&mut self, ident: &Ident) {
        self.shared |= Self::FORBIDDEN.contains(&ident.sym.as_ref());
    }

    fn visit_member_prop(&mut self, prop: &MemberProp) {
        if let Some(name) = Self::name(prop) {
            self.shared |= Self::PROTOTYPE.contains(&name.as_str());
        }
        prop.visit_children_with(self);
    }

    fn visit_this_expr(&mut self, _: &ThisExpr) {
        self.shared = true;
    }

    fn visit_with_stmt(&mut self, _: &WithStmt) {
        self.shared = true;
    }
}

#[cfg(test)]
mod tests {
    use super::Isolation;

    #[test]
    fn plain_handlers_share_a_runtime() {
        let sources = [
            "export const handler = async (input) => { let x = 1; x++; return { x, y: `a${input.a}b` }; };",
            "const RATE = 0.2\nexport async function handler(input) { return { v: input.v * RATE } }",
            "function helper(a) { return a + 1 }\nexport const handler = (input) => ({ v: helper(input.v) })",
            "import dayjs from 'dayjs';\nexport const handler = async (input) => { const out = {}; out.at = dayjs(input.at).format(); const list = []; list.push(1); input.seen = true; return { out, list }; };",
            "export const handler = async (input) => { const items = input.items.map((i) => ({ ...i, total: i.qty * i.price })); items.sort((a, b) => a.total - b.total); return { items }; };",
        ];
        for source in sources {
            assert!(Isolation::shareable(source), "{source}");
        }
    }

    #[test]
    fn module_state_needs_a_fresh_runtime() {
        let sources = [
            "let count = 0;\nexport const handler = async () => ({ n: ++count });",
            "const cache = new Map();\nexport const handler = async (input) => { cache.set(1, 2); return {}; };",
            "const seen = {};\nexport const handler = async (input) => ({ first: !seen.x });",
            "export const handler = async () => { globalThis.flag = 1; return {}; };",
            "var hits = 0\nexport const handler = () => ({ hits: hits++ })",
            "Array.prototype.last = function () { return this[this.length - 1]; };\nexport const handler = () => ({})",
            "console.log('loaded');\nexport const handler = () => ({})",
            "const LIMIT = 5, queue = [];\nexport const handler = () => { queue.push(1); return { n: queue.length } }",
            "const counter = (() => { let n = 0; return () => ++n; })();\nexport const handler = () => ({ n: counter() })",
            "function helper() { helper.calls = (helper.calls || 0) + 1; return helper.calls }\nexport const handler = () => ({ n: helper() })",
            "export const handler = () => { Math.seen = (Math.seen || 0) + 1; return { n: Math.seen } }",
            "import Big from 'big.js';\nexport const handler = () => { Big.DP = 2; return {} }",
            "import dayjs from 'dayjs';\nexport const handler = () => { dayjs.locale('de'); return {} }",
            "export const handler = () => { const m = Math; m.x = 1; return {} }",
            "export const handler = () => { const p = Object.getPrototypeOf([]); p.x = 1; return {} }",
            "export const handler = () => { config.x = 1; return {} }",
            "const pattern = /a/g;\nexport const handler = (input) => ({ ok: pattern.test(input.s) })",
            "import zod from 'zod';\nconst schema = zod.object({ a: zod.number() });\nexport const handler = (input) => ({ a: schema.parse(input).a })",
        ];
        for source in sources {
            assert!(!Isolation::shareable(source), "{source}");
        }
    }
}
