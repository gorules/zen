use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

pub(crate) fn unaccent(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        match c {
            'ß' => out.push_str("ss"),
            'ẞ' => out.push_str("SS"),
            'ø' => out.push('o'),
            'Ø' => out.push('O'),
            'ł' => out.push('l'),
            'Ł' => out.push('L'),
            'đ' | 'ð' => out.push('d'),
            'Đ' | 'Ð' => out.push('D'),
            'æ' => out.push_str("ae"),
            'Æ' => out.push_str("AE"),
            'œ' => out.push_str("oe"),
            'Œ' => out.push_str("OE"),
            'þ' => out.push_str("th"),
            'Þ' => out.push_str("Th"),
            'ı' => out.push('i'),
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn token_sort(s: &str) -> String {
    let mut t = tokens(s);
    t.sort();
    t.join(" ")
}

pub(crate) fn name_key(s: &str) -> String {
    token_sort(&unaccent(&s.to_lowercase()))
}

pub(crate) fn jaro_winkler(a: &str, b: &str) -> f64 {
    strsim::jaro_winkler(a, b)
}

pub(crate) fn token_set_similarity(a: &str, b: &str) -> f64 {
    let a: std::collections::BTreeSet<String> = tokens(a).into_iter().collect();
    let b: std::collections::BTreeSet<String> = tokens(b).into_iter().collect();
    match (a.is_empty(), b.is_empty()) {
        (true, true) => 1.0,
        (true, false) | (false, true) => 0.0,
        _ => {
            let shared = a.intersection(&b).count();
            let all = a.union(&b).count();
            shared as f64 / all as f64
        }
    }
}

pub(crate) fn soundex(s: &str) -> String {
    fn code(c: char) -> u8 {
        match c {
            'B' | 'F' | 'P' | 'V' => b'1',
            'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => b'2',
            'D' | 'T' => b'3',
            'L' => b'4',
            'M' | 'N' => b'5',
            'R' => b'6',
            'H' | 'W' => b'-',
            _ => b'0',
        }
    }
    let letters: Vec<char> = unaccent(s)
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let Some(&first) = letters.first() else {
        return String::new();
    };
    let mut out = String::with_capacity(4);
    out.push(first);
    let mut last = code(first);
    for &c in &letters[1..] {
        let k = code(c);
        match k {
            b'-' => continue,
            b'0' => last = b'0',
            k if k != last => {
                out.push(k as char);
                last = k;
                if out.len() == 4 {
                    break;
                }
            }
            _ => {}
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unaccent_folds_latin_diacritics_and_keeps_other_scripts() {
        assert_eq!(unaccent("Kovačić Müller Ærø"), "Kovacic Muller AEro");
        assert_eq!(
            unaccent("Straße Łódź Đorđe Œuvre"),
            "Strasse Lodz Dorde OEuvre"
        );
        assert_eq!(unaccent("ﬁle Ａ"), "file A");
        assert_eq!(unaccent("Анна Ἀθῆναι"), "Анна Αθηναι");
        assert_eq!(unaccent(""), "");
    }

    #[test]
    fn squish_trims_and_collapses_whitespace() {
        assert_eq!(squish("  Anna \t  Kovacic\n"), "Anna Kovacic");
        assert_eq!(squish("   "), "");
    }

    #[test]
    fn token_sort_and_name_key_ignore_order_case_punctuation_and_accents() {
        assert_eq!(token_sort("Kovacic, Anna"), "anna kovacic");
        assert_eq!(token_sort("  anna   KOVACIC "), "anna kovacic");
        assert_eq!(name_key("Kovačić, Anna"), "anna kovacic");
        assert_eq!(name_key("ANNA-KOVAČIĆ"), "anna kovacic");
        assert_eq!(name_key("Hauptstraße 5, 2. OG"), "2 5 hauptstrasse og");
        assert_eq!(name_key("İstanbul"), "istanbul");
        assert_eq!(name_key("!!"), "");
    }

    #[test]
    fn similarities_are_between_zero_and_one() {
        assert_eq!(jaro_winkler("anna", "anna"), 1.0);
        assert!(jaro_winkler("kovacic", "kovacich") > 0.9);
        assert!(jaro_winkler("anna", "zorn") < 0.6);
        assert_eq!(token_set_similarity("Anna Kovacic", "KOVACIC anna"), 1.0);
        assert_eq!(token_set_similarity("anna kovacic", "anna"), 0.5);
        assert_eq!(token_set_similarity("", " , "), 1.0);
        assert_eq!(token_set_similarity("anna", ""), 0.0);
    }

    #[test]
    fn soundex_codes_names_by_sound() {
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(soundex("Tymczak"), "T522");
        assert_eq!(soundex("Pfister"), "P236");
        assert_eq!(soundex("Meyer"), soundex("Maier"));
        assert_eq!(soundex("Müller"), soundex("Muller"));
        assert_eq!(soundex("A"), "A000");
        assert_eq!(soundex("123"), "");
    }
}
