//! Literate R documents: R Markdown (`.Rmd`), Quarto (`.qmd`) and Sweave
//! (`.Rnw`).
//!
//! Analysis reads one of these as the R program it contains, by blanking
//! everything that is not R: every prose byte becomes a space and every newline
//! stays a newline. That keeps the converted text **byte-for-byte the same
//! length** as the original, so no range needs translating at either end.
//! That covers every range a diagnostic reports and every position the editor
//! sends. It also makes
//! the whole document one R script, which is what it is at knit time: a chunk
//! sees the bindings earlier chunks created.

/// Whether a file extension names a literate R document.
pub fn is_literate_extension(extension: &str) -> bool {
    matches!(
        extension,
        "Rmd" | "rmd" | "qmd" | "Rnw" | "rnw" | "Snw" | "snw"
    )
}

/// The R program a literate document contains, with prose blanked to spaces so
/// offsets match the original text exactly.
pub fn r_source_of_literate(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_chunk = false;
    for line in split_keeping_terminators(text) {
        let body = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[body.len()..];
        let trimmed = body.trim_start();
        let keep = if in_chunk {
            if is_chunk_end(trimmed) {
                in_chunk = false;
                false
            } else {
                // A chunk option line (`#| echo: false`) is a comment in R, so
                // it survives conversion untouched and needs no special case.
                true
            }
        } else {
            if is_chunk_start(trimmed) {
                in_chunk = true;
            }
            false
        };
        if keep {
            out.push_str(body);
        } else {
            blank_prose(body, &mut out);
        }
        out.push_str(terminator);
    }
    out
}

/// One prose line, blanked to spaces. Inline R is the exception, because it is
/// code.
///
/// A character becomes as many spaces as it occupies BYTES: every range
/// downstream is a byte offset, so anything else shifts each diagnostic after a
/// non-ASCII prose character. Exotic whitespace is blanked with the rest. A
/// non-breaking space is whitespace to Rust and an unexpected character to R's
/// lexer, so keeping it verbatim reports a syntax error in prose no chunk
/// contained.
fn blank_prose(body: &str, out: &mut String) {
    let mut index = 0;
    while index < body.len() {
        if let Some((expression, end)) = inline_expression(body, index) {
            // `` `r total` `` becomes `   total;`: the delimiter and language
            // tag blank to spaces, the expression keeps its bytes and its
            // offset, and the closing backtick becomes the `;` that separates
            // this statement from the next inline expression on the same line.
            for _ in index..expression.start {
                out.push(' ');
            }
            out.push_str(&body[expression.clone()]);
            out.push(';');
            index = end;
            continue;
        }
        let character = body[index..].chars().next().unwrap_or(' ');
        for _ in 0..character.len_utf8() {
            out.push(' ');
        }
        index += character.len_utf8();
        // The loop advances by whole characters and whole spans, so the index
        // always lands on a character boundary.
        debug_assert!(body.is_char_boundary(index));
    }
}

/// An inline R expression starting at `index`: `` `r EXPR` `` in R Markdown and
/// Quarto. Returns the expression's byte range and the index just past the
/// closing backtick. A span naming another language (`` `python x` ``), a plain
/// Markdown code span (`` `total` ``), a multi-backtick span, and an unclosed
/// span are all prose.
fn inline_expression(body: &str, index: usize) -> Option<(std::ops::Range<usize>, usize)> {
    let rest = body.get(index..)?;
    let after_tick = rest.strip_prefix('`')?;
    if after_tick.starts_with('`') {
        return None;
    }
    let after_tag = after_tick.strip_prefix('r')?;
    // The tag must be delimited: `` `rate` `` is a code span, not R.
    let tag_gap = after_tag.chars().next()?;
    if !tag_gap.is_whitespace() {
        return None;
    }
    let expression_start = index + 1 + 1 + tag_gap.len_utf8();
    let body_after = body.get(expression_start..)?;
    let close = body_after.find('`')?;
    let expression = expression_start..expression_start + close;
    // A span carrying no expression (`` `r ` ``) is nothing to analyse.
    if body[expression.clone()].trim().is_empty() {
        return None;
    }
    Some((expression, expression_start + close + 1))
}

/// A fenced R chunk's opening line: ```` ```{r ... } ```` (Markdown) or
/// `<<...>>=` (Sweave). A fence naming another language is prose.
fn is_chunk_start(trimmed: &str) -> bool {
    if let Some(rest) = trimmed.strip_prefix("<<") {
        return rest.ends_with(">>=");
    }
    let Some(rest) = fence_body(trimmed) else {
        return false;
    };
    let Some(inside) = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')) else {
        return false;
    };
    let language = inside
        .split([',', ' ', '\t'])
        .next()
        .unwrap_or_default()
        .trim();
    language.eq_ignore_ascii_case("r")
}

/// A chunk's closing line: a bare fence, or Sweave's `@`.
fn is_chunk_end(trimmed: &str) -> bool {
    trimmed == "@" || fence_body(trimmed).is_some_and(str::is_empty)
}

/// The text after a Markdown fence's backticks, when the line opens with at
/// least three of them.
fn fence_body(trimmed: &str) -> Option<&str> {
    let backticks = trimmed.chars().take_while(|&c| c == '`').count();
    (backticks >= 3).then(|| trimmed[backticks..].trim())
}

/// Lines with their line terminators attached, so the conversion can reproduce
/// `\n` and `\r\n` exactly rather than normalizing them.
fn split_keeping_terminators(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest.find('\n').map_or(rest.len(), |index| index + 1);
        let (line, remainder) = rest.split_at(end);
        rest = remainder;
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_preserves_length_and_keeps_chunk_code() {
        let document = "# Title\n\nSome prose.\n\n```{r setup}\nx <- 1L\n```\n\nMore prose.\n\n```{r}\nprint(x)\n```\n";
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len(), "{converted:?}");
        assert!(converted.contains("x <- 1L"));
        assert!(converted.contains("print(x)"));
        assert!(!converted.contains("Title"));
        assert!(!converted.contains("prose"));
        // Every kept line sits at its original offset.
        assert_eq!(
            converted.find("x <- 1L"),
            document.find("x <- 1L"),
            "{converted:?}"
        );
    }

    #[test]
    fn a_fence_for_another_language_is_prose() {
        let document = "```{python}\nx = 1\n```\n```{r}\ny <- 2L\n```\n";
        let converted = r_source_of_literate(document);
        assert!(!converted.contains("x = 1"));
        assert!(converted.contains("y <- 2L"));
    }

    #[test]
    fn sweave_chunks_convert() {
        let document = "\\section{One}\n<<setup, echo=FALSE>>=\nz <- 3L\n@\ntext\n";
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len());
        assert!(converted.contains("z <- 3L"));
        assert!(!converted.contains("section"));
    }

    #[test]
    fn multibyte_prose_keeps_byte_offsets_aligned() {
        // Byte length is the property that matters: every range downstream is
        // a byte offset into the original file.
        let document = "prosé — ünicode\n```{r}\nx <- 1L\n```\n";
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len(), "{converted:?}");
        assert_eq!(
            document.find("x <- 1L"),
            converted.find("x <- 1L"),
            "the chunk must sit at the same byte offset"
        );
    }

    #[test]
    fn exotic_whitespace_in_prose_is_blanked() {
        // A non-breaking space is whitespace to Rust and an unexpected
        // character to R's lexer, so leaving it in reports a syntax error
        // against prose.
        let document = "texte\u{a0}ins\u{e9}cable\n```{r}\nx <- 1L\n```\n";
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len(), "{converted:?}");
        assert!(
            !converted.contains('\u{a0}'),
            "prose must not survive: {converted:?}"
        );
        assert!(converted.contains("x <- 1L"));
    }

    #[test]
    fn inline_expressions_are_code() {
        // A value a report only displays inline is still used, and a typo inside
        // an inline expression is still a typo.
        let document =
            "```{r}\ntotal <- 42L\n```\n\nWe sold `r total` units and `r nchar(total)` digits.\n";
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len(), "{converted:?}");
        assert_eq!(
            document.find("total)"),
            converted.find("total)"),
            "an inline expression keeps its byte offset"
        );
        assert!(converted.contains("total;"), "{converted:?}");
        assert!(converted.contains("nchar(total);"), "{converted:?}");
        // Two expressions on one line need a separator to parse as two
        // statements, which is what the closing backtick becomes.
        assert!(!converted.contains("units"), "{converted:?}");
    }

    #[test]
    fn a_code_span_that_is_not_inline_r_stays_prose() {
        // `` `total` `` is Markdown showing code, not evaluating it; `` `rate` ``
        // must not be mistaken for an `r` tag; another language is not R; and an
        // unclosed span is prose.
        let document = "See `total` and `rate` and `python x` and `r unclosed\n";
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len(), "{converted:?}");
        assert_eq!(
            converted.trim(),
            "",
            "nothing on this line is inline R: {converted:?}"
        );
    }

    #[test]
    fn an_unclosed_chunk_still_yields_its_code() {
        let document = "```{r}\nx <- 1L\n";
        assert!(r_source_of_literate(document).contains("x <- 1L"));
    }
}
