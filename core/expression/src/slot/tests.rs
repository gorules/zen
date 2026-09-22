use super::encode_string;

#[test]
fn encode_string_picks_a_quote() {
    assert_eq!(encode_string("open").as_deref(), Some("\"open\""));
    assert_eq!(encode_string("say \"hi\"").as_deref(), Some("'say \"hi\"'"));
    assert_eq!(encode_string("it's \"x\""), None);
}
