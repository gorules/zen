use crate::functions::sketch::{canonical, Digest, Hll};
use crate::functions::stats;
use crate::vm::date::DynamicVariableExt;
use crate::vm::VmDate;
use crate::Variable as V;
use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use anyhow::{anyhow, bail, Context};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::str::FromStr;

pub(crate) fn present(items: &[V]) -> impl Iterator<Item = &V> {
    items.iter().filter(|v| !matches!(v, V::Null))
}

pub(crate) fn numbers(items: &[V]) -> anyhow::Result<Vec<Decimal>> {
    present(items)
        .map(|v| v.as_number().context("Expected a number array"))
        .collect()
}

fn date_of(value: &V) -> Option<&VmDate> {
    match value {
        V::Dynamic(d) => d.as_date(),
        _ => None,
    }
}

pub(crate) fn count(value: &V, pos: usize) -> anyhow::Result<usize> {
    let n = value
        .as_number()
        .with_context(|| format!("Argument on {pos} is not a number"))?;
    if n.is_sign_negative() || !n.fract().is_zero() {
        bail!("Expected a whole number of items, got {n}");
    }
    n.to_usize().context("Number of items is too large")
}

pub(crate) fn sum(items: &[V]) -> anyhow::Result<V> {
    let total = numbers(items)?
        .into_iter()
        .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(v))
        .context("Number overflow")?;
    Ok(V::Number(total))
}

pub(crate) fn avg(items: &[V]) -> anyhow::Result<V> {
    let values = numbers(items)?;
    if values.is_empty() {
        return Ok(V::Null);
    }
    let total = values
        .iter()
        .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v))
        .context("Number overflow")?;
    let mean = total
        .checked_div(Decimal::from(values.len()))
        .context("Number overflow")?;
    Ok(V::Number(mean))
}

pub(crate) fn extreme(items: &[V], max: bool) -> anyhow::Result<V> {
    let values: Vec<&V> = present(items).collect();
    let Some(first) = values.first() else {
        return Ok(V::Null);
    };
    let pick = |ordering: Ordering| match max {
        true => ordering == Ordering::Greater,
        false => ordering == Ordering::Less,
    };
    if first.as_number().is_some() {
        let mut best: Option<Decimal> = None;
        for value in &values {
            let n = value.as_number().context("Expected a number array")?;
            if best.map_or(true, |b| pick(n.cmp(&b))) {
                best = Some(n);
            }
        }
        return Ok(best.map_or(V::Null, V::Number));
    }
    let mut best: Option<&VmDate> = None;
    for value in &values {
        let date = date_of(value).context("Expected a number or date array")?;
        if best.map_or(true, |b| pick(date.cmp(b))) {
            best = Some(date);
        }
    }
    Ok(best.map_or(V::Null, |d| V::Dynamic(Rc::new(d.clone()))))
}

pub(crate) fn median(items: &[V]) -> anyhow::Result<V> {
    let mut values = numbers(items)?;
    if values.is_empty() {
        return Ok(V::Null);
    }
    values.sort();
    let center = values.len() / 2;
    if values.len() % 2 == 1 {
        return Ok(V::Number(values[center]));
    }
    let value = values[center - 1]
        .checked_add(values[center])
        .context("Number overflow")?
        / dec!(2);
    Ok(V::Number(value))
}

pub(crate) fn mode(items: &[V]) -> anyhow::Result<V> {
    let values: Vec<&V> = present(items).collect();
    if values.is_empty() {
        return Ok(V::Null);
    }
    if values.iter().all(|v| matches!(v, V::Number(_))) {
        let mut counts = BTreeMap::new();
        for value in &values {
            if let V::Number(n) = value {
                *counts.entry(*n).or_insert(0usize) += 1;
            }
        }
        return Ok(counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map_or(V::Null, |(n, _)| V::Number(n)));
    }
    let mut counts = BTreeMap::new();
    for value in &values {
        let V::String(s) = value else {
            bail!("Expected an array of numbers or strings");
        };
        *counts.entry(s.clone()).or_insert(0usize) += 1;
    }
    Ok(counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map_or(V::Null, |(s, _)| V::String(s)))
}

fn number_or_null(value: Option<Decimal>) -> V {
    value.map_or(V::Null, V::Number)
}

pub(crate) fn variance(items: &[V], sample: bool) -> anyhow::Result<V> {
    Ok(number_or_null(stats::variance(&numbers(items)?, sample)?))
}

pub(crate) fn stddev(items: &[V], sample: bool) -> anyhow::Result<V> {
    Ok(number_or_null(stats::stddev(&numbers(items)?, sample)?))
}

pub(crate) fn skew(items: &[V]) -> anyhow::Result<V> {
    Ok(number_or_null(stats::skew(&numbers(items)?)?))
}

pub(crate) fn kurtosis(items: &[V]) -> anyhow::Result<V> {
    Ok(number_or_null(stats::kurtosis(&numbers(items)?)?))
}

pub(crate) fn percentile(items: &[V], q: Decimal) -> anyhow::Result<V> {
    let mut values = numbers(items)?;
    values.sort();
    Ok(number_or_null(stats::percentile(&values, q)?))
}

fn decimal_of(value: f64) -> Option<Decimal> {
    Decimal::from_str(&format!("{value}"))
        .or_else(|_| Decimal::from_scientific(&format!("{value:e}")))
        .ok()
        .or_else(|| Decimal::from_f64(value))
        .map(|d| d.normalize())
}

pub(crate) fn percentile_approx(items: &[V], q: Decimal) -> anyhow::Result<V> {
    if !(Decimal::ZERO..=Decimal::ONE).contains(&q) {
        bail!("Percentile must be between 0 and 1, got {q}");
    }
    let values = numbers(items)?;
    let digest = Digest::from_values(
        values
            .iter()
            .filter_map(|v| v.to_f64())
            .filter(|v| v.is_finite()),
    );
    let q = q.to_f64().context("Invalid percentile")?;
    Ok(digest
        .and_then(|d| d.quantile(q))
        .and_then(decimal_of)
        .map_or(V::Null, V::Number))
}

pub(crate) fn count_distinct(items: &[V]) -> V {
    let keys: HashSet<String> = present(items).map(canonical).collect();
    V::Number(keys.len().into())
}

pub(crate) fn count_distinct_approx(items: &[V]) -> V {
    let mut hll = Hll::new();
    present(items).for_each(|v| hll.insert(v));
    V::Number(hll.estimate().into())
}

fn settled(value: &V) -> V {
    match value {
        V::Number(n) => V::Number(n.normalize()),
        other => other.clone(),
    }
}

pub(crate) fn distinct(items: &[V]) -> V {
    let mut seen = HashSet::new();
    let out = present(items)
        .filter(|v| seen.insert(canonical(v)))
        .cloned()
        .collect();
    V::from_array(out)
}

pub(crate) fn top_k(items: &[V], k: usize) -> V {
    let mut counts: HashMap<String, (usize, &V)> = HashMap::new();
    for value in present(items) {
        counts.entry(canonical(value)).or_insert((0, value)).0 += 1;
    }
    let mut ranked: Vec<(String, (usize, &V))> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
    V::from_array(
        ranked
            .into_iter()
            .take(k)
            .map(|(_, (_, v))| settled(v))
            .collect(),
    )
}

pub(crate) fn first(items: &[V]) -> V {
    present(items).next().cloned().unwrap_or(V::Null)
}

pub(crate) fn last(items: &[V]) -> V {
    items
        .iter()
        .rev()
        .find(|v| !matches!(v, V::Null))
        .cloned()
        .unwrap_or(V::Null)
}

enum Rank<'a> {
    Number(Decimal),
    Date(&'a VmDate),
    Text(String),
}

impl<'a> Rank<'a> {
    fn of(value: &'a V) -> Self {
        match value {
            V::Number(n) => Rank::Number(*n),
            other => match date_of(other) {
                Some(date) => Rank::Date(date),
                None => Rank::Text(canonical(other)),
            },
        }
    }

    fn order(&self) -> u8 {
        match self {
            Rank::Number(_) => 0,
            Rank::Date(_) => 1,
            Rank::Text(_) => 2,
        }
    }

    fn cmp(&self, other: &Rank) -> Ordering {
        match (self, other) {
            (Rank::Number(a), Rank::Number(b)) => a.cmp(b),
            (Rank::Date(a), Rank::Date(b)) => a.cmp(b),
            (Rank::Text(a), Rank::Text(b)) => a.cmp(b),
            _ => self.order().cmp(&other.order()),
        }
    }
}

pub(crate) fn pairs(items: &[V]) -> anyhow::Result<Vec<(V, V)>> {
    items
        .iter()
        .map(|item| match item {
            V::Array(pair) => match pair.borrow().as_slice() {
                [value, key] => Ok((value.clone(), key.clone())),
                _ => Err(anyhow!("Expected [value, key] pairs")),
            },
            _ => Err(anyhow!("Expected [value, key] pairs")),
        })
        .collect()
}

pub(crate) fn arg_extreme(items: &[V], max: bool) -> anyhow::Result<V> {
    let pairs = pairs(items)?;
    let mut best: Option<(&V, Rank, String)> = None;
    for (value, by) in pairs.iter() {
        if matches!(value, V::Null) || matches!(by, V::Null) {
            continue;
        }
        let rank = Rank::of(by);
        let key = canonical(value);
        let better = match &best {
            None => true,
            Some((_, best_rank, best_key)) => {
                let ordering = rank.cmp(best_rank).then_with(|| key.cmp(best_key));
                match max {
                    true => ordering == Ordering::Greater,
                    false => ordering == Ordering::Less,
                }
            }
        };
        if better {
            best = Some((value, rank, key));
        }
    }
    Ok(best.map_or(V::Null, |(value, _, _)| value.clone()))
}
