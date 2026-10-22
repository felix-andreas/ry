//! NAMESPACE problem diagnostics; parsing lives in `semantics::metadata`.

use semantics::diagnostics::{Diagnostic, Severity};
use semantics::lints::{LintLevel, lint_severity};
pub use semantics::metadata::{NamespaceImport, parse_namespace_imports};
use std::collections::BTreeSet;
use syntax::{SyntaxKind, TextRange};

/// Unknown namespaces produce nothing: without stubs there is no export set. An
/// error, not a warning, because R refuses to load the package.
pub fn namespace_import_problems(
    imports: &[NamespaceImport],
    knows_namespace: &dyn Fn(&str) -> bool,
    exports: &dyn Fn(&str, &str) -> bool,
) -> Vec<Diagnostic> {
    imports
        .iter()
        .filter_map(|import| {
            let name = import.name.as_ref()?;
            if !knows_namespace(&import.namespace) || exports(&import.namespace, name) {
                return None;
            }
            Some(Diagnostic::new(
                import.range,
                Severity::Error,
                "unresolved",
                format!(
                    "`{name}` is not exported by `{}`, so this package will not load.",
                    import.namespace
                ),
            ))
        })
        .collect()
}

/// `R CMD check`'s "undefined exports". `defines` is the package's whole
/// top-level name set, so a name bound by any file counts.
pub fn namespace_export_problems(
    exports: &[(String, TextRange)],
    defines: &dyn Fn(&str) -> bool,
) -> Vec<Diagnostic> {
    exports
        .iter()
        .filter(|(name, _)| !defines(name))
        .map(|(name, range)| {
            Diagnostic::new(
                *range,
                Severity::Error,
                "unresolved",
                format!("`{name}` is exported but this package defines no such top-level object."),
            )
        })
        .collect()
}

/// `used_tokens` is every token text across the package's sources: a conservative
/// scan that under-reports rather than risk a false positive. Whole-namespace
/// `import(pkg)` directives are not checked.
pub fn unused_import_diagnostics(
    imports: &[NamespaceImport],
    used_tokens: &BTreeSet<String>,
    level: LintLevel,
) -> Vec<Diagnostic> {
    let Some(severity) = lint_severity(level, None) else {
        return Vec::new();
    };
    imports
        .iter()
        .filter_map(|import| {
            let name = import.name.as_ref()?;
            if used_tokens.contains(name) {
                return None;
            }
            Some(Diagnostic::new(
                import.range,
                severity,
                "unused-import",
                format!(
                    "imported name `{name}` from `{}` is never used.",
                    import.namespace
                ),
            ))
        })
        .collect()
}

/// Uses the real parser, not a hand-rolled scanner, so it sees exactly R's tokens;
/// string tokens contribute their unquoted content.
pub fn collect_used_tokens(source: &str, out: &mut BTreeSet<String>) {
    let parse = syntax::parse(source);
    for element in parse.syntax_node().descendants_with_tokens() {
        if let syntax::SyntaxElement::Token(token) = element {
            if token.kind().is_trivia() {
                continue;
            }
            let text = token.text();
            out.insert(text.to_owned());
            if token.kind() == SyntaxKind::STRING {
                let content = text
                    .trim_start_matches(['"', '\''])
                    .trim_end_matches(['"', '\'']);
                if !content.is_empty() {
                    out.insert(content.to_owned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_backed<'db>(
        db: &'db semantics::RootDatabase,
    ) -> (
        impl Fn(&str) -> bool + 'db,
        impl Fn(&str, &str) -> bool + 'db,
    ) {
        (
            move |namespace: &str| {
                semantics::stubs::namespace_known(db, namespace).unwrap_or(false)
            },
            move |namespace: &str, name: &str| {
                semantics::stubs::namespace_exports(db, namespace, name)
            },
        )
    }

    fn problems_for(source: &str) -> Vec<String> {
        let db = semantics::RootDatabase::default();
        semantics::stubs::install_shipped_stubs(&db);
        let (knows, exports) = stub_backed(&db);
        namespace_import_problems(&parse_namespace_imports(source), &knows, &exports)
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    }

    #[test]
    fn known_namespace_with_real_exports_is_clean() {
        let problems = problems_for("import(stats)\nimportFrom(stats, sd, median)\n");
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn known_namespace_with_a_typo_is_an_error() {
        let db = semantics::RootDatabase::default();
        semantics::stubs::install_shipped_stubs(&db);
        let (knows, exports) = stub_backed(&db);
        let problems = namespace_import_problems(
            &parse_namespace_imports("importFrom(stats, sd, medain)\n"),
            &knows,
            &exports,
        );
        assert_eq!(
            problems
                .iter()
                .map(|problem| problem.message.as_str())
                .collect::<Vec<_>>(),
            ["`medain` is not exported by `stats`, so this package will not load."]
        );
        assert!(
            problems
                .iter()
                .all(|problem| problem.severity == Severity::Error),
            "{problems:?}"
        );
    }

    #[test]
    fn unknown_namespaces_are_not_checked() {
        let problems = problems_for("import(dplyr)\nimportFrom(dplyr, mutate)\n");
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn string_spellings_and_other_directives_parse() {
        let problems = problems_for(
            "export(run)\nS3method(print, thing)\nimportFrom(\"stats\", \"nope_not_real\")\n",
        );
        assert_eq!(
            problems,
            ["`nope_not_real` is not exported by `stats`, so this package will not load."]
        );
    }

    fn unused_for(namespace: &str, sources: &[&str], level: LintLevel) -> Vec<String> {
        let imports = parse_namespace_imports(namespace);
        let mut used = BTreeSet::new();
        for source in sources {
            collect_used_tokens(source, &mut used);
        }
        unused_import_diagnostics(&imports, &used, level)
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    }

    #[test]
    fn unused_import_is_flagged_when_opted_in() {
        let unused = unused_for(
            "importFrom(stats, sd, median)\n",
            &["x <- sd(values)\n"],
            LintLevel::Warn,
        );
        assert_eq!(
            unused,
            ["imported name `median` from `stats` is never used."]
        );
    }

    #[test]
    fn used_names_including_namespaced_and_operators_are_not_flagged() {
        let unused = unused_for(
            "importFrom(dplyr, mutate, filter)\nimportFrom(magrittr, \"%>%\")\n",
            &["out <- df %>% mutate(x = 1)\ny <- dplyr::filter(df, x > 0)\n"],
            LintLevel::Warn,
        );
        assert!(unused.is_empty(), "{unused:?}");
    }

    #[test]
    fn unused_import_is_silent_by_default() {
        let unused = unused_for(
            "importFrom(stats, median)\n",
            &["x <- 1\n"],
            LintLevel::Default,
        );
        assert!(unused.is_empty(), "{unused:?}");
    }
}
