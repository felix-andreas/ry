//! Golden error-message suite: renders ONLY the diagnostics (with source
//! context and carets), so wording and spans are pinned and reviewed as the
//! product surface they are. A case with no diagnostics renders `no errors`,
//! which pins that a tricky but valid input stays clean.

use std::fmt::Write as _;
use std::path::Path;

#[test]
fn error_messages() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/errors");
    syntax::testing::run_fixture_suite(&suite, &render_errors);
}

fn render_errors(source: &str) -> String {
    let parse = syntax::parse(source);
    if parse.errors().is_empty() {
        return "no errors".to_owned();
    }

    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            source
                .char_indices()
                .filter(|&(_, c)| c == '\n')
                .map(|(at, _)| at + 1),
        )
        .collect();
    let line_of = |offset: usize| line_starts.partition_point(|&start| start <= offset) - 1;

    let mut out = String::new();
    for error in parse.errors() {
        let start = u32::from(error.range.start()) as usize;
        let end = u32::from(error.range.end()) as usize;
        let line = line_of(start.min(source.len().saturating_sub(1)));
        let line_start = line_starts[line];
        let line_text = source[line_start..].lines().next().unwrap_or("");
        let column = source[line_start..start.min(source.len())].chars().count();
        let width = source
            .get(start..end.min(line_start + line_text.len()).max(start))
            .map(|s| s.chars().count().max(1))
            .unwrap_or(1);

        let _ = writeln!(out, "error: {}", error.message);
        let _ = writeln!(out, " --> {}:{}", line + 1, column + 1);
        let _ = writeln!(out, "  | {line_text}");
        let _ = writeln!(out, "  | {}{}", " ".repeat(column), "^".repeat(width));
    }
    out
}
