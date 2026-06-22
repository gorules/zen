use serde::Deserialize;
use serde_json::Value;
use zen_expression::intellisense::dependency::{ReadDependency, Reference};

use zen_expression::intellisense::diagnostic::{DiagnosticSource, Severity};
use zen_expression::intellisense::{ExpressionAnalysis, IntelliSense};
use zen_expression::variable::VariableType;

#[derive(Debug, Deserialize)]
struct TestFile {
    test: Vec<TestCase>,
}

#[derive(Debug, Deserialize)]
struct TestCase {
    name: String,
    expression: String,
    input: Option<String>,
    return_type: Option<String>,
    reads: Option<Vec<ReadDependency>>,
    reads_with_spans: Option<Vec<ReadDependency>>,
    references: Option<Vec<Reference>>,
    diagnostics: Option<Vec<ExpectedDiagnostic>>,
    #[serde(default)]
    unary: bool,
    #[serde(default)]
    strict: StrictField,
    loose: Option<ModeExpectations>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum StrictField {
    #[default]
    Absent,
    Bool(bool),
    Expectations(ModeExpectations),
}

impl StrictField {
    fn as_bool(&self) -> bool {
        matches!(self, StrictField::Bool(true))
    }

    fn as_expectations(&self) -> Option<&ModeExpectations> {
        match self {
            StrictField::Expectations(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct ModeExpectations {
    return_type: Option<String>,
    diagnostics: Option<Vec<ExpectedDiagnostic>>,
}

#[derive(Debug, Deserialize)]
struct ExpectedDiagnostic {
    source: String,
    severity: String,
}

fn parse_data_type(test: &TestCase) -> VariableType {
    match &test.input {
        Some(input_str) => {
            // Try parsing as VariableType directly (for enum types etc.),
            // fall back to JSON Value conversion
            serde_json::from_str::<VariableType>(input_str).unwrap_or_else(|_| {
                let input: Value = serde_json5::from_str(input_str)
                    .unwrap_or_else(|e| panic!("[{}] Failed to parse input: {e}", test.name));
                VariableType::from(input)
            })
        }
        None => VariableType::Any,
    }
}

fn run_analysis_inner(test: &TestCase, strict: bool, unary: bool) -> ExpressionAnalysis {
    let mut is = IntelliSense::new().with_strict(strict);
    let data_type = parse_data_type(test);
    let rc = if unary {
        is.analyze_unary(&test.expression, &data_type)
    } else {
        is.analyze(&test.expression, &data_type)
    };
    std::rc::Rc::unwrap_or_clone(rc)
}

fn parse_diagnostic_source(s: &str) -> DiagnosticSource {
    match s {
        "lexer" => DiagnosticSource::Lexer,
        "parser" => DiagnosticSource::Parser,
        "type_check" => DiagnosticSource::TypeCheck,
        "compiler" => DiagnosticSource::Compiler,
        other => panic!("Unknown diagnostic source: {other}"),
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        other => panic!("Unknown severity: {other}"),
    }
}

fn run_test_file(file_name: &str, toml_data: &str) {
    run_test_file_inner(file_name, toml_data, false);
}

fn run_test_file_unary(file_name: &str, toml_data: &str) {
    run_test_file_inner(file_name, toml_data, true);
}

fn run_test_file_inner(file_name: &str, toml_data: &str, force_unary: bool) {
    let test_file: TestFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("Failed to parse {file_name}: {e}"));

    for test in &test_file.test {
        let unary = test.unary || force_unary;
        let has_mode_sections = test.loose.is_some() || test.strict.as_expectations().is_some();

        if has_mode_sections {
            if let Some(loose) = &test.loose {
                let result = run_analysis_inner(test, false, unary);
                check_mode(file_name, test, &result, loose, "loose");
            }
            if let Some(strict) = test.strict.as_expectations() {
                let result = run_analysis_inner(test, true, unary);
                check_mode(file_name, test, &result, strict, "strict");
            }
        } else {
            let result = run_analysis_inner(test, test.strict.as_bool(), unary);
            check_result(file_name, test, &result);
        }
    }
}

fn check_mode(
    file_name: &str,
    test: &TestCase,
    result: &ExpressionAnalysis,
    expected: &ModeExpectations,
    mode: &str,
) {
    if let Some(expected_rt) = &expected.return_type {
        let expected_type: VariableType = serde_json::from_str(expected_rt).unwrap_or_else(|e| {
            panic!(
                "[{file_name}:{}:{mode}] Failed to parse return_type '{}': {e}",
                test.name, expected_rt
            )
        });
        assert_eq!(
            result.return_type, expected_type,
            "[{file_name}:{}:{mode}] return_type mismatch.\n  Expression: {}\n  Expected: {:?}\n  Got: {:?}",
            test.name, test.expression, expected_type, result.return_type
        );
    }

    if let Some(expected_reads) = &test.reads {
        let actual_stripped: Vec<_> = result.reads.iter().map(|r| r.without_spans()).collect();
        let expected_stripped: Vec<_> = expected_reads.iter().map(|r| r.without_spans()).collect();
        assert_eq!(
            actual_stripped,
            expected_stripped,
            "[{file_name}:{}:{mode}] reads mismatch.\n  Expression: {}\n  Expected: {}\n  Got: {}",
            test.name,
            test.expression,
            serde_json::to_string_pretty(expected_reads).unwrap(),
            serde_json::to_string_pretty(&result.reads).unwrap()
        );
    }

    if let Some(expected_reads) = &test.reads_with_spans {
        assert_eq!(
            &result.reads, expected_reads,
            "[{file_name}:{}:{mode}] reads_with_spans mismatch.\n  Expression: {}\n  Expected: {}\n  Got: {}",
            test.name, test.expression,
            serde_json::to_string_pretty(expected_reads).unwrap(),
            serde_json::to_string_pretty(&result.reads).unwrap()
        );
    }

    if let Some(expected_refs) = &test.references {
        let actual: Vec<_> = result
            .references
            .iter()
            .map(|r| r.without_via_alias())
            .collect();
        let expected: Vec<_> = expected_refs
            .iter()
            .map(|r| r.without_via_alias())
            .collect();
        assert_eq!(
            actual, expected,
            "[{file_name}:{}:{mode}] references mismatch.\n  Expression: {}\n  Expected: {}\n  Got: {}",
            test.name, test.expression,
            serde_json::to_string_pretty(expected_refs).unwrap(),
            serde_json::to_string_pretty(&result.references).unwrap()
        );
    }

    if let Some(expected_diags) = &expected.diagnostics {
        check_diagnostics(file_name, test, result, expected_diags, mode);
    } else if expected.return_type.is_some() || test.reads.is_some() {
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "[{file_name}:{}:{mode}] Expected no error diagnostics, but got: {:?}\n  Expression: {}",
            test.name, errors, test.expression
        );
    }
}

fn check_result(file_name: &str, test: &TestCase, result: &ExpressionAnalysis) {
    let mode = if test.strict.as_bool() {
        "strict"
    } else {
        "loose"
    };

    if let Some(expected_rt) = &test.return_type {
        let expected: VariableType = serde_json::from_str(expected_rt).unwrap_or_else(|e| {
            panic!(
                "[{file_name}:{}] Failed to parse return_type '{}': {e}",
                test.name, expected_rt
            )
        });
        assert_eq!(
            result.return_type, expected,
            "[{file_name}:{}:{mode}] return_type mismatch.\n  Expression: {}\n  Expected: {:?}\n  Got: {:?}",
            test.name, test.expression, expected, result.return_type
        );
    }

    if let Some(expected_reads) = &test.reads {
        let actual_stripped: Vec<_> = result.reads.iter().map(|r| r.without_spans()).collect();
        let expected_stripped: Vec<_> = expected_reads.iter().map(|r| r.without_spans()).collect();
        assert_eq!(
            actual_stripped,
            expected_stripped,
            "[{file_name}:{}] reads mismatch.\n  Expression: {}\n  Expected: {}\n  Got: {}",
            test.name,
            test.expression,
            serde_json::to_string_pretty(expected_reads).unwrap(),
            serde_json::to_string_pretty(&result.reads).unwrap()
        );
    }

    if let Some(expected_reads) = &test.reads_with_spans {
        assert_eq!(
            &result.reads,
            expected_reads,
            "[{file_name}:{}] reads_with_spans mismatch.\n  Expression: {}\n  Expected: {}\n  Got: {}",
            test.name,
            test.expression,
            serde_json::to_string_pretty(expected_reads).unwrap(),
            serde_json::to_string_pretty(&result.reads).unwrap()
        );
    }

    if let Some(expected_refs) = &test.references {
        assert_eq!(
            &result.references,
            expected_refs,
            "[{file_name}:{}] references mismatch.\n  Expression: {}\n  Expected: {}\n  Got: {}",
            test.name,
            test.expression,
            serde_json::to_string_pretty(expected_refs).unwrap(),
            serde_json::to_string_pretty(&result.references).unwrap()
        );
    }

    if let Some(expected_diags) = &test.diagnostics {
        check_diagnostics(file_name, test, result, expected_diags, mode);
    } else if test.reads.is_some() || test.return_type.is_some() {
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "[{file_name}:{}:{mode}] Expected no error diagnostics, but got: {:?}\n  Expression: {}",
            test.name, errors, test.expression
        );
    }
}

fn check_diagnostics(
    file_name: &str,
    test: &TestCase,
    result: &ExpressionAnalysis,
    expected_diags: &[ExpectedDiagnostic],
    mode: &str,
) {
    assert!(
        result.diagnostics.len() >= expected_diags.len(),
        "[{file_name}:{}:{mode}] Expected at least {} diagnostics, got {}.\n  Expression: {}\n  Diagnostics: {:?}",
        test.name, expected_diags.len(), result.diagnostics.len(), test.expression, result.diagnostics
    );

    for (i, expected) in expected_diags.iter().enumerate() {
        let expected_source = parse_diagnostic_source(&expected.source);
        let expected_severity = parse_severity(&expected.severity);

        let matching = result
            .diagnostics
            .iter()
            .any(|d| d.source == expected_source && d.severity == expected_severity);

        assert!(
            matching,
            "[{file_name}:{}:{mode}] Diagnostic #{i} not found: expected source={}, severity={}.\n  Expression: {}\n  Got diagnostics: {:?}",
            test.name, expected.source, expected.severity, test.expression, result.diagnostics
        );
    }
}

#[test]
fn analysis_types() {
    run_test_file("types", include_str!("data/analysis/types.toml"));
}

#[test]
fn analysis_dependencies() {
    run_test_file(
        "dependencies",
        include_str!("data/analysis/dependencies.toml"),
    );
}

#[test]
fn analysis_closures() {
    run_test_file("closures", include_str!("data/analysis/closures.toml"));
}

#[test]
fn analysis_functions() {
    run_test_file("functions", include_str!("data/analysis/functions.toml"));
}

#[test]
fn analysis_diagnostics() {
    run_test_file(
        "diagnostics",
        include_str!("data/analysis/diagnostics.toml"),
    );
}

#[test]
fn analysis_references() {
    run_test_file("references", include_str!("data/analysis/references.toml"));
}

#[test]
fn analysis_edge_cases() {
    run_test_file("edge_cases", include_str!("data/analysis/edge_cases.toml"));
}

#[derive(Debug, Deserialize)]
struct CompletionTestFile {
    test: Vec<CompletionTestCase>,
}

#[derive(Debug, Deserialize)]
struct CompletionTestCase {
    name: String,
    expression: String,
    pos: u32,
    input: Option<String>,
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    excludes: Vec<String>,
}

fn run_completion_test_file(file_name: &str, toml_data: &str) {
    let test_file: CompletionTestFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("Failed to parse {file_name}: {e}"));

    for test in &test_file.test {
        let mut is = IntelliSense::new();

        let data_type = match &test.input {
            Some(input_str) => {
                let input: Value = serde_json5::from_str(input_str).unwrap_or_else(|e| {
                    panic!("[{file_name}:{}] Failed to parse input: {e}", test.name)
                });
                VariableType::from(input)
            }
            None => VariableType::Any,
        };

        let completions = is.completions(&test.expression, test.pos, &data_type);
        let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();

        for inc in &test.includes {
            assert!(
                labels.contains(&inc.as_str()),
                "[{file_name}:{}] Expected completion '{}' not found.\n  Expression: {:?} @ pos {}\n  Got: {:?}",
                test.name, inc, test.expression, test.pos, labels
            );
        }

        for exc in &test.excludes {
            assert!(
                !labels.contains(&exc.as_str()),
                "[{file_name}:{}] Unexpected completion '{}' found.\n  Expression: {:?} @ pos {}\n  Got: {:?}",
                test.name, exc, test.expression, test.pos, labels
            );
        }
    }
}

#[test]
fn analysis_completions() {
    run_completion_test_file(
        "completions",
        include_str!("data/analysis/completions.toml"),
    );
}

#[derive(Debug, Deserialize)]
struct InspectTestFile {
    test: Vec<InspectTestCase>,
}

#[derive(Debug, Deserialize)]
struct InspectTestCase {
    name: String,
    expression: String,
    pos: u32,
    input: Option<String>,
    label: Option<String>,
    kind: Option<String>,
}

fn run_inspect_test_file(file_name: &str, toml_data: &str) {
    let test_file: InspectTestFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("Failed to parse {file_name}: {e}"));

    for test in &test_file.test {
        let mut is = IntelliSense::new();

        let data_type = match &test.input {
            Some(input_str) => {
                let input: Value = serde_json5::from_str(input_str).unwrap_or_else(|e| {
                    panic!("[{file_name}:{}] Failed to parse input: {e}", test.name)
                });
                VariableType::from(input)
            }
            None => VariableType::Any,
        };

        let result = is.inspect(&test.expression, test.pos, &data_type);

        if test.label.is_some() || test.kind.is_some() {
            let result = result.unwrap_or_else(|| {
                panic!(
                    "[{file_name}:{}] Expected inspect result, got None.\n  Expression: {:?} @ pos {}",
                    test.name, test.expression, test.pos
                )
            });

            if let Some(expected_label) = &test.label {
                assert_eq!(
                    &result.label, expected_label,
                    "[{file_name}:{}] label mismatch.\n  Expression: {:?} @ pos {}",
                    test.name, test.expression, test.pos
                );
            }

            if let Some(expected_kind) = &test.kind {
                let expected: VariableType =
                    serde_json::from_str(expected_kind).unwrap_or_else(|e| {
                        panic!(
                            "[{file_name}:{}] Failed to parse kind '{}': {e}",
                            test.name, expected_kind
                        )
                    });
                assert_eq!(
                    result.kind, expected,
                    "[{file_name}:{}] kind mismatch.\n  Expression: {:?} @ pos {}",
                    test.name, test.expression, test.pos
                );
            }
        }
    }
}

#[test]
fn analysis_inspect() {
    run_inspect_test_file("inspect", include_str!("data/analysis/inspect.toml"));
}

#[test]
fn analysis_unary() {
    run_test_file_unary("unary", include_str!("data/analysis/unary.toml"));
}

#[test]
fn analysis_enums() {
    run_test_file("enums", include_str!("data/analysis/enums.toml"));
}

#[test]
fn analysis_read_spans() {
    run_test_file("read_spans", include_str!("data/analysis/read_spans.toml"));
}

#[test]
fn analysis_nullable() {
    run_test_file("nullable", include_str!("data/analysis/nullable.toml"));
}

#[test]
#[cfg_attr(miri, ignore)]
fn analysis_nested_closures_complete_quickly() {
    let depth = 25;
    let mut source = String::new();
    (0..depth).for_each(|_| source.push_str("map([0], "));
    source.push('1');
    (0..depth).for_each(|_| source.push(')'));

    let start = std::time::Instant::now();
    let mut is = IntelliSense::new().with_strict(true);
    let analysis = is.analyze(&source, &VariableType::Any);
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "nested closure type-check took {:?}",
        start.elapsed()
    );

    let errors: Vec<_> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "unexpected diagnostics: {errors:?}");
}

#[test]
fn analysis_nested_closure_shallow_diagnostics_unchanged() {
    let input = VariableType::from(serde_json::json!({"items": [1, 2]}));
    let mut is = IntelliSense::new().with_strict(true);
    let analysis = is.analyze("filter(items, # + 1)", &input);
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error && d.source == DiagnosticSource::TypeCheck),
        "expected callback return-type error, got: {:?}",
        analysis.diagnostics
    );
}

#[test]
fn analysis_overlapping_enum_equality_has_no_hint() {
    let input: VariableType = serde_json::from_str(
        r#"{"Object":{"a":{"Enum":[null,["a","b"]]},"b":{"Enum":[null,["b","c"]]}}}"#,
    )
    .unwrap();

    for strict in [false, true] {
        for expression in ["a == b", "a != b"] {
            let mut is = IntelliSense::new().with_strict(strict);
            let analysis = is.analyze(expression, &input);
            assert!(
                analysis.diagnostics.is_empty(),
                "[{expression}:strict={strict}] expected no diagnostics, got: {:?}",
                analysis.diagnostics
            );
        }
    }
}

#[test]
fn analysis_structured_equality_hint_fires() {
    let input: VariableType = serde_json::from_str(
        r#"{"Object":{"a":{"Object":{"x":"Number"}},"b":{"Object":{"x":"Number"}},"c":{"Array":"Number"},"d":{"Array":"Number"}}}"#,
    )
    .unwrap();

    for strict in [false, true] {
        for expression in ["a == b", "a != b", "c == d", "c != d"] {
            let mut is = IntelliSense::new().with_strict(strict);
            let analysis = is.analyze(expression, &input);
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == Severity::Warning
                        && d.source == DiagnosticSource::TypeCheck),
                "[{expression}:strict={strict}] expected always-constant hint, got: {:?}",
                analysis.diagnostics
            );
        }
    }
}

#[test]
fn analysis_nullable_structured_equality_no_hint() {
    let input: VariableType = serde_json::from_str(
        r#"{"Object":{"a":{"Nullable":{"Object":{"x":"Number"}}},"b":{"Nullable":{"Object":{"x":"Number"}}},"c":{"Object":{"x":"Number"}}}}"#,
    )
    .unwrap();

    for strict in [false, true] {
        for expression in ["a == b", "a != b", "c == null", "c != null", "a == c"] {
            let mut is = IntelliSense::new().with_strict(strict);
            let analysis = is.analyze(expression, &input);
            assert!(
                analysis.diagnostics.is_empty(),
                "[{expression}:strict={strict}] expected no diagnostics, got: {:?}",
                analysis.diagnostics
            );
        }
    }
}

#[test]
fn analysis_disjoint_enum_equality_hint_fires() {
    let input: VariableType = serde_json::from_str(
        r#"{"Object":{"a":{"Enum":[null,["a","b"]]},"b":{"Enum":[null,["c","d"]]}}}"#,
    )
    .unwrap();

    for strict in [false, true] {
        for expression in ["a == b", "a != b"] {
            let mut is = IntelliSense::new().with_strict(strict);
            let analysis = is.analyze(expression, &input);
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == Severity::Warning
                        && d.source == DiagnosticSource::TypeCheck),
                "[{expression}:strict={strict}] expected always-constant hint, got: {:?}",
                analysis.diagnostics
            );
        }
    }
}
