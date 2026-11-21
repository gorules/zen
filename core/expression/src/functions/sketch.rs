use crate::Variable;
use std::cmp::Ordering;

pub const HLL_PRECISION: u32 = 12;
pub const HLL_REGISTERS: usize = 1 << HLL_PRECISION;

pub const DIGEST_COMPRESSION: f64 = 100.0;
const DIGEST_KEEP: usize = 5 * DIGEST_COMPRESSION as usize;

pub fn canonical(value: &Variable) -> String {
    match value {
        Variable::Number(n) => n.normalize().to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

pub fn hash_of(value: &Variable) -> u64 {
    xxhash_rust::xxh64::xxh64(canonical(value).as_bytes(), 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hll {
    registers: Vec<u8>,
}

impl Default for Hll {
    fn default() -> Self {
        Self {
            registers: vec![0u8; HLL_REGISTERS],
        }
    }
}

impl Hll {
    pub fn new() -> Self {
        Self::default()
    }

    fn split(hash: u64) -> (usize, u8) {
        let index = (hash >> (64 - HLL_PRECISION)) as usize;
        let rest = (hash << HLL_PRECISION) | (1 << (HLL_PRECISION - 1));
        (index, rest.leading_zeros() as u8 + 1)
    }

    pub fn insert_hash(&mut self, hash: u64) {
        let (index, rank) = Self::split(hash);
        let slot = &mut self.registers[index];
        *slot = (*slot).max(rank);
    }

    pub fn insert(&mut self, value: &Variable) {
        self.insert_hash(hash_of(value));
    }

    pub fn estimate(&self) -> u64 {
        let m = HLL_REGISTERS as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let sum: f64 = self
            .registers
            .iter()
            .map(|r| 2f64.powi(-(*r as i32)))
            .sum();
        let zeros = self.registers.iter().filter(|r| **r == 0).count();
        let raw = alpha * m * m / sum;
        let estimate = if raw <= 2.5 * m && zeros > 0 {
            m * (m / zeros as f64).ln()
        } else {
            raw
        };
        estimate.round() as u64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Digest {
    pub count: u64,
    pub min: f64,
    pub max: f64,
    pub centroids: Vec<(f64, u64)>,
}

impl Digest {
    pub fn of(value: f64) -> Digest {
        Digest {
            count: 1,
            min: value,
            max: value,
            centroids: vec![(value, 1)],
        }
    }

    pub fn from_values(values: impl IntoIterator<Item = f64>) -> Option<Digest> {
        let mut digest: Option<Digest> = None;
        for value in values {
            match digest.as_mut() {
                None => digest = Some(Digest::of(value)),
                Some(digest) => digest.insert(value),
            }
        }
        digest
    }

    pub fn insert(&mut self, value: f64) {
        self.count = self.count.saturating_add(1);
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.centroids.push((value, 1));
        if self.centroids.len() > DIGEST_KEEP {
            *self = self.compressed();
        }
    }

    fn sorted(centroids: &mut [(f64, u64)]) {
        centroids.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
    }

    fn scale(q: f64) -> f64 {
        DIGEST_COMPRESSION / (2.0 * std::f64::consts::PI) * (2.0 * q.clamp(0.0, 1.0) - 1.0).asin()
    }

    pub fn compressed(&self) -> Digest {
        let mut sorted = self.centroids.clone();
        Self::sorted(&mut sorted);
        let total = self.count as f64;
        let mut out: Vec<(f64, u64)> = Vec::new();
        let mut seen = 0f64;
        let mut limit = Self::scale(0.0) + 1.0;
        for (mean, weight) in &sorted {
            let q = (seen + *weight as f64) / total;
            match out.last_mut() {
                Some(last) if Self::scale(q) <= limit => {
                    let merged = last.1 + weight;
                    last.0 += (mean - last.0) * (*weight as f64 / merged as f64);
                    last.1 = merged;
                }
                _ => {
                    limit = Self::scale(seen / total) + 1.0;
                    out.push((*mean, *weight));
                }
            }
            seen += *weight as f64;
        }
        Digest {
            centroids: out,
            ..self.clone()
        }
    }

    pub fn quantile(&self, q: f64) -> Option<f64> {
        if self.count == 0 || self.centroids.is_empty() {
            return None;
        }
        let digest = self.compressed();
        let centroids = &digest.centroids;
        if centroids.len() == 1 {
            return Some(centroids[0].0);
        }
        let total = self.count as f64;
        let target = q.clamp(0.0, 1.0) * total;
        let mut seen = 0f64;
        let mut previous: Option<(f64, f64)> = None;
        for (mean, weight) in centroids {
            let middle = seen + *weight as f64 / 2.0;
            if target < middle {
                return Some(match previous {
                    None => {
                        let span = middle.max(f64::EPSILON);
                        self.min + (mean - self.min) * (target / span)
                    }
                    Some((before_mean, before_middle)) => {
                        let span = (middle - before_middle).max(f64::EPSILON);
                        before_mean + (mean - before_mean) * ((target - before_middle) / span)
                    }
                });
            }
            previous = Some((*mean, middle));
            seen += *weight as f64;
        }
        let (last_mean, last_middle) = previous?;
        let span = (total - last_middle).max(f64::EPSILON);
        Some(last_mean + (self.max - last_mean) * ((target - last_middle) / span).min(1.0))
    }
}
