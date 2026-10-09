//! Mergeable approximate summaries, computed in exact decimals (no floats)
//! so every implementation that follows these rules gets the same digits:
//!
//! - [`Hll`]: HyperLogLog distinct counting (`countDistinctApprox`),
//! - [`DdSketch`]: relative-accuracy quantiles (`percentileApprox`),
//! - [`hash_value`]: the stable value hash both [`Hll`] and hosts use.

use crate::vm::date::DynamicVariableExt;
use crate::Variable;
use chrono::{SecondsFormat, Utc};
use once_cell::sync::Lazy;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, MathematicalOps, RoundingStrategy};
use std::collections::BTreeMap;
use xxhash_rust::xxh3::xxh3_64;

/// Type tag of a number in [`canonical_bytes`].
pub const TAG_NUMBER: u8 = 0x01;
/// Type tag of a string in [`canonical_bytes`].
pub const TAG_STRING: u8 = 0x02;
/// Type tag of a bool in [`canonical_bytes`].
pub const TAG_BOOL: u8 = 0x03;
/// Type tag of a date in [`canonical_bytes`].
pub const TAG_DATE: u8 = 0x04;

/// The canonical bytes of a scalar value: one type-tag byte, then the
/// payload, with nothing in between and no terminator.
///
/// | value  | tag    | payload                                                       |
/// |--------|--------|---------------------------------------------------------------|
/// | number | `0x01` | ASCII of the normalized decimal (see below)                   |
/// | string | `0x02` | the string's UTF-8 bytes, as they are (no normalization)      |
/// | bool   | `0x03` | ASCII `true` or `false`                                       |
/// | date   | `0x04` | ASCII RFC 3339 in UTC with milliseconds (see below)           |
///
/// - **Number**: `rust_decimal`'s `Decimal::normalize()` (trailing fractional
///   zeros stripped, `-0` becomes `0`) printed with `Display`: plain
///   notation, never an exponent, `-` for negatives, no `+`, no leading
///   zeros beyond a single `0` before the point. So `1`, `1.0` and `1.000`
///   are all `1`; `-0.50` is `-0.5`; `0.0001` is `0.0001`; `1e3` is `1000`.
/// - **Date**: the instant converted to UTC and printed as
///   `YYYY-MM-DDTHH:MM:SS.mmmZ` (chrono's `to_rfc3339_opts(Millis, true)`):
///   always exactly three fractional digits, finer precision truncated
///   (not rounded), `Z` for the offset. A date written with any offset or
///   time zone hashes as its UTC instant. An invalid date has no bytes.
/// - **Record** (an object, as a reference reads: `t.merchant`): the bytes
///   of its `id` field, so a record hashes exactly as its id (the key a
///   feature store aggregates). No `id`, or an object `id`: no bytes.
/// - **Null, arrays** and other dynamic values have no canonical bytes
///   (`None`): aggregates skip them.
///
/// The tags keep types apart: the string `"1"`, the number `1` and the
/// bool `true` never collide, nor does a date with its own text.
pub fn canonical_bytes(value: &Variable) -> Option<Vec<u8>> {
    let (tag, payload) = match value {
        Variable::Number(n) => (TAG_NUMBER, n.normalize().to_string()),
        Variable::String(s) => (TAG_STRING, s.as_str().to_string()),
        Variable::Bool(b) => (TAG_BOOL, b.to_string()),
        Variable::Dynamic(dynamic) => {
            let date_time = dynamic.as_date()?.0?;
            let text = date_time
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true);
            (TAG_DATE, text)
        }
        // A record (what a reference reads, `t.merchant`) is its `id`: the
        // key a feature store aggregates.
        Variable::Object(object) => {
            let id = object.borrow().get_str("id").cloned()?;
            return match id {
                Variable::Object(_) => None,
                id => canonical_bytes(&id),
            };
        }
        Variable::Null | Variable::Array(_) => return None,
    };
    let mut bytes = Vec::with_capacity(1 + payload.len());
    bytes.push(tag);
    bytes.extend_from_slice(payload.as_bytes());
    Some(bytes)
}

/// A stable 64-bit hash of a scalar: XXH3-64 (seed 0) of its
/// [`canonical_bytes`]. `None` for null, arrays, records without an `id`
/// and invalid dates.
pub fn hash_value(value: &Variable) -> Option<u64> {
    canonical_bytes(value).map(|bytes| xxh3_64(&bytes))
}

/// HyperLogLog precision: the top 12 bits of a hash pick the register.
pub const HLL_PRECISION: u32 = 12;
/// Number of HyperLogLog registers, `2^12`.
pub const HLL_REGISTERS: u32 = 1 << HLL_PRECISION;
/// Largest register value: the 52 low bits all zero.
pub const HLL_MAX_RANK: u8 = (64 - HLL_PRECISION + 1) as u8;

/// HyperLogLog with `p = 12` (`m = 4096` registers), kept sparse: only the
/// registers that saw a value, `index → rank`.
///
/// A hash `h` goes to register `h >> 52`; its rank is the number of
/// leading zeros of the remaining 52 bits (as a 52-bit field) plus one, so
/// 1..=53. A register keeps the largest rank it saw. Merging takes the
/// larger of each register, so merged sketches equal one fed every value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hll {
    registers: BTreeMap<u16, u8>,
}

impl Hll {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores a sketch from its registers (as [`Hll::registers`] gives
    /// them). Rank-0 entries are dropped (an absent register is 0). `None`
    /// for an index outside `0..4096` or a rank above 53.
    pub fn from_registers(registers: BTreeMap<u16, u8>) -> Option<Self> {
        if registers
            .iter()
            .any(|(&index, &rank)| u32::from(index) >= HLL_REGISTERS || rank > HLL_MAX_RANK)
        {
            return None;
        }
        Some(Self {
            registers: registers
                .into_iter()
                .filter(|&(_, rank)| rank > 0)
                .collect(),
        })
    }

    /// The registers that saw a value, `index → rank` (rank ≥ 1).
    pub fn registers(&self) -> &BTreeMap<u16, u8> {
        &self.registers
    }

    pub fn insert_hash(&mut self, hash: u64) {
        let index = (hash >> (64 - HLL_PRECISION)) as u16;
        let rest = hash & ((1u64 << (64 - HLL_PRECISION)) - 1);
        // The 52-bit field's leading zeros, plus one: 53 when all are zero.
        let rank = (rest.leading_zeros() - HLL_PRECISION + 1) as u8;
        let register = self.registers.entry(index).or_insert(0);
        *register = (*register).max(rank);
    }

    /// Adds a value by its [`hash_value`]; `false` (and no change) for the
    /// values without one (null, arrays, objects).
    pub fn insert(&mut self, value: &Variable) -> bool {
        match hash_value(value) {
            Some(hash) => {
                self.insert_hash(hash);
                true
            }
            None => false,
        }
    }

    pub fn merge(&mut self, other: &Hll) {
        for (&index, &rank) in &other.registers {
            let register = self.registers.entry(index).or_insert(0);
            *register = (*register).max(rank);
        }
    }

    /// The estimated number of distinct values, a whole number:
    ///
    /// ```text
    /// α = 0.7213 / (1 + 1.079/m)
    /// Z = Σ_j 2^−M[j]   over all m registers (an absent register is 2^0 = 1)
    /// E = α·m² / Z
    /// if E ≤ 2.5·m and V > 0: E = m·ln(m/V)   (V: registers still 0)
    /// ```
    ///
    /// `Z` is summed exactly as the integer `Z·2^53 = Σ_j 2^(53−M[j])`, so
    /// `E = (α·m²)·2^53 / (Z·2^53)`. `ln` is `rust_decimal`'s. The result
    /// is rounded to an integer half to even (banker's rounding,
    /// `RoundingStrategy::MidpointNearestEven`). Empty: `0`.
    pub fn estimate(&self) -> Decimal {
        self.raw_estimate()
            .unwrap_or(Decimal::ZERO)
            .round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven)
            .normalize()
    }

    fn raw_estimate(&self) -> Option<Decimal> {
        let m = Decimal::from(HLL_REGISTERS);
        let zeros = HLL_REGISTERS - self.registers.len() as u32;
        let shift = u32::from(HLL_MAX_RANK);
        let scaled_z: u128 = (u128::from(zeros) << shift)
            + self
                .registers
                .values()
                .map(|&rank| 1u128 << (shift - u32::from(rank)))
                .sum::<u128>();
        let alpha = Decimal::new(7213, 4)
            .checked_div(Decimal::ONE.checked_add(Decimal::new(1079, 3).checked_div(m)?)?)?;
        let estimate = alpha
            .checked_mul(m.checked_mul(m)?)?
            .checked_mul(Decimal::from(1u64 << shift))?
            .checked_div(Decimal::from_u128(scaled_z)?)?;
        if estimate <= Decimal::new(25, 1).checked_mul(m)? && zeros > 0 {
            let ratio = m.checked_div(Decimal::from(zeros))?;
            return m.checked_mul(ratio.checked_ln()?);
        }
        Some(estimate)
    }
}

/// DDSketch relative accuracy `α`: `0.01`.
pub const DDSKETCH_ACCURACY: Decimal = Decimal::from_parts(1, 0, 0, false, 2);
/// Most buckets per sign before the lowest ones collapse.
pub const DDSKETCH_MAX_BUCKETS: usize = 2048;

/// `γ = (1 + α) / (1 − α) = 101/99`.
static GAMMA: Lazy<Decimal> = Lazy::new(|| Decimal::from(101) / Decimal::from(99));
static LN_GAMMA: Lazy<Decimal> = Lazy::new(|| GAMMA.checked_ln().unwrap_or(Decimal::ONE));

/// DDSketch with relative accuracy `α = 0.01` (`γ = 101/99`, in decimals).
///
/// A positive `x` lands in bucket `i = ceil(ln(x) / ln(γ))` (`ln` is
/// `rust_decimal`'s, `γ` the decimal `101/99`); a negative `x` in the
/// negative store by `|x|`; an exact zero in a separate count. Each store
/// keeps at most 2048 buckets: past that, the lowest-index buckets fold
/// into the lowest kept one. Merging adds the counts (then folds), so
/// merged sketches equal one fed every value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DdSketch {
    positive: BTreeMap<i32, u64>,
    negative: BTreeMap<i32, u64>,
    zero: u64,
    count: u64,
}

impl DdSketch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores a sketch from its buckets and zero count (as the accessors
    /// give them); empty buckets are dropped and the stores fold to 2048.
    pub fn from_parts(
        positive: BTreeMap<i32, u64>,
        negative: BTreeMap<i32, u64>,
        zero: u64,
    ) -> Self {
        let mut sketch = Self {
            positive: positive.into_iter().filter(|&(_, c)| c > 0).collect(),
            negative: negative.into_iter().filter(|&(_, c)| c > 0).collect(),
            zero,
            count: 0,
        };
        sketch.count = sketch
            .positive
            .values()
            .chain(sketch.negative.values())
            .fold(zero, |total, &c| total.saturating_add(c));
        collapse(&mut sketch.positive);
        collapse(&mut sketch.negative);
        sketch
    }

    /// Buckets of the positive values, `index → count`.
    pub fn positive(&self) -> &BTreeMap<i32, u64> {
        &self.positive
    }

    /// Buckets of the negative values by `|x|`, `index → count`.
    pub fn negative(&self) -> &BTreeMap<i32, u64> {
        &self.negative
    }

    /// How many exact zeros were inserted.
    pub fn zero_count(&self) -> u64 {
        self.zero
    }

    /// How many values were inserted.
    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The bucket of a positive `x`: `ceil(ln(x) / ln(γ))`; `None` for
    /// `x ≤ 0`.
    pub fn bucket_index(x: Decimal) -> Option<i32> {
        if x <= Decimal::ZERO {
            return None;
        }
        x.checked_ln()?.checked_div(*LN_GAMMA)?.ceil().to_i32()
    }

    /// The value a bucket stands for: `2·γ^i / (γ + 1)`, which for
    /// `γ = 101/99` is exactly `0.99·γ^i` (`γ^i` by `rust_decimal`'s
    /// `checked_powi`). Saturates at `Decimal::MAX` past the decimal range.
    pub fn bucket_value(index: i32) -> Decimal {
        GAMMA
            .checked_powi(i64::from(index))
            .and_then(|power| power.checked_mul(Decimal::new(99, 2)))
            .map_or(Decimal::MAX, |value| value.normalize())
    }

    pub fn insert(&mut self, x: Decimal) {
        if x.is_zero() {
            self.zero = self.zero.saturating_add(1);
            self.count = self.count.saturating_add(1);
            return;
        }
        // `ln` of a positive decimal always exists: no value is lost.
        let Some(index) = Self::bucket_index(x.abs()) else {
            return;
        };
        let store = if x.is_sign_negative() {
            &mut self.negative
        } else {
            &mut self.positive
        };
        let bucket = store.entry(index).or_insert(0);
        *bucket = bucket.saturating_add(1);
        self.count = self.count.saturating_add(1);
        collapse(store);
    }

    pub fn merge(&mut self, other: &DdSketch) {
        for (store, theirs) in [
            (&mut self.positive, &other.positive),
            (&mut self.negative, &other.negative),
        ] {
            for (&index, &c) in theirs {
                let bucket = store.entry(index).or_insert(0);
                *bucket = bucket.saturating_add(c);
            }
            collapse(store);
        }
        self.zero = self.zero.saturating_add(other.zero);
        self.count = self.count.saturating_add(other.count);
    }

    /// The value at quantile `q` (`0..=1`): with `rank = q·(n − 1)`, walk
    /// the negatives from the most negative (highest index) up, then the
    /// zeros, then the positives ascending; the first bucket whose running
    /// count exceeds `rank` gives the answer (its [`bucket_value`],
    /// negated for negatives; zeros give `0`). That is the sketch's
    /// estimate of the `⌊rank⌋`-th smallest value (no interpolation).
    /// `None` when empty or `q` is outside `0..=1`.
    ///
    /// [`bucket_value`]: DdSketch::bucket_value
    pub fn quantile(&self, q: Decimal) -> Option<Decimal> {
        if self.count == 0 || q < Decimal::ZERO || q > Decimal::ONE {
            return None;
        }
        let rank = q.checked_mul(Decimal::from(self.count - 1))?;
        let mut seen: u64 = 0;
        let mut exceeds = |c: u64| {
            seen = seen.saturating_add(c);
            Decimal::from(seen) > rank
        };
        for (&index, &c) in self.negative.iter().rev() {
            if exceeds(c) {
                return Some(-Self::bucket_value(index));
            }
        }
        if self.zero > 0 && exceeds(self.zero) {
            return Some(Decimal::ZERO);
        }
        for (&index, &c) in &self.positive {
            if exceeds(c) {
                return Some(Self::bucket_value(index));
            }
        }
        // Counts past `u64` saturate: the largest bucket.
        match self.positive.last_key_value() {
            Some((&index, _)) => Some(Self::bucket_value(index)),
            None if self.zero > 0 => Some(Decimal::ZERO),
            None => self
                .negative
                .first_key_value()
                .map(|(&index, _)| -Self::bucket_value(index)),
        }
    }
}

/// Folds the lowest-index buckets into the lowest kept one until at most
/// [`DDSKETCH_MAX_BUCKETS`] remain.
fn collapse(store: &mut BTreeMap<i32, u64>) {
    while store.len() > DDSKETCH_MAX_BUCKETS {
        let Some((_, c)) = store.pop_first() else {
            return;
        };
        if let Some(mut lowest) = store.first_entry() {
            *lowest.get_mut() = lowest.get().saturating_add(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DateValue;
    use rust_decimal_macros::dec;

    fn number(n: Decimal) -> Variable {
        Variable::Number(n)
    }

    #[test]
    fn a_record_hashes_as_its_id() {
        let record: Variable = serde_json::json!({ "id": "M7", "mcc": "5411" }).into();
        let id: Variable = serde_json::json!("M7").into();
        assert_eq!(hash_value(&record), hash_value(&id));
        let no_id: Variable = serde_json::json!({ "mcc": "5411" }).into();
        assert_eq!(hash_value(&no_id), None);
    }

    #[test]
    fn canonical_bytes_rule() {
        assert_eq!(canonical_bytes(&number(dec!(1.000))).unwrap(), b"\x011");
        assert_eq!(canonical_bytes(&number(dec!(-0.50))).unwrap(), b"\x01-0.5");
        assert_eq!(canonical_bytes(&number(dec!(-0.0))).unwrap(), b"\x010");
        assert_eq!(canonical_bytes(&number(dec!(1e3))).unwrap(), b"\x011000");
        assert_eq!(
            canonical_bytes(&number(dec!(0.0001))).unwrap(),
            b"\x010.0001"
        );
        assert_eq!(
            canonical_bytes(&Variable::String("héllo".into())).unwrap(),
            "\x02héllo".as_bytes()
        );
        assert_eq!(canonical_bytes(&Variable::Bool(true)).unwrap(), b"\x03true");
        assert_eq!(
            canonical_bytes(&Variable::Bool(false)).unwrap(),
            b"\x03false"
        );
        let date = DateValue::from_text("2024-03-01T10:20:30.123456+02:00").unwrap();
        assert_eq!(
            canonical_bytes(&date).unwrap(),
            b"\x042024-03-01T08:20:30.123Z"
        );
        let date = DateValue::from_text("2024-03-01T08:20:30Z").unwrap();
        assert_eq!(
            canonical_bytes(&date).unwrap(),
            b"\x042024-03-01T08:20:30.000Z"
        );
        assert_eq!(canonical_bytes(&Variable::Null), None);
        assert_eq!(
            canonical_bytes(&Variable::from_array(vec![number(dec!(1))])),
            None
        );
    }

    #[test]
    fn hash_stability() {
        // XXH3-64, seed 0, of the canonical bytes: fixed across releases.
        // The reference vector of XXH3-64 for no input:
        assert_eq!(xxh3_64(b""), 0x2d06800538d394c2);
        assert_eq!(hash_value(&number(dec!(1))), Some(xxh3_64(b"\x011")));
        assert_eq!(hash_value(&number(dec!(1))), Some(HASH_NUMBER_1));
        assert_eq!(
            hash_value(&Variable::String("1".into())),
            Some(HASH_STRING_1)
        );
        assert_eq!(hash_value(&Variable::Bool(true)), Some(HASH_TRUE));
        assert_eq!(
            hash_value(&Variable::String("".into())),
            Some(HASH_EMPTY_STRING)
        );

        assert_eq!(hash_value(&number(dec!(1))), hash_value(&number(dec!(1.0))));
        assert_eq!(
            hash_value(&number(dec!(0))),
            hash_value(&number(dec!(-0.00)))
        );
        let one = hash_value(&number(dec!(1)));
        assert_ne!(one, hash_value(&Variable::String("1".into())));
        assert_ne!(one, hash_value(&Variable::Bool(true)));
        assert_ne!(
            hash_value(&Variable::String("true".into())),
            hash_value(&Variable::Bool(true))
        );
        assert_eq!(hash_value(&Variable::Null), None);
    }

    const HASH_NUMBER_1: u64 = 0x3fdad128b7dfd3e3; // 0x01 '1'
    const HASH_STRING_1: u64 = 0x534a116932758ab1; // 0x02 '1'
    const HASH_TRUE: u64 = 0x6b7690728bda8e6a; // 0x03 't' 'r' 'u' 'e'
    const HASH_EMPTY_STRING: u64 = 0xc9f42e6c9e93dfff; // 0x02

    fn hll_of(range: std::ops::Range<i64>) -> Hll {
        let mut hll = Hll::new();
        for i in range {
            assert!(hll.insert(&number(Decimal::from(i))));
        }
        hll
    }

    #[test]
    fn hll_rank_and_index() {
        let mut hll = Hll::new();
        hll.insert_hash(0);
        hll.insert_hash(u64::MAX);
        hll.insert_hash((5u64 << 52) | (1u64 << 51));
        hll.insert_hash((5u64 << 52) | 1);
        assert_eq!(
            hll.registers()
                .iter()
                .map(|(&i, &r)| (i, r))
                .collect::<Vec<_>>(),
            vec![(0, 53), (5, 52), (4095, 1)]
        );
    }

    #[test]
    fn hll_small_cardinalities() {
        assert_eq!(Hll::new().estimate(), dec!(0));
        for n in [1i64, 2, 3, 10, 50, 100, 500] {
            let estimate = hll_of(0..n).estimate();
            let error = (estimate - Decimal::from(n)).abs();
            assert!(
                error <= Decimal::from(n) / dec!(50) + dec!(1),
                "n = {n}: {estimate}"
            );
        }
        assert_eq!(hll_of(0..1).estimate(), dec!(1));
        assert_eq!(hll_of(0..10).estimate(), dec!(10));

        // Duplicates do not count.
        let mut hll = hll_of(0..10);
        hll.insert(&number(dec!(3.0)));
        hll.insert(&number(dec!(4)));
        assert_eq!(hll, hll_of(0..10));
    }

    #[test]
    fn hll_large_cardinality() {
        let n = 100_000i64;
        let estimate = hll_of(0..n).estimate();
        let error = (estimate - Decimal::from(n)).abs() / Decimal::from(n);
        assert!(error <= dec!(0.03), "{estimate}");
    }

    #[test]
    fn hll_merge_equals_insert_all() {
        let mut merged = hll_of(0..3000);
        merged.merge(&hll_of(2000..7000));
        assert_eq!(merged, hll_of(0..7000));
        assert_eq!(merged.estimate(), hll_of(0..7000).estimate());

        let restored = Hll::from_registers(merged.registers().clone()).unwrap();
        assert_eq!(restored, merged);
        assert_eq!(Hll::from_registers(BTreeMap::from([(4096, 1)])), None);
        assert_eq!(Hll::from_registers(BTreeMap::from([(1, 54)])), None);
    }

    /// `α`, plus the last digits a decimal `γ^i` may round.
    const TOLERANCE: Decimal = dec!(0.01000000000000000001);

    fn exact_quantile(values: &[Decimal], q: Decimal) -> Decimal {
        let mut sorted = values.to_vec();
        sorted.sort();
        let rank = (q * Decimal::from(sorted.len() - 1)).floor();
        sorted[rank.to_usize().unwrap()]
    }

    fn sketch_of(values: &[Decimal]) -> DdSketch {
        let mut sketch = DdSketch::new();
        for &x in values {
            sketch.insert(x);
        }
        sketch
    }

    fn spread() -> Vec<Decimal> {
        let mut values = Vec::new();
        for i in 1..=500i64 {
            // 0.01 .. ~5000, mixed magnitudes.
            values.push(Decimal::from(i * i) / dec!(50));
            values.push(-Decimal::from(i * 7 % 113) / dec!(3));
            if i % 25 == 0 {
                values.push(Decimal::ZERO);
            }
        }
        values.push(dec!(123456789.123));
        values.push(dec!(0.000001));
        values
    }

    #[test]
    fn ddsketch_relative_accuracy() {
        let values = spread();
        let sketch = sketch_of(&values);
        assert_eq!(sketch.count(), values.len() as u64);
        assert_eq!(sketch.zero_count(), 24);
        for q in 0..=100i64 {
            let q = Decimal::new(q, 2);
            let exact = exact_quantile(&values, q);
            let approx = sketch.quantile(q).unwrap();
            if exact.is_zero() {
                assert_eq!(approx, Decimal::ZERO, "q = {q}");
                continue;
            }
            let error = ((approx - exact) / exact).abs();
            assert!(error <= TOLERANCE, "q = {q}: {approx} vs {exact}");
        }
        assert_eq!(sketch.quantile(dec!(1.5)), None);
        assert_eq!(sketch.quantile(dec!(-0.1)), None);
        assert_eq!(DdSketch::new().quantile(dec!(0.5)), None);
    }

    #[test]
    fn ddsketch_buckets() {
        assert_eq!(DdSketch::bucket_index(dec!(1)), Some(0));
        assert_eq!(DdSketch::bucket_index(dec!(1.01)), Some(1));
        assert_eq!(DdSketch::bucket_index(dec!(0)), None);
        assert_eq!(DdSketch::bucket_value(0), dec!(0.99));
        let one_bucket = DdSketch::bucket_value(1);
        assert!((one_bucket - dec!(1.01)).abs() < dec!(0.000000000000000000000001));
        assert!(DdSketch::bucket_index(Decimal::MAX).is_some());
        assert!(DdSketch::bucket_index(dec!(0.0000000000000000000000000001)).is_some());
        for x in [Decimal::MAX, dec!(0.0000000000000000000000000001)] {
            let approx = sketch_of(&[x]).quantile(dec!(0.5)).unwrap();
            assert!(((approx - x) / x).abs() <= TOLERANCE, "{x}: {approx}");
        }
        let five = DdSketch::bucket_value(DdSketch::bucket_index(dec!(5)).unwrap());
        assert_eq!(sketch_of(&[dec!(-5)]).quantile(dec!(0)), Some(-five));
    }

    #[test]
    fn ddsketch_merge_equals_insert_all() {
        let values = spread();
        let (left, right) = values.split_at(values.len() / 3);
        let mut merged = sketch_of(left);
        merged.merge(&sketch_of(right));
        assert_eq!(merged, sketch_of(&values));

        let restored = DdSketch::from_parts(
            merged.positive().clone(),
            merged.negative().clone(),
            merged.zero_count(),
        );
        assert_eq!(restored, merged);
    }

    #[test]
    fn ddsketch_collapses_lowest_buckets() {
        // γ^i for 3000 consecutive buckets: 1e-13 .. 1e13.
        let mut sketch = DdSketch::new();
        let mut values = Vec::new();
        for i in -1500..1500i32 {
            let x = DdSketch::bucket_value(i);
            values.push(x);
            sketch.insert(x);
            sketch.insert(-x);
        }
        assert_eq!(sketch.positive().len(), DDSKETCH_MAX_BUCKETS);
        assert_eq!(sketch.negative().len(), DDSKETCH_MAX_BUCKETS);
        assert_eq!(sketch.count(), 6000);
        let lowest = *sketch.positive().keys().next().unwrap();
        assert_eq!(sketch.positive()[&lowest], 3000 - 2047);

        // The upper quantiles stay accurate.
        let all: Vec<Decimal> = values.iter().flat_map(|&x| [x, -x]).collect();
        for q in [dec!(0.75), dec!(0.9), dec!(0.99), dec!(1)] {
            let exact = exact_quantile(&all, q);
            let approx = sketch.quantile(q).unwrap();
            let error = ((approx - exact) / exact).abs();
            assert!(error <= TOLERANCE, "q = {q}: {approx} vs {exact}");
        }

        // Folding is the same whether merged or inserted.
        let mut merged = DdSketch::new();
        for chunk in values.chunks(700) {
            let mut part = DdSketch::new();
            for &x in chunk {
                part.insert(x);
                part.insert(-x);
            }
            merged.merge(&part);
        }
        assert_eq!(merged.positive(), sketch.positive());
    }
}
