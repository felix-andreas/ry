//! Each generated case is one seed of [`for_each_seed`], so `FUZZ_SEED` replays a failure and
//! `FUZZ_ITERS` sets the budget.

use {
    std::{panic::catch_unwind, sync::LazyLock},
    syntax::{
        NodeOrToken, Parse, SyntaxNode, TextRange, TextSize, lex,
        literate::r_source_of_literate,
        parse, reparse,
        testing::{
            SplitMix64, check_parse_invariants as check_invariants, corpus_sample,
            fixture_case_sources, for_each_seed, generate_program, mutate_bytes, mutate_seed,
            program_corpus_sources, random_bytes, small_edit, splice_of, split_files, token_soup,
        },
    },
};

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
    "a <- [1, 2, 3]; e <- []; n <- [[1L], [2L, 3L]]; a[[1L]]; a[2]\n[4]",
    "xs: Array<double> <- -[1.5, 2] * [3, 4][1L]; (x\n[1]); f([x, y],)",
    "n <- 5i64 + 0xFFi64 * 1.5f64 - 1e3f64 / 2i64",
    "g <- f\"Hello {name}, {{n}} {f'{n + 1}'}\"",
    "p <- (x = try Str.to_i64(a)$v, y = try b); q <- (..p, x = 3)",
    "match (xs) {\n  [first, .., last] as all => all\n  [] => 0i64\n  (x =, ..rest) => rest\n}",
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
    "@pure",
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
    "[",
    "[[",
    "]",
    "]]",
    "Array<",
    "f\"",
    "f'",
    "\"",
    "5i64",
    "1.5f64",
    "try ",
    "..rest",
    " as ",
];

/// Inputs the coverage-guided `parse` target has broken. Unlike `SEEDS` these may hold syntax
/// errors; only the invariants must hold.
const REGRESSIONS: &[&str] = &[
    // A `#:` behind a type definition: erasure must not leave it as a second annotation on the
    // definition's comment line, nor alone above the next statement.
    "alias Shape = :Circle(radius: double) |\n      :Rect(width: double, height: double)#: character\ny <- x\n",
    "#: @alias Size integer\nalias Size = integer #: character\ny <- 1L\n",
];

const ITERATIONS: usize = 1500;

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

/// Edits the language-server simulator and the small-edit arm found where a splice reparse
/// disagreed with a full parse: `(old text, new text, deleted range in the old text, inserted length)`.
const SPLICE_REGRESSIONS: &[(&str, &str, (u32, u32), u32)] = &[
    (
        // The middle's quote opens a string the suffix's first quote closes; the lexer had ended
        // it at its line break because the middle alone holds no closing quote.
        "\nz <- c(1, 2, 3) |> rev()\nlabel: character <- \"日本語\"\ntota: integer, 3))\ndouble_it <- function(n) n * 2L\narea(\"a\", 3)\n",
        "\nz <- c(1, 2, 3) |> rev()\nlabel: ctotal <- NULL\n日本語\"\ntota: integer, 3))\ndouble_it <- function(n) \tn * 2L\narea(\"a\", 3)\n",
        (34, 102),
        70,
    ),
    (
        // The edit opens a parenthesis in the first statement that a later line closes, so the
        // statement boundary the old tree had after the first line no longer exists.
        "halve <- function(value: double) -> double value / 2\nif (TRU<-E)<-ats)\nrea(1, 2) else NUL)L\n",
        "halve <- function(value(: double) -> double value / 2\nif (TRU<-E)<-alibrary(stats)\nts)\nrea(1, 2) else NUL)L\n",
        (23, 67),
        60,
    ),
    (
        // The third line's `(` is never closed, so recovery ends its group at the line break;
        // the `)` the edit adds four lines later closes it in a full parse.
        "m\ne\n (ict\narea(\"a\", 3totaltotal <é👋library(stats)\n- NULL\n <- NULL\naract👋é👋👋,tiltotal <- NULL\ns)# comment\n 2)\nilibr日本ary(stats{)medi\nea(1,melibrary(stat\"s)\n\t$weightsN👋ULL\n",
        "m\ne\n (ict\narea(\"a\", 3totaltotal <é👋library(stats)\n- NUL)L\n <- NULL\naract👋é👋👋,tiltotal <- NULL\ns)# comment\n 2)\nilibr日本ary(stats{)medi\nea(1,melibrary(stat\"s)\n\t$weightsN👋ULL\n",
        (59, 59),
        1,
    ),
    (
        // The first line's `[[` is closed by one `]` only, which the parser reports at the `[[`
        // rather than as an unclosed group, so the splice guard must recognize that message too.
        "x[[1]\ny <- 2\nz <- 3\n",
        "x[[1]\ny <- 2\nz <- 3]\n",
        (19, 19),
        1,
    ),
    (
        // A list type that breaks off reports its error on the next line, which is in the middle,
        // where only the statement before the prefix's line break can report it.
        "alias S = list{\n  kind: char\nacter, # c\n  size: T,\n}",
        "alias S = list{\n  kind: char\nacter,# c\n  size: T,\n}",
        (35, 36),
        0,
    ),
    (
        // A type continues onto a line that starts with `|`, so the line break after the alias is
        // a statement boundary only while the next line starts with something else.
        "alias A = integer\nfoo\nx <- 1\n",
        "alias A = integer\n| character\nx <- 1\n",
        (18, 21),
        11,
    ),
    (
        // The record type on the first line is open, and the edit closes it on the second.
        "type P = Record{x: double\nz\ny <- 1\n",
        "type P = Record{x: double\n, y: double}\ny <- 1\n",
        (26, 27),
        12,
    ),
    (
        // The body of the function is the next line until the edit makes that line part of the
        // return type; the middle alone then ends without a body.
        "f <- function(x) -> integer\nfoo\ny <- 1\n",
        "f <- function(x) -> integer\n| character\ny <- 1\n",
        (28, 31),
        11,
    ),
    (
        // The unclosed call ends where a line assigns; once that line no longer assigns, the call
        // runs on into the suffix.
        "f(a,\nb c\ny <- 2\nz\n",
        "f(a,\nb c\ny + 2\nz\n",
        (11, 13),
        1,
    ),
    (
        // The definition's error sits at the end of the file, in the suffix, and the edit removes
        // the definition.
        "type UserId = \n# f <- function(id) id",
        "# type UserId = \n# f <- function(id) id",
        (0, 0),
        2,
    ),
    (
        // The tag type's error sits on the next line, in the suffix, and the edit removes the type.
        "type Shape = :Circle(radius: double | :Empty\ns <- 1\narea <- 1\n",
        "\ns <- 1\narea <- 1\n",
        (0, 44),
        0,
    ),
    (
        // The annotation's error names the token its region stopped at, which the edit replaces.
        "#: @alias A {integer\n\n# c\norigin <- list(v = 1)\ny <- 2\n",
        "#: @alias A {integer\n\n# c\nalias <- list(v = 1)\ny <- 2\n",
        (26, 32),
        5,
    ),
    (
        // The `#:` function type runs to the end of the text, so its unclosed `(` is blamed on
        // the end of input before the edit and on the appended `{` after it.
        "#: fn(#   a : integer,\n# #:   b : double\n# )->logical",
        "#: fn(#   a : integer,\n# #:   b : double\n# )->logical{\n  {\n    1L\n  }\n}",
        (53, 53),
        18,
    ),
    (
        // An f-string's `{}` spans lines, so the last line lexes as its closing quote only
        // while the first line opens it.
        "g <in- f\"Hello {naif (a > 1) b else cmz <- 2i + 0x1FLe}, {{n}} {f'{nbase**size + 1}'}{\n  if (a) 1\n  else 2\n}\"coux <<- y ->> znt:m[, y ~ a + b1]:= integerL",
        "g <in- fHello {naif (a > 1) b else cmz <- 2i + 0x1FLe}, {{n}} {f'{nbase**size + 1}'}{\n  if (a) 1\n  else 2\n}\"coux <<- y ->> znt:m[, y ~ a + b1]:= integerL",
        (8, 9),
        0,
    ),
    (
        // Deleting the `]` between a lone `\r` and a `\n` leaves one `\r\n` line break.
        "a ~repeat/ Wbm[, 1x <- 1\r\ny <- 2\r]\nfo",
        "a ~repeat/ Wbm[, 1x <- 1\r\ny <- 2\r\nfo",
        (33, 34),
        0,
    ),
];

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

/// GNU R reads a file with CRLF or lone CR line endings as it reads one with LF endings, and a
/// Windows checkout can give any embedded or user source CRLF endings. Each ending must parse
/// into the same tree, with the same errors, as LF, and round-trip byte for byte.
#[test]
fn crlf_and_cr_line_endings_parse_like_lf() {
    let mut sources: Vec<(String, String)> = program_corpus_sources();
    sources.extend(
        SEEDS
            .iter()
            .map(|seed| ("seed".to_owned(), (*seed).to_owned())),
    );
    for (id, source) in sources.iter().filter(|(_, source)| !source.contains('\r')) {
        let expected = shape(&parse(source));
        for ending in ["\r\n", "\r"] {
            let converted = source.replace('\n', ending);
            let actual = parse(&converted);
            assert_eq!(actual.text(), converted, "`{id}` with {ending:?} endings");
            assert_eq!(
                shape(&actual),
                expected,
                "`{id}` with {ending:?} endings parses differently from LF"
            );
        }
    }
}

/// Every node and token kind in order, with each token's text in LF spelling, and the error
/// messages: everything about a parse but its offsets.
fn shape(parse: &Parse) -> (Vec<String>, Vec<String>) {
    fn walk(node: &SyntaxNode, out: &mut Vec<String>) {
        out.push(format!("{:?}", node.kind()));
        for child in node.children_with_tokens() {
            match child {
                NodeOrToken::Node(child) => walk(&child, out),
                NodeOrToken::Token(token) => out.push(format!(
                    "{:?} {}",
                    token.kind(),
                    token.text().replace("\r\n", "\n").replace('\r', "\n")
                )),
            }
        }
    }
    let mut elements = Vec::new();
    walk(&parse.syntax_node(), &mut elements);
    let errors = parse
        .errors()
        .iter()
        .map(|error| error.message.clone())
        .collect();
    (elements, errors)
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
    for_each_seed(ITERATIONS, |seed| {
        let mut rng = SplitMix64(0x11_7E_A7 ^ seed);
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
    });
}

#[test]
fn fuzz_random_bytes() {
    run_random_bytes(ITERATIONS);
}

#[test]
fn fuzz_token_soup() {
    run_token_soup(ITERATIONS);
}

#[test]
fn fuzz_seed_mutations() {
    run_seed_mutations(ITERATIONS);
}

#[test]
fn fuzz_token_level_mutations() {
    run_token_level_mutations(ITERATIONS);
}

#[test]
fn fuzz_edit_stream() {
    run_edit_stream(40);
}

#[test]
fn fuzz_generated_programs() {
    run_generated_programs(400);
}

#[test]
fn fuzz_small_edits() {
    run_small_edits(300);
}

#[test]
fn fixture_sources_hold_invariants() {
    for (id, source) in fixture_case_sources() {
        let files = split_files(&source).unwrap_or_else(|| vec![(String::new(), source)]);
        for (_, text) in files {
            catch_unwind(|| check_invariants(&text))
                .unwrap_or_else(|_| panic!("fixture case `{id}` broke a parse invariant"));
        }
    }
}

#[test]
fn fuzz_corpus_seeded() {
    let Some(files) = corpus_sample(24) else {
        eprintln!("fuzz_corpus_seeded: corpus not fetched; skipping");
        return;
    };
    for_each_seed(ITERATIONS / 3, |seed| {
        let mut rng = SplitMix64(0xC0_2B_05 ^ seed);
        let source = &files[rng.below(files.len())];
        let mut bytes = mutate_bytes(&mut rng, source);
        if rng.below(2) == 0 && !bytes.is_empty() {
            bytes.truncate(rng.below(bytes.len()));
        }
        check_invariants(&String::from_utf8_lossy(&bytes));
    });
}

#[test]
#[ignore = "deep fuzz; run explicitly with a large FUZZ_ITERS"]
fn fuzz_deep() {
    let budget = 20_000;
    run_random_bytes(budget);
    run_token_soup(budget);
    run_seed_mutations(budget);
    run_token_level_mutations(budget);
    run_edit_stream(budget / 40);
    run_generated_programs(budget / 4);
    run_small_edits(budget / 4);
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
    for_each_seed(budget, |seed| {
        let mut rng = SplitMix64(0xDEC0_DE01 ^ seed);
        check_invariants(&random_bytes(&mut rng));
    });
}

fn run_token_soup(budget: usize) {
    for_each_seed(budget, |seed| {
        let mut rng = SplitMix64(0x50_D4_11 ^ seed);
        check_invariants(&token_soup(&mut rng, ALPHABET));
    });
}

fn run_seed_mutations(budget: usize) {
    for_each_seed(budget, |seed| {
        let mut rng = SplitMix64(0x5EED_F00D ^ seed);
        check_invariants(&mutate_seed(&mut rng, SEEDS, 16));
    });
}

fn run_token_level_mutations(budget: usize) {
    for_each_seed(budget, |seed| {
        let mut rng = SplitMix64(0x70C3_2015 ^ seed);
        let sample = POOL[rng.below(POOL.len())].as_str();
        let (tokens, _) = lex(sample);
        if tokens.is_empty() {
            return;
        }
        let mut spans: Vec<&str> = Vec::with_capacity(tokens.len());
        let mut offset = 0usize;
        for token in &tokens {
            let len = u32::from(token.len) as usize;
            spans.push(&sample[offset..offset + len]);
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
    });
}

/// A generated program is valid by construction, so a syntax error in one is a bug in the
/// parser or in the generator.
fn run_generated_programs(budget: usize) {
    for_each_seed(budget, |seed| {
        let mut rng = SplitMix64(0x6E4E_0001 ^ seed);
        let program = generate_program(&mut rng);
        let parsed = parse(&program);
        assert!(
            parsed.errors().is_empty(),
            "a generated program does not parse: {:?}\n{program}",
            parsed.errors()
        );
        check_invariants(&program);
    });
}

/// Sessions of edits the size a keystroke or a refactoring step makes, each from a real or a
/// generated program: every state holds the parse invariants, and the splice reparse of each
/// edit equals a full parse.
fn run_small_edits(budget: usize) {
    for_each_seed(budget, |seed| {
        let mut rng = SplitMix64(0x5A11_ED17 ^ seed);
        let mut text = match rng.below(3) {
            0 => generate_program(&mut rng),
            _ => POOL[rng.below(POOL.len())].clone(),
        };
        let mut previous = parse(&text);
        for _ in 0..8 {
            let donor = POOL[rng.below(POOL.len())].as_str();
            let edited = small_edit(&mut rng, &text, donor);
            check_invariants(&edited);
            let (deleted, inserted) = splice_of(&text, &edited);
            assert_splice_matches(&previous, &edited, deleted, inserted);
            previous = parse(&edited);
            text = edited;
        }
    });
}

/// Sessions of twenty byte-level edits, each from a program of the pool. A session is short
/// because random byte edits degrade a program into token soup within a few dozen steps, and
/// the splice reparse matters on text that still has statements to keep.
fn run_edit_stream(sessions: usize) {
    for_each_seed(sessions, |seed| {
        let mut rng = SplitMix64(0xED17_57EA ^ seed);
        let mut buffer = POOL[rng.below(POOL.len())].clone();
        buffer.truncate(buffer.floor_char_boundary(4096));
        let mut previous = parse(&buffer);
        for _ in 0..20 {
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
            let deleted = TextRange::new(
                TextSize::from(deleted.start as u32),
                TextSize::from(deleted.end as u32),
            );
            assert_splice_matches(&previous, &buffer, deleted, TextSize::from(inserted as u32));
            previous = parse(&buffer);
        }
    });
}

fn assert_splice_matches(previous: &Parse, text: &str, deleted: TextRange, inserted: TextSize) {
    let spliced = reparse(previous, text, deleted, inserted);
    let scratch = parse(text);
    assert_eq!(
        spliced.tree(),
        scratch.tree(),
        "splice != from-scratch tree: previous {:?}, deleted {deleted:?}, inserted {inserted:?}, now {text:?}",
        previous.text(),
    );
    assert_eq!(
        spliced.errors(),
        scratch.errors(),
        "splice errors != from-scratch errors: previous {:?}, deleted {deleted:?}, inserted {inserted:?}, now {text:?}",
        previous.text(),
    );
    assert_eq!(
        spliced.lookahead(),
        scratch.lookahead(),
        "splice lookahead != from-scratch lookahead: previous {:?}, deleted {deleted:?}, inserted {inserted:?}, now {text:?}",
        previous.text(),
    );
}

/// The programs the mutating arms start from: the hand-written seeds, every fixture case of
/// every suite (a multi-file case as its files), and the mined program corpus.
static POOL: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut pool: Vec<String> = SEEDS.iter().map(|seed| (*seed).to_owned()).collect();
    for (_, source) in fixture_case_sources() {
        match split_files(&source) {
            Some(files) => pool.extend(files.into_iter().map(|(_, text)| text)),
            None => pool.push(source),
        }
    }
    pool.extend(
        program_corpus_sources()
            .into_iter()
            .map(|(_, source)| source),
    );
    pool
});
