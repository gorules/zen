//! Shape of a distribution (skewness, excess kurtosis) from exact decimal
//! power sums, so partial sums `(n, Σx, Σx², Σx³, Σx⁴)` can be merged
//! (added) anywhere and still give the same digits.
//!
//! With `mean = s1/n`, the population central moments are
//!
//! ```text
//! m2 = s2/n − mean²
//! m3 = s3/n − 3·mean·(s2/n) + 2·mean³
//! m4 = s4/n − 4·mean·(s3/n) + 6·mean²·(s2/n) − 3·mean⁴
//! ```
//!
//! Every step is a checked `rust_decimal` operation (28 significant
//! digits); an overflow gives `None`, never a panic.

use rust_decimal::{Decimal, MathematicalOps};

/// The power sums `(n, [Σx, Σx², Σx³, Σx⁴])` of `values`; `None` on overflow.
pub fn power_sums<I>(values: I) -> Option<(i64, [Decimal; 4])>
where
    I: IntoIterator<Item = Decimal>,
{
    let mut n = 0i64;
    let mut s = [Decimal::ZERO; 4];
    for x in values {
        n = n.checked_add(1)?;
        let mut power = Decimal::ONE;
        for sum in s.iter_mut() {
            power = power.checked_mul(x)?;
            *sum = sum.checked_add(power)?;
        }
    }
    Some((n, s))
}

/// `(n, mean, s2/n, m2)` when there are at least two values and `m2 > 0`.
fn spread(n: i64, s: &[Decimal; 4]) -> Option<(Decimal, Decimal, Decimal, Decimal)> {
    if n < 2 {
        return None;
    }
    let n = Decimal::from(n);
    let mean = s[0].checked_div(n)?;
    let s2n = s[1].checked_div(n)?;
    let m2 = s2n.checked_sub(mean.checked_mul(mean)?)?;
    if m2 <= Decimal::ZERO {
        return None;
    }
    Some((n, mean, s2n, m2))
}

/// Decimal places of `skew` and `kurtosis`: Σx⁴ of large values rounds at
/// each add, so a sum taken in another order (a feature store merging time
/// buckets) differs in the last digits; at 12 places both agree.
pub const MOMENT_SCALE: u32 = 12;

fn finish(value: Decimal) -> Decimal {
    value
        .round_dp_with_strategy(MOMENT_SCALE, rust_decimal::RoundingStrategy::MidpointNearestEven)
        .normalize()
}

/// Population skewness `g1 = m3 / m2^(3/2)` (`m2^(3/2)` as `m2·√m2`) from
/// `s = [Σx, Σx², Σx³, Σx⁴]` over `n` values. `None` when `n < 2`, when
/// `m2 ≤ 0` (all values equal) or on overflow.
/// Rounded to [`MOMENT_SCALE`] places.
pub fn skew(n: i64, s: [Decimal; 4]) -> Option<Decimal> {
    let (nd, mean, s2n, m2) = spread(n, &s)?;
    let s3n = s[2].checked_div(nd)?;
    let mean2 = mean.checked_mul(mean)?;
    let mean3 = mean2.checked_mul(mean)?;
    let m3 = s3n
        .checked_sub(Decimal::from(3).checked_mul(mean)?.checked_mul(s2n)?)?
        .checked_add(Decimal::TWO.checked_mul(mean3)?)?;
    let denominator = m2.checked_mul(m2.sqrt()?)?;
    Some(finish(m3.checked_div(denominator)?))
}

/// Excess kurtosis `g2 = m4 / m2² − 3` from `s = [Σx, Σx², Σx³, Σx⁴]` over
/// `n` values. `None` when `n < 2`, when `m2 ≤ 0` or on overflow. Rounded
/// to [`MOMENT_SCALE`] places.
pub fn kurtosis(n: i64, s: [Decimal; 4]) -> Option<Decimal> {
    let (nd, mean, s2n, m2) = spread(n, &s)?;
    let s3n = s[2].checked_div(nd)?;
    let s4n = s[3].checked_div(nd)?;
    let mean2 = mean.checked_mul(mean)?;
    let mean4 = mean2.checked_mul(mean2)?;
    let m4 = s4n
        .checked_sub(Decimal::from(4).checked_mul(mean)?.checked_mul(s3n)?)?
        .checked_add(Decimal::from(6).checked_mul(mean2)?.checked_mul(s2n)?)?
        .checked_sub(Decimal::from(3).checked_mul(mean4)?)?;
    let g2 = m4
        .checked_div(m2.checked_mul(m2)?)?
        .checked_sub(Decimal::from(3))?;
    Some(finish(g2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sums(values: &[Decimal]) -> (i64, [Decimal; 4]) {
        power_sums(values.iter().copied()).unwrap()
    }

    fn close(a: Decimal, b: Decimal, tolerance: Decimal) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn hand_computed() {
        // Deviations from the mean 4: −3, −2, −1, 0, 6.
        // m2 = 50/5 = 10, m3 = 180/5 = 36, m4 = 1394/5 = 278.8.
        let (n, s) = sums(&[dec!(1), dec!(2), dec!(3), dec!(4), dec!(10)]);
        assert_eq!(n, 5);
        assert_eq!(s, [dec!(20), dec!(130), dec!(1100), dec!(10354)]);
        // 36 / (10·√10) = 1.13841995766062...
        let g1 = skew(n, s).unwrap();
        // 1.13841995766061655952… at 12 places.
        assert_eq!(g1, dec!(1.138419957661));
        // 278.8 / 100 − 3, exactly.
        assert_eq!(kurtosis(n, s), Some(dec!(-0.212)));
    }

    #[test]
    fn symmetric() {
        let (n, s) = sums(&[dec!(1), dec!(2), dec!(3)]);
        assert_eq!(skew(n, s), Some(Decimal::ZERO));
        // m2 = 2/3, m4 = 2/3: (2/3)/(4/9) − 3 = −1.5.
        let g2 = kurtosis(n, s).unwrap();
        assert!(close(g2, dec!(-1.5), dec!(0.000000000000000000001)), "{g2}");
    }

    #[test]
    fn negative_skew_and_decimals() {
        let (n, s) = sums(&[dec!(-1), dec!(-2), dec!(-3), dec!(-4), dec!(-10)]);
        let g1 = skew(n, s).unwrap();
        assert_eq!(g1, dec!(-1.138419957661));
        assert_eq!(kurtosis(n, s), Some(dec!(-0.212)));

        // Scaling does not change the shape.
        let (n, s) = sums(&[dec!(0.1), dec!(0.2), dec!(0.3), dec!(0.4), dec!(1.0)]);
        let g1 = skew(n, s).unwrap();
        assert_eq!(g1, dec!(1.138419957661));
        // At 12 places the rounding of the scaled sums is gone.
        assert_eq!(kurtosis(n, s), Some(dec!(-0.212)));
    }

    #[test]
    fn too_little_or_flat() {
        assert_eq!(skew(0, [Decimal::ZERO; 4]), None);
        let (n, s) = sums(&[dec!(5)]);
        assert_eq!(skew(n, s), None);
        assert_eq!(kurtosis(n, s), None);
        let (n, s) = sums(&[dec!(5), dec!(5), dec!(5)]);
        assert_eq!(skew(n, s), None);
        assert_eq!(kurtosis(n, s), None);
    }

    #[test]
    fn overflow_is_none() {
        assert_eq!(power_sums([Decimal::MAX]), None);
        let big = dec!(10000000000000000000000);
        assert_eq!(power_sums([big, Decimal::ONE]), None);
        let huge = [Decimal::MAX, Decimal::MAX, Decimal::MAX, Decimal::MAX];
        assert_eq!(skew(2, huge), None);
        assert_eq!(kurtosis(2, huge), None);
    }
}
