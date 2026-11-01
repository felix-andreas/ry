use crate::annotations::{
    Annotation, NamedDefinition, Slot, comment_slots, inline_slots, is_green_inline_site,
    is_type_kind, lower_annotation, lower_inline_annotation, statement_inline_site,
};
use crate::check::{
    ItemCheck, NativeValueKind, OperandExpectation, StrictOriginKind, TypeError, TypeErrorKind,
    column_element,
};
use crate::exhaustiveness::{MatchFinding, MatchFindingKind};
use crate::hir::{ExprId, ExpressionKind, Symbol};
use crate::infer::apply_definition;
use crate::infer::{FunctionMismatch, RecordMismatch};
use crate::lints::LintConfig;
use crate::metadata::{
    declared_dependency, imported_by_name, imports_every_name, is_own_package, native_symbol,
};
use crate::naming::{BindingKind, ItemNaming, NamespaceRead};
use crate::stubs::{namespace_exports, namespace_known, stubs};
use crate::types::{
    Atomic, Constraint, FunctionType, InferenceVar, Name, NativeField, RecordField, Ty, TyKind,
    TypeScheme, map_child_types, substitute_rigid, union_of,
};
use crate::{
    AnnotationTarget, Db, DocumentKind, ItemKind, ProjectFiles, SourceFile, file_type_definitions,
    inline_annotations_enabled, item_check, item_hir, item_name_range, item_naming, item_spans,
    item_tree, parse, project_type_definitions, statement_annotations,
};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::mem;
use syntax::SyntaxNode;
use syntax::ast::is_expression_kind;
use syntax::{
    GreenRef, SyntaxKind, TextRange, TextSize, is_green_inline_annotation, is_syntactic_name,
    matching_nodes, paired_comment_definition,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, salsa::SalsaValue)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct Diagnostic {
    pub range: TextRange,
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub related: Vec<RelatedLocation>,
}

impl Diagnostic {
    pub fn new(range: TextRange, severity: Severity, code: &'static str, message: String) -> Self {
        Diagnostic {
            range,
            severity,
            code,
            message,
            related: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct RelatedLocation {
    pub file: SourceFile,
    pub range: TextRange,
    pub message: &'static str,
}

/// Derives every fact a later pass reads from the file's tree, while the tree is resident.
/// A cold pass that evicts trees between batches calls this first, so checking a file never
/// re-parses another one: past this point only the file's own diagnostics read its tree.
pub fn derive_from_syntax(db: &dyn Db, file: SourceFile, lints: &LintConfig) {
    let _ = crate::metadata::file_attached_namespaces(db, file);
    let _ = file_error_ranges(db, file);
    let _ = file_annotation_references(db, file);
    let _ = agreement_candidates(db, file);
    let _ = definition_agreement_candidates(db, file);
    let _ = crate::lints::file_syntax_lints(db, file, *lints);
    let _ = crate::file_typing_mode(db, file);
    let _ = crate::file_type_definitions(db, file);
    let _ = type_declaration_names(db, file);
    let _ = parse_stage_diagnostics(db, file);
    for &item in item_tree(db, file) {
        let _ = crate::item_hir(db, item);
        let _ = crate::item_annotations(db, item);
        let _ = item_name_range(db, item);
    }
}

/// Pure functions of the parse, served by a host's fast publication wave.
/// [`file_diagnostics`] starts from the same set, so that wave stays a subset.
#[salsa::tracked(returns(clone))]
pub fn parse_stage_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let parsed = parse(db, file);
    for error in parsed.errors() {
        diagnostics.push(Diagnostic::new(
            error.range,
            Severity::Error,
            // The code names whose grammar broke (R or `#:`), not which stage noticed.
            if error.in_annotation {
                "annotation"
            } else {
                "syntax-error"
            },
            error.message.clone(),
        ));
    }

    for (range, value) in crate::file_typing_directives(db, file).1 {
        diagnostics.push(Diagnostic::new(range, Severity::Error, "annotation", format!(
                "Unknown typing directive `{value}`. Use `# typing: on`, `# typing: off`, or `# typing: strict`."
            )));
    }

    let root = parsed.syntax_node();
    for node in matching_nodes(&root, |node, _| node.kind() == SyntaxKind::ANNOTATION) {
        let annotation = lower_annotation(db, &node);
        for (message, range) in &annotation.errors {
            diagnostics.push(Diagnostic::new(
                *range,
                Severity::Error,
                "annotation",
                message.clone(),
            ));
        }
        if !annotation.definitions.is_empty() && node.parent().as_ref() != Some(&root) {
            diagnostics.push(Diagnostic::new(
                node.text_range(),
                Severity::Error,
                "annotation",
                "Type definition blocks are only allowed at the top level of a file.".to_owned(),
            ));
        }
    }
    for node in matching_nodes(&root, |node, _| node.kind() == SyntaxKind::TYPE_DEFINITION)
        .into_iter()
        .filter(|node| node.parent().as_ref() != Some(&root))
    {
        diagnostics.push(Diagnostic::new(
            node.text_range(),
            Severity::Error,
            "annotation",
            "Type definitions are only allowed at the top level of a file.".to_owned(),
        ));
    }
    diagnostics.extend(dangling_annotation_diagnostics(db, file));
    diagnostics.extend(inline_annotation_diagnostics(db, file));
    diagnostics.extend(ry_syntax_diagnostics(db, file));
    diagnostics
}

/// Records, tuples, tags and `match` have no meaning in R, so a project that turned
/// inline annotations off to stay runnable by R hears about each one. They are still
/// typed, but `match` coverage is not counted.
fn ry_syntax_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let parsed = parse(db, file);
    let sites = matching_nodes(&parsed.syntax_node(), |node, _| {
        matches!(
            node.kind(),
            SyntaxKind::RECORD_EXPR
                | SyntaxKind::TUPLE_EXPR
                | SyntaxKind::TAG_EXPR
                | SyntaxKind::MATCH_EXPR
        )
    });
    if sites.is_empty() || inline_annotations_enabled(db) {
        return Vec::new();
    }
    sites
        .iter()
        .map(|node| {
            let (what, range) = match node.kind() {
                SyntaxKind::RECORD_EXPR => ("record", node.text_range()),
                SyntaxKind::TUPLE_EXPR => ("tuple", node.text_range()),
                SyntaxKind::TAG_EXPR => ("tag", node.text_range()),
                // The arms can be long, so the head `match (x)` stands for the expression.
                _ => {
                    let head_end = node
                        .children()
                        .find(|child| is_expression_kind(child.kind()))
                        .map_or(node.text_range().end(), |scrutinee| scrutinee.text_range().end());
                    ("`match`", TextRange::new(node.text_range().start(), head_end))
                }
            };
            Diagnostic::new(
                range,
                Severity::Warning,
                "ry-syntax",
                format!(
                    "this {what} is ry syntax that R cannot run, and inline annotations are turned off (`[check] inline-annotations = false`) to keep code runnable by R. Rewrite it in plain R, or turn inline annotations back on"
                ),
            )
        })
        .collect()
}

/// An inline annotation's lowering errors, like a `#:` block's. Turned off, each inline
/// annotation instead warns that it types nothing, because the code still cannot run
/// under R and the author should know the annotation is not checked.
fn inline_annotation_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let parsed = parse(db, file);
    let sites = matching_nodes(&parsed.syntax_node(), |node, _| is_green_inline_site(node));
    if sites.is_empty() {
        return Vec::new();
    }
    let mut diagnostics = Vec::new();
    if !inline_annotations_enabled(db) {
        for node in matching_nodes(&parsed.syntax_node(), |node, parent| {
            is_green_inline_annotation(node.kind(), parent.map(GreenRef::kind))
        }) {
            diagnostics.push(Diagnostic::new(
                node.text_range(),
                Severity::Warning,
                "inline-annotation",
                "inline type annotations are turned off (`[check] inline-annotations = false`), so this one is not checked. R itself cannot run this code: delete the annotation, or turn inline annotations back on".to_owned(),
            ));
        }
        return diagnostics;
    }
    for site in &sites {
        for (message, range) in lower_inline_annotation(db, site).errors {
            diagnostics.push(Diagnostic::new(
                range,
                Severity::Error,
                "annotation",
                message,
            ));
        }
    }
    diagnostics
}

/// An annotation must be followed on the next line by the expression it types.
/// Blocks inside argument lists attach to nothing and are reported separately;
/// braceless bodies, braceless `if` branches, and parenthesized expressions do attach.
fn dangling_annotation_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let parsed = parse(db, file);
    let root = parsed.syntax_node();
    let mut diagnostics = Vec::new();
    // Only a sequence with an annotation among its statements can hold a dangling one.
    let sequences = matching_nodes(&root, |node, parent| {
        (parent.is_none() || node.kind() == SyntaxKind::BRACE_EXPR)
            && node.has_child(SyntaxKind::ANNOTATION)
    });
    for (node, target) in sequences
        .iter()
        .flat_map(|parent| crate::statement_annotations(&parent))
    {
        let annotation = lower_annotation(db, &node);
        if !annotation.definitions.is_empty()
            || annotation.strict.is_some()
            || !annotation.errors.is_empty()
            || !annotation.typing_errors.is_empty()
        {
            continue;
        }
        let message = match target {
            AnnotationTarget::Attached(_) => {
                if node.children().next().is_none() {
                    "A `#:` typing comment must include a type expression."
                } else {
                    continue;
                }
            }
            AnnotationTarget::BlankLineSeparated => {
                "A `#:` typing comment cannot be separated from its expression by an empty line."
            }
            AnnotationTarget::Dangling => {
                // Almost always a `#:` block placed above a roxygen2 block, so name the fix.
                "A `#:` typing comment must be followed immediately by an expression. Put it directly above the definition, below any roxygen2 block."
            }
        };
        diagnostics.push(Diagnostic::new(
            node.text_range(),
            Severity::Error,
            "annotation",
            message.to_owned(),
        ));
    }
    for node in matching_nodes(&root, |node, _| node.kind() == SyntaxKind::ANNOTATION) {
        if node.parent().map(|parent| parent.kind()) != Some(SyntaxKind::ARGUMENT_LIST) {
            continue;
        }
        // A block that failed to lower already owns a diagnostic.
        let annotation = lower_annotation(db, &node);
        if !annotation.errors.is_empty() || !annotation.typing_errors.is_empty() {
            continue;
        }
        diagnostics.push(Diagnostic::new(
            node.text_range(),
            Severity::Error,
            "annotation",
            // Not "move it up a line": that would type the enclosing statement instead.
            "A `#:` typing comment annotates the statement that follows it, and an argument is not a statement, so this one annotates nothing. Give the value its own binding and annotate that.".to_owned(),
        ));
    }
    diagnostics
}

/// Name → index of the earliest item whose top-level frame writes it. Conditional
/// writes count: they create the document's slot just like unconditional ones.
#[salsa::tracked(returns(ref))]
fn frame_slot_positions(db: &dyn Db, file: SourceFile) -> FxHashMap<Symbol, usize> {
    let mut positions = FxHashMap::default();
    for (index, &item) in item_tree(db, file).iter().enumerate() {
        let Some(naming) = item_naming(db, item) else {
            continue;
        };
        for binding in naming.bindings.values() {
            if binding.kind != BindingKind::TopLevel {
                continue;
            }
            positions.entry(binding.name).or_insert(index);
        }
    }
    positions
}

/// The ranges of the file's R syntax errors. Annotation-grammar errors leave their
/// statement intact, so they are not here.
#[salsa::tracked(returns(ref))]
fn file_error_ranges(db: &dyn Db, file: SourceFile) -> Vec<TextRange> {
    parse(db, file)
        .errors()
        .iter()
        .filter(|error| !error.in_annotation)
        .map(|error| error.range)
        .collect()
}

#[salsa::tracked(returns(clone))]
pub fn file_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let mut diagnostics = parse_stage_diagnostics(db, file);

    // A statement with an R syntax error reports nothing else: its names and types
    // are suspect. Annotation-grammar errors leave the statement intact.
    let broken_ranges = file_error_ranges(db, file);
    let is_broken = |range: TextRange| {
        broken_ranges
            .iter()
            .any(|error| error.start() <= range.end() && range.start() <= error.end())
    };

    for (item_index, span) in item_spans(db, file).iter().enumerate() {
        if is_broken(span.range) {
            continue;
        }
        let item = span.item;
        let offset = span.range.start();
        let Some(check) = item_check(db, item) else {
            continue;
        };
        for error in &check.errors {
            diagnostics.push(Diagnostic::new(
                error.range + offset,
                Severity::Error,
                "type-mismatch",
                render_type_error_message(db, error),
            ));
        }
        // Without inline annotations, records, tags and `match` are reported as `ry-syntax`
        // and their coverage is not counted.
        if !check.match_findings.is_empty() && inline_annotations_enabled(db) {
            for finding in &check.match_findings {
                diagnostics.push(match_finding_diagnostic(finding, offset));
            }
        }
        let Some(module) = item_hir(db, item) else {
            continue;
        };
        let Some(naming) = item_naming(db, item) else {
            continue;
        };
        for unused in &naming.unused_assignments {
            let range = unused.range + offset;
            diagnostics.push(Diagnostic::new(
                range,
                Severity::Warning,
                "unused",
                format!("`{}` is assigned but never used.", unused.name),
            ));
        }
        for target in &naming.invalid_assignment_targets {
            let range = target.range + offset;
            // A trailing operator pulled the next line in only if the break falls after
            // the operator; a break before it is an ordinary continuation.
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
            diagnostics.push(Diagnostic::new(
                range,
                Severity::Error,
                "syntax-error",
                format!("this is a value, not a name, so nothing can be assigned to it{hint}"),
            ));
        }
        // The arm body reads every name the pattern binds, so an alternative that leaves one
        // out would run the body with that name unset or holding a stale value.
        for mismatch in &naming.or_pattern_mismatches {
            diagnostics.push(Diagnostic::new(
                mismatch.range + offset,
                Severity::Error,
                "syntax-error",
                format!(
                    "this alternative does not bind `{}`, but another alternative of the `|` pattern does. Every alternative must bind the same names",
                    mismatch.name
                ),
            ));
        }
        // R errors on a read with no prior write. Local slots only: a top-level
        // variable's unwritten path falls through to the enclosing environment.
        for expression in &naming.maybe_undefined {
            let Some(binding) = naming
                .resolutions
                .get(expression)
                .and_then(|slot| naming.bindings.get(slot))
            else {
                continue;
            };
            if binding.kind == BindingKind::TopLevel {
                continue;
            }
            let range = module.expression(*expression).range + offset;
            diagnostics.push(Diagnostic::new(
                range,
                Severity::Warning,
                "maybe-undefined",
                format!(
                    "`{}` is only assigned on some paths here, so this read can find nothing.",
                    display_name(&binding.name)
                ),
            ));
        }
        for (expression, read) in &naming.namespace_reads {
            let Some(message) = namespace_read_message(db, read) else {
                continue;
            };
            let range = module.expression(*expression).range + offset;
            diagnostics.push(Diagnostic::new(
                range,
                Severity::Warning,
                "unresolved",
                message,
            ));
        }
        // Immediate reads see only slots created by earlier statements; reads inside a
        // nested function run after the frame settled, so any slot in the document
        // resolves them (including self-recursion).
        for (expression, name) in &naming.non_locals {
            if check.column_misses.contains_key(expression) {
                continue;
            }
            let NonLocalRead::Unresolved { suggestion } =
                classify_non_local_read(db, file, item_index, &check, naming, expression, name)
            else {
                continue;
            };
            let range = module.expression(*expression).range + offset;
            let display = display_name(name);
            let suggestion = suggestion
                .map(|nearest| format!(" Did you mean `{nearest}`?"))
                .unwrap_or_default();
            diagnostics.push(Diagnostic::new(range, Severity::Warning, "unresolved", format!(
                    "I could not resolve `{display}` in this package, its imports, or builtins.{suggestion}"
                )));
        }
        // A name inside a frame query is a column first, and R falls back to the enclosing
        // scope only for a non-column, so a miss is reported only when nothing defines it.
        for (expression, miss) in &check.column_misses {
            let Some(name) = naming
                .non_locals
                .get(expression)
                .or_else(|| naming.quiet_reads.get(expression))
            else {
                continue;
            };
            let NonLocalRead::Unresolved { suggestion } =
                classify_non_local_read(db, file, item_index, &check, naming, expression, name)
            else {
                continue;
            };
            let suggestion = nearest_name(name, miss.columns.iter().map(String::as_str))
                .or(suggestion)
                .map(|nearest| format!(" Did you mean `{nearest}`?"))
                .unwrap_or_default();
            let subject = match &miss.frame {
                Some(frame) => format!("the frame `{frame}`"),
                None => "this frame".to_owned(),
            };
            let range = module.expression(*expression).range + offset;
            diagnostics.push(Diagnostic::new(
                range,
                Severity::Warning,
                "unresolved",
                format!(
                    "{subject} has no column `{}`, and nothing else defines that name.{suggestion}",
                    display_name(name)
                ),
            ));
        }
    }

    if *file.kind(db) == DocumentKind::Script {
        diagnostics.extend(script_unused_bindings(db, file));
    }
    diagnostics.extend(duplicate_binding_diagnostics(db, file));
    diagnostics.extend(duplicate_type_diagnostics(db, file));
    diagnostics.extend(unknown_type_diagnostics(db, file));
    diagnostics.extend(annotation_rule_diagnostics(db, file));
    diagnostics.extend(annotation_agreement_diagnostics(db, file));

    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    diagnostics
}

/// Shared by the ordinary check and strict mode so they cannot disagree.
/// `Tolerated`: undefined, but silenced by an attached package whose exports
/// cannot be known; strict mode reports exactly these.
enum NonLocalRead {
    Resolvable,
    Tolerated,
    Unresolved { suggestion: Option<String> },
}

/// Guards run cheapest first: most non-local reads resolve through frame slots.
fn classify_non_local_read(
    db: &dyn Db,
    file: SourceFile,
    item_index: usize,
    check: &ItemCheck<'_>,
    naming: &ItemNaming,
    expression: &ExprId,
    name: &str,
) -> NonLocalRead {
    if check.masked_reads.contains(expression) {
        return NonLocalRead::Resolvable;
    }
    if let Some(&earliest) = frame_slot_positions(db, file).get(name)
        && (earliest < item_index
            || naming.deferred_non_locals.contains(expression)
            || naming.deferred_quiet_reads.contains(expression))
    {
        return NonLocalRead::Resolvable;
    }
    if R6_INJECTED_BINDINGS.contains(&name) && *file_defines_r6_class(db, file) {
        return NonLocalRead::Resolvable;
    }
    if DISPATCH_VARIABLES.contains(&name) && naming.deferred_non_locals.contains(expression) {
        return NonLocalRead::Resolvable;
    }
    if native_symbol(db, name, naming.native_routine_reads.contains(expression)) {
        return NonLocalRead::Resolvable;
    }
    let files = ProjectFiles::try_get(db);
    let test_files = files.filter(|_| {
        matches!(
            *file.kind(db),
            DocumentKind::Test | DocumentKind::TestHelper
        )
    });
    if crate::package_scheme_exists(db, name)
        || test_files.is_some_and(|files| {
            let binding = crate::test_name(db, files, Name::new(db, name));
            binding.winner.is_some() || !binding.conditional_writers.is_empty()
        })
        || super_globals(db, file).contains(name)
        || files.is_some_and(|files| project_global_variable_declarations(db, files).contains(name))
        || imported_by_name(db, name)
    {
        return NonLocalRead::Resolvable;
    }
    // A near-miss of a name the project itself binds (including the item's locals
    // and parameters) is a typo, reported even under the blanket tolerance. It is
    // checked before the stub corpus because it is the likelier intent.
    let project_typo = files
        .and_then(|files| {
            project_typo_suggestion(db, files, Name::new(db, name), test_files.is_some())
        })
        .or_else(|| {
            nearest_name(
                name,
                naming
                    .bindings
                    .values()
                    .map(|binding| binding.name.as_str()),
            )
        });
    if project_typo.is_none() && imports_every_name(db) {
        return NonLocalRead::Tolerated;
    }
    NonLocalRead::Unresolved {
        suggestion: project_typo.or_else(|| typo_suggestion(db, Name::new(db, name))),
    }
}

/// Without this, a misspelled type name would silently become an opaque nominal.
fn unknown_type_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let references: Vec<&(String, TextRange)> = file_annotation_references(db, file)
        .iter()
        .flat_map(|annotation| &annotation.nominal_references)
        .collect();
    if references.is_empty() {
        return Vec::new();
    }

    let mut known: BTreeSet<String> = stubs(db)
        .map(|library| library.nominals.iter().cloned().collect())
        .unwrap_or_default();
    if let Some(files) = ProjectFiles::try_get(db) {
        for name in project_type_definitions(db, files).keys() {
            known.insert(name.text(db).to_owned());
        }
    }
    // A script's own declarations shadow the project namespace.
    for definition in file_type_definitions(db, file) {
        known.insert(definition.name.text(db).to_owned());
    }

    references
        .into_iter()
        .filter(|(name, _)| !known.contains(name))
        .map(|&(ref name, range)| {
            let suggestion = nearest_name(
                name,
                known
                    .iter()
                    .map(String::as_str)
                    .chain(ANNOTATION_PRIMITIVES.iter().copied()),
            )
            .map(|nearest| format!(" Did you mean `{nearest}`?"))
            .unwrap_or_default();
            Diagnostic::new(range, Severity::Error, "annotation", format!(
                    "I do not know the type `{name}`. It is not a built-in type, a declared `@type` or `@alias` name, or a class from the standard-library stubs.{suggestion}"
                ))
        })
        .collect()
}

/// The depth and vector findings use the typing code so they vanish when typing is off.
fn annotation_rule_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let definitions = visible_type_definitions(db, file);
    // Names declared more than once have no winning definition; the
    // duplicate-declaration error reports them instead.
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    if let Some(files) = ProjectFiles::try_get(db) {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
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
    let stub_nominals: BTreeSet<String> = stubs(db)
        .map(|library| library.nominals.iter().cloned().collect())
        .unwrap_or_default();

    for annotation in file_annotation_references(db, file) {
        for (message, range) in &annotation.typing_errors {
            diagnostics.push(Diagnostic::new(
                *range,
                Severity::Error,
                "type-mismatch",
                message.clone(),
            ));
        }
        for (element, range) in &annotation.vector_elements {
            if vector_element_atomic(db, *element, &definitions, &stub_nominals) {
                continue;
            }
            let mut renderer = TypeRenderer::default();
            let rendered = renderer.render(db, *element);
            diagnostics.push(Diagnostic::new(*range, Severity::Error, "type-mismatch", format!(
                    "the element of a `[]` vector type must be an atomic type, found `{rendered}`. For a list of these, write `list[{rendered}]`"
                )));
        }
        // A bare reference to a generic must be applied, except under `@new`, which
        // infers its arguments through the representation check.
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
                diagnostics.push(Diagnostic::new(
                    *range,
                    Severity::Error,
                    "annotation",
                    format!("`{name}` is not a generic type. It takes no type arguments."),
                ));
            } else if arity != *count {
                diagnostics.push(Diagnostic::new(
                    *range,
                    Severity::Error,
                    "annotation",
                    format!(
                        "generic type `{name}` expects {arity} type {}, but found {count}.",
                        plural(arity, "argument", "arguments"),
                    ),
                ));
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
            if let Some((_, new_range)) = &annotation.new_nominal
                && new_range.contains_range(*range)
            {
                continue;
            }
            let Some(arity) = declared_arity(name) else {
                continue;
            };
            if arity > 0 {
                diagnostics.push(Diagnostic::new(
                    *range,
                    Severity::Error,
                    "annotation",
                    format!(
                        "generic type `{name}` expects {arity} type {}, but found 0.",
                        plural(arity, "argument", "arguments"),
                    ),
                ));
            }
        }
        if let Some((name, range)) = &annotation.new_nominal
            && !ambiguous.contains(name.text(db))
            && definitions
                .get(name.text(db))
                .is_some_and(|definition| definition.alias)
        {
            diagnostics.push(Diagnostic::new(
                *range,
                Severity::Error,
                "annotation",
                format!(
                    "`@new` requires a nominal type declared with `@type`, but `{}` is an alias.",
                    name.text(db)
                ),
            ));
        }
    }
    diagnostics
}

/// What the vocabulary checks need from one annotation of the file.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct AnnotationReferences<'db> {
    typing_errors: Vec<(String, TextRange)>,
    // Sound: `TextRange` is plain offsets, so only the `'db` handles need retaining.
    #[salsa_value(unsafe(prove_safe_to_retain_manually))]
    vector_elements: Vec<(Ty<'db>, TextRange)>,
    applied_references: Vec<(String, usize, TextRange)>,
    nominal_references: Vec<(String, TextRange)>,
    #[salsa_value(unsafe(prove_safe_to_retain_manually))]
    new_nominal: Option<(Name<'db>, TextRange)>,
}

/// Every annotation in the file lowered with file ranges (the `#:` blocks, and the inline
/// sites unless inline annotations are turned off), keeping only those that reference a
/// type or report a typing error. Memoized because it reads the tree, which a cold check
/// evicts after its first pass.
#[salsa::tracked(returns(ref))]
fn file_annotation_references<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<AnnotationReferences<'db>> {
    let root = parse(db, file).syntax_node();
    let mut annotations: Vec<Annotation<'db>> =
        matching_nodes(&root, |node, _| node.kind() == SyntaxKind::ANNOTATION)
            .iter()
            .map(|node| lower_annotation(db, node))
            .collect();
    let sites = matching_nodes(&root, |node, _| is_green_inline_site(node));
    if !sites.is_empty() && inline_annotations_enabled(db) {
        annotations.extend(sites.iter().map(|site| lower_inline_annotation(db, site)));
    }
    annotations
        .into_iter()
        .map(|annotation| AnnotationReferences {
            typing_errors: annotation.typing_errors,
            vector_elements: annotation.vector_elements,
            applied_references: annotation.applied_references,
            nominal_references: annotation.nominal_references,
            new_nominal: annotation.new_nominal.map(|(name, _, range)| (name, range)),
        })
        .filter(|references| {
            !references.typing_errors.is_empty()
                || !references.vector_elements.is_empty()
                || !references.applied_references.is_empty()
                || !references.nominal_references.is_empty()
                || references.new_nominal.is_some()
        })
        .collect()
}

/// The stubs' and the project's declarations, with a script's own declarations shadowing them.
fn visible_type_definitions<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> BTreeMap<String, NamedDefinition<'db>> {
    let mut definitions = BTreeMap::new();
    if let Some(library) = stubs(db) {
        for (name, definition) in &library.type_definitions {
            definitions.insert(name.text(db).to_owned(), definition.clone());
        }
    }
    if let Some(files) = ProjectFiles::try_get(db) {
        for (name, definition) in project_type_definitions(db, files) {
            definitions.insert(name.text(db).to_owned(), definition.clone());
        }
    }
    for definition in file_type_definitions(db, file) {
        definitions.insert(definition.name.text(db).to_owned(), definition);
    }
    definitions
}

/// A statement annotated both ways must say the same thing both ways. Slots are compared
/// one at a time (the value, each parameter, the rest parameter, the return), as types:
/// an alias and its expansion agree, and so do two orders of one union.
fn annotation_agreement_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let statements = agreement_candidates(db, file);
    let definitions_both_ways = definition_agreement_candidates(db, file);
    if statements.is_empty() && definitions_both_ways.is_empty() {
        return Vec::new();
    }
    let definitions = visible_type_definitions(db, file);
    let mut diagnostics = Vec::new();
    for candidate in statements.iter().chain(definitions_both_ways) {
        let pair = match candidate {
            AgreementCandidate::Finding(diagnostic) => {
                diagnostics.push(diagnostic.clone());
                continue;
            }
            AgreementCandidate::Definition(pair) => {
                if !same_type(
                    db,
                    pair.inline_compared,
                    pair.comment_compared,
                    &definitions,
                ) {
                    diagnostics.push(Diagnostic {
                        range: pair.range,
                        severity: Severity::Warning,
                        code: "annotation-mismatch",
                        message: format!(
                            "the inline definition of `{}` says `{}`, but the `#:` comment says `{}`. ry uses the inline definition; make the two agree, or delete one",
                            pair.name.text(db),
                            TypeRenderer::default().render(db, pair.inline),
                            TypeRenderer::default().render(db, pair.comment)
                        ),
                        related: vec![RelatedLocation {
                            file,
                            range: pair.comment_range,
                            message: "the `#:` comment's type is here",
                        }],
                    });
                }
                continue;
            }
            AgreementCandidate::Slot(pair) => pair,
        };
        if same_type(
            db,
            pair.inline_compared,
            pair.comment_compared,
            &definitions,
        ) {
            continue;
        }
        // Each side renders on its own, so its type parameters keep their written names.
        let inline_type = TypeRenderer::default().render(db, pair.inline);
        let comment_type = TypeRenderer::default().render(db, pair.comment);
        let disagreement = match &pair.slot {
            Slot::Value => format!(
                "this binding's inline annotation says `{inline_type}`, but its `#:` comment says `{comment_type}`"
            ),
            Slot::Parameter(name) => format!(
                "the inline annotation gives parameter `{}` the type `{inline_type}`, but the `#:` comment gives it `{comment_type}`",
                display_name(name)
            ),
            Slot::Rest => format!(
                "the inline annotation gives `...` the type `{inline_type}`, but the `#:` comment gives it `{comment_type}`"
            ),
            Slot::Return => format!(
                "the inline annotation says this function returns `{inline_type}`, but the `#:` comment says it returns `{comment_type}`"
            ),
        };
        let winner = match pair.governing {
            Some((side, directive)) => format!("ry applies the {side} `{directive}`"),
            None => "ry uses the inline type".to_owned(),
        };
        diagnostics.push(Diagnostic {
            range: pair.range,
            severity: Severity::Warning,
            code: "annotation-mismatch",
            message: format!("{disagreement}. {winner}; make the two agree, or delete one"),
            related: vec![RelatedLocation {
                file,
                range: pair.comment_range,
                message: "the `#:` comment's type is here",
            }],
        });
    }
    diagnostics
}

/// A disagreement between a statement's inline annotation and its `#:` comment, found from
/// syntax alone. Memoized because it reads the tree, which a cold check evicts after its
/// first pass.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
enum AgreementCandidate<'db> {
    /// Compared as written (a type-parameter constraint, a definition's kind or arity), so
    /// this one is already a finding.
    Finding(Diagnostic),
    /// Slot types compare as types, which needs the project's declarations.
    Slot(SlotPair<'db>),
    /// So do the bodies of a type defined both ways.
    Definition(DefinitionPair<'db>),
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct DefinitionPair<'db> {
    name: Name<'db>,
    inline: Ty<'db>,
    comment: Ty<'db>,
    /// Both bodies with their type parameters renamed by position.
    inline_compared: Ty<'db>,
    comment_compared: Ty<'db>,
    range: TextRange,
    comment_range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct SlotPair<'db> {
    slot: Slot,
    inline: Ty<'db>,
    comment: Ty<'db>,
    /// Both types with their type parameters renamed by position.
    inline_compared: Ty<'db>,
    comment_compared: Ty<'db>,
    /// The coercion directive that decides the type, and which annotation wrote it.
    governing: Option<(&'static str, &'static str)>,
    range: TextRange,
    comment_range: TextRange,
}

#[salsa::tracked(returns(ref))]
fn agreement_candidates<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<AgreementCandidate<'db>> {
    let root = parse(db, file).syntax_node();
    if matching_nodes(&root, |node, _| is_green_inline_site(node)).is_empty()
        || !inline_annotations_enabled(db)
    {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let mut parents: Vec<SyntaxNode> = Vec::new();
    for node in matching_nodes(&root, |node, _| node.kind() == SyntaxKind::ANNOTATION) {
        if let Some(parent) = node.parent()
            && !parents.contains(&parent)
        {
            parents.push(parent);
        }
    }
    for (comment, target) in parents.iter().flat_map(statement_annotations) {
        let AnnotationTarget::Attached(statement) = target else {
            continue;
        };
        let Some(site) = statement_inline_site(&statement) else {
            continue;
        };
        let signature = (site.kind() == SyntaxKind::FUNCTION_DEF).then_some(&site);
        let comment_slots = comment_slots(db, &comment, signature);
        let lowered_comment = lower_annotation(db, &comment);
        // Type parameters compare by position, so `<T>` and `<U>` over the same signature
        // agree; each binder's constraint is compared on its own.
        let comment_binders = lowered_comment
            .declared
            .as_ref()
            .map(|declared| declared.binders.clone())
            .unwrap_or_default();
        let inline_binders = lower_inline_annotation(db, &site)
            .declared
            .map(|declared| declared.binders)
            .unwrap_or_default();
        let comment_names = positional_binders(db, &comment_binders);
        let inline_names = positional_binders(db, &inline_binders);
        let binder_nodes = |node: &SyntaxNode| -> Vec<TextRange> {
            node.descendants()
                .filter(|binder| binder.kind() == SyntaxKind::TYPE_BINDER)
                .map(|binder| binder.text_range())
                .collect()
        };
        let comment_binder_ranges = binder_nodes(&comment);
        let inline_binder_ranges = (site.kind() == SyntaxKind::FUNCTION_DEF)
            .then(|| {
                site.children()
                    .find(|child| child.kind() == SyntaxKind::TYPE_BINDER_LIST)
                    .map(|list| binder_nodes(&list))
            })
            .flatten()
            .unwrap_or_default();
        for (index, ((_, inline_constraint), (_, comment_constraint))) in
            inline_binders.iter().zip(&comment_binders).enumerate()
        {
            let (Some(&inline_range), Some(&comment_range)) = (
                inline_binder_ranges.get(index),
                comment_binder_ranges.get(index),
            ) else {
                continue;
            };
            if inline_constraint == comment_constraint {
                continue;
            }
            let describe = |constraint: &Constraint| match constraint.spelling() {
                Some(spelling) => format!("`{spelling}`"),
                None => "no constraint".to_owned(),
            };
            candidates.push(AgreementCandidate::Finding(Diagnostic {
                range: inline_range,
                severity: Severity::Warning,
                code: "annotation-mismatch",
                message: format!(
                    "the inline annotation gives this type parameter {}, but the `#:` comment gives it {}. ry uses the inline one; make the two agree, or delete one",
                    describe(inline_constraint),
                    describe(comment_constraint)
                ),
                related: vec![RelatedLocation {
                    file,
                    range: comment_range,
                    message: "the `#:` comment's type parameter is here",
                }],
            }));
        }
        // A coercion governs wherever it is written, the inline one first.
        let coercion = |annotation: &Annotation| {
            if annotation.trusted {
                Some("@trust")
            } else if annotation.if_unknown {
                Some("@if-unknown")
            } else if annotation.new_nominal.is_some() {
                Some("@new")
            } else {
                None
            }
        };
        let governing = match coercion(&lower_inline_annotation(db, &site)) {
            Some(directive) => Some(("inline annotation's", directive)),
            None => coercion(&lowered_comment).map(|directive| ("comment's", directive)),
        };
        for inline in inline_slots(db, &site) {
            let Some(written) = comment_slots.iter().find(|slot| slot.slot == inline.slot) else {
                continue;
            };
            let inline_compared = substitute_rigid(db, inline.ty, &inline_names);
            let comment_compared = substitute_rigid(db, written.ty, &comment_names);
            if inline_compared == comment_compared {
                continue;
            }
            candidates.push(AgreementCandidate::Slot(SlotPair {
                slot: inline.slot,
                inline: inline.ty,
                comment: written.ty,
                inline_compared,
                comment_compared,
                governing,
                range: inline.range,
                comment_range: written.range,
            }));
        }
    }
    candidates
}

/// A type defined both ways (a `#:` block directly above an inline definition of the same
/// name) must be the same definition both ways: the same kind, the same type parameters
/// with the same constraints, and the same body once type parameters pair by position.
/// Memoized for the same reason as [`agreement_candidates`].
#[salsa::tracked(returns(ref))]
fn definition_agreement_candidates<'db>(
    db: &'db dyn Db,
    file: SourceFile,
) -> Vec<AgreementCandidate<'db>> {
    let root = parse(db, file).syntax_node();
    let pairs: Vec<(SyntaxNode, SyntaxNode)> = root
        .children()
        .filter(|node| node.kind() == SyntaxKind::TYPE_DEFINITION)
        .filter_map(|node| paired_comment_definition(&node).map(|comment| (node, comment)))
        .collect();
    if pairs.is_empty() || !inline_annotations_enabled(db) {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for (inline_node, comment) in pairs {
        let inline = lower_inline_annotation(db, &inline_node);
        let lowered_comment = lower_annotation(db, &comment);
        if !inline.errors.is_empty() || !lowered_comment.errors.is_empty() {
            continue;
        }
        let Some(inline_definition) = inline.definitions.first() else {
            continue;
        };
        let Some((comment_definition, &comment_site)) = lowered_comment
            .definitions
            .iter()
            .zip(&lowered_comment.definition_sites)
            .find(|(definition, _)| definition.name == inline_definition.name)
        else {
            continue;
        };
        let Some(directive) = comment
            .children()
            .find(|child| child.text_range() == comment_site)
        else {
            continue;
        };
        let name = inline_definition.name.text(db);
        let child_of = |node: &SyntaxNode, kind: SyntaxKind| {
            node.children().find(|child| child.kind() == kind)
        };
        let body_of = |node: &SyntaxNode| {
            node.children().find(|child| {
                is_type_kind(child.kind()) && child.kind() != SyntaxKind::TYPE_BINDER_LIST
            })
        };
        let mut push = |range: TextRange,
                        disagreement: String,
                        related_range: TextRange,
                        related: &'static str| {
            candidates.push(AgreementCandidate::Finding(Diagnostic {
                range,
                severity: Severity::Warning,
                code: "annotation-mismatch",
                message: format!(
                    "{disagreement}. ry uses the inline definition; make the two agree, or delete one"
                ),
                related: vec![RelatedLocation {
                    file,
                    range: related_range,
                    message: related,
                }],
            }));
        };
        let name_range = child_of(&inline_node, SyntaxKind::NAME)
            .map_or(inline_node.text_range(), |node| node.text_range());
        if inline_definition.alias != comment_definition.alias {
            let (inline_kind, comment_directive) = if inline_definition.alias {
                ("an alias", "@type")
            } else {
                ("a nominal type", "@alias")
            };
            push(
                name_range,
                format!(
                    "the inline definition makes `{name}` {inline_kind}, but the `#:` comment declares it with `{comment_directive}`"
                ),
                directive.text_range(),
                "the `#:` comment's definition is here",
            );
            continue;
        }
        let inline_binders = child_of(&inline_node, SyntaxKind::TYPE_BINDER_LIST);
        let comment_binders = child_of(&directive, SyntaxKind::TYPE_BINDER_LIST);
        let (inline_count, comment_count) = (
            inline_definition.parameters.len(),
            comment_definition.parameters.len(),
        );
        if inline_count != comment_count {
            push(
                inline_binders
                    .as_ref()
                    .map_or(name_range, |list| list.text_range()),
                format!(
                    "the inline definition gives `{name}` {inline_count} type {}, but the `#:` comment gives it {comment_count}",
                    plural(inline_count, "parameter", "parameters")
                ),
                comment_binders
                    .as_ref()
                    .map_or(directive.text_range(), |list| list.text_range()),
                "the `#:` comment's type parameters are here",
            );
            continue;
        }
        let constraints = |list: &Option<SyntaxNode>| -> Vec<(String, TextRange)> {
            list.iter()
                .flat_map(|list| list.children())
                .filter(|binder| binder.kind() == SyntaxKind::TYPE_BINDER)
                .map(|binder| {
                    let spelling = binder
                        .children()
                        .filter(|word| word.kind() == SyntaxKind::NAME)
                        .skip(1)
                        .map(|word| word.text().to_string())
                        .collect::<Vec<_>>()
                        .join(" ");
                    (spelling, binder.text_range())
                })
                .collect()
        };
        let describe = |spelling: &str| {
            if spelling.is_empty() {
                "no constraint".to_owned()
            } else {
                format!("`{spelling}`")
            }
        };
        for ((inline_spelling, inline_range), (comment_spelling, comment_range)) in
            constraints(&inline_binders)
                .iter()
                .zip(&constraints(&comment_binders))
        {
            if inline_spelling != comment_spelling {
                push(
                    *inline_range,
                    format!(
                        "the inline definition gives this type parameter {}, but the `#:` comment gives it {}",
                        describe(inline_spelling),
                        describe(comment_spelling)
                    ),
                    *comment_range,
                    "the `#:` comment's type parameter is here",
                );
            }
        }
        let [inline_body, comment_body] =
            [inline_definition, comment_definition].map(|definition| {
                let binders: Vec<(Name, Constraint)> = definition
                    .parameters
                    .iter()
                    .map(|parameter| (*parameter, Constraint::Unconstrained))
                    .collect();
                substitute_rigid(db, definition.body, &positional_binders(db, &binders))
            });
        if inline_body != comment_body
            && let (Some(inline_type), Some(comment_type)) =
                (body_of(&inline_node), body_of(&directive))
        {
            candidates.push(AgreementCandidate::Definition(DefinitionPair {
                name: inline_definition.name,
                inline: inline_definition.body,
                comment: comment_definition.body,
                inline_compared: inline_body,
                comment_compared: comment_body,
                range: inline_type.text_range(),
                comment_range: comment_type.text_range(),
            }));
        }
    }
    candidates
}

/// Renames type parameters by position, so two signatures that differ only in what
/// they call their type parameters compare equal.
fn positional_binders<'db>(
    db: &'db dyn Db,
    binders: &[(Name<'db>, Constraint)],
) -> FxHashMap<Name<'db>, Ty<'db>> {
    binders
        .iter()
        .enumerate()
        .map(|(index, (name, _))| {
            (
                *name,
                Ty::new(db, TyKind::Rigid(Name::new(db, format!("#{index}")))),
            )
        })
        .collect()
}

/// Equal once aliases are expanded and union members are put in one order.
fn same_type<'db>(
    db: &'db dyn Db,
    left: Ty<'db>,
    right: Ty<'db>,
    definitions: &BTreeMap<String, NamedDefinition<'db>>,
) -> bool {
    left == right
        || canonical_type(db, left, definitions, 0) == canonical_type(db, right, definitions, 0)
}

fn canonical_type<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    definitions: &BTreeMap<String, NamedDefinition<'db>>,
    depth: usize,
) -> Ty<'db> {
    // An alias cycle is reported elsewhere; past this depth the type is compared as written.
    if depth > 64 {
        return ty;
    }
    match ty.kind(db) {
        TyKind::Named(name, arguments) => {
            if let Some(definition) = definitions
                .get(name.text(db))
                .filter(|definition| definition.alias)
            {
                let expanded = apply_definition(db, definition, arguments);
                return canonical_type(db, expanded, definitions, depth + 1);
            }
        }
        TyKind::Union(members) => {
            let mut members: Vec<(String, Ty<'db>)> = members
                .iter()
                .map(|&member| {
                    let member = canonical_type(db, member, definitions, depth + 1);
                    (TypeRenderer::default().render(db, member), member)
                })
                .collect();
            members.sort_by(|left, right| left.0.cmp(&right.0));
            return union_of(db, members.into_iter().map(|(_, member)| member));
        }
        _ => {}
    }
    map_child_types(db, ty, &mut |child| {
        canonical_type(db, child, definitions, depth + 1)
    })
}

/// Rigid binders count (their use adds the atomic bound); nominals never do, and
/// undeclared names stay silent because the unknown-type error reports them.
fn vector_element_atomic<'db>(
    db: &'db dyn Db,
    element: Ty<'db>,
    definitions: &BTreeMap<String, NamedDefinition<'db>>,
    stub_nominals: &BTreeSet<String>,
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

/// Typo-suggestion candidates only: lowering resolves these before the
/// unknown-name check.
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
    "frame",
];

#[salsa::tracked(returns(ref))]
fn top_level_name_sites(db: &dyn Db, file: SourceFile) -> Vec<(String, TextRange)> {
    let mut sites = Vec::new();
    for &item in item_tree(db, file) {
        if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value) {
            continue;
        }
        let Some(name) = item.name(db).clone() else {
            continue;
        };
        let Some(range) = item_name_range(db, item) else {
            continue;
        };
        sites.push((name, range));
    }
    sites
}

/// Range-free, so the project-wide duplicate map depends only on which names
/// exist and survives body edits unchanged.
#[salsa::tracked(returns(ref))]
fn top_level_binding_names(db: &dyn Db, file: SourceFile) -> Vec<String> {
    let mut names = Vec::new();
    for &item in item_tree(db, file) {
        if !matches!(*item.kind(db), ItemKind::Function | ItemKind::Value) {
            continue;
        }
        if let Some(name) = item.name(db).clone() {
            names.push(name);
        }
    }
    names
}

/// Near-empty in healthy projects, so its value-equality firewall holds.
#[salsa::tracked(returns(ref))]
fn duplicate_binding_map(db: &dyn Db, files: ProjectFiles) -> FxHashMap<String, Vec<SourceFile>> {
    let mut sites: FxHashMap<String, Vec<SourceFile>> = FxHashMap::default();
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

/// Each site warns: per-site winner semantics make every earlier binding dead.
/// Ranges are read only for files involved in a duplication.
fn duplicate_binding_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    if *file.kind(db) != DocumentKind::Package {
        return Vec::new();
    }
    let Some(files) = ProjectFiles::try_get(db) else {
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

#[salsa::tracked(returns(ref))]
fn type_declaration_sites(db: &dyn Db, file: SourceFile) -> Vec<(String, TextRange)> {
    let parsed = parse(db, file);
    let mut sites: Vec<(String, TextRange)> = Vec::new();
    let declarations = matching_nodes(&parsed.syntax_node(), |node, _| {
        matches!(
            node.kind(),
            SyntaxKind::ANNOTATION | SyntaxKind::TYPE_DEFINITION
        )
    });
    for node in declarations {
        let annotation = match node.kind() {
            SyntaxKind::ANNOTATION => lower_annotation(db, &node),
            SyntaxKind::TYPE_DEFINITION => {
                // A `#:` block spelling the same definition directly above is the same
                // declaration, so the inline site stands for both.
                if let Some(comment) = paired_comment_definition(&node) {
                    let comment_range = comment.text_range();
                    let name = lower_inline_annotation(db, &node)
                        .definitions
                        .first()
                        .map(|definition| definition.name.text(db).to_owned());
                    sites.retain(|(site_name, range)| {
                        Some(site_name) != name.as_ref() || !comment_range.contains_range(*range)
                    });
                }
                lower_inline_annotation(db, &node)
            }
            _ => continue,
        };
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

/// Range-free for the same firewall as [`top_level_binding_names`].
#[salsa::tracked(returns(ref))]
fn type_declaration_names(db: &dyn Db, file: SourceFile) -> Vec<String> {
    type_declaration_sites(db, file)
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

#[salsa::tracked(returns(ref))]
fn duplicate_type_map(db: &dyn Db, files: ProjectFiles) -> FxHashMap<String, Vec<SourceFile>> {
    let mut sites: FxHashMap<String, Vec<SourceFile>> = FxHashMap::default();
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

/// `@type` and `@alias` share one namespace: project-global for package files,
/// file-local for scripts, hence the separate walks.
fn duplicate_type_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    if *file.kind(db) != DocumentKind::Package {
        return script_duplicate_type_diagnostics(db, file);
    }
    let Some(files) = ProjectFiles::try_get(db) else {
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

/// `:::` reaches unexported names, so only `::` checks exports. No stub corpus
/// means no validation.
fn namespace_read_message(db: &dyn Db, read: &NamespaceRead) -> Option<String> {
    // A package's export set is not the names its sources bind (re-exports, S4
    // generics, lazy-loaded data, `.onLoad` bindings), so its own qualified reads
    // are never validated.
    if is_own_package(db, &read.package) {
        return None;
    }
    match namespace_known(db, &read.package)? {
        // A declared dependency without stubs is real, just undescribed.
        false if declared_dependency(db, &read.package) => None,
        false => Some(format!("unknown package namespace `{}`.", read.package)),
        true => {
            let name = read.name.as_ref()?;
            if !read.internal && !namespace_exports(db, &read.package, name) {
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
fn super_globals(db: &dyn Db, file: SourceFile) -> BTreeSet<Symbol> {
    let mut names = BTreeSet::new();
    for &item in item_tree(db, file) {
        if let Some(naming) = item_naming(db, item) {
            names.extend(naming.super_globals.iter().copied());
        }
    }
    names
}

/// Names declared via top-level `globalVariables(...)`, the escape hatch for
/// NSE. Only direct top-level calls with literal strings are recognized.
#[salsa::tracked(returns(ref))]
fn file_global_variable_declarations(db: &dyn Db, file: SourceFile) -> BTreeSet<String> {
    use crate::hir::LiteralKind;
    let mut declared = BTreeSet::new();
    for &item in item_tree(db, file) {
        let Some(module) = item_hir(db, item) else {
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
        let mut collect = |id| {
            if let ExpressionKind::Literal(LiteralKind::String(value)) = &module.expression(id).kind
                && !value.is_empty()
            {
                declared.insert(value.to_string());
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

/// Unioned once per project so the per-read guard is a single lookup.
#[salsa::tracked(returns(ref))]
fn project_global_variable_declarations(db: &dyn Db, files: ProjectFiles) -> BTreeSet<String> {
    let mut declared = BTreeSet::new();
    for &file in files.files(db).iter() {
        declared.extend(file_global_variable_declarations(db, file).iter().cloned());
    }
    declared
}

/// R6 injects these into every method's environment at construction, so they
/// resolve nowhere lexically.
const R6_INJECTED_BINDINGS: [&str; 3] = ["self", "private", "super"];

/// `UseMethod`, `NextMethod` and group-generic dispatch bind these in the method's frame, and
/// any function can be a method, so a read inside a function body may see them.
const DISPATCH_VARIABLES: [&str; 6] = [
    ".Generic",
    ".Method",
    ".Class",
    ".Group",
    ".GenericCallEnv",
    ".GenericDefEnv",
];

/// Purely syntactic: a local binding shadowing `R6Class` is not honored.
#[salsa::tracked]
fn file_defines_r6_class(db: &dyn Db, file: SourceFile) -> bool {
    item_spans(db, file).iter().any(|span| {
        item_hir(db, span.item).as_ref().is_some_and(|module| {
            module.expressions.iter().any(|expression| {
                let ExpressionKind::Call { callee, .. } = &expression.kind else {
                    return false;
                };
                match &module.expression(*callee).kind {
                    ExpressionKind::NameRef(name) => name == "R6Class",
                    ExpressionKind::Namespace { name, .. } => name.as_deref() == Some("R6Class"),
                    _ => false,
                }
            })
        })
    })
}

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

/// A per-name firewall over the project-wide definition maps: a new definition elsewhere
/// re-runs this, but a reading file re-checks only when its suggestion changes.
#[salsa::tracked(returns(clone))]
fn project_typo_suggestion<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    name: Name<'db>,
    tests: bool,
) -> Option<String> {
    let test_names = tests
        .then(|| crate::test_definitions(db, files).keys())
        .into_iter()
        .flatten();
    nearest_name(
        name.text(db),
        crate::package_definitions(db, files)
            .keys()
            .chain(test_names)
            .map(String::as_str),
    )
}

/// Memoized per name: the scan over the whole stub corpus is the expensive part.
#[salsa::tracked(returns(clone))]
fn typo_suggestion<'db>(db: &'db dyn Db, name: Name<'db>) -> Option<String> {
    let library = stubs(db)?;
    nearest_name(
        name.text(db),
        library
            .schemes
            .keys()
            .chain(library.known_exports.iter())
            .map(String::as_str),
    )
}

/// Ties break lexicographically for determinism. The budget is deliberately
/// tight (a wrong guess is worse than none); transposition counts as one edit
/// so `lenght` still finds `length`.
pub(crate) fn nearest_name<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<String> {
    let name_characters: Vec<char> = name.chars().collect();
    if name_characters.len() < 4 {
        return None;
    }
    // A standard-library name such as `Str.split` is usually the start of a longer one
    // (`Str.split_on`), which is farther away by edit distance than an unrelated base function.
    let candidates: Vec<&str> = candidates.collect();
    if ["List.", "Str.", "Num.", "Result.", "Dict.", "Set."]
        .iter()
        .any(|module| name.starts_with(module))
    {
        let completion = candidates
            .iter()
            .filter(|candidate| candidate.len() > name.len() && candidate.starts_with(name))
            .min_by_key(|candidate| (candidate.len(), **candidate));
        if let Some(candidate) = completion {
            return Some((*candidate).to_owned());
        }
    }
    let candidates = candidates.into_iter();
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
    best.map(|(_, candidate)| candidate.to_owned())
}

/// Optimal string alignment distance, or `None` past `budget`. The caller lends
/// the DP rows so a scan allocates nothing per candidate.
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
    // Transposition reads the row two above, hence a third buffer.
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
        mem::swap(&mut before_previous, previous);
        mem::swap(previous, current);
        current.clear();
        current.resize(right.len() + 1, 0);
    }
    (previous[right.len()] <= budget).then_some(previous[right.len()])
}

/// A script's top level is one frame run in order, so the unused check spans
/// items. A read inside a nested function keeps every write to the name live.
fn script_unused_bindings(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let error_ranges = file_error_ranges(db, file);
    struct Definer {
        item_index: usize,
        name: Symbol,
        range: TextRange,
        used: bool,
        /// Rebinds only on some runs, so it does not end earlier definers' liveness.
        conditional: bool,
    }
    let mut definers: Vec<Definer> = Vec::new();
    let mut reads: Vec<(usize, Symbol, bool)> = Vec::new();
    for (index, span) in item_spans(db, file).iter().enumerate() {
        let item = span.item;
        let offset = span.range.start();
        let Some(naming) = item_naming(db, item) else {
            continue;
        };
        let item_name = matches!(*item.kind(db), ItemKind::Function | ItemKind::Value)
            .then(|| item.name(db).clone())
            .flatten();
        let broken = |range: TextRange| {
            error_ranges
                .iter()
                .any(|error| error.start() <= range.end() && range.start() <= error.end())
        };
        if let Some(name) = item_name.clone()
            && let Some(module) = item_hir(db, item)
            && let Some(root) = module.root
            && let Some(name_range) = item_name_range(db, item)
        {
            let root_range = module.expression(root).range;
            let statement_range = root_range + offset;
            // The finding goes on the name, but a syntax error anywhere in the statement
            // suppresses it.
            if !broken(statement_range) {
                definers.push(Definer {
                    item_index: index,
                    name: Symbol::new(&name),
                    range: name_range,
                    used: false,
                    conditional: false,
                });
            }
        }
        for binding in naming.bindings.values() {
            if binding.kind != BindingKind::TopLevel
                || item_name.as_deref() == Some(binding.name.as_str())
            {
                continue;
            }
            let range = binding.range + offset;
            if !broken(range) {
                definers.push(Definer {
                    item_index: index,
                    name: binding.name,
                    range,
                    // A read within the item itself already keeps it alive.
                    used: naming.used_top_level_names.contains(&binding.name),
                    conditional: true,
                });
            }
        }
        for (expression, name) in &naming.non_locals {
            let deferred = naming.deferred_non_locals.contains(expression);
            reads.push((index, *name, deferred));
        }
        // Quiet reads are never reported unresolved, but at run time they fall back to
        // enclosing bindings, so they keep definers alive.
        for (expression, name) in &naming.quiet_reads {
            let deferred = naming.deferred_quiet_reads.contains(expression);
            reads.push((index, *name, deferred));
        }
        // R resolves `%op%` when the expression runs and helper operators are used
        // project-wide, so operator reads count as deferred.
        for name in &naming.quiet_operator_reads {
            reads.push((index, *name, true));
        }
        // A loop's first iteration reads the earlier binding of its carried variable.
        for expression in &naming.maybe_undefined {
            let Some(slot) = naming.resolutions.get(expression) else {
                continue;
            };
            let Some(binding) = naming.bindings.get(slot) else {
                continue;
            };
            if binding.kind == BindingKind::TopLevel {
                reads.push((index, binding.name, false));
            }
        }
    }
    for (read_index, name, deferred) in &reads {
        if *deferred {
            // The function may run any time after the frame exists, so every write is live.
            for definer in definers.iter_mut().filter(|d| d.name == *name) {
                definer.used = true;
            }
        } else {
            // Continue past conditional definers: they rebind only on some runs.
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
    // S3 methods are reached by dispatch, not by a read of their name.
    let generics = crate::s3_generics(db, file);
    definers
        .into_iter()
        .filter(|definer| {
            !definer.used
                && !definer.name.starts_with('.')
                && !definer.name.starts_with('_')
                && !crate::is_s3_method_name(db, &definer.name, &generics)
        })
        .map(|definer| {
            Diagnostic::new(
                definer.range,
                Severity::Warning,
                "unused",
                format!("`{}` is assigned but never used.", definer.name),
            )
        })
        .collect()
}

/// Published by hosts only under `[check] strict` or the per-file directive. An
/// origin that is an assignment's value is phrased against the assigned name.
#[salsa::tracked(returns(clone))]
pub fn strict_diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    // Reads silenced by an unknowable export set are undetermined, which is what
    // strict mode reports.
    let broken_ranges = file_error_ranges(db, file);
    for (item_index, span) in item_spans(db, file).iter().enumerate() {
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
            item_hir(db, item),
            item_naming(db, item),
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
            diagnostics.push(Diagnostic::new(module.expression(*expression).range + offset, Severity::Error, "strict", format!(
                    "strict mode: nothing this project can see defines `{}`. It is silent only because an attached package's exports are unknown. Declare the package with a `.Rtypes` stub to check it",
                    display_name(name)
                )));
        }
    }
    for span in item_spans(db, file) {
        let item = span.item;
        let offset = span.range.start();
        let Some(check) = item_check(db, item) else {
            continue;
        };
        if check.strict_origins.is_empty() {
            continue;
        }
        let Some(module) = item_hir(db, item) else {
            continue;
        };
        let mut assignment_targets = FxHashMap::default();
        for expression in &module.expressions {
            if let ExpressionKind::Assign { target, value, .. } = &expression.kind
                && let ExpressionKind::NameRef(name) = &module.expression(*target).kind
            {
                assignment_targets.insert(*value, *name);
            }
        }
        for origin in &check.strict_origins {
            // Loop-widened and recursive origins are about the named binding.
            let assignment_target = match &origin.kind {
                StrictOriginKind::LoopWidened(_)
                | StrictOriginKind::RecursiveUnknown(_)
                | StrictOriginKind::UntypedParameter(_) => None,
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
                    StrictOriginKind::UntypedParameter(name) => format!(
                        "strict mode: parameter `{}` has no type. How the function uses it does not determine one, so any value is accepted unchecked. Annotate it, as in `{}: TYPE`",
                        display_name(name),
                        display_name(name)
                    ),
                }
            };
            diagnostics.push(Diagnostic::new(
                origin.range + offset,
                Severity::Error,
                "strict",
                message,
            ));
        }
    }
    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    diagnostics
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
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
        TypeErrorKind::MissingArguments { names } => {
            let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
            let listed = match quoted.split_last() {
                Some((last, [])) => last.clone(),
                Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                None => String::new(),
            };
            // A declared required parameter need not be read, so the reason is the
            // signature's, not the body's.
            format!("this call leaves out {listed}, which the function requires")
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
                    expected_parameters
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
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
        TypeErrorKind::ColumnDoesNotExist {
            column,
            frame,
            columns,
            suggestion,
        } => {
            let subject = match frame {
                Some(frame) => format!("the frame `{frame}`"),
                None => "this frame".to_owned(),
            };
            let hint = match suggestion {
                Some(nearest) => format!(". Did you mean `{nearest}`?"),
                None if columns.is_empty() => ", which has no columns".to_owned(),
                None => format!(
                    ". Its columns are {}",
                    columns
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            format!("{subject} has no column `{column}`{hint}")
        }
        TypeErrorKind::BranchTypesDiffer {
            function,
            first,
            first_ty,
            second,
            second_ty,
        } => format!(
            "`{function}` needs `{first}` and `{second}` of one type, but `{first}` is `{}` and `{second}` is `{}`",
            renderer.render(db, *first_ty),
            renderer.render(db, *second_ty)
        ),
        TypeErrorKind::ColumnNameCount { names, columns } => format!(
            "this frame has {columns} {}, but {names} {} given",
            if *columns == 1 { "column" } else { "columns" },
            if *names == 1 { "name is" } else { "names are" }
        ),
        TypeErrorKind::IncompatibleJoinColumns { condition, x, i } => format!(
            "the join condition `{condition}` compares `{}` with `{}`. A join matches numbers with numbers, strings with strings or factors, and logicals with logicals.",
            renderer.render(db, *x),
            renderer.render(db, *i)
        ),
        TypeErrorKind::OrderedJoinOnText {
            condition,
            column,
            found,
        } => format!(
            "the join condition `{condition}` compares numbers, but `{column}` is `{}`. Only `==` joins strings.",
            renderer.render(db, *found)
        ),
        TypeErrorKind::InSubsetColumn { column, inner } => {
            let inner = TypeError {
                range: error.range,
                kind: (**inner).clone(),
            };
            let message = render_type_error_message(db, &inner);
            format!(
                "{}, for the column `{column}` of `.SD`",
                message.trim_end_matches('.')
            )
        }
        TypeErrorKind::RollingNonEquiJoin => {
            "a rolling join needs every join condition to be `==`".to_owned()
        }
        TypeErrorKind::LossyColumnWrite {
            column,
            column_ty,
            found,
        } => {
            let effect = match (
                column_element(db, *column_ty).kind(db),
                column_element(db, *found).kind(db),
            ) {
                (TyKind::Named(..), _) => {
                    "is read as a level's position rather than a label, and a number that is not one becomes `NA`"
                }
                (TyKind::Scalar(Atomic::Logical), TyKind::Scalar(Atomic::Character)) => {
                    "is parsed as `TRUE` or `FALSE`, and any other string becomes `NA`"
                }
                (_, TyKind::Scalar(Atomic::Character)) => {
                    "is parsed as a number, and a string that is not one becomes `NA`"
                }
                _ => "is truncated to an integer",
            };
            format!(
                "column `{column}` holds `{}`, so this `{}` value {effect}",
                renderer.render(db, *column_ty),
                renderer.render(db, *found)
            )
        }
        TypeErrorKind::BadRowIndex { found } => {
            let hint = if matches!(found.kind(db), TyKind::Frame(_)) {
                ". To join two frames, add `on =`."
            } else {
                ""
            };
            format!(
                "a frame's row index must be logical (a filter) or numeric (row positions), but this is `{}`{hint}",
                renderer.render(db, *found)
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
        TypeErrorKind::PatternNeverMatches { found, hint } => {
            let mut message = format!(
                "this pattern can never match: the value is `{}`",
                renderer.render(db, *found)
            );
            if let Some(hint) = hint {
                message.push_str(&format!(". {hint}"));
            }
            message
        }
        TypeErrorKind::NativeValueToR { function, kind } => {
            let kind = match kind {
                NativeValueKind::Record => "record",
                NativeValueKind::Tuple => "tuple",
                NativeValueKind::Tag => "tag",
                NativeValueKind::Dict => "dict",
                NativeValueKind::Set => "set",
            };
            format!(
                "`{function}` is an R function, and this value is a ry {kind}, not an R object. Convert it with `as.list()` first"
            )
        }
        TypeErrorKind::FieldCannotBeAdded { field, container } => format!(
            "field `{field}` does not exist in `{}`, and assigning to it cannot add it, because a record's fields are fixed",
            renderer.render(db, *container)
        ),
        TypeErrorKind::ConflictingTags { first, second } => format!(
            "these branches build `{}` and `{}`, one tag with two payload shapes. Within a union a tag has one shape, so that a `match` can tell its cases apart",
            renderer.render(db, *first),
            renderer.render(db, *second)
        ),
    }
}

fn match_finding_diagnostic(finding: &MatchFinding, offset: TextSize) -> Diagnostic {
    let range = finding.range + offset;
    match &finding.kind {
        MatchFindingKind::Unreachable => Diagnostic::new(
            range,
            Severity::Warning,
            "unreachable-pattern",
            "this arm is unreachable: the arms above it already match every value its pattern matches".to_owned(),
        ),
        MatchFindingKind::NonExhaustive { missing, more } => {
            let message = if missing.iter().all(|case| case == "_") {
                "this `match` does not handle every value: add a catch-all `_ => ...` arm".to_owned()
            } else {
                let mut cases: Vec<String> = missing.iter().map(|case| format!("`{case}`")).collect();
                if *more {
                    cases.push("more".to_owned());
                }
                let listed = match cases.split_last() {
                    Some((last, [])) => last.clone(),
                    Some((last, others)) => format!("{} and {last}", others.join(", ")),
                    None => String::new(),
                };
                let (arm, pronoun) = if cases.len() == 1 {
                    ("an arm", "it")
                } else {
                    ("arms", "them")
                };
                format!(
                    "this `match` does not handle {listed}: add {arm} for {pronoun}, or a catch-all `_ => ...` arm"
                )
            };
            Diagnostic::new(range, Severity::Error, "non-exhaustive-match", message)
        }
    }
}

/// Names variables `T`/`U`/`V` in first-occurrence order. Share one renderer
/// across every type in a message: a fresh one restarts the numbering.
#[derive(Default)]
pub struct TypeRenderer<'db> {
    names: Vec<RenderedVar<'db>>,
}

#[derive(PartialEq, Eq)]
enum RenderedVar<'db> {
    Inference(InferenceVar),
    Rigid(Name<'db>),
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
            TyKind::Tuple(items) => format!("list{{{}}}", self.render_items(db, items)),
            TyKind::Record(fields) => format!("list{{{}}}", self.render_fields(db, fields)),
            TyKind::NativeTuple(items) => format!("tuple{{{}}}", self.render_items(db, items)),
            TyKind::NativeRecord(fields) => {
                format!("record{{{}}}", self.render_native_fields(db, fields))
            }
            TyKind::Tag(name, payload) => {
                let name = name.text(db);
                match payload.map(|payload| payload.kind(db)) {
                    None => format!(":{name}"),
                    Some(TyKind::NativeTuple(items)) => {
                        format!(":{name}({})", self.render_items(db, items))
                    }
                    Some(TyKind::NativeRecord(fields)) => {
                        format!(":{name}({})", self.render_native_fields(db, fields))
                    }
                    Some(_) => format!(":{name}(Unknown)"),
                }
            }
            TyKind::OtherTags => "..".to_owned(),
            TyKind::Frame(Some(columns)) => {
                format!("frame{{{}}}", self.render_fields(db, columns))
            }
            TyKind::Frame(None) => "frame".to_owned(),
            TyKind::Function(function) => self.render_function(db, function),
            TyKind::Union(members) => {
                let members: Vec<String> = members
                    .iter()
                    .map(|&member| {
                        // `->` extends over a whole union, so a function member needs its
                        // parentheses or the rendered type means something else.
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

    /// Registers each binder's display name so the body rendered afterwards through
    /// this renderer reuses them.
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

    fn render_items(&mut self, db: &'db dyn Db, items: &[Ty<'db>]) -> String {
        let items: Vec<String> = items.iter().map(|&item| self.render(db, item)).collect();
        items.join(", ")
    }

    fn render_native_fields(&mut self, db: &'db dyn Db, fields: &[NativeField<'db>]) -> String {
        let fields: Vec<String> = fields
            .iter()
            .map(|field| {
                let name = field.name.text(db);
                if is_syntactic_name(name) {
                    format!("{name}: {}", self.render(db, field.ty))
                } else {
                    format!("`{name}`: {}", self.render(db, field.ty))
                }
            })
            .collect();
        fields.join(", ")
    }

    fn render_fields(&mut self, db: &'db dyn Db, fields: &[RecordField<'db>]) -> String {
        let fields: Vec<String> = fields
            .iter()
            .map(|field| {
                let name = field.name.text(db);
                // Rendered types must be copyable into annotations, and an unquoted
                // non-syntactic name can even parse as a different type
                // (`list{a,b: integer}`).
                if is_syntactic_name(name) {
                    format!("{name}: {}", self.render(db, field.ty))
                } else {
                    format!("`{name}`: {}", self.render(db, field.ty))
                }
            })
            .collect();
        fields.join(", ")
    }

    fn render_function(&mut self, db: &'db dyn Db, function: &FunctionType<'db>) -> String {
        let mut parameters = Vec::new();
        for ty in &function.positional {
            parameters.push(self.render(db, *ty));
        }
        for (index, field) in function.named.iter().enumerate() {
            // The rest parameter sits at its declared position: R matches remaining
            // positional arguments into it.
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

fn constraint_description(constraint: Constraint) -> &'static str {
    match constraint {
        Constraint::Unconstrained => "a value",
        Constraint::Numeric => "a numeric value (`integer` or `double`)",
        Constraint::AtomicElement => {
            "an atomic value (`logical`, `integer`, `double`, `complex`, `character`, or `raw`)"
        }
        Constraint::Key => "a key (a `logical`, `integer`, `double` or `character` scalar)",
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
    use crate::RootDatabase;

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
        // Past the check cap but under the parse cap.
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
        let t = Ty::new(&db, TyKind::Rigid(Name::new(&db, "A".to_owned())));
        let u = Ty::new(&db, TyKind::Rigid(Name::new(&db, "B".to_owned())));
        assert_eq!(renderer.render(&db, t), "T");
        assert_eq!(renderer.render(&db, u), "U");
        assert_eq!(renderer.render(&db, t), "T");
    }
}
