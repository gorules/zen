pub struct ShortKey;

impl ShortKey {
    #[inline(always)]
    pub fn of(bytes: &[u8]) -> u64 {
        match bytes.first_chunk::<8>() {
            Some(head) => u64::from_le_bytes(*head),
            None => bytes
                .iter()
                .enumerate()
                .fold(0u64, |key, (i, b)| key | u64::from(*b) << (8 * i)),
        }
    }

    #[inline(always)]
    pub fn mask(len: usize) -> u64 {
        match len {
            0 => 0,
            1..=7 => (1u64 << (8 * len)) - 1,
            _ => u64::MAX,
        }
    }

    #[inline(always)]
    pub fn at(data: &[u8], lo: usize, len: usize) -> u64 {
        match data.get(lo..).and_then(<[u8]>::first_chunk::<8>) {
            Some(word) => u64::from_le_bytes(*word) & Self::mask(len),
            None => data.get(lo..lo + len.min(8)).map_or(0, Self::of),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ShortKey;

    #[test]
    fn short_keys_match_their_padded_prefix() {
        let data = b"abcdefghijkl";
        for lo in 0..data.len() {
            for len in 0..=data.len() - lo {
                let head = &data[lo..lo + len.min(8)];
                let mut padded = [0u8; 8];
                padded[..head.len()].copy_from_slice(head);
                assert_eq!(
                    ShortKey::at(data, lo, len),
                    u64::from_le_bytes(padded),
                    "{lo} {len}"
                );
                assert_eq!(
                    ShortKey::of(&data[lo..lo + len]),
                    u64::from_le_bytes(padded),
                    "{lo} {len}"
                );
            }
        }
    }
}
