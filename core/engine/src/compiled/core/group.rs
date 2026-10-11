pub(crate) enum Attempt<T> {
    Typed(T, usize),
    Deferred,
    Failed,
}

pub(crate) enum Step<T> {
    Lowered,
    Stop,
    Done(T),
}

pub(crate) enum Built<T> {
    Done(T),
    Shrink { consumed: usize, fallback: T },
}

pub(crate) struct Prefix;

impl Prefix {
    pub fn lower<S, T>(len: usize, mut start: impl FnMut() -> S, mut unit: impl FnMut(&mut S, usize) -> Step<T>, mut build: impl FnMut(S) -> Built<T>) -> T {
        let mut limit = len;
        loop {
            let mut state = start();
            for at in 0..limit {
                match unit(&mut state, at) {
                    Step::Lowered => {}
                    Step::Stop => break,
                    Step::Done(done) => return done,
                }
            }
            match build(state) {
                Built::Done(done) => return done,
                Built::Shrink { consumed, fallback } if consumed <= 1 => return fallback,
                Built::Shrink { consumed, .. } => limit = consumed - 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Built, Prefix, Step};

    fn lower(len: usize, fits: usize, stop: Option<usize>) -> (Option<usize>, Vec<usize>) {
        let mut limits = Vec::new();
        let found = Prefix::lower(
            len,
            Vec::new,
            |taken: &mut Vec<usize>, at| match Some(at) == stop {
                true => Step::Stop,
                false => {
                    taken.push(at);
                    Step::Lowered
                }
            },
            |taken| {
                limits.push(taken.len());
                match taken.len() {
                    0 => Built::Done(None),
                    n if n <= fits => Built::Done(Some(n)),
                    n => Built::Shrink { consumed: n, fallback: None },
                }
            },
        );
        (found, limits)
    }

    #[test]
    fn shrinks_by_one_below_what_was_consumed() {
        assert_eq!(lower(5, 3, None), (Some(3), vec![5, 4, 3]));
        assert_eq!(lower(5, 2, Some(4)), (Some(2), vec![4, 3, 2]));
    }

    #[test]
    fn a_single_failing_unit_falls_back() {
        assert_eq!(lower(3, 0, None), (None, vec![3, 2, 1]));
        assert_eq!(lower(3, 3, Some(0)), (None, vec![0]));
    }

    #[test]
    fn done_returns_immediately() {
        let found = Prefix::lower(4, || (), |_, at| if at == 1 { Step::Done(7) } else { Step::Lowered }, |_| Built::Done(0));
        assert_eq!(found, 7);
    }
}
