//! Fuzz + property harness, run against every parser increment from its first
//! commit.
//!
//! Invariants, checked on every input:
//!   1. never panic (any panic fails the test);
//!   2. the parse is lossless, so the tree reprints byte-for-byte to the
//!      input;
//!   3. the lexed token lengths cover the input exactly, with no zero-length
//!      token;
//!   4. the tree geometry holds, so every node's children tile its range
//!      exactly, meaning the ranges nest, are contiguous, and cover;
//!   5. parsing is deterministic, so parsing the same input twice yields
//!      structurally equal green trees.
//!
//! Generators: random bytes, token soup from an R-shaped alphabet, byte-level
//! seed mutations, token-level structure-aware mutations (delete/duplicate/
//! swap whole tokens of a real parse), corpus-seeded mutations (real R files
//! from the fetched corpus, when present), and a sequential edit-stream fuzzer
//! (one buffer, hundreds of random edits, and the invariants at every step,
//! which is the foundation the splice-reparse equivalence check plugs into).
//!
//! `FUZZ_ITERS` scales the per-generator budget (default 1500); runs are
//! deterministic per seed. `fuzz_deep` multiplies everything for nightly/manual
//! runs.

use std::path::PathBuf;
use syntax::testing::check_parse_invariants as check_invariants;

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
}

const SEEDS: &[&str] = &[
    "x <- 1L",
    "f <- function(x, y = 2) x + y",
    "if (a > 1) b else c",
    "for (i in 1:10) { total <- total + i }",
    "while (TRUE) break",
    "repeat next",
    "lst$field[[2]]$name",
    "pkg::fn(a, b = 2, ...)",
    "m[, 1]",
    "x |> f() |> g(1)",
    "y ~ a + b",
    "p <- \\(x) x^2",
    "s <- \"a string with \\\"escapes\\\"\"",
    "r <- r\"(raw \\ string)\"",
    "z <- 2i + 0x1FL",
    "`odd name` <- 5",
    "a %in% b %o% c",
    "#: fn(x: integer) -> double\nf <- function(x) x / 2",
    "#: @type Point {list[double]}\n#: @strict\np <- list(1, 2)",
    "df[df$col > 3, c(\"a\", \"b\")]",
    "x = 5; y = 6ate; z",
    "if (a) { b } else { c }",
    "tryCatch(expr, error = function(e) NULL)",
    "-2^2 + (1)",
    "x <<- y ->> z",
    "d <- data$frame@slot::name",
    "x <- 1\r\ny <- 2\r\n",
    "x[[y[1]]]",
    "names(x) <- c(\"a\")",
    "if (a) 1 else if (b) 2 else 3",
    "base**size",
    "switch(t, NULL = , chr = 1)",
    "#: <T: numeric> fn(x: T, [d]: integer, ...r: T) -> T[]\nround2 <- function(x, d = 0L, ...) x",
    "{\n  if (a) 1\n  else 2\n}",
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
    "2i",
    "\"s\"",
    "'c'",
    "%in%",
    "%%",
    "|>",
    "|",
    "&&",
    "~",
    "?",
    "$",
    "@",
    "::",
    ":",
    "`n`",
    "...",
    "..1",
    "#:",
    "# comment",
    "NULL",
    "TRUE",
    "NA",
    "Inf",
    "r\"(x)\"",
    "->",
    "->>",
    "<<-",
    "!",
    "==",
    "<=",
    ">",
    "_",
    "0x1F",
    "1e5",
    ".5",
    "next",
    "break",
    "repeat",
    "in",
    "**",
    ":=",
    "@type",
    "@param",
    "fn",
    "list[",
    "list{",
    "named",
    "<T>",
    "NULL =",
    "\r\n",
];

fn iterations() -> usize {
    std::env::var("FUZZ_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1500)
}

#[test]
fn seeds_hold_invariants() {
    for seed in SEEDS {
        check_invariants(seed);
    }
}

/// The mined legacy corpus: ~2,000 curated R edge cases from the frozen stack's
/// suites, which nothing in the shipping crates otherwise runs.
#[test]
fn legacy_corpus_holds_invariants() {
    let sources = syntax::testing::legacy_corpus_sources();
    assert!(
        sources.len() > 1_000,
        "expected the mined corpus, found {}",
        sources.len()
    );
    for (id, source) in &sources {
        std::panic::catch_unwind(|| check_invariants(source))
            .unwrap_or_else(|_| panic!("legacy corpus case `{id}` broke a parse invariant"));
    }
}

/// Blanking prose out of a literate document must not move a single byte:
/// every offset the analysis reports is an offset into the *original* file, so
/// a transform that shortened or lengthened the text by one byte would shift
/// every diagnostic after it. The unit tests pin that on four hand-written
/// documents; this pins it over generated ones, which is where a stray
/// multibyte character or an unterminated fence would show up.
#[test]
fn literate_conversion_never_moves_a_byte() {
    const PIECES: &[&str] = &[
        "# Title\n",
        "Some prose.\n",
        "```{r}\n",
        "```{r setup, echo=FALSE}\n",
        "```{python}\n",
        "```\n",
        "x <- 1L\n",
        "café <- \"héllo 😀\"\n",
        "<<chunk, echo=TRUE>>=\n",
        "@\n",
        "\\section{One}\n",
        "\n",
        "``` {r}\n",
        "```{r",
        "text without a newline",
        "\r\n",
        "#| echo: false\n",
    ];
    let mut rng = SplitMix64(0x11_7E_A7);
    for _ in 0..iterations() {
        let mut document = String::new();
        for _ in 0..rng.below(12) {
            document.push_str(PIECES[rng.below(PIECES.len())]);
        }
        let converted = syntax::literate::r_source_of_literate(&document);
        assert_eq!(
            converted.len(),
            document.len(),
            "length changed for {document:?} -> {converted:?}"
        );
        let original_newlines: Vec<usize> =
            document.match_indices('\n').map(|(at, _)| at).collect();
        let converted_newlines: Vec<usize> =
            converted.match_indices('\n').map(|(at, _)| at).collect();
        assert_eq!(
            original_newlines, converted_newlines,
            "newlines moved for {document:?} -> {converted:?}"
        );
        // The converted text is fed straight to the parser, so it must also be
        // something the parser can hold: the invariants below are the same ones
        // every other arm asserts.
        check_invariants(&converted);
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
fn fuzz_token_level_mutations() {
    run_token_level_mutations(iterations());
}

#[test]
fn fuzz_edit_stream() {
    run_edit_stream(400);
}

/// Real corpus files (when fetched) as mutation seeds: byte and token-level
/// mutations over genuine R sources exercise shapes no hand seed covers.
#[test]
fn fuzz_corpus_seeded() {
    let Some(files) = corpus_sample(24) else {
        eprintln!("fuzz_corpus_seeded: corpus not fetched; skipping");
        return;
    };
    let mut rng = SplitMix64(0xC0_2B_05);
    let budget = iterations() / 3;
    for _ in 0..budget {
        let source = &files[rng.below(files.len())];
        let mut bytes = source.as_bytes().to_vec();
        // A handful of byte edits, then a truncation half the time (simulates
        // a file mid-edit).
        for _ in 0..(1 + rng.below(6)) {
            if bytes.is_empty() {
                break;
            }
            match rng.below(3) {
                0 => {
                    let at = rng.below(bytes.len());
                    bytes.remove(at);
                }
                1 => {
                    let at = rng.below(bytes.len() + 1);
                    bytes.insert(at, (rng.next() & 0x7F) as u8);
                }
                _ => {
                    let at = rng.below(bytes.len());
                    bytes[at] = (rng.next() & 0x7F) as u8;
                }
            }
        }
        if rng.below(2) == 0 && !bytes.is_empty() {
            bytes.truncate(rng.below(bytes.len()));
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        check_invariants(&text);
    }
}

/// The everything-at-once deep run for nightly/manual use:
/// `FUZZ_ITERS=50000 cargo test -p syntax --release --test test_fuzz -- --ignored fuzz_deep`
#[test]
#[ignore = "deep fuzz; run explicitly with a large FUZZ_ITERS"]
fn fuzz_deep() {
    let budget = iterations().max(20_000);
    run_random_bytes(budget);
    run_token_soup(budget);
    run_seed_mutations(budget);
    run_token_level_mutations(budget);
    run_edit_stream(budget / 20);
}

fn run_random_bytes(budget: usize) {
    let mut rng = SplitMix64(0xDEC0_DE01);
    for _ in 0..budget {
        let len = rng.below(160);
        let bytes: Vec<u8> = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        check_invariants(&text);
    }
}

fn run_token_soup(budget: usize) {
    let mut rng = SplitMix64(0x50_D4_11);
    for _ in 0..budget {
        let pieces = rng.below(48);
        let mut text = String::new();
        for _ in 0..pieces {
            text.push_str(ALPHABET[rng.below(ALPHABET.len())]);
            if rng.below(3) == 0 {
                text.push(' ');
            }
        }
        check_invariants(&text);
    }
}

fn run_seed_mutations(budget: usize) {
    let mut rng = SplitMix64(0x5EED_F00D);
    for _ in 0..budget {
        let seed = SEEDS[rng.below(SEEDS.len())];
        let mut text = seed.as_bytes().to_vec();
        for _ in 0..(1 + rng.below(4)) {
            match rng.below(4) {
                0 if !text.is_empty() => {
                    let at = rng.below(text.len());
                    text.remove(at);
                }
                1 => {
                    let at = rng.below(text.len() + 1);
                    text.insert(at, (rng.next() & 0x7F) as u8);
                }
                2 if !text.is_empty() => {
                    let at = rng.below(text.len());
                    text[at] = (rng.next() & 0x7F) as u8;
                }
                // Cross-seed splice: paste a slice of another seed.
                _ => {
                    let donor = SEEDS[rng.below(SEEDS.len())].as_bytes();
                    let from = rng.below(donor.len());
                    let to = (from + rng.below(16)).min(donor.len());
                    let at = rng.below(text.len() + 1);
                    for (offset, &byte) in donor[from..to].iter().enumerate() {
                        text.insert(at + offset, byte);
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(&text).into_owned();
        check_invariants(&text);
    }
}

/// Structure-aware mutation operates on whole lexed tokens of a real seed. It
/// deletes, duplicates, or swaps a token span, so an input stays
/// token-plausible and reaches a deep parser path that byte noise rarely
/// finds.
fn run_token_level_mutations(budget: usize) {
    let mut rng = SplitMix64(0x70C3_2015);
    for _ in 0..budget {
        let seed = SEEDS[rng.below(SEEDS.len())];
        let (tokens, _) = syntax::lex(seed);
        if tokens.is_empty() {
            continue;
        }
        let mut spans: Vec<&str> = Vec::with_capacity(tokens.len());
        let mut offset = 0usize;
        for token in &tokens {
            let len = u32::from(token.len) as usize;
            spans.push(&seed[offset..offset + len]);
            offset += len;
        }
        for _ in 0..(1 + rng.below(3)) {
            match rng.below(3) {
                0 if spans.len() > 1 => {
                    let at = rng.below(spans.len());
                    spans.remove(at);
                }
                1 => {
                    let at = rng.below(spans.len());
                    let duplicate = spans[at];
                    spans.insert(at, duplicate);
                }
                _ if spans.len() > 1 => {
                    let a = rng.below(spans.len());
                    let b = rng.below(spans.len());
                    spans.swap(a, b);
                }
                _ => {}
            }
        }
        let text: String = spans.concat();
        check_invariants(&text);
    }
}

/// One buffer, a long stream of random edits; the invariant battery holds at
/// every step, and the statement-splice incremental reparse must be byte- and
/// structure-equal to the from-scratch parse after every edit (doctrine layer:
/// statement-reparse equivalence).
fn run_edit_stream(steps: usize) {
    let mut rng = SplitMix64(0xED17_57EA);
    let mut buffer = String::from("f <- function(x, y) {\n  total <- x + y\n  total * 2\n}\n");
    let mut previous = syntax::parse(&buffer);
    for _ in 0..steps {
        let (deleted, inserted) = match rng.below(4) {
            0 if !buffer.is_empty() => {
                let at = buffer.floor_char_boundary(rng.below(buffer.len()));
                let end = buffer.ceil_char_boundary((at + 1 + rng.below(3)).min(buffer.len()));
                buffer.replace_range(at..end, "");
                (at..end, 0usize)
            }
            1 => {
                let at = buffer.floor_char_boundary(rng.below(buffer.len() + 1));
                let piece = ALPHABET[rng.below(ALPHABET.len())];
                buffer.insert_str(at, piece);
                (at..at, piece.len())
            }
            2 => {
                let at = buffer.floor_char_boundary(rng.below(buffer.len() + 1));
                let ch = (0x20 + (rng.next() % 0x5F) as u8) as char;
                buffer.insert(at, ch);
                (at..at, ch.len_utf8())
            }
            _ => {
                let seed = SEEDS[rng.below(SEEDS.len())];
                let at = buffer.floor_char_boundary(rng.below(buffer.len() + 1));
                buffer.insert_str(at, seed);
                (at..at, seed.len())
            }
        };
        check_invariants(&buffer);

        // Splice-reparse equivalence: identical green tree AND identical
        // errors versus the from-scratch parse.
        let deleted_range = rowan::TextRange::new(
            rowan::TextSize::from(deleted.start as u32),
            rowan::TextSize::from(deleted.end as u32),
        );
        let spliced = syntax::reparse(
            &previous,
            &buffer,
            deleted_range,
            rowan::TextSize::from(inserted as u32),
        );
        let scratch = syntax::parse(&buffer);
        assert_eq!(
            spliced.green(),
            scratch.green(),
            "splice != from-scratch tree: previous {:?}, deleted {deleted:?}, inserted {inserted}, now {buffer:?}",
            previous.text(),
        );
        assert_eq!(
            spliced.errors(),
            scratch.errors(),
            "splice errors != from-scratch errors after editing to {buffer:?}"
        );
        previous = scratch;

        // Keep the buffer bounded so the stream stays fast; resync the
        // baseline parse when truncating (an out-of-band "edit").
        if buffer.len() > 4096 {
            let cut = buffer.floor_char_boundary(2048);
            buffer.truncate(cut);
            previous = syntax::parse(&buffer);
        }
    }
}

/// Deep nesting must be refused gracefully (diagnostic, no stack overflow).
#[test]
fn deep_nesting_is_refused_not_fatal() {
    let deep = "(".repeat(2000) + "1" + &")".repeat(2000);
    let parse = syntax::parse(&deep);
    assert_eq!(parse.text(), deep);
    assert!(
        parse
            .errors()
            .iter()
            .any(|error| error.message.contains("too deep"))
    );

    let deep_annotation = format!("#: {}integer{}", "(".repeat(2000), ")".repeat(2000));
    let parse = syntax::parse(&deep_annotation);
    assert_eq!(parse.text(), deep_annotation);
}

/// A bounded sample of real corpus files for corpus-seeded fuzzing.
fn corpus_sample(count: usize) -> Option<Vec<String>> {
    let root = {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidate = syntax::testing::env_var("CORPUS_DIR")
            .ok_or(std::env::VarError::NotPresent)
            .map(PathBuf::from)
            .unwrap_or_else(|_| manifest.join("../../corpus"));
        candidate.is_dir().then_some(candidate)?
    };
    let mut files = Vec::new();
    let mut stack = vec![root.join("r-base"), root.join("cran")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "R") {
                files.push(path);
            }
        }
    }
    files.sort();
    // Deterministic spread across the corpus.
    let step = (files.len() / count.max(1)).max(1);
    let sample: Vec<String> = files
        .iter()
        .step_by(step)
        .take(count)
        .filter_map(|path| std::fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    (!sample.is_empty()).then_some(sample)
}
