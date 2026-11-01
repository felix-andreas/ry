use std::path::Path;
use syntax::parse;
use syntax::testing::run_fixture_suite;

#[test]
fn syntax_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/syntax");
    run_fixture_suite(&suite, &|source| parse(source).debug_dump());
}

/// tree-sitter-r's test corpus, with ry's own trees as the expectations.
#[test]
fn tree_sitter_r_corpus_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsr");
    run_fixture_suite(&suite, &|source| parse(source).debug_dump());
}

/// The plain R each inline form erases to, as the type system reference documents it.
#[test]
fn erasure_of_inline_forms() {
    let cases = [
        ("count: integer <- 0L\n", "count <- 0L\n"),
        // Whitespace around an erased annotation stays.
        ("f <- function<T>(x: T) -> T x\n", "f <- function(x)  x\n"),
        ("id <- @UserId(5L)\n", "id <- (5L)\n"),
        ("p <- @Box<integer>((x = 1,))\n", "p <- ((x = 1,))\n"),
        ("x: @trust integer <- v\n", "x <- v\n"),
        ("type UserId = integer\n", "#: @type UserId {integer}\n"),
        (
            "alias Box<T: numeric> = list{value: T}\n",
            "#: @alias Box<T: numeric> {list{value: T}}\n",
        ),
        (
            "alias Shape =\n  | :Circle(radius: double)\n  | :Empty\n",
            "#: @alias Shape {| :Circle(radius: double)\n#:   | :Empty}\n",
        ),
        ("type <- 1\nalias(type)\n", "type <- 1\nalias(type)\n"),
    ];
    for (input, expected) in cases {
        let parse = parse(input);
        assert!(parse.errors().is_empty(), "{input:?}: {:?}", parse.errors());
        assert_eq!(syntax::erase_inline_annotations(&parse), expected, "{input:?}");
    }
}
