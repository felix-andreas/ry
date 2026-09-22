//! The diagnostic edge: item-relative findings become file diagnostics with
//! absolute positions, and structured type errors become wording.
//!
//! One type-display policy, one renderer: inference variables and rigid
//! binders display as `T`/`U`/`V` in first-occurrence order, and one renderer
//! instance must span everything that shares names (both sides of an
//! expected/found message), because a fresh renderer restarts the numbering.

use crate::check::{OperandExpectation, TypeError, TypeErrorKind};
use crate::infer::{FunctionMismatch, RecordMismatch};
use crate::types::{Atomic, Constraint, FunctionType, Name, Ty, TyKind, TypeScheme};
use crate::{Db, DocumentKind, SourceFile, item_check, item_tree, parse};
use syntax::TextRange;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, salsa::SalsaValue)]
pub enum Severity {
    Error,
    Warning,
}

/// One rendered finding, file-absolute.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct Diagnostic {
    pub range: TextRange,
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    /// Companion locations for findings whose story spans sites (the sibling
    /// definition an overwrite warning refers to). Rendered as `note:` lines
    /// by the CLI and as related information over LSP.
    pub related: Vec<RelatedLocation>,
}

/// One companion location of a diagnostic, possibly in another file.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct RelatedLocation {
    pub file: SourceFile,
    pub range: TextRange,
    pub message: &'static str,
}

/// The diagnostics that are pure functions of the parse. They are the syntax
/// errors, the typing-directive errors, and the `#:` block-form refusals. A
/// host's fast publication wave serves exactly these, plus the lints, so typing
/// never waits on naming or type checking. [`file_diagnostics`] starts from the
/// same set, which keeps the fast wave a faithful subset of the settled one.
#[salsa::tracked(returns(clone))]
pub fn parse_stage_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let parsed = parse(db, file);
    for error in parsed.errors() {
        diagnostics.push(Diagnostic {
            range: error.range,
            severity: Severity::Error,
            // The code says whose grammar was broken, not which stage noticed:
            // `syntax-error` is R the parser could not read, `annotation` is a
            // `#:` comment that is wrong. Keying it on the stage instead put a
            // deliberate expressiveness limit like the higher-rank refusal
            // under `syntax-error`, while its siblings report as `annotation`
            // because lowering rather than parsing happened to catch them. An
            // unknown constraint and a malformed block are such siblings.
            code: if error.in_annotation {
                "annotation"
            } else {
                "syntax-error"
            },
            message: error.message.clone(),
            related: Vec::new(),
        });
    }

    for (range, value) in crate::file_typing_directives(db, file).1 {
        diagnostics.push(Diagnostic {
            range,
            severity: Severity::Error,
            code: "annotation",
            message: format!(
                "Unknown typing directive `{value}`. Use `# typing: on`, `# typing: off`, or `# typing: strict`."
            ),
            related: Vec::new(),
        });
    }

    let root = parsed.syntax_node();
    for node in root
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        let annotation = crate::annotations::lower_annotation(db, &node);
        for (message, range) in &annotation.errors {
            diagnostics.push(Diagnostic {
                range: *range,
                severity: Severity::Error,
                code: "annotation",
                message: message.clone(),
                related: Vec::new(),
            });
        }
        if !annotation.definitions.is_empty() && node.parent().as_ref() != Some(&root) {
            diagnostics.push(Diagnostic {
                range: node.text_range(),
                severity: Severity::Error,
                code: "annotation",
                message: "Type definition blocks are only allowed at the top level of a file."
                    .to_owned(),
                related: Vec::new(),
            });
        }
    }
    diagnostics.extend(dangling_annotation_diagnostics(db, file));
    diagnostics
}

/// Errors for annotations that promise typing for an expression but have
/// none: the annotated expression must start on the very next line (see
/// `statement_annotations` for the association rule). Every statement sequence
/// is checked, which is the file root and every braced block. A `@type` or
/// `@alias` definition block and an `@strict` toggle stand alone, and a block
/// already refused for its shape reports only that refusal.
///
/// A block inside a call's argument list is reported separately, because no
/// statement can follow it there. The association walk sees an `ARGUMENT` node
/// next, which is not an expression kind, so the block attaches to nothing. The
/// sequence walk never visits an argument list, so it said nothing either. A deliberately wrong type beside a lambda argument was accepted in
/// silence. Every other position that looks like it might dangle does attach,
/// measured against a no-annotation control: a braceless function body, a
/// braceless `if` branch, and a parenthesised expression all apply their
/// annotation, so none of them may be reported here.
fn dangling_annotation_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let parsed = parse(db, file);
    let root = parsed.syntax_node();
    let mut diagnostics = Vec::new();
    let sequences = std::iter::once(root.clone()).chain(
        root.descendants()
            .filter(|node| node.kind() == syntax::SyntaxKind::BRACE_EXPR),
    );
    for (node, target) in sequences.flat_map(|parent| crate::statement_annotations(&parent)) {
        let annotation = crate::annotations::lower_annotation(db, &node);
        if !annotation.definitions.is_empty()
            || annotation.strict.is_some()
            || !annotation.errors.is_empty()
            || !annotation.typing_errors.is_empty()
        {
            continue;
        }
        let message = match target {
            crate::AnnotationTarget::Attached(_) => {
                // The association holds; the only remaining shape problem is
                // a block with no content at all.
                if node.children().next().is_none() {
                    "A `#:` typing comment must include a type expression."
                } else {
                    continue;
                }
            }
            crate::AnnotationTarget::BlankLineSeparated => {
                "A `#:` typing comment cannot be separated from its expression by an empty line."
            }
            crate::AnnotationTarget::Dangling => {
                // Overwhelmingly this is a `#:` block placed above a roxygen2
                // block instead of below it, so the message names the fix
                // rather than only the rule: every documented function in a
                // package has roxygen above it, and the order is the one thing
                // a reader cannot guess.
                "A `#:` typing comment must be followed immediately by an expression. Put it directly above the definition, below any roxygen2 block."
            }
        };
        diagnostics.push(Diagnostic {
            range: node.text_range(),
            severity: Severity::Error,
            code: "annotation",
            message: message.to_owned(),
            related: Vec::new(),
        });
    }
    for node in root
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        if node.parent().map(|parent| parent.kind()) != Some(syntax::SyntaxKind::ARGUMENT_LIST) {
            continue;
        }
        // A block that already failed to lower owns its own diagnostic; a
        // placement error on top would report one mistake twice.
        let annotation = crate::annotations::lower_annotation(db, &node);
        if !annotation.errors.is_empty() || !annotation.typing_errors.is_empty() {
            continue;
        }
        diagnostics.push(Diagnostic {
            range: node.text_range(),
            severity: Severity::Error,
            code: "annotation",
            // Deliberately not "move it up a line": annotating the enclosing
            // statement types that statement, not the argument the block was
            // aimed at, and a lambda parameter has no annotatable position at
            // all. Lifting the function to its own binding is the only thing
            // that actually types it.
            message: "A `#:` typing comment annotates the statement that follows it, and an argument is not a statement, so this one annotates nothing. Give the value its own binding and annotate that.".to_owned(),
            related: Vec::new(),
        });
    }
    diagnostics
}

/// name → the index of the earliest item whose top-level frame writes it.
/// Conditional writes (inside a top-level `for`/`while`/`if`/`repeat` or a
/// bare block) create the document's variable slot exactly like an
/// unconditional `name <- value`, so they participate: a later read in the
/// same document resolves to the slot even though the name is not
/// package-visible.
#[salsa::tracked(returns(ref))]
fn frame_slot_positions(db: &dyn Db, file: SourceFile) -> rustc_hash::FxHashMap<String, usize> {
    let mut positions = rustc_hash::FxHashMap::default();
    for (index, &item) in item_tree(db, file).iter().enumerate() {
        let Some(naming) = crate::item_naming(db, item) else {
            continue;
        };
        for binding in naming.bindings.values() {
            if binding.kind != crate::naming::BindingKind::TopLevel {
                continue;
            }
            positions.entry(binding.name.clone()).or_insert(index);
        }
    }
    positions
}

/// All diagnostics of one file: syntax errors, naming findings, and type
/// errors, in position order.
#[salsa::tracked(returns(clone))]
pub fn file_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let mut diagnostics = parse_stage_diagnostics(db, file);

    // A broken statement contributes nothing beyond the syntax error covering
    // it: its names, reads and inferred types are all suspect, so judging them
    // turns one mistake into a report storm. Only an R-grammar error counts
    // here, because an annotation-grammar error leaves the R statement
    // intact.
    let broken_ranges: Vec<TextRange> = parse(db, file)
        .errors()
        .iter()
        .filter(|error| !error.in_annotation)
        .map(|error| error.range)
        .collect();
    let is_broken = |range: TextRange| {
        broken_ranges
            .iter()
            .any(|error| error.start() <= range.end() && range.start() <= error.end())
    };

    // Walk the spans, not the item tree: they already pair each item with its
    // absolute range, so the offset needs no per-item lookup.
    for (item_index, span) in crate::item_spans(db, file).iter().enumerate() {
        if is_broken(span.range) {
            continue;
        }
        let item = span.item;
        let offset = span.range.start();
        let Some(check) = item_check(db, item) else {
            continue;
        };
        for error in &check.errors {
            let range = TextRange::new(error.range.start() + offset, error.range.end() + offset);
            diagnostics.push(render_type_error(db, range, error));
        }
        // Naming findings are item-relative too.
        let Some(module) = crate::item_hir(db, item) else {
            continue;
        };
        let Some(naming) = crate::item_naming(db, item) else {
            continue;
        };
        for unused in &naming.unused_assignments {
            let range = TextRange::new(unused.range.start() + offset, unused.range.end() + offset);
            diagnostics.push(Diagnostic {
                range,
                severity: Severity::Warning,
                code: "unused",
                message: format!("`{}` is assigned but never used.", unused.name),
                related: Vec::new(),
            });
        }
        for target in &naming.invalid_assignment_targets {
            let range = TextRange::new(target.range.start() + offset, target.range.end() + offset);
            // The dominant cause is a trailing operator pulling the next line
            // in as its right-hand operand. It is that shape only when the
            // line break falls AFTER the operator: a break before it (`a`
            // newline `+ b`) is an ordinary continuation, and a multi-line
            // left operand says nothing about where the operator sits.
            let dangling_operator = target.operand_gap.is_some_and(|gap| {
                file.text(db)
                    .get(usize::from(gap.start() + offset)..usize::from(gap.end() + offset))
                    .is_some_and(|between| {
                        let through_operator = between.trim_end_matches(char::is_whitespace);
                        between[through_operator.len()..].contains('\n')
                    })
            });
            let hint = if dangling_operator {
                ". The operator at the end of this line pulled the next line in as its right-hand side"
            } else {
                ""
            };
            diagnostics.push(Diagnostic {
                range,
                severity: Severity::Error,
                code: "syntax-error",
                message: format!(
                    "this is a value, not a name, so nothing can be assigned to it{hint}"
                ),
                related: Vec::new(),
            });
        }
        // A read some path reaches with no prior write. R errors there with
        // `object 'x' not found`, so the read is worth naming even though it
        // resolves. Only a *local* slot qualifies. A top-level variable's
        // unwritten path reaches the enclosing environment at run time, where
        // the name's cross-item binding answers it.
        for expression in &naming.maybe_undefined {
            let Some(binding) = naming
                .resolutions
                .get(expression)
                .and_then(|slot| naming.bindings.get(slot))
            else {
                continue;
            };
            if binding.kind == crate::naming::BindingKind::TopLevel {
                continue;
            }
            let expression_range = module.expression(*expression).range;
            let range = TextRange::new(
                expression_range.start() + offset,
                expression_range.end() + offset,
            );
            diagnostics.push(Diagnostic {
                range,
                severity: Severity::Warning,
                code: "maybe-undefined",
                message: format!(
                    "`{}` is only assigned on some paths here, so this read can find nothing.",
                    display_name(&binding.name)
                ),
                related: Vec::new(),
            });
        }
        for (expression, read) in &naming.namespace_reads {
            let Some(message) = namespace_read_message(db, read) else {
                continue;
            };
            let expression_range = module.expression(*expression).range;
            let range = TextRange::new(
                expression_range.start() + offset,
                expression_range.end() + offset,
            );
            diagnostics.push(Diagnostic {
                range,
                severity: Severity::Warning,
                code: "unresolved",
                message,
                related: Vec::new(),
            });
        }
        // Scripts resolve reads against the same universe as package files
        // (their own bindings, the package interface, imports, builtins), so
        // an unresolved read warns in both document kinds. Document-local
        // variable slots resolve sequentially: an immediate read sees only
        // slots created by earlier statements (a use before every definition
        // is unresolved, matching the top-down run), while a read from
        // inside a nested function is deferred. The closure runs after the
        // frame settled, so any slot in the document resolves it, including the
        // enclosing statement's own target, which is self-recursion.
        for (expression, name) in &naming.non_locals {
            let NonLocalRead::Unresolved { suggestion } =
                classify_non_local_read(db, file, item_index, &check, naming, expression, name)
            else {
                continue;
            };
            let expression_range = module.expression(*expression).range;
            let range = TextRange::new(
                expression_range.start() + offset,
                expression_range.end() + offset,
            );
            let display = display_name(name);
            let suggestion = suggestion
                .map(|nearest| format!(" Did you mean `{nearest}`?"))
                .unwrap_or_default();
            diagnostics.push(Diagnostic {
                range,
                severity: Severity::Warning,
                code: "unresolved",
                message: format!(
                    "I could not resolve `{display}` in this package, its imports, or builtins.{suggestion}"
                ),
                related: Vec::new(),
            });
        }
    }

    if *file.kind(db) == DocumentKind::Script {
        diagnostics.extend(script_unused_bindings(db, file));
    }
    diagnostics.extend(duplicate_binding_diagnostics(db, file));
    diagnostics.extend(duplicate_type_diagnostics(db, file));
    diagnostics.extend(unknown_type_diagnostics(db, file));
    diagnostics.extend(annotation_rule_diagnostics(db, file));

    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    diagnostics
}

/// What resolution made of one non-local read.
///
/// Two diagnostic streams depend on this and must not disagree, so the
/// classification lives in one place. The ordinary check reports what nothing
/// explains, and strict mode reports what an unknowable export set silenced.
/// That is *exactly* the set the ordinary check let through.
enum NonLocalRead {
    /// Something resolves it after all, so it is not a finding at all.
    Resolvable,
    /// Nothing in the project or the corpus defines it, and an attached package
    /// whose export set cannot be known earned it silence.
    Tolerated,
    /// Nothing defines it and nothing excuses it.
    Unresolved { suggestion: Option<String> },
}

/// The guards are ordered cheapest first. The file-local resolutions, which
/// are the masked reads and the document's own frame slots, come before the
/// project-wide and corpus-wide ones. In a script-heavy workspace most
/// non-local reads are cross-statement reads that the frame slots resolve.
fn classify_non_local_read(
    db: &dyn Db,
    file: SourceFile,
    item_index: usize,
    check: &crate::check::ItemCheck<'_>,
    naming: &crate::naming::ItemNaming,
    expression: &crate::hir::ExprId,
    name: &str,
) -> NonLocalRead {
    if check.masked_reads.contains(expression) {
        return NonLocalRead::Resolvable;
    }
    if let Some(&earliest) = frame_slot_positions(db, file).get(name)
        && (earliest < item_index || naming.deferred_non_locals.contains(expression))
    {
        return NonLocalRead::Resolvable;
    }
    if R6_INJECTED_BINDINGS.contains(&name) && *file_defines_r6_class(db, file) {
        return NonLocalRead::Resolvable;
    }
    if crate::package_scheme_exists(db, name)
        || super_globals(db, file).contains(name)
        || declared_global_variable(db, name)
        || crate::metadata::imported_by_name(db, name)
    {
        return NonLocalRead::Resolvable;
    }
    // A near-miss of a name the project itself binds is reported even under the
    // blanket tolerance an unknowable export set earns. `library(shiny)` cannot
    // explain `repositry` in a function whose parameter is `repository`, and
    // `library(testthat)` cannot explain `validte_url` in a project that defines
    // `validate_url`: a name one edit away from a binding of your own is a typo,
    // not somebody else's export. Locals and parameters of the enclosing item
    // count as well as the project's top-level definitions. A typo of a name in
    // lexical scope on the same line is the case a user is least willing to see
    // missed.
    let project_typo = project_definition_suggestion(db, name).or_else(|| {
        nearest_name(
            name,
            naming
                .bindings
                .values()
                .map(|binding| binding.name.as_str()),
        )
        .map(str::to_owned)
    });
    if project_typo.is_none() && crate::metadata::imports_every_name(db) {
        return NonLocalRead::Tolerated;
    }
    NonLocalRead::Unresolved {
        suggestion: project_typo.or_else(|| unresolved_suggestion(db, name)),
    }
}

/// Errors for `#:` type references no vocabulary declares: a name in an
/// annotation must be a built-in type (excluded at lowering already), an
/// in-scope binder (likewise), a project `@type`/`@alias` declaration, or a
/// stub-declared class. Anything else is a typo the checker would otherwise
/// silently treat as an opaque nominal. The classic case is a misspelled
/// record field type inside a `@type` body.
fn unknown_type_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let parsed = parse(db, file);
    let mut references: Vec<(String, TextRange)> = Vec::new();
    for node in parsed
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        references.extend(crate::annotations::lower_annotation(db, &node).nominal_references);
    }
    if references.is_empty() {
        return Vec::new();
    }

    let mut known: std::collections::BTreeSet<String> = crate::stubs::stubs(db)
        .map(|library| library.nominals.iter().cloned().collect())
        .unwrap_or_default();
    if let Some(files) = crate::ProjectFiles::try_get(db) {
        for name in crate::project_type_definitions(db, files).keys() {
            known.insert(name.text(db).to_owned());
        }
    }
    // A script's own declarations shadow the project namespace; for a
    // package file this adds nothing the project map lacks.
    for definition in crate::file_type_definitions(db, file) {
        known.insert(definition.name.text(db).to_owned());
    }

    references
        .into_iter()
        .filter(|(name, _)| !known.contains(name))
        .map(|(name, range)| {
            let suggestion = nearest_name(
                &name,
                known
                    .iter()
                    .map(String::as_str)
                    .chain(ANNOTATION_PRIMITIVES.iter().copied()),
            )
            .map(|nearest| format!(" Did you mean `{nearest}`?"))
            .unwrap_or_default();
            Diagnostic {
                range,
                severity: Severity::Error,
                code: "annotation",
                message: format!(
                    "I do not know the type `{name}`. It is not a built-in type, a declared `@type` or `@alias` name, or a class from the standard-library stubs.{suggestion}"
                ),
                related: Vec::new(),
            }
        })
        .collect()
}

/// Vocabulary-dependent annotation rules: check-depth refusals, the
/// atomic-element requirement of `[]` / `[named]` vector types, and `@new`
/// naming an alias. The depth finding and the vector finding carry the typing
/// code, so they disappear when typing is off, like the checks they stand in
/// for.
fn annotation_rule_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let parsed = parse(db, file);
    let mut diagnostics = Vec::new();
    let mut definitions: std::collections::BTreeMap<String, crate::annotations::NamedDefinition> =
        std::collections::BTreeMap::new();
    // Names declared more than once across the package have no single
    // winning definition; alias-based judgments would be arbitrary, and the
    // duplicate-declaration error already reports the real mistake.
    let mut ambiguous: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(files) = crate::ProjectFiles::try_get(db) {
        for (name, definition) in crate::project_type_definitions(db, files) {
            definitions.insert(name.text(db).to_owned(), definition.clone());
        }
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for &project_file in files.files(db) {
            if *project_file.kind(db) != DocumentKind::Package {
                continue;
            }
            for (name, _) in type_declaration_sites(db, project_file) {
                *counts.entry(name.as_str()).or_default() += 1;
            }
        }
        ambiguous.extend(
            counts
                .into_iter()
                .filter(|(_, count)| *count >= 2)
                .map(|(name, _)| name.to_owned()),
        );
    }
    for definition in crate::file_type_definitions(db, file) {
        definitions.insert(definition.name.text(db).to_owned(), definition);
    }
    let stub_nominals: std::collections::BTreeSet<String> = crate::stubs::stubs(db)
        .map(|library| library.nominals.iter().cloned().collect())
        .unwrap_or_default();

    for node in parsed
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        let annotation = crate::annotations::lower_annotation(db, &node);
        for (message, range) in &annotation.typing_errors {
            diagnostics.push(Diagnostic {
                range: *range,
                severity: Severity::Error,
                code: "type-mismatch",
                message: message.clone(),
                related: Vec::new(),
            });
        }
        for (element, range) in &annotation.vector_elements {
            if vector_element_atomic(db, *element, &definitions, &stub_nominals) {
                continue;
            }
            let mut renderer = TypeRenderer::default();
            let rendered = renderer.render(db, *element);
            diagnostics.push(Diagnostic {
                range: *range,
                severity: Severity::Error,
                code: "type-mismatch",
                message: format!(
                    "the element of a `[]` vector type must be an atomic type, found `{rendered}`. For a list of these, write `list[{rendered}]`"
                ),
                related: Vec::new(),
            });
        }
        // An applied name must be a generic with exactly that many
        // parameters, and a BARE reference to a generic must be applied. The
        // exception is `@new`, where an unapplied generic infers its arguments
        // through the representation check, which the typing reference
        // documents.
        let declared_arity = |name: &str| {
            definitions
                .get(name)
                .map(|definition| definition.parameters.len())
                .or_else(|| stub_nominals.contains(name).then_some(0))
        };
        for (name, count, range) in &annotation.applied_references {
            let Some(arity) = declared_arity(name) else {
                continue;
            };
            if arity == 0 {
                diagnostics.push(Diagnostic {
                    range: *range,
                    severity: Severity::Error,
                    code: "annotation",
                    message: format!("`{name}` is not a generic type. It takes no type arguments."),
                    related: Vec::new(),
                });
            } else if arity != *count {
                diagnostics.push(Diagnostic {
                    range: *range,
                    severity: Severity::Error,
                    code: "annotation",
                    message: format!(
                        "generic type `{name}` expects {arity} type {}, but found {count}.",
                        plural(arity, "argument", "arguments"),
                    ),
                    related: Vec::new(),
                });
            }
        }
        for (name, range) in &annotation.nominal_references {
            if annotation
                .applied_references
                .iter()
                .any(|(_, _, applied)| applied == range)
            {
                continue;
            }
            if let Some((_, _, new_range)) = &annotation.new_nominal
                && new_range.contains_range(*range)
            {
                continue;
            }
            let Some(arity) = declared_arity(name) else {
                continue;
            };
            if arity > 0 {
                diagnostics.push(Diagnostic {
                    range: *range,
                    severity: Severity::Error,
                    code: "annotation",
                    message: format!(
                        "generic type `{name}` expects {arity} type {}, but found 0.",
                        plural(arity, "argument", "arguments"),
                    ),
                    related: Vec::new(),
                });
            }
        }
        if let Some((name, _, range)) = &annotation.new_nominal
            && !ambiguous.contains(name.text(db))
            && definitions
                .get(name.text(db))
                .is_some_and(|definition| definition.alias)
        {
            diagnostics.push(Diagnostic {
                range: *range,
                severity: Severity::Error,
                code: "annotation",
                message: format!(
                    "`@new` requires a nominal type declared with `@type`, but `{}` is an alias.",
                    name.text(db)
                ),
                related: Vec::new(),
            });
        }
    }
    diagnostics
}

/// Whether a vector element type is atomic: an atomic scalar, a type
/// parameter (its use adds the atomic bound), tolerance types, or an alias
/// expanding to one. A nominal is opaque and therefore never atomic. An
/// undeclared name stays silent here, because the unknown-type error already
/// reports it.
fn vector_element_atomic<'db>(
    db: &'db dyn Db,
    element: Ty<'db>,
    definitions: &std::collections::BTreeMap<String, crate::annotations::NamedDefinition<'db>>,
    stub_nominals: &std::collections::BTreeSet<String>,
) -> bool {
    let mut expanding: Vec<String> = Vec::new();
    let mut current = element;
    loop {
        return match current.kind(db) {
            TyKind::Scalar(_) | TyKind::Rigid(_) | TyKind::Unknown | TyKind::Any => true,
            TyKind::Named(name, _) => {
                let name = name.text(db);
                match definitions.get(name) {
                    Some(definition) if definition.alias => {
                        if expanding.iter().any(|seen| seen == name) {
                            return false;
                        }
                        expanding.push(name.to_owned());
                        current = definition.body;
                        continue;
                    }
                    Some(_) => false,
                    None => !stub_nominals.contains(name),
                }
            }
            _ => false,
        };
    }
}

/// The built-in type names annotations may spell, as typo-suggestion
/// candidates (lowering resolves them before the unknown-name check, so they
/// never appear as references).
const ANNOTATION_PRIMITIVES: &[&str] = &[
    "Any",
    "Unknown",
    "NULL",
    "logical",
    "integer",
    "double",
    "complex",
    "character",
    "raw",
];

/// The named top-level definition sites of one package file, in item order:
/// each name with its binding-name range (file-absolute).
#[salsa::tracked(returns(ref))]
fn top_level_name_sites(db: &dyn Db, file: SourceFile) -> Vec<(String, TextRange)> {
    let mut sites = Vec::new();
    for &item in item_tree(db, file) {
        if !matches!(
            *item.kind(db),
            crate::ItemKind::Function | crate::ItemKind::Value
        ) {
            continue;
        }
        let Some(name) = item.name(db).clone() else {
            continue;
        };
        let Some(node) = crate::item_node(db, item) else {
            continue;
        };
        let range = node
            .descendants()
            .find(|child| {
                child.kind() == syntax::SyntaxKind::NAME && child.text().to_string() == name
            })
            .map(|child| child.text_range())
            .unwrap_or_else(|| TextRange::empty(node.text_range().start()));
        sites.push((name, range));
    }
    sites
}

/// The named top-level definition names of one package file, in item order.
/// This is a range-free projection of [`top_level_name_sites`], so the
/// project-wide duplicate map depends only on which names exist and never on
/// where they are. A body edit that shifts ranges therefore leaves this value
/// equal, and the map validates without re-executing.
#[salsa::tracked(returns(ref))]
fn top_level_binding_names(db: &dyn Db, file: SourceFile) -> Vec<String> {
    let mut names = Vec::new();
    for &item in item_tree(db, file) {
        if !matches!(
            *item.kind(db),
            crate::ItemKind::Function | crate::ItemKind::Value
        ) {
            continue;
        }
        if let Some(name) = item.name(db).clone() {
            names.push(name);
        }
    }
    names
}

/// Package names defined at top level more than once, each mapped to its
/// definition sites' files in project order (a file repeats per site).
/// Healthy projects keep this near-empty, so the value-equality firewall
/// holds: per-file diagnostics re-validate cheaply after edits elsewhere.
#[salsa::tracked(returns(ref))]
fn duplicate_binding_map(
    db: &dyn Db,
    files: crate::ProjectFiles,
) -> rustc_hash::FxHashMap<String, Vec<SourceFile>> {
    let mut sites: rustc_hash::FxHashMap<String, Vec<SourceFile>> =
        rustc_hash::FxHashMap::default();
    for &project_file in files.files(db) {
        if *project_file.kind(db) != DocumentKind::Package {
            continue;
        }
        for name in top_level_binding_names(db, project_file) {
            sites.entry(name.clone()).or_default().push(project_file);
        }
    }
    sites.retain(|_, files| files.len() >= 2);
    sites
}

/// Warnings for a package name defined at top level more than once: per-site
/// winner semantics make every earlier binding dead, so each site warns, with
/// a note pointing at its nearest neighbouring definition. Occurrence order
/// is project order, then item order within a file. `ProjectFiles` lists
/// package documents first, in workspace path order. Ranges are fetched
/// only for the files actually involved in a duplication, so the common
/// duplicate-free file never reads another file's positions.
fn duplicate_binding_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    if *file.kind(db) != DocumentKind::Package {
        return Vec::new();
    }
    let Some(files) = crate::ProjectFiles::try_get(db) else {
        return Vec::new();
    };
    let map = duplicate_binding_map(db, files);
    if map.is_empty() {
        return Vec::new();
    }
    let mut own_duplicated: Vec<&str> = Vec::new();
    for name in top_level_binding_names(db, file) {
        if map.contains_key(name) && !own_duplicated.contains(&name.as_str()) {
            own_duplicated.push(name);
        }
    }
    let mut diagnostics = Vec::new();
    for name in own_duplicated {
        let mut sites: Vec<(SourceFile, TextRange)> = Vec::new();
        let mut seen_files: Vec<SourceFile> = Vec::new();
        for &site_file in &map[name] {
            if seen_files.contains(&site_file) {
                continue;
            }
            seen_files.push(site_file);
            for (site_name, range) in top_level_name_sites(db, site_file) {
                if site_name == name {
                    sites.push((site_file, *range));
                }
            }
        }
        for (index, &(site_file, range)) in sites.iter().enumerate() {
            if site_file != file {
                continue;
            }
            if index > 0 {
                let (earlier_file, earlier_range) = sites[index - 1];
                diagnostics.push(Diagnostic {
                    range,
                    severity: Severity::Warning,
                    code: "duplicate",
                    message: format!(
                        "Top-level binding `{name}` overwrites an earlier top-level binding in this package."
                    ),
                    related: vec![RelatedLocation {
                        file: earlier_file,
                        range: earlier_range,
                        message: "the earlier binding is here.",
                    }],
                });
            }
            if index + 1 < sites.len() {
                let (later_file, later_range) = sites[index + 1];
                diagnostics.push(Diagnostic {
                    range,
                    severity: Severity::Warning,
                    code: "duplicate",
                    message: format!(
                        "Top-level binding `{name}` is overwritten by a later top-level binding in this package."
                    ),
                    related: vec![RelatedLocation {
                        file: later_file,
                        range: later_range,
                        message: "the later binding is here.",
                    }],
                });
            }
        }
    }
    diagnostics
}

/// The `@type` / `@alias` declaration sites of one package file, in
/// declaration order: each declared name with the range of its declaring
/// directive.
#[salsa::tracked(returns(ref))]
fn type_declaration_sites(db: &dyn Db, file: SourceFile) -> Vec<(String, TextRange)> {
    let parsed = parse(db, file);
    let mut sites = Vec::new();
    for node in parsed
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        let annotation = crate::annotations::lower_annotation(db, &node);
        for (definition, range) in annotation
            .definitions
            .iter()
            .zip(annotation.definition_sites.iter())
        {
            sites.push((definition.name.text(db).to_owned(), *range));
        }
    }
    sites
}

/// The declared type names of one package file, in declaration order. This is
/// the range-free projection of [`type_declaration_sites`], for the same
/// value-equality firewall as [`top_level_binding_names`].
#[salsa::tracked(returns(ref))]
fn type_declaration_names(db: &dyn Db, file: SourceFile) -> Vec<String> {
    type_declaration_sites(db, file)
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

/// Project-global type names declared more than once, mapped to their
/// declaring files in project order (a file repeats per declaration).
#[salsa::tracked(returns(ref))]
fn duplicate_type_map(
    db: &dyn Db,
    files: crate::ProjectFiles,
) -> rustc_hash::FxHashMap<String, Vec<SourceFile>> {
    let mut sites: rustc_hash::FxHashMap<String, Vec<SourceFile>> =
        rustc_hash::FxHashMap::default();
    for &project_file in files.files(db) {
        if *project_file.kind(db) != DocumentKind::Package {
            continue;
        }
        for name in type_declaration_names(db, project_file) {
            sites.entry(name.clone()).or_default().push(project_file);
        }
    }
    sites.retain(|_, files| files.len() >= 2);
    sites
}

/// Errors for a type name declared more than once: `@type` and `@alias` share
/// one namespace and every declaration participating in a duplicate-name
/// conflict is erroneous (see the typing reference). A duplicate is judged
/// against the namespace the declaration lives in, which differs by document
/// kind, so the two cases are separate walks. Package files share the
/// project-global namespace, and a script's declarations reach its own file
/// only. Occurrence
/// order is project order, then declaration order within a file; each site
/// points at its nearest neighbouring declaration.
fn duplicate_type_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    if *file.kind(db) != DocumentKind::Package {
        return script_duplicate_type_diagnostics(db, file);
    }
    let Some(files) = crate::ProjectFiles::try_get(db) else {
        return Vec::new();
    };
    let map = duplicate_type_map(db, files);
    if map.is_empty() {
        return Vec::new();
    }
    let mut own_duplicated: Vec<&str> = Vec::new();
    for name in type_declaration_names(db, file) {
        if map.contains_key(name) && !own_duplicated.contains(&name.as_str()) {
            own_duplicated.push(name);
        }
    }
    let mut diagnostics = Vec::new();
    for name in own_duplicated {
        let mut sites: Vec<(SourceFile, TextRange)> = Vec::new();
        let mut seen_files: Vec<SourceFile> = Vec::new();
        for &site_file in &map[name] {
            if seen_files.contains(&site_file) {
                continue;
            }
            seen_files.push(site_file);
            for (site_name, range) in type_declaration_sites(db, site_file) {
                if site_name == name {
                    sites.push((site_file, *range));
                }
            }
        }
        for (index, &(site_file, range)) in sites.iter().enumerate() {
            if site_file != file {
                continue;
            }
            let (neighbour_file, neighbour_range) = if index > 0 {
                sites[index - 1]
            } else {
                sites[index + 1]
            };
            diagnostics.push(Diagnostic {
                range,
                severity: Severity::Error,
                code: "annotation",
                message: format!(
                    "the type name `{name}` is declared more than once. `@type` and `@alias` declarations share one project-global namespace."
                ),
                related: vec![RelatedLocation {
                    file: neighbour_file,
                    range: neighbour_range,
                    message: "another declaration of this name is here.",
                }],
            });
        }
    }
    diagnostics
}

/// Duplicate type names inside one script. A script's type declarations reach
/// only its own file, and the next script cannot see them, so two scripts may
/// each declare a name without conflicting. The duplicate to report is
/// therefore a repeat *within* the file. Left unreported, the later declaration silently
/// won and every diagnostic it produced was unfalsifiable from the visible
/// source: a name declared `double` above and `character` below yields
/// ``expected `character`, found `double` `` with nothing to explain it.
fn script_duplicate_type_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let sites = type_declaration_sites(db, file);
    let mut diagnostics = Vec::new();
    for (index, (name, range)) in sites.iter().enumerate() {
        let Some((_, neighbour_range)) = sites
            .iter()
            .enumerate()
            .filter(|(other, (other_name, _))| *other != index && other_name == name)
            .map(|(other, (_, other_range))| (other, *other_range))
            .min_by_key(|(other, _)| index.abs_diff(*other))
        else {
            continue;
        };
        diagnostics.push(Diagnostic {
            range: *range,
            severity: Severity::Error,
            code: "annotation",
            message: format!(
                "the type name `{name}` is declared more than once. `@type` and `@alias` declarations share one namespace, which for a script is this file."
            ),
            related: vec![RelatedLocation {
                file,
                range: neighbour_range,
                message: "another declaration of this name is here.",
            }],
        });
    }
    diagnostics
}

/// The validation failure of one qualified read, if any. That is an unknown
/// namespace, or a name the namespace does not declare. The second applies to
/// `::` only, because `:::` reaches an unexported name. No stub corpus means no
/// validation.
fn namespace_read_message(db: &dyn Db, read: &crate::naming::NamespaceRead) -> Option<String> {
    // A package qualifying its own names reads definitions the checker already
    // holds, so the stub corpus has no say, and neither does validation. A
    // package's export set is not the set of names its sources bind. A
    // re-export (`importFrom(htmltools, validateCssUnit)` beside
    // `export(validateCssUnit)`), an S4 generic from `setGeneric`, a lazy-
    // loaded dataset under `data/`, and a binding installed by `.onLoad` are
    // all exported without a definition in view. Every candidate report
    // measured across a CRAN corpus was one of those, so this stays silent
    // rather than guessing at the difference.
    if crate::metadata::is_own_package(db, &read.package) {
        return None;
    }
    match crate::stubs::namespace_known(db, &read.package)? {
        // A declared dependency without stubs is a real package the corpus
        // simply does not describe, so its reads stay quiet rather than
        // "unknown".
        false if crate::metadata::declared_dependency(db, &read.package) => None,
        false => Some(format!("unknown package namespace `{}`.", read.package)),
        true => {
            let name = read.name.as_ref()?;
            if !read.internal && !crate::stubs::namespace_exports(db, &read.package, name) {
                Some(format!("`{name}` is not exported by `{}`.", read.package))
            } else {
                None
            }
        }
    }
}

/// Names written by `<<-` with no enclosing binding anywhere in the file: R
/// creates them in the global environment, so reads of them resolve.
#[salsa::tracked(returns(ref))]
fn super_globals(db: &dyn Db, file: SourceFile) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for &item in item_tree(db, file) {
        if let Some(naming) = crate::item_naming(db, item) {
            names.extend(naming.super_globals.iter().cloned());
        }
    }
    names
}

/// Names declared via top-level `globalVariables(c("a", "b"))` /
/// `utils::globalVariables(...)` calls in one file. This is the
/// ecosystem-standard escape hatch for a name bound dynamically, through
/// non-standard evaluation or a generated binding. Reads of them resolve nowhere lexically on purpose,
/// so the unresolved check skips them package-wide. Only direct top-level
/// calls with literal string arguments are recognized.
#[salsa::tracked(returns(ref))]
fn file_global_variable_declarations(
    db: &dyn Db,
    file: SourceFile,
) -> std::collections::BTreeSet<String> {
    use crate::hir::{ExpressionKind, LiteralKind};
    let mut declared = std::collections::BTreeSet::new();
    for &item in item_tree(db, file) {
        let Some(module) = crate::item_hir(db, item) else {
            continue;
        };
        let Some(root) = module.root else {
            continue;
        };
        let ExpressionKind::Call { callee, arguments } = &module.expression(root).kind else {
            continue;
        };
        let is_global_variables = match &module.expression(*callee).kind {
            ExpressionKind::NameRef(name) => name == "globalVariables",
            ExpressionKind::Namespace { package, name, .. } => {
                package.as_deref() == Some("utils") && name.as_deref() == Some("globalVariables")
            }
            _ => false,
        };
        if !is_global_variables {
            continue;
        }
        let Some(first) = arguments.first().and_then(|argument| argument.value) else {
            continue;
        };
        // A single string, or a `c(...)` of strings. Only what is statically
        // knowable is declared, and any other entry is skipped.
        let mut collect = |id| {
            if let ExpressionKind::Literal(LiteralKind::String(value)) = &module.expression(id).kind
                && !value.is_empty()
            {
                declared.insert(value.clone());
            }
        };
        match &module.expression(first).kind {
            ExpressionKind::Call {
                callee: inner,
                arguments: entries,
            } if matches!(
                &module.expression(*inner).kind,
                ExpressionKind::NameRef(name) if name == "c"
            ) =>
            {
                for entry in entries {
                    if let Some(value) = entry.value {
                        collect(value);
                    }
                }
            }
            _ => {
                collect(first);
            }
        }
    }
    declared
}

/// Every name any project file declares via `globalVariables`, unioned once
/// per project revision. The per-read guard is then a single set lookup. A
/// per-file scan here multiplies by every non-local read in the project, and it
/// dominated whole-workspace diagnostics at real scale.
#[salsa::tracked(returns(ref))]
fn project_global_variable_declarations(
    db: &dyn Db,
    files: crate::ProjectFiles,
) -> std::collections::BTreeSet<String> {
    let mut declared = std::collections::BTreeSet::new();
    for &file in files.files(db).iter() {
        declared.extend(file_global_variable_declarations(db, file).iter().cloned());
    }
    declared
}

/// The bindings R6 injects into every method's enclosing environment. They
/// resolve nowhere lexically, because R6 builds them at construction. A read
/// of one inside a class that defines methods is therefore not an unresolved
/// name, exactly as `this` is not one in a JavaScript class.
const R6_INJECTED_BINDINGS: [&str; 3] = ["self", "private", "super"];

/// Whether a file constructs an R6 class, and so injects `self`/`private`/
/// `super` into the method bodies it contains. Purely syntactic, like the
/// other escape hatches here: a local binding shadowing `R6Class` is not
/// honored.
#[salsa::tracked]
fn file_defines_r6_class(db: &dyn Db, file: SourceFile) -> bool {
    use crate::hir::ExpressionKind;
    crate::item_spans(db, file).iter().any(|span| {
        crate::item_hir(db, span.item)
            .as_ref()
            .is_some_and(|module| {
                module.expressions.iter().any(|expression| {
                    let ExpressionKind::Call { callee, .. } = &expression.kind else {
                        return false;
                    };
                    match &module.expression(*callee).kind {
                        ExpressionKind::NameRef(name) => name == "R6Class",
                        ExpressionKind::Namespace { name, .. } => {
                            name.as_deref() == Some("R6Class")
                        }
                        _ => false,
                    }
                })
            })
    })
}

/// Whether any package file declares `name` via `globalVariables`.
fn declared_global_variable(db: &dyn Db, name: &str) -> bool {
    crate::ProjectFiles::try_get(db)
        .is_some_and(|files| project_global_variable_declarations(db, files).contains(name))
}

/// A name as R source spells it. A non-syntactic name needs backticks. A
/// leading dot must not be followed by a digit, so `.2way` is not syntactic.
fn display_name(name: &str) -> String {
    let mut characters = name.chars();
    let syntactic = matches!(characters.next(), Some(first) if first.is_alphabetic() || first == '.')
        && characters.all(|c| c.is_alphanumeric() || c == '.' || c == '_')
        && !(name.starts_with('.') && name[1..].chars().next().is_some_and(|c| c.is_ascii_digit()));
    if syntactic {
        name.to_owned()
    } else {
        format!("`{name}`")
    }
}

/// The nearest name in a small, closed candidate set, such as a container's
/// fields or a function's named parameters. The budget is the same as the
/// corpus-wide hint's. Here the set is the declaration itself, so a near-miss
/// is all but certainly the intent.
pub(crate) fn nearest_field_name<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<String> {
    nearest_name(name, candidates).map(str::to_owned)
}

/// The nearest of the project's own top-level definitions. This is checked
/// before the stub corpus, because a near-miss of a name this project defines
/// is far more likely the intent than a same-distance name from the standard
/// library.
fn project_definition_suggestion(db: &dyn Db, name: &str) -> Option<String> {
    let files = crate::ProjectFiles::try_get(db)?;
    let definitions = crate::package_definitions(db, files);
    nearest_name(name, definitions.keys().map(String::as_str)).map(str::to_owned)
}

/// The nearest stub name for a typo hint on an unresolved reference.
/// Memoized per name: the same unresolved name recurs across a project and
/// the candidate scan over the whole stub corpus is the expensive part.
fn unresolved_suggestion(db: &dyn Db, name: &str) -> Option<String> {
    typo_suggestion(db, crate::types::Name::new(db, name.to_owned()))
}

#[salsa::tracked(returns(clone))]
fn typo_suggestion<'db>(db: &'db dyn Db, name: crate::types::Name<'db>) -> Option<String> {
    let library = crate::stubs::stubs(db)?;
    nearest_name(
        name.text(db),
        library
            .schemes
            .keys()
            .chain(library.known_exports.iter())
            .map(String::as_str),
    )
    .map(str::to_owned)
}

/// The closest candidate within an edit-distance budget scaled to the name's
/// length; distance ties break to the lexicographically smallest candidate so
/// the hint is deterministic.
///
/// The budget is deliberately tight. It is one edit below eight characters and
/// two at or above. A wrong guess is worse than no guess, because `ggplot` is
/// not a typo of `biplot` and `aes` is not a typo of `abs`. On a short name two
/// edits change a third of it. Transposition counts as one edit, which is what
/// keeps the real typos inside that budget: `lenght` for `length`.
fn nearest_name<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let name_characters: Vec<char> = name.chars().collect();
    if name_characters.len() < 4 {
        return None;
    }
    let budget = if name_characters.len() >= 8 { 2 } else { 1 };
    let mut candidate_characters: Vec<char> = Vec::new();
    let mut previous: Vec<usize> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        if candidate == name {
            continue;
        }
        candidate_characters.clear();
        candidate_characters.extend(candidate.chars());
        let Some(distance) = edit_distance_within(
            &name_characters,
            &candidate_characters,
            budget,
            &mut previous,
            &mut current,
        ) else {
            continue;
        };
        best = match best {
            Some((best_distance, best_name))
                if (best_distance, best_name) <= (distance, candidate) =>
            {
                Some((best_distance, best_name))
            }
            _ => Some((distance, candidate)),
        };
    }
    best.map(|(_, candidate)| candidate)
}

/// Optimal string alignment distance, which is Levenshtein plus an adjacent
/// transposition as a single edit. Swapping two letters is the most common real
/// typo, and counting it as two edits would push it outside the budget
/// above. `None` when the distance exceeds `budget` (with a length
/// pre-check and an early bail once a whole DP row exceeds it). The caller
/// lends the DP rows so a scan over many candidates allocates nothing per
/// candidate.
fn edit_distance_within(
    left: &[char],
    right: &[char],
    budget: usize,
    previous: &mut Vec<usize>,
    current: &mut Vec<usize>,
) -> Option<usize> {
    if left.len().abs_diff(right.len()) > budget {
        return None;
    }
    // Transposition reads the row two above, so the rows rotate through three
    // buffers; the third lives here because only this function needs it.
    let mut before_previous: Vec<usize> = Vec::new();
    previous.clear();
    previous.extend(0..=right.len());
    current.clear();
    current.resize(right.len() + 1, 0);
    for (row, left_character) in left.iter().enumerate() {
        current[0] = row + 1;
        let mut row_minimum = current[0];
        for (column, right_character) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_character != right_character);
            let mut best = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
            if row > 0
                && column > 0
                && Some(left_character) == right.get(column - 1)
                && left.get(row - 1) == Some(right_character)
                && let Some(&transposed) = before_previous.get(column - 1)
            {
                best = best.min(transposed + 1);
            }
            current[column + 1] = best;
            row_minimum = row_minimum.min(best);
        }
        if row_minimum > budget {
            return None;
        }
        // Rotate: the row just computed becomes `previous`, the old
        // `previous` becomes `before_previous`, and the buffer they displace
        // is recycled as the next `current`.
        std::mem::swap(&mut before_previous, previous);
        std::mem::swap(previous, current);
        current.clear();
        current.resize(right.len() + 1, 0);
    }
    (previous[right.len()] <= budget).then_some(previous[right.len()])
}

/// A script's top level is one frame executed in order, so its bindings are
/// subject to the unused check across items: an assignment is dead when no
/// later item reads it before the next rebinding, and no nested function
/// reads the name at all. A deferred read runs after the frame is built, so it
/// keeps every write to the name observable, which is the captured-slot rule.
fn script_unused_bindings(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    // A broken statement reports its syntax error and nothing else, so a
    // definer inside an R-grammar error region never warns as unused.
    let error_ranges: Vec<TextRange> = crate::parse(db, file)
        .errors()
        .iter()
        .filter(|error| !error.in_annotation)
        .map(|error| error.range)
        .collect();
    struct Definer {
        item_index: usize,
        name: String,
        range: TextRange,
        used: bool,
        /// A conditionally executed write (inside a top-level loop or `if`):
        /// it rebinds the slot only on some runs, so it does not end the
        /// liveness of earlier definers of the name.
        conditional: bool,
    }
    let mut definers: Vec<Definer> = Vec::new();
    // Every item participates as a reader, so a bare `print(x)` statement
    // keeps `x` alive. The definers are the item's own named definition plus
    // any other top-level frame slot the item creates. A conditional write
    // inside a top-level `for`, `while`, or `if` binds the frame like any
    // assignment.
    let mut reads: Vec<(usize, String, bool)> = Vec::new();
    for (index, span) in crate::item_spans(db, file).iter().enumerate() {
        let item = span.item;
        let offset = span.range.start();
        let Some(naming) = crate::item_naming(db, item) else {
            continue;
        };
        let item_name = matches!(
            *item.kind(db),
            crate::ItemKind::Function | crate::ItemKind::Value
        )
        .then(|| item.name(db).clone())
        .flatten();
        let broken = |range: TextRange| {
            error_ranges
                .iter()
                .any(|error| error.start() <= range.end() && range.start() <= error.end())
        };
        if let Some(name) = item_name.clone()
            && let Some(module) = crate::item_hir(db, item)
            && let Some(root) = module.root
        {
            let root_range = module.expression(root).range;
            let statement_range =
                TextRange::new(root_range.start() + offset, root_range.end() + offset);
            // The finding belongs on the assigned name, not on the whole
            // statement: the value being computed is fine, the binding is what
            // is dead. `broken` still consults the whole statement, because a
            // syntax error anywhere in it suppresses the finding.
            let name_range = naming
                .bindings
                .values()
                .find(|binding| {
                    binding.kind == crate::naming::BindingKind::TopLevel && binding.name == name
                })
                .map_or(statement_range, |binding| {
                    TextRange::new(binding.range.start() + offset, binding.range.end() + offset)
                });
            if !broken(statement_range) {
                definers.push(Definer {
                    item_index: index,
                    name,
                    range: name_range,
                    used: false,
                    conditional: false,
                });
            }
        }
        for binding in naming.bindings.values() {
            if binding.kind != crate::naming::BindingKind::TopLevel
                || item_name.as_deref() == Some(binding.name.as_str())
            {
                continue;
            }
            let range =
                TextRange::new(binding.range.start() + offset, binding.range.end() + offset);
            if !broken(range) {
                definers.push(Definer {
                    item_index: index,
                    name: binding.name.clone(),
                    range,
                    // A read within the item itself (a loop reading its own
                    // carried variable) already keeps the definer alive.
                    used: naming.used_top_level_names.contains(&binding.name),
                    conditional: true,
                });
            }
        }
        for (expression, name) in &naming.non_locals {
            let deferred = naming.deferred_non_locals.contains(expression);
            reads.push((index, name.clone(), deferred));
        }
        // A quiet read (data masking, an opaque operator) is never reported
        // as unresolved, but at runtime it falls back to enclosing bindings,
        // so it keeps definers alive like any other read.
        for (expression, name) in &naming.quiet_reads {
            let deferred = naming.deferred_quiet_reads.contains(expression);
            reads.push((index, name.clone(), deferred));
        }
        // An operator use keeps every definition of that operator alive: R
        // resolves `%op%` when the expression runs, and a helper operator is
        // typically defined in one file and used across the project, so the
        // read is treated as deferred (order-independent).
        for name in &naming.quiet_operator_reads {
            reads.push((index, name.clone(), true));
        }
        // A maybe-undefined read of the item's own top-level slot (a loop
        // reading its carried variable) reaches the enclosing frame on the
        // unwritten path. Its first iteration reads the earlier binding, so it
        // counts as a cross-item read too.
        for expression in &naming.maybe_undefined {
            let Some(slot) = naming.resolutions.get(expression) else {
                continue;
            };
            let Some(binding) = naming.bindings.get(slot) else {
                continue;
            };
            if binding.kind == crate::naming::BindingKind::TopLevel {
                reads.push((index, binding.name.clone(), false));
            }
        }
    }
    for (read_index, name, deferred) in &reads {
        if *deferred {
            // A read from inside a function: the function may run any time
            // after the frame exists, so every write to the name is live.
            for definer in definers.iter_mut().filter(|d| d.name == *name) {
                definer.used = true;
            }
        } else {
            // An immediate read sees the binding current at its item, which is
            // the nearest earlier definer. A conditional definer rebinds only
            // on some runs, so marking continues past it until the nearest
            // unconditional one.
            for definer in definers
                .iter_mut()
                .rev()
                .filter(|d| d.name == *name && d.item_index < *read_index)
            {
                definer.used = true;
                if !definer.conditional {
                    break;
                }
            }
        }
    }
    // An S3 method is reached by dispatch, not by a read of its name, so the
    // liveness walk above cannot see the use: `speak.dog` is called by every
    // `speak(x)` on a dog. Reporting it dead is a false positive on code that
    // works, and the ecosystem's own answer, which is to export it, is
    // unavailable in a script.
    let generics = crate::s3_generics(db, file);
    definers
        .into_iter()
        .filter(|definer| {
            !definer.used
                && !definer.name.starts_with('.')
                && !definer.name.starts_with('_')
                && !crate::is_s3_method_name(db, &definer.name, &generics)
        })
        .map(|definer| Diagnostic {
            range: definer.range,
            severity: Severity::Warning,
            code: "unused",
            message: format!("`{}` is assigned but never used.", definer.name),
            related: Vec::new(),
        })
        .collect()
}

/// Strict-mode diagnostics: reports at `Unknown` origins, assembled
/// separately so hosts publish them only under `[check] strict` or the
/// per-file directive. An origin that is an assignment's value is phrased
/// against the assigned name (that is where the annotation goes); any other
/// origin is phrased against the expression itself.
#[salsa::tracked(returns(clone))]
pub fn strict_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    use crate::check::StrictOriginKind;
    use crate::hir::ExpressionKind;

    let mut diagnostics = Vec::new();
    // The blanket tolerance is the one hole strict mode used to leave open: an
    // attached package whose export set cannot be known silences every name
    // nothing defines, so a whole class of checking switched off project-wide
    // and the run still read as "I understood everything". Such a read IS
    // undetermined, which is what strict mode is for. The classification is
    // shared with the ordinary check, so this reports exactly the reads that
    // one let through.
    let broken_ranges: Vec<TextRange> = parse(db, file)
        .errors()
        .iter()
        .filter(|error| !error.in_annotation)
        .map(|error| error.range)
        .collect();
    for (item_index, span) in crate::item_spans(db, file).iter().enumerate() {
        if broken_ranges
            .iter()
            .any(|error| error.start() <= span.range.end() && span.range.start() <= error.end())
        {
            continue;
        }
        let item = span.item;
        let offset = span.range.start();
        let (Some(check), Some(module), Some(naming)) = (
            item_check(db, item),
            crate::item_hir(db, item),
            crate::item_naming(db, item),
        ) else {
            continue;
        };
        for (expression, name) in &naming.non_locals {
            if !matches!(
                classify_non_local_read(db, file, item_index, &check, naming, expression, name),
                NonLocalRead::Tolerated
            ) {
                continue;
            }
            let expression_range = module.expression(*expression).range;
            diagnostics.push(Diagnostic {
                range: TextRange::new(
                    expression_range.start() + offset,
                    expression_range.end() + offset,
                ),
                severity: Severity::Error,
                code: "strict",
                message: format!(
                    "strict mode: nothing this project can see defines `{}`. It is silent only because an attached package's exports are unknown. Declare the package with a `.Rtypes` stub to check it",
                    display_name(name)
                ),
                related: Vec::new(),
            });
        }
    }
    for span in crate::item_spans(db, file) {
        let item = span.item;
        let offset = span.range.start();
        let Some(check) = item_check(db, item) else {
            continue;
        };
        if check.strict_origins.is_empty() {
            continue;
        }
        let Some(module) = crate::item_hir(db, item) else {
            continue;
        };
        let mut assignment_targets = rustc_hash::FxHashMap::default();
        for expression in &module.expressions {
            if let ExpressionKind::Assign { target, value, .. } = &expression.kind
                && let ExpressionKind::NameRef(name) = &module.expression(*target).kind
            {
                assignment_targets.insert(*value, name.clone());
            }
        }
        for origin in &check.strict_origins {
            // A loop-widened or recursive origin is about the named binding,
            // never about the expression it happens to be the value of.
            let assignment_target = match &origin.kind {
                StrictOriginKind::LoopWidened(_) | StrictOriginKind::RecursiveUnknown(_) => None,
                _ => assignment_targets.get(&origin.expression),
            };
            let message = if let Some(name) = assignment_target {
                format!(
                    "strict mode: could not determine the type of `{name}`; add a type annotation"
                )
            } else {
                match &origin.kind {
                    StrictOriginKind::UnsupportedConstruct => {
                        "strict mode: this expression has an undetermined type (`Unknown`)"
                            .to_owned()
                    }
                    StrictOriginKind::UndeterminedReference(name) => format!(
                        "strict mode: could not determine the type of `{name}`; it has no known type"
                    ),
                    StrictOriginKind::LoopWidened(name) => format!(
                        "strict mode: could not determine the type of `{name}`, because it does not stabilize across loop iterations. Add a type annotation"
                    ),
                    StrictOriginKind::RecursiveUnknown(name) => format!(
                        "strict mode: could not determine the full type of `{name}`, because it is defined recursively. Add a type annotation"
                    ),
                }
            };
            diagnostics.push(Diagnostic {
                range: TextRange::new(origin.range.start() + offset, origin.range.end() + offset),
                severity: Severity::Error,
                code: "strict",
                message,
                related: Vec::new(),
            });
        }
    }
    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    diagnostics
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

fn render_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_type_error(db: &dyn Db, range: TextRange, error: &TypeError<'_>) -> Diagnostic {
    let message = render_type_error_message(db, error);
    Diagnostic {
        range,
        severity: Severity::Error,
        code: "type-mismatch",
        message,
        related: Vec::new(),
    }
}

fn render_type_error_message(db: &dyn Db, error: &TypeError<'_>) -> String {
    let mut renderer = TypeRenderer::default();
    match &error.kind {
        TypeErrorKind::Mismatch { expected, found } => format!(
            "expected `{}`, found `{}`",
            renderer.render(db, *expected),
            renderer.render(db, *found)
        ),
        TypeErrorKind::MissingFormalRead { name } => format!(
            "reading `{name}` here would fail at run time: this branch runs only when `{name}` is missing, and it has no default."
        ),
        TypeErrorKind::NotAFunction { found } => {
            format!(
                "this has type `{}`, which is not a function, so it cannot be called",
                renderer.render(db, *found)
            )
        }
        TypeErrorKind::ArityMismatch { expected, found } => {
            if found < expected {
                format!(
                    "this call supplies {found} {}, but the function requires {expected}, so a required argument is missing",
                    plural(*found, "argument", "arguments"),
                )
            } else {
                format!(
                    "this call passes {found} positional {}, but the function only takes {expected}",
                    plural(*found, "argument", "arguments"),
                )
            }
        }
        TypeErrorKind::NamedArgumentMismatch {
            argument,
            duplicate,
            suggestion,
            expected_parameters,
        } => {
            if *duplicate {
                return format!(
                    "this call gives `{argument}` more than once. R matches each named parameter at most once"
                );
            }
            if let Some(nearest) = suggestion {
                return format!(
                    "this function has no parameter `{argument}`. Did you mean `{nearest}`?"
                );
            }
            if expected_parameters.is_empty() {
                format!("this function has no parameter `{argument}`. It has no named parameters")
            } else {
                format!(
                    "this function has no parameter `{argument}`. Its named {} {}",
                    plural(expected_parameters.len(), "parameter is", "parameters are"),
                    render_names(expected_parameters),
                )
            }
        }
        TypeErrorKind::MaybeNullCallee { found } => format!(
            "this may be `NULL` here (its type is `{}`), so calling it is not safe. Guard it with `is.null()`, or give the `switch` a default branch",
            renderer.render(db, *found)
        ),
        TypeErrorKind::KnownTypeUnderIfUnknown { found } => format!(
            "`@if-unknown` applies only where the type is unknown, and this is already `{}`. Drop the annotation, or use `#:` to check the type or `@trust` to override it",
            renderer.render(db, *found)
        ),
        TypeErrorKind::AnnotationRequiredButDefaulted { name } => format!(
            "this annotation declares `{name}` as required, but the function gives it a default. Write `[{name}]` to declare it optional, or drop the default"
        ),
        TypeErrorKind::AnnotationParameterMismatch { name } => format!(
            "this annotation names a parameter `{name}`, but the function does not define one. An annotation parameter name must match the function's parameter name"
        ),
        TypeErrorKind::NullDefaultNotAdmitted { name, declared } => {
            let mut renderer = TypeRenderer::default();
            let declared = renderer.render(db, *declared);
            format!(
                "`{name}` defaults to `NULL`, which its declared type `{declared}` does not admit. A caller who omits it leaves `NULL` in the body. Declare it `{declared} | NULL` and narrow with `is.null()`"
            )
        }
        TypeErrorKind::AliasCycle { name } => {
            format!("Type alias `{name}` expands in a cycle.")
        }
        TypeErrorKind::ConstraintViolation { constraint, found } => format!(
            "expected {}, found `{}`",
            constraint_description(*constraint),
            renderer.render(db, *found)
        ),
        // A constrained position has no type to show. The constraint is what
        // refuses the value, so the message says what the body needs
        // instead.
        // Two record types differ in one field. Naming that field, and
        // printing only the two types at that position, is the whole point of
        // the variant. The alternative is a pair of long strings to diff by
        // eye.
        TypeErrorKind::RecordShape { mismatch } => match mismatch.as_ref() {
            RecordMismatch::Field {
                path,
                expected,
                found,
            } => format!(
                "expected `{}` for field `{}`, found `{}`",
                renderer.render(db, *expected),
                path.join("."),
                renderer.render(db, *found)
            ),
            RecordMismatch::Missing { path, near } => match near {
                Some(near) => format!(
                    "expected a field `{}` here, and this list has `{near}` instead. Check the spelling",
                    path.join(".")
                ),
                None => format!(
                    "expected a field `{}` here, which this list does not have",
                    path.join(".")
                ),
            },
            RecordMismatch::Unexpected { path, near } => match near {
                Some(near) => format!(
                    "this list has a field `{}`, which is not expected here. Did you mean `{near}`?",
                    path.join(".")
                ),
                None => format!(
                    "this list has a field `{}`, which is not expected here",
                    path.join(".")
                ),
            },
        },
        TypeErrorKind::CallbackShape { mismatch } => match mismatch.as_ref() {
            FunctionMismatch::Parameter {
                name,
                passed,
                accepts,
                constraint,
            } => {
                let passed = renderer.render(db, *passed);
                let position = match name {
                    Some(name) => format!("its parameter `{name}`"),
                    None => "its parameter".to_owned(),
                };
                match constraint {
                    Some(constraint) => format!(
                        "this function is passed `{passed}`, but {position} is used as {}",
                        constraint_description(*constraint)
                    ),
                    None => format!(
                        "this function is passed `{passed}`, but {position} accepts `{}`",
                        renderer.render(db, *accepts)
                    ),
                }
            }
            FunctionMismatch::Return {
                required,
                returns,
                constraint,
            } => {
                let required = renderer.render(db, *required);
                match constraint {
                    Some(constraint) => format!(
                        "this function must return `{required}`, but its body produces {}",
                        constraint_description(*constraint)
                    ),
                    None => format!(
                        "this function must return `{required}`, but it returns `{}`",
                        renderer.render(db, *returns)
                    ),
                }
            }
        },
        TypeErrorKind::NotAList { found } => {
            format!("expected a list, found `{}`", renderer.render(db, *found))
        }
        TypeErrorKind::NotIterable { found } => format!(
            "this `for` sequence is `{}`, which cannot be iterated. Expected a vector or list.",
            renderer.render(db, *found)
        ),
        TypeErrorKind::UnsupportedSubset { found } => {
            format!("`[` is not supported on `{}`", renderer.render(db, *found))
        }
        TypeErrorKind::BadVectorIndex { index } => format!(
            "a vector cannot be indexed by `{}`. Expected a numeric, logical, or character index",
            renderer.render(db, *index)
        ),
        TypeErrorKind::UnsupportedIndexShape { index_count } => match index_count {
            0 => "indexing with an empty index (`x[]`) is not supported yet".to_owned(),
            1 => "indexing with a named index argument is not supported yet".to_owned(),
            count => format!(
                "indexing with {count} indexes is not supported yet. ry does not model matrix and data.frame subsetting"
            ),
        },
        TypeErrorKind::PositionDoesNotExist {
            position,
            container,
        } => format!(
            "position {position} does not exist in `{}`",
            renderer.render(db, *container)
        ),
        TypeErrorKind::FieldDoesNotExist {
            suggestion,
            field,
            container,
        } => {
            let hint = suggestion
                .as_ref()
                .map(|nearest| format!(". Did you mean `{nearest}`?"))
                .unwrap_or_default();
            format!(
                "field `{field}` does not exist in `{}`{hint}",
                renderer.render(db, *container)
            )
        }
        TypeErrorKind::DollarOnAtomicVector { found } => format!(
            "R's `$` operator is invalid on atomic vectors, and this value is `{}`. Extract an element with `[[` instead.",
            renderer.render(db, *found)
        ),
        TypeErrorKind::InvalidOperand { expected, found } => {
            let expected_description = match expected {
                OperandExpectation::Numeric => "a numeric value (`integer` or `double`)",
                OperandExpectation::ScalarNumeric => {
                    "a scalar numeric value (`integer` or `double`)"
                }
                OperandExpectation::Logical => "a `logical` value",
                OperandExpectation::Comparable => {
                    "a comparable value (numeric, `character`, or `logical`)"
                }
            };
            format!(
                "expected {expected_description}, found `{}`",
                renderer.render(db, *found)
            )
        }
        TypeErrorKind::UnsupportedOperandPair {
            operator,
            left,
            right,
        } => format!(
            "`{operator}` is not defined between `{}` and `{}`",
            renderer.render(db, *left),
            renderer.render(db, *right)
        ),
        TypeErrorKind::NoMatchingOverload {
            name,
            candidates,
            first,
        } => {
            let mut message = format!(
                "no overload of `{name}` matches these arguments. I tried all {candidates} declared signatures"
            );
            if let Some(first) = first {
                message.push_str(&format!(
                    "; the first candidate fails with: {}",
                    render_type_error_message(db, first)
                ));
            }
            message
        }
        TypeErrorKind::InfiniteType {
            variable,
            container,
        } => format!(
            "I cannot construct an infinite type: {} occurs inside `{}`.",
            renderer.render(db, *variable),
            renderer.render(db, *container)
        ),
    }
}

/// The user-facing type renderer: `T`/`U`/`V`… in first-occurrence order.
#[derive(Default)]
pub struct TypeRenderer<'db> {
    names: Vec<RenderedVar<'db>>,
}

#[derive(PartialEq, Eq)]
enum RenderedVar<'db> {
    Inference(crate::types::InferenceVar),
    Rigid(Name<'db>),
}

/// A record field name as the `#:` grammar would have to spell it.
///
/// R lets a list carry any string as a name, so `list(\`max size\` = 10L)` is an
/// ordinary record. Rendering that field bare produces a type nobody can write
/// back, and one shape is worse than an error, because `list{a,b: integer}`
/// parses as a *different* type rather than failing. Every rendered type is copyable
/// into an annotation, so the renderer quotes what the grammar needs quoted.
fn spell_field_name(name: &str) -> String {
    match syntax::is_syntactic_name(name) {
        true => name.to_owned(),
        false => format!("`{name}`"),
    }
}

impl<'db> TypeRenderer<'db> {
    pub fn render(&mut self, db: &'db dyn Db, ty: Ty<'db>) -> String {
        match ty.kind(db) {
            TyKind::Any => "Any".to_owned(),
            TyKind::Unknown => "Unknown".to_owned(),
            TyKind::Null => "NULL".to_owned(),
            TyKind::Scalar(atomic) => atomic_name(*atomic).to_owned(),
            TyKind::Vector(element) => format!("{}[]", self.render(db, *element)),
            TyKind::NamedVector(element) => format!("{}[named]", self.render(db, *element)),
            TyKind::List(element) => format!("list[{}]", self.render(db, *element)),
            TyKind::NamedList(element) => format!("list[named: {}]", self.render(db, *element)),
            TyKind::Tuple(items) => {
                let items: Vec<String> = items.iter().map(|&item| self.render(db, item)).collect();
                format!("list{{{}}}", items.join(", "))
            }
            TyKind::Record(fields) => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|field| {
                        format!(
                            "{}: {}",
                            spell_field_name(field.name.text(db)),
                            self.render(db, field.ty)
                        )
                    })
                    .collect();
                format!("list{{{}}}", fields.join(", "))
            }
            TyKind::Function(function) => self.render_function(db, function),
            TyKind::Union(members) => {
                let members: Vec<String> = members
                    .iter()
                    .map(|&member| {
                        // `->` extends over a whole union, so a function member
                        // needs its grouping parentheses back or the rendered
                        // type is a different type: without them
                        // `(fn(A) -> B) | NULL` and `fn(A) -> (B | NULL)` print
                        // the same, and copying one out of a finding into an
                        // annotation silently changes it.
                        let rendered = self.render(db, member);
                        match member.kind(db) {
                            TyKind::Function(_) => format!("({rendered})"),
                            _ => rendered,
                        }
                    })
                    .collect();
                members.join(" | ")
            }
            TyKind::Named(name, arguments) => {
                if arguments.is_empty() {
                    name.text(db).to_owned()
                } else {
                    let arguments: Vec<String> = arguments
                        .iter()
                        .map(|&argument| self.render(db, argument))
                        .collect();
                    format!("{}<{}>", name.text(db), arguments.join(", "))
                }
            }
            TyKind::Var(var) => self.variable_name(RenderedVar::Inference(*var)),
            TyKind::Rigid(name) => self.variable_name(RenderedVar::Rigid(*name)),
        }
    }

    pub fn render_scheme(&mut self, db: &'db dyn Db, scheme: &TypeScheme<'db>) -> String {
        match self.render_binder_prefix(&scheme.binders) {
            Some(prefix) => format!("{prefix} {}", self.render(db, scheme.body)),
            None => self.render(db, scheme.body),
        }
    }

    /// The `<T, U: numeric>` binder prefix of a scheme, registering each
    /// binder's display name so the body rendered afterwards through the same
    /// renderer reuses them. `None` for an empty binder list.
    pub fn render_binder_prefix(&mut self, binders: &[(Name<'db>, Constraint)]) -> Option<String> {
        if binders.is_empty() {
            return None;
        }
        let binders: Vec<String> = binders
            .iter()
            .map(|(name, constraint)| {
                let rendered = self.variable_name(RenderedVar::Rigid(*name));
                match constraint.spelling() {
                    Some(spelling) => format!("{rendered}: {spelling}"),
                    None => rendered,
                }
            })
            .collect();
        Some(format!("<{}>", binders.join(", ")))
    }

    fn render_function(&mut self, db: &'db dyn Db, function: &FunctionType<'db>) -> String {
        let mut parameters = Vec::new();
        for ty in &function.positional {
            parameters.push(self.render(db, *ty));
        }
        for (index, field) in function.named.iter().enumerate() {
            // The rest parameter sits at its declared boundary among the
            // named parameters rather than always last. The position is part
            // of the shape, because R matches the remaining positional
            // arguments into it.
            if let Some(rest) = &function.variadic
                && rest.preceding_named == index
            {
                parameters.push(format!("...: {}", self.render(db, rest.element)));
            }
            let name = if field.optional {
                format!("[{}]", field.name.text(db))
            } else {
                field.name.text(db).to_owned()
            };
            parameters.push(format!("{name}: {}", self.render(db, field.ty)));
        }
        if let Some(rest) = &function.variadic
            && rest.preceding_named >= function.named.len()
        {
            parameters.push(format!("...: {}", self.render(db, rest.element)));
        }
        let ret = self.render(db, function.ret);
        format!("fn({}) -> {}", parameters.join(", "), ret)
    }

    fn variable_name(&mut self, var: RenderedVar<'db>) -> String {
        let index = match self.names.iter().position(|existing| *existing == var) {
            Some(index) => index,
            None => {
                self.names.push(var);
                self.names.len() - 1
            }
        };
        let letter = (b'T' + (index as u8 % 7)) as char;
        let suffix = index / 7;
        if suffix == 0 {
            letter.to_string()
        } else {
            format!("{letter}{suffix}")
        }
    }
}

/// What a constraint admits, in the same words wherever it is reported.
fn constraint_description(constraint: Constraint) -> &'static str {
    match constraint {
        Constraint::Unconstrained => "a value",
        Constraint::Numeric => "a numeric value (`integer` or `double`)",
        Constraint::AtomicElement => {
            "an atomic value (`logical`, `integer`, `double`, `complex`, `character`, or `raw`)"
        }
        Constraint::ScalarNumeric => "a scalar numeric value (`integer` or `double`)",
    }
}

fn atomic_name(atomic: Atomic) -> &'static str {
    match atomic {
        Atomic::Logical => "logical",
        Atomic::Integer => "integer",
        Atomic::Double => "double",
        Atomic::Complex => "complex",
        Atomic::Character => "character",
        Atomic::Raw => "raw",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};

    fn render_all(db: &RootDatabase, file: SourceFile) -> Vec<String> {
        file_diagnostics(db, file)
            .into_iter()
            .map(|d| {
                format!(
                    "{}..{} {}[{}] {}",
                    u32::from(d.range.start()),
                    u32::from(d.range.end()),
                    match d.severity {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                    },
                    d.code,
                    d.message
                )
            })
            .collect()
    }

    #[test]
    fn file_diagnostics_end_to_end() {
        let db = RootDatabase::default();
        let util = SourceFile::new(
            &db,
            "add <- function(x, y) x + y\n".to_owned(),
            DocumentKind::Package,
        );
        let main = SourceFile::new(
            &db,
            "bad <- function() add(\"a\", 2L)\nmissing_fn <- function() nowhere()\n".to_owned(),
            DocumentKind::Package,
        );
        ProjectFiles::new(&db, vec![util, main]);
        let rendered = render_all(&db, main);
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("type-mismatch") && line.contains("character")),
            "expected a mismatch mentioning character: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("unresolved") && line.contains("nowhere")),
            "expected an unresolved warning for nowhere: {rendered:?}"
        );
        // The second item's finding is offset into the file (absolute range).
        let missing = rendered
            .iter()
            .find(|line| line.contains("nowhere"))
            .expect("nowhere finding");
        let start: u32 = missing.split("..").next().unwrap().parse().unwrap();
        assert!(start > 30, "range must be file-absolute, got {missing}");
    }

    #[test]
    fn unused_and_syntax_diagnostics_render() {
        let db = RootDatabase::default();
        let file = SourceFile::new(
            &db,
            "f <- function() {\n  dead <- 1\n  dead <- 2\n  dead\n}\ng <- function( {\n".to_owned(),
            DocumentKind::Script,
        );
        let rendered = render_all(&db, file);
        assert!(
            rendered.iter().any(|line| line.contains("unused")),
            "expected a dead-store warning: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("syntax-error")),
            "expected a syntax error: {rendered:?}"
        );
    }

    #[test]
    fn nesting_caps_split_by_depth() {
        let deep = |levels: usize| {
            format!(
                "#: {}integer{}\nvalue <- 1L\n",
                "list[".repeat(levels),
                "]".repeat(levels)
            )
        };
        let db = RootDatabase::default();
        // Past the check cap but under the parse cap: a typing-class refusal,
        // and the payload drop keeps the mismatch from also firing.
        let checked = SourceFile::new(&db, deep(140), DocumentKind::Package);
        ProjectFiles::new(&db, vec![checked]);
        let rendered = render_all(&db, checked);
        assert_eq!(rendered.len(), 1, "one finding only: {rendered:?}");
        assert!(
            rendered[0].contains("type-mismatch") && rendered[0].contains("more than 128"),
            "expected the check-depth refusal: {rendered:?}"
        );

        let db = RootDatabase::default();
        let refused = SourceFile::new(&db, deep(200), DocumentKind::Package);
        ProjectFiles::new(&db, vec![refused]);
        let rendered = render_all(&db, refused);
        assert_eq!(rendered.len(), 1, "one finding only: {rendered:?}");
        assert!(
            rendered[0].contains("annotation") && rendered[0].contains("more than 160"),
            "expected the shape refusal: {rendered:?}"
        );
    }

    #[test]
    fn renderer_shares_names_across_both_sides() {
        let db = RootDatabase::default();
        let mut renderer = TypeRenderer::default();
        let t = crate::types::Ty::new(
            &db,
            crate::types::TyKind::Rigid(crate::types::Name::new(&db, "A".to_owned())),
        );
        let u = crate::types::Ty::new(
            &db,
            crate::types::TyKind::Rigid(crate::types::Name::new(&db, "B".to_owned())),
        );
        // First occurrence order: A -> T, B -> U; A again stays T.
        assert_eq!(renderer.render(&db, t), "T");
        assert_eq!(renderer.render(&db, u), "U");
        assert_eq!(renderer.render(&db, t), "T");
    }
}
