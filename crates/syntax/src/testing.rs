//! Fixture harness for the syntax crate.
//!
//! The file format is the project-wide fixture format: `#==== <group>` opens a
//! group, `#---- <case>` opens a case, the case source runs until a `#++++`
//! line, and the expectation body runs until the next directive line (or end of
//! file). Case identity is `group__case` and must be unique across a whole
//! suite. `FIXTURE_FILTER=group__case` runs one case; `RY_BLESS=1`
//! rewrites expectation bodies in place from the rendered output.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct FixtureCase {
    pub id: String,
    pub source: String,
    pub expected: String,
    /// Byte range of the expectation body inside the fixture file.
    expected_span: Range<usize>,
}

pub struct FixtureFile {
    pub path: PathBuf,
    pub text: String,
    pub cases: Vec<FixtureCase>,
}

/// Parse every `.test` fixture file under `suite_dir` (recursively), without
/// running anything — for callers that want the corpus rather than a verdict,
/// such as deciding whether a focused-run filter names a real case.
pub fn parse_fixture_files(suite_dir: &Path) -> Vec<FixtureFile> {
    let mut paths = Vec::new();
    collect_fixture_files(suite_dir, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
            parse_fixture_file(&path, text)
        })
        .collect()
}

/// Every fixture case source in the workspace, as `(case id, source)`.
///
/// The hand-written fixture sources are the highest-quality R in the repository
/// — each one was written to exercise something — so the invariant batteries and
/// the differential oracles want all of them, not the subset one crate happens
/// to know about. Listing the suites here rather than in each harness is what
/// keeps a newly added suite from being silently invisible to every battery.
pub fn fixture_case_sources() -> Vec<(String, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits under crates/");
    let mut sources = Vec::new();
    for suite in FIXTURE_SUITES {
        for file in parse_fixture_files(&crates.join(suite)) {
            for case in file.cases {
                sources.push((format!("{suite}::{}", case.id), case.source));
            }
        }
    }
    sources
}

/// Every fixture suite in the workspace, workspace-relative under `crates/`.
/// A suite missing from this list runs its own expectations and nothing else.
const FIXTURE_SUITES: [&str; 13] = [
    "format/tests/format",
    "ide/tests/ide",
    "semantics/tests/lints",
    "semantics/tests/lints-style",
    "semantics/tests/lowering",
    "semantics/tests/naming",
    "semantics/tests/typing",
    "semantics/tests/typing-imports",
    "semantics/tests/typing-scripts",
    "semantics/tests/typing-strict",
    "syntax/tests/errors",
    "syntax/tests/syntax",
    "syntax/tests/tsr",
];

/// Every source in the mined legacy corpus (`tests/corpus-legacy/*.R.corpus`),
/// oldest-stack fixture cases kept for their *inputs* rather than their
/// expectations.
///
/// The frozen stack's suites hold ~2,000 curated R edge cases whose expected
/// output cannot be ported — it renders binding-resolution trees and a different
/// type notation — but the programs themselves are the richest hand-written
/// corpus in the repository, and nothing in the shipping crates ran a single one
/// of them. They are wired into the invariant batteries instead, where no
/// expectation is needed: never panic, stay deterministic, keep ranges in
/// bounds, and do not lose code when formatting.
///
/// Each file is `#---- <id>` followed by that case's source, up to the next
/// header. Sources are deduplicated across suites.
pub fn legacy_corpus_sources() -> Vec<(String, String)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus-legacy");
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "corpus")
        })
        .collect();
    files.sort();
    let mut sources = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut id = String::new();
        let mut body = String::new();
        for line in text.lines() {
            match line.strip_prefix("#---- ") {
                Some(next) => {
                    if !id.is_empty() {
                        sources.push((std::mem::take(&mut id), std::mem::take(&mut body)));
                    }
                    id = next.to_owned();
                    body.clear();
                }
                None => {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
        if !id.is_empty() {
            sources.push((id, body));
        }
    }
    sources
}

/// Reads an environment variable under its `RY_` name, falling back to the
/// pre-rename `ROUGHLY_` spelling. Both are honoured so a developer's existing
/// shell aliases and CI scripts keep working after the rename.
pub fn env_var(suffix: &str) -> Option<String> {
    std::env::var(format!("RY_{suffix}"))
        .or_else(|_| std::env::var(format!("ROUGHLY_{suffix}")))
        .ok()
}

/// Run every `.test` fixture under `suite_dir` through `render`, comparing (or
/// blessing) expectations. Panics with a readable report on mismatch.
pub fn run_fixture_suite(suite_dir: &Path, render: &dyn Fn(&str) -> String) {
    let mut files = Vec::new();
    collect_fixture_files(suite_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no fixture files found under {}",
        suite_dir.display()
    );

    let filter = std::env::var("FIXTURE_FILTER").ok();
    let bless = env_var("BLESS").is_some_and(|value| value == "1");
    let mut seen_ids = HashSet::new();
    let mut failures = Vec::new();
    let mut matched = 0usize;

    for path in files {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        let file = parse_fixture_file(&path, text);
        for case in &file.cases {
            assert!(
                seen_ids.insert(case.id.clone()),
                "duplicate fixture id `{}` (fixture ids must be unique across the suite)",
                case.id
            );
        }

        let mut blessed_edits: Vec<(Range<usize>, String)> = Vec::new();
        for case in &file.cases {
            if filter.as_deref().is_some_and(|filter| filter != case.id) {
                continue;
            }
            matched += 1;
            let rendered = normalize(&render(&case.source));
            let expected = normalize(&case.expected);
            if rendered == expected {
                continue;
            }
            if bless {
                blessed_edits.push((case.expected_span.clone(), rendered));
            } else {
                let mut report = String::new();
                let _ = writeln!(report, "fixture `{}` ({})", case.id, file.path.display());
                let _ = writeln!(report, "---- expected ----\n{expected}");
                let _ = writeln!(report, "---- actual ----\n{rendered}");
                failures.push(report);
            }
        }

        if !blessed_edits.is_empty() {
            let mut new_text = file.text.clone();
            for (span, rendered) in blessed_edits.into_iter().rev() {
                let at_eof = span.end == new_text.len();
                let replacement = if at_eof {
                    format!("{rendered}\n")
                } else {
                    format!("{rendered}\n\n")
                };
                new_text.replace_range(span, &replacement);
            }
            std::fs::write(&file.path, new_text)
                .unwrap_or_else(|error| panic!("cannot bless {}: {error}", file.path.display()));
        }
    }

    if let Some(filter) = &filter {
        assert!(
            matched > 0 || sibling_suite_holds(suite_dir, filter),
            "FIXTURE_FILTER=`{filter}` names no fixture case in any suite. Check the id.\n\
             A filter that matches nothing would otherwise run zero cases and report a pass."
        );
    }

    assert!(
        failures.is_empty(),
        "{} fixture failure(s):\n\n{}\n(set RY_BLESS=1 to accept the new output)",
        failures.len(),
        failures.join("\n")
    );
}

/// Whether a suite next to `suite_dir` holds `id`. One test binary drives
/// several suites, so a focused run naming one case leaves every other suite
/// matching nothing — and "this case lives next door" is the only thing that
/// distinguishes that ordinary skip from a mistyped id, which is otherwise
/// indistinguishable from a passing run.
fn sibling_suite_holds(suite_dir: &Path, id: &str) -> bool {
    let Some(parent) = suite_dir.parent() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.is_dir()
            && path != suite_dir
            && parse_fixture_files(&path)
                .iter()
                .any(|file| file.cases.iter().any(|case| case.id == id))
    })
}

fn collect_fixture_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("cannot read fixture dir {}: {error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_fixture_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "test")
        {
            out.push(path);
        }
    }
}

/// Parse one fixture file. Public for harnesses that pre-filter which files
/// to consume (the legacy-corpus differential skips multi-document
/// composites the shared single-file format cannot express).
pub fn parse_fixture_file(path: &Path, text: String) -> FixtureFile {
    let mut cases = Vec::new();
    let mut group: Option<String> = None;
    let mut case: Option<String> = None;
    let mut source_lines: Vec<&str> = Vec::new();
    let mut expected_start: Option<usize> = None;
    let mut expected_end = 0usize;

    let mut offset = 0usize;
    let mut flush = |group: &Option<String>,
                     case: &mut Option<String>,
                     source_lines: &mut Vec<&str>,
                     expected_start: &mut Option<usize>,
                     expected_end: usize,
                     text: &str| {
        if let Some(case_name) = case.take() {
            let group_name = group.clone().unwrap_or_else(|| {
                panic!(
                    "{}: case `{case_name}` appears before any `#====` group",
                    path.display()
                )
            });
            let start = expected_start.take().unwrap_or_else(|| {
                panic!(
                    "{}: case `{case_name}` has no `#++++` expectation",
                    path.display()
                )
            });
            let source = source_lines.join("\n").trim_end().to_owned();
            source_lines.clear();
            cases.push(FixtureCase {
                id: format!("{group_name}__{case_name}"),
                source,
                expected: text[start..expected_end].to_owned(),
                expected_span: start..expected_end,
            });
        }
        source_lines.clear();
    };

    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(name) = trimmed.strip_prefix("#====") {
            flush(
                &group,
                &mut case,
                &mut source_lines,
                &mut expected_start,
                expected_end,
                &text,
            );
            group = Some(name.trim().to_owned());
        } else if let Some(name) = trimmed.strip_prefix("#----") {
            flush(
                &group,
                &mut case,
                &mut source_lines,
                &mut expected_start,
                expected_end,
                &text,
            );
            case = Some(name.trim().to_owned());
        } else if trimmed.starts_with("#++++") {
            expected_start = Some(offset);
            expected_end = offset;
        } else if expected_start.is_some() && case.is_some() {
            expected_end = offset;
        } else if case.is_some() {
            source_lines.push(trimmed);
        }
    }
    flush(
        &group,
        &mut case,
        &mut source_lines,
        &mut expected_start,
        expected_end,
        &text,
    );

    FixtureFile {
        path: path.to_owned(),
        text,
        cases,
    }
}

/// Trailing-whitespace-insensitive comparison form.
fn normalize(text: &str) -> String {
    let mut normalized = text
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    while normalized.ends_with('\n') {
        normalized.pop();
    }
    normalized.trim_end().to_owned()
}

/// The full invariant battery for one input.
pub fn check_parse_invariants(input: &str) {
    let parse = crate::parse(input);
    let reprinted = parse.text();
    assert_eq!(
        reprinted, input,
        "lossless round-trip violated for input {input:?}"
    );

    let (tokens, _errors) = crate::lex(input);
    let total: u32 = tokens.iter().map(|token| u32::from(token.len)).sum();
    assert_eq!(
        total as usize,
        input.len(),
        "token cover violated for input {input:?}"
    );
    for token in &tokens {
        assert!(
            u32::from(token.len) > 0,
            "zero-length token for input {input:?}"
        );
    }

    check_geometry(&parse.syntax_node(), input);

    // Error-report quality: every diagnostic is renderable (non-empty
    // message, in-bounds range), and the report count stays linear in the
    // input — a cascade that fans one mistake into a storm is a bug even
    // when every individual message is well-formed.
    assert!(
        parse.errors().len() <= 2 * tokens.len() + 16,
        "error cascade: {} errors from {} tokens for input {input:?}",
        parse.errors().len(),
        tokens.len()
    );
    for error in parse.errors() {
        assert!(
            !error.message.is_empty(),
            "empty error message for input {input:?}"
        );
        assert!(
            u32::from(error.range.end()) as usize <= input.len(),
            "error range out of bounds for input {input:?}"
        );
    }

    // Determinism: a second parse yields a structurally equal tree and the
    // same errors.
    let again = crate::parse(input);
    assert_eq!(
        parse.green(),
        again.green(),
        "non-deterministic parse for input {input:?}"
    );
    assert_eq!(
        parse.errors(),
        again.errors(),
        "non-deterministic errors for input {input:?}"
    );
}

/// Every node's children (nodes and tokens) must tile its range exactly.
fn check_geometry(node: &crate::SyntaxNode, input: &str) {
    let range = node.text_range();
    let mut cursor = range.start();
    for child in node.children_with_tokens() {
        let child_range = child.text_range();
        assert_eq!(
            child_range.start(),
            cursor,
            "child ranges not contiguous under {:?} for input {input:?}",
            node.kind()
        );
        cursor = child_range.end();
        if let rowan::NodeOrToken::Node(child) = child {
            check_geometry(&child, input);
        }
    }
    if node.children_with_tokens().next().is_some() {
        assert_eq!(
            cursor,
            range.end(),
            "children do not cover {:?} for input {input:?}",
            node.kind()
        );
    }
}
