//! `FUZZ_ITERS` scales the per-generator budget; runs are deterministic.

use format::check_format_invariants as check_invariants;
use format::{Config, format};
use std::panic::catch_unwind;
use syntax::testing::{
    SplitMix64, corpus_sample, fixture_case_sources, fuzz_iterations, mutate_bytes, mutate_seed,
    program_corpus_sources, random_bytes, token_soup,
};

const SEEDS: &[&str] = &[
    "x <- 1L\n",
    "f <- function(x, y = 2) x + y\n",
    "if (a > 1) b else c\n",
    "for (i in 1:10) {\n  total <- total + i\n}\n",
    "while (TRUE) break\n",
    "lst$field[[2]]$name\n",
    "pkg::fn(a, b = 2, ...)\n",
    "m[, 1]\n",
    "x |> f() |> g(1)\n",
    "p <- \\(x) x^2\n",
    "s <- \"a string with \\\"escapes\\\"\"\n",
    "s2 <- 'single'\n",
    "r <- r\"(raw \\ string)\"\n",
    "`odd name` <- 5\n",
    "a %in% b %o% c\n",
    "#: fn(x: integer) -> double\nf <- function(x) x / 2\n",
    "#: @type Point {list[double]}\n#: @strict\np <- list(1, 2)\n",
    "#: list{\n#:   a: integer\n#: }\nx <- list(a = 1L)\n",
    "x <- 1 #: integer\n",
    "df[df$col > 3, c(\"a\", \"b\")]\n",
    "x = 5; y = 6; z\n",
    "if (a) {\n  b\n} else {\n  c\n}\n",
    "tryCatch(expr, error = function(e) NULL)\n",
    "call(\n  first,\n  second = 2\n)\n",
    "call(first,\n     second)\n",
    "f(x) # trailing comment\n",
    "# a leading comment\nvalue\n",
    "#foo\n#' roxygen\n#!shebang\n#| echo: false\n## banner\n",
    "m <- rbind(c(1,  2),\n           c(30, 4)) # fmt: skip\n",
    "# fmt: off\nweird   <-  1\n# fmt: on\nplain <- 2\n",
    "switch(t, NULL = , chr = 1)\n",
    "names(x) <- c(\"a\")\n",
    "x[[y[1]]]\n",
    "repeat {\n  next\n}\n",
    "{\n  1; 2\n  3\n}\n",
    "p <- (x = 1, y = 2); t <- (1L, \"a\"); e <- (); one <- (x = 1,)\n",
    "s <- :Rect(width = 2, height = 3); n <- :None\n",
    "match (s) { # c\n  :Circle(radius = r) => r, :Rect(width = w, ..) if (w > 0) => w\n\n  (a, 'x') | (_, a) => a; -1L => 0\n  _ => NULL\n}\n",
    "x: :Some(integer) | :None | .. <- :None\n",
    "#: fn(p: record{x: double}) -> :Ok(value: double)\nf <- function(p) p\n",
    "type UserId = integer\nalias Box<T: numeric> = list{value: T}\nid <- @UserId(5L)\n",
    "alias Shape =\n  | :Circle(radius: double)\n  | :Empty\nx: @trust Shape <- y\n",
    "#: @alias Pair<T> {tuple{T, T}}\nalias Pair<U> = tuple{U, U}\np <- @Pair<integer>((1L, 2L))\n",
];

const ALPHABET: &[&str] = &[
    "x",
    "value",
    "f",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "[[",
    ",",
    ";",
    "\n",
    " ",
    "<-",
    "=",
    "+",
    "-",
    "*",
    "/",
    "^",
    "if",
    "else",
    "for",
    "while",
    "function",
    "\\",
    "1",
    "2.5",
    "1L",
    "\"s\"",
    "'c'",
    "%in%",
    "|>",
    "&&",
    "~",
    "$",
    "@",
    "::",
    ":",
    "`n`",
    "...",
    "#:",
    "# comment",
    "# fmt: skip",
    "# fmt: off",
    "# fmt: on",
    "NULL",
    "TRUE",
    "r\"(x)\"",
    "->",
    "<<-",
    "!",
    "==",
    "next",
    "break",
    "repeat",
    "in",
    "@type",
    "fn",
    "list[",
    "list{",
    "\r\n",
    "=>",
    "match",
    "..",
    ":None",
    ":Some(",
    "record{",
    "type ",
    "alias ",
    "|",
    "@trust ",
    "tuple{",
];

/// Coverage-guided fuzzer finds. Unlike `SEEDS` these may refuse; only the
/// invariants must hold.
const REGRESSIONS: &[&str] = &[
    "{\n  ;\n}\n",
    "{#:\n}\n",
    "foo:::#ar\nfoCo:::bar(1)\nfoo:::...\n",
    "f((f\n),)",
    "f(,\n)",
    "f(xf(x\n),x.= ,)\n",
    "(r\n#:\n-a)",
    "(ir\n#:dL\n--a)",
    "#\r#:",
    "#e\r\r#:on",
    "\r\r#:@e\r\r#:on\n",
    "-\n#:\nb",
    "---\n#: L\nbL",
    "1-----\n#: @\n\n- bL",
    "--fn(ir\n#:dL\n--a, )",
    " if (1)E else repeat 4",
    "repeat if (1) TRUE else repeat 42\nif (TRUE) ifNA\n(if (TRUE) FALSE\nelse NA)\n",
    "{ repeat 4 }",
    "{ repeat commentt\n}",
    "f <-#: Pers function(x, y) y\non\nx\n",
    "1L --- fn(ier\n#:dL\n----a, bar)\n\n!\n L\n",
];

fn iterations() -> usize {
    fuzz_iterations(1500)
}

#[test]
fn seeds_hold_invariants() {
    for seed in SEEDS {
        assert!(
            format(seed, Config::default()).is_ok(),
            "seed unexpectedly refused: {seed:?}"
        );
        check_invariants(seed);
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        check_invariants(input);
    }
}

#[test]
fn fixture_sources_hold_invariants() {
    let sources = fixture_case_sources();
    for (_, source) in &sources {
        check_invariants(source);
    }
    assert!(
        sources.len() > 100,
        "expected the fixture corpus, found {}",
        sources.len()
    );
}

#[test]
fn program_corpus_holds_invariants() {
    let sources = program_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    for (id, source) in &sources {
        catch_unwind(|| check_invariants(source))
            .unwrap_or_else(|_| panic!("program corpus case `{id}` broke a format invariant"));
    }
}

#[test]
fn fuzz_random_bytes() {
    run_random_bytes(iterations());
}

#[test]
fn fuzz_token_soup() {
    run_token_soup(iterations());
}

#[test]
fn fuzz_seed_mutations() {
    run_seed_mutations(iterations());
}

#[test]
fn fuzz_corpus_seeded() {
    let Some(files) = corpus_sample(48) else {
        eprintln!("fuzz_corpus_seeded: corpus not fetched; skipping");
        return;
    };
    for source in &files {
        check_invariants(source);
    }
    let mut rng = SplitMix64(0xF0_44_A7);
    for _ in 0..iterations() / 3 {
        let source = &files[rng.below(files.len())];
        check_invariants(&String::from_utf8_lossy(&mutate_bytes(&mut rng, source)));
    }
}

#[test]
#[ignore = "deep fuzz; run explicitly with a large FUZZ_ITERS"]
fn fuzz_deep() {
    let budget = iterations().max(20_000);
    run_random_bytes(budget);
    run_token_soup(budget);
    run_seed_mutations(budget);
}

fn run_random_bytes(budget: usize) {
    let mut rng = SplitMix64(0xF0_12_34);
    for _ in 0..budget {
        check_invariants(&random_bytes(&mut rng));
    }
}

fn run_token_soup(budget: usize) {
    let mut rng = SplitMix64(0xF0_50_42);
    for _ in 0..budget {
        check_invariants(&token_soup(&mut rng, ALPHABET));
    }
}

fn run_seed_mutations(budget: usize) {
    let mut rng = SplitMix64(0xF0_5E_ED);
    for _ in 0..budget {
        check_invariants(&mutate_seed(&mut rng, SEEDS, 16));
    }
}
