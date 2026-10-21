//! Host-side NAMESPACE validation: warnings at the import site and the
//! `unused-import` lint. Parsing lives in `semantics::metadata` (the same
//! parse also feeds the `PackageMetadata` input so imports resolve bare
//! reads); this module renders the problems a host reports.

use semantics::diagnostics::{Diagnostic, Severity};
use semantics::lints::LintLevel;
pub use semantics::metadata::{NamespaceImport, parse_namespace_imports};
use std::collections::BTreeSet;
use syntax::{SyntaxKind, TextRange};

/// One error per `importFrom(pkg, name)` whose namespace the export table knows
/// but whose name it does not list. This is the same fact `pkg::name`
/// validation checks, surfaced at the import site. An unknown namespace
/// produces nothing, because without stubs there is no export set to check
/// against.
///
/// An error, not a warning, for the same reason its `export()` sibling below is
/// one: R refuses to *load* the package (`object 'x' is not exported by
/// 'namespace:pkg'`), so the code cannot run at all. As a warning it shipped
/// through a `--min-severity error` gate.
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
            Some(Diagnostic {
                range: import.range,
                severity: Severity::Error,
                code: "unresolved",
                message: format!(
                    "`{name}` is not exported by `{}`, so this package will not load.",
                    import.namespace
                ),
                related: Vec::new(),
            })
        })
        .collect()
}

/// One error per `export(name)` in the NAMESPACE naming something the package
/// defines nowhere at top level. This is `R CMD check`'s "undefined exports",
/// which otherwise surfaces only at install time. `defines` is the package's whole
/// top-level name set, so a name bound by any file (in any order) counts.
pub fn namespace_export_problems(
    exports: &[(String, TextRange)],
    defines: &dyn Fn(&str) -> bool,
) -> Vec<Diagnostic> {
    exports
        .iter()
        .filter(|(name, _)| !defines(name))
        .map(|(name, range)| Diagnostic {
            range: *range,
            severity: Severity::Error,
            code: "unresolved",
            message: format!(
                "`{name}` is exported but this package defines no such top-level object."
            ),
            related: Vec::new(),
        })
        .collect()
}

/// The `unused-import` lint findings: one per `importFrom(pkg, name)` whose
/// `name` appears nowhere in `used_tokens`, which is the set of every token
/// text used across the package's R sources. Default-off because a package may import a
/// name only to re-export it or for a side effect, and usage is a
/// deliberately conservative token scan (any token equal to the name counts,
/// including `pkg::name` and operator spellings), so it under-reports rather
/// than risk a false positive. Whole-namespace `import(pkg)` directives are
/// not checked.
pub fn unused_import_diagnostics(
    imports: &[NamespaceImport],
    used_tokens: &BTreeSet<String>,
    level: LintLevel,
) -> Vec<Diagnostic> {
    let severity = match level {
        LintLevel::Default | LintLevel::Off => return Vec::new(),
        LintLevel::Warn => Severity::Warning,
        LintLevel::Error => Severity::Error,
    };
    imports
        .iter()
        .filter_map(|import| {
            let name = import.name.as_ref()?;
            if used_tokens.contains(name) {
                return None;
            }
            Some(Diagnostic {
                range: import.range,
                severity: severity.clone(),
                code: "unused-import",
                message: format!(
                    "imported name `{name}` from `{}` is never used.",
                    import.namespace
                ),
                related: Vec::new(),
            })
        })
        .collect()
}

/// Every token's text across an R source, for the conservative
/// `unused-import` usage scan. Uses the real parser so it sees exactly the
/// tokens R does (identifiers, operators like `%>%`, string contents), never
/// a hand-rolled scanner that could drift. String tokens contribute their
/// unquoted content (a NAMESPACE import may name an operator that sources
/// spell as a plain token).
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
        // R refuses to load the package, so this is not survivable advice.
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
