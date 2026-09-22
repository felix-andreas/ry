//! Corpus tests over real R sources, and the tree-sitter acceptance oracle.
//!
//! `scripts/fetch-corpus.rs` populates a gitignored `corpus/` directory with the
//! R base library and a set of CRAN packages. These tests run the hand-written
//! parser over every `.R` file there:
//!
//!   * `corpus_round_trip` checks the lossless invariant. The tree must
//!     reprint byte-for-byte to its input.
//!   * `corpus_acceptance` compares our "has errors" verdict against tree-sitter-r
//!     as an oracle, bucketing agreement, and gates the ours-only-error bucket
//!     (we reject what tree-sitter accepts) against an adjudicated allowlist.
//!
//! Both skip cleanly when the corpus is absent (CI may run without a fetch).
//! Read files with lossy UTF-8 decoding: some R sources are latin1, not UTF-8.
//!
//! `in_tree_acceptance` runs the same differential over sources that are always
//! present, and gates the *other* direction. Its own comment says why that is
//! the direction nothing else in the project can see.
//!
//! Run with `cargo test -p syntax --test test_corpus -- --nocapture`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

#[test]
fn corpus_round_trip() {
    let Some(root) = corpus_dir() else {
        eprintln!(
            "corpus_round_trip: no corpus found (set RY_CORPUS_DIR or run \
             scripts/fetch-corpus.rs); skipping"
        );
        return;
    };

    let files = corpus_r_files(&root);
    let mut lossy = 0usize;
    let mut failures = Vec::new();
    for path in &files {
        let (text, was_lossy) = read_lossy(path);
        if was_lossy {
            lossy += 1;
        }
        if syntax::parse(&text).text() != text {
            failures.push(path.clone());
        }
    }

    eprintln!(
        "corpus_round_trip: {} .R files, {lossy} needed lossy UTF-8 conversion, {} round-trip \
         failure(s)",
        files.len(),
        failures.len()
    );
    for path in failures.iter().take(30) {
        eprintln!("  ROUND-TRIP FAIL: {}", path.display());
    }
    assert!(
        failures.is_empty(),
        "{} corpus file(s) did not reprint byte-exactly",
        failures.len()
    );
}

#[test]
fn corpus_acceptance() {
    let Some(root) = corpus_dir() else {
        eprintln!("corpus_acceptance: no corpus found; skipping");
        return;
    };

    let files = corpus_r_files(&root);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("load tree-sitter-r grammar");

    let mut both_clean = 0usize;
    let mut both_error = 0usize;
    let mut ours_only: Vec<(PathBuf, String)> = Vec::new();
    let mut ts_only: Vec<PathBuf> = Vec::new();
    let mut lossy = 0usize;

    for path in &files {
        let (text, was_lossy) = read_lossy(path);
        if was_lossy {
            lossy += 1;
        }
        let parse = syntax::parse(&text);
        let ours_error = !parse.errors().is_empty();
        // A `None` tree means tree-sitter could not parse at all, so treat it
        // as an error.
        let ts_error = parser
            .parse(text.as_str(), None)
            .is_none_or(|tree| tree.root_node().has_error());

        match (ours_error, ts_error) {
            (false, false) => both_clean += 1,
            (true, true) => both_error += 1,
            (true, false) => {
                let message = parse.errors().first().map(|error| error.message.clone());
                ours_only.push((path.clone(), message.unwrap_or_default()));
            }
            (false, true) => ts_only.push(path.clone()),
        }
    }

    eprintln!(
        "corpus_acceptance: {} .R files ({lossy} lossy)",
        files.len()
    );
    eprintln!("  both-clean:      {both_clean}");
    eprintln!("  both-error:      {both_error}");
    eprintln!(
        "  ours-only-error: {}  (BAD. We reject what tree-sitter accepts)",
        ours_only.len()
    );
    eprintln!(
        "  ts-only-error:   {}  (interesting. tree-sitter rejects what we accept)",
        ts_only.len()
    );

    eprintln!("--- ours-only-error (up to 30) ---");
    for (path, message) in ours_only.iter().take(30) {
        eprintln!("  BAD {}: {message}", path.display());
    }
    eprintln!("--- ts-only-error (up to 30) ---");
    for path in ts_only.iter().take(30) {
        eprintln!("  {}", path.display());
    }

    assert!(
        !files.is_empty(),
        "corpus present but no .R files found under r-base/ or cran/"
    );
    // HARD GATE: zero acceptance divergence is the measured baseline over the
    // full corpus. Rejecting a file tree-sitter accepts means a real grammar
    // gap; a legitimate divergence (tree-sitter itself being wrong, R as
    // referee) earns an entry in tests/acceptance-allowlist.txt instead.
    let allowlist = allowlisted_paths();
    let unexplained: Vec<_> = ours_only
        .iter()
        .filter(|(path, _)| {
            !allowlist
                .iter()
                .any(|entry| path.to_string_lossy().ends_with(entry.as_str()))
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "{} unexplained acceptance divergence(s); fix the parser or adjudicate (R as referee) into tests/acceptance-allowlist.txt",
        unexplained.len()
    );
}

/// The acceptance differential over in-tree sources, gating the direction the
/// rest of the project cannot see: **we accept what tree-sitter rejects**.
///
/// Every other parser invariant bounds errors from *above*. The cascade guard
/// caps how many we report, and nothing at all asserts that a broken file
/// reports anything. That matters because the parser carries a lot of dedup and
/// first-wins suppression, so over-suppression is the live regression risk, and
/// a silently dropped syntax error passes determinism, round-trip, range
/// geometry and the whole fuzz battery. tree-sitter is the second opinion.
///
/// Only the theirs-only direction is gated here. The ours-only direction is
/// noise on this input set: the fixture suites are full of `#:` annotations,
/// which tree-sitter reads as comments and we parse as types, plus deliberately
/// broken sources. The fetched-corpus test above gates ours-only over real
/// packages, where it is meaningful; between the two, both directions are
/// covered.
#[test]
fn in_tree_acceptance() {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("load tree-sitter-r grammar");

    let mut sources = syntax::testing::legacy_corpus_sources();
    sources.extend(syntax::testing::fixture_case_sources());

    let mut accepted_only_by_us = Vec::new();
    for (id, text) in &sources {
        if syntax::parse(text).errors().is_empty()
            && parser
                .parse(text.as_str(), None)
                .is_none_or(|tree| tree.root_node().has_error())
        {
            accepted_only_by_us.push(id.clone());
        }
    }

    eprintln!(
        "in_tree_acceptance: {} sources, {} accepted only by us",
        sources.len(),
        accepted_only_by_us.len()
    );
    for id in &accepted_only_by_us {
        eprintln!("  {id}");
    }

    assert!(
        sources.len() > 2_000,
        "expected the in-tree corpus, found {} sources",
        sources.len()
    );
    let allowlist = allowlisted_in_tree_ids();
    let unexplained: Vec<&String> = accepted_only_by_us
        .iter()
        .filter(|id| !allowlist.contains(*id))
        .collect();
    assert!(
        unexplained.is_empty(),
        "{} source(s) we accept and tree-sitter rejects, none adjudicated: {unexplained:?}\n\
         Either the parser stopped reporting an error it used to report, or the divergence is \
         legitimate (R as referee) and belongs in tests/in-tree-acceptance-allowlist.txt",
        unexplained.len()
    );
}

/// Phase 1 speed gate: ≥5× tree-sitter batch parse over the real corpus.
/// Run in release (`cargo test -p syntax --release --test test_corpus -- --ignored --nocapture`):
/// tree-sitter's C is compiled optimized even in debug profiles, so a debug
/// run would compare unoptimized Rust against optimized C.
#[test]
#[ignore = "perf gate; run explicitly in release"]
fn corpus_parse_speed() {
    let Some(root) = corpus_dir() else {
        eprintln!("corpus_parse_speed: corpus not fetched; skipping");
        return;
    };
    let mut files = Vec::new();
    for base in ["r-base", "cran"] {
        collect_r_files(&root.join(base), &mut files);
    }
    let sources: Vec<String> = files
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    let total_bytes: usize = sources.iter().map(String::len).sum();
    let total_lines: usize = sources.iter().map(|s| s.lines().count()).sum();

    // Warm both parsers once.
    for source in sources.iter().take(50) {
        let _ = syntax::parse(source);
    }
    let ours_start = std::time::Instant::now();
    for source in &sources {
        let parse = syntax::parse(source);
        std::hint::black_box(parse.green());
    }
    let ours = ours_start.elapsed();

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("grammar loads");
    let ts_start = std::time::Instant::now();
    for source in &sources {
        let tree = parser.parse(source, None);
        std::hint::black_box(&tree);
    }
    let ts = ts_start.elapsed();

    let ratio = ts.as_secs_f64() / ours.as_secs_f64();
    let ours_us_per_loc = ours.as_secs_f64() * 1e6 / total_lines as f64;
    let ts_us_per_loc = ts.as_secs_f64() * 1e6 / total_lines as f64;
    eprintln!(
        "corpus_parse_speed: {} files, {:.1} MB, {} LoC",
        sources.len(),
        total_bytes as f64 / 1e6,
        total_lines
    );
    eprintln!("  ours:        {ours:?}  ({ours_us_per_loc:.2} µs/LoC)");
    eprintln!("  tree-sitter: {ts:?}  ({ts_us_per_loc:.2} µs/LoC)");
    eprintln!("  ratio:       {ratio:.2}x");
    assert!(
        ratio >= 5.0,
        "Phase 1 gate: expected ≥5× tree-sitter batch parse speed, measured {ratio:.2}x"
    );
}

/// Incremental (per-keystroke) comparison: our full reparse per edit vs
/// tree-sitter's true incremental reparse, over a typing simulation in large
/// real files. The comparison is honest by construction. tree-sitter gets its
/// old tree plus an `InputEdit`, and we reparse from scratch, because `parse`
/// is a pure per-file query and sub-file incrementality lives one level down in
/// the per-item cutoffs. Run this in release.
#[test]
#[ignore = "perf measurement; run explicitly in release"]
fn corpus_incremental_speed() {
    let Some(root) = corpus_dir() else {
        eprintln!("corpus_incremental_speed: corpus not fetched; skipping");
        return;
    };
    let mut files = Vec::new();
    for base in ["r-base", "cran"] {
        collect_r_files(&root.join(base), &mut files);
    }
    // The largest corpus file plus a synthetic ~20K-LoC concatenation
    // (the pathological huge-file shape).
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut largest = String::new();
    for path in &files {
        if let Ok(bytes) = std::fs::read(path) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if text.len() > largest.len() {
                largest = text;
            }
        }
    }
    sources.push((
        format!("largest real file ({} LoC)", largest.lines().count()),
        largest,
    ));
    let mut synthetic = String::new();
    for path in files.iter().take(200) {
        if synthetic.lines().count() > 20_000 {
            break;
        }
        if let Ok(bytes) = std::fs::read(path) {
            synthetic.push_str(&String::from_utf8_lossy(&bytes));
            synthetic.push('\n');
        }
    }
    sources.push((
        format!("synthetic ({} LoC)", synthetic.lines().count()),
        synthetic,
    ));

    const EDITS: usize = 100;
    for (label, source) in &sources {
        // Typing simulation: insert one character at a time in the middle of
        // the file (a body position), then delete them again.
        let middle = {
            let mut at = source.len() / 2;
            while at > 0 && !source.is_char_boundary(at) {
                at -= 1;
            }
            at
        };

        // Ours: full reparse per edit.
        let mut text = source.clone();
        let ours_start = std::time::Instant::now();
        for step in 0..EDITS {
            text.insert(middle + step, 'x');
            let parse = syntax::parse(&text);
            std::hint::black_box(parse.green());
        }
        let ours = ours_start.elapsed() / EDITS as u32;

        // Ours: statement-splice incremental reparse per edit.
        let mut text = source.clone();
        let mut previous = syntax::parse(&text);
        let splice_start = std::time::Instant::now();
        for step in 0..EDITS {
            let at = middle + step;
            text.insert(at, 'x');
            let parse = syntax::reparse(
                &previous,
                &text,
                rowan::TextRange::new(
                    rowan::TextSize::from(at as u32),
                    rowan::TextSize::from(at as u32),
                ),
                rowan::TextSize::from(1),
            );
            std::hint::black_box(parse.green());
            previous = parse;
        }
        let splice = splice_start.elapsed() / EDITS as u32;

        // Tree-sitter: incremental reparse with the previous tree + InputEdit.
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_r::LANGUAGE.into())
            .expect("grammar loads");
        let mut text = source.clone();
        let mut tree = parser.parse(&text, None).expect("initial parse");
        let point = |text: &str, offset: usize| {
            let row = text[..offset].matches('\n').count();
            let line_start = text[..offset].rfind('\n').map(|at| at + 1).unwrap_or(0);
            tree_sitter::Point::new(row, offset - line_start)
        };
        let ts_start = std::time::Instant::now();
        for step in 0..EDITS {
            let at = middle + step;
            text.insert(at, 'x');
            tree.edit(&tree_sitter::InputEdit {
                start_byte: at,
                old_end_byte: at,
                new_end_byte: at + 1,
                start_position: point(&text, at),
                old_end_position: point(&text, at),
                new_end_position: point(&text, at + 1),
            });
            tree = parser.parse(&text, Some(&tree)).expect("incremental parse");
            std::hint::black_box(&tree);
        }
        let ts = ts_start.elapsed() / EDITS as u32;

        eprintln!("corpus_incremental_speed [{label}]:");
        eprintln!("  ours (full reparse):        {ours:?}/edit");
        eprintln!("  ours (statement splice):    {splice:?}/edit");
        eprintln!("  tree-sitter (incremental):  {ts:?}/edit");
        let full_ratio = ours.as_secs_f64() / ts.as_secs_f64().max(1e-9);
        let splice_ratio = ts.as_secs_f64() / splice.as_secs_f64().max(1e-9);
        eprintln!("  full/ts: {full_ratio:.1}x slower; ts/splice: {splice_ratio:.1}x faster");
    }
}

/// Committed, adjudicated divergences (one corpus-relative path suffix per
/// line, with `#` starting a comment). It is currently empty, because the
/// measured baseline is zero.
fn allowlisted_paths() -> Vec<String> {
    read_allowlist("tests/acceptance-allowlist.txt").collect()
}

/// The in-tree counterpart, keyed by fixture case id rather than path.
fn allowlisted_in_tree_ids() -> std::collections::HashSet<String> {
    read_allowlist("tests/in-tree-acceptance-allowlist.txt").collect()
}

fn read_allowlist(relative: &str) -> impl Iterator<Item = String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
}

/// Resolve the corpus root: `RY_CORPUS_DIR` if set, else `<workspace>/corpus`
/// (this crate is `<workspace>/crates/syntax`). `None` when the directory is
/// absent, so the tests can skip.
fn corpus_dir() -> Option<PathBuf> {
    if let Some(dir) = syntax::testing::env_var("CORPUS_DIR") {
        let path = PathBuf::from(dir);
        return path.is_dir().then_some(path);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("corpus");
    path.is_dir().then_some(path)
}

/// Every `.R` file under the parseable corpus subtrees (`r-base`, `cran`), sorted
/// for stable reporting. The tree-sitter grammar-test corpus is `.txt` and lives
/// elsewhere, so it is naturally excluded.
fn corpus_r_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for subtree in ["r-base", "cran"] {
        collect_r_files(&root.join(subtree), &mut files);
    }
    files.sort();
    files
}

fn collect_r_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_r_files(&path, out);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "R" | "r" | "S" | "s" | "q"))
        {
            out.push(path);
        }
    }
}

/// R sources are usually UTF-8 but some are latin1; decode lossily and report
/// whether any byte had to be replaced.
fn read_lossy(path: &Path) -> (String, bool) {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    match String::from_utf8_lossy(&bytes) {
        Cow::Borrowed(text) => (text.to_owned(), false),
        Cow::Owned(text) => (text, true),
    }
}
