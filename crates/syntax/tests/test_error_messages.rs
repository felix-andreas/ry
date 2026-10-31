use std::iter::once;
use std::path::Path;
use syntax::parse;
use syntax::testing::run_fixture_suite;

#[test]
fn error_messages() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/errors");
    run_fixture_suite(&suite, &render_errors);
}

fn render_errors(source: &str) -> String {
    let parsed = parse(source);
    if parsed.errors().is_empty() {
        return "no errors".to_owned();
    }

    let line_starts: Vec<usize> = once(0)
        .chain(
            source
                .char_indices()
                .filter(|&(_, character)| character == '\n')
                .map(|(at, _)| at + 1),
        )
        .collect();
    let line_of = |offset: usize| line_starts.partition_point(|&start| start <= offset) - 1;

    let mut out = String::new();
    for error in parsed.errors() {
        let start = u32::from(error.range.start()) as usize;
        let end = u32::from(error.range.end()) as usize;
        let line = line_of(start.min(source.len().saturating_sub(1)));
        let line_start = line_starts[line];
        let line_text = source[line_start..].lines().next().unwrap_or("");
        let column = source[line_start..start.min(source.len())].chars().count();
        let width = source
            .get(start..end.min(line_start + line_text.len()).max(start))
            .map_or(1, |text| text.chars().count().max(1));
        out.push_str(&format!(
            "error: {}\n --> {}:{}\n  | {line_text}\n  | {}{}\n",
            error.message,
            line + 1,
            column + 1,
            " ".repeat(column),
            "^".repeat(width)
        ));
    }
    out
}
