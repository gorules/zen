use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Path {
    Typed,
    Lane,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Route {
    pub path: Path,
    pub fresh: bool,
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

    pub(crate) fn path(&self) -> Route {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if call < Self::WARM {
            return Route {
                path: Path::Typed,
                fresh: false,
            };
        }
        if call < 2 * Self::WARM {
            return Route {
                path: Path::Lane,
                fresh: false,
            };
        }
        let (faster, slower) =
            match self.typed.load(Ordering::Relaxed) <= self.lane.load(Ordering::Relaxed) {
                true => (Path::Typed, Path::Lane),
                false => (Path::Lane, Path::Typed),
            };
        match call % Self::PERIOD {
            0 => Route {
                path: slower,
                fresh: true,
            },
            1 => Route {
                path: faster,
                fresh: true,
            },
            _ => Route {
                path: faster,
                fresh: false,
            },
        }
    }

    pub(crate) fn typed(&self, per: u64) {
        self.typed.store(per, Ordering::Relaxed);
    }

    pub(crate) fn credit(&self, saving: u64) {
        if saving > 0 {
            let _ = self
                .typed
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |typed| {
                    Some(typed.saturating_sub(saving))
                });
        }
    }

    pub(crate) fn record(&self, Route { path, fresh }: Route, elapsed: Duration, rows: usize) {
        let per = u64::try_from(elapsed.as_nanos().saturating_mul(1024) / rows.max(1) as u128)
            .unwrap_or(u64::MAX - 1);
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
    use super::{Cost, Density, Path, Route};
    use std::time::Duration;

    fn timed(cost: &Cost, typed: u64, lane: u64) -> Vec<Route> {
        (0..130)
            .map(|_| {
                let path = cost.path();
                let nanos = match path.path {
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
        assert_eq!(
            &calls[..4],
            &[
                Route {
                    path: Path::Typed,
                    fresh: false
                },
                Route {
                    path: Path::Typed,
                    fresh: false
                },
                Route {
                    path: Path::Lane,
                    fresh: false
                },
                Route {
                    path: Path::Lane,
                    fresh: false
                }
            ]
        );
        assert!(calls[4..64].iter().all(|&call| call
            == Route {
                path: Path::Typed,
                fresh: false
            }));
        assert_eq!(
            calls[64],
            Route {
                path: Path::Lane,
                fresh: true
            }
        );
        assert_eq!(
            calls[65],
            Route {
                path: Path::Typed,
                fresh: true
            }
        );
        assert!(calls[66..128].iter().all(|&call| call
            == Route {
                path: Path::Typed,
                fresh: false
            }));
        assert_eq!(
            calls[128],
            Route {
                path: Path::Lane,
                fresh: true
            }
        );
        assert_eq!(
            calls[129],
            Route {
                path: Path::Typed,
                fresh: true
            }
        );
    }

    #[test]
    fn lane_wins_when_it_is_faster() {
        let cost = Cost::new();
        let calls = timed(&cost, 30, 20);
        assert!(calls[4..64].iter().all(|&call| call
            == Route {
                path: Path::Lane,
                fresh: false
            }));
        assert_eq!(
            calls[64],
            Route {
                path: Path::Typed,
                fresh: true
            }
        );
        assert_eq!(
            calls[65],
            Route {
                path: Path::Lane,
                fresh: true
            }
        );
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
        cost.record(
            Route {
                path: Path::Typed,
                fresh: true,
            },
            Duration::from_nanos(50),
            1024,
        );
        assert_eq!(
            cost.path(),
            Route {
                path: Path::Typed,
                fresh: true
            }
        );
        cost.record(
            Route {
                path: Path::Typed,
                fresh: false,
            },
            Duration::from_nanos(5),
            1024,
        );
        assert_eq!(
            cost.path(),
            Route {
                path: Path::Typed,
                fresh: true
            }
        );
        assert_eq!(
            cost.path(),
            Route {
                path: Path::Typed,
                fresh: false
            }
        );
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
