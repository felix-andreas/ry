//! The signature a function literal has before any type is resolved, which the checker and the
//! runtime both read through `syntax::annotation::signature`.

use syntax::{SyntaxKind, annotation::signature};

/// The signature of the first function literal in `source`, as `(formal types) -> result` with
/// `_` for a slot nothing types.
fn resolved(source: &str, inline: bool) -> String {
    let root = syntax::parse(source).syntax_node();
    let function = root
        .descendants()
        .find(|node| node.kind() == SyntaxKind::FUNCTION_DEF)
        .unwrap_or_else(|| panic!("no function literal in {source:?}"));
    let signature = signature(&function, inline);
    let text = |slot: &Option<syntax::SyntaxNode>| {
        slot.as_ref()
            .map_or("_".to_owned(), |ty| ty.text().to_string())
    };
    let formals: Vec<String> = signature.formals.iter().map(text).collect();
    format!("({}) -> {}", formals.join(", "), text(&signature.ret))
}

#[test]
fn a_signature_counts_the_same_wherever_it_is_written() {
    let cases = [
        (
            "f <- function(n: I64, m: F64) -> Str n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nf = function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nfunction(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: (fn(n: I64, m: F64) -> Str)\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: @param n {I64}\n#: @param m {F64}\n#: @return {Str}\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "f: fn(n: I64, m: F64) -> Str <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "f: fn(n: I64, m: F64) -> Str <-\n  function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nfunction(n, m) n -> f",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nf <<- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: <T> fn(n: T, m: F64) -> T\nf <- function(n, m) n",
            "(T, F64) -> T",
        ),
        (
            "#: @forall T\n#: @param n {T}\n#: @param m {F64}\n#: @return {T}\nf <- function(n, m) n",
            "(T, F64) -> T",
        ),
        (
            "#: @pure\n#: fn(n: I64, m: F64) -> Str\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        // A parameter takes the declared type of its own name, in any order.
        (
            "#: fn(m: F64, n: I64) -> Str\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        // A type declared without a name goes to the next formal no name claimed.
        (
            "#: fn(I64, F64) -> Str\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(F64, n: I64) -> Str\nf <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, ...: Str) -> Str\nf <- function(n, ...) n",
            "(I64, Str) -> Str",
        ),
        // A type written on the literal wins its slot, and the declaration fills the rest.
        (
            "#: fn(n: I64, m: F64) -> Str\nf <- function(n: Bool, m) n",
            "(Bool, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nf <- function(n, m) -> Bool n",
            "(I64, F64) -> Bool",
        ),
        // The type on the name replaces the block as a whole.
        (
            "#: fn(n: I64, m: F64) -> Str\nf: fn(n: Bool) -> Bool <- function(n, m) n",
            "(Bool, _) -> Bool",
        ),
        // A function nested in a typed one is read by the same rule.
        (
            "outer <- function(xs: List<I64>) -> I64 {\n  #: fn(n: I64) -> I64\n  inner <- function(n) n\n  inner(1)\n}",
            "(List<I64>) -> I64",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(resolved(source, true), expected, "{source}");
    }
}

#[test]
fn what_declares_nothing() {
    let untyped = [
        "f <- function(n, m) n",
        // A blank line or a plain comment detaches the block.
        "#: fn(n: I64, m: F64) -> Str\n\nf <- function(n, m) n",
        "#: fn(n: I64, m: F64) -> Str\n# a note\nf <- function(n, m) n",
        // The block belongs to the statement, not to a literal somewhere inside it.
        "#: fn(n: I64, m: F64) -> Str\nf <- g(function(n, m) n)",
        // A coercion says how the value gets its type, a definition declares another name, and
        // an alias names a function type without writing it.
        "#: @trust fn(n: I64, m: F64) -> Str\nf <- function(n, m) n",
        // A coercion governs the value wherever it is written, so the types on the literal
        // and on the name are not a signature either.
        "#: @trust fn(n: I64, m: F64) -> Str\nf <- function(n: I64, m: F64) -> Str n",
        "#: @trust fn(n: I64, m: F64) -> Str\nf: fn(n: I64, m: F64) -> Str <- function(n, m) n",
        "f: @trust fn(n: I64, m: F64) -> Str <- function(n: I64, m: F64) -> Str n",
        "#: @if-unknown fn(n: I64, m: F64) -> Str\nf <- function(n: I64, m: F64) -> Str n",
        // Directives out of order are an error, and the block declares nothing.
        "#: @return {Str}\n#: @param n {I64}\n#: @param m {F64}\nf <- function(n, m) n",
        "#: @param n {I64}\n#: @param m {F64}\n#: @return {Str}\n#: @return {Str}\nf <- function(n, m) n",
        "#: @param n {T}\n#: @param m {F64}\n#: @forall T\n#: @return {T}\nf <- function(n, m) n",
        "#: @alias Pick {fn(n: I64, m: F64) -> Str}\nf <- function(n, m) n",
        "#: Pick\nf <- function(n, m) n",
        "f: Pick <- function(n, m) n",
        // Two compact annotations in one block are refused.
        "#: fn(n: I64, m: F64) -> Str\n#: fn(n: I64, m: F64) -> Str\nf <- function(n, m) n",
    ];
    for source in untyped {
        assert_eq!(resolved(source, true), "(_, _) -> _", "{source}");
    }
}

#[test]
fn with_inline_annotations_off_only_the_block_counts() {
    let cases = [
        ("f <- function(n: I64, m: F64) -> Str n", "(_, _) -> _"),
        (
            "f: fn(n: I64, m: F64) -> Str <- function(n, m) n",
            "(_, _) -> _",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nf <- function(n: Bool, m) -> Bool n",
            "(I64, F64) -> Str",
        ),
        (
            "#: fn(n: I64, m: F64) -> Str\nf: fn(n: Bool) -> Bool <- function(n, m) n",
            "(I64, F64) -> Str",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(resolved(source, false), expected, "{source}");
    }
}
