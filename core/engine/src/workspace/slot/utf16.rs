use crate::workspace::types::Span;

pub(crate) fn utf16_to_byte(text: &str, pos: u32) -> usize {
    let mut units = 0u32;
    for (byte, ch) in text.char_indices() {
        let len = ch.len_utf16() as u32;
        if pos < units + len {
            return byte;
        }
        units += len;
    }
    text.len()
}

pub(crate) fn byte_to_utf16(text: &str, byte: usize) -> u32 {
    let mut end = byte.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].encode_utf16().count() as u32
}

pub(crate) fn utf16_span(text: &str, span: Span) -> Span {
    (
        byte_to_utf16(text, span.0 as usize),
        byte_to_utf16(text, span.1 as usize),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_identity() {
        assert_eq!(utf16_to_byte("abc", 2), 2);
        assert_eq!(byte_to_utf16("abc", 2), 2);
        assert_eq!(utf16_span("abc", (1, 3)), (1, 3));
    }

    #[test]
    fn multibyte_and_surrogate_pairs_round_trip() {
        let text = "é😀x";
        assert_eq!(utf16_to_byte(text, 1), 2);
        assert_eq!(utf16_to_byte(text, 3), 6);
        assert_eq!(byte_to_utf16(text, 6), 3);
        assert_eq!(utf16_span(text, (2, 6)), (1, 3));
        assert_eq!(utf16_to_byte(text, 2), 2);
        assert_eq!(utf16_to_byte(text, 99), text.len());
        assert_eq!(byte_to_utf16(text, 99), 4);
    }
}
