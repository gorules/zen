use bumpalo::collections::String as BumpString;
use bumpalo::Bump;

#[derive(Debug)]
pub(crate) struct SanitisedString<'str>(&'str str);

impl<'str> From<&'str str> for SanitisedString<'str> {
    fn from(value: &'str str) -> Self {
        Self(value)
    }
}

impl<'str> SanitisedString<'str> {
    fn contains_escapes(&self) -> bool {
        self.0.contains('\\')
    }

    pub(crate) fn into_bump_str<'arena>(self, bump: &'arena Bump) -> &'arena str
    where
        'str: 'arena,
    {
        if !self.contains_escapes() {
            return self.0;
        }

        let mut result = BumpString::new_in(bump);
        let mut chars = self.0.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\\' {
                result.push(c);
                continue;
            }

            if let Some(&'\\') = chars.peek() {
                result.push('\\');
                chars.next();
            }
        }

        return result.into_bump_str();
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::sanitised_string::SanitisedString;
    use bumpalo::Bump;

    fn sanitise_string(data: &str) -> String {
        let bump = Bump::new();
        let string = SanitisedString::from(data).into_bump_str(&bump);

        String::from(string)
    }

    fn test_case(data: &str, expected: &str) {
        let sanitised = sanitise_string(data);
        assert_eq!(sanitised.as_str(), expected);
    }

    #[test]
    fn it_handles_varied_normal_strings() {
        test_case("1234567890", "1234567890");
        test_case("Abc123Xyz890", "Abc123Xyz890");
        test_case("!@# $%^ &*()", "!@# $%^ &*()");
        test_case("", "");
    }

    #[test]
    fn it_handles_various_strings() {
        // Test case with single backslashes that should be removed
        test_case("This is a test\\ string.", "This is a test string.");

        // Test case with double backslashes that should be reduced to single backslashes
        test_case(
            "A string with \\\\ double backslashes.",
            "A string with \\ double backslashes.",
        );

        // Test case with a mixture of single and double backslashes
        test_case(
            "Mix of \\single and \\\\double backslashes.",
            "Mix of single and \\double backslashes.",
        );

        // Test case with backslashes at the beginning and end of the string
        test_case("\\Start and end\\", "Start and end");

        // Test case with consecutive double backslashes
        test_case(
            "Consecutive \\\\\\\\ backslashes.",
            "Consecutive \\\\ backslashes.",
        );
    }

    #[test]
    fn it_removes_single_backslashes() {
        test_case(r#"\Start of string"#, r#"Start of string"#);

        // Single backslash at the end of the string
        test_case(r#"End of string\"#, r#"End of string"#);

        // Single backslashes surrounding a word
        test_case(r#"Word \with\ backslashes"#, r#"Word with backslashes"#);

        // Single backslash before a space and a character
        test_case(
            r#"Backslash before \ space and \c"#,
            r#"Backslash before  space and c"#,
        );

        // Single backslash adjacent to punctuation
        test_case(r#"Punctuation\! and \?"#, r#"Punctuation! and ?"#);

        test_case(
            r#"Double quotes \" and single quotes \'"#,
            r#"Double quotes " and single quotes '"#,
        );
    }
}
