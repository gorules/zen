fn main() {
    #[cfg(not(feature = "arbitrary_precision"))]
    println!("cargo:warning=zen: arbitrary_precision is now opt-in across all zen crates \
        (zen-engine, zen-expression, zen-types). \
        If you rely on precise decimal handling, add features = [\"arbitrary_precision\"]. \
        See CHANGELOG for details.");
}
