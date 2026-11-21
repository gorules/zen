use anyhow::{bail, Context};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, MathematicalOps, RoundingStrategy};

pub const SPREAD_DP: u32 = 12;

fn exact_add(a: Decimal, b: Decimal) -> anyhow::Result<Decimal> {
    let sum = a.checked_add(b).context("Number overflow")?;
    if sum.scale() < a.scale().max(b.scale()) {
        bail!("The sum would lose precision");
    }
    Ok(sum)
}

fn exact_square(value: Decimal) -> anyhow::Result<Decimal> {
    let square = value.checked_mul(value).context("Number overflow")?;
    if square.scale() < value.scale() * 2 {
        bail!("The square of {value} would lose precision");
    }
    Ok(square)
}

fn raw_variance(values: &[Decimal], sample: bool) -> anyhow::Result<Option<Decimal>> {
    let mut sum = Decimal::ZERO;
    let mut squares = Decimal::ZERO;
    for x in values {
        sum = exact_add(sum, *x)?;
        squares = exact_add(squares, exact_square(*x)?)?;
    }
    let n = Decimal::from(values.len());
    let divisor = match sample {
        true => n - Decimal::ONE,
        false => n,
    };
    if divisor <= Decimal::ZERO {
        return Ok(None);
    }
    let centered = sum
        .checked_mul(sum)
        .and_then(|s| s.checked_div(n))
        .and_then(|s| squares.checked_sub(s))
        .context("Number overflow")?;
    let variance = centered
        .max(Decimal::ZERO)
        .checked_div(divisor)
        .context("Number overflow")?;
    Ok(Some(variance))
}

fn root(value: Decimal) -> Option<Decimal> {
    let guess = Decimal::from_f64(value.to_f64()?.sqrt())?;
    if guess.is_zero() {
        return Some(guess);
    }
    let mut x = guess;
    for _ in 0..4 {
        x = x
            .checked_add(value.checked_div(x)?)?
            .checked_div(Decimal::TWO)?;
    }
    Some(x)
}

fn spread(value: Decimal) -> Decimal {
    value.round_dp(SPREAD_DP).normalize()
}

pub fn variance(values: &[Decimal], sample: bool) -> anyhow::Result<Option<Decimal>> {
    Ok(raw_variance(values, sample)?.map(spread))
}

pub fn stddev(values: &[Decimal], sample: bool) -> anyhow::Result<Option<Decimal>> {
    match raw_variance(values, sample)? {
        None => Ok(None),
        Some(variance) => root(variance)
            .map(|r| Some(spread(r)))
            .context("Number overflow"),
    }
}

pub fn percentile(sorted: &[Decimal], q: Decimal) -> anyhow::Result<Option<Decimal>> {
    if !(Decimal::ZERO..=Decimal::ONE).contains(&q) {
        bail!("Percentile must be between 0 and 1, got {q}");
    }
    if sorted.is_empty() {
        return Ok(None);
    }
    let position = q
        .checked_mul(Decimal::from(sorted.len() - 1))
        .context("Number overflow")?;
    let low = position.floor();
    let index = low.to_usize().context("Index out of bounds")?;
    let fraction = position - low;
    let value = if fraction.is_zero() {
        sorted[index]
    } else {
        let (a, b) = (sorted[index], sorted[index + 1]);
        b.checked_sub(a)
            .and_then(|d| d.checked_mul(fraction))
            .and_then(|d| a.checked_add(d))
            .context("Number overflow")?
    };
    Ok(Some(value.normalize()))
}

fn power_sums(values: &[Decimal]) -> Option<(Decimal, [Decimal; 4])> {
    let mut s = [Decimal::ZERO; 4];
    for x in values {
        let mut power = Decimal::ONE;
        for sum in s.iter_mut() {
            power = power.checked_mul(*x)?;
            *sum = sum.checked_add(power)?;
        }
    }
    Some((Decimal::from(values.len()), s))
}

fn moments(values: &[Decimal]) -> anyhow::Result<Option<(Decimal, [Decimal; 4], Decimal)>> {
    if values.len() < 2 {
        return Ok(None);
    }
    let (n, s) = power_sums(values).context("Number overflow")?;
    let mean = s[0].checked_div(n).context("Number overflow")?;
    let s2n = s[1].checked_div(n).context("Number overflow")?;
    let m2 = mean
        .checked_mul(mean)
        .and_then(|m| s2n.checked_sub(m))
        .context("Number overflow")?;
    if m2 <= Decimal::ZERO {
        return Ok(None);
    }
    Ok(Some((n, s, m2)))
}

fn shape(value: Decimal) -> Decimal {
    value
        .round_dp_with_strategy(SPREAD_DP, RoundingStrategy::MidpointNearestEven)
        .normalize()
}

pub fn skew(values: &[Decimal]) -> anyhow::Result<Option<Decimal>> {
    let Some((n, s, m2)) = moments(values)? else {
        return Ok(None);
    };
    let compute = || -> Option<Decimal> {
        let mean = s[0].checked_div(n)?;
        let s2n = s[1].checked_div(n)?;
        let s3n = s[2].checked_div(n)?;
        let mean3 = mean.checked_mul(mean)?.checked_mul(mean)?;
        let m3 = s3n
            .checked_sub(Decimal::from(3).checked_mul(mean)?.checked_mul(s2n)?)?
            .checked_add(Decimal::TWO.checked_mul(mean3)?)?;
        let denominator = m2.checked_mul(m2.sqrt()?)?;
        m3.checked_div(denominator)
    };
    compute().map(|v| Some(shape(v))).context("Number overflow")
}

pub fn kurtosis(values: &[Decimal]) -> anyhow::Result<Option<Decimal>> {
    let Some((n, s, m2)) = moments(values)? else {
        return Ok(None);
    };
    let compute = || -> Option<Decimal> {
        let mean = s[0].checked_div(n)?;
        let s2n = s[1].checked_div(n)?;
        let s3n = s[2].checked_div(n)?;
        let s4n = s[3].checked_div(n)?;
        let mean2 = mean.checked_mul(mean)?;
        let mean4 = mean2.checked_mul(mean2)?;
        let m4 = s4n
            .checked_sub(Decimal::from(4).checked_mul(mean)?.checked_mul(s3n)?)?
            .checked_add(Decimal::from(6).checked_mul(mean2)?.checked_mul(s2n)?)?
            .checked_sub(Decimal::from(3).checked_mul(mean4)?)?;
        m4.checked_div(m2.checked_mul(m2)?)?
            .checked_sub(Decimal::from(3))
    };
    compute().map(|v| Some(shape(v))).context("Number overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn moments_match_hand_computed_values() {
        let values = [dec!(1), dec!(2), dec!(3), dec!(4), dec!(10)];
        assert_eq!(skew(&values).unwrap(), Some(dec!(1.138419957661)));
        assert_eq!(kurtosis(&values).unwrap(), Some(dec!(-0.212)));
        let flat = [dec!(5), dec!(5)];
        assert_eq!(skew(&flat).unwrap(), None);
        assert_eq!(kurtosis(&[dec!(5)]).unwrap(), None);
    }

    #[test]
    fn spreads_match_the_textbook() {
        let values = [
            dec!(2),
            dec!(4),
            dec!(4),
            dec!(4),
            dec!(5),
            dec!(5),
            dec!(7),
            dec!(9),
        ];
        assert_eq!(variance(&values, false).unwrap(), Some(dec!(4)));
        assert_eq!(stddev(&values, false).unwrap(), Some(dec!(2)));
        assert_eq!(variance(&values, true).unwrap(), Some(dec!(4.571428571429)));
        assert_eq!(stddev(&values, true).unwrap(), Some(dec!(2.138089935299)));
        assert_eq!(variance(&[dec!(1)], true).unwrap(), None);
        assert_eq!(variance(&[dec!(1)], false).unwrap(), Some(dec!(0)));
        assert_eq!(variance(&[], false).unwrap(), None);
    }

    #[test]
    fn percentile_interpolates_between_ranks() {
        let sorted = [dec!(1), dec!(2), dec!(3), dec!(4)];
        assert_eq!(percentile(&sorted, dec!(0.5)).unwrap(), Some(dec!(2.5)));
        assert_eq!(percentile(&sorted, dec!(1)).unwrap(), Some(dec!(4)));
        assert_eq!(percentile(&sorted, dec!(0)).unwrap(), Some(dec!(1)));
        assert!(percentile(&sorted, dec!(1.5)).is_err());
        assert_eq!(percentile(&[], dec!(0.5)).unwrap(), None);
    }
}
