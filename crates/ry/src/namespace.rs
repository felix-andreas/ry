//! NAMESPACE problem diagnostics; parsing lives in `semantics::metadata`.

use rustc_hash::FxHashSet;
use semantics::diagnostics::{Diagnostic, Severity};
use semantics::lints::{LintLevel, lint_severity};
use semantics::metadata::NamespaceImport;
use std::collections::BTreeSet;
use syntax::{SyntaxKind, SyntaxNode, TextRange, for_each_token};

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
/// top-level name set, so a name bound by any file counts. An imported name is in the
/// namespace too, so a package may re-export it: an `importFrom(pkg, name)`, or an
/// `import(pkg)` whose namespace exports the name or has no known export set. A package that
/// creates bindings at run time (`assign(name, ...)`, `makeActiveBinding("name", ...)`, a loop
/// over a list of styles) can export a name it never writes as a definition, so a name the
/// sources mention while they call such a definer is not reported either.
pub fn namespace_export_problems(
    exports: &[(String, TextRange)],
    imports: &[NamespaceImport],
    defines: &dyn Fn(&str) -> bool,
    knows_namespace: &dyn Fn(&str) -> bool,
    namespace_exports: &dyn Fn(&str, &str) -> bool,
    used_tokens: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let imported = |name: &str| {
        imports.iter().any(|import| match &import.name {
            Some(imported) => imported == name,
            None => {
                !knows_namespace(&import.namespace) || namespace_exports(&import.namespace, name)
            }
        })
    };
    let defines_at_run_time = RUNTIME_DEFINERS
        .iter()
        .any(|definer| used_tokens.contains(*definer));
    exports
        .iter()
        .filter(|(name, _)| {
            !defines(name)
                && !imported(name)
                && !(defines_at_run_time && used_tokens.contains(name))
        })
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

const RUNTIME_DEFINERS: &[&str] = &[
    "assign",
    "makeActiveBinding",
    "delayedAssign",
    "list2env",
    "setGeneric",
    "setClass",
    "setRefClass",
    "sys.source",
];

/// `used_tokens` holds the scanned names (see `token_scan_names`) that appear as a token
/// anywhere in the package's sources: a conservative scan that under-reports rather than risk
/// a false positive. Whole-namespace
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

/// The names the NAMESPACE checks may look up in `used_tokens` (imported names, exported
/// names and the run-time definers), so a token scan records only those instead of every
/// token text in the package. Empty when no check would ask.
pub fn token_scan_names<'a>(
    imports: &'a [NamespaceImport],
    exports: &'a [(String, TextRange)],
) -> FxHashSet<&'a str> {
    let mut names: FxHashSet<&str> = imports
        .iter()
        .filter_map(|import| import.name.as_deref())
        .chain(exports.iter().map(|(name, _)| name.as_str()))
        .collect();
    if !names.is_empty() {
        names.extend(RUNTIME_DEFINERS.iter().copied());
    }
    names
}

/// Uses the real parser, not a hand-rolled scanner, so it sees exactly R's tokens;
/// string tokens contribute their unquoted content.
pub fn collect_used_tokens(
    root: &SyntaxNode,
    scanned: &FxHashSet<&str>,
    out: &mut BTreeSet<String>,
) {
    let mut insert = |text: &str| {
        if scanned.contains(text) && !out.contains(text) {
            out.insert(text.to_owned());
        }
    };
    for_each_token(root, |kind, text| {
        if kind.is_trivia() {
            return;
        }
        insert(text);
        if kind == SyntaxKind::STRING {
            let content = text
                .trim_start_matches(['"', '\''])
                .trim_end_matches(['"', '\'']);
            if !content.is_empty() {
                insert(content);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantics::RootDatabase;
    use semantics::metadata::parse_namespace;
    use semantics::stubs::{install_shipped_stubs, namespace_exports, namespace_known};

    fn stub_backed<'db>(
        db: &'db RootDatabase,
    ) -> (
        impl Fn(&str) -> bool + 'db,
        impl Fn(&str, &str) -> bool + 'db,
    ) {
        (
            move |namespace: &str| namespace_known(db, namespace).unwrap_or(false),
            move |namespace: &str, name: &str| namespace_exports(db, namespace, name),
        )
    }

    fn problems_for(source: &str) -> Vec<String> {
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let (knows, exports) = stub_backed(&db);
        namespace_import_problems(&parse_namespace(source).imports, &knows, &exports)
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
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let (knows, exports) = stub_backed(&db);
        let problems = namespace_import_problems(
            &parse_namespace("importFrom(stats, sd, medain)\n").imports,
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
        let namespace = parse_namespace(namespace);
        let scanned = token_scan_names(&namespace.imports, &namespace.exports);
        let mut used = BTreeSet::new();
        for source in sources {
            collect_used_tokens(&syntax::parse(source).syntax_node(), &scanned, &mut used);
        }
        unused_import_diagnostics(&namespace.imports, &used, level)
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
