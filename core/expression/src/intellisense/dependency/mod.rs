use crate::functions::FunctionKind;
use crate::parser::Node;
use ahash::HashSet;
use bumpalo::collections::String as BumpString;
use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use serde::Serialize;
use std::rc::Rc;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyProviderResponse {
    provides: HashSet<Rc<str>>,
    dependencies: HashSet<Rc<str>>,
    confidence: ConfidenceLevel,
}

impl From<DependencyProvider<'_>> for DependencyProviderResponse {
    fn from(value: DependencyProvider) -> Self {
        Self {
            provides: value.provides,
            dependencies: value.dependencies,
            confidence: value.confidence,
        }
    }
}

#[derive(Debug)]
pub struct DependencyProvider<'arena> {
    dependencies: HashSet<Rc<str>>,
    provides: HashSet<Rc<str>>,
    confidence: ConfidenceLevel,
    arena: &'arena Bump,
}

#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
enum ConfidenceLevel {
    Static,
    Dynamic,
}

impl<'arena> DependencyProvider<'arena> {
    pub fn generate(root: &Node<'arena>, arena: &'arena Bump) -> DependencyProviderResponse {
        let mut provider = Self {
            provides: Default::default(),
            dependencies: Default::default(),
            confidence: ConfidenceLevel::Static,
            arena,
        };

        provider.determine(
            root,
            &Scope {
                arena,
                kind: ScopeKind::Root,
                current_pointer: None,
            },
        );

        DependencyProviderResponse::from(provider)
    }

    fn set_dynamic(&mut self) {
        self.confidence = ConfidenceLevel::Dynamic;
    }

    fn determine(&mut self, node: &Node<'arena>, scope: &Scope<'arena>) -> DetermineResult<'arena> {
        use DetermineResult as D;

        match node {
            Node::Null | Node::Bool(_) | Node::Number(_) => D::None,
            Node::String(s) => D::Str(s),
            Node::Assignments(assignments) => {
                for (k, v) in *assignments {
                    let Node::String(key) = k else {
                        self.set_dynamic();
                        continue;
                    };

                    self.provides.insert(Rc::from(*key));
                    self.determine(v, scope);
                }

                D::None
            }
            Node::Array(items) | Node::TemplateString(items) => {
                let mut parts = BumpVec::new_in(&self.arena);
                for item in *items {
                    parts.push(self.determine(item, scope));
                }

                let is_defined = parts.iter().all(|p| matches!(p, D::Str(_)));
                if is_defined {
                    let collected: String = parts
                        .into_iter()
                        .filter_map(|p| match p {
                            DetermineResult::None
                            | DetermineResult::Ref(_)
                            | DetermineResult::Root => None,
                            DetermineResult::Str(s) => Some(s),
                        })
                        .collect();

                    return D::Str(self.arena.alloc_str(collected.as_str()));
                }

                D::None
            }
            Node::Object(object) => {
                for (k, v) in *object {
                    self.determine(k, scope);
                    self.determine(v, scope);
                }

                D::None
            }

            Node::Identifier(identifier) => match scope.kind {
                ScopeKind::Property => D::Ref(identifier),
                ScopeKind::Root => {
                    self.dependencies.insert(Rc::from(*identifier));
                    D::None
                }
            },
            Node::Member { node, property } => {
                match (
                    self.determine(node, &scope.property()),
                    self.determine(property, &scope.property()),
                ) {
                    (D::Ref(base), D::Str(property)) => {
                        let mut b = BumpString::new_in(&self.arena);
                        b.push_str(base);
                        b.push_str(".");
                        b.push_str(property);

                        if scope.kind == ScopeKind::Root {
                            self.dependencies.insert(Rc::from(b.as_str()));
                            D::None
                        } else {
                            D::Ref(b.into_bump_str())
                        }
                    }
                    // 2 references - turn into 2 dependencies effectively
                    (D::Ref(base), D::Ref(property)) => {
                        self.dependencies.insert(Rc::from(property));

                        if scope.kind == ScopeKind::Root {
                            self.dependencies.insert(Rc::from(base));
                            D::None
                        } else {
                            D::Ref(base)
                        }
                    }
                    // Root is handled much same in both cases
                    (D::Root, D::Str(property) | D::Ref(property)) => {
                        if scope.kind == ScopeKind::Root {
                            self.dependencies.insert(Rc::from(property));
                            D::None
                        } else {
                            D::Ref(property)
                        }
                    }
                    (a, b) => {
                        println!("Unknown variant!!!: {a:?} {b:?}");
                        D::None
                    }
                }
            }

            // Technically this moves to "current" scope,
            Node::Root => D::Root,
            Node::Pointer => match scope.current_pointer {
                None => {
                    println!("No pointer {scope:?}. This should be impossible.");
                    D::None
                }
                Some(s) => D::Ref(s),
            },

            Node::Slice { node, from, to } => {
                self.determine(node, scope);
                if let Some(from) = from {
                    self.determine(from, scope);
                }

                if let Some(to) = to {
                    self.determine(to, scope);
                }

                D::None
            }
            Node::Binary { left, right, .. } => {
                self.determine(left, scope);
                self.determine(right, scope);

                D::None
            }
            Node::Unary { node, .. } => {
                self.determine(node, scope);

                D::None
            }
            Node::Interval { left, right, .. } => {
                self.determine(left, scope);
                self.determine(right, scope);

                D::None
            }
            Node::Conditional {
                condition,
                on_false,
                on_true,
            } => {
                self.determine(condition, scope);
                self.determine(on_false, scope);
                self.determine(on_true, scope);

                D::None
            }
            Node::Parenthesized(n) => self.determine(n, scope),
            Node::Closure(c) => self.determine(c, scope),
            Node::FunctionCall { kind, arguments } => match kind {
                FunctionKind::Internal(_) | FunctionKind::Deprecated(_) => {
                    for arg in *arguments {
                        self.determine(arg, scope);
                    }

                    D::None
                }
                FunctionKind::Closure(_) => {
                    let [target, closure] = arguments else {
                        self.set_dynamic();
                        return D::None;
                    };

                    let target_ref = match self.determine(target, &scope.property()) {
                        DetermineResult::Ref(r) => {
                            let mut full_target = BumpString::new_in(&self.arena);
                            full_target.push_str(r);
                            full_target.push_str("[]");
                            full_target.into_bump_str()
                        }
                        _ => {
                            self.set_dynamic();
                            return D::None;
                        }
                    };

                    let mut scp = scope.property();
                    scp.current_pointer = Some(target_ref);

                    let closure_ref = self.determine(closure, &scp);
                    match closure_ref {
                        DetermineResult::None | DetermineResult::Str(_) | DetermineResult::Root => {
                            self.set_dynamic();
                            D::None
                        }
                        DetermineResult::Ref(r) => {
                            self.dependencies.insert(Rc::from(r));
                            D::None
                        }
                    }
                }
            },
            Node::MethodCall {
                this, arguments, ..
            } => {
                self.determine(this, scope);
                for arg in *arguments {
                    self.determine(arg, scope);
                }

                D::None
            }
            Node::Error { node, .. } => {
                if let Some(n) = node {
                    self.determine(n, scope);
                }

                D::Root
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Scope<'a> {
    arena: &'a Bump,
    kind: ScopeKind,
    current_pointer: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
enum ScopeKind {
    Root,
    Property,
}

impl<'a> Scope<'a> {
    fn property(&self) -> Self {
        Self {
            arena: self.arena,
            current_pointer: self.current_pointer,
            kind: ScopeKind::Property,
        }
    }

    #[allow(unused)]
    fn root(&self) -> Self {
        Self {
            arena: self.arena,
            current_pointer: self.current_pointer,
            kind: ScopeKind::Root,
        }
    }
}

#[derive(Debug)]
enum DetermineResult<'arena> {
    None,
    Str(&'arena str),
    Ref(&'arena str),
    Root,
}
