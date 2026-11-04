use super::collect::{Slot, Template};
use super::core::escape_seg;
use super::native::{same_prim, veq, write_clean};
use super::value::write_str;
use smallvec::SmallVec;
use zen_types::variable::Variable;

pub struct RowWriter<'t> {
    sources: Vec<(&'t str, Variable)>,
    template: &'t Template,
    open: SmallVec<[Slot; 8]>,
    count: usize,
    used: SmallVec<[Slot; 8]>,
    path: String,
    sets: Vec<(String, Variable)>,
    deletes: Vec<String>,
}

impl<'t> RowWriter<'t> {
    pub fn new(template: &'t Template) -> Self {
        Self {
            sources: Vec::new(),
            template,
            open: SmallVec::new(),
            count: 0,
            used: SmallVec::new(),
            path: String::new(),
            sets: Vec::new(),
            deletes: Vec::new(),
        }
    }

    pub fn fits(&self, slot: Slot) -> bool {
        self.template.node(slot).is_some_and(|n| n.head.is_some())
    }

    pub fn begin(
        &mut self,
        engine: Option<&str>,
        evaluation: Option<&super::Evaluation>,
        out: &mut String,
    ) {
        self.open.clear();
        self.count = 0;
        self.used.clear();
        out.push_str("{\"version\":1,\"kind\":\"graph\"");
        if let Some(engine) = engine {
            out.push_str(",\"engine\":");
            write_str(engine, out);
        }
        if let Some(e) = evaluation {
            e.write(out);
        }
        out.push_str(",\"nodes\":[");
    }

    pub fn finish(&mut self, out: &mut String) {
        out.push_str("]}");
    }

    pub fn first(&self) -> bool {
        self.count == 0
    }

    pub fn node(&mut self, slot: Slot, perf: &str, parents: &[Slot], out: &mut String) -> bool {
        let Some(head) = self.template.node(slot).and_then(|n| n.head.as_deref()) else {
            return false;
        };
        if self.count > 0 {
            out.push(',');
        }
        out.push_str(head);
        out.push_str(perf);
        self.used.clear();
        match (self.count, parents.is_empty()) {
            (0, true) => {}
            (0, false) | (_, true) => return false,
            (_, false) => {
                if self.open.as_slice() != parents {
                    out.push_str(",\"input\":{\"$merge\":[");
                    for (i, p) in parents.iter().enumerate() {
                        let Some(key) = self.template.node(*p).map(|n| n.key.as_str()) else {
                            return false;
                        };
                        if i > 0 {
                            out.push(',');
                        }
                        out.push_str(key);
                    }
                    out.push_str("]}");
                }
                self.used.extend_from_slice(parents);
            }
        }
        true
    }

    pub fn end(&mut self, slot: Slot, out: &mut String) {
        out.push('}');
        if !self.used.is_empty() {
            match self.open == self.used {
                true => self.open.clear(),
                false => {
                    let used = std::mem::take(&mut self.used);
                    self.open.retain(|x| !used.contains(x));
                    self.used = used;
                }
            }
            self.used.clear();
        }
        self.open.push(slot);
        self.count += 1;
    }

    pub fn touched(&mut self, key: &str, before: Option<&Variable>, after: Option<&Variable>) {
        self.path.clear();
        escape_seg(key, &mut self.path);
        let mut path = std::mem::take(&mut self.path);
        self.diff(&mut path, before, after);
        self.path = path;
    }

    fn diff(&mut self, path: &mut String, before: Option<&Variable>, after: Option<&Variable>) {
        match (before, after) {
            (None, None) => {}
            (Some(_), None) => self.deletes.push(path.clone()),
            (None, Some(b)) => self.sets.push((path.clone(), b.clone())),
            (Some(a), Some(b)) => {
                if same_prim(a, b) {
                    return;
                }
                match (a, b) {
                    (Variable::Object(x), Variable::Object(y)) => {
                        let (x, y) = (x.borrow(), y.borrow());
                        let mark = path.len();
                        for (k, _) in x.iter() {
                            if !y.contains_key_str(k) && &**k != "$nodes" {
                                escape_seg(k, path);
                                self.deletes.push(path.clone());
                                path.truncate(mark);
                            }
                        }
                        for (k, v) in y.iter() {
                            if &**k == "$nodes" {
                                continue;
                            }
                            escape_seg(k, path);
                            self.diff(path, x.get_str(k), Some(v));
                            path.truncate(mark);
                        }
                    }
                    _ if veq(a, b) => {}
                    _ => self.sets.push((path.clone(), b.clone())),
                }
            }
        }
    }

    pub fn edits(&mut self, from: Option<&str>, out: &mut String) -> bool {
        if self.sets.is_empty() && self.deletes.is_empty() {
            if let Some(from) = from {
                self.output_from(from, out);
            }
            return true;
        }
        self.deletes.sort_unstable();
        self.sets.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        out.push_str(",\"output\":{");
        if let Some(from) = from {
            out.push_str("\"$from\":\"");
            out.push_str(from);
            out.push_str("\",");
        }
        if !self.deletes.is_empty() {
            out.push_str("\"$delete\":[");
            for (i, p) in self.deletes.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(p, out);
            }
            out.push(']');
        }
        let mut ok = true;
        if !self.sets.is_empty() {
            if !self.deletes.is_empty() {
                out.push(',');
            }
            out.push_str("\"$set\":{");
            for (i, (p, v)) in self.sets.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(p, out);
                out.push(':');
                ok &= put_value(&self.sources, v, out);
            }
            out.push('}');
        }
        out.push('}');
        self.sets.clear();
        self.deletes.clear();
        ok
    }

    pub fn output_from(&mut self, label: &str, out: &mut String) {
        out.push_str(",\"output\":{\"$from\":\"");
        out.push_str(label);
        out.push_str("\"}");
    }

    pub fn output(&mut self, value: &Variable, out: &mut String) -> bool {
        out.push_str(",\"output\":");
        put_value(&self.sources, value, out)
    }

    pub fn sources(&mut self, request: &Variable, parents: impl Iterator<Item = (Slot, Variable)>) {
        self.sources.clear();
        self.sources.push(("request", request.clone()));
        for (slot, value) in parents {
            let Some(node) = self.template.node(slot) else {
                continue;
            };
            self.sources.push((node.source.as_str(), value));
        }
    }

    pub fn list(&mut self, items: &[Variable], out: &mut String) -> bool {
        if items.len() >= 4 {
            if let Some(ok) = pack_list(items, out) {
                return ok;
            }
        }
        out.push('[');
        let mut ok = true;
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            ok &= put_value(&self.sources, x, out);
        }
        out.push(']');
        ok
    }

    pub fn results<'v>(
        &mut self,
        slot: Slot,
        entries: impl Iterator<Item = (&'v str, Variable)>,
        out: &mut String,
    ) -> bool {
        out.push_str(",\"results\":{");
        let mut ok = true;
        for (i, (k, v)) in entries.enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_str(k, out);
            out.push(':');
            ok &= put_value(&self.sources, &v, out);
        }
        out.push('}');
        if let Some(at) = self
            .template
            .node(slot)
            .and_then(|n| n.results_at.as_deref())
        {
            out.push_str(at);
        }
        ok
    }

    pub fn items(&mut self, slot: Slot, out: &mut String) -> bool {
        match self.template.node(slot).and_then(|n| n.items.as_deref()) {
            Some(items) => {
                out.push_str(items);
                true
            }
            None => false,
        }
    }

    pub fn empty_loop(&mut self, out: &mut String) {
        out.push_str(",\"loop\":[]");
    }

    pub fn results_list(&mut self, slot: Slot, items: &[Variable], out: &mut String) -> bool {
        out.push_str(",\"results\":");
        let ok = self.list(items, out);
        if let Some(at) = self
            .template
            .node(slot)
            .and_then(|n| n.results_at.as_deref())
        {
            out.push_str(at);
        }
        ok
    }

    pub fn results_open(&mut self, out: &mut String) {
        out.push_str(",\"results\":[");
    }

    pub fn result_item<'v>(
        &mut self,
        first: bool,
        entries: impl Iterator<Item = (&'v str, Variable)>,
        out: &mut String,
    ) -> bool {
        if !first {
            out.push(',');
        }
        out.push('{');
        let mut ok = true;
        for (i, (k, v)) in entries.enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_str(k, out);
            out.push(':');
            ok &= write_clean(&v, out);
        }
        out.push('}');
        ok
    }

    pub fn results_close(&mut self, slot: Slot, out: &mut String) {
        out.push(']');
        if let Some(at) = self
            .template
            .node(slot)
            .and_then(|n| n.results_at.as_deref())
        {
            out.push_str(at);
        }
    }

    pub fn looped_rules(
        &mut self,
        slot: Slot,
        hits: impl Iterator<Item = (usize, u32)>,
        out: &mut String,
    ) -> bool {
        let Some(node) = self.template.node(slot).filter(|n| n.looped) else {
            return false;
        };
        let mut texts: Vec<String> = Vec::new();
        let mut keys: Vec<(u32, Option<usize>)> = Vec::new();
        let mut at: Vec<u32> = Vec::new();
        let mut next = 0usize;
        for (item, rule) in hits {
            let Some(entry) = node.rules.get(rule as usize) else {
                return false;
            };
            let marker = (item != next).then_some(item);
            next = item + 1;
            let key = (rule, marker);
            let k = match keys.iter().position(|x| *x == key) {
                Some(k) => k,
                None => {
                    let mut text = String::with_capacity(entry.head.len() + 16);
                    text.push_str(&entry.head);
                    if let Some(item) = marker {
                        text.push_str(",\"item\":");
                        push_digits(item, &mut text);
                    }
                    text.push('}');
                    texts.push(text);
                    keys.push(key);
                    keys.len() - 1
                }
            };
            at.push(k as u32);
        }
        out.push_str(",\"rules\":");
        if emit_packed(&at, &texts, out) {
            return true;
        }
        out.push('[');
        for (i, k) in at.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&texts[*k as usize]);
        }
        out.push(']');
        true
    }

    pub fn expected(
        &self,
        slot: Slot,
        before: Option<&Variable>,
        results: &[Vec<(&str, Variable)>],
        looped: bool,
    ) -> Option<Variable> {
        use super::native::{build_results, deep_merge, place_at, vpath};
        let node = self.template.node(slot)?;
        let built = |entries: &Vec<(&str, Variable)>| {
            let keyed: Vec<(super::value::Key, Variable)> = entries
                .iter()
                .map(|(k, v)| (std::rc::Rc::from(*k), v.clone()))
                .collect();
            build_results(&keyed)
        };
        let at = node
            .at
            .as_deref()
            .map(super::core::dotted)
            .unwrap_or_default();
        let place = |v: Variable| match at.is_empty() {
            true => v,
            false => place_at(&at, v),
        };
        let placed = match looped {
            false => place(built(results.first()?)),
            true => {
                let list: Vec<Variable> = match before {
                    None => results.iter().map(built).collect(),
                    Some(before) => {
                        let items_at = super::core::dotted(node.items_at.as_deref()?);
                        let items = vpath(before, &items_at)?;
                        let items = items.as_array()?;
                        let items = items.borrow();
                        results
                            .iter()
                            .enumerate()
                            .map(|(i, r)| match items.get(i) {
                                Some(item) => deep_merge(item, &built(r)),
                                None => built(r),
                            })
                            .collect()
                    }
                };
                place(Variable::from_array(list))
            }
        };
        Some(match before {
            Some(before) => deep_merge(before, &placed),
            None => placed,
        })
    }

    pub fn hits(
        &mut self,
        slot: Slot,
        hits: &[super::collect::Hit],
        single: bool,
        scope: &Variable,
        out: &mut String,
    ) -> bool {
        let Some(node) = self.template.node(slot).filter(|n| !n.scoped && !n.looped) else {
            return false;
        };
        out.push_str(match single {
            true => ",\"rule\":",
            false => ",\"rules\":[",
        });
        for (i, hit) in hits.iter().enumerate() {
            let Some(rule) = node.rules.get(hit.rule as usize) else {
                return false;
            };
            if i > 0 {
                out.push(',');
            }
            if !super::native::hit_text(rule, hit, Some(scope), None, out) {
                return false;
            }
        }
        if !single {
            out.push(']');
        }
        true
    }

    pub fn rule(&mut self, slot: Slot, rule: u32, out: &mut String) -> bool {
        let Some(rule) = self
            .template
            .node(slot)
            .filter(|n| !n.scoped)
            .and_then(|n| n.rules.get(rule as usize))
        else {
            return false;
        };
        out.push_str(",\"rule\":");
        out.push_str(&rule.head);
        out.push('}');
        true
    }

    pub fn branches(&mut self, slot: Slot, taken: &[u32], out: &mut String) -> bool {
        let Some(node) = self.template.node(slot) else {
            return false;
        };
        out.push_str(",\"branches\":[");
        for (i, at) in taken.iter().enumerate() {
            let Some(id) = node.statements.get(*at as usize) else {
                return false;
            };
            if i > 0 {
                out.push(',');
            }
            write_str(id, out);
        }
        out.push(']');
        true
    }
}

pub fn perf(ns: u64) -> String {
    match super::core::duration_perf(std::time::Duration::from_nanos(ns)) {
        Ok(n) => n.to_string(),
        Err(text) => {
            let mut out = String::with_capacity(text.len() + 2);
            write_str(&text, &mut out);
            out
        }
    }
}

pub fn same(a: &Variable, b: &Variable) -> bool {
    veq(a, b)
}

pub fn null_free(v: &Variable) -> bool {
    match v {
        Variable::Null => false,
        Variable::Array(a) => a.borrow().iter().all(null_free),
        Variable::Object(o) => o.borrow().iter().all(|(_, x)| null_free(x)),
        _ => true,
    }
}

pub fn reserved(v: &Variable) -> bool {
    let mut clean: SmallVec<[*const (); 8]> = SmallVec::new();
    reserved_in(v, &mut clean)
}

fn reserved_in(v: &Variable, clean: &mut SmallVec<[*const (); 8]>) -> bool {
    match v {
        Variable::Array(a) => a.borrow().iter().any(|x| {
            matches!(x, Variable::Array(_) | Variable::Object(_)) && reserved_in(x, clean)
        }),
        Variable::Object(o) => {
            let o = o.borrow();
            match o.slots() {
                Some((shape, values)) => {
                    let at = shape as *const _ as *const ();
                    if !clean.contains(&at) {
                        if shape.keys().iter().any(|k| &**k == "$nodes") {
                            return true;
                        }
                        if clean.len() < 64 {
                            clean.push(at);
                        }
                    }
                    values.iter().any(|x| {
                        matches!(x, Variable::Array(_) | Variable::Object(_))
                            && reserved_in(x, clean)
                    })
                }
                None => o
                    .iter()
                    .any(|(k, x)| &**k == "$nodes" || reserved_in(x, clean)),
            }
        }
        _ => false,
    }
}

pub fn merge_exact(base: &Variable, patch: &Variable) -> bool {
    let (Variable::Object(a), Variable::Object(b)) = (base, patch) else {
        return true;
    };
    if std::rc::Rc::ptr_eq(a, b) {
        return true;
    }
    let (a, b) = (a.borrow(), b.borrow());
    b.iter().all(|(k, v)| match v {
        Variable::Null => false,
        Variable::Object(_) => a.get(k).is_none_or(|x| merge_exact(x, v)),
        _ => true,
    })
}

fn reference(sources: &[(&str, Variable)], v: &Variable, out: &mut String) -> bool {
    let same = |a: &Variable| match (a, v) {
        (Variable::Array(x), Variable::Array(y)) => std::rc::Rc::ptr_eq(x, y),
        (Variable::Object(x), Variable::Object(y)) => std::rc::Rc::ptr_eq(x, y),
        _ => false,
    };
    for (prefix, source) in sources {
        if same(source) {
            out.push_str("{\"$ref\":");
            write_str(prefix, out);
            out.push('}');
            return true;
        }
        let Variable::Object(o) = source else {
            continue;
        };
        for (k, child) in o.borrow().iter() {
            if same(child) {
                let mut text = prefix.to_string();
                escape_seg(k, &mut text);
                out.push_str("{\"$ref\":");
                write_str(&text, out);
                out.push('}');
                return true;
            }
        }
    }
    false
}

fn put_value(sources: &[(&str, Variable)], v: &Variable, out: &mut String) -> bool {
    let big = match v {
        Variable::Array(a) => a.borrow().len() >= 8,
        Variable::Object(o) => o.borrow().len() >= 8,
        _ => false,
    };
    if big && reference(sources, v, out) {
        return true;
    }
    match v {
        Variable::Array(a) => {
            let a = a.borrow();
            if a.len() >= 4 {
                if let Some(ok) = pack_list(&a, out) {
                    return ok;
                }
            }
            out.push('[');
            let mut ok = true;
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                ok &= put_value(sources, x, out);
            }
            out.push(']');
            ok
        }
        Variable::Object(o) => {
            let o = o.borrow();
            if o.iter()
                .any(|(k, _)| k.starts_with('$') && &**k != "$nodes")
            {
                drop(o);
                return write_clean(v, out);
            }
            out.push('{');
            let mut ok = true;
            let mut first = true;
            let mut entries: SmallVec<[(&str, &Variable); 8]> =
                o.iter().map(|(k, x)| (&**k, x)).collect();
            if o.shape().is_none() {
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            }
            for (k, x) in entries {
                if k == "$nodes" {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                write_str(k, out);
                out.push(':');
                ok &= put_value(sources, x, out);
            }
            out.push('}');
            ok
        }
        _ => write_clean(v, out),
    }
}

fn int_list(items: &[Variable]) -> Option<Vec<i64>> {
    items
        .iter()
        .map(|x| match x {
            Variable::Number(d) if d.scale() == 0 => i64::try_from(d.mantissa()).ok(),
            _ => None,
        })
        .collect()
}

fn hash_of(v: &Variable) -> u64 {
    fn mix(h: u64, x: u64) -> u64 {
        (h ^ x).wrapping_mul(0x100_0000_01b3)
    }
    match v {
        Variable::Null => 1,
        Variable::Bool(b) => 2 + *b as u64,
        Variable::Number(d) => mix(3, (d.mantissa() as u64) ^ ((d.scale() as u64) << 56)),
        Variable::String(s) => s.bytes().fold(4u64, |h, b| mix(h, b as u64)),
        Variable::Array(a) => a.borrow().iter().fold(5u64, |h, x| mix(h, hash_of(x))),
        Variable::Object(o) => {
            let o = o.borrow();
            let mut h = 6u64;
            for (k, x) in o.iter() {
                h ^= k
                    .bytes()
                    .fold(7u64, |h, b| mix(h, b as u64))
                    .wrapping_add(hash_of(x));
            }
            h
        }
        Variable::Dynamic(_) => 8,
    }
}

fn push_digits(n: usize, out: &mut String) {
    out.push_str(itoa::Buffer::new().format(n));
}

fn pack_list(items: &[Variable], out: &mut String) -> Option<bool> {
    use super::core::Packing;
    if let Some(ints) = int_list(items) {
        let packed = super::core::pack_ints(&ints)?;
        match packed {
            Packing::Runs(runs) => {
                out.push_str("{\"$runs\":[");
                for (i, (v, n, d)) in runs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    out.push_str(itoa::Buffer::new().format(*v));
                    if *n > 1 || d.is_some() {
                        out.push(',');
                        push_digits(*n, out);
                    }
                    if let Some(d) = d {
                        out.push(',');
                        out.push_str(itoa::Buffer::new().format(*d));
                    }
                    out.push(']');
                }
                out.push_str("]}");
            }
            Packing::Pick(values, at) => {
                out.push_str("{\"$pick\":[");
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(itoa::Buffer::new().format(*v));
                }
                out.push_str("],\"at\":[");
                for (i, k) in at.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    push_digits(*k as usize, out);
                }
                out.push_str("]}");
            }
        }
        return Some(true);
    }
    let n = items.len();
    let hashes: Vec<u64> = items.iter().map(hash_of).collect();
    let mut firsts: Vec<usize> = Vec::new();
    let mut seen: super::value::FastMap<u64, smallvec::SmallVec<[u32; 2]>> = Default::default();
    let mut at: Vec<u32> = Vec::with_capacity(n);
    for (i, item) in items.iter().enumerate() {
        let bucket = seen.entry(hashes[i]).or_default();
        let found = bucket.iter().copied().find(|&k| {
            veq(&items[firsts[k as usize]], item) && same_shape(&items[firsts[k as usize]], item)
        });
        let k = match found {
            Some(k) => k,
            None => {
                firsts.push(i);
                let k = (firsts.len() - 1) as u32;
                bucket.push(k);
                k
            }
        };
        at.push(k);
    }
    if firsts.len() * 2 > n && (1..n).all(|i| at[i] != at[i - 1]) {
        return None;
    }
    let mut texts: Vec<String> = Vec::with_capacity(firsts.len());
    for &i in &firsts {
        let mut text = String::new();
        if !write_clean(&items[i], &mut text) {
            return None;
        }
        texts.push(text);
    }
    emit_packed(&at, &texts, out).then_some(true)
}

fn emit_packed(at: &[u32], texts: &[String], out: &mut String) -> bool {
    let n = at.len();
    if n < 4 {
        return false;
    }
    let len = |i: usize| texts[at[i] as usize].len();
    let plain: usize = (0..n).map(|i| len(i) + 1).sum::<usize>() + 1;
    let mut runs: Vec<(u32, usize)> = Vec::new();
    for &k in at {
        match runs.last_mut() {
            Some((last, count)) if *last == k => *count += 1,
            _ => runs.push((k, 1)),
        }
    }
    let runs_size: usize = runs
        .iter()
        .map(|(k, count)| texts[*k as usize].len() + super::core::digits(*count) + 4)
        .sum::<usize>()
        + 12;
    let pick_size: usize = texts.iter().map(|t| t.len() + 1).sum::<usize>()
        + at.iter()
            .map(|&k| super::core::digits(k as usize) + 1)
            .sum::<usize>()
        + 16;
    let plain8 = plain as f64 * 0.8;
    if (runs_size as f64) < plain8 && runs_size <= pick_size {
        out.push_str("{\"$runs\":[");
        for (i, (k, count)) in runs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&texts[*k as usize]);
            if *count > 1 {
                out.push(',');
                push_digits(*count, out);
            }
            out.push(']');
        }
        out.push_str("]}");
        return true;
    }
    if (pick_size as f64) < plain8 {
        out.push_str("{\"$pick\":[");
        for (i, text) in texts.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(text);
        }
        out.push_str("],\"at\":[");
        for (i, k) in at.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_digits(*k as usize, out);
        }
        out.push_str("]}");
        return true;
    }
    false
}

fn same_shape(a: &Variable, b: &Variable) -> bool {
    match (a, b) {
        (Variable::Number(x), Variable::Number(y)) => {
            x.mantissa() == y.mantissa() && x.scale() == y.scale()
        }
        (Variable::Array(x), Variable::Array(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same_shape(p, q))
        }
        (Variable::Object(x), Variable::Object(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get_str(k).is_some_and(|w| same_shape(v, w)))
        }
        _ => true,
    }
}
