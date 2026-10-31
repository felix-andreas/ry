//! Shared by the language server and `ry check` so their gating cannot drift.

use crate::config::Config;
use crate::position::LineIndex;
use semantics::diagnostics::{
    Diagnostic, Severity, file_diagnostics, parse_stage_diagnostics, strict_diagnostics,
};
use semantics::lints::lint_file;
use semantics::{Db, SourceFile, TypingMode, file_typing_mode};

/// In position order. Suppression comments are the caller's step.
pub fn document_diagnostics(db: &dyn Db, file: SourceFile, config: &Config) -> Vec<Diagnostic> {
    let (typing_enabled, strict_enabled) = match file_typing_mode(db, file) {
        Some(TypingMode::Off) => (false, false),
        Some(TypingMode::On) => (true, false),
        Some(TypingMode::Strict) => (true, true),
        None => (config.check.typing, config.check.strict),
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

/// Drops a diagnostic when a suppression naming its code (or `all`) sits on its
/// line or the line directly above.
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
    let index = LineIndex::new(source);
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
        Diagnostic::new(
            TextRange::new(TextSize::from(offset), TextSize::from(offset + 1)),
            Severity::Warning,
            code,
            "test".to_owned(),
        )
    }

    #[test]
    fn trailing_and_line_above_suppressions_apply() {
        let source = "x <- T # ry: allow(boolean-shorthand)\n# ry: allow(unused)\ny <- 1\nz <- 1\n";
        // Derived so a change to the suppression prefix length cannot shift the offsets.
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
}
