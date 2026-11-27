use std::sync::Arc;

use swc_common::errors::{DiagnosticBuilder, Emitter, Handler};
use swc_common::input::StringInput;
use swc_common::source_map::SmallPos;
use swc_common::sync::Lrc;
use swc_common::{BytePos, SourceMap, GLOBALS};
use swc_ecma_parser::{lexer::Lexer, EsSyntax, Parser, Syntax};
use swc_ts_fast_strip::{operate, Mode, Options};

pub struct TypeStripper;

struct SilentEmitter;

impl Emitter for SilentEmitter {
    fn emit(&mut self, _db: &mut DiagnosticBuilder) {}
}

impl TypeStripper {
    pub fn strip(source: &str) -> Arc<str> {
        GLOBALS.set(&Default::default(), || {
            if let Some(stripped) =
                Self::operate_with(source, Mode::StripOnly).filter(|code| Self::parses_as_js(code))
            {
                return Arc::from(stripped);
            }
            Self::operate_with(source, Mode::Transform)
                .map(Arc::from)
                .unwrap_or_else(|| Arc::from(source))
        })
    }

    fn parses_as_js(source: &str) -> bool {
        let lexer = Lexer::new(
            Syntax::Es(EsSyntax::default()),
            Default::default(),
            StringInput::new(
                source,
                BytePos::from_usize(0),
                BytePos::from_usize(source.len()),
            ),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let parsed = parser.parse_module();
        parsed.is_ok() && parser.take_errors().is_empty()
    }

    fn operate_with(source: &str, mode: Mode) -> Option<String> {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_emitter(true, false, Box::new(SilentEmitter));
        let options = Options {
            module: Some(true),
            mode,
            ..Default::default()
        };
        operate(&cm, &handler, source.to_string(), options)
            .ok()
            .map(|output| output.code)
    }
}

#[cfg(test)]
mod tests {
    use super::TypeStripper;

    #[test]
    fn strips_type_annotations_preserving_positions() {
        let source = "export const handler = async (input: { age: number }): Promise<{ total: number }> => {\n  return { total: input.age * 2 };\n};";
        let stripped = TypeStripper::strip(source);
        assert!(!stripped.contains("number"), "{stripped}");
        assert!(
            stripped.contains("return { total: input.age * 2 };"),
            "{stripped}"
        );
        assert_eq!(source.lines().count(), stripped.lines().count());
        for (original, result) in source.lines().zip(stripped.lines()) {
            assert_eq!(original.chars().count(), result.chars().count());
        }
    }

    #[test]
    fn plain_javascript_is_untouched() {
        let source = "export const handler = async (input) => ({ total: input.age * 2 });";
        let stripped = TypeStripper::strip(source);
        assert_eq!(stripped.as_ref(), source);
    }

    #[test]
    fn interfaces_and_aliases_are_erased() {
        let source = "interface Input { age: number }\ntype Out = { total: number };\nexport const handler = (input: Input): Out => ({ total: input.age });";
        let stripped = TypeStripper::strip(source);
        assert!(!stripped.contains("interface"), "{stripped}");
        assert!(stripped.contains("({ total: input.age })"), "{stripped}");
    }

    #[test]
    fn broken_source_passes_through() {
        let source = "export const handler = (input => {";
        let stripped = TypeStripper::strip(source);
        assert_eq!(stripped.as_ref(), source);
    }

    #[test]
    fn enums_are_lowered_via_transform_mode() {
        let source = "enum Tier { Gold = 'gold', Basic = 'basic' }\nexport const handler = async (input: { vip: boolean }) => ({ tier: input.vip ? Tier.Gold : Tier.Basic });";
        let stripped = TypeStripper::strip(source);
        assert!(!stripped.contains("enum Tier"), "{stripped}");
        assert!(stripped.contains("Tier"), "{stripped}");
        assert!(!stripped.contains(": boolean"), "{stripped}");
    }
}
