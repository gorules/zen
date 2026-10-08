use crate::compiled::typed::{Bits, Leaf};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::Variable;

struct Trie {
    path: Arc<str>,
    children: Vec<(Arc<str>, usize)>,
    column: Option<usize>,
}

pub(crate) struct Shredder {
    rows: usize,
    nodes: Vec<Trie>,
    columns: Vec<(Arc<str>, Vec<Variable>, Vec<u64>)>,
}

impl Shredder {
    pub(crate) fn new(rows: usize) -> Self {
        Self {
            rows,
            nodes: vec![Trie {
                path: Arc::from(""),
                children: Vec::new(),
                column: None,
            }],
            columns: Vec::new(),
        }
    }

    fn child(&mut self, node: usize, key: &str) -> usize {
        if let Some((_, at)) = self.nodes[node].children.iter().find(|(k, _)| k.as_ref() == key) {
            return *at;
        }
        let path: Arc<str> = match self.nodes[node].path.is_empty() {
            true => Arc::from(key),
            false => Arc::from(format!("{}.{key}", self.nodes[node].path)),
        };
        self.nodes.push(Trie {
            path,
            children: Vec::new(),
            column: None,
        });
        let at = self.nodes.len() - 1;
        self.nodes[node].children.push((Arc::from(key), at));
        at
    }

    fn put(&mut self, node: usize, row: usize, value: Variable) {
        let column = match self.nodes[node].column {
            Some(column) => column,
            None => {
                self.columns.push((self.nodes[node].path.clone(), vec![Variable::Null; self.rows], vec![0u64; self.rows.div_ceil(64)]));
                self.nodes[node].column = Some(self.columns.len() - 1);
                self.columns.len() - 1
            }
        };
        let (_, values, present) = &mut self.columns[column];
        values[row] = value;
        Bits::set(present, row, true);
    }

    pub(crate) fn visit(&mut self, value: &Variable, node: usize, row: usize) {
        match value {
            Variable::Object(map) => {
                let map = map.borrow();
                if map.iter().any(|(key, _)| key.is_empty() || key.contains('.')) {
                    self.put(node, row, value.clone());
                    return;
                }
                for (key, child) in map.iter() {
                    let at = self.child(node, key);
                    self.visit(child, at, row);
                }
            }
            Variable::Null => {}
            other => self.put(node, row, other.clone()),
        }
    }

    pub(crate) fn finish<'a>(self) -> Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)> {
        self.columns
            .into_iter()
            .map(|(path, values, present)| (path, Leaf::Any(values.into()), Some(present.into())))
            .collect()
    }
}
