use std::sync::Arc;

use zen_expression::intellisense::dependency::ReadDependency;

use super::context::PropertyRead;

pub(crate) struct ReadFlattener;

impl ReadFlattener {
    pub(crate) fn extend_from_deps(
        deps: &[ReadDependency],
        expression_id: &Option<Arc<str>>,
        out: &mut Vec<PropertyRead>,
    ) {
        let aliases: ahash::HashMap<std::rc::Rc<str>, Vec<std::rc::Rc<str>>> =
            ahash::HashMap::default();
        Self::walk_deps(deps, expression_id, &aliases, out, 0);
    }

    fn walk_deps(
        deps: &[ReadDependency],
        expression_id: &Option<Arc<str>>,
        aliases: &ahash::HashMap<std::rc::Rc<str>, Vec<std::rc::Rc<str>>>,
        out: &mut Vec<PropertyRead>,
        depth: usize,
    ) {
        if depth >= crate::policy::MAX_RECURSION_DEPTH {
            return;
        }
        use crate::policy::queries::scope::PathSegments;
        for dep in deps {
            match dep {
                ReadDependency::Direct {
                    path,
                    span,
                    via_index,
                } => {
                    let was_aliased = !path.is_empty() && aliases.contains_key(&path[0]);
                    let resolved = Self::resolve_path(path, aliases);
                    if !resolved.is_empty() {
                        out.push(PropertyRead {
                            path: Arc::from(resolved.as_slice().to_dotted()),
                            expression_id: expression_id.clone(),
                            span: Some(*span),
                            via_alias: was_aliased || *via_index,
                            unresolved: false,
                        });
                    }
                }
                ReadDependency::Unresolved { path, span } => {
                    let resolved = Self::resolve_path(path, aliases);
                    if !resolved.is_empty() {
                        out.push(PropertyRead {
                            path: Arc::from(resolved.as_slice().to_dotted()),
                            expression_id: expression_id.clone(),
                            span: Some(*span),
                            via_alias: true,
                            unresolved: true,
                        });
                    }
                }
                ReadDependency::Iteration {
                    collection,
                    span,
                    alias,
                    reads,
                } => {
                    let collection_was_aliased =
                        !collection.is_empty() && aliases.contains_key(&collection[0]);
                    let resolved_collection = Self::resolve_path(collection, aliases);
                    if !resolved_collection.is_empty() {
                        out.push(PropertyRead {
                            path: Arc::from(resolved_collection.as_slice().to_dotted()),
                            expression_id: expression_id.clone(),
                            span: Some(*span),
                            via_alias: collection_was_aliased,
                            unresolved: false,
                        });
                    }
                    if let Some(a) = alias {
                        let mut inner_aliases = aliases.clone();
                        inner_aliases.insert(a.clone(), resolved_collection);
                        Self::walk_deps(reads, expression_id, &inner_aliases, out, depth + 1);
                    } else {
                        Self::walk_deps(reads, expression_id, aliases, out, depth + 1);
                    }
                }
            }
        }
    }

    fn resolve_path(
        path: &[std::rc::Rc<str>],
        aliases: &ahash::HashMap<std::rc::Rc<str>, Vec<std::rc::Rc<str>>>,
    ) -> Vec<std::rc::Rc<str>> {
        if path.is_empty() {
            return Vec::new();
        }
        if let Some(expansion) = aliases.get(&path[0]) {
            let mut out = expansion.clone();
            out.extend_from_slice(&path[1..]);
            out
        } else {
            path.to_vec()
        }
    }
}
