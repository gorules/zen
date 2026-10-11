use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Path {
    Typed,
    Lane,
}

pub(crate) struct Cost {
    calls: AtomicU32,
    typed: AtomicU64,
    lane: AtomicU64,
}

pub(crate) struct Density;

impl Density {
    pub(crate) fn sparse(local: usize, rows: usize) -> bool {
        local * 4 < rows
    }
}

impl Cost {
    const WARM: u32 = 2;
    const PERIOD: u32 = 64;

    pub(crate) fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
            typed: AtomicU64::new(u64::MAX),
            lane: AtomicU64::new(u64::MAX),
        }
    }

    pub(crate) fn path(&self) -> (Path, bool) {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if call < Self::WARM {
            return (Path::Typed, false);
        }
        if call < 2 * Self::WARM {
            return (Path::Lane, false);
        }
        let (faster, slower) = match self.typed.load(Ordering::Relaxed) <= self.lane.load(Ordering::Relaxed) {
            true => (Path::Typed, Path::Lane),
            false => (Path::Lane, Path::Typed),
        };
        match call % Self::PERIOD {
            0 => (slower, true),
            1 => (faster, true),
            _ => (faster, false),
        }
    }

    pub(crate) fn record(&self, (path, fresh): (Path, bool), elapsed: Duration, rows: usize) {
        let per = u64::try_from(elapsed.as_nanos().saturating_mul(1024) / rows.max(1) as u128).unwrap_or(u64::MAX - 1);
        let cell = match path {
            Path::Typed => &self.typed,
            Path::Lane => &self.lane,
        };
        match fresh {
            true => cell.store(per, Ordering::Relaxed),
            false => {
                cell.fetch_min(per, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cost, Density, Path};
    use std::time::Duration;

    fn timed(cost: &Cost, typed: u64, lane: u64) -> Vec<(Path, bool)> {
        (0..130)
            .map(|_| {
                let path = cost.path();
                let nanos = match path.0 {
                    Path::Typed => typed,
                    Path::Lane => lane,
                };
                cost.record(path, Duration::from_nanos(nanos), 1024);
                path
            })
            .collect()
    }

    #[test]
    fn warm_calls_alternate_then_pick_the_faster_path() {
        let cost = Cost::new();
        let calls = timed(&cost, 10, 20);
        assert_eq!(&calls[..4], &[(Path::Typed, false), (Path::Typed, false), (Path::Lane, false), (Path::Lane, false)]);
        assert!(calls[4..64].iter().all(|&call| call == (Path::Typed, false)));
        assert_eq!(calls[64], (Path::Lane, true));
        assert_eq!(calls[65], (Path::Typed, true));
        assert!(calls[66..128].iter().all(|&call| call == (Path::Typed, false)));
        assert_eq!(calls[128], (Path::Lane, true));
        assert_eq!(calls[129], (Path::Typed, true));
    }

    #[test]
    fn lane_wins_when_it_is_faster() {
        let cost = Cost::new();
        let calls = timed(&cost, 30, 20);
        assert!(calls[4..64].iter().all(|&call| call == (Path::Lane, false)));
        assert_eq!(calls[64], (Path::Typed, true));
        assert_eq!(calls[65], (Path::Lane, true));
    }

    #[test]
    fn fresh_probe_replaces_the_minimum() {
        let cost = Cost::new();
        for _ in 0..4 {
            let path = cost.path();
            cost.record(path, Duration::from_nanos(10), 1024);
        }
        (4..64).for_each(|_| {
            cost.path();
        });
        cost.record((Path::Typed, true), Duration::from_nanos(50), 1024);
        assert_eq!(cost.path(), (Path::Typed, true));
        cost.record((Path::Typed, false), Duration::from_nanos(5), 1024);
        assert_eq!(cost.path(), (Path::Typed, true));
        assert_eq!(cost.path(), (Path::Typed, false));
    }

    #[test]
    fn density_matches_both_gates() {
        for rows in 0..70 {
            for local in 0..=rows {
                let full = local == rows;
                assert_eq!(Density::sparse(local, rows), !full && local * 4 < rows);
            }
        }
    }
}
