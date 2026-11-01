//! Fuzz harness for the semantic pipeline. `FUZZ_ITERS` scales the per-generator budget;
//! runs are deterministic per seed.

use salsa::Setter as _;
use semantics::diagnostics::file_diagnostics;
use semantics::stubs::install_shipped_stubs;
use semantics::testing::{
    check_semantics_input, check_semantics_invariants, render_semantics as render,
};
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use syntax::testing::{SplitMix64, fuzz_iterations, program_corpus_sources};

const NAMES: &[&str] = &["a", "b", "f", "g", "x", "helper"];

/// `{n}` is the defined name, `{m}` another from the pool; the collisions are the point.
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
    "{n}: integer <- {m}",
    "{n}: list[named: double] = list()",
    "{n} <- function(x: integer, y = {m}) -> integer x",
    "{n} <- function<T>(x: T, ...: character) -> T {m}(x)",
    "#: fn(x: double) -> double\n{n} <- function(x: integer) x",
    "#: double\n{n}: integer <- {m}",
    "{n} <- lapply(1:3, \\(x: integer) -> integer {m}(x))",
    "{n} <- function() {\n  local_value: character <- {m}\n  local_value\n}",
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
    "{n} <- (x = {m}, y = 1L)",
    "{n} <- ({m}, \"a\")",
    "{n} <- ({m},)",
    "{n} <- {m}$x + {m}[[1L]]",
    "{n} <- if ({m}) :Some({m}) else :None",
    "{n} <- :Rect(width = {m}, height = 2)",
    "{n} <- match ({m}) {\n  :Some(v) if (v > 0L) => v\n  :Some(_) | :None => 0L\n}",
    "{n} <- match ({m}) {\n  (x = a, ..) => a\n  (p, q) => p\n  _ => {m}\n}",
    "{n} <- match ({m}) {\n  TRUE => 1L\n  FALSE => 2L\n}",
    "#: fn(o: :Some(integer) | :None) -> integer\n{n} <- function(o) match (o) {\n  :Some(v) => v\n}",
    "#: fn(r: :Ok(integer) | ..) -> integer\n{n} <- function(r) match (r) {\n  :Ok(v) => v\n  _ => {m}\n}",
    "#: @alias Shape {:Circle(radius: double) | :Empty}\n\n#: Shape\n{n} <- :Circle(radius = {m})",
    "#: @pure\n{n} <- function(p) {m}(p)",
    "#: @pure fn(p: integer) -> integer\n{n} <- function(p) p",
    "{n} <- @pure function(p) {m}(p)",
    "{n} <- @pure \\(q) q + {m}",
    "{n} <- {m}(1L)",
    "{n} <<- {m}",
    "names({n}) <- {m}",
    "{n} <- ({m} <- 1L) + 1L",
    "{n} <- memoise::memoise({m})",
    "if ({m}) {n} <- @pure function() 1L",
    "{n} <- local({\n  for (i in 1:2) {m} <- i\n  {m}\n})",
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
    "...", ":=", "<<-", "\\", "@new", "match", "=>", ":Some", ":None", "_", "..", "|", "@pure",
];

fn iterations() -> usize {
    fuzz_iterations(250)
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

/// Prints the failing inputs on panic: a bare salsa or checker panic names no source.
fn run_single_file(budget: usize, seed: u64, generate: fn(&mut SplitMix64) -> String) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let source = generate(&mut rng);
        let kind = if rng.chance(1, 4) {
            DocumentKind::Script
        } else {
            DocumentKind::Package
        };
        let edited_source = generate_program(&mut rng);
        let result = catch_unwind(|| check_semantics_invariants(&source, &edited_source, kind));
        if let Err(payload) = result {
            eprintln!(
                "---- pipeline panicked ({kind:?}) ----\n{source}\n---- edited to ----\n{edited_source}\n----"
            );
            resume_unwind(payload);
        }
    }
}

fn run_multi_file(budget: usize, seed: u64) {
    let mut rng = SplitMix64(seed);
    for _ in 0..budget {
        let first_source = generate_program(&mut rng);
        let second_source = generate_program(&mut rng);
        let mut db = RootDatabase::default();
        install_shipped_stubs(&db);
        let first = SourceFile::new(&db, first_source.clone(), DocumentKind::Package);
        let second = SourceFile::new(&db, second_source.clone(), DocumentKind::Package);
        ProjectFiles::new(&db, vec![first, second]);
        let before = (render(&db, first), render(&db, second));

        let edited_source = generate_program(&mut rng);
        second.set_text(&mut db).to(edited_source.clone());
        let after_first = render(&db, first);
        let after_second = render(&db, second);

        let fresh_db = RootDatabase::default();
        install_shipped_stubs(&fresh_db);
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

/// Inputs that once broke an invariant, pinned without the gitignored fuzz corpus.
const REGRESSIONS: &[&str] = &["foo$\n", "f)\nla<- function()"];

/// One shared database (a fresh one re-parses the stubs per case), so this asserts only
/// never-panic and range geometry; the generated arms cover determinism and equivalence.
#[test]
fn program_corpus_holds_invariants() {
    let sources = program_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
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
        let diagnostics = catch_unwind(AssertUnwindSafe(|| file_diagnostics(&db, *file)))
            .unwrap_or_else(|_| panic!("program corpus case `{id}` panicked the pipeline"));
        for diagnostic in diagnostics {
            let start = u32::from(diagnostic.range.start()) as usize;
            let end = u32::from(diagnostic.range.end()) as usize;
            assert!(
                start <= end && end <= length,
                "program corpus case `{id}`: range {start}..{end} escapes the file (length {length})"
            );
        }
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        check_semantics_input(input);
    }
}

#[test]
fn fuzz_generated_programs() {
    run_single_file(iterations(), 0xDECA_FBAD, generate_program);
}

#[test]
fn fuzz_token_soup() {
    run_single_file(iterations() / 2, 0xBEEF_FACE, generate_soup);
}

#[test]
fn fuzz_multi_file() {
    run_multi_file(iterations() / 5, 0xCAFE_D00D);
}

#[test]
#[ignore = "long fuzz run for nightly/manual use"]
fn fuzz_deep() {
    run_single_file(iterations() * 20, 0x5EED_0001, generate_program);
    run_single_file(iterations() * 10, 0x5EED_0002, generate_soup);
    run_multi_file(iterations() * 4, 0x5EED_0003);
}
