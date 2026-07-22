use crate::functions::{ClosureFunction, FunctionKind};
use crate::lexer::{LogicalOperator, Operator};
use crate::parser::{Node, NodeMetadata};
use crate::variable::Variable;
use ahash::HashSet;
use nohash_hasher::BuildNoHashHasher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadDependency {
    Direct {
        path: Vec<Rc<str>>,
        #[serde(default)]
        span: (u32, u32),
        #[serde(default, skip_serializing_if = "is_false")]
        via_index: bool,
    },
    Iteration {
        collection: Vec<Rc<str>>,
        #[serde(default)]
        span: (u32, u32),
        #[serde(skip_serializing_if = "Option::is_none")]
        alias: Option<Rc<str>>,
        reads: Vec<ReadDependency>,
    },
    Unresolved {
        path: Vec<Rc<str>>,
        #[serde(default)]
        span: (u32, u32),
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ReadDependency {
    pub fn without_spans(&self) -> Self {
        match self {
            ReadDependency::Direct {
                path, via_index, ..
            } => ReadDependency::Direct {
                path: path.clone(),
                span: (0, 0),
                via_index: *via_index,
            },
            ReadDependency::Iteration {
                collection,
                alias,
                reads,
                ..
            } => ReadDependency::Iteration {
                collection: collection.clone(),
                span: (0, 0),
                alias: alias.clone(),
                reads: reads.iter().map(|r| r.without_spans()).collect(),
            },
            ReadDependency::Unresolved { path, .. } => ReadDependency::Unresolved {
                path: path.clone(),
                span: (0, 0),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reference {
    pub path: Vec<Rc<str>>,
    pub spans: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_alias: Option<AliasBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_index: Option<Vec<Rc<str>>>,
}

impl Reference {
    pub fn without_via_alias(&self) -> Self {
        Self {
            path: self.path.clone(),
            spans: self.spans.clone(),
            via_alias: None,
            via_index: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AliasBinding {
    pub alias: Rc<str>,
    pub collection: Vec<Rc<str>>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyResult {
    pub reads: Vec<ReadDependency>,
    pub references: Vec<Reference>,
}

type MetadataMap = HashMap<usize, NodeMetadata, BuildNoHashHasher<usize>>;

enum ChainSegment<'a> {
    Field { name: Rc<str>, prop: &'a Node<'a> },
    Dynamic { prop: &'a Node<'a> },
}

struct FlatChain<'a> {
    root: &'a Node<'a>,
    segments: Vec<ChainSegment<'a>>,
}

pub(crate) struct DependencyResolutionWalker<'a> {
    reads: Vec<ReadDependency>,
    references: Vec<Reference>,
    metadata: &'a MetadataMap,
}

#[derive(Debug, Clone, Default)]
struct Scope {
    locals: HashSet<Rc<str>>,
    bindings: Vec<Vec<Rc<str>>>,
    aliases: HashMap<Rc<str>, Vec<Rc<str>>>,
    unresolved_aliases: HashSet<Rc<str>>,
    pointer_collection: Option<Vec<Rc<str>>>,
}

impl Scope {
    fn is_local(&self, path: &[Rc<str>]) -> bool {
        if path.is_empty() {
            return false;
        }

        if self.locals.contains(&path[0]) {
            return true;
        }

        self.bindings
            .iter()
            .any(|b| path.len() >= b.len() && path.iter().zip(b.iter()).all(|(a, b)| a == b))
    }

    fn alias_binding_for(&self, path: &[Rc<str>]) -> Option<AliasBinding> {
        let root = path.first()?;
        let collection = self.aliases.get(root)?;

        Some(AliasBinding {
            alias: root.clone(),
            collection: collection.clone(),
        })
    }

    fn expand_alias_root(&self, path: &[Rc<str>]) -> Vec<Rc<str>> {
        match path.first().and_then(|root| self.aliases.get(root)) {
            Some(prefix) => prefix.iter().chain(path.iter().skip(1)).cloned().collect(),
            None => path.to_vec(),
        }
    }
}

fn node_address(node: &Node) -> usize {
    node as *const Node as usize
}

impl<'a> DependencyResolutionWalker<'a> {
    pub fn walk(root: &Node, metadata: &'a MetadataMap) -> DependencyResult {
        Self::walk_with_locals(root, metadata, &[])
    }

    pub fn walk_with_locals(
        root: &Node,
        metadata: &'a MetadataMap,
        locals: &[&str],
    ) -> DependencyResult {
        let mut walker = Self {
            reads: Vec::new(),
            references: Vec::new(),
            metadata,
        };

        let mut scope = Scope::default();
        scope.locals.extend(locals.iter().map(|l| Rc::from(*l)));
        walker.resolve(root, &mut scope);

        DependencyResult {
            reads: walker.reads,
            references: walker.references,
        }
    }

    pub fn field_dependencies(
        root: &Node,
        metadata: &'a MetadataMap,
        field_path: &[&str],
    ) -> Option<Vec<ReadDependency>> {
        let mut bindings: Vec<(Rc<str>, Vec<Rc<str>>)> = Vec::new();
        let mut values: Vec<&Node> = Vec::new();
        Self::navigate_field(root, field_path, &mut bindings, &mut values);
        if values.is_empty() {
            return None;
        }

        let mut reads: Vec<ReadDependency> = Vec::new();
        for value in values {
            let (r, _refs) = Self::walk_inner(value, &mut Scope::default(), metadata);
            reads.extend(r);
        }

        let mut wrapped = reads;
        for (alias, collection) in bindings.into_iter().rev() {
            wrapped = vec![ReadDependency::Iteration {
                collection,
                span: (0, 0),
                alias: Some(alias),
                reads: wrapped,
            }];
        }
        Some(wrapped)
    }

    fn navigate_field<'n>(
        node: &'n Node<'n>,
        field_path: &[&str],
        bindings: &mut Vec<(Rc<str>, Vec<Rc<str>>)>,
        out: &mut Vec<&'n Node<'n>>,
    ) {
        match node {
            Node::Parenthesized(inner) => Self::navigate_field(inner, field_path, bindings, out),
            Node::Assignments {
                output: Some(output),
                ..
            } => Self::navigate_field(output, field_path, bindings, out),
            Node::Conditional {
                on_true, on_false, ..
            } => {
                Self::navigate_field(on_true, field_path, bindings, out);
                Self::navigate_field(on_false, field_path, bindings, out);
            }
            Node::FunctionCall {
                kind: FunctionKind::Closure(ClosureFunction::Map | ClosureFunction::FlatMap),
                arguments,
            } if arguments.len() >= 2 => {
                if let Node::Closure { body, alias } = arguments[1] {
                    if let (Some(a), Some(src)) =
                        (alias, Self::collection_source_path(arguments[0]))
                    {
                        bindings.push((Rc::from(*a), src));
                    }
                    Self::navigate_field(body, field_path, bindings, out);
                }
            }
            Node::Object(pairs) => {
                let Some((first, rest)) = field_path.split_first() else {
                    return;
                };
                let value = pairs.iter().find_map(|(k, v)| match k {
                    Node::String(s) if s == first => Some(*v),
                    _ => None,
                });
                if let Some(value) = value {
                    if rest.is_empty() {
                        out.push(value);
                    } else {
                        Self::navigate_field(value, rest, bindings, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn walk_inner(
        node: &Node,
        scope: &mut Scope,
        metadata: &'a MetadataMap,
    ) -> (Vec<ReadDependency>, Vec<Reference>) {
        let mut walker = Self {
            reads: Vec::new(),
            references: Vec::new(),
            metadata,
        };
        walker.resolve(node, scope);
        (walker.reads, walker.references)
    }

    fn node_span(&self, node: &Node) -> (u32, u32) {
        node.span()
            .or_else(|| self.metadata.get(&node_address(node)).map(|m| m.span))
            .unwrap_or_default()
    }

    fn extract_path_with_spans(&self, node: &Node) -> Option<(Vec<Rc<str>>, Vec<(u32, u32)>)> {
        match node {
            Node::Identifier(name) => Some((vec![Rc::from(*name)], vec![self.node_span(node)])),
            Node::Root => Some((vec![Variable::root_key()], vec![self.node_span(node)])),
            Node::Member { node: n, property } => {
                let (mut path, mut spans) = self.extract_path_with_spans(n)?;
                match property {
                    Node::String(key) => {
                        path.push(Rc::from(*key));
                        spans.push(self.node_span(property));
                        Some((path, spans))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn collection_source_path(node: &Node) -> Option<Vec<Rc<str>>> {
        match node {
            Node::Identifier(name) => Some(vec![Rc::from(*name)]),
            Node::Root => Some(vec![Variable::root_key()]),
            Node::Member { .. } => Self::extract_read_path(node),
            Node::Parenthesized(inner) => Self::collection_source_path(inner),
            Node::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                ..
            } => Self::collection_source_path(left),
            Node::FunctionCall {
                kind: FunctionKind::Closure(ClosureFunction::Filter),
                arguments,
            } if !arguments.is_empty() => Self::collection_source_path(arguments[0]),
            _ => None,
        }
    }

    fn flatten_member_chain<'n>(node: &'n Node<'n>) -> Option<FlatChain<'n>> {
        let mut segments: Vec<ChainSegment<'n>> = Vec::new();
        let mut current = node;
        loop {
            match current {
                Node::Member { node: n, property } => {
                    segments.push(match property {
                        Node::String(key) => ChainSegment::Field {
                            name: Rc::from(*key),
                            prop: property,
                        },
                        _ => ChainSegment::Dynamic { prop: property },
                    });
                    current = n;
                }
                Node::Identifier(_) | Node::Root | Node::Pointer => {
                    segments.reverse();
                    return Some(FlatChain {
                        root: current,
                        segments,
                    });
                }
                _ => return None,
            }
        }
    }

    fn resolve_member_chain(&mut self, node: &Node, scope: &mut Scope) {
        let Some(chain) = Self::flatten_member_chain(node) else {
            if let Node::Member { node: n, property } = node {
                self.resolve(n, scope);
                self.resolve(property, scope);
            }

            return;
        };

        let root_name: Rc<str> = match chain.root {
            Node::Identifier(name) if *name == "$" => {
                if scope.pointer_collection.is_some() {
                    self.resolve_pointer_chain(&chain, scope);
                } else {
                    self.reference_dollar_chain(&chain, scope);
                }
                return;
            }
            Node::Identifier(name) => Rc::from(*name),
            Node::Root => Variable::root_key(),
            Node::Pointer => {
                self.resolve_pointer_chain(&chain, scope);
                return;
            }
            _ => return,
        };
        let root_span = self.node_span(chain.root);

        if scope.unresolved_aliases.contains(&root_name) {
            let mut path = vec![root_name.clone()];
            let mut spans = vec![root_span];
            for segment in &chain.segments {
                match segment {
                    ChainSegment::Field { name, prop } => {
                        path.push(name.clone());
                        spans.push(self.node_span(prop));
                    }
                    ChainSegment::Dynamic { prop } => self.resolve(prop, scope),
                }
            }
            let span = match (spans.first(), spans.last()) {
                (Some(first), Some(last)) => (first.0, last.1),
                _ => Default::default(),
            };
            self.reads.push(ReadDependency::Unresolved {
                path: path.clone(),
                span,
            });
            self.references.push(Reference {
                path,
                spans,
                via_alias: None,
                via_index: None,
            });

            return;
        }

        let via_alias_first =
            scope
                .aliases
                .get(&root_name)
                .cloned()
                .map(|collection| AliasBinding {
                    alias: root_name.clone(),
                    collection,
                });

        if via_alias_first.is_none() && scope.is_local(std::slice::from_ref(&root_name)) {
            for segment in &chain.segments {
                if let ChainSegment::Dynamic { prop } = segment {
                    self.resolve(prop, scope);
                }
            }
            return;
        }

        let mut group_path: Vec<Rc<str>> = vec![root_name.clone()];
        let mut group_spans: Vec<(u32, u32)> = vec![root_span];
        let mut cumulative: Vec<Rc<str>> = vec![root_name];
        let mut via_index_for_group: Option<Vec<Rc<str>>> = None;
        let mut is_first_group = true;

        for segment in &chain.segments {
            match segment {
                ChainSegment::Field { name, prop } => {
                    group_path.push(name.clone());
                    group_spans.push(self.node_span(prop));
                    cumulative.push(name.clone());
                }
                ChainSegment::Dynamic { prop } => {
                    let emittable = is_first_group || !group_path.is_empty();
                    if emittable && !group_path.is_empty() {
                        let via_alias = if is_first_group {
                            via_alias_first.clone()
                        } else {
                            None
                        };
                        self.emit_group(
                            std::mem::take(&mut group_path),
                            std::mem::take(&mut group_spans),
                            via_alias,
                            via_index_for_group.clone(),
                            scope,
                        );
                    }
                    group_path.clear();
                    group_spans.clear();
                    self.resolve(prop, scope);
                    via_index_for_group = Some(cumulative.clone());
                    is_first_group = false;
                }
            }
        }

        if !group_path.is_empty() {
            let via_alias = if is_first_group {
                via_alias_first
            } else {
                None
            };
            self.emit_group(
                group_path,
                group_spans,
                via_alias,
                via_index_for_group,
                scope,
            );
        }
    }

    fn reference_dollar_chain(&mut self, chain: &FlatChain, scope: &mut Scope) {
        let mut path: Vec<Rc<str>> = vec![Rc::from("$")];
        let mut spans = vec![self.node_span(chain.root)];
        let mut grouping = true;
        for segment in &chain.segments {
            match segment {
                ChainSegment::Field { name, prop } if grouping => {
                    path.push(name.clone());
                    spans.push(self.node_span(prop));
                }
                ChainSegment::Field { .. } => {}
                ChainSegment::Dynamic { prop } => {
                    grouping = false;
                    self.resolve(prop, scope);
                }
            }
        }
        if path.len() > 1 {
            self.references.push(Reference {
                path,
                spans,
                via_alias: None,
                via_index: None,
            });
        }
    }

    fn resolve_pointer_chain(&mut self, chain: &FlatChain, scope: &mut Scope) {
        let Some(collection) = scope.pointer_collection.clone() else {
            for segment in &chain.segments {
                if let ChainSegment::Dynamic { prop } = segment {
                    self.resolve(prop, scope);
                }
            }
            return;
        };

        let mut group_path: Vec<Rc<str>> = Vec::new();
        let mut group_spans: Vec<(u32, u32)> = Vec::new();
        let mut cumulative: Vec<Rc<str>> = collection.clone();
        let mut via_index_for_group = collection;

        for segment in &chain.segments {
            match segment {
                ChainSegment::Field { name, prop } => {
                    group_path.push(name.clone());
                    group_spans.push(self.node_span(prop));
                    cumulative.push(name.clone());
                }
                ChainSegment::Dynamic { prop } => {
                    if !group_path.is_empty() {
                        self.references.push(Reference {
                            path: std::mem::take(&mut group_path),
                            spans: std::mem::take(&mut group_spans),
                            via_alias: None,
                            via_index: Some(via_index_for_group.clone()),
                        });
                    }
                    self.resolve(prop, scope);
                    via_index_for_group = cumulative.clone();
                }
            }
        }

        if !group_path.is_empty() {
            self.references.push(Reference {
                path: group_path,
                spans: group_spans,
                via_alias: None,
                via_index: Some(via_index_for_group),
            });
        }
    }

    fn emit_group(
        &mut self,
        path: Vec<Rc<str>>,
        spans: Vec<(u32, u32)>,
        via_alias: Option<AliasBinding>,
        via_index: Option<Vec<Rc<str>>>,
        scope: &Scope,
    ) {
        if path.is_empty() {
            return;
        }
        if via_alias.is_none() && scope.is_local(&path) {
            return;
        }
        let span = match (spans.first(), spans.last()) {
            (Some(first), Some(last)) => (first.0, last.1),
            _ => Default::default(),
        };
        let read_path = match &via_index {
            Some(prefix) => prefix.iter().chain(path.iter()).cloned().collect(),
            None => path.clone(),
        };
        self.reads.push(ReadDependency::Direct {
            path: read_path,
            span,
            via_index: via_index.is_some(),
        });
        self.references.push(Reference {
            path,
            spans,
            via_alias,
            via_index,
        });
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn resolve(&mut self, node: &Node, scope: &mut Scope) {
        match node {
            Node::Identifier(name) => {
                let path = vec![Rc::from(*name)];
                let span = self.node_span(node);
                if scope.unresolved_aliases.contains(&path[0]) {
                    self.reads.push(ReadDependency::Unresolved {
                        path: path.clone(),
                        span,
                    });
                    self.references.push(Reference {
                        path,
                        spans: vec![span],
                        via_alias: None,
                        via_index: None,
                    });
                    return;
                }
                let via_alias =
                    scope
                        .aliases
                        .get(&path[0])
                        .cloned()
                        .map(|collection| AliasBinding {
                            alias: path[0].clone(),
                            collection,
                        });
                if via_alias.is_some() || !scope.is_local(&path) {
                    self.reads.push(ReadDependency::Direct {
                        path: path.clone(),
                        span,
                        via_index: false,
                    });
                    self.references.push(Reference {
                        path,
                        spans: vec![span],
                        via_alias,
                        via_index: None,
                    });
                }
            }

            Node::Member { .. } => self.resolve_member_chain(node, scope),

            Node::Assignments { list, output } => {
                for (key, value) in list.iter() {
                    self.resolve(value, scope);

                    if let Some(path) = Self::extract_binding_path(key) {
                        if path.len() == 1 {
                            scope.locals.insert(path[0].clone());
                        } else if path.len() > 1 {
                            scope.bindings.push(path);
                        }
                    }
                }

                if let Some(output) = output {
                    self.resolve(output, scope);
                }
            }

            Node::FunctionCall { kind, arguments } => {
                if let FunctionKind::Closure(_) = kind {
                    if arguments.len() >= 2 {
                        let collection_node = arguments[0];
                        let closure_node = arguments[1];

                        let collection_info = self.extract_path_with_spans(collection_node);
                        let collection_source = collection_info
                            .as_ref()
                            .map(|(p, _)| p.clone())
                            .or_else(|| Self::collection_source_path(collection_node));

                        let (alias, inner_reads, inner_refs) = match closure_node {
                            Node::Closure { body, alias } => {
                                let mut inner_scope = scope.clone();
                                inner_scope.locals.insert(Variable::dollar_key());
                                inner_scope.pointer_collection = collection_source
                                    .as_ref()
                                    .filter(|source| !scope.is_local(source))
                                    .map(|source| scope.expand_alias_root(source));

                                match (alias, collection_source.as_ref()) {
                                    (Some(alias_name), Some(source)) => {
                                        let expanded = scope.expand_alias_root(source);
                                        if scope.is_local(&expanded) {
                                            inner_scope
                                                .unresolved_aliases
                                                .insert(Rc::from(*alias_name));
                                        } else {
                                            inner_scope
                                                .aliases
                                                .insert(Rc::from(*alias_name), expanded);
                                        }
                                    }
                                    (Some(alias_name), None) => {
                                        inner_scope
                                            .unresolved_aliases
                                            .insert(Rc::from(*alias_name));
                                    }
                                    _ => {}
                                }
                                let (reads, refs) =
                                    Self::walk_inner(body, &mut inner_scope, self.metadata);
                                (alias.map(|a| Rc::from(a)), reads, refs)
                            }
                            _ => {
                                self.resolve(closure_node, scope);
                                return;
                            }
                        };

                        match collection_info {
                            Some((collection, spans)) if !scope.is_local(&collection) => {
                                let span = self.node_span(collection_node);
                                self.references.push(Reference {
                                    path: collection.clone(),
                                    spans,
                                    via_alias: scope.alias_binding_for(&collection),
                                    via_index: None,
                                });
                                self.references.extend(inner_refs);
                                self.reads.push(ReadDependency::Iteration {
                                    collection,
                                    span,
                                    alias,
                                    reads: inner_reads,
                                });
                            }
                            _ => {
                                self.resolve(collection_node, scope);
                                match collection_source {
                                    Some(source)
                                        if !source.is_empty() && !scope.is_local(&source) =>
                                    {
                                        let span = self.node_span(collection_node);
                                        self.references.extend(inner_refs);
                                        self.reads.push(ReadDependency::Iteration {
                                            collection: source,
                                            span,
                                            alias,
                                            reads: inner_reads,
                                        });
                                    }
                                    _ => {
                                        self.reads.extend(inner_reads);
                                        self.references.extend(inner_refs);
                                    }
                                }
                            }
                        }
                    } else {
                        for arg in arguments.iter() {
                            self.resolve(arg, scope);
                        }
                    }
                } else {
                    for arg in arguments.iter() {
                        self.resolve(arg, scope);
                    }
                }
            }

            Node::Closure { body, alias: _ } => {
                let mut inner = scope.clone();
                inner.locals.insert(Variable::dollar_key());
                inner.pointer_collection = None;
                self.resolve(body, &mut inner);
            }

            Node::MethodCall {
                this, arguments, ..
            } => {
                self.resolve(this, scope);
                for arg in arguments.iter() {
                    self.resolve(arg, scope);
                }
            }

            Node::Binary { left, right, .. } => {
                self.resolve(left, scope);
                self.resolve(right, scope);
            }

            Node::Unary { node, .. } => self.resolve(node, scope),

            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.resolve(condition, scope);
                self.resolve(on_true, scope);
                self.resolve(on_false, scope);
            }

            Node::Parenthesized(n) => self.resolve(n, scope),

            Node::Array(items) => {
                for item in items.iter() {
                    self.resolve(item, scope);
                }
            }

            Node::Object(pairs) => {
                for (k, v) in pairs.iter() {
                    self.resolve(k, scope);
                    self.resolve(v, scope);
                }
            }

            Node::TemplateString(parts) => {
                for part in parts.iter() {
                    self.resolve(part, scope);
                }
            }

            Node::Slice { node, from, to } => {
                self.resolve(node, scope);
                if let Some(f) = from {
                    self.resolve(f, scope);
                }
                if let Some(t) = to {
                    self.resolve(t, scope);
                }
            }

            Node::Interval { left, right, .. } => {
                self.resolve(left, scope);
                self.resolve(right, scope);
            }

            Node::Error { node, .. } => {
                if let Some(n) = node {
                    self.resolve(n, scope);
                }
            }

            Node::Root => {
                let path: Vec<Rc<str>> = vec![Variable::root_key()];
                let span = self.node_span(node);
                self.reads.push(ReadDependency::Direct {
                    path: path.clone(),
                    span,
                    via_index: false,
                });
                self.references.push(Reference {
                    path,
                    spans: vec![span],
                    via_alias: None,
                    via_index: None,
                });
            }

            Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) | Node::Pointer => {}
        }
    }

    fn extract_binding_path(node: &Node) -> Option<Vec<Rc<str>>> {
        match node {
            Node::String(s) => Some(s.split('.').map(Rc::from).collect()),
            _ => Self::extract_read_path(node),
        }
    }

    fn extract_read_path(node: &Node) -> Option<Vec<Rc<str>>> {
        match node {
            Node::Identifier(name) => Some(vec![Rc::from(*name)]),
            Node::Root => Some(vec![Variable::root_key()]),
            Node::Member { node, property } => {
                let mut path = Self::extract_read_path(node)?;
                match property {
                    Node::String(key) => {
                        path.push(Rc::from(*key));
                        Some(path)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}
