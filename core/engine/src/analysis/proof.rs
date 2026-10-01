use std::collections::BTreeMap;
use std::fmt::Write;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::intellisense::{AstMetadata, IntelliSense};
use zen_expression::lexer::{LogicalOperator, Operator};
use zen_expression::parser::Node;

use crate::policy::linter::AstOps;
use crate::workspace::types::Span;

#[derive(Clone)]
pub(crate) struct FixEdit {
    pub(crate) deletions: Vec<Span>,
    pub(crate) kept: Span,
    pub(crate) swap: Option<(Span, bool)>,
}

#[derive(Default)]
struct Layer {
    members: Vec<usize>,
    taken: BTreeMap<u32, u32>,
    targets: HashSet<Span>,
    kept: HashSet<Span>,
}

impl Layer {
    fn admits(&self, edit: &FixEdit) -> bool {
        let free = edit.deletions.iter().all(|(start, end)| {
            self.taken
                .range(..*end)
                .next_back()
                .is_none_or(|(_, taken_end)| taken_end <= start)
        });
        free && edit.swap.is_none_or(|(target, _)| {
            !self.kept.contains(&target) && !self.targets.contains(&edit.kept)
        })
    }

    fn insert(&mut self, idx: usize, edit: &FixEdit) {
        self.members.push(idx);
        self.taken.extend(edit.deletions.iter().copied());
        if let Some((target, _)) = edit.swap {
            self.targets.insert(target);
            self.kept.insert(edit.kept);
        }
    }
}

impl FixEdit {
    pub(crate) fn unwrap(outer: Span, kept: Span, swap: Option<(Span, bool)>) -> Self {
        Self {
            deletions: [(outer.0, kept.0), (kept.1, outer.1)]
                .into_iter()
                .filter(|(start, end)| start < end)
                .collect(),
            kept,
            swap,
        }
    }

    pub(crate) fn apply(&self, source: &str) -> Option<String> {
        FixProof::splice(source, &[self])
    }
}

pub(crate) struct FixProof;

impl FixProof {
    pub(crate) fn proven(
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
        edits: &[FixEdit],
    ) -> Vec<bool> {
        let mut proven = vec![false; edits.len()];
        let mut layers: Vec<Layer> = Vec::new();
        for (idx, edit) in edits.iter().enumerate() {
            match layers.iter_mut().find(|layer| layer.admits(edit)) {
                Some(layer) => layer.insert(idx, edit),
                None => {
                    let mut layer = Layer::default();
                    layer.insert(idx, edit);
                    layers.push(layer);
                }
            }
        }
        for layer in layers {
            Self::bisect(is, source, unary, edits, &layer.members, &mut proven);
        }
        proven
    }

    pub(crate) fn holds(
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
        edits: &[&FixEdit],
    ) -> Option<String> {
        let fixed = Self::splice(source, edits)?;
        let swaps: HashMap<Span, bool> = edits.iter().filter_map(|edit| edit.swap).collect();
        let expected = is
            .with_ast(source, unary, |root, metadata| {
                Shape::of(root, metadata, &swaps)
            })
            .flatten()?;
        let actual = is
            .with_ast(&fixed, unary, |root, metadata| {
                Shape::of(root, metadata, &HashMap::new())
            })
            .flatten()?;
        (expected == actual).then_some(fixed)
    }

    fn bisect(
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
        edits: &[FixEdit],
        members: &[usize],
        proven: &mut [bool],
    ) {
        if members.is_empty() {
            return;
        }
        let batch: Vec<&FixEdit> = members.iter().map(|&idx| &edits[idx]).collect();
        if Self::holds(is, source, unary, &batch).is_some() {
            members.iter().for_each(|&idx| proven[idx] = true);
            return;
        }
        if members.len() == 1 {
            return;
        }
        let (left, right) = members.split_at(members.len() / 2);
        Self::bisect(is, source, unary, edits, left, proven);
        Self::bisect(is, source, unary, edits, right, proven);
    }

    fn splice(source: &str, edits: &[&FixEdit]) -> Option<String> {
        let deletions: Vec<(Span, &str)> = edits
            .iter()
            .flat_map(|edit| edit.deletions.iter().map(|span| (*span, "")))
            .collect();
        AstOps::splice(source, &deletions)
    }
}

struct Shape<'m> {
    metadata: &'m AstMetadata,
    swaps: &'m HashMap<Span, bool>,
    matched: usize,
    out: String,
}

impl<'m> Shape<'m> {
    fn of(
        root: &Node,
        metadata: &'m AstMetadata,
        swaps: &'m HashMap<Span, bool>,
    ) -> Option<String> {
        let mut shape = Shape {
            metadata,
            swaps,
            matched: 0,
            out: String::new(),
        };
        shape.write(root);
        (shape.matched == swaps.len()).then_some(shape.out)
    }

    fn write(&mut self, node: &Node) {
        if let Node::Binary {
            left,
            operator: Operator::Logical(LogicalOperator::NullishCoalescing),
            right,
        } = node
        {
            if let Some(keep_left) = AstOps::span(self.metadata, node)
                .and_then(|span| self.swaps.get(&span))
                .copied()
            {
                self.matched += 1;
                return self.write(if keep_left { left } else { right });
            }
        }
        match node {
            Node::Parenthesized(inner) => return self.write(inner),
            Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::Pointer
            | Node::Identifier(_)
            | Node::Root => {
                let _ = write!(self.out, "{node:?};");
                return;
            }
            _ => {}
        }
        self.out.push_str(node.into());
        let _ = match node {
            Node::Closure { alias, .. } => write!(self.out, "{alias:?}"),
            Node::Interval {
                left_bracket,
                right_bracket,
                ..
            } => write!(self.out, "{left_bracket:?}{right_bracket:?}"),
            Node::Unary { operator, .. } | Node::Binary { operator, .. } => {
                write!(self.out, "{operator:?}")
            }
            Node::FunctionCall { kind, .. } => write!(self.out, "{kind:?}"),
            Node::MethodCall { kind, .. } => write!(self.out, "{kind:?}"),
            Node::Error { error, .. } => write!(self.out, "{error:?}"),
            _ => Ok(()),
        };
        self.out.push('(');
        match node {
            Node::TemplateString(items) | Node::Array(items) => {
                items.iter().for_each(|item| self.write(item))
            }
            Node::Object(entries) => entries.iter().for_each(|(key, value)| {
                self.write(key);
                self.write(value);
            }),
            Node::Assignments { list, output } => {
                list.iter().for_each(|(key, value)| {
                    self.write(key);
                    self.write(value);
                });
                self.optional(*output);
            }
            Node::Closure { body, .. } => self.write(body),
            Node::Member { node, property } => {
                self.write(node);
                self.write(property);
            }
            Node::Slice { node, from, to } => {
                self.write(node);
                self.optional(*from);
                self.optional(*to);
            }
            Node::Interval { left, right, .. } | Node::Binary { left, right, .. } => {
                self.write(left);
                self.write(right);
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.write(condition);
                self.write(on_true);
                self.write(on_false);
            }
            Node::Unary { node, .. } => self.write(node),
            Node::FunctionCall { arguments, .. } => {
                arguments.iter().for_each(|argument| self.write(argument))
            }
            Node::MethodCall {
                this, arguments, ..
            } => {
                self.write(this);
                arguments.iter().for_each(|argument| self.write(argument));
            }
            Node::Error { node, .. } => self.optional(*node),
            _ => {}
        }
        self.out.push(')');
    }

    fn optional(&mut self, node: Option<&Node>) {
        match node {
            Some(node) => self.write(node),
            None => self.out.push('_'),
        }
    }
}
