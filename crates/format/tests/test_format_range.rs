//! Range-formatting property tests: the invariant battery over every R source
//! in the repository, every byte selection of a few short sources against an
//! independent line oracle, plus the texts the fixture format cannot spell:
//! ones without a trailing newline, with CRLF endings, and the empty file.

use {
    format::{
        Config, TextEdit, apply_edits, check_range_format_invariants as check_invariants, format,
        format_range,
    },
    syntax::{TextRange, TextSize, testing::in_parallel},
};

#[test]
fn fixture_sources_hold_range_invariants() {
    let sources = syntax::testing::fixture_case_sources();
    hold_invariants(&sources, |id| format!("fixture case `{id}`"));
}

#[test]
fn program_corpus_holds_range_invariants() {
    let sources = syntax::testing::program_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    hold_invariants(&sources, |id| format!("program corpus case `{id}`"));
}

/// Checks the battery on every source, one worker per core, and fails naming each source that
/// broke an invariant (the battery's own panic message precedes the list).
fn hold_invariants(sources: &[(String, String)], describe: impl Fn(&str) -> String + Sync) {
    let broken: Vec<String> = in_parallel(sources, |(id, source)| {
        std::panic::catch_unwind(|| check_invariants(source))
            .err()
            .map(|_| describe(id))
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        broken.is_empty(),
        "{} broke a range-format invariant",
        broken.join(", ")
    );
}

/// The same sources re-indented the ways real files are (four spaces, tabs,
/// and a mix of steps that leaves the commonest one to decide), because a
/// selection is laid out at the step the file already uses, and the battery's
/// convergence and composition checks are what prove that step stays put
/// while a file is formatted piece by piece. The fixtures are written at the
/// default step, so on their own they never exercise any other.
#[test]
fn reindented_sources_hold_range_invariants() {
    let reindent = |source: &str, step: &dyn Fn(usize) -> &'static str| -> String {
        source
            .split_inclusive('\n')
            .enumerate()
            .map(|(index, line)| {
                let content = line.trim_start_matches(' ');
                let levels = (line.len() - content.len()) / 2;
                format!("{}{content}", step(index).repeat(levels))
            })
            .collect()
    };
    let steps: [&dyn Fn(usize) -> &'static str; 3] = [&|_| "    ", &|_| "\t", &|index| {
        if index % 3 == 0 { "  " } else { "   " }
    }];
    let variants: Vec<(String, String)> = syntax::testing::fixture_case_sources()
        .into_iter()
        .filter(|(_, source)| source.lines().any(|line| line.starts_with("  ")))
        .step_by(5)
        .flat_map(|(id, source)| {
            steps
                .iter()
                .map(|step| (id.clone(), reindent(&source, step)))
                .collect::<Vec<_>>()
        })
        .collect();
    hold_invariants(&variants, |id| format!("fixture case `{id}` re-indented"));
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

/// Each case is a source, a caret in it, the one edit formatting that caret's line makes, and
/// the source after the edit.
#[test]
fn a_caret_formats_its_line_with_the_documents_line_endings() {
    let cases = [
        // The last line of a file without a trailing newline gains one.
        ("x <- 1\ny<-2", 11, 7..11, "y <- 2\n", "x <- 1\ny <- 2\n"),
        // The newline belongs to the last line's own edit, so an earlier line must not reach it.
        ("x<-1\ny <- 2", 0, 0..5, "x <- 1\n", "x <- 1\ny <- 2"),
        (
            "x <- 1\r\ny<-2\r\n",
            9,
            8..14,
            "y <- 2\r\n",
            "x <- 1\r\ny <- 2\r\n",
        ),
    ];
    for (source, caret, range, new_text, result) in cases {
        let edits = edits_at(source, caret);
        assert_eq!(
            edits,
            [TextEdit {
                range: TextRange::new(TextSize::new(range.start), TextSize::new(range.end)),
                new_text: new_text.to_owned(),
            }],
            "{source:?}"
        );
        assert_eq!(apply_edits(source, &edits), result, "{source:?}");
    }
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
/// past the end, makes exactly the edits of the lines it touches, checked
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
