use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use swc_common::input::StringInput;
use swc_common::source_map::SmallPos;
use swc_common::{BytePos, GLOBALS};
use swc_ecma_ast::{
    ArrowExpr, AssignTarget, AssignTargetPat, BinaryOp, BlockStmt, BlockStmtOrExpr, Callee, Decl, Expr, ExprOrSpread,
    ForHead, Function, ImportDecl, ImportSpecifier, Lit, MemberExpr, MemberProp, ModuleDecl, ModuleExportName,
    ModuleItem, ObjectPatProp, OptChainBase, Pat, Prop, PropName, PropOrSpread, SimpleAssignTarget, Stmt, UnaryOp,
    VarDecl, VarDeclKind, VarDeclOrExpr,
};
use swc_ecma_parser::{lexer::Lexer, EsSyntax, Parser, Syntax};

use super::v1::script::Session;

pub(crate) struct Purity;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Script,
    Module,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lib {
    Dayjs,
    Big,
    Zen,
    Opaque,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    TopFn,
    TopConst,
    Local,
    Lib(Lib),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    TopFn,
    TopConst,
    Local,
    Lib(Lib),
    Global,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Value,
    Consumed,
}

struct Walk {
    dialect: Dialect,
    collect: bool,
    sites: HashMap<String, Vec<Site>>,
    classes: HashMap<String, Class>,
    pure: bool,
}

impl Purity {
    const LIMIT: usize = 256;
    const RESERVED: &'static [&'static str] = &["main", "now", "log", "console", "dayjs", "Big", "handler"];
    const BANNED: &'static [&'static str] = &[
        "__proto__", "prototype", "constructor", "caller", "callee", "arguments", "__defineGetter__", "__defineSetter__",
        "__lookupGetter__", "__lookupSetter__",
    ];
    const VALUES: &'static [&'static str] = &["undefined", "NaN", "Infinity"];
    const CALLABLE: &'static [&'static str] = &[
        "Number", "String", "Boolean", "parseFloat", "parseInt", "isNaN", "isFinite", "Array", "Date", "Error", "TypeError",
        "RangeError", "encodeURIComponent", "decodeURIComponent", "encodeURI", "decodeURI",
    ];
    const CONSTRUCTORS: &'static [&'static str] = &[
        "Date", "Map", "Set", "Array", "Object", "Error", "TypeError", "RangeError", "RegExp", "Number", "String", "Boolean",
    ];
    const STATICS: &'static [(&'static str, &'static [&'static str])] = &[
        ("Object", &["keys", "values", "entries", "fromEntries", "hasOwn", "is"]),
        (
            "Math",
            &[
                "abs", "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "cbrt", "ceil", "clz32", "cos", "cosh", "exp",
                "expm1", "floor", "fround", "hypot", "imul", "log", "log10", "log1p", "log2", "max", "min", "pow", "random",
                "round", "sign", "sin", "sinh", "sqrt", "tan", "tanh", "trunc",
            ],
        ),
        ("JSON", &["stringify", "parse"]),
        ("Number", &["isInteger", "isFinite", "isNaN", "isSafeInteger", "parseFloat", "parseInt"]),
        ("Array", &["isArray", "from", "of"]),
        ("String", &["fromCharCode", "fromCodePoint"]),
        ("Date", &["now", "parse", "UTC"]),
    ];
    const MODULE_STATICS: &'static [(&'static str, &'static [&'static str])] = &[("console", &["log"])];
    const CONSTANTS: &'static [(&'static str, &'static [&'static str])] = &[
        ("Math", &["PI", "E", "LN2", "LN10", "LOG2E", "LOG10E", "SQRT1_2", "SQRT2"]),
        (
            "Number",
            &[
                "MAX_SAFE_INTEGER", "MIN_SAFE_INTEGER", "EPSILON", "MAX_VALUE", "MIN_VALUE", "POSITIVE_INFINITY",
                "NEGATIVE_INFINITY", "NaN",
            ],
        ),
    ];
    const MODULE_CONSTANTS: &'static [(&'static str, &'static [&'static str])] =
        &[("config", &["iteration", "maxDepth", "trace"])];
    const METHODS: &'static [&'static str] = &[
        "at", "concat", "entries", "every", "filter", "find", "findIndex", "findLast", "findLastIndex", "flat", "flatMap",
        "forEach", "includes", "indexOf", "join", "keys", "lastIndexOf", "map", "reduce", "reduceRight", "slice", "some",
        "values", "toReversed", "toSorted", "toSpliced", "with", "push", "pop", "shift", "unshift", "splice", "sort",
        "reverse", "fill", "charAt", "charCodeAt", "codePointAt", "endsWith", "localeCompare", "match", "matchAll",
        "normalize", "padEnd", "padStart", "repeat", "replace", "replaceAll", "search", "split", "startsWith", "substring",
        "substr", "toLowerCase", "toUpperCase", "trim", "trimStart", "trimEnd", "toFixed", "toPrecision", "toExponential",
        "toString", "valueOf", "hasOwnProperty", "has", "set", "add", "delete", "clear", "getTime", "getFullYear", "getMonth",
        "getDate", "getDay", "getHours", "getMinutes", "getSeconds", "getMilliseconds", "getUTCFullYear", "getUTCMonth",
        "getUTCDate", "getUTCDay", "getUTCHours", "getUTCMinutes", "getUTCSeconds", "getUTCMilliseconds",
        "getTimezoneOffset", "toISOString", "toJSON", "toDateString", "toTimeString", "toUTCString", "test", "exec", "abs",
        "cmp", "div", "eq", "gt", "gte", "lt", "lte", "minus", "mul", "neg", "plus", "pow", "prec", "round", "sqrt", "sub",
        "times", "toNumber", "subtract", "format", "diff", "isBefore", "isAfter", "isSame", "isValid", "startOf", "endOf",
        "year", "month", "date", "day", "hour", "minute", "second", "millisecond", "unix", "toDate", "daysInMonth", "clone",
        "utcOffset",
    ];
    const MEMBERS: &'static [&'static str] = &[
        "$g", "$locale", "$set", "$utils", "abs", "add", "anchor", "apply", "at", "big", "bind", "blink", "bold", "call",
        "charAt", "charCodeAt", "clear", "clone", "cmp", "codePointAt", "compile", "concat", "copyWithin", "date", "day",
        "daysInMonth", "delete", "diff", "div", "endOf", "endsWith", "entries", "eq", "every", "exec", "fill", "filter",
        "finally", "find", "findIndex", "findLast", "findLastIndex", "fixed", "flat", "flatMap", "fontcolor", "fontsize",
        "forEach", "format", "get", "getDate", "getDay", "getFullYear", "getHours", "getMilliseconds", "getMinutes",
        "getMonth", "getSeconds", "getTime", "getTimezoneOffset", "getUTCDate", "getUTCDay", "getUTCFullYear",
        "getUTCHours", "getUTCMilliseconds", "getUTCMinutes", "getUTCMonth", "getUTCSeconds", "getYear", "gt", "gte",
        "has", "hasOwnProperty", "hour", "includes", "indexOf", "init", "isAfter", "isBefore", "isPrototypeOf", "isSame",
        "isValid", "isWellFormed", "italics", "join", "keys", "lastIndexOf", "link", "locale", "localeCompare", "lt",
        "lte", "map", "match", "matchAll", "millisecond", "minus", "minute", "mod", "month", "mul", "neg", "next", "normalize",
        "padEnd", "padStart", "parse", "plus", "pop", "pow", "prec", "propertyIsEnumerable", "push", "reduce",
        "reduceRight", "repeat", "replace", "replaceAll", "reverse", "round", "search", "second", "set", "setDate",
        "setFullYear", "setHours", "setMilliseconds", "setMinutes", "setMonth", "setSeconds", "setTime", "setUTCDate",
        "setUTCFullYear", "setUTCHours", "setUTCMilliseconds", "setUTCMinutes", "setUTCMonth", "setUTCSeconds",
        "setYear", "shift", "slice", "small", "some", "sort", "splice", "split", "sqrt", "startOf", "startsWith", "strike",
        "sub", "subtract", "substr", "substring", "sup", "test", "then", "times", "toDate", "toDateString",
        "toExponential", "toFixed", "toGMTString", "toISOString", "toJSON", "toLocaleDateString", "toLocaleLowerCase",
        "toLocaleString", "toLocaleTimeString", "toLocaleUpperCase", "toLowerCase", "toNumber", "toPrecision",
        "toReversed", "toSorted", "toSpliced", "toString", "toTimeString", "toUTCString", "toUpperCase", "toWellFormed",
        "trim", "trimEnd", "trimLeft", "trimRight", "trimStart", "unix", "unshift", "utcOffset", "valueOf", "values",
        "with", "year", "catch", "drop", "take", "toArray", "throw", "return", "union", "intersection", "difference",
        "symmetricDifference", "isSubsetOf", "isSupersetOf", "isDisjointFrom", "getOrInsert", "getOrInsertComputed",
    ];

    const ZEN: &'static [&'static str] = &["evaluateExpression", "evaluateUnaryExpression"];

    thread_local! {
        static LAYOUTS: RefCell<HashMap<Rc<str>, Option<Rc<str>>>> = RefCell::new(HashMap::new());
    }

    pub(crate) fn layout(source: &str) -> Option<Rc<str>> {
        if let Some(cached) = Self::LAYOUTS.with_borrow(|layouts| layouts.get(source).cloned()) {
            return cached;
        }
        let layout = Self::analyze(source).then(|| Self::blank(source));
        Self::LAYOUTS.with_borrow_mut(|layouts| {
            if layouts.len() >= Self::LIMIT {
                layouts.clear();
            }
            layouts.insert(Rc::from(source), layout.clone());
        });
        layout
    }

    fn blank(source: &str) -> Rc<str> {
        let blank: String = source.bytes().map(|byte| if byte == b'\n' { '\n' } else { ' ' }).collect();
        Rc::from(blank)
    }

    fn analyze(source: &str) -> bool {
        if source.contains(['\r', '\u{2028}', '\u{2029}']) || source.contains("\\\n") || source.contains(Session::ROW) {
            return false;
        }
        GLOBALS.set(&Default::default(), || {
            let lexer = Lexer::new(
                Syntax::Es(EsSyntax::default()),
                Default::default(),
                StringInput::new(source, BytePos::from_usize(0), BytePos::from_usize(source.len())),
                None,
            );
            let mut parser = Parser::new_from(lexer);
            match parser.parse_script() {
                Ok(script) if parser.take_errors().is_empty() => Walk::script(&script.body),
                _ => false,
            }
        })
    }

    pub(crate) fn module(source: &str) -> bool {
        GLOBALS.set(&Default::default(), || {
            let lexer = Lexer::new(
                Syntax::Es(EsSyntax::default()),
                Default::default(),
                StringInput::new(source, BytePos::from_usize(0), BytePos::from_usize(source.len())),
                None,
            );
            let mut parser = Parser::new_from(lexer);
            match parser.parse_module() {
                Ok(module) if parser.take_errors().is_empty() => Walk::module(&module.body),
                _ => false,
            }
        })
    }

    fn listed(table: &[(&str, &[&str])], owner: &str, name: &str) -> bool {
        table.iter().any(|(listed, names)| *listed == owner && names.contains(&name))
    }

    fn library(source: &str, specifier: &ImportSpecifier) -> Lib {
        let imported = match specifier {
            ImportSpecifier::Default(_) => Some("default"),
            ImportSpecifier::Namespace(_) => None,
            ImportSpecifier::Named(named) => match &named.imported {
                None => Some(named.local.sym.as_ref()),
                Some(ModuleExportName::Ident(ident)) => Some(ident.sym.as_ref()),
                Some(ModuleExportName::Str(_)) => None,
            },
        };
        match (source, imported) {
            ("dayjs", Some("default")) => Lib::Dayjs,
            ("big.js", Some("default" | "Big")) => Lib::Big,
            ("zen", Some("default") | None) => Lib::Zen,
            _ => Lib::Opaque,
        }
    }
}

impl Walk {
    fn new(dialect: Dialect) -> Self {
        Walk {
            dialect,
            collect: true,
            sites: HashMap::new(),
            classes: HashMap::new(),
            pure: true,
        }
    }

    fn script(body: &[Stmt]) -> bool {
        let mut decls = Vec::new();
        for stmt in body {
            match stmt {
                Stmt::Decl(decl) => decls.push(decl),
                Stmt::Empty(_) => {}
                _ => return false,
            }
        }
        Walk::new(Dialect::Script).walk(&decls)
    }

    fn module(items: &[ModuleItem]) -> bool {
        let mut walk = Walk::new(Dialect::Module);
        let mut decls = Vec::new();
        for item in items {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => walk.import(import),
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => decls.push(&export.decl),
                ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(named)) if named.src.is_none() => {}
                ModuleItem::Stmt(Stmt::Decl(decl)) => decls.push(decl),
                ModuleItem::Stmt(Stmt::Empty(_)) => {}
                _ => return false,
            }
        }
        walk.walk(&decls)
    }

    fn walk(mut self, decls: &[&Decl]) -> bool {
        if !self.top(decls) {
            return false;
        }
        self.top_level(decls);
        if !self.classify() {
            return false;
        }
        self.collect = false;
        self.top_level(decls);
        self.pure
    }

    fn import(&mut self, import: &ImportDecl) {
        let source = import.src.value.to_string_lossy();
        for specifier in &import.specifiers {
            let local = match specifier {
                ImportSpecifier::Named(named) => &named.local,
                ImportSpecifier::Default(default) => &default.local,
                ImportSpecifier::Namespace(namespace) => &namespace.local,
            };
            self.site(local.sym.as_ref(), Site::Lib(Purity::library(&source, specifier)));
        }
    }

    fn top(&self, decls: &[&Decl]) -> bool {
        let mut handlers = 0;
        for decl in decls {
            match decl {
                Decl::Fn(declared) => {
                    let name = declared.ident.sym.as_ref();
                    handlers += usize::from(name == "handler");
                    if !self.declarable(name) {
                        return false;
                    }
                }
                Decl::Var(var) if var.kind == VarDeclKind::Const => {
                    for declarator in &var.decls {
                        let (Pat::Ident(binding), Some(init)) = (&declarator.name, &declarator.init) else {
                            return false;
                        };
                        let name = binding.id.sym.as_ref();
                        if name == "handler" {
                            if !matches!(&**init, Expr::Arrow(_) | Expr::Fn(_)) {
                                return false;
                            }
                            handlers += 1;
                        } else if !self.declarable(name) || !Self::inert(init) {
                            return false;
                        }
                    }
                }
                _ => return false,
            }
        }
        self.dialect == Dialect::Module || handlers == 1
    }

    fn declarable(&self, name: &str) -> bool {
        self.dialect == Dialect::Module || name == "handler" || !Purity::RESERVED.contains(&name)
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

    fn classify(&mut self) -> bool {
        for (name, sites) in &self.sites {
            let Some(&first) = sites.first() else {
                return false;
            };
            if sites.iter().any(|site| *site != first) {
                return false;
            }
            let class = match first {
                Site::TopFn => Class::TopFn,
                Site::TopConst => Class::TopConst,
                Site::Local => Class::Local,
                Site::Lib(lib) => Class::Lib(lib),
            };
            self.classes.insert(name.clone(), class);
        }
        true
    }

    fn class(&self, name: &str) -> Class {
        match (self.classes.get(name), self.dialect, name) {
            (Some(class), _, _) => *class,
            (None, Dialect::Script, "dayjs") => Class::Lib(Lib::Dayjs),
            (None, Dialect::Script, "Big") => Class::Lib(Lib::Big),
            (None, _, _) => Class::Global,
        }
    }

    fn site(&mut self, name: &str, site: Site) {
        if self.collect {
            self.sites.entry(name.to_string()).or_default().push(site);
        }
    }

    fn reject(&mut self) {
        self.check(false);
    }

    fn check(&mut self, ok: bool) {
        if !self.collect {
            self.pure &= ok;
        }
    }

    fn top_level(&mut self, decls: &[&Decl]) {
        for decl in decls {
            match decl {
                Decl::Fn(declared) => {
                    let handler = declared.ident.sym.as_ref() == "handler";
                    self.site(declared.ident.sym.as_ref(), Site::TopFn);
                    self.function(&declared.function, handler);
                }
                Decl::Var(var) => {
                    for declarator in &var.decls {
                        let (Pat::Ident(binding), Some(init)) = (&declarator.name, &declarator.init) else {
                            continue;
                        };
                        let name = binding.id.sym.as_ref();
                        let handler = name == "handler";
                        match &**init {
                            Expr::Arrow(arrow) => {
                                self.site(name, Site::TopFn);
                                self.arrow(arrow, handler);
                            }
                            Expr::Fn(function) => {
                                self.site(name, Site::TopFn);
                                if let Some(ident) = &function.ident {
                                    self.site(ident.sym.as_ref(), Site::Local);
                                }
                                self.function(&function.function, handler);
                            }
                            _ => self.site(name, Site::TopConst),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn function(&mut self, function: &Function, handler: bool) {
        if function.is_generator || !function.decorators.is_empty() || !self.awaits(function.is_async) {
            return self.reject();
        }
        let params: Vec<&Pat> = function.params.iter().map(|param| &param.pat).collect();
        self.check(function.params.iter().all(|param| param.decorators.is_empty()));
        self.params(&params, handler);
        match &function.body {
            Some(body) => self.block(body),
            None => self.reject(),
        }
    }

    fn arrow(&mut self, arrow: &ArrowExpr, handler: bool) {
        if arrow.is_generator || !self.awaits(arrow.is_async) {
            return self.reject();
        }
        let params: Vec<&Pat> = arrow.params.iter().collect();
        self.params(&params, handler);
        match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(body) => self.block(body),
            BlockStmtOrExpr::Expr(expr) => self.expr(expr, Use::Value),
        }
    }

    fn awaits(&self, asynchronous: bool) -> bool {
        !asynchronous || self.dialect == Dialect::Module
    }

    fn params(&mut self, params: &[&Pat], handler: bool) {
        for (index, param) in params.iter().enumerate() {
            if handler && index == 1 && self.dialect == Dialect::Script {
                self.helpers(param);
            } else {
                self.pat(param);
            }
        }
    }

    fn helpers(&mut self, pat: &Pat) {
        let Pat::Object(object) = pat else {
            return self.reject();
        };
        for prop in &object.props {
            let (key, binding) = match prop {
                ObjectPatProp::Assign(assign) if assign.value.is_none() => (assign.key.id.sym.as_ref(), assign.key.id.sym.as_ref()),
                ObjectPatProp::KeyValue(pair) => match (&pair.key, &*pair.value) {
                    (PropName::Ident(key), Pat::Ident(binding)) => (key.sym.as_ref(), binding.id.sym.as_ref()),
                    _ => return self.reject(),
                },
                _ => return self.reject(),
            };
            let site = match key {
                "moment" | "dayjs" => Site::Lib(Lib::Dayjs),
                "Big" => Site::Lib(Lib::Big),
                "env" => Site::Local,
                _ => return self.reject(),
            };
            self.site(binding, site);
        }
    }

    fn pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(binding) => self.site(binding.id.sym.as_ref(), Site::Local),
            Pat::Array(array) => {
                for element in array.elems.iter().flatten() {
                    self.pat(element);
                }
            }
            Pat::Rest(rest) => self.pat(&rest.arg),
            Pat::Object(object) => {
                for prop in &object.props {
                    match prop {
                        ObjectPatProp::KeyValue(pair) => {
                            self.key(&pair.key, true);
                            self.pat(&pair.value);
                        }
                        ObjectPatProp::Assign(assign) => {
                            let name = assign.key.id.sym.as_ref();
                            self.check(!Purity::BANNED.contains(&name) && !Purity::MEMBERS.contains(&name));
                            self.site(name, Site::Local);
                            if let Some(value) = &assign.value {
                                self.expr(value, Use::Value);
                            }
                        }
                        ObjectPatProp::Rest(rest) => self.pat(&rest.arg),
                    }
                }
            }
            Pat::Assign(assign) => {
                self.pat(&assign.left);
                self.expr(&assign.right, Use::Value);
            }
            Pat::Expr(_) | Pat::Invalid(_) => self.reject(),
        }
    }

    fn key(&mut self, key: &PropName, read: bool) {
        match key {
            PropName::Ident(ident) => {
                let name = ident.sym.as_ref();
                self.check(name != "__proto__" && !(read && (Purity::BANNED.contains(&name) || Purity::MEMBERS.contains(&name))));
            }
            PropName::Str(text) => {
                let name = text.value.to_string_lossy();
                self.check(name != "__proto__" && !(read && (Purity::BANNED.contains(&name.as_ref()) || Purity::MEMBERS.contains(&name.as_ref()))));
            }
            PropName::Num(_) | PropName::BigInt(_) => {}
            PropName::Computed(computed) => match read {
                true => self.reject(),
                false => self.expr(&computed.expr, Use::Value),
            },
        }
    }

    fn block(&mut self, block: &BlockStmt) {
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn var(&mut self, var: &VarDecl) {
        if var.declare {
            return self.reject();
        }
        for declarator in &var.decls {
            self.pat(&declarator.name);
            if let Some(init) = &declarator.init {
                self.expr(init, Use::Value);
            }
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Block(block) => self.block(block),
            Stmt::Empty(_) | Stmt::Debugger(_) | Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::Return(ret) => {
                if let Some(arg) = &ret.arg {
                    self.expr(arg, Use::Value);
                }
            }
            Stmt::Labeled(labeled) => self.stmt(&labeled.body),
            Stmt::If(branch) => {
                self.expr(&branch.test, Use::Consumed);
                self.stmt(&branch.cons);
                if let Some(alt) = &branch.alt {
                    self.stmt(alt);
                }
            }
            Stmt::Switch(switch) => {
                self.expr(&switch.discriminant, Use::Consumed);
                for case in &switch.cases {
                    if let Some(test) = &case.test {
                        self.expr(test, Use::Consumed);
                    }
                    for stmt in &case.cons {
                        self.stmt(stmt);
                    }
                }
            }
            Stmt::Throw(throw) => self.expr(&throw.arg, Use::Value),
            Stmt::Try(attempt) => {
                self.block(&attempt.block);
                if let Some(handler) = &attempt.handler {
                    if let Some(param) = &handler.param {
                        self.pat(param);
                    }
                    self.block(&handler.body);
                }
                if let Some(finalizer) = &attempt.finalizer {
                    self.block(finalizer);
                }
            }
            Stmt::While(looped) => {
                self.expr(&looped.test, Use::Consumed);
                self.stmt(&looped.body);
            }
            Stmt::DoWhile(looped) => {
                self.stmt(&looped.body);
                self.expr(&looped.test, Use::Consumed);
            }
            Stmt::For(looped) => {
                match &looped.init {
                    Some(VarDeclOrExpr::VarDecl(var)) => self.var(var),
                    Some(VarDeclOrExpr::Expr(expr)) => self.expr(expr, Use::Value),
                    None => {}
                }
                if let Some(test) = &looped.test {
                    self.expr(test, Use::Consumed);
                }
                if let Some(update) = &looped.update {
                    self.expr(update, Use::Value);
                }
                self.stmt(&looped.body);
            }
            Stmt::ForIn(looped) => {
                self.head(&looped.left);
                self.expr(&looped.right, Use::Value);
                self.stmt(&looped.body);
            }
            Stmt::ForOf(looped) => {
                if looped.is_await {
                    return self.reject();
                }
                self.head(&looped.left);
                self.expr(&looped.right, Use::Value);
                self.stmt(&looped.body);
            }
            Stmt::Decl(Decl::Var(var)) => self.var(var),
            Stmt::Decl(Decl::Fn(decl)) => {
                if decl.declare {
                    return self.reject();
                }
                self.site(decl.ident.sym.as_ref(), Site::Local);
                self.function(&decl.function, false);
            }
            Stmt::Expr(expr) => self.expr(&expr.expr, Use::Value),
            Stmt::With(_) | Stmt::Decl(_) => self.reject(),
        }
    }

    fn head(&mut self, head: &ForHead) {
        match head {
            ForHead::VarDecl(var) => self.var(var),
            ForHead::Pat(pat) => self.target_pat(pat),
            ForHead::UsingDecl(_) => self.reject(),
        }
    }

    fn name(prop: &MemberProp) -> Option<String> {
        match prop {
            MemberProp::Ident(ident) => Some(ident.sym.to_string()),
            MemberProp::Computed(computed) => match &*computed.expr {
                Expr::Lit(Lit::Str(text)) => Some(text.value.to_string_lossy().into_owned()),
                Expr::Lit(Lit::Num(number)) => Some(number.value.to_string()),
                _ => None,
            },
            MemberProp::PrivateName(_) => None,
        }
    }

    fn global(&self, expr: &Expr) -> Option<(Class, String)> {
        match expr {
            Expr::Ident(ident) => Some((self.class(ident.sym.as_ref()), ident.sym.to_string())),
            Expr::Paren(inner) => self.global(&inner.expr),
            _ => None,
        }
    }

    fn member(&mut self, member: &MemberExpr, usage: Use) {
        let Some(name) = Self::name(&member.prop) else {
            return self.reject();
        };
        if Purity::BANNED.contains(&name.as_str()) {
            return self.reject();
        }
        if let Some((class, object)) = self.global(&member.obj) {
            match class {
                Class::Global | Class::Lib(_) | Class::TopFn => {
                    let constant = class == Class::Global
                        && (Purity::listed(Purity::CONSTANTS, &object, &name)
                            || (self.dialect == Dialect::Module && Purity::listed(Purity::MODULE_CONSTANTS, &object, &name)));
                    return self.check(constant);
                }
                Class::TopConst | Class::Local => {}
            }
        }
        if usage == Use::Value && Purity::MEMBERS.contains(&name.as_str()) {
            return self.reject();
        }
        self.expr(&member.obj, Use::Value);
    }

    fn args(&mut self, args: &[ExprOrSpread]) {
        for arg in args {
            self.expr(&arg.expr, Use::Value);
        }
    }

    fn call(&mut self, callee: &Expr, args: &[ExprOrSpread]) {
        match callee {
            Expr::Paren(inner) => return self.call(&inner.expr, args),
            Expr::Arrow(arrow) => self.arrow(arrow, false),
            Expr::Fn(function) => {
                if let Some(ident) = &function.ident {
                    self.site(ident.sym.as_ref(), Site::Local);
                }
                self.function(&function.function, false);
            }
            Expr::Ident(ident) => {
                let name = ident.sym.as_ref();
                let allowed = match self.class(name) {
                    Class::Local | Class::TopFn | Class::TopConst => true,
                    Class::Lib(Lib::Big) => args.len() <= 1 && args.iter().all(|arg| arg.spread.is_none()),
                    Class::Lib(Lib::Dayjs) => Self::dated(args),
                    Class::Lib(Lib::Zen | Lib::Opaque) => false,
                    Class::Global => Purity::CALLABLE.contains(&name),
                };
                self.check(allowed);
            }
            Expr::Member(member) => self.method(member),
            Expr::OptChain(chain) => match &*chain.base {
                OptChainBase::Member(member) => self.method(member),
                OptChainBase::Call(_) => self.reject(),
            },
            _ => self.reject(),
        }
        self.args(args);
    }

    fn dated(args: &[ExprOrSpread]) -> bool {
        if args.iter().any(|arg| arg.spread.is_some()) {
            return false;
        }
        match args {
            [] | [_] => true,
            [_, format] => matches!(&*format.expr, Expr::Lit(Lit::Str(_))),
            _ => false,
        }
    }

    fn method(&mut self, member: &MemberExpr) {
        let Some(name) = Self::name(&member.prop) else {
            return self.reject();
        };
        if let Some((class, object)) = self.global(&member.obj) {
            match class {
                Class::Global => {
                    let allowed = Purity::listed(Purity::STATICS, &object, &name)
                        || (self.dialect == Dialect::Module && Purity::listed(Purity::MODULE_STATICS, &object, &name));
                    return self.check(allowed);
                }
                Class::Lib(Lib::Dayjs) => return self.check(matches!(name.as_str(), "unix" | "isDayjs")),
                Class::Lib(Lib::Zen) => return self.check(Purity::ZEN.contains(&name.as_str())),
                Class::Lib(Lib::Big | Lib::Opaque) | Class::TopFn => return self.reject(),
                Class::TopConst | Class::Local => {}
            }
        }
        if !Purity::METHODS.contains(&name.as_str()) {
            return self.reject();
        }
        self.expr(&member.obj, Use::Value);
    }

    fn target(&mut self, target: &SimpleAssignTarget) {
        match target {
            SimpleAssignTarget::Ident(binding) => {
                let local = self.class(binding.id.sym.as_ref()) == Class::Local;
                self.check(local);
            }
            SimpleAssignTarget::Member(member) => self.written(member),
            SimpleAssignTarget::Paren(inner) => match &*inner.expr {
                Expr::Ident(ident) => {
                    let local = self.class(ident.sym.as_ref()) == Class::Local;
                    self.check(local);
                }
                Expr::Member(member) => self.written(member),
                _ => self.reject(),
            },
            _ => self.reject(),
        }
    }

    fn written(&mut self, member: &MemberExpr) {
        let Some(name) = Self::name(&member.prop) else {
            return self.reject();
        };
        if Purity::BANNED.contains(&name.as_str()) || name.starts_with('$') {
            return self.reject();
        }
        self.expr(&member.obj, Use::Value);
    }

    fn target_pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(binding) => {
                let local = self.class(binding.id.sym.as_ref()) == Class::Local;
                self.check(local);
            }
            Pat::Expr(expr) => match &**expr {
                Expr::Member(member) => self.written(member),
                Expr::Ident(ident) => {
                    let local = self.class(ident.sym.as_ref()) == Class::Local;
                    self.check(local);
                }
                _ => self.reject(),
            },
            Pat::Array(array) => {
                for element in array.elems.iter().flatten() {
                    self.target_pat(element);
                }
            }
            Pat::Rest(rest) => self.target_pat(&rest.arg),
            Pat::Object(object) => {
                for prop in &object.props {
                    match prop {
                        ObjectPatProp::KeyValue(pair) => {
                            self.key(&pair.key, true);
                            self.target_pat(&pair.value);
                        }
                        ObjectPatProp::Assign(assign) => {
                            let name = assign.key.id.sym.as_ref();
                            let local = self.class(name) == Class::Local;
                            self.check(local);
                            self.check(!Purity::BANNED.contains(&name) && !Purity::MEMBERS.contains(&name));
                            if let Some(value) = &assign.value {
                                self.expr(value, Use::Value);
                            }
                        }
                        ObjectPatProp::Rest(rest) => self.target_pat(&rest.arg),
                    }
                }
            }
            Pat::Assign(assign) => {
                self.target_pat(&assign.left);
                self.expr(&assign.right, Use::Value);
            }
            Pat::Invalid(_) => self.reject(),
        }
    }

    fn ident(&mut self, name: &str, usage: Use) {
        if self.collect {
            return;
        }
        let allowed = match self.class(name) {
            Class::Local | Class::TopConst => true,
            Class::TopFn | Class::Lib(_) => usage == Use::Consumed,
            Class::Global => {
                let reserved = self.dialect == Dialect::Script && Purity::RESERVED.contains(&name);
                Purity::VALUES.contains(&name) || (usage == Use::Consumed && name != "arguments" && !reserved)
            }
        };
        self.check(allowed);
    }

    fn expr(&mut self, expr: &Expr, usage: Use) {
        match expr {
            Expr::Lit(_) => {}
            Expr::Ident(ident) => self.ident(ident.sym.as_ref(), usage),
            Expr::Paren(inner) => self.expr(&inner.expr, usage),
            Expr::Array(array) => {
                for element in array.elems.iter().flatten() {
                    self.expr(&element.expr, Use::Value);
                }
            }
            Expr::Object(object) => {
                for prop in &object.props {
                    match prop {
                        PropOrSpread::Spread(spread) => self.expr(&spread.expr, Use::Value),
                        PropOrSpread::Prop(prop) => match &**prop {
                            Prop::Shorthand(ident) => {
                                self.check(ident.sym.as_ref() != "__proto__");
                                self.ident(ident.sym.as_ref(), Use::Value);
                            }
                            Prop::KeyValue(pair) => {
                                self.key(&pair.key, false);
                                self.expr(&pair.value, Use::Value);
                            }
                            Prop::Getter(getter) => {
                                self.key(&getter.key, false);
                                match &getter.body {
                                    Some(body) => self.block(body),
                                    None => self.reject(),
                                }
                            }
                            Prop::Setter(setter) => {
                                self.key(&setter.key, false);
                                if setter.this_param.is_some() {
                                    return self.reject();
                                }
                                self.pat(&setter.param);
                                match &setter.body {
                                    Some(body) => self.block(body),
                                    None => self.reject(),
                                }
                            }
                            Prop::Method(method) => {
                                self.key(&method.key, false);
                                self.function(&method.function, false);
                            }
                            Prop::Assign(_) => self.reject(),
                        },
                    }
                }
            }
            Expr::Fn(function) => {
                if let Some(ident) = &function.ident {
                    self.site(ident.sym.as_ref(), Site::Local);
                }
                self.function(&function.function, false);
            }
            Expr::Arrow(arrow) => self.arrow(arrow, false),
            Expr::Unary(unary) => match unary.op {
                UnaryOp::Delete => match &*unary.arg {
                    Expr::Member(member) => self.written(member),
                    _ => self.reject(),
                },
                _ => self.expr(&unary.arg, Use::Consumed),
            },
            Expr::Update(update) => match &*update.arg {
                Expr::Ident(ident) => {
                    let local = self.class(ident.sym.as_ref()) == Class::Local;
                    self.check(local);
                }
                Expr::Member(member) => self.written(member),
                _ => self.reject(),
            },
            Expr::Bin(bin) => match bin.op {
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::NullishCoalescing => {
                    self.expr(&bin.left, usage);
                    self.expr(&bin.right, usage);
                }
                BinaryOp::In => {
                    self.expr(&bin.left, Use::Consumed);
                    self.expr(&bin.right, Use::Value);
                }
                BinaryOp::InstanceOf => {
                    self.expr(&bin.left, Use::Consumed);
                    let constructor = match self.global(&bin.right) {
                        Some((Class::Global, name)) => Purity::CONSTRUCTORS.contains(&name.as_str()),
                        Some((Class::Lib(Lib::Big), _)) => true,
                        _ => false,
                    };
                    if !constructor {
                        self.expr(&bin.right, Use::Value);
                    }
                }
                _ => {
                    self.expr(&bin.left, Use::Consumed);
                    self.expr(&bin.right, Use::Consumed);
                }
            },
            Expr::Assign(assign) => {
                match &assign.left {
                    AssignTarget::Simple(target) => self.target(target),
                    AssignTarget::Pat(AssignTargetPat::Array(array)) => {
                        for element in array.elems.iter().flatten() {
                            self.target_pat(element);
                        }
                    }
                    AssignTarget::Pat(AssignTargetPat::Object(object)) => {
                        self.target_pat(&Pat::Object(object.clone()));
                    }
                    AssignTarget::Pat(AssignTargetPat::Invalid(_)) => self.reject(),
                }
                self.expr(&assign.right, Use::Value);
            }
            Expr::Member(member) => self.member(member, usage),
            Expr::Cond(cond) => {
                self.expr(&cond.test, Use::Consumed);
                self.expr(&cond.cons, usage);
                self.expr(&cond.alt, usage);
            }
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => self.call(callee, &call.args),
                Callee::Super(_) | Callee::Import(_) => self.reject(),
            },
            Expr::New(new) => {
                let constructor = match self.global(&new.callee) {
                    Some((Class::Global, name)) => Purity::CONSTRUCTORS.contains(&name.as_str()),
                    Some((Class::Lib(Lib::Big), _)) => true,
                    _ => false,
                };
                self.check(constructor);
                if let Some(args) = &new.args {
                    self.args(args);
                }
            }
            Expr::Seq(seq) => {
                let last = seq.exprs.len().saturating_sub(1);
                for (index, expr) in seq.exprs.iter().enumerate() {
                    self.expr(expr, if index == last { usage } else { Use::Consumed });
                }
            }
            Expr::Tpl(template) => {
                for expr in &template.exprs {
                    self.expr(expr, Use::Consumed);
                }
            }
            Expr::Await(awaited) if self.dialect == Dialect::Module => self.expr(&awaited.arg, Use::Value),
            Expr::OptChain(chain) => match &*chain.base {
                OptChainBase::Member(member) => self.member(member, usage),
                OptChainBase::Call(call) => self.call(&call.callee, &call.args),
            },
            _ => self.reject(),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::Purity;
    use crate::nodes::function::v2::serde::{JsValue, JsValueWithNodes};
    use crate::nodes::NodeHandlerExtensions;
    use zen_expression::Variable;

    #[test]
    fn plain_modules_share_a_runtime() {
        let sources = [
            "export const handler = async (input) => { let x = 1; x++; return { x, y: `a${input.a}b` }; };",
            "const RATE = 0.2\nexport async function handler(input) { return { v: input.v * RATE } }",
            "function helper(a) { return a + 1 }\nexport const handler = (input) => ({ v: helper(input.v) })",
            "import dayjs from 'dayjs';\nexport const handler = async (input) => { const out = {}; out.at = dayjs(input.when).format(); const list = []; list.push(1); input.seen = true; return { out, list }; };",
            "export const handler = async (input) => { const items = input.items.map((i) => ({ ...i, total: i.qty * i.price })); items.sort((a, b) => a.total - b.total); return { items }; };",
            "export const handler = async (input) => ({ hello: 'world', multiplied: input.input * 2 })",
            "import zen from 'zen';\n\n/** @type {Handler} **/\nexport const handler = async (input) => {\n  return {\n    functionParentNodes: input['$nodes'].request['$nodes']\n, functionRequest: input['$nodes'].request\n  };\n};\n",
            "import Big from 'big.js';\nexport const handler = async (input) => {\n  const total = input.items.reduce((sum, item) => sum.plus(Big(item.price).times(item.qty)), Big(0));\n  console.log('total', total.toString());\n  return { total: total.toNumber(), depth: config.maxDepth };\n};",
            "import zen from 'zen';\nexport const handler = async (input) => {\n  const helper = async (value) => value * 2;\n  const doubled = await helper(input.n);\n  return { doubled, sum: await zen.evaluateExpression('a + b', { a: 1, b: input.n }) };\n};",
        ];
        for source in sources {
            assert!(Purity::module(source), "{source}");
        }
    }

    #[test]
    fn shared_state_needs_a_fresh_runtime() {
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
            "export const handler = async (input) => { const f = () => Math; f().seen = (f().seen || 0) + 1; return { n: f().seen } }",
            "export const handler = async (input) => { const o = { m: Math }; o.m.seen = (o.m.seen || 0) + 1; return { n: o.m.seen } }",
            "export const handler = async (input) => { const bump = (t) => { t.seen = (t.seen || 0) + 1; return t.seen }; return { n: bump(JSON) } }",
            "export const handler = async (input) => { const c = config; c.seen = 1; return {} }",
            "export const handler = async (input) => ({ c: [config][0] })",
            "import { z } from 'zod';\nexport const handler = async (input) => ({ ok: z.string().safeParse(input.s).success })",
            "import zen from 'zen';\nexport const handler = async (input) => zen.evaluate('child', input)",
            "import http from 'http';\nexport const handler = async (input) => (await http.get('https://example.com')).data",
            "export const handler = async (input) => { await console.sleep(5); return {} }",
            "export const handler = async (input) => { const log = console.log; log(1); return {} }",
            "export const handler = async (input) => ({ v: await Promise.resolve(input.n) })",
            "export const handler = async (input) => { input.then((x) => x); return {} }",
            "export function* handler(input) { yield 1 }",
            "export default async (input) => ({})",
            "export { handler } from './other';",
            "import other from 'other';\nexport const handler = async (input) => other(input)",
            "export const handler = async (input) => ({ v: input[input.key] })",
            "export const handler = async (input) => ({ v: this })",
            "export const handler = async (input) => ({ v: import.meta })",
            "export class Handler {}\nexport const handler = async (input) => ({})",
        ];
        for source in sources {
            assert!(!Purity::module(source), "{source}");
        }
    }

    #[tokio::test]
    async fn denied_members_cover_the_module_realm() {
        let extensions = NodeHandlerExtensions::default();
        let function = extensions.function_runtime().await.expect("runtime");
        let probe = r#"
            import dayjs from 'dayjs';
            import Big from 'big.js';
            const iterators = [[].values(), ''[Symbol.iterator](), new Map().values(), new Set().values(), 'a'.matchAll(/a/g)];
            const protos = [Object.prototype, Function.prototype, Array.prototype, String.prototype, Number.prototype,
                Boolean.prototype, Symbol.prototype, BigInt.prototype, Date.prototype, RegExp.prototype, Error.prototype,
                Map.prototype, Set.prototype, WeakMap.prototype, WeakSet.prototype, Promise.prototype,
                Object.getPrototypeOf(Big(1)), Object.getPrototypeOf(dayjs())];
            for (const iterator of iterators) {
                for (let proto = Object.getPrototypeOf(iterator); proto && proto !== Object.prototype; proto = Object.getPrototypeOf(proto)) {
                    protos.push(proto);
                }
            }
            const names = new Set();
            for (const proto of protos) {
                for (const key of Object.getOwnPropertyNames(proto)) {
                    const value = Object.getOwnPropertyDescriptor(proto, key).value;
                    if (typeof value === 'function' || (typeof value === 'object' && value !== null)) {
                        names.add(key);
                    }
                }
            }
            for (const instance of [Big(1), dayjs(), new Date(), /a/, [], {}, new Map(), new Set(), new Error('x')]) {
                for (const key of Object.getOwnPropertyNames(instance)) {
                    if (typeof instance[key] === 'function') {
                        names.add(key);
                    }
                }
            }
            export const handler = async () => [...names].join(',');
        "#;
        let name = function.shared_module("probe", probe).await.expect("module");
        let response = function
            .call_handler(&name, JsValueWithNodes(JsValue(Variable::Null)))
            .await
            .expect("names");
        let names = response.data.as_str().map(str::to_string).unwrap_or_default();
        assert!(!names.is_empty());
        let missing: Vec<&str> = names
            .split(',')
            .filter(|name| !Purity::MEMBERS.contains(name) && !Purity::BANNED.contains(name))
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn plain_handlers_share_a_context() {
        let sources = [
            "const handler = (input, { moment, env }) => {\n  return { output: input.input * 2 };\n}",
            "const handler = (input, { moment, env }) => {\n  const count = (flag) => Object.values(input?.flag || {}).reduce((acc, curr) => {\n    if (curr === flag) return acc + 1;\n    return acc;\n  }, 0);\n  return { critical: count('critical'), red: count('red') };\n}",
            "const handler = (input, { dayjs, Big }) => {\n  const flags = input.flags || {};\n  let overall = 'green';\n  if (Object.values(flags).some((f) => f === 'red')) {\n    overall = 'red';\n  }\n  return { ...input, overall };\n}",
            "const handler = (input, { dayjs, Big }) => {\n  const round = (val) => val?.toFixed !== undefined ? val.round(2).toNumber() : val;\n  const base = Big(input.base.pi).times(Big(input.tax.gst).div(100));\n  return { breakdown: [{ key: 'premium', value: round(base) }] };\n}",
            "const RATE = 0.2;\nfunction helper(a) { return a * RATE }\nconst handler = (input) => {\n  const out = [];\n  for (const item of input.items) { out.push({ ...item, v: helper(item.v) }); }\n  out.sort((a, b) => a.v - b.v);\n  const total = { sum: 0 };\n  out.forEach((item) => { total.sum += item.v; });\n  return { out, total, at: dayjs(input.when).add(1, 'day').format('YYYY-MM-DD') };\n}",
            "function handler(input, { dayjs: d }) {\n  const map = new Map();\n  map.set('a', 1);\n  try { return { v: map.has('a'), when: d(input.when).isValid(), n: Math.max(1, Number(input.n)) } } catch (e) { return { e: e.message } }\n}",
        ];
        for source in sources {
            assert!(Purity::layout(source).is_some(), "{source}");
        }
    }

    #[test]
    fn shared_state_keeps_a_fresh_context() {
        let sources = [
            "let count = 0;\nconst handler = () => ({ n: ++count });",
            "const cache = {};\nconst handler = (input) => { cache.x = 1; return {}; };",
            "const handler = () => { globalThis.flag = 1; return {}; };",
            "var hits = 0\nconst handler = () => ({ hits: hits++ })",
            "const handler = (input, { Big }) => { Big.DP = 2; return {} }",
            "const handler = (input, { dayjs }) => { dayjs.locale('de'); return {} }",
            "const handler = (input, { dayjs }) => { const d = dayjs; d.x = 1; return {} }",
            "const handler = (input, { dayjs }) => ({ d: dayjs(input.at, { locale: { name: 'x' } }) })",
            "const handler = (input, { dayjs }) => { const d = dayjs(); d.$L = { name: 'x' }; return { v: d.add(1, 'day') } }",
            "const handler = (input, { dayjs }) => ({ u: dayjs().get(input.unit) })",
            "const handler = (input) => { const o = { map: dayjs().add }; return o.map(1) }",
            "const handler = (input) => { const f = [].push; return {} }",
            "const handler = (input) => ({ v: input[input.key] })",
            "const handler = (input) => { input[input.key] = 1; return input }",
            "const handler = (input) => { const p = input.constructor; return {} }",
            "const handler = (input) => { console.log(input); return {} }",
            "const handler = (input) => { log.push(1); return {} }",
            "function helper() { helper.calls = (helper.calls || 0) + 1; return helper.calls }\nconst handler = () => ({ n: helper() })",
            "const handler = () => { Math.seen = 1; return {} }",
            "const handler = () => { const m = Math; return {} }",
            "const handler = (input, { Big }) => ({ v: Big(input.a).mod(3) })",
            "const handler = async (input) => ({ v: await input })",
            "const handler = (input) => ({ v: this })",
            "const handler = (input) => ({ v: eval('1') })",
            "const handler = (input) => ({ v: Function('return 1')() })",
            "const handler = (input) => ({ v: [1].map.call(input, (x) => x) })",
            "const handler = (input) => ({ v: input.list.map((x) => x)() })",
            "const handler = (input) => ({ __proto__: input })",
            "const handler = (input) => { const { constructor } = input; return {} }",
            "const handler = (input) => ({ v: `a\\\nb` })",
            "const handler = (input) => ({ v: 1 })\r\n",
            "const handler = (input) => ({ v: typeof __zenRow })",
            "const handler = (input) => ({ v: input.at > now })",
            "const handler = (input) => ({ v: typeof main })",
            "console.log('loaded');\nconst handler = () => ({})",
            "const pattern = /a/g;\nconst handler = (input) => ({ ok: pattern.test(input.s) })",
            "const handler = (input, helpers) => ({ v: helpers.dayjs() })",
            "const handler = (input) => ({})\nconst handler2 = handler;",
            "const main = 1;\nconst handler = (input) => ({})",
            "const handler = (input) => { handler.x = 1; return {} }",
            "const handler = (input) => ({ f: handler })",
            "class A {}\nconst handler = (input) => ({})",
            "const handler = (input) => { x = 1; return {} }",
            "const handler = (input) => { const count = 1; return {} }\nconst count = () => 1;",
            "const handler = (input) => ({ v: Object.assign(input, {}) })",
            "const handler = (input) => { Reflect.set(input, 'a', 1); return {} }",
            "const handler = (input) => { const f = () => Math; f().seen = (f().seen || 0) + 1; return { n: f().seen } }",
            "const handler = (input) => { const o = { m: Math }; o.m.seen = (o.m.seen || 0) + 1; return { n: o.m.seen } }",
            "const handler = (input) => { const bump = (t) => { t.seen = (t.seen || 0) + 1; return t.seen }; return { n: bump(JSON) } }",
        ];
        for source in sources {
            assert!(Purity::layout(source).is_none(), "{source}");
        }
    }

    #[test]
    fn fixture_handlers_share_a_context() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/");
        let files = ["function.json", "credit-analysis.json", "graphs/aml.json", "graphs/company-analysis.json", "graphs/insurance-breakdown.json"];
        for file in files {
            let text = std::fs::read_to_string(format!("{root}{file}")).unwrap();
            let graph: serde_json::Value = serde_json::from_str(&text).unwrap();
            let sources: Vec<&str> = graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| node["type"] == "functionNode")
                .filter_map(|node| node["content"].as_str())
                .collect();
            assert!(!sources.is_empty(), "{file}");
            for source in sources {
                assert!(Purity::layout(source).is_some(), "{file}");
            }
        }
    }

    #[test]
    fn throwing_handlers_share_a_context() {
        let sources = [
            "/**\n * é ünïcödé — 日本\n */\nconst handler = (input, { moment, env }) => {\n  const label = `row\n${input.n} é`;\n  if (input.n % 3 === 0) {\n    throw new Error('boom ' + input.n + label);\n  }\n  return { label };\n}",
            "const handler = (input, { Big }) => ({ v: Big(input.n % 4 === 0 ? 'x' : input.n).plus(1).toString() })",
            "const handler = (input) => ({ v: input.n % 5 === 0 ? 10n : 1 })",
            "const handler = (input) => {\n  const o = input.n > 5 ? input.items : null;\n  return { v: o.length };\n}",
            "const handler = (input) => input.n % 2 ? undefined : { a: 1 }",
            "const deep = (n) => deep(n + 1);\nconst handler = (input) => {\n  if (input.n === 7) {\n    deep(0);\n  }\n  return { ok: true };\n}",
            "const handler = (input) => {\n  if (input.n === 4) throw { code: input.n, items: input.items };\n  if (input.n === 8) throw 'plain ' + input.n;\n  return { list: input.items.map((x) => x * 1.5), s: input.s.toUpperCase() };\n}",
            "const handler = (input, { moment, env }) => {\n  while (true) {}\n  return input;\n}",
        ];
        for source in sources {
            assert!(Purity::layout(source).is_some(), "{source}");
        }
    }

    #[test]
    fn denied_members_cover_the_script_realm() {
        let runtime = crate::nodes::function::v1::runtime::create_runtime().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        let names: String = context.with(|ctx| {
            rquickjs::Module::evaluate(ctx.clone(), "main", "import 'internals';").unwrap().finish::<()>().unwrap();
            ctx.eval(
                r#"
                const iterators = [[].values(), ''[Symbol.iterator](), new Map().values(), new Set().values(), 'a'.matchAll(/a/g)];
                const protos = [Object.prototype, Function.prototype, Array.prototype, String.prototype, Number.prototype,
                    Boolean.prototype, Symbol.prototype, BigInt.prototype, Date.prototype, RegExp.prototype, Error.prototype,
                    Map.prototype, Set.prototype, WeakMap.prototype, WeakSet.prototype, Promise.prototype,
                    Object.getPrototypeOf(Big(1)), Object.getPrototypeOf(dayjs())];
                for (const iterator of iterators) {
                    for (let proto = Object.getPrototypeOf(iterator); proto && proto !== Object.prototype; proto = Object.getPrototypeOf(proto)) {
                        protos.push(proto);
                    }
                }
                const names = new Set();
                for (const proto of protos) {
                    for (const key of Object.getOwnPropertyNames(proto)) {
                        const value = Object.getOwnPropertyDescriptor(proto, key).value;
                        if (typeof value === 'function' || (typeof value === 'object' && value !== null)) {
                            names.add(key);
                        }
                    }
                }
                for (const instance of [Big(1), dayjs(), new Date(), /a/, [], {}, new Map(), new Set(), new Error('x')]) {
                    for (const key of Object.getOwnPropertyNames(instance)) {
                        if (typeof instance[key] === 'function') {
                            names.add(key);
                        }
                    }
                }
                [...names].join(',')
                "#,
            )
            .unwrap()
        });
        let missing: Vec<&str> = names
            .split(',')
            .filter(|name| !Purity::MEMBERS.contains(name) && !Purity::BANNED.contains(name))
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn layout_keeps_lines_and_columns() {
        let source = "const handler = (input) => {\n  // é ünïcödé\n  return { v: `a\nb` };\n}";
        let layout = Purity::layout(source).unwrap();
        assert_eq!(layout.len(), source.len());
        assert_eq!(layout.matches('\n').count(), source.matches('\n').count());
        assert!(layout.chars().all(|c| c == ' ' || c == '\n'));
    }
}
