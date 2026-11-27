pub(crate) struct Density;

impl Density {
    pub(crate) fn sparse(local: usize, rows: usize) -> bool {
        local * 4 < rows
    }
}

#[cfg(test)]
mod tests {
    use super::Density;

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
