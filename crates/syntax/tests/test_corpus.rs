//! The `corpus_*` tests read a gitignored `corpus/` of real R sources (see
//! `scripts/fetch-corpus.rs`) and skip when it is absent.

use {
    std::{
        borrow::Cow,
        collections::HashSet,
        fs,
        hint::black_box,
        path::{Path, PathBuf},
        time::Instant,
    },
    syntax::{
        SyntaxKind::{ARRAY_EXPR, MATCH_EXPR, RECORD_EXPR, TAG_EXPR, TUPLE_EXPR},
        TextRange, TextSize, erase_inline_annotations, parse, reparse,
        testing::{corpus_dir, fixture_case_sources, program_corpus_sources},
    },
    tree_sitter::{InputEdit, Parser, Point},
};

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
        if parse(&text).text() != text {
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
    let mut parser = tree_sitter_parser();

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
        let parsed = parse(&text);
        let ours_error = !parsed.errors().is_empty();
        let ts_error = parser
            .parse(text.as_str(), None)
            .is_none_or(|tree| tree.root_node().has_error());

        match (ours_error, ts_error) {
            (false, false) => both_clean += 1,
            (true, true) => both_error += 1,
            (true, false) => {
                let message = parsed.errors().first().map(|error| error.message.clone());
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
    let allowlist = read_allowlist("tests/acceptance-allowlist.txt");
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

/// Every other invariant bounds errors from above, so only this catches a
/// silently dropped syntax error. The reverse direction is noise here: fixtures
/// carry `#:` annotations and deliberately broken sources.
#[test]
fn in_tree_acceptance() {
    let mut parser = tree_sitter_parser();
    let mut sources = program_corpus_sources();
    sources.extend(fixture_case_sources());

    let mut accepted_only_by_us = Vec::new();
    for (id, text) in &sources {
        // Inline annotations are ry syntax that R rejects by design, so tree-sitter
        // judges the plain R program they erase to.
        let parsed = parse(text);
        // Records, tuples, Arrays, tags and `match` are ry syntax chosen to be text R
        // rejects, so a source that uses them has no R program to compare.
        if parsed.syntax_node().descendants().any(|node| {
            matches!(
                node.kind(),
                TUPLE_EXPR | RECORD_EXPR | ARRAY_EXPR | TAG_EXPR | MATCH_EXPR
            )
        }) {
            continue;
        }
        let erased = erase_inline_annotations(&parsed);
        if parsed.errors().is_empty()
            && parser
                .parse(erased.as_str(), None)
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
    let allowlist: HashSet<String> = read_allowlist("tests/in-tree-acceptance-allowlist.txt")
        .into_iter()
        .collect();
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

/// Release only: tree-sitter's C is compiled optimized even in debug profiles.
#[test]
#[ignore = "perf gate; run explicitly in release"]
fn corpus_parse_speed() {
    let Some(root) = corpus_dir() else {
        eprintln!("corpus_parse_speed: corpus not fetched; skipping");
        return;
    };
    let sources: Vec<String> = corpus_r_files(&root)
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    let total_bytes: usize = sources.iter().map(String::len).sum();
    let total_lines: usize = sources.iter().map(|source| source.lines().count()).sum();

    for source in sources.iter().take(50) {
        black_box(parse(source));
    }
    let ours_start = Instant::now();
    for source in &sources {
        black_box(parse(source).green());
    }
    let ours = ours_start.elapsed();

    let mut parser = tree_sitter_parser();
    let ts_start = Instant::now();
    for source in &sources {
        black_box(parser.parse(source, None));
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
        "expected ≥5× tree-sitter batch parse speed, measured {ratio:.2}x"
    );
}

#[test]
#[ignore = "perf measurement; run explicitly in release"]
fn corpus_incremental_speed() {
    let Some(root) = corpus_dir() else {
        eprintln!("corpus_incremental_speed: corpus not fetched; skipping");
        return;
    };
    let files = corpus_r_files(&root);
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut largest = String::new();
    for path in &files {
        if let Ok(bytes) = fs::read(path) {
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
        if let Ok(bytes) = fs::read(path) {
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
        let middle = source.floor_char_boundary(source.len() / 2);

        let mut text = source.clone();
        let ours_start = Instant::now();
        for step in 0..EDITS {
            text.insert(middle + step, 'x');
            black_box(parse(&text).green());
        }
        let ours = ours_start.elapsed() / EDITS as u32;

        let mut text = source.clone();
        let mut previous = parse(&text);
        let splice_start = Instant::now();
        for step in 0..EDITS {
            let at = TextSize::from((middle + step) as u32);
            text.insert(middle + step, 'x');
            previous = reparse(&previous, &text, TextRange::empty(at), TextSize::from(1));
            black_box(previous.green());
        }
        let splice = splice_start.elapsed() / EDITS as u32;

        let mut parser = tree_sitter_parser();
        let mut text = source.clone();
        let mut tree = parser.parse(&text, None).expect("initial parse");
        let point = |text: &str, offset: usize| {
            let row = text[..offset].matches('\n').count();
            let line_start = text[..offset].rfind('\n').map(|at| at + 1).unwrap_or(0);
            Point::new(row, offset - line_start)
        };
        let ts_start = Instant::now();
        for step in 0..EDITS {
            let at = middle + step;
            text.insert(at, 'x');
            tree.edit(&InputEdit {
                start_byte: at,
                old_end_byte: at,
                new_end_byte: at + 1,
                start_position: point(&text, at),
                old_end_position: point(&text, at),
                new_end_position: point(&text, at + 1),
            });
            tree = parser.parse(&text, Some(&tree)).expect("incremental parse");
            black_box(&tree);
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

fn tree_sitter_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("load tree-sitter-r grammar");
    parser
}

fn read_allowlist(relative: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn corpus_r_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for subtree in ["r-base", "cran"] {
        collect_r_files(&root.join(subtree), &mut files);
    }
    files.sort();
    files
}

fn collect_r_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
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

/// Some R sources are latin1; the flag reports whether decoding was lossy.
fn read_lossy(path: &Path) -> (String, bool) {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    match String::from_utf8_lossy(&bytes) {
        Cow::Borrowed(text) => (text.to_owned(), false),
        Cow::Owned(text) => (text, true),
    }
}
