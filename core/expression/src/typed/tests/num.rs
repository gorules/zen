use crate::lane::scaled::Scaled;
use crate::typed::Dec;
use rust_decimal::Decimal;
use std::cmp::Ordering;

struct Gen(u64);

impl Gen {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }

    fn sign(&mut self) -> i64 {
        if self.next(2) == 0 {
            1
        } else {
            -1
        }
    }

    fn mantissa(&mut self) -> i64 {
        match self.next(10) {
            0 | 1 => 0,
            2 => i64::MAX - self.next(3) as i64,
            3 => i64::MIN + self.next(3) as i64,
            4 => Dec::POW10[self.next(19) as usize] * self.sign(),
            5 => (self.next(1 << 62) as i64) * self.sign(),
            6 => (self.next(1000) as i64 * 10) * self.sign(),
            _ => self.next(20_000) as i64 - 10_000,
        }
    }

    fn scale(&mut self) -> u8 {
        match self.next(8) {
            0 => 28,
            1 => self.next(29) as u8,
            _ => self.next(4) as u8,
        }
    }

    fn dec(&mut self) -> Dec {
        Dec::new(self.mantissa(), self.scale())
    }
}

struct Exact;

impl Exact {
    fn same(got: Dec, want: Decimal) -> bool {
        Scaled::parts(&want) == Some((got.mant, got.scale)) && got.decimal().to_string() == want.to_string()
    }
}

#[test]
fn num_matches_rust_decimal() {
    let mut g = Gen(0xA5A5_5A5A_1234_5678);
    let mut checked = [0usize; 5];
    for _ in 0..1_000_000 {
        let (a, b) = (g.dec(), g.dec());
        let (x, y) = (a.decimal(), b.decimal());
        let ops: [(Result<Dec, crate::typed::Fault>, Option<Decimal>); 3] =
            [(a.checked_add(b), x.checked_add(y)), (a.checked_sub(b), x.checked_sub(y)), (a.checked_mul(b), x.checked_mul(y))];
        for (k, (got, want)) in ops.into_iter().enumerate() {
            if let Ok(got) = got {
                let want = want.unwrap_or_else(|| panic!("op {k}: {x} {y} -> {got} but rust_decimal overflows"));
                assert!(Exact::same(got, want), "op {k}: {x} {y} -> {}e-{} vs {want}", got.mant, got.scale);
                checked[k] += 1;
            }
        }
        match a.checked_div(b) {
            Ok(Some(q)) => {
                let want = x.checked_div(y).unwrap_or_else(|| panic!("{x} / {y} -> {q} but rust_decimal fails"));
                assert!(Exact::same(q, want), "{x} / {y} -> {}e-{} vs {want}", q.mant, q.scale);
                checked[3] += 1;
            }
            Ok(None) => assert!(b.is_zero(), "{x} / {y} gave null"),
            Err(_) => assert!(!b.is_zero(), "{x} / {y} faulted on zero"),
        }
        match a.checked_neg() {
            Ok(n) => assert!(Exact::same(n, -x), "-{x}"),
            Err(_) => assert!(a.is_zero() || a.mant == i64::MIN, "-{x} faulted"),
        }
        assert_eq!(a.compare(b), x.cmp(&y), "{x} <=> {y}");
        assert_eq!(a.eq_value(b), x == y, "{x} == {y}");
        checked[4] += 1;
    }
    assert!(checked.iter().all(|c| *c > 100_000), "{checked:?}");
}

#[test]
fn powers_of_ten_divide_like_rust_decimal() {
    let mut g = Gen(0x0F0F_F0F0_DEAD_BEEF);
    let mut checked = 0usize;
    for _ in 0..1_000_000 {
        let a = g.dec();
        let k = g.next(10) as u32;
        let by = Dec::new(Dec::POW10[k as usize], g.scale());
        assert_eq!(by.power(), Some(k));
        let want = a.decimal().checked_div(by.decimal());
        match a.checked_div(by) {
            Ok(Some(q)) => {
                assert!(want.is_some_and(|w| Exact::same(q, w)), "{a} / {by} -> {}e-{} vs {want:?}", q.mant, q.scale);
                checked += 1;
            }
            Ok(None) => panic!("{a} / {by} gave null"),
            Err(_) => {}
        }
    }
    assert!(checked > 500_000, "{checked}");
}

#[test]
fn zero_rules_keep_rust_decimal_scale() {
    let cases = [
        (Dec::new(0, 2), Dec::int(5), "5", "-5", "0"),
        (Dec::int(5), Dec::new(0, 2), "5", "5", "0"),
        (Dec::new(0, 2), Dec::new(0, 3), "0.000", "0.000", "0"),
        (Dec::new(150, 2), Dec::int(2), "3.50", "-0.50", "3.00"),
    ];
    for (a, b, sum, diff, prod) in cases {
        assert_eq!(a.checked_add(b).map(|d| d.to_string()), Ok(sum.to_string()), "{a} + {b}");
        assert_eq!(a.checked_sub(b).map(|d| d.to_string()), Ok(diff.to_string()), "{a} - {b}");
        assert_eq!(a.checked_mul(b).map(|d| d.to_string()), Ok(prod.to_string()), "{a} * {b}");
        assert_eq!(a.checked_add(b).map(|d| d.to_string()), Ok(a.decimal().checked_add(b.decimal()).map(|d| d.to_string()).unwrap_or_default()));
    }
    let hundred = Dec::int(100);
    assert_eq!(Dec::int(150).checked_div(hundred).map(|q| q.map(|q| q.to_string())), Ok(Some("1.50".to_string())));
    assert_eq!(Dec::int(300).checked_div(hundred).map(|q| q.map(|q| q.to_string())), Ok(Some("3".to_string())));
    assert_eq!(Dec::new(0, 4).checked_div(hundred).map(|q| q.map(|q| q.to_string())), Ok(Some("0".to_string())));
    assert_eq!(Dec::int(1).checked_div(Dec::ZERO), Ok(None));
    assert!(Dec::new(0, 2).checked_neg().is_err());
    assert_eq!(Dec::int(1).compare(Dec::new(100, 2)), Ordering::Equal);
}

#[test]
fn power_kernel_matches_scaled() {
    use crate::typed::kernel::K;
    let mut g = Gen(0x1357_9BDF_2468_ACE0);
    for round in 0..2_000_000u32 {
        let m = match round % 4 {
            0 => g.mantissa() % 1_000_000,
            1 => (g.mantissa() % 100_000) * Dec::POW10[g.next(8) as usize],
            2 => ((g.mantissa() % 4096) << 24) * Dec::POW10[g.next(4) as usize],
            _ => g.mantissa(),
        };
        let (s, by, k) = (g.scale(), g.scale(), g.next(10) as u32);
        let (q, t, slow) = K::div_pow(m, s as i32 - by as i32, k);
        if !slow {
            assert_eq!(Some((q, t)), Scaled::divide_power((m, s), k, by), "{m}e-{s} / 10^{k}e-{by}");
        }
    }
}
