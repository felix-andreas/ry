//! Fuzz + property harness for the semantic pipeline, run against every
//! stage from its first commit. Fuzzing is pipeline-wide and never a
//! parser-only afterthought.
//!
//! Invariants, checked on every input:
//!   1. Nothing panics. The parse, the item tree, the HIR, naming, inference,
//!      diagnostics, and the strict stream all run to completion, and every
//!      salsa fixpoint cycle converges without hitting the iteration cap.
//!   2. The output is deterministic. A fresh database on the same text
//!      produces an identical rendering, covering the schemes, the
//!      diagnostics, and every lint under an everything-on configuration.
//!   3. The geometry holds. Every diagnostic and lint range lies inside the
//!      file, with start no greater than end.
//!   4. The analysis is incrementally equivalent. After editing the file
//!      through the salsa setter, the re-checked output equals a fresh
//!      database built on the edited text, and editing back restores the
//!      original output.
//!
//! The generator biases toward semantically live shapes: small programs
//! assembled from templates over a tiny name pool, so definitions, uses,
//! shadowing, self references, mutual reference cycles, closures,
//! annotations (checked, definitions, invalid mixes), and typing-mode
//! directives arise constantly. A token-soup arm keeps robustness against
//! syntactically broken input.
//!
//! `FUZZ_ITERS` scales the per-generator budget; runs are deterministic per
//! seed. `fuzz_deep` multiplies everything for nightly/manual runs.

use salsa::Setter as _;
use semantics::testing::{check_semantics_invariants, render_semantics as render};
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }

    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.next() % denominator < numerator
    }
}

const NAMES: &[&str] = &["a", "b", "f", "g", "x", "helper"];

/// Item templates; `{n}` is the defined name, `{m}` an arbitrary (often
/// different) name from the pool. The collisions are the point.
const TEMPLATES: &[&str] = &[
    "{n} <- {m}",
    "{n} <- {n} + 1L",
    "{n} <- function(p) {m}(p)",
    "{n} <- function(p) p + 1L",
    "{n} <- function() {n}()",
    "{n} <- c({m}, 1L)",
    "{n} <- list(x = 1, y = {m})",
    "{n} <- if ({m} > 1L) {m} else NULL",
    "{n} <- function(p) if (is.null(p)) 0L else p",
    "for (i in 1:3) {n} <- {m} + i",
    "{n} <- local({ t <- {m}; t })",
    "{n} <- \\(q) q + {m}",
    "print({m})",
    "{n} <- sum({m}, 1L)",
    "{n} <- {m}$field",
    "{n} <- {m}[[1L]]",
    "#: fn(x: integer) -> integer\n{n} <- function(x) x",
    "#: fn(x: integer) -> integer\n{n} <- function(x) {m}(x)",
    "#: integer\n{n} <- 1L",
    "#: @alias Alias {integer}\n\n#: Alias\n{n} <- {m}",
    "#: @type Point {list{v: double}}\n\n#: @new Point\n{n} <- list(v = 1)",
    "#: @trust fn(x: integer) -> character\n{n} <- function(x) x",
    "#: @param x {integer}\n#: @return {integer}\n{n} <- function(x) x",
    "#: @type Bad {integer}\n#: integer\n{n} <- 1L",
    "#: @alias Loop {Loop}\n\n#: Loop\n{n} <- 1L",
    "{n} <- {m}[x > 1L, .(s = sum(y)), by = z]",
    "{n} <- with({m}, x + y)",
    "{n} <- stats::sd(c(1, 2))",
    "{n} <- nosuchpkg::mystery()",
    "{n} <- {m} |> print()",
    "{n} <- y ~ {m}",
    "repeat break",
    "while ({m} > 0L) {n} <- {n} - 1L",
    "{n} <- function(...) list(...)",
    "{n} <- switch({m}, a = 1L, b = 2L)",
];

const DIRECTIVES: &[&str] = &[
    "# typing: strict",
    "# typing: on",
    "# typing: off",
    "#: @strict",
];

const SOUP: &[&str] = &[
    "x", "f", "(", ")", "{", "}", "[", "[[", "]", ",", "\n", " ", "<-", "+", "if", "else",
    "function", "1L", "\"s\"", "%in%", "|>", "~", "$", "::", "#:", "@type", "fn", "list{", "NULL",
    "...", ":=", "<<-", "\\", "@new",
];

fn iterations() -> usize {
    std::env::var("FUZZ_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(250)
}

fn generate_program(rng: &mut SplitMix64) -> String {
    let mut lines = Vec::new();
    if rng.chance(1, 6) {
        lines.push(DIRECTIVES[rng.below(DIRECTIVES.len())].to_owned());
        lines.push(String::new());
    }
    let item_count = 1 + rng.below(7);
    for _ in 0..item_count {
        let template = TEMPLATES[rng.below(TEMPLATES.len())];
        let defined = NAMES[rng.below(NAMES.len())];
        let mentioned = NAMES[rng.below(NAMES.len())];
        lines.push(template.replace("{n}", defined).replace("{m}", mentioned));
    }
    lines.join("\n")
}

fn generate_soup(rng: &mut SplitMix64) -> String {
    let length = 1 + rng.below(60);
    let mut source = String::new();
    for _ in 0..length {
        source.push_str(SOUP[rng.below(SOUP.len())]);
    }
    source
}

/// The canonical rendering of one file's full pipeline output; running it is
/// the never-panic check, its string is the determinism/equivalence witness.
/// It attaches the failing inputs to any panic escaping the pipeline. A bare
/// salsa or checker panic names no source, which makes a fuzz find useless.
fn check_pipeline_reporting(rng: &mut SplitMix64, source: &str) {
    let kind = if rng.chance(1, 4) {
        DocumentKind::Script
    } else {
        DocumentKind::Package
    };
    let edited_source = generate_program(rng);
    let result =
        std::panic::catch_unwind(|| check_semantics_invariants(source, &edited_source, kind));
    if let Err(payload) = result {
        eprintln!(
            "---- pipeline panicked ({kind:?}) ----\n{source}\n---- edited to ----\n{edited_source}\n----"
        );
        std::panic::resume_unwind(payload);
    }
}

fn run_generated(budget: usize, seed: u64) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let source = generate_program(&mut rng);
        check_pipeline_reporting(&mut rng, &source);
    }
}

fn run_soup(budget: usize, seed: u64) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let source = generate_soup(&mut rng);
        // Broken input still must not panic or misbehave; the incremental
        // arm runs too (an edit from a broken file to a live one).
        check_pipeline_reporting(&mut rng, &source);
    }
}

/// Two-file projects: cross-file references and cycles through the package
/// interface, with edits to one file.
fn run_multi_file(budget: usize, seed: u64) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let first_source = generate_program(&mut rng);
        let second_source = generate_program(&mut rng);
        let mut db = RootDatabase::default();
        semantics::stubs::install_shipped_stubs(&db);
        let first = SourceFile::new(&db, first_source.clone(), DocumentKind::Package);
        let second = SourceFile::new(&db, second_source.clone(), DocumentKind::Package);
        ProjectFiles::new(&db, vec![first, second]);
        let before = (render(&db, first), render(&db, second));

        let edited_source = generate_program(&mut rng);
        second.set_text(&mut db).to(edited_source.clone());
        let after_first = render(&db, first);
        let after_second = render(&db, second);

        let fresh_db = RootDatabase::default();
        semantics::stubs::install_shipped_stubs(&fresh_db);
        let fresh_first = SourceFile::new(&fresh_db, first_source.clone(), DocumentKind::Package);
        let fresh_second = SourceFile::new(&fresh_db, edited_source, DocumentKind::Package);
        ProjectFiles::new(&fresh_db, vec![fresh_first, fresh_second]);
        assert_eq!(
            after_first,
            render(&fresh_db, fresh_first),
            "cross-file incremental output diverges (untouched file)"
        );
        assert_eq!(
            after_second,
            render(&fresh_db, fresh_second),
            "cross-file incremental output diverges (edited file)"
        );

        second.set_text(&mut db).to(second_source);
        assert_eq!(
            (render(&db, first), render(&db, second)),
            before,
            "cross-file output not restored after editing back"
        );
    }
}

/// Inputs the coverage-guided `fuzz/semantics` target once broke an
/// invariant on, kept verbatim so the failures stay pinned without the
/// gitignored corpus. `foo$\n`: a splice middle ending in a dangling `$`
/// must refuse suffix reuse (the fresh parse continues across the newline,
/// moving the error position).
/// `f)\nla<- function()`: an appending edit leaves an empty splice suffix,
/// and the old parse's end-of-file error must not be rebased into the new
/// text (the middle re-derives the end state).
const REGRESSIONS: &[&str] = &["foo$\n", "f)\nla<- function()"];

/// The mined legacy corpus through the full pipeline. One shared database over
/// all of it rather than the per-input battery: the corpus is ~2,000 cases and a
/// fresh database costs a stub re-parse each, so this arm asserts what a shared
/// database can, which is that nothing panics and that a diagnostic range lies
/// inside its file. It leaves determinism and incremental equivalence to the
/// generated arms.
#[test]
fn legacy_corpus_holds_invariants() {
    use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};

    let sources = syntax::testing::legacy_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let files: Vec<(String, SourceFile)> = sources
        .iter()
        .map(|(id, source)| {
            (
                id.clone(),
                SourceFile::new(&db, source.clone(), DocumentKind::Package),
            )
        })
        .collect();
    ProjectFiles::new(&db, files.iter().map(|(_, file)| *file).collect());
    for (id, file) in &files {
        let length = file.text(&db).len();
        let diagnostics = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            semantics::diagnostics::file_diagnostics(&db, *file)
        }))
        .unwrap_or_else(|_| panic!("legacy corpus case `{id}` panicked the pipeline"));
        for diagnostic in diagnostics {
            let start = u32::from(diagnostic.range.start()) as usize;
            let end = u32::from(diagnostic.range.end()) as usize;
            assert!(
                start <= end && end <= length,
                "legacy corpus case `{id}`: range {start}..{end} escapes the file (length {length})"
            );
        }
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        semantics::testing::check_semantics_input(input);
    }
}

#[test]
fn fuzz_generated_programs() {
    run_generated(iterations(), 0xDECA_FBAD);
}

#[test]
fn fuzz_token_soup() {
    run_soup(iterations() / 2, 0xBEEF_FACE);
}

#[test]
fn fuzz_multi_file() {
    run_multi_file(iterations() / 5, 0xCAFE_D00D);
}

#[test]
#[ignore = "long fuzz run for nightly/manual use"]
fn fuzz_deep() {
    run_generated(iterations() * 20, 0x5EED_0001);
    run_soup(iterations() * 10, 0x5EED_0002);
    run_multi_file(iterations() * 4, 0x5EED_0003);
}
