//! Golden range-formatting fixtures: each case's source carries the selection
//! as `$0` markers — two for a selection, one for a bare caret, none at all to
//! select the whole file — and the expectation lists the edits it produces and
//! shows the document they leave behind, with every replacement between `«`
//! and `»`. `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs
//! one case.
//!
//! A case source gets the trailing newline the fixture format strips, because
//! a file without one makes the formatter rewrite its last line and every
//! expectation here would carry that instead of the behavior it is about.
//! Files that genuinely lack one are covered by the property tests, which can
//! spell the text out exactly.

use format::{Config, TextEdit, format_range};
use std::fmt::Write as _;
use std::path::Path;
use syntax::{TextRange, TextSize};

const MARKER: &str = "$0";

fn render(source: &str) -> String {
    let (mut text, selection) = split_markers(source);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let edits = match format_range(&text, Config::default(), selection) {
        Ok(edits) => edits,
        Err(error) => return format!("ERROR: {error}"),
    };
    if edits.is_empty() {
        return "no edits".to_owned();
    }
    let mut output = String::new();
    for edit in &edits {
        let _ = writeln!(
            output,
            "edit {}-{}",
            position(&text, edit.range.start()),
            position(&text, edit.range.end())
        );
    }
    output.push_str("----\n");
    output.push_str(&marked(&text, &edits));
    output
}

/// The document the edits produce, with each replacement between `«` and `»`.
/// A replacement's closing marker goes before its last line break so that it
/// sits at the end of the text it marks rather than alone on the next line.
fn marked(text: &str, edits: &[TextEdit]) -> String {
    let mut output = String::new();
    let mut cursor = 0usize;
    for edit in edits {
        output.push_str(&text[cursor..usize::from(edit.range.start())]);
        output.push('«');
        match edit.new_text.strip_suffix('\n') {
            Some(body) => {
                output.push_str(body.strip_suffix('\r').unwrap_or(body));
                output.push('»');
                output.push('\n');
            }
            None => {
                output.push_str(&edit.new_text);
                output.push('»');
            }
        }
        cursor = usize::from(edit.range.end());
    }
    output.push_str(&text[cursor..]);
    output
}

/// The `$0` markers, stripped from the source: two mark a selection, one a
/// bare caret, and none selects the whole file.
fn split_markers(source: &str) -> (String, TextRange) {
    let mut text = source.to_owned();
    let mut offsets = Vec::new();
    while let Some(at) = text.find(MARKER) {
        text.replace_range(at..at + MARKER.len(), "");
        offsets.push(TextSize::new(at as u32));
    }
    let selection = match offsets[..] {
        [] => TextRange::up_to(TextSize::of(text.as_str())),
        [caret] => TextRange::empty(caret),
        [start, end, ..] => TextRange::new(start, end),
    };
    (text, selection)
}

/// A one-based line and character column, the way the CLI reports a position.
fn position(text: &str, offset: TextSize) -> String {
    let offset = usize::from(offset).min(text.len());
    let line = text[..offset].matches('\n').count();
    let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    format!(
        "{}:{}",
        line + 1,
        text[line_start..offset].chars().count() + 1
    )
}

#[test]
fn format_range_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/format-range");
    syntax::testing::run_fixture_suite(&suite, &render);
}
