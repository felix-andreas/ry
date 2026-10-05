//! Range-formatting property tests: the invariant battery over every R source
//! in the repository, every byte selection of a few short sources against an
//! independent line oracle, plus the texts the fixture format cannot spell —
//! ones without a trailing newline, with CRLF endings, and the empty file.

use format::check_range_format_invariants as check_invariants;
use format::{Config, TextEdit, apply_edits, format, format_range};
use syntax::{TextRange, TextSize};

#[test]
fn fixture_sources_hold_range_invariants() {
    for (id, source) in syntax::testing::fixture_case_sources() {
        std::panic::catch_unwind(|| check_invariants(&source))
            .unwrap_or_else(|_| panic!("fixture case `{id}` broke a range-format invariant"));
    }
}

#[test]
fn legacy_corpus_holds_range_invariants() {
    let sources = syntax::testing::legacy_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    for (id, source) in sources {
        std::panic::catch_unwind(|| check_invariants(&source))
            .unwrap_or_else(|_| panic!("legacy corpus case `{id}` broke a range-format invariant"));
    }
}

/// The edits for the line `caret` sits on.
fn edits_at(source: &str, caret: usize) -> Vec<TextEdit> {
    format_range(
        source,
        Config::default(),
        TextRange::empty(TextSize::new(caret as u32)),
    )
    .expect("the source formats")
}

#[test]
fn the_last_line_of_a_file_without_a_trailing_newline_gains_one() {
    let source = "x <- 1\ny<-2";
    let edits = edits_at(source, source.len());
    assert_eq!(
        edits,
        [TextEdit {
            range: TextRange::new(TextSize::new(7), TextSize::new(11)),
            new_text: "y <- 2\n".to_owned(),
        }]
    );
    assert_eq!(apply_edits(source, &edits), "x <- 1\ny <- 2\n");
}

#[test]
fn a_line_above_a_missing_trailing_newline_does_not_gain_one() {
    // The newline belongs to the last line's own edit, so selecting an earlier
    // line must not reach it.
    let source = "x<-1\ny <- 2";
    let edits = edits_at(source, 0);
    assert_eq!(
        edits,
        [TextEdit {
            range: TextRange::new(TextSize::new(0), TextSize::new(5)),
            new_text: "x <- 1\n".to_owned(),
        }]
    );
    assert_eq!(apply_edits(source, &edits), "x <- 1\ny <- 2");
}

#[test]
fn replacement_text_carries_the_documents_line_endings() {
    let source = "x <- 1\r\ny<-2\r\n";
    let edits = edits_at(source, 9);
    assert_eq!(
        edits,
        [TextEdit {
            range: TextRange::new(TextSize::new(8), TextSize::new(14)),
            new_text: "y <- 2\r\n".to_owned(),
        }]
    );
    assert_eq!(apply_edits(source, &edits), "x <- 1\r\ny <- 2\r\n");
}

#[test]
fn an_empty_document_has_nothing_to_lay_out() {
    assert_eq!(edits_at("", 0), []);
    assert_eq!(
        format_range("", Config::default(), TextRange::up_to(TextSize::new(0))),
        Ok(Vec::new())
    );
}

#[test]
fn a_document_that_does_not_parse_refuses_like_whole_file_formatting() {
    let source = "x <- (\ny <- 2\n";
    assert_eq!(
        format_range(
            source,
            Config::default(),
            TextRange::up_to(TextSize::new(6))
        )
        .unwrap_err(),
        format(source, Config::default()).unwrap_err()
    );
}

#[test]
fn selecting_everything_reproduces_whole_file_formatting() {
    let source = "f<-function(x){\n  a<-1\n\n\n  b<-2\n}\nx<-1;y<-2\n";
    let whole = TextRange::up_to(TextSize::of(source));
    let edits = format_range(source, Config::default(), whole).expect("the source formats");
    assert_eq!(
        apply_edits(source, &edits),
        format(source, Config::default()).expect("the source formats")
    );
}

#[test]
fn auto_line_endings_ignore_the_blank_lines_formatting_removes() {
    // Deciding from the leading blank lines would pick LF here, and the second
    // pass, which no longer sees them, CRLF: the same file would format two
    // ways depending on how much of it was formatted before.
    let source = "\n\nx<-1\r\ny<-2\r\n";
    let formatted = format(source, Config::default()).expect("the source formats");
    assert_eq!(formatted, "x <- 1\r\ny <- 2\r\n");
    assert_eq!(format(&formatted, Config::default()), Ok(formatted.clone()));
    assert_eq!(
        edits_at(source, 8),
        [TextEdit {
            range: TextRange::new(TextSize::new(8), TextSize::new(14)),
            new_text: "y <- 2\r\n".to_owned(),
        }]
    );
}

#[test]
fn auto_line_endings_of_a_file_without_code_come_from_its_first_line_break() {
    for source in ["\r\n", ";\r\n", "\r\n\r\nx"] {
        let formatted = format(source, Config::default()).expect("the source formats");
        assert!(
            formatted.ends_with("\r\n"),
            "{source:?} formatted to {formatted:?}"
        );
        assert_eq!(format(&formatted, Config::default()), Ok(formatted));
    }
}

/// Every selection, including ones that start or end mid-character and ones
/// past the end, makes exactly the edits of the lines it touches — checked
/// against an oracle that finds those lines by scanning for `\n` itself.
#[test]
fn every_byte_selection_takes_exactly_the_lines_it_touches() {
    let sources = [
        "x<-1\ny<-2",
        "x<-1\r\n\r\ny<-2\r\n",
        "f(a,\n  b)\n\n# c\nz<-'é🙂'\n",
        "{\nx<-1\n}\n\n\nw<-2\r",
    ];
    for source in sources {
        let starts: Vec<usize> = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(at, _)| at + 1))
            .filter(|&start| start == 0 || start < source.len())
            .collect();
        let line_of = |offset: usize| {
            starts
                .iter()
                .rposition(|&start| start <= offset.min(source.len()))
                .unwrap_or(0)
        };
        let per_line: Vec<Vec<TextEdit>> = starts
            .iter()
            .map(|&start| edits_at(source, start))
            .collect();
        for start in 0..=source.len() + 2 {
            for end in start..=source.len() + 2 {
                let first = line_of(start);
                // An end at a line start selects nothing of that line.
                let last = if end > start { line_of(end - 1) } else { first }.max(first);
                let mut expected: Vec<TextEdit> = per_line[first..=last].concat();
                expected.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
                expected.dedup();
                let selection =
                    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32));
                assert_eq!(
                    format_range(source, Config::default(), selection),
                    Ok(expected),
                    "bytes {start}..{end} of {source:?} (lines {first}..={last})"
                );
            }
        }
    }
}
