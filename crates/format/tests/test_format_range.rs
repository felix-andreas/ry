//! Range-formatting property tests: the invariant battery over every R source
//! in the repository, plus the texts the fixture format cannot spell — one
//! without a trailing newline, one with CRLF endings, and the empty file.

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
