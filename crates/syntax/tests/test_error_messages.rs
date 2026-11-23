use {
    std::{iter::once, path::Path},
    syntax::{parse, testing::run_fixture_suite},
};

#[test]
fn error_messages() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/errors");
    run_fixture_suite(&suite, &render_errors);
}

fn render_errors(source: &str) -> String {
    let parsed = parse(source);
    if parsed.errors().is_empty() {
        return "no errors".to_owned();
    }

    let line_starts: Vec<usize> = once(0)
        .chain(
            source
                .char_indices()
                .filter(|&(_, character)| character == '\n')
                .map(|(at, _)| at + 1),
        )
        .collect();
    let line_of = |offset: usize| line_starts.partition_point(|&start| start <= offset) - 1;

    let mut out = String::new();
    for error in parsed.errors() {
        let start = u32::from(error.range.start()) as usize;
        let end = u32::from(error.range.end()) as usize;
        let line = line_of(start.min(source.len().saturating_sub(1)));
        let line_start = line_starts[line];
        let line_text = source[line_start..].lines().next().unwrap_or("");
        let column = source[line_start..start.min(source.len())].chars().count();
        let width = source
            .get(start..end.min(line_start + line_text.len()).max(start))
            .map_or(1, |text| text.chars().count().max(1));
        out.push_str(&format!(
            "error: {}\n --> {}:{}\n  | {line_text}\n  | {}{}\n",
            error.message,
            line + 1,
            column + 1,
            " ".repeat(column),
            "^".repeat(width)
        ));
    }
    out
}

#[test]
fn a_console_reads_on_only_when_the_input_ended_too_early() {
    let unfinished = [
        "f <- function(x) {\n",
        "print(1)\nf <- function(x) {\n  x +\n",
        "c(1,\n",
        "x <-\n",
        "x +\n",
        "x$\n",
        "f(a::\n",
        "if (TRUE) {\n  1\n",
        // A check R makes when a construct closes does not end the wait for the rest.
        "f(x |> y,\n",
        "function(a, a\n",
        "c(\"\" = 1,\n",
        "1 |> 2 +\n",
        "f(g(_),\n",
        "{\n  1 |> 2\n",
        // A `#:` block annotates the line after it, so the console waits for that line.
        "#: fn(x: I64) -> I64\n",
        "#: fn(x: I64) -> I64",
        "x <- 1\n#: @param x {I64}\n#: @return {I64}\n",
        "#: @trust fn(x: I64) -> I64\n",
    ];
    let finished = [
        "x <- 1\n",
        "{ x y\n",
        "f(1))\n",
        "{ f(1))\n",
        // R skips no line break after `::` or after a `\` that starts a statement.
        "a::\n",
        "a::",
        "{a::\n",
        "x; \\\n",
        "#: fn(x: \nx <- 1\n",
        "",
        // A blank line detaches the block, a broken block is only a comment, a block that
        // declares a type stands alone, and a block trailing code belongs to no later line.
        "#: fn(x: I64) -> I64\n\n",
        "#: fn(x: \n",
        "#: @alias Id {I64}\n",
        "x <- 1 #: I64\n",
        "#: fn(x: I64) -> I64\nf <- function(x) x\n",
        // A complete construct that breaks one of ry's own rules is an error to report now.
        "(1L, y = 2)\n",
        "(a = 1, a = 2)\n",
        "[x = 1]\n",
        ":Some()\n",
        // So is a check R makes on a construct that closed, and one its lexer makes.
        "x |> y\n",
        "function(a, a) 1\n",
        "f(g(_))\n",
        "f(``,\n",
        "f(\"\\q\",\n",
    ];
    for text in unfinished {
        assert!(syntax::input_is_unfinished(text), "unfinished: {text:?}");
    }
    for text in finished {
        assert!(!syntax::input_is_unfinished(text), "finished: {text:?}");
    }
}

// A fixture's source ends at its last code, so the cases with trailing line breaks live here.
#[test]
fn an_error_at_the_end_after_a_line_break_points_past_the_last_code() {
    for text in [
        "answer <- if (ready)\n",
        "answer <- if (ready)\n\n\n",
        "answer <- if (ready)  \n  \n",
    ] {
        let parsed = parse(text);
        let [error] = parsed.errors() else {
            panic!("one error in {text:?}: {:?}", parsed.errors());
        };
        assert_eq!(u32::from(error.range.start()), 20, "{text:?}");
        assert!(error.range.is_empty(), "{text:?}");
        assert!(error.incomplete, "{text:?}");
    }
}

// A fixture file is text, so the NUL cases live here.
#[test]
fn a_nul_byte_is_an_error_in_a_string_only() {
    for (text, offset) in [
        ("x <- \"a\0b\"", 7),
        ("x <- 'a\0b'", 7),
        ("x <- r\"(a\0b)\"", 9),
    ] {
        let parsed = parse(text);
        let [error] = parsed.errors() else {
            panic!("one error in {text:?}: {:?}", parsed.errors());
        };
        assert_eq!(
            error.message,
            "a NUL byte, which no R string can hold; remove it"
        );
        assert_eq!(u32::from(error.range.start()), offset, "{text:?}");
        assert_eq!(u32::from(error.range.len()), 1, "{text:?}");
    }
    // R cuts a name at a NUL and reads past one in a comment.
    let accepted = [
        "`a\0b` <- 1",
        "1 %a\0b% 2",
        "x <- 1 # a\0b",
        "# a\0b\nx <- 1",
        "#: fn(x: \0) -> I64\nf <- function(x) x",
    ];
    for text in accepted {
        let parsed = parse(text);
        assert!(
            parsed.errors().iter().all(|error| error.in_annotation),
            "{text:?}"
        );
    }
}
