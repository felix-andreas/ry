//! R Markdown, Quarto and Sweave documents read as one R script (as at knit
//! time). Prose is blanked byte-for-byte to spaces, so every offset matches the
//! original and ranges need no translation.

use std::ops::Range;

pub fn is_literate_extension(extension: &str) -> bool {
    matches!(
        extension,
        "Rmd" | "rmd" | "qmd" | "Rnw" | "rnw" | "Snw" | "snw"
    )
}

/// A stretch of a literate document that is R code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub range: Range<usize>,
    pub kind: RegionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// The lines of a chunk between its fences, with their line terminators. `indented` is set
    /// when the opening fence is indented (a chunk inside a list item), where every code line
    /// carries the fence's indentation too.
    Chunk { indented: bool },
    /// The expression of an inline `` `r expr` ``, without the backticks.
    Inline,
}

/// The R regions of a literate document, in order.
pub fn r_regions(text: &str) -> Vec<Region> {
    let mut regions = Vec::new();
    let mut chunk: Option<(usize, bool)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        match chunk {
            Some((start, indented)) if is_chunk_end(trimmed) => {
                regions.push(Region {
                    range: start..offset,
                    kind: RegionKind::Chunk { indented },
                });
                chunk = None;
            }
            Some(_) => {}
            None if is_chunk_start(trimmed) => {
                chunk = Some((offset + line.len(), trimmed.len() != body.len()));
            }
            None => {
                let mut index = 0;
                while index < body.len() {
                    match inline_expression(body, index) {
                        Some((expression, end)) => {
                            regions.push(Region {
                                range: offset + expression.start..offset + expression.end,
                                kind: RegionKind::Inline,
                            });
                            index = end;
                        }
                        None => {
                            index += body[index..].chars().next().map_or(1, char::len_utf8);
                        }
                    }
                }
            }
        }
        offset += line.len();
    }
    if let Some((start, indented)) = chunk {
        regions.push(Region {
            range: start..text.len(),
            kind: RegionKind::Chunk { indented },
        });
    }
    regions
}

/// The document as one R script, as knitting runs it: everything outside the R regions is
/// blanked, and each inline expression's closing backtick becomes `;`, so two inline
/// expressions on one line parse as two statements. The script has the document's exact byte
/// length and line breaks, so every offset in it is an offset in the document.
pub fn r_source_of_literate(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for region in r_regions(text) {
        blank(&text[cursor..region.range.start], &mut out);
        out.push_str(&text[region.range.clone()]);
        cursor = region.range.end;
        if region.kind == RegionKind::Inline {
            out.push(';');
            cursor += 1;
        }
    }
    blank(&text[cursor..], &mut out);
    out
}

/// A character becomes one space per UTF-8 byte, so offsets stay aligned. Exotic whitespace is
/// blanked too: R's lexer rejects a non-breaking space.
fn blank(prose: &str, out: &mut String) {
    for character in prose.chars() {
        match character {
            '\n' | '\r' => out.push(character),
            other => out.extend(std::iter::repeat_n(' ', other.len_utf8())),
        }
    }
}

/// Returns the expression's byte range and the index just past the closing
/// backtick.
fn inline_expression(body: &str, index: usize) -> Option<(Range<usize>, usize)> {
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
    if body[expression.clone()].trim().is_empty() {
        return None;
    }
    Some((expression, expression_start + close + 1))
}

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

fn is_chunk_end(trimmed: &str) -> bool {
    trimmed == "@" || fence_body(trimmed).is_some_and(str::is_empty)
}

fn fence_body(trimmed: &str) -> Option<&str> {
    let backticks = trimmed.chars().take_while(|&c| c == '`').count();
    (backticks >= 3).then(|| trimmed[backticks..].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(document: &str) -> String {
        let converted = r_source_of_literate(document);
        assert_eq!(converted.len(), document.len(), "{converted:?}");
        converted
    }

    #[test]
    fn conversion_preserves_length_and_keeps_chunk_code() {
        let document = "# Title\n\nSome prose.\n\n```{r setup}\nx <- 1L\n```\n\nMore prose.\n\n```{r}\nprint(x)\n```\n";
        let converted = convert(document);
        assert!(converted.contains("x <- 1L"));
        assert!(converted.contains("print(x)"));
        assert!(!converted.contains("Title"));
        assert!(!converted.contains("prose"));
        assert_eq!(
            converted.find("x <- 1L"),
            document.find("x <- 1L"),
            "{converted:?}"
        );
    }

    #[test]
    fn a_fence_for_another_language_is_prose() {
        let document = "```{python}\nx = 1\n```\n```{r}\ny <- 2L\n```\n";
        let converted = convert(document);
        assert!(!converted.contains("x = 1"));
        assert!(converted.contains("y <- 2L"));
    }

    #[test]
    fn sweave_chunks_convert() {
        let document = "\\section{One}\n<<setup, echo=FALSE>>=\nz <- 3L\n@\ntext\n";
        let converted = convert(document);
        assert!(converted.contains("z <- 3L"));
        assert!(!converted.contains("section"));
    }

    #[test]
    fn multibyte_prose_keeps_byte_offsets_aligned() {
        let document = "prosé — ünicode\n```{r}\nx <- 1L\n```\n";
        let converted = convert(document);
        assert_eq!(
            document.find("x <- 1L"),
            converted.find("x <- 1L"),
            "the chunk must sit at the same byte offset"
        );
    }

    #[test]
    fn exotic_whitespace_in_prose_is_blanked() {
        let document = "texte\u{a0}ins\u{e9}cable\n```{r}\nx <- 1L\n```\n";
        let converted = convert(document);
        assert!(
            !converted.contains('\u{a0}'),
            "prose must not survive: {converted:?}"
        );
        assert!(converted.contains("x <- 1L"));
    }

    #[test]
    fn inline_expressions_are_code() {
        let document =
            "```{r}\ntotal <- 42L\n```\n\nWe sold `r total` units and `r nchar(total)` digits.\n";
        let converted = convert(document);
        assert_eq!(
            document.find("total)"),
            converted.find("total)"),
            "an inline expression keeps its byte offset"
        );
        assert!(converted.contains("total;"), "{converted:?}");
        assert!(converted.contains("nchar(total);"), "{converted:?}");
        assert!(!converted.contains("units"), "{converted:?}");
    }

    #[test]
    fn a_code_span_that_is_not_inline_r_stays_prose() {
        let document = "See `total` and `rate` and `python x` and `r unclosed\n";
        let converted = convert(document);
        assert_eq!(
            converted.trim(),
            "",
            "nothing on this line is inline R: {converted:?}"
        );
    }

    #[test]
    fn regions_name_chunk_bodies_and_inline_expressions() {
        let document = "Intro `r 1 + 1`.\n```{r}\nx <- 1\n```\n  ```{r}\n  y\n  ```\n";
        let regions: Vec<(&str, RegionKind)> = r_regions(document)
            .into_iter()
            .map(|region| (&document[region.range], region.kind))
            .collect();
        assert_eq!(
            regions,
            [
                ("1 + 1", RegionKind::Inline),
                ("x <- 1\n", RegionKind::Chunk { indented: false }),
                ("  y\n", RegionKind::Chunk { indented: true }),
            ]
        );
    }

    #[test]
    fn an_unclosed_chunk_still_yields_its_code() {
        let document = "```{r}\nx <- 1L\n";
        assert!(convert(document).contains("x <- 1L"));
    }
}
