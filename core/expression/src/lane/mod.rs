mod builtins;
mod cells;
mod columns;
mod compile;
mod date;
mod exec;
mod interval;
mod mask;
mod ops;
mod output;
mod program;
mod scaled;
mod source;

pub use cells::{CellEnv, CellSet, Pieces};
pub use columns::{Column, Columns, Dictionary, Values};
pub use compile::{Hints, LaneCompiler};
pub use exec::{Context, Executor, Fault, Frame};
pub use mask::{LaneSet, Mask, Word, LANES};
pub use output::{Cell, Output, Shape};
pub use source::SourceInfo;
pub use program::{Binary, Binding, ClosureOp, Const, Kind, Layout, Op, Program, Reg, Step};

use crate::lexer::Lexer;
use crate::parser::{Node, Parser};
use crate::scope::Scope;
use crate::variable::Variable;
use crate::{ExpressionKind, IsolateError};
use bumpalo::Bump;

#[derive(Debug, Clone)]
enum Source {
    Single(std::sync::Arc<str>),
    Many(std::sync::Arc<[(String, String)]>, bool),
    Cells(std::sync::Arc<[(String, Option<String>)]>),
}

#[derive(Debug, Clone)]
pub struct LaneProgram {
    program: Program,
    kind: ExpressionKind,
    source: Source,
    constant: Option<Const>,
}

impl LaneProgram {
    const NODES: usize = 20_000;

    fn guard<'n>(roots: impl IntoIterator<Item = &'n Node<'n>>) -> Result<(), IsolateError> {
        let count = std::cell::Cell::new(0usize);
        for root in roots {
            root.walk(|_| count.set(count.get() + 1));
        }
        match count.get() > Self::NODES {
            false => Ok(()),
            true => Err(IsolateError::VMError {
                source: crate::vm::VMError::OpcodeErr {
                    opcode: "Compile".to_string(),
                    message: format!(
                        "expression is too large for the lane VM ({} nodes, limit {})",
                        count.get(),
                        Self::NODES
                    ),
                },
            }),
        }
    }

    pub fn compile(source: &str, kind: ExpressionKind) -> Result<Self, IsolateError> {
        Self::compile_with(source, kind, None)
    }

    pub fn compile_with(
        source: &str,
        kind: ExpressionKind,
        hints: Option<&Hints>,
    ) -> Result<Self, IsolateError> {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let tokens = lexer.tokenize(&bump, source)?;
        let parser = Parser::try_new(&tokens, &bump)?;
        let parsed = match kind {
            ExpressionKind::Standard => parser.standard().parse(),
            ExpressionKind::Unary => parser.unary().parse(),
        };
        parsed.error()?;
        Self::guard([parsed.root])?;
        let program = LaneCompiler::compile_with(parsed.root, hints)?;
        let constant = match kind {
            ExpressionKind::Standard => Const::of(parsed.root),
            ExpressionKind::Unary => None,
        };
        Ok(Self {
            program,
            kind,
            source: Source::Single(source.into()),
            constant,
        })
    }

    fn recompile(&self, hints: &Hints) -> Result<Self, IsolateError> {
        match &self.source {
            Source::Single(source) => Self::compile_with(source, self.kind.clone(), Some(hints)),
            Source::Many(entries, chain) => {
                let refs: Vec<(&str, &str)> = entries
                    .iter()
                    .map(|(k, s)| (k.as_str(), s.as_str()))
                    .collect();
                Self::compile_many_with(&refs, *chain, Some(hints))
            }
            Source::Cells(cells) => {
                let refs: Vec<(&str, Option<&str>)> = cells.iter().map(|(s, f)| (s.as_str(), f.as_deref())).collect();
                Self::compile_cells_with(&refs, Some(hints))
            }
        }
    }

    pub fn source(&self) -> Option<&str> {
        match &self.source {
            Source::Single(source) => Some(source),
            _ => None,
        }
    }

    pub fn compile_cells(cells: &[(&str, Option<&str>)]) -> Result<Self, IsolateError> {
        Self::compile_cells_with(cells, None)
    }

    fn compile_cells_with(cells: &[(&str, Option<&str>)], hints: Option<&Hints>) -> Result<Self, IsolateError> {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let mut roots = Vec::with_capacity(cells.len());
        for (index, (source, field)) in cells.iter().enumerate() {
            let tokens = lexer.tokenize(&bump, source)?;
            let parser = Parser::try_new(&tokens, &bump)?;
            let root = match field {
                Some(field) => {
                    let parsed = parser.unary().parse();
                    parsed.error()?;
                    let tokens = lexer.tokenize(&bump, field)?;
                    let target = Parser::try_new(&tokens, &bump)?.standard().parse();
                    target.error()?;
                    Self::rooted(&bump, parsed.root, target.root).ok_or_else(|| Self::unsupported("cell"))?
                }
                None => {
                    let parsed = parser.standard().parse();
                    parsed.error()?;
                    parsed.root
                }
            };
            let key: &str = bump.alloc_str(&format!("c{index}"));
            roots.push((key, root));
        }
        Self::guard(roots.iter().map(|(_, root)| *root))?;
        let program = LaneCompiler::compile_isolated(&roots, hints)?;
        let owned: Vec<(String, Option<String>)> = cells.iter().map(|(s, f)| (s.to_string(), f.map(str::to_string))).collect();
        Ok(Self {
            program,
            kind: ExpressionKind::Standard,
            source: Source::Cells(owned.into()),
            constant: None,
        })
    }

    fn unsupported(what: &str) -> IsolateError {
        IsolateError::VMError {
            source: crate::vm::VMError::OpcodeErr {
                opcode: "Compile".to_string(),
                message: format!("{what} is not supported in a combined cell program"),
            },
        }
    }

    fn rooted<'b>(bump: &'b Bump, node: &'b Node<'b>, root: &'b Node<'b>) -> Option<&'b Node<'b>> {
        let map = |n: &'b Node<'b>| Self::rooted(bump, n, root);
        let list = |items: &'b [&'b Node<'b>]| -> Option<&'b [&'b Node<'b>]> {
            let mapped: Vec<&'b Node<'b>> = items.iter().map(|n| map(n)).collect::<Option<_>>()?;
            Some(bump.alloc_slice_copy(&mapped))
        };
        let rebuilt = match node {
            Node::Identifier("$") => return Some(root),
            Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) | Node::Pointer | Node::Identifier(_) | Node::Root => {
                return Some(node)
            }
            Node::TemplateString(parts) => Node::TemplateString(list(parts)?),
            Node::Array(items) => Node::Array(list(items)?),
            Node::Object(pairs) => {
                let mapped: Vec<(&'b Node<'b>, &'b Node<'b>)> =
                    pairs.iter().map(|(k, v)| Some((map(k)?, map(v)?))).collect::<Option<_>>()?;
                Node::Object(bump.alloc_slice_copy(&mapped))
            }
            Node::Closure { body, alias } => Node::Closure {
                body: map(body)?,
                alias: *alias,
            },
            Node::Parenthesized(inner) => Node::Parenthesized(map(inner)?),
            Node::Member { node, property } => Node::Member {
                node: map(node)?,
                property: map(property)?,
            },
            Node::Slice { node, from, to } => Node::Slice {
                node: map(node)?,
                from: match from {
                    Some(n) => Some(map(n)?),
                    None => None,
                },
                to: match to {
                    Some(n) => Some(map(n)?),
                    None => None,
                },
            },
            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } => Node::Interval {
                left: map(left)?,
                right: map(right)?,
                left_bracket: *left_bracket,
                right_bracket: *right_bracket,
            },
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => Node::Conditional {
                condition: map(condition)?,
                on_true: map(on_true)?,
                on_false: map(on_false)?,
            },
            Node::Unary { node, operator } => Node::Unary {
                node: map(node)?,
                operator: *operator,
            },
            Node::Binary { left, operator, right } => Node::Binary {
                left: map(left)?,
                operator: *operator,
                right: map(right)?,
            },
            Node::FunctionCall { kind, arguments } => Node::FunctionCall {
                kind: kind.clone(),
                arguments: list(arguments)?,
            },
            Node::MethodCall { kind, this, arguments } => Node::MethodCall {
                kind: kind.clone(),
                this: map(this)?,
                arguments: list(arguments)?,
            },
            Node::Assignments { .. } | Node::Error { .. } => return None,
        };
        Some(bump.alloc(rebuilt))
    }

    pub fn compile_typed(
        source: &str,
        kind: ExpressionKind,
        input: &crate::variable::VariableType,
    ) -> Result<Self, IsolateError> {
        use crate::intellisense::scope::IntelliSenseScope;
        use crate::intellisense::type_provider::TypesProvider;
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let tokens = lexer.tokenize(&bump, source)?;
        let parser = Parser::try_new(&tokens, &bump)?;
        let parsed = match kind {
            ExpressionKind::Standard => parser.standard().parse(),
            ExpressionKind::Unary => parser.unary().parse(),
        };
        parsed.error()?;
        Self::guard([parsed.root])?;
        let scope = IntelliSenseScope {
            pointer_data: input.shallow_clone(),
            root_data: input.shallow_clone(),
            current_data: input.shallow_clone(),
            ..Default::default()
        };
        let types = TypesProvider::generate(parsed.root, scope, false);
        let program = LaneCompiler::compile_typed(parsed.root, None, Some(&types))?;
        Ok(Self {
            program,
            kind,
            source: Source::Single(source.into()),
            constant: None,
        })
    }

    pub fn compile_many(entries: &[(&str, &str)], chain: bool) -> Result<Self, IsolateError> {
        Self::compile_many_with(entries, chain, None)
    }

    fn compile_many_with(
        entries: &[(&str, &str)],
        chain: bool,
        hints: Option<&Hints>,
    ) -> Result<Self, IsolateError> {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let mut roots = Vec::with_capacity(entries.len());
        for (key, source) in entries {
            let tokens = lexer.tokenize(&bump, source)?;
            let parsed = Parser::try_new(&tokens, &bump)?.standard().parse();
            parsed.error()?;
            roots.push((*key, parsed.root));
        }
        Self::guard(roots.iter().map(|(_, root)| *root))?;
        let program = LaneCompiler::compile_many(&roots, chain, hints)?;
        let owned: Vec<(String, String)> = entries
            .iter()
            .map(|(k, s)| (k.to_string(), s.to_string()))
            .collect();
        Ok(Self {
            program,
            kind: ExpressionKind::Standard,
            source: Source::Many(owned.into(), chain),
            constant: None,
        })
    }

    pub fn specialize(&self, samples: &[Scope]) -> Result<Self, IsolateError> {
        let mut hints = Hints::default();
        for key in &self.program.keys {
            let segments: Vec<&str> = key.split('.').collect();
            let mut counts = [0usize; 5];
            for scope in samples {
                Self::observe(scope.base(), &segments, &mut counts);
            }
            if let Some(kind) = Self::vote(counts) {
                hints.insert(key.clone(), kind);
            }
        }
        self.recompile(&hints)
    }

    fn vote(counts: [usize; 5]) -> Option<Kind> {
        let [num, bool, str, other, _] = counts;
        let total = num + bool + str + other;
        match total {
            0 => None,
            _ if num * 2 > total => Some(Kind::Num),
            _ if bool * 2 > total => Some(Kind::Bool),
            _ if str * 2 > total => Some(Kind::Str),
            _ => None,
        }
    }

    fn column_hint(columns: &Columns, key: &str) -> Option<Kind> {
        let segments: Vec<&str> = key.split('.').collect();
        for split in (1..=segments.len()).rev() {
            let last = segments[split - 1];
            let (name, each) = match last.strip_suffix("[]") {
                Some(name) => (name, true),
                None => (last, false),
            };
            let mut path = segments[..split - 1].join(".");
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
            let Some(i) = columns.find(&path) else {
                continue;
            };
            let column = &columns.columns[i].1;
            let rest = &segments[split..];
            if let (true, true, Values::List { child, .. }) = (each, rest.is_empty(), column.values)
            {
                return match child.kind() {
                    Kind::Dyn => None,
                    kind => Some(kind),
                };
            }
            let mut counts = [0usize; 5];
            for row in 0..columns.rows.min(256) {
                let value = column.variable(row);
                match (each, &value) {
                    (true, Variable::Array(items)) => {
                        for item in items.borrow().iter().take(64) {
                            Self::observe(item, rest, &mut counts);
                        }
                    }
                    (true, _) => {}
                    (false, v) => Self::observe(v, rest, &mut counts),
                }
            }
            return Self::vote(counts);
        }
        None
    }

    pub fn specialize_bound(&self, columns: &Columns, bind: &dyn Fn(&str) -> Binding) -> Result<Self, IsolateError> {
        let mut hints = Hints::default();
        for key in &self.program.keys {
            let kind = match bind(key) {
                Binding::Column(i) => columns.columns.get(i).and_then(|(name, column)| match column.kind() {
                    Kind::Dyn => Self::column_hint(columns, name),
                    kind => Some(kind),
                }),
                _ => match columns.find(key).map(|i| columns.columns[i].1.kind()) {
                    Some(Kind::Dyn) | None => Self::column_hint(columns, key),
                    other => other,
                },
            };
            if let Some(kind) = kind {
                hints.insert(key.clone(), kind);
            }
        }
        self.recompile(&hints)
    }

    pub fn specialize_columns(&self, columns: &Columns) -> Result<Self, IsolateError> {
        let mut hints = Hints::default();
        for key in &self.program.keys {
            let exact = columns.find(key).map(|i| columns.columns[i].1.kind());
            let kind = match exact {
                Some(Kind::Dyn) | None => Self::column_hint(columns, key),
                other => other,
            };
            if let Some(kind) = kind {
                hints.insert(key.clone(), kind);
            }
        }
        self.recompile(&hints)
    }

    fn observe(value: &Variable, segments: &[&str], counts: &mut [usize; 5]) {
        let Some((segment, rest)) = segments.split_first() else {
            match value {
                Variable::Number(_) => counts[0] += 1,
                Variable::Bool(_) => counts[1] += 1,
                Variable::String(_) => counts[2] += 1,
                Variable::Null => counts[4] += 1,
                _ => counts[3] += 1,
            }
            return;
        };
        let (name, each) = match segment.strip_suffix("[]") {
            Some(name) => (name, true),
            None => (*segment, false),
        };
        let next = match value {
            Variable::Object(o) => o.borrow().get_str(name).cloned().unwrap_or(Variable::Null),
            _ => Variable::Null,
        };
        match (each, &next) {
            (true, Variable::Array(items)) => {
                for item in items.borrow().iter().take(64) {
                    Self::observe(item, rest, counts);
                }
            }
            (true, _) => {}
            (false, v) => Self::observe(v, rest, counts),
        }
    }

    pub fn object_fields(source: &str) -> Option<Vec<(String, String)>> {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let tokens = lexer.tokenize(&bump, source).ok()?;
        let parsed = Parser::try_new(&tokens, &bump).ok()?.with_metadata().standard().parse();
        parsed.error().ok()?;
        let metadata = parsed.metadata.as_ref()?;
        let Node::Object(pairs) = parsed.root else {
            return None;
        };
        let mut fields: Vec<(String, String)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs.iter() {
            let Node::String(key) = key else {
                return None;
            };
            if key.is_empty() || key.contains('.') || fields.iter().any(|(k, _)| k == key) {
                return None;
            }
            let (a, b) = metadata.get(&(*value as *const Node as usize))?.span;
            let fragment = source.get(a as usize..b as usize)?;
            let inner = Bump::new();
            let mut relexer = Lexer::new();
            let retokens = relexer.tokenize(&inner, fragment).ok()?;
            let reparsed = Parser::try_new(&retokens, &inner).ok()?.standard().parse();
            if reparsed.error().is_err() || reparsed.root != *value {
                return None;
            }
            fields.push((key.to_string(), fragment.to_string()));
        }
        Some(fields)
    }

    pub fn standard(source: &str) -> Result<Self, IsolateError> {
        Self::compile(source, ExpressionKind::Standard)
    }

    pub fn unary(source: &str) -> Result<Self, IsolateError> {
        Self::compile(source, ExpressionKind::Unary)
    }

    pub fn kind(&self) -> &ExpressionKind {
        &self.kind
    }

    pub fn path(&self) -> Option<&str> {
        let p = &self.program;
        match (p.steps.as_slice(), &p.layout, p.outputs.is_empty()) {
            ([step], None, true) => match &step.op {
                Op::Env { dst, site, .. } | Op::Path { dst, site, .. } if *dst == p.out => {
                    p.site_keys.get(*site as usize)?.as_deref()
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub fn literal(&self) -> Option<Variable> {
        if let Some(constant) = &self.constant {
            return Some(constant.variable());
        }
        let p = &self.program;
        match (p.steps.as_slice(), &p.layout, p.outputs.is_empty()) {
            ([step], None, true) => match step.op {
                Op::Const { dst, id } if dst == p.out => p.consts.get(id as usize).map(Const::variable),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn program(&self) -> &Program {
        &self.program
    }
}

#[derive(Debug, Default)]
pub struct LaneRunner {
    frames: Vec<Frame<Word>>,
    wide: Vec<Frame<Mask>>,
    rows: Vec<u32>,
    scopes: Vec<Scope>,
}

impl LaneRunner {
    pub fn new() -> Self {
        Self::default()
    }

    fn frames(&mut self, program: &Program) {
        if self.frames.len() < program.frames {
            self.frames.resize_with(program.frames, Frame::default);
        }
    }

    fn wide(&mut self, program: &Program) {
        if self.wide.len() < program.frames {
            self.wide.resize_with(program.frames, Frame::default);
        }
    }

    thread_local! {
        static FOLD: std::cell::RefCell<Vec<Frame<Word>>> = Default::default();
    }

    pub(crate) fn constant(program: &Program) -> Option<Variable> {
        Self::FOLD.with_borrow_mut(|frames| {
            if frames.len() < program.frames {
                frames.resize_with(program.frames, Frame::default);
            }
            let roots = [Scope::default()];
            let mut ctx = Context::new(program, &roots);
            Executor::run(program, frames, &mut ctx, &[0]);
            frames.first_mut()?.take_result(program, 0).ok()
        })
    }

    pub fn evaluate_one(
        &mut self,
        program: &LaneProgram,
        scope: &Scope,
    ) -> Result<Variable, IsolateError> {
        let p = &program.program;
        self.frames(p);
        let roots = std::slice::from_ref(scope);
        let mut ctx = Context::new(p, roots);
        Executor::run(p, &mut self.frames, &mut ctx, &[0]);
        self.frames[0]
            .take_result(p, 0)
            .map_err(|source| IsolateError::VMError { source })
    }

    pub fn evaluate(
        &mut self,
        program: &LaneProgram,
        scopes: &[Scope],
    ) -> Vec<Result<Variable, IsolateError>> {
        let mut out = Vec::with_capacity(scopes.len());
        self.evaluate_with(program, scopes, |_, r| out.push(r));
        out
    }

    pub fn evaluate_with(
        &mut self,
        program: &LaneProgram,
        scopes: &[Scope],
        mut sink: impl FnMut(usize, Result<Variable, IsolateError>),
    ) {
        let p = &program.program;
        self.frames(p);
        for (chunk, roots) in scopes.chunks(Word::LANES).enumerate() {
            self.rows.clear();
            self.rows.extend(0..roots.len() as u32);
            let mut ctx = Context::new(p, roots);
            Executor::run(p, &mut self.frames, &mut ctx, &self.rows);
            let frame = &mut self.frames[0];
            for lane in 0..roots.len() {
                let result = frame
                    .take_result(p, lane)
                    .map_err(|source| IsolateError::VMError { source });
                sink(chunk * Word::LANES + lane, result);
            }
        }
    }

    pub fn evaluate_many(
        &mut self,
        program: &LaneProgram,
        scopes: &[Scope],
        mask: Option<&[bool]>,
        mut sink: impl FnMut(usize, Option<Result<Vec<Variable>, (usize, IsolateError)>>),
    ) {
        let p = &program.program;
        self.frames(p);
        for (chunk, roots) in scopes.chunks(Word::LANES).enumerate() {
            let start = chunk * Word::LANES;
            self.rows.clear();
            self.rows.extend(0..roots.len() as u32);
            let mut ctx = Context::new(p, roots);
            let Some((frame, _)) = self.frames.split_first_mut() else {
                return;
            };
            Executor::enter_top(p, frame, &self.rows);
            if let Some(mask) = mask {
                let mut bits = Word::none(roots.len());
                for l in (0..roots.len()).filter(|l| mask.get(start + l).copied().unwrap_or(false))
                {
                    bits.set(l);
                }
                frame.restrict(bits);
            }
            Executor::execute_top(p, &mut self.frames, &mut ctx);
            let frame = &mut self.frames[0];
            for lane in 0..roots.len() {
                if !frame.active(lane) {
                    sink(start + lane, None);
                    continue;
                }
                let result = match frame.take_result(p, lane) {
                    Ok(_) => Ok(frame.outputs(p, lane)),
                    Err(source) => Err((
                        frame.stage_of(lane) as usize,
                        IsolateError::VMError { source },
                    )),
                };
                sink(start + lane, Some(result));
            }
        }
    }

    pub fn evaluate_bound(
        &mut self,
        program: &LaneProgram,
        scopes: &[Scope],
        columns: &Columns,
        bind: &dyn Fn(&str) -> Binding,
        subset: Option<&[usize]>,
        mut sink: impl FnMut(usize, Result<Vec<Variable>, (usize, IsolateError)>),
    ) {
        let p = &program.program;
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|k| match (p.writes_env, k.as_deref()) {
                (true, _) | (_, None) => Binding::Row,
                (false, Some(k)) => bind(k),
            })
            .collect();
        let total = subset.map_or(scopes.len(), <[usize]>::len);
        match total <= Word::LANES {
            true => {
                self.frames(p);
                self.frames.iter_mut().for_each(Frame::forget);
                let (frames, rows) = (&mut self.frames, &mut self.rows);
                Self::bound_blocks(p, frames, rows, scopes, columns, &bound, subset, Word::LANES, &mut sink);
            }
            false => {
                self.wide(p);
                self.wide.iter_mut().for_each(Frame::forget);
                let (frames, rows) = (&mut self.wide, &mut self.rows);
                Self::bound_blocks(p, frames, rows, scopes, columns, &bound, subset, LANES, &mut sink);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_bound_into(
        &mut self,
        program: &LaneProgram,
        scopes: &[Scope],
        columns: &Columns,
        bind: &dyn Fn(&str) -> Binding,
        subset: Option<&[usize]>,
        outs: &mut Vec<Output>,
        failure: impl FnMut(usize, usize, Fault),
    ) {
        let p = &program.program;
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|k| match (p.writes_env, k.as_deref()) {
                (true, _) | (_, None) => Binding::Row,
                (false, Some(k)) => bind(k),
            })
            .collect();
        self.evaluate_sites(program, scopes, columns, &bound, subset, outs, failure)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_sites(
        &mut self,
        program: &LaneProgram,
        scopes: &[Scope],
        columns: &Columns,
        bound: &[Binding],
        subset: Option<&[usize]>,
        outs: &mut Vec<Output>,
        mut failure: impl FnMut(usize, usize, Fault),
    ) {
        let p = &program.program;
        let total = subset.map_or(scopes.len(), <[usize]>::len);
        let regs: Vec<Reg> = match p.outputs.is_empty() {
            true => vec![p.out],
            false => p.outputs.clone(),
        };
        outs.resize_with(regs.len(), Output::new);
        for (reg, out) in regs.iter().zip(outs.iter_mut()) {
            match (&p.layout, p.outputs.is_empty()) {
                (Some(layout), true) => out.reset_layout(layout, &p.kinds, total),
                _ => out.reset(p.kinds[*reg as usize], total),
            }
        }
        let mut sink = |at: usize, stage: usize, fault| failure(at, stage, fault);
        match total <= Word::LANES {
            true => {
                self.frames(p);
                self.frames.iter_mut().for_each(Frame::forget);
                let (frames, rows) = (&mut self.frames, &mut self.rows);
                Self::export_blocks(p, frames, rows, scopes, columns, bound, subset, Word::LANES, outs, &mut sink);
            }
            false => {
                self.wide(p);
                self.wide.iter_mut().for_each(Frame::forget);
                let (frames, rows) = (&mut self.wide, &mut self.rows);
                Self::export_blocks(p, frames, rows, scopes, columns, bound, subset, LANES, outs, &mut sink);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn export_blocks<M: LaneSet>(
        p: &Program,
        frames: &mut [Frame<M>],
        rows: &mut Vec<u32>,
        scopes: &[Scope],
        columns: &Columns,
        bound: &[Binding],
        subset: Option<&[usize]>,
        width: usize,
        outs: &mut [Output],
        sink: &mut dyn FnMut(usize, usize, Fault),
    ) {
        let total = subset.map_or(scopes.len(), <[usize]>::len);
        let mut start = 0;
        while start < total {
            let n = (total - start).min(width);
            rows.clear();
            let (roots, base) = match subset {
                None => {
                    rows.extend(0..n as u32);
                    (&scopes[start..start + n], start)
                }
                Some(list) => {
                    rows.extend(list[start..start + n].iter().map(|&r| r as u32));
                    (scopes, 0)
                }
            };
            let mut ctx = Context::columnar(p, roots, columns, bound, base);
            Executor::run(p, frames, &mut ctx, rows);
            let Some(frame) = frames.first_mut() else {
                return;
            };
            frame.export_outputs(p, outs, start, n, &mut *sink);
            start += n;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bound_blocks<M: LaneSet>(
        p: &Program,
        frames: &mut [Frame<M>],
        rows: &mut Vec<u32>,
        scopes: &[Scope],
        columns: &Columns,
        bound: &[Binding],
        subset: Option<&[usize]>,
        width: usize,
        sink: &mut impl FnMut(usize, Result<Vec<Variable>, (usize, IsolateError)>),
    ) {
        let many = !p.outputs.is_empty();
        let total = subset.map_or(scopes.len(), <[usize]>::len);
        let mut start = 0;
        while start < total {
            let n = (total - start).min(width);
            rows.clear();
            let (roots, base) = match subset {
                None => {
                    rows.extend(0..n as u32);
                    (&scopes[start..start + n], start)
                }
                Some(list) => {
                    rows.extend(list[start..start + n].iter().map(|&r| r as u32));
                    (scopes, 0)
                }
            };
            let mut ctx = Context::columnar(p, roots, columns, bound, base);
            Executor::run(p, frames, &mut ctx, rows);
            let Some(frame) = frames.first_mut() else {
                return;
            };
            for lane in 0..n {
                let row = match subset {
                    None => start + lane,
                    Some(list) => list[start + lane],
                };
                let result = match frame.take_result(p, lane) {
                    Ok(value) => Ok(match many {
                        true => frame.outputs(p, lane),
                        false => vec![value],
                    }),
                    Err(source) => Err((
                        frame.stage_of(lane) as usize,
                        IsolateError::VMError { source },
                    )),
                };
                sink(row, result);
            }
            start += n;
        }
    }

    pub fn evaluate_columns(
        &mut self,
        program: &LaneProgram,
        columns: &Columns,
        mut sink: impl FnMut(usize, Result<Variable, IsolateError>),
    ) {
        let p = &program.program;
        self.columnar(p, columns, |frame, start, n| {
            for lane in 0..n {
                let result = frame
                    .take_result(p, lane)
                    .map_err(|source| IsolateError::VMError { source });
                sink(start + lane, result);
            }
        });
    }

    pub fn evaluate_columns_into(
        &mut self,
        program: &LaneProgram,
        columns: &Columns,
        out: &mut Output,
    ) {
        let p = &program.program;
        match &p.layout {
            Some(layout) => out.reset_layout(layout, &p.kinds, columns.rows),
            None => out.reset(p.kinds[p.out as usize], columns.rows),
        }
        self.columnar(p, columns, |frame, start, n| frame.export(p, out, start, n));
    }

    fn columnar(
        &mut self,
        p: &Program,
        columns: &Columns,
        mut chunk: impl FnMut(&mut Frame<Mask>, usize, usize),
    ) {
        self.wide(p);
        let bound: Vec<Binding> = p
            .site_keys
            .iter()
            .map(|k| match (p.writes_env, k.as_deref()) {
                (true, _) | (_, None) => Binding::Row,
                (false, Some(k)) => columns.bind(k),
            })
            .collect();
        let rows = p.needs_rows(&bound);
        self.wide.iter_mut().for_each(Frame::forget);
        let mut start = 0;
        while start < columns.rows {
            let n = (columns.rows - start).min(LANES);
            self.rows.clear();
            self.rows.extend(0..n as u32);
            self.scopes.clear();
            if rows {
                self.scopes
                    .extend((start..start + n).map(|r| Scope::new(columns.row(r))));
            }
            let mut ctx = Context::columnar(p, &self.scopes, columns, &bound, start);
            Executor::run(p, &mut self.wide, &mut ctx, &self.rows);
            chunk(&mut self.wide[0], start, n);
            start += n;
        }
    }
}

pub struct Backend;

impl Backend {
    thread_local! {
        static STATE: std::cell::RefCell<BackendState> = Default::default();
    }

    pub fn run(
        source: &str,
        kind: ExpressionKind,
        scope: &Scope,
    ) -> Result<Variable, IsolateError> {
        Self::STATE.with_borrow_mut(|state| {
            let BackendState {
                runner,
                standard,
                unary,
            } = state;
            let programs = match kind {
                ExpressionKind::Standard => standard,
                ExpressionKind::Unary => unary,
            };
            if let Some(p) = programs.get(source) {
                return runner.evaluate_one(p, scope);
            }
            let p = LaneProgram::compile(source, kind)?;
            let result = runner.evaluate_one(&p, scope);
            programs.insert(source.to_string(), p);
            result
        })
    }
}

#[derive(Default)]
struct BackendState {
    runner: LaneRunner,
    standard: ahash::HashMap<String, LaneProgram>,
    unary: ahash::HashMap<String, LaneProgram>,
}
