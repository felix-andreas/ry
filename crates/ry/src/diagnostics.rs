//! The final diagnostic set for one file: every class the analysis computes,
//! gated by config exactly the same way for the language server's publish
//! path and `ry check`, so the two surfaces can never drift.

use crate::config::{CheckConfig, Config};
use semantics::diagnostics::{Diagnostic, Severity, file_diagnostics, strict_diagnostics};
use semantics::lints::lint_file;
use semantics::{Db, SourceFile, TypingMode, file_typing_mode};

/// The published diagnostics of `file` under `config`, in position order.
/// Suppression comments are the caller's step (applied against the source
/// text it holds).
pub fn document_diagnostics(db: &dyn Db, file: SourceFile, config: &Config) -> Vec<Diagnostic> {
    let (typing_enabled, strict_enabled) = effective_typing(db, file, &config.check);
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

/// The cheap per-file classes that are pure functions of the parse. They are
/// the syntax errors, the annotation problems, and the lints, and they publish
/// immediately on an edit so typing never waits on naming or type checking.
/// They are a faithful subset of [`document_diagnostics`] for identical text,
/// because the settled wave only adds findings and never moves or removes
/// one.
pub fn first_wave_diagnostics(db: &dyn Db, file: SourceFile, config: &Config) -> Vec<Diagnostic> {
    let mut rendered = semantics::diagnostics::parse_stage_diagnostics(db, file);
    rendered.extend(lint_file(db, file, &config.lint));
    rendered.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    rendered
}

/// The effective (typing, strict) publication gates: a per-file directive
/// (`# typing: off|on|strict` or `#: @strict`) overrides the configured
/// default.
pub fn effective_typing(db: &dyn Db, file: SourceFile, check: &CheckConfig) -> (bool, bool) {
    match file_typing_mode(db, file) {
        Some(TypingMode::Off) => (false, false),
        Some(TypingMode::On) => (true, false),
        Some(TypingMode::Strict) => (true, true),
        None => (check.typing, check.strict),
    }
}

/// The suppression comment's prefixes, in precedence order. `roughly:` is the
/// project's former name and stays valid forever: a suppression lives inside a
/// user's source file, so retiring the old spelling would silently un-suppress
/// diagnostics in code nobody edited.
const SUPPRESSION_PREFIXES: [&str; 2] = ["ry:", "roughly:"];

/// Applies `# ry: allow(code, ...)` suppression comments: a diagnostic
/// is dropped when a suppression naming its code (or `all`) sits on the
/// diagnostic's own line (a trailing comment) or on the line directly above
/// it. Applied at assembly, after severity decisions, so a suppressed
/// escalated error is dropped like any other diagnostic.
pub fn apply_suppressions(diagnostics: Vec<Diagnostic>, source: &str) -> Vec<Diagnostic> {
    if !SUPPRESSION_PREFIXES
        .iter()
        .any(|prefix| source.contains(prefix))
    {
        return diagnostics;
    }
    let mut allowed_by_line: Vec<(usize, Vec<String>)> = Vec::new();
    for (line_index, line) in source.lines().enumerate() {
        let Some(comment_start) = line.find('#') else {
            continue;
        };
        let comment = line[comment_start..].trim_start_matches('#').trim();
        let Some(rest) = SUPPRESSION_PREFIXES
            .iter()
            .find_map(|prefix| comment.strip_prefix(prefix))
        else {
            continue;
        };
        let Some(arguments) = rest
            .trim()
            .strip_prefix("allow(")
            .and_then(|rest| rest.split_once(')'))
            .map(|(inside, _)| inside)
        else {
            continue;
        };
        let codes: Vec<String> = arguments
            .split(',')
            .map(|code| code.trim().to_owned())
            .filter(|code| !code.is_empty())
            .collect();
        if !codes.is_empty() {
            allowed_by_line.push((line_index, codes));
        }
    }
    if allowed_by_line.is_empty() {
        return diagnostics;
    }
    let index = crate::position::LineIndex::new(source);
    diagnostics
        .into_iter()
        .filter(|diagnostic| {
            let line = index.line_column(diagnostic.range.start()).line as usize;
            !allowed_by_line
                .iter()
                .filter(|(allowed_line, _)| {
                    *allowed_line == line || Some(*allowed_line) == line.checked_sub(1)
                })
                .flat_map(|(_, codes)| codes)
                .any(|code| code == "all" || code == diagnostic.code)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::apply_suppressions;
    use semantics::diagnostics::{Diagnostic, Severity};
    use syntax::{TextRange, TextSize};

    fn diagnostic(offset: u32, code: &'static str) -> Diagnostic {
        Diagnostic {
            range: TextRange::new(TextSize::from(offset), TextSize::from(offset + 1)),
            severity: Severity::Warning,
            code,
            message: "test".to_owned(),
            related: Vec::new(),
        }
    }

    #[test]
    fn trailing_and_line_above_suppressions_apply() {
        let source = "x <- T # ry: allow(boolean-shorthand)\n# ry: allow(unused)\ny <- 1\nz <- 1\n";
        // Offsets are derived, not written out: hard-coding them encodes the
        // length of the suppression prefix, so changing that prefix silently
        // moves every diagnostic off the line it was meant to sit on.
        let suppressed_inline = source.find('T').expect("line 1 has a `T`") as u32;
        let suppressed_below = source.find("y <- 1").expect("line 3") as u32;
        let reported = source.find("z <- 1").expect("line 4") as u32;
        let kept = apply_suppressions(
            vec![
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
}
