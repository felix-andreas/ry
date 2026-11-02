//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use {
    semantics::{
        DocumentKind,
        diagnostics::{Applicability, Severity},
        lints::{LintConfig, LintLevel, NameStyle, lint_file},
        testing::with_fixture_project,
    },
    std::path::Path,
    syntax::testing::run_fixture_suite,
};

fn render_with(source: &str, config: &LintConfig) -> String {
    with_fixture_project(
        vec![(source.to_owned(), DocumentKind::Package)],
        |db, files| {
            let &[file] = files else {
                panic!("a fixture case is one file");
            };
            let mut output = String::new();
            for diagnostic in lint_file(db, file, config) {
                let severity = match diagnostic.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                };
                output.push_str(&format!(
                    "{}..{} {severity}[{}] {}\n",
                    u32::from(diagnostic.range.start()),
                    u32::from(diagnostic.range.end()),
                    diagnostic.code,
                    diagnostic.message
                ));
                if let Some(fix) = &diagnostic.fix {
                    let applicability = match fix.applicability {
                        Applicability::Safe => "safe",
                        Applicability::Unsafe => "unsafe",
                    };
                    output.push_str(&format!(
                        "  {applicability} fix {}..{} {:?}\n",
                        u32::from(fix.range.start()),
                        u32::from(fix.range.end()),
                        fix.replacement
                    ));
                }
            }
            if output.is_empty() {
                output.push_str("clean\n");
            }
            output
        },
    )
}

#[test]
fn lint_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lints");
    run_fixture_suite(&suite, &|source| {
        render_with(source, &LintConfig::default())
    });
}

#[test]
fn lint_style_fixtures() {
    let config = LintConfig {
        naming_style: Some(NameStyle::Snake),
        unused_parameter: LintLevel::Warn,
        shadows_builtin: LintLevel::Warn,
        shadows_namespace: LintLevel::Warn,
        commented_code: LintLevel::Warn,
        redundant_return: LintLevel::Warn,
        ..LintConfig::default()
    };
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lints-style");
    run_fixture_suite(&suite, &|source| render_with(source, &config));
}

#[test]
fn lint_object_systems_off_fixtures() {
    let config = LintConfig {
        object_system: LintLevel::Off,
        object_systems_disabled: true,
        ..LintConfig::default()
    };
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lints-object-systems-off");
    run_fixture_suite(&suite, &|source| render_with(source, &config));
}
