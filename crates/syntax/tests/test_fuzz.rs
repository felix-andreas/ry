//! `FUZZ_ITERS` scales the per-generator budget; runs are deterministic.

use std::panic::catch_unwind;
use syntax::literate::r_source_of_literate;
use syntax::testing::{
    SplitMix64, check_parse_invariants as check_invariants, corpus_sample, fuzz_iterations,
    mutate_bytes, mutate_seed, program_corpus_sources, random_bytes, token_soup,
};
use syntax::{TextRange, TextSize, lex, parse, reparse};

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
    "count: integer <- 1L",
    "label: character | NULL = NULL",
    "f <- function(x: integer, y: list[named: double] = list()) -> double { x }",
    "g <- function<T: numeric>(...: T) -> T[] c(...)",
    "lapply(xs, \\(x: integer) -> integer x + 1L)",
    "{\n  step: fn(x: integer) -> integer <- function(x) x\n}",
    "p <- (x = 1, y = 2); t <- (1L, \"a\"); e <- (); one <- (x = 1,)",
    "s <- :Rect(width = 2, height = 3); n <- :None",
    "match (s) {\n  :Circle(radius = r) => r\n  :Rect(width = w, ..) if (w > 0) => w\n  (a, _) | (_, a) => a\n  -1L => 0; _ => NULL\n}",
    "x: :Some(integer) | :None | .. <- :None",
    "f <- function(p: Record{x: double}, t: Tuple{integer}) -> :Ok(value: double) p",
    "type UserId = integer\nalias Pair<T: numeric> = Tuple{T, T}\nu <- @UserId(5L)",
    "type Shape =\n  :Circle(radius: double) |\n  :Rect(width: double, height: double)\ns <- @Shape(:Circle(radius = 1))",
    "alias Point = list{\n  x: double, # across\n  y: double,\n}\np: @new Point <- list(x = 1, y = 2)",
    "x: @trust integer <- f(); y: @if-unknown character | NULL <- g()",
    "type <- \"a\"; alias(type = 1); type\nX <- 2",
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
    "type ",
    "alias ",
    "@trust ",
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
    "=>",
    "match",
    "..",
    ":None",
    ":Some(",
    "Record{",
    "Tuple{",
];

/// Inputs the coverage-guided `parse` target has broken. Unlike `SEEDS` these may hold syntax
/// errors; only the invariants must hold.
const REGRESSIONS: &[&str] = &[];

fn iterations() -> usize {
    fuzz_iterations(1500)
}

#[test]
fn seeds_hold_invariants() {
    for seed in SEEDS {
        check_invariants(seed);
    }
}

#[test]
fn fuzz_regressions_hold_invariants() {
    for input in REGRESSIONS {
        check_invariants(input);
    }
}

/// Edits the language-server simulator found where a splice reparse disagreed with a full
/// parse: `(old text, new text, deleted range in the old text, inserted length)`.
const SPLICE_REGRESSIONS: &[(&str, &str, (u32, u32), u32)] = &[(
    // The middle's quote opens a string the suffix's first quote closes; the lexer had ended
    // it at its line break because the middle alone holds no closing quote.
    "\nz <- c(1, 2, 3) |> rev()\nlabel: character <- \"日本語\"\ntota: integer, 3))\ndouble_it <- function(n) n * 2L\narea(\"a\", 3)\n",
    "\nz <- c(1, 2, 3) |> rev()\nlabel: ctotal <- NULL\n日本語\"\ntota: integer, 3))\ndouble_it <- function(n) \tn * 2L\narea(\"a\", 3)\n",
    (34, 102),
    70,
), (
    // The edit opens a parenthesis in the first statement that a later line closes, so the
    // statement boundary the old tree had after the first line no longer exists.
    "halve <- function(value: double) -> double value / 2\nif (TRU<-E)<-ats)\nrea(1, 2) else NUL)L\n",
    "halve <- function(value(: double) -> double value / 2\nif (TRU<-E)<-alibrary(stats)\nts)\nrea(1, 2) else NUL)L\n",
    (23, 67),
    60,
)];

#[test]
fn splice_regressions_match_a_full_parse() {
    for (old, new, (start, end), inserted) in SPLICE_REGRESSIONS {
        let deleted = TextRange::new(TextSize::from(*start), TextSize::from(*end));
        let spliced = reparse(&parse(old), new, deleted, TextSize::from(*inserted));
        assert_eq!(spliced, parse(new), "splicing {old:?} into {new:?}");
    }
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
            .unwrap_or_else(|_| panic!("program corpus case `{id}` broke a parse invariant"));
    }
}

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
        let converted = r_source_of_literate(&document);
        assert_eq!(
            converted.len(),
            document.len(),
            "length changed for {document:?} -> {converted:?}"
        );
        let newlines =
            |text: &str| -> Vec<usize> { text.match_indices('\n').map(|(at, _)| at).collect() };
        assert_eq!(
            newlines(&document),
            newlines(&converted),
            "newlines moved for {document:?} -> {converted:?}"
        );
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

#[test]
fn fuzz_corpus_seeded() {
    let Some(files) = corpus_sample(24) else {
        eprintln!("fuzz_corpus_seeded: corpus not fetched; skipping");
        return;
    };
    let mut rng = SplitMix64(0xC0_2B_05);
    for _ in 0..iterations() / 3 {
        let source = &files[rng.below(files.len())];
        let mut bytes = mutate_bytes(&mut rng, source);
        if rng.below(2) == 0 && !bytes.is_empty() {
            bytes.truncate(rng.below(bytes.len()));
        }
        check_invariants(&String::from_utf8_lossy(&bytes));
    }
}

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

#[test]
fn deep_nesting_is_refused_not_fatal() {
    let deep = "(".repeat(2000) + "1" + &")".repeat(2000);
    let deep_parse = parse(&deep);
    assert_eq!(deep_parse.text(), deep);
    assert!(
        deep_parse
            .errors()
            .iter()
            .any(|error| error.message.contains("too deep"))
    );

    let deep_annotation = format!("#: {}integer{}", "(".repeat(2000), ")".repeat(2000));
    assert_eq!(parse(&deep_annotation).text(), deep_annotation);
}

fn run_random_bytes(budget: usize) {
    let mut rng = SplitMix64(0xDEC0_DE01);
    for _ in 0..budget {
        check_invariants(&random_bytes(&mut rng));
    }
}

fn run_token_soup(budget: usize) {
    let mut rng = SplitMix64(0x50_D4_11);
    for _ in 0..budget {
        check_invariants(&token_soup(&mut rng, ALPHABET));
    }
}

fn run_seed_mutations(budget: usize) {
    let mut rng = SplitMix64(0x5EED_F00D);
    for _ in 0..budget {
        check_invariants(&mutate_seed(&mut rng, SEEDS, 16));
    }
}

fn run_token_level_mutations(budget: usize) {
    let mut rng = SplitMix64(0x70C3_2015);
    for _ in 0..budget {
        let seed = SEEDS[rng.below(SEEDS.len())];
        let (tokens, _) = lex(seed);
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
                    spans.insert(at, spans[at]);
                }
                _ if spans.len() > 1 => {
                    let first = rng.below(spans.len());
                    let second = rng.below(spans.len());
                    spans.swap(first, second);
                }
                _ => {}
            }
        }
        check_invariants(&spans.concat());
    }
}

fn run_edit_stream(steps: usize) {
    let mut rng = SplitMix64(0xED17_57EA);
    let mut buffer = String::from("f <- function(x, y) {\n  total <- x + y\n  total * 2\n}\n");
    let mut previous = parse(&buffer);
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
                let character = (0x20 + (rng.next_u64() % 0x5F) as u8) as char;
                buffer.insert(at, character);
                (at..at, character.len_utf8())
            }
            _ => {
                let seed = SEEDS[rng.below(SEEDS.len())];
                let at = buffer.floor_char_boundary(rng.below(buffer.len() + 1));
                buffer.insert_str(at, seed);
                (at..at, seed.len())
            }
        };
        check_invariants(&buffer);

        let deleted_range = TextRange::new(
            TextSize::from(deleted.start as u32),
            TextSize::from(deleted.end as u32),
        );
        let spliced = reparse(
            &previous,
            &buffer,
            deleted_range,
            TextSize::from(inserted as u32),
        );
        let scratch = parse(&buffer);
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

        // Truncation is an out-of-band edit, so the baseline is reparsed.
        if buffer.len() > 4096 {
            let cut = buffer.floor_char_boundary(2048);
            buffer.truncate(cut);
            previous = parse(&buffer);
        }
    }
}
