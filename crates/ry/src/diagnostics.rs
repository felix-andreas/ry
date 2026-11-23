//! Shared by the language server and `ry check` so their gating cannot drift.

use {
    crate::{config::Config, position::LineIndex},
    semantics::{
        Db, SourceFile, TypingMode,
        diagnostics::{
            Diagnostic, Severity, file_diagnostics, parse_stage_diagnostics, strict_diagnostics,
        },
        file_is_strict, file_typing_mode,
        lints::lint_file,
    },
    syntax::{TextRange, TextSize},
};

/// In position order. Suppression comments are the caller's step.
pub fn document_diagnostics(db: &dyn Db, file: SourceFile, config: &Config) -> Vec<Diagnostic> {
    // The database's settings carry `[check] strict`, because the checks record strict mode's
    // origins only in a strict file.
    let strict_enabled = file_is_strict(db, file);
    let typing_enabled = match file_typing_mode(db, file) {
        Some(TypingMode::Off) => false,
        Some(TypingMode::On | TypingMode::Strict) => true,
        // Strict mode rejects code it cannot type, which would mean little if it then hid the
        // type errors in the code it can, so it implies typing like `# typing: strict` does.
        None => config.check.typing || strict_enabled,
    };
    let mut rendered: Vec<Diagnostic> = file_diagnostics(db, file)
        .into_iter()
        .filter(|diagnostic| match diagnostic.code {
            "unused" => config.check.unused,
            "maybe-undefined" => config.check.maybe_undefined,
            "type-mismatch" => typing_enabled,
            _ => true,
        })
        .collect();
    rendered.extend(lint_file(db, file, &config.lint));
    if strict_enabled {
        rendered.extend(strict_diagnostics(db, file));
        for diagnostic in &mut rendered {
            if diagnostic.code == "unresolved" {
                diagnostic.severity = Severity::Error;
            }
        }
    }
    rendered.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    rendered
}

/// The parse-only classes, published immediately on edit. For identical text this
/// is a subset of [`document_diagnostics`]: the settled set only adds findings.
pub fn first_wave_diagnostics(db: &dyn Db, file: SourceFile, config: &Config) -> Vec<Diagnostic> {
    let mut rendered = parse_stage_diagnostics(db, file);
    rendered.extend(lint_file(db, file, &config.lint));
    rendered.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    rendered
}

/// `roughly:` is the former name and must stay valid: retiring it would silently
/// un-suppress diagnostics in code nobody edited.
const SUPPRESSION_PREFIXES: [&str; 2] = ["ry:", "roughly:"];

/// Drops a diagnostic its line's suppression comments cover. `# ry: allow(code, ...)` covers
/// its own line and the line below, and `all` stands for every code. `# ry: expect(code, ...)`
/// covers the same lines, and [`settle_suppressions`] reports it when nothing it names appeared.
/// lintr's own comments are honored too, so code written for lintr keeps its suppressions:
/// `# nolint` covers its line, `# nolint start` to `# nolint end` covers the lines between, and
/// either may name linters (`# nolint: seq_linter.`), each standing for the ry codes in
/// [`lintr_codes`].
///
/// This is the step for a partial set, such as the parse-only first wave, which cannot tell
/// whether an expectation holds.
pub fn apply_suppressions(diagnostics: Vec<Diagnostic>, source: &str) -> Vec<Diagnostic> {
    suppress(diagnostics, source, false)
}

/// [`apply_suppressions`] for a file's complete set of findings, which also reports each
/// `# ry: expect(...)` that suppressed nothing, as `unfulfilled-expect`.
pub fn settle_suppressions(diagnostics: Vec<Diagnostic>, source: &str) -> Vec<Diagnostic> {
    suppress(diagnostics, source, true)
}

fn suppress(diagnostics: Vec<Diagnostic>, source: &str, complete: bool) -> Vec<Diagnostic> {
    if !SUPPRESSION_PREFIXES
        .iter()
        .chain(&["nolint"])
        .any(|prefix| source.contains(prefix))
    {
        return diagnostics;
    }
    let suppressions = suppressions(source);
    if suppressions.is_empty() {
        return diagnostics;
    }
    let index = LineIndex::new(source);
    // Which code of which expectation a suppressed finding fulfilled.
    let mut fulfilled: Vec<Vec<bool>> = suppressions
        .iter()
        .map(|suppression| match &suppression.allowed {
            Allowed::Codes(codes) => vec![false; codes.len()],
            Allowed::EveryFinding => Vec::new(),
        })
        .collect();
    let mut kept: Vec<Diagnostic> = diagnostics
        .into_iter()
        .filter(|diagnostic| {
            let line = index.line_column(diagnostic.range.start()).line as usize;
            let mut suppressed = false;
            for (suppression, fulfilled) in suppressions.iter().zip(&mut fulfilled) {
                if !(suppression.first_line..=suppression.last_line).contains(&line) {
                    continue;
                }
                match &suppression.allowed {
                    Allowed::Codes(codes) => {
                        for (code, fulfilled) in codes.iter().zip(fulfilled.iter_mut()) {
                            if code == "all" || code == diagnostic.code {
                                *fulfilled = true;
                                suppressed = true;
                            }
                        }
                    }
                    Allowed::EveryFinding => suppressed |= diagnostic.code != "syntax-error",
                }
            }
            !suppressed
        })
        .collect();
    if complete {
        for (suppression, fulfilled) in suppressions.iter().zip(&fulfilled) {
            let (Some(range), Allowed::Codes(codes)) = (suppression.expect, &suppression.allowed)
            else {
                continue;
            };
            for (code, _) in codes
                .iter()
                .zip(fulfilled)
                .filter(|(_, fulfilled)| !**fulfilled)
            {
                let missing = match code.as_str() {
                    "all" => "no finding".to_owned(),
                    code => format!("no `{code}` finding"),
                };
                let fix = match codes.len() {
                    1 => "Remove the comment".to_owned(),
                    _ => format!("Remove `{code}` from it"),
                };
                kept.push(Diagnostic::new(
                    range,
                    Severity::Warning,
                    "unfulfilled-expect",
                    format!(
                        "{missing} is on this line or the next, so this expectation is stale. {fix}"
                    ),
                ));
            }
        }
        kept.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    }
    kept
}

struct Suppression {
    first_line: usize,
    last_line: usize,
    allowed: Allowed,
    /// Where an `expect(...)` directive is written; `None` for `allow` and lintr's comments.
    expect: Option<TextRange>,
}

enum Allowed {
    Codes(Vec<String>),
    /// A bare `# nolint`, which in lintr silences every linter. A syntax error stays, because
    /// lintr reports one regardless.
    EveryFinding,
}

fn suppressions(source: &str) -> Vec<Suppression> {
    let mut suppressions = Vec::new();
    let mut open_block: Option<(usize, Allowed)> = None;
    let mut last_line = 0;
    let mut line_start = 0;
    for (line_index, line) in source.lines().enumerate() {
        last_line = line_index;
        let start = line_start;
        line_start += line.len() + usize::from(source[start + line.len()..].starts_with('\r')) + 1;
        // Every `#` is tried, because the first may sit in a string (`"#FF0000"`) or start a
        // comment that names the directive later, as lintr's `#[[:space:]]*nolint` also allows.
        let directive = line.match_indices('#').find_map(|(start, _)| {
            let comment = line[start..].trim_start_matches('#').trim();
            let is_directive = SUPPRESSION_PREFIXES
                .iter()
                .any(|prefix| comment.starts_with(prefix))
                || comment
                    .strip_prefix("nolint")
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', ':']));
            is_directive.then_some(comment)
        });
        let Some(comment) = directive else {
            continue;
        };
        if let Some(rest) = SUPPRESSION_PREFIXES
            .iter()
            .find_map(|prefix| comment.strip_prefix(prefix))
        {
            let rest = rest.trim();
            let (expects, rest) = match rest.strip_prefix("expect(") {
                Some(rest) => (true, rest),
                None => (false, rest.strip_prefix("allow(").unwrap_or("")),
            };
            let Some((arguments, _)) = rest.split_once(')') else {
                continue;
            };
            // The directive's own text, from `ry:` to its `)`, carries a stale expectation.
            let expect = expects.then(|| {
                let directive_end =
                    arguments.as_ptr() as usize - line.as_ptr() as usize + arguments.len() + 1;
                let directive_start = comment.as_ptr() as usize - line.as_ptr() as usize;
                TextRange::new(
                    TextSize::from((start + directive_start) as u32),
                    TextSize::from((start + directive_end) as u32),
                )
            });
            let codes: Vec<String> = arguments
                .split(',')
                .map(|code| code.trim().to_owned())
                .filter(|code| !code.is_empty())
                .collect();
            if !codes.is_empty() {
                suppressions.push(Suppression {
                    first_line: line_index,
                    last_line: line_index + 1,
                    allowed: Allowed::Codes(codes),
                    expect,
                });
            }
            continue;
        }
        let Some(rest) = comment
            .strip_prefix("nolint")
            .filter(|rest| rest.is_empty() || rest.starts_with([' ', '\t', ':']))
        else {
            continue;
        };
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix("start") {
            open_block = Some((line_index, nolint_allowed(rest)));
        } else if rest.starts_with("end") {
            if let Some((first_line, allowed)) = open_block.take() {
                suppressions.push(Suppression {
                    first_line,
                    last_line: line_index,
                    allowed,
                    expect: None,
                });
            }
        } else {
            suppressions.push(Suppression {
                first_line: line_index,
                last_line: line_index,
                allowed: nolint_allowed(rest),
                expect: None,
            });
        }
    }
    // lintr reads an unclosed block as running to the end of the file.
    if let Some((first_line, allowed)) = open_block {
        suppressions.push(Suppression {
            first_line,
            last_line,
            allowed,
            expect: None,
        });
    }
    suppressions
}

/// What follows `nolint` (or `nolint start`): nothing, or `: name, name.` naming linters.
fn nolint_allowed(rest: &str) -> Allowed {
    let Some(names) = rest.trim_start().strip_prefix(':') else {
        return Allowed::EveryFinding;
    };
    let names = names.split_once('.').map_or(names, |(names, _)| names);
    Allowed::Codes(
        names
            .split(',')
            .flat_map(|name| lintr_codes(name.trim()))
            .map(|code| (*code).to_owned())
            .collect(),
    )
}

/// The ry codes that report what a lintr linter reports. lintr accepts a name with or without
/// its `_linter` suffix. A linter ry has no counterpart for maps to nothing, so naming it
/// suppresses nothing.
pub(crate) fn lintr_codes(name: &str) -> &'static [&'static str] {
    let name = name.strip_suffix("_linter").unwrap_or(name);
    match name {
        "assignment" => &["assignment-operator"],
        "T_and_F_symbol" => &["boolean-shorthand"],
        "equals_na" => &["equals-na"],
        "vector_logic" => &["vector-logic"],
        "seq" => &["length-sequence"],
        "class_equals" => &["class-equals"],
        "return" => &["redundant-return"],
        "commented_code" => &["commented-code"],
        "object_usage" => &["unused", "unresolved"],
        "object_name" => &["naming-style"],
        "quotes" => &["single-quotes"],
        "any_is_na" => &["any-is-na"],
        "paste" => &["paste-sep"],
        "lengths" => &["lengths"],
        "literal_coercion" => &["literal-coercion"],
        "redundant_equals" => &["redundant-equals"],
        "expect_null" | "expect_true_false" | "expect_length" | "expect_named"
        | "expect_s3_class" | "expect_not" => &["testthat-expectation"],
        "condition_message" => &["condition-message"],
        "comparison_negation" => &["comparison-negation"],
        "outer_negation" => &["outer-negation"],
        "fixed_regex" => &["fixed-regex"],
        "which_grepl" => &["which-grepl"],
        "length_levels" => &["length-levels"],
        "browser" => &["browser-call"],
        "numeric_leading_zero" => &["numeric-leading-zero"],
        "line_length" => &["line-length"],
        "unreachable_code" => &["unreachable-code"],
        "sprintf" => &["sprintf-arguments"],
        "duplicate_argument" => &["duplicate-argument"],
        "length_test" => &["length-test"],
        "implicit_assignment" => &["implicit-assignment"],
        "repeat" => &["while-true"],
        "scalar_in" => &["scalar-in"],
        "sort" => &["order-subset"],
        "matrix_apply" => &["matrix-apply"],
        "empty_assignment" => &["empty-assignment"],
        "unnecessary_lambda" => &["unnecessary-lambda"],
        "yoda_test" => &["yoda-test"],
        "nzchar" => &["nzchar"],
        "sample_int" => &["sample-int"],
        "unnecessary_concatenation" => &["unnecessary-concatenation"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{apply_suppressions, settle_suppressions},
        semantics::diagnostics::{Diagnostic, Severity},
        syntax::{TextRange, TextSize},
    };

    fn diagnostic(offset: u32, code: &'static str) -> Diagnostic {
        Diagnostic::new(
            TextRange::new(TextSize::from(offset), TextSize::from(offset + 1)),
            Severity::Warning,
            code,
            "test".to_owned(),
        )
    }

    #[test]
    fn trailing_and_line_above_suppressions_apply() {
        let source =
            "x <- T # ry: allow(unused, boolean-shorthand)\n# ry: allow(unused)\ny <- 1\nz <- 1\n";
        // Derived so a change to the suppression prefix length cannot shift the offsets.
        let suppressed_inline = source.find('T').expect("line 1 has a `T`") as u32;
        let suppressed_below = source.find("y <- 1").expect("line 3") as u32;
        let reported = source.find("z <- 1").expect("line 4") as u32;
        let kept = apply_suppressions(
            vec![
                diagnostic(0, "unused"),
                diagnostic(suppressed_inline, "boolean-shorthand"),
                diagnostic(suppressed_below, "unused"),
                diagnostic(reported, "unused"),
            ],
            source,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(u32::from(kept[0].range.start()), reported);
    }

    #[test]
    fn all_wildcard_suppresses_every_code() {
        let source = "x = T # ry: allow(all)\n";
        let kept = apply_suppressions(
            vec![
                diagnostic(0, "assignment-operator"),
                diagnostic(4, "boolean-shorthand"),
            ],
            source,
        );
        assert!(kept.is_empty(), "{kept:?}");
    }

    #[test]
    fn a_fulfilled_expect_suppresses_and_a_stale_one_is_reported() {
        let source =
            "# ry: expect(unused)\nx <- 1\ny <- T # ry: expect(boolean-shorthand, unused)\n";
        let suppressed = source.find("x <- 1").expect("line 2") as u32;
        let shorthand = source.find('T').expect("line 3 has a `T`") as u32;
        let directive = source.rfind("ry: expect").expect("line 3 has an expect") as u32;
        let kept = settle_suppressions(
            vec![
                diagnostic(suppressed, "unused"),
                diagnostic(shorthand, "boolean-shorthand"),
            ],
            source,
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].code, "unfulfilled-expect");
        assert_eq!(u32::from(kept[0].range.start()), directive);
        assert!(
            kept[0]
                .message
                .contains("no `unused` finding is on this line or the next, so this expectation is stale. Remove `unused` from it"),
            "{}",
            kept[0].message
        );
    }

    #[test]
    fn a_stale_expect_naming_one_code_says_to_remove_the_comment() {
        let kept = settle_suppressions(
            Vec::new(),
            "x <- 1 # ry: expect(unused)
",
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(
            kept[0].message.ends_with("Remove the comment"),
            "{}",
            kept[0].message
        );
    }

    #[test]
    fn an_expect_is_not_judged_before_the_findings_settle() {
        let source = "# ry: expect(unused)\nx <- 1\n";
        assert!(apply_suppressions(Vec::new(), source).is_empty());
        assert_eq!(settle_suppressions(Vec::new(), source).len(), 1);
    }

    #[test]
    fn former_prefix_suppresses_and_other_codes_do_not() {
        let source =
            "x <- T # roughly: allow(boolean-shorthand)\n# ry: allow(naming-style)\ny <- T\n";
        let suppressed = source.find('T').expect("line 1 has a `T`") as u32;
        let reported = source.rfind('T').expect("line 3 has a `T`") as u32;
        let kept = apply_suppressions(
            vec![
                diagnostic(suppressed, "boolean-shorthand"),
                diagnostic(reported, "boolean-shorthand"),
            ],
            source,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(u32::from(kept[0].range.start()), reported);
    }

    #[test]
    fn nolint_covers_its_line_and_names_map_to_codes() {
        let source = "x = T # nolint\ny = T # nolint: T_and_F_symbol.\nz = T # nolint: seq_linter\nw = T # nolintish\n";
        let line = |number: usize| {
            source
                .lines()
                .take(number)
                .map(|line| line.len() as u32 + 1)
                .sum::<u32>()
        };
        let kept = apply_suppressions(
            vec![
                diagnostic(line(0), "assignment-operator"),
                diagnostic(line(0) + 4, "boolean-shorthand"),
                diagnostic(line(1), "assignment-operator"),
                diagnostic(line(1) + 4, "boolean-shorthand"),
                diagnostic(line(2) + 4, "boolean-shorthand"),
                diagnostic(line(3) + 4, "boolean-shorthand"),
            ],
            source,
        );
        let codes: Vec<(u32, &str)> = kept
            .iter()
            .map(|kept| (u32::from(kept.range.start()), kept.code))
            .collect();
        assert_eq!(
            codes,
            vec![
                (line(1), "assignment-operator"),
                (line(2) + 4, "boolean-shorthand"),
                (line(3) + 4, "boolean-shorthand"),
            ]
        );
    }

    #[test]
    fn nolint_after_a_hash_in_a_string_or_other_comment_text_applies() {
        let source = "x = \"#FF\" # nolint\ny = 1 # TODO # nolint\n";
        let second = source.find("y =").expect("the line exists") as u32;
        let kept = apply_suppressions(
            vec![
                diagnostic(0, "assignment-operator"),
                diagnostic(second, "assignment-operator"),
            ],
            source,
        );
        assert!(kept.is_empty());
    }

    #[test]
    fn nolint_blocks_cover_the_lines_between_and_spare_syntax_errors() {
        let source = "a = 1\n# nolint start\nb = 1\nc(\n# nolint end\nd = 1\n";
        let start_of = |text: &str| source.find(text).expect("the line exists") as u32;
        let kept = apply_suppressions(
            vec![
                diagnostic(start_of("a = 1"), "assignment-operator"),
                diagnostic(start_of("b = 1"), "assignment-operator"),
                diagnostic(start_of("c("), "syntax-error"),
                diagnostic(start_of("d = 1"), "assignment-operator"),
            ],
            source,
        );
        let starts: Vec<u32> = kept
            .iter()
            .map(|kept| u32::from(kept.range.start()))
            .collect();
        assert_eq!(
            starts,
            vec![start_of("a = 1"), start_of("c("), start_of("d = 1")]
        );
    }
}
