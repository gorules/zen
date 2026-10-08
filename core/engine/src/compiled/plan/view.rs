use std::sync::Arc;

pub(crate) type SlotId = u32;
pub(crate) type MaskId = u32;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub(crate) enum M {
    None,
    All,
    Id(MaskId),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Expr {
    Validity(SlotId),
    Valued(SlotId),
    And(M, M),
    Or(M, M),
    Not(M),
    Produced(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shape {
    Num,
    Bool,
    Text,
    Null,
    Dyn,
    List,
    Unknown,
}

impl Shape {
    pub fn scalar(self) -> bool {
        matches!(self, Shape::Num | Shape::Bool | Shape::Text | Shape::Null)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Ref {
    pub slot: SlotId,
    pub present: M,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) struct Node {
    pub leaf: Option<Ref>,
    pub obj: Option<M>,
    pub fields: Vec<(Arc<str>, Node)>,
}

pub(crate) enum Resolved {
    Leaf(Ref),
    Absent,
    Object(Node),
    Extract(Ref, Vec<Arc<str>>),
}

#[derive(Default, Clone)]
pub(crate) struct Masks {
    pub exprs: Vec<Expr>,
    produced: u32,
}

impl Masks {
    pub fn produced(&mut self) -> M {
        self.produced += 1;
        self.intern(Expr::Produced(self.produced - 1))
    }

    fn intern(&mut self, expr: Expr) -> M {
        match self.exprs.iter().position(|e| *e == expr) {
            Some(at) => M::Id(at as MaskId),
            None => {
                self.exprs.push(expr);
                M::Id((self.exprs.len() - 1) as MaskId)
            }
        }
    }

    pub fn validity(&mut self, slot: SlotId) -> M {
        self.intern(Expr::Validity(slot))
    }

    pub fn valued(&mut self, slot: SlotId) -> M {
        self.intern(Expr::Valued(slot))
    }

    fn negates(&self, a: M, b: M) -> bool {
        match (a, b) {
            (M::Id(x), y) if self.exprs[x as usize] == Expr::Not(y) => true,
            (x, M::Id(y)) if self.exprs[y as usize] == Expr::Not(x) => true,
            _ => false,
        }
    }

    pub fn and(&mut self, a: M, b: M) -> M {
        match (a, b) {
            (M::None, _) | (_, M::None) => M::None,
            (M::All, x) | (x, M::All) => x,
            (x, y) if x == y => x,
            (x, y) if self.negates(x, y) => M::None,
            (x, y) => self.intern(Expr::And(x.min(y), x.max(y))),
        }
    }

    pub fn or(&mut self, a: M, b: M) -> M {
        match (a, b) {
            (M::All, _) | (_, M::All) => M::All,
            (M::None, x) | (x, M::None) => x,
            (x, y) if x == y => x,
            (x, y) if self.negates(x, y) => M::All,
            (x, y) => self.intern(Expr::Or(x.min(y), x.max(y))),
        }
    }

    pub fn implied(&self, leaf: Ref) -> bool {
        match leaf.present {
            M::All => true,
            M::None => false,
            M::Id(id) => matches!(self.exprs[id as usize], Expr::Validity(s) | Expr::Valued(s) if s == leaf.slot),
        }
    }

    pub fn not(&mut self, a: M) -> M {
        match a {
            M::None => M::All,
            M::All => M::None,
            M::Id(id) => match self.exprs[id as usize] {
                Expr::Not(inner) => inner,
                _ => self.intern(Expr::Not(a)),
            },
        }
    }

    pub fn and_not(&mut self, a: M, b: M) -> M {
        let b = self.not(b);
        self.and(a, b)
    }

    pub fn any(&mut self, items: impl IntoIterator<Item = M>) -> M {
        items.into_iter().fold(M::None, |acc, m| self.or(acc, m))
    }
}

pub(crate) trait Slots {
    fn shape(&self, slot: SlotId) -> Shape;
    fn select(&mut self, a: Ref, b: Ref, take: M, present: M) -> SlotId;
    fn merge_rows(&mut self, a: Node, b: Node, parts: [M; 3], present: M) -> SlotId;
}

impl Node {
    pub fn root(obj: M) -> Node {
        Node {
            leaf: None,
            obj: Some(obj),
            fields: Vec::new(),
        }
    }

    pub fn empty(&self) -> bool {
        self.leaf.is_none() && self.obj.is_none_or(|o| o == M::None) && self.fields.is_empty()
    }

    fn obj(&self) -> M {
        self.obj.unwrap_or(M::None)
    }

    pub fn insert(&mut self, path: &str, leaf: Ref, obj: M) -> Result<(), String> {
        let mut node = self;
        let mut segments = path.split('.').peekable();
        while let Some(segment) = segments.next() {
            let at = match node.fields.iter().position(|(k, _)| k.as_ref() == segment) {
                Some(at) => at,
                None => {
                    node.fields.push((Arc::from(segment), Node::default()));
                    node.fields.len() - 1
                }
            };
            node = &mut node.fields[at].1;
            match segments.peek() {
                Some(_) => {
                    if node.leaf.is_some() {
                        return Err(format!("path {path} overlaps a leaf"));
                    }
                    node.obj = Some(obj);
                }
                None => {
                    if !node.fields.is_empty() {
                        return Err(format!("path {path} overlaps an object"));
                    }
                    node.leaf = Some(leaf);
                }
            }
        }
        Ok(())
    }

    pub fn insert_node(&mut self, path: &str, value: Node, obj: M) -> Result<(), String> {
        let mut node = self;
        let mut segments = path.split('.').peekable();
        while let Some(segment) = segments.next() {
            let at = match node.fields.iter().position(|(k, _)| k.as_ref() == segment) {
                Some(at) => at,
                None => {
                    node.fields.push((Arc::from(segment), Node::default()));
                    node.fields.len() - 1
                }
            };
            node = &mut node.fields[at].1;
            match segments.peek() {
                Some(_) => {
                    if node.leaf.is_some() {
                        return Err(format!("path {path} overlaps a leaf"));
                    }
                    node.obj = Some(obj);
                }
                None => {
                    if !node.fields.is_empty() || node.leaf.is_some() {
                        return Err(format!("path {path} overlaps a value"));
                    }
                    *node = value;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    pub fn insert_object(&mut self, path: &str, obj: M) -> Result<(), String> {
        let mut node = self;
        for segment in path.split('.') {
            let at = match node.fields.iter().position(|(k, _)| k.as_ref() == segment) {
                Some(at) => at,
                None => {
                    node.fields.push((Arc::from(segment), Node::default()));
                    node.fields.len() - 1
                }
            };
            node = &mut node.fields[at].1;
            if node.leaf.is_some() {
                return Err(format!("object {path} overlaps a leaf"));
            }
            node.obj = Some(obj);
        }
        Ok(())
    }

    pub fn objects_from_leaves(&mut self, masks: &mut Masks) -> M {
        let mut present = Vec::new();
        for (_, child) in self.fields.iter_mut() {
            present.push(child.objects_from_leaves(masks));
        }
        if let Some(leaf) = self.leaf {
            present.push(leaf.present);
        }
        let any = masks.any(present);
        if !self.fields.is_empty() {
            self.obj = Some(any);
        }
        any
    }

    pub fn resolve(&self, path: &str, slots: &impl Slots) -> Result<Resolved, String> {
        let mut node = self;
        let segments: Vec<&str> = path.split('.').collect();
        for (at, segment) in segments.iter().enumerate() {
            if let Some(leaf) = node.leaf {
                if !slots.shape(leaf.slot).scalar() {
                    return match node.fields.is_empty() {
                        true => Ok(Resolved::Extract(leaf, segments[at..].iter().map(|s| Arc::from(*s)).collect())),
                        false => Err(format!("read {path} goes through a dynamic value")),
                    };
                }
            }
            match node.fields.iter().find(|(k, _)| k.as_ref() == *segment) {
                Some((_, child)) => node = child,
                None => return Ok(Resolved::Absent),
            }
        }
        match (node.leaf, node.fields.is_empty()) {
            (Some(leaf), true) => Ok(Resolved::Leaf(leaf)),
            (None, true) => Ok(Resolved::Absent),
            (None, false) => Ok(Resolved::Object(node.clone())),
            (Some(_), false) => Err(format!("read {path} is a value or an object")),
        }
    }

    pub fn wrap(node: Node, path: &str) -> Node {
        let mut segments: Vec<&str> = path.split('.').collect();
        let mut current = node;
        while let Some(last) = segments.pop() {
            current = Node {
                leaf: None,
                obj: Some(M::All),
                fields: vec![(Arc::from(last), current)],
            };
        }
        current
    }

    pub fn head(&self) -> Node {
        let mut head = self.clone();
        head.obj = Some(M::All);
        head
    }

    pub fn merge(a: &Node, b: &Node, masks: &mut Masks, slots: &mut dyn Slots) -> Result<Node, String> {
        if a.leaf.is_some() || b.leaf.is_some() {
            return Self::child(a, b, M::All, M::None, M::None, masks, slots);
        }
        let (oa, ob) = (a.obj(), b.obj());
        let rr = masks.and(oa, ob);
        let rb = masks.and_not(ob, oa);
        let ra = masks.not(ob);
        let mut node = Node::root(M::None);
        let left = masks.and(ra, oa);
        let both = masks.or(ob, left);
        node.obj = Some(both);
        node.fields = Self::fields(a, b, rr, rb, ra, masks, slots)?;
        Ok(node)
    }

    fn fields(a: &Node, b: &Node, rr: M, rb: M, ra: M, masks: &mut Masks, slots: &mut dyn Slots) -> Result<Vec<(Arc<str>, Node)>, String> {
        let empty = Node::default();
        let mut keys: Vec<&Arc<str>> = a.fields.iter().map(|(k, _)| k).collect();
        for (k, _) in &b.fields {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let ca = a.fields.iter().find(|(k, _)| k == key).map_or(&empty, |(_, n)| n);
            let cb = b.fields.iter().find(|(k, _)| k == key).map_or(&empty, |(_, n)| n);
            let merged = Self::child(ca, cb, rr, rb, ra, masks, slots)?;
            if !merged.empty() {
                out.push((key.clone(), merged));
            }
        }
        Ok(out)
    }

    fn child(a: &Node, b: &Node, rr: M, rb: M, ra: M, masks: &mut Masks, slots: &mut dyn Slots) -> Result<Node, String> {
        let pb = b.leaf.map_or(M::None, |l| l.present);
        let pa = a.leaf.map_or(M::None, |l| l.present);
        let (oa, ob) = (a.obj(), b.obj());
        let dynamic = |leaf: Option<Ref>| leaf.is_some_and(|l| !slots.shape(l.slot).scalar() && slots.shape(l.slot) != Shape::List);
        let (a_dyn, b_dyn) = (dynamic(a.leaf), dynamic(b.leaf));
        let a_object = a_dyn || oa != M::None;
        let b_object = b_dyn || ob != M::None;
        if rr != M::None && a_object && b_object && (a_dyn || b_dyn) {
            {
                let present = masks.produced();
                let slot = slots.merge_rows(a.clone(), b.clone(), [rr, rb, ra], present);
                return Ok(Node {
                    leaf: Some(Ref { slot, present }),
                    obj: None,
                    fields: Vec::new(),
                });
            }
        }
        let vb = match b.leaf {
            Some(leaf) => masks.valued(leaf.slot),
            None => M::None,
        };
        let keep_b = masks.and(rb, pb);
        let set_b = {
            let x = masks.and(rr, pb);
            masks.and(x, vb)
        };
        let lb = masks.or(keep_b, set_b);
        let untouched = {
            let x = masks.and_not(rr, pb);
            masks.and_not(x, ob)
        };
        let keep = masks.or(ra, untouched);
        let la = masks.and(pa, keep);
        let leaf = match (b.leaf, a.leaf, lb, la) {
            (_, _, M::None, M::None) => None,
            (Some(b), _, lb, M::None) => Some(Ref { slot: b.slot, present: lb }),
            (_, Some(a), M::None, la) => Some(Ref { slot: a.slot, present: la }),
            (Some(b), Some(a), lb, la) => {
                let present = masks.or(lb, la);
                Some(Ref {
                    slot: slots.select(a, b, lb, present),
                    present,
                })
            }
            _ => None,
        };
        let obj = {
            let b_side = {
                let r = masks.or(rb, rr);
                masks.and(ob, r)
            };
            let a_side = {
                let kept = masks.and_not(rr, pb);
                let r = masks.or(ra, kept);
                masks.and(oa, r)
            };
            masks.or(b_side, a_side)
        };
        let rr2 = {
            let x = masks.and(rr, ob);
            masks.and(x, oa)
        };
        let rb2 = {
            let r = masks.and_not(rr, oa);
            let r = masks.or(rb, r);
            masks.and(ob, r)
        };
        let ra2 = masks.and(oa, keep);
        let fields = match a.fields.is_empty() && b.fields.is_empty() {
            true => Vec::new(),
            false => Self::fields(a, b, rr2, rb2, ra2, masks, slots)?,
        };
        Ok(Node {
            leaf,
            obj: (obj != M::None).then_some(obj),
            fields,
        })
    }

    pub fn leaves(&self, prefix: &str, out: &mut Vec<(Arc<str>, Ref)>) {
        if let Some(leaf) = self.leaf {
            out.push((Arc::from(prefix), leaf));
        }
        for (key, child) in &self.fields {
            let path = match prefix.is_empty() {
                true => key.to_string(),
                false => format!("{prefix}.{key}"),
            };
            child.leaves(&path, out);
        }
    }
}
