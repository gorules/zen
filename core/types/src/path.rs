pub struct DotPath;

impl DotPath {
    #[inline]
    pub fn tail<'a>(long: &'a str, short: &str) -> Option<&'a str> {
        long.strip_prefix(short)?.strip_prefix('.')
    }

    #[inline]
    pub fn under(long: &str, short: &str) -> bool {
        Self::tail(long, short).is_some()
    }

    #[inline]
    pub fn within(long: &str, short: &str) -> bool {
        long == short || Self::under(long, short)
    }

    #[inline]
    pub fn nested(a: &str, b: &str) -> bool {
        Self::under(a, b) || Self::under(b, a)
    }

    #[inline]
    pub fn overlaps(a: &str, b: &str) -> bool {
        a == b || Self::nested(a, b)
    }

    #[inline]
    pub fn indexed(long: &str, short: &str) -> bool {
        long.strip_prefix(short)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
    }
}

#[cfg(test)]
mod tests {
    use super::DotPath;

    #[test]
    fn dotted_relations() {
        assert_eq!(DotPath::tail("a.b.c", "a"), Some("b.c"));
        assert_eq!(DotPath::tail("ab", "a"), None);
        assert!(DotPath::under("a.b", "a"));
        assert!(!DotPath::under("ab", "a"));
        assert!(!DotPath::under("a", "a"));
        assert!(DotPath::within("a", "a"));
        assert!(DotPath::within("a.b.c", "a.b"));
        assert!(!DotPath::within("a.bc", "a.b"));
        assert!(DotPath::nested("a", "a.b") && DotPath::nested("a.b", "a"));
        assert!(!DotPath::nested("a", "a"));
        assert!(
            DotPath::overlaps("a", "a")
                && DotPath::overlaps("a.b", "a")
                && !DotPath::overlaps("a", "b")
        );
        assert!(DotPath::indexed("a[0]", "a") && DotPath::indexed("a.b", "a"));
        assert!(!DotPath::indexed("ab", "a"));
    }
}
