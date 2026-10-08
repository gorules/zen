use super::frame::Fault;
use super::frame::Frame;
use crate::lane::mask::{LaneSet, Lanes};
use crate::lane::output::{Output, Shape};
use crate::lane::program::{Kind, Layout, Program, Reg};
use crate::variable::Variable;
use crate::vm::VMError;
use std::rc::Rc;
use zen_types::symbol::Symbol;
use zen_types::variable::Shape as MapShape;

impl<M: LaneSet> Frame<M> {
    pub(super) fn collect_shapes(layout: &Layout, out: &mut Vec<Option<Rc<MapShape>>>) {
        match layout {
            Layout::Value(_) => {}
            Layout::Struct(fields) => {
                let keys: Vec<Symbol> = fields
                    .iter()
                    .rev()
                    .map(|(key, _)| Symbol::from(key.as_ref()))
                    .collect();
                let distinct = keys
                    .iter()
                    .enumerate()
                    .all(|(i, k)| keys[..i].iter().all(|x| x.as_str() != k.as_str()));
                out.push(
                    (distinct && keys.len() <= 32)
                        .then(|| MapShape::of(keys.iter()))
                        .flatten(),
                );
                fields
                    .iter()
                    .rev()
                    .for_each(|(_, field)| Self::collect_shapes(field, out));
            }
            Layout::List(items) => items
                .iter()
                .for_each(|item| Self::collect_shapes(item, out)),
        }
    }

    pub(super) fn assemble_taken(
        &mut self,
        layout: &Layout,
        lane: usize,
        mut next: Option<&mut usize>,
    ) -> Variable {
        match layout {
            Layout::Value(reg) => self.take(*reg, lane),
            Layout::Struct(fields) => {
                let shape = next.as_deref_mut().and_then(|n| {
                    *n += 1;
                    self.shapes.get(*n - 1).cloned().flatten()
                });
                if let Some(shape) = shape {
                    let values: Vec<Variable> = fields
                        .iter()
                        .rev()
                        .map(|(_, f)| self.assemble_taken(f, lane, next.as_deref_mut()))
                        .collect();
                    return Variable::from_object(crate::variable::VariableMap::from_shape(
                        shape, values,
                    ));
                }
                let mut map = crate::variable::VariableMap::with_capacity(fields.len());
                for (key, field) in fields.iter().rev() {
                    let value = self.assemble_taken(field, lane, next.as_deref_mut());
                    map.insert(Symbol::from(key.as_ref()), value);
                }
                Variable::from_object(map)
            }
            Layout::List(items) => {
                let values = items
                    .iter()
                    .map(|i| self.assemble_taken(i, lane, next.as_deref_mut()))
                    .collect();
                Variable::from_array(values)
            }
        }
    }

    pub(super) fn cell(
        &mut self,
        layout: &Layout,
        lane: usize,
        out: &mut Output,
        at: usize,
        failed: bool,
    ) {
        if failed {
            out.fail_at(at);
        }
        match (layout, &mut out.shape) {
            (Layout::Struct(fields), Shape::Struct(outputs)) => {
                for ((_, field), (_, output)) in fields.iter().zip(outputs.iter_mut()) {
                    self.cell(field, lane, output, at, failed);
                }
            }
            (Layout::List(items), Shape::List(child)) => {
                let width = items.len();
                for (i, item) in items.iter().enumerate() {
                    self.cell(item, lane, child, at * width + i, failed);
                }
            }
            _ if failed => {}
            (Layout::Value(reg), Shape::Scalar) => self.scalar(*reg, lane, out, at),
            (layout, _) => {
                let value = self.assemble_taken(layout, lane, None);
                match out.kind() {
                    Kind::Dyn => out.values[at] = value,
                    _ => out.box_at(at, value),
                }
            }
        }
    }

    pub(super) fn scalar(&mut self, reg: Reg, lane: usize, out: &mut Output, at: usize) {
        let kind = out.kind();
        if kind == Kind::Dyn {
            out.values[at] = self.take(reg, lane);
            return;
        }
        let wide = kind == Kind::Num && self.wide[reg as usize].get(lane);
        if self.kind(reg) != kind || self.boxed[reg as usize].get(lane) || wide {
            let value = self.take(reg, lane);
            out.box_at(at, value);
            return;
        }
        let i = self.at(reg, lane);
        match kind {
            Kind::Num => {
                out.mant[at] = self.mant[i];
                out.scale[at] = self.scales[i];
            }
            Kind::Bool => out.set_bit(at, self.bits[reg as usize].get(lane)),
            Kind::Date => out.dates[at] = self.dates[i].0,
            Kind::List => self.push_list(i, out),
            _ => {
                let (a, b) = self.spans[i];
                out.push_text(self.arena.get(a as usize..b as usize).unwrap_or_default());
            }
        }
    }

    pub(super) fn bulk_lists(&self, base: usize, n: usize, out: &mut Output) -> bool {
        let Some(spans) = self.lists.get(base..base + n) else {
            return false;
        };
        let Some((a, b)) = Self::block(spans) else {
            return false;
        };
        let Some((mant, scales)) = self.items.numbers(a, b) else {
            return false;
        };
        out.extend_lists(mant, scales, spans.iter().map(|(_, end)| end - a as u32))
    }

    pub(super) fn push_list(&self, at: usize, out: &mut Output) {
        let (a, b) = self.lists[at];
        if let Shape::List(child) = &mut out.shape {
            match self.items.numbers(a as usize, b as usize) {
                Some((mant, scales)) => child.extend_nums(mant, scales),
                None => {
                    for i in a as usize..b as usize {
                        child.push_item(&self.items.get(i), &self.arena);
                    }
                }
            }
        }
        out.close();
    }

    pub fn export(&mut self, program: &Program, out: &mut Output, start: usize, n: usize) {
        let failed = !self.alive & M::all(n);
        match &program.layout {
            Some(layout) => self.export_shape(layout, out, start, n, failed),
            None => self.export_value(program.out, out, start, n, failed),
        }
        for lane in Lanes::of(failed) {
            let fault = self.errors[lane]
                .take()
                .unwrap_or(Fault::Vm(VMError::NumberConversionError));
            out.errors.push(((start + lane) as u32, crate::lane::output::Failure::new(fault)));
        }
    }

    pub fn export_outputs(
        &mut self,
        program: &Program,
        outs: &mut [Output],
        start: usize,
        n: usize,
        mut failure: impl FnMut(usize, usize, Fault),
    ) {
        if program.isolated {
            self.revive();
        }
        let failed = !self.alive & M::all(n);
        match (program.outputs.is_empty(), &program.layout, outs.first_mut()) {
            (true, Some(layout), Some(out)) => self.export_shape(layout, out, start, n, failed),
            (true, None, Some(out)) => self.export_value(program.out, out, start, n, failed),
            _ => {
                for (reg, out) in program.outputs.iter().zip(outs.iter_mut()) {
                    self.export_value(*reg, out, start, n, failed);
                }
            }
        }
        for (lane, stage, fault) in self.soft.drain(..) {
            failure(start + lane, stage as usize, fault);
        }
        for lane in Lanes::of(failed) {
            let fault = self.errors[lane]
                .take()
                .unwrap_or(Fault::Vm(VMError::NumberConversionError));
            failure(start + lane, self.stages[lane] as usize, fault);
        }
    }

    pub(super) fn export_shape(
        &mut self,
        layout: &Layout,
        out: &mut Output,
        start: usize,
        n: usize,
        failed: M,
    ) {
        if let Layout::Value(reg) = layout {
            if out.kind() == self.kinds[*reg as usize] && !matches!(out.shape, Shape::Struct(_)) {
                return self.export_value(*reg, out, start, n, failed);
            }
        }
        match (layout, &mut out.shape) {
            (Layout::Struct(fields), Shape::Struct(outputs)) => {
                for ((_, field), (_, output)) in fields.iter().zip(outputs.iter_mut()) {
                    self.export_shape(field, output, start, n, failed);
                }
                (failed).store(&mut out.failed, start / 64);
            }
            (Layout::List(items), Shape::List(child))
                if failed.is_empty() && self.strided(items, child.kind(), n) =>
            {
                let width = items.len();
                for (i, item) in items.iter().enumerate() {
                    let Layout::Value(reg) = item else {
                        continue;
                    };
                    let base = *reg as usize * self.width;
                    for lane in 0..n {
                        let at = (start + lane) * width + i;
                        match child.kind() {
                            Kind::Num => {
                                child.mant[at] = self.mant[base + lane];
                                child.scale[at] = self.scales[base + lane];
                            }
                            _ => child.set_bit(at, self.bits[*reg as usize].get(lane)),
                        }
                    }
                }
            }
            _ => {
                for lane in 0..n {
                    self.cell(layout, lane, out, start + lane, failed.get(lane));
                }
            }
        }
    }

    pub(super) fn strided(&self, items: &[Layout], kind: Kind, n: usize) -> bool {
        let lanes = M::all(n);
        matches!(kind, Kind::Num | Kind::Bool)
            && items.iter().all(|item| match item {
                Layout::Value(reg) => {
                    let r = *reg as usize;
                    self.kinds[r] == kind && ((self.boxed[r] | self.wide[r]) & lanes).is_empty()
                }
                _ => false,
            })
    }

    pub(super) fn export_value(
        &mut self,
        reg: Reg,
        out: &mut Output,
        start: usize,
        n: usize,
        failed: M,
    ) {
        let r = reg as usize;
        let word = start / 64;
        let lanes = M::all(n);
        let base = r * self.width;
        let boxed = match self.kinds[r] {
            Kind::Num => {
                out.mant[start..start + n].copy_from_slice(&self.mant[base..base + n]);
                out.scale[start..start + n].copy_from_slice(&self.scales[base..base + n]);
                (self.boxed[r] | self.wide[r]) & lanes & !failed
            }
            Kind::Bool => {
                (self.bits[r] & lanes & !failed).store(&mut out.bits, word);
                self.boxed[r] & lanes & !failed
            }
            Kind::Dyn => {
                for lane in Lanes::of(lanes & !failed) {
                    out.values[start + lane] = self.take(reg, lane);
                }
                self.none()
            }
            Kind::List => {
                let boxed = self.boxed[r] & lanes & !failed;
                let plain = lanes & !failed & !boxed;
                if plain != lanes || !self.bulk_lists(base, n, out) {
                    for lane in 0..n {
                        match plain.get(lane) {
                            true => self.push_list(base + lane, out),
                            _ => out.close(),
                        }
                    }
                }
                boxed
            }
            Kind::Date => {
                for (o, d) in out.dates[start..start + n]
                    .iter_mut()
                    .zip(&self.dates[base..base + n])
                {
                    *o = d.0;
                }
                self.boxed[r] & lanes & !failed
            }
            Kind::Str if out.coding() => {
                let boxed = self.boxed[r] & lanes & !failed;
                let plain = lanes & !failed & !boxed;
                let mut seen: Vec<((u32, u32), i32)> = Vec::new();
                for lane in 0..n {
                    if !plain.get(lane) {
                        out.push_empty();
                        continue;
                    }
                    let span = self.spans[base + lane];
                    match (out.coding(), seen.iter().find(|(s, _)| *s == span)) {
                        (true, Some((_, code))) => out.push_code(*code),
                        _ => {
                            out.push_text(self.arena.get(span.0 as usize..span.1 as usize).unwrap_or_default());
                            if let Some(code) = out.last_code() {
                                seen.push((span, code));
                            }
                        }
                    }
                }
                boxed
            }
            Kind::Str => {
                let boxed = self.boxed[r] & lanes & !failed;
                let spans = &self.spans[base..base + n];
                let clean = failed.is_empty() && boxed.is_empty();
                if let (true, Some(first)) = (clean, spans.first().copied()) {
                    if spans.iter().all(|span| *span == first) {
                        let text = self
                            .arena
                            .get(first.0 as usize..first.1 as usize)
                            .unwrap_or_default()
                            .as_bytes();
                        let origin = out.data.len();
                        out.data.extend_from_slice(text);
                        while out.data.len() - origin < text.len() * n {
                            let have = out.data.len() - origin;
                            let take = have.min(text.len() * n - have);
                            out.data.extend_from_within(origin..origin + take);
                        }
                        out.offsets
                            .extend((1..=n).map(|i| (origin + i * text.len()) as u32));
                        return self.export_rest(out, start, failed, self.none(), reg);
                    }
                }
                if let (true, Some((a, b))) = (clean, Self::block(spans)) {
                    if let Some(text) = self.arena.get(a..b) {
                        let shift = out.data.len() as i64 - a as i64;
                        out.data.extend_from_slice(text.as_bytes());
                        out.offsets
                            .extend(spans.iter().map(|(_, end)| (*end as i64 + shift) as u32));
                        return self.export_rest(out, start, failed, self.none(), reg);
                    }
                }
                let plain = lanes & !failed & !boxed;
                for lane in 0..n {
                    if plain.get(lane) {
                        let (a, b) = self.spans[base + lane];
                        if let Some(text) = self.arena.get(a as usize..b as usize) {
                            out.data.extend_from_slice(text.as_bytes());
                        }
                    }
                    out.offsets.push(out.data.len() as u32);
                }
                boxed
            }
        };
        self.export_rest(out, start, failed, boxed, reg)
    }

    pub(super) fn export_rest(
        &mut self,
        out: &mut Output,
        start: usize,
        failed: M,
        boxed: M,
        reg: Reg,
    ) {
        for lane in Lanes::of(boxed) {
            let v = self.take(reg, lane);
            out.extra.push(((start + lane) as u32, v));
        }
        (boxed).store(&mut out.boxed, start / 64);
        (failed).store(&mut out.failed, start / 64);
    }

    pub fn take_result(&mut self, program: &Program, lane: usize) -> Result<Variable, VMError> {
        match self.alive.get(lane) {
            true => Ok(match &program.layout {
                Some(layout) => self.assemble_taken(layout, lane, Some(&mut 0)),
                None => self.take(program.out, lane),
            }),
            _ => Err(self.errors[lane]
                .take().map(Fault::vm)
                .unwrap_or(VMError::NumberConversionError)),
        }
    }
}
