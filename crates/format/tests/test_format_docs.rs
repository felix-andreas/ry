//! The formatter documentation page is generated from
//! `tests/formatter.template.md`: every ```r example is run through the
//! shipping formatter and the result rendered into
//! `docs/src/content/docs/reference/formatting-rules.md`, so the page can never drift from
//! actual behavior. `RY_BLESS=1` rewrites the page; otherwise the test
//! fails when it is out of date.

use format::{Config, format};
use regex::{Captures, Regex};
use std::fs;

#[test]
fn documentation_examples() {
    let markdown =
        fs::read_to_string("tests/formatter.template.md").expect("read the docs template");

    let code_blocks = Regex::new(r"(?m)```r\n([\s\S]*?)```").expect("valid regex");
    const BEFORE: &str = "R CODE IN THIS FILE IS FORMATTED AND SAVED TO docs/src/content/docs/reference/formatting-rules.md";
    const AFTER: &str = "THIS FILE IS GENERATED AUTOMATICALLY. \
MAKE CHANGES TO crates/format/tests/formatter.template.md INSTEAD";

    let formatted = code_blocks
        .replace_all(&markdown, |captures: &Captures| {
            let text = captures.get(1).expect("capture group").as_str();
            text.split_once('\n')
                .and_then(|(first, rest)| first.strip_prefix('#').map(|comment| (comment, rest)))
                .and_then(|(comment, text)| {
                    comment
                        .split_once(':')
                        .map(|(_name, directive)| (directive.trim(), text))
                })
                .map(|(directive, text)| {
                    if directive == "skip" {
                        return text.to_owned();
                    }
                    let code = format(text, Config::default()).expect("template example formats");
                    match directive {
                        "compare" => {
                            format!("# Before formatting\n{text}\n# After formatting\n{code}")
                        }
                        "format" => code,
                        other => panic!("unknown template directive `{other}`"),
                    }
                })
                .map(|content| format!("```r\n{content}```"))
                // Falling back to the input here would publish unformatted R on
                // the page as if the formatter had produced it — the one failure
                // this generator exists to prevent.
                .unwrap_or_else(|| {
                    panic!(
                        "template block has no `# name: directive` header line, so it would be \
                         published unformatted:\n{text}"
                    )
                })
        })
        .replace(BEFORE, AFTER);

    let path = "../../docs/src/content/docs/reference/formatting-rules.md";
    if syntax::testing::env_var("BLESS").as_deref() == Some("1") {
        // The docs tree is absent under a sandboxed build where the source
        // tree is read-only; there is nothing to rewrite then.
        if fs::metadata(path).is_ok() {
            fs::write(path, &formatted).expect("write the generated docs page");
        }
        return;
    }

    // When the generated page is available, assert it matches; under a
    // sandboxed build without the docs tree there is nothing to compare.
    if let Ok(existing) = fs::read_to_string(path) {
        assert_eq!(
            existing, formatted,
            "docs/src/content/docs/reference/formatting-rules.md is out of date; rerun with RY_BLESS=1"
        );
    }
}
