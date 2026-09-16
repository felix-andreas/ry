//! IDE features over the semantics crate's salsa queries. Every feature is a
//! pure read of the database at a byte offset; UTF-16 and editor-protocol
//! concerns live in the server, not here.
//!
//! Positions cross one boundary: per-item query results carry item-relative
//! ranges (the position-independent unit salsa cutoffs work on), while the
//! feature API speaks file-absolute offsets. `semantics::item_node` is the
//! edge that anchors an item at its current absolute position.
//!
//! Goto-definition, references, and rename share ONE occurrence engine: the
//! cursor resolves to a symbol target (a variable slot inside an item, or a
//! project-defined global name), and every feature is a projection of that
//! target's occurrence list.

use semantics::diagnostics::TypeRenderer;
use semantics::hir::{Argument, ExprId, ExpressionKind};
use semantics::naming::{BindingId, BindingKind};
use semantics::types::{FunctionType, Ty, TyKind, TypeScheme};
use semantics::{
    Db, Item, ItemKind, ProjectFiles, SourceFile, item_annotation_syntax, item_check, item_hir,
    item_naming, item_node, item_tree, package_definitions,
};
use syntax::{TextRange, TextSize};

/// A hover result: the hovered expression's absolute range, the rendered
/// type lines, and — for a variable use — where the name is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    pub range: TextRange,
    pub lines: Vec<String>,
    pub definition: Option<HoverDefinition>,
}

/// Where a hovered name is defined. Location rendering (paths, line:column)
/// happens in the host, which owns the file-to-path mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverDefinition {
    /// A slot inside the item: a local assignment, parameter, or loop
    /// variable. `maybe_undefined` marks reads not dominated by a write.
    Local {
        target: NavigationTarget,
        maybe_undefined: bool,
    },
    /// A project top-level definition (the name's winner).
    Global { target: NavigationTarget },
    /// A stdlib stub name: its declaring namespace, how many overload
    /// candidates the corpus declares for it, and — when the loader recorded
    /// one — its declaration site inside the stub corpus.
    Stub {
        namespace: String,
        overloads: usize,
        declaration: Option<StubTarget>,
    },
}

/// One phase's internal facts for the hovered position, shown by hosts under
/// a debug flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSection {
    pub title: &'static str,
    pub body: String,
}

/// A navigation target: a file and an absolute range inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavigationTarget {
    pub file: SourceFile,
    pub range: TextRange,
}

/// Where goto-definition lands: a project location, or a declaration inside
/// the installed stub corpus. Stub sources are not project files — the host
/// maps `source_index` (the position in the `StubSources` order it
/// installed) to a file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionTarget {
    Project(NavigationTarget),
    Stub(StubTarget),
}

/// A declaration site inside one installed stub source: the source's index
/// in the installed order and the name token's range within its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StubTarget {
    pub source_index: usize,
    pub range: TextRange,
}

fn stub_target(db: &dyn Db, name: &str) -> Option<StubTarget> {
    semantics::stubs::stub_declaration(db, name).map(|declaration| StubTarget {
        source_index: declaration.source_index,
        range: declaration.range,
    })
}

/// One occurrence of a symbol: a range plus whether it is a declaration
/// (an assignment target, a parameter, a for-loop variable, or a top-level
/// definition's name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub file: SourceFile,
    pub range: TextRange,
    pub is_declaration: bool,
}

pub fn hover(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<Hover> {
    // A cursor inside a `#:` annotation resolves against the type notation,
    // not the R expression it decorates.
    if let Some(cursor) = annotation_type_at(db, file, offset) {
        return annotation_type_hover(db, file, &cursor);
    }
    // Elsewhere inside a `@type`/`@alias` block (the `@`, the keyword, the
    // braces), the whole declaration hovers.
    if let Some(hover) = annotation_definition_hover(db, file, offset) {
        return Some(hover);
    }

    let position = position_in_item(db, file, offset)?;
    let check = item_check(db, position.item)?;
    let hir = item_hir(db, position.item).as_ref()?;
    // The smallest containing expression with a recorded type: write targets
    // and operators record none, so the hover widens to the enclosing typed
    // expression instead of going silent.
    let (expression, ty) = position
        .expressions_at()
        .into_iter()
        .find_map(|id| check.expression_types.get(&id).map(|ty| (id, *ty)))?;

    let mut renderer = TypeRenderer::default();
    let (line, definition) = match &hir.expression(expression).kind {
        ExpressionKind::NameRef(name) => {
            // The item's own top-level name renders the EXPORTED scheme —
            // the single exported truth — not the initializer's checked
            // type: a `#: @new` declaration brands the scheme even when the
            // initializer's own type is Unknown.
            let exported = item_naming(db, position.item).as_ref().and_then(|naming| {
                let binding = naming.resolutions.get(&expression)?;
                (naming.bindings.get(binding)?.kind == BindingKind::TopLevel).then_some(())?;
                check.scheme.as_ref()
            });
            // The whole scheme, binders included: rendering only the body
            // drops the `<T>` prefix, so a polymorphic function hovered as
            // `fn(x: T) -> T` left `T` unexplained — and disagreed with the
            // inlay hint for the same binding, which renders the scheme.
            let rendered = match exported {
                Some(scheme) => renderer.render_scheme(db, scheme),
                None => renderer.render(db, ty),
            };
            (
                format!("{name}: {rendered}"),
                hover_definition(db, files, position.item, expression, name),
            )
        }
        _ => (renderer.render(db, ty), None),
    };

    // Expression nodes may swallow trailing trivia; the hover highlight must
    // stop at the significant end.
    let range = trim_trailing_trivia(
        file.text(db),
        hir.expression(expression).range + position.item_offset,
    );
    Some(Hover {
        range,
        lines: vec![line],
        definition,
    })
}

/// Where the hovered name-use is defined: a slot's binding site, the global
/// name's winner declaration, or a stub's declaring namespace.
fn hover_definition<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    item: Item<'db>,
    expression: ExprId,
    name: &str,
) -> Option<HoverDefinition> {
    let naming = item_naming(db, item).as_ref()?;
    if let Some(binding) = naming.resolutions.get(&expression) {
        let info = naming.bindings.get(binding)?;
        // The item's own top-level binding IS the global definition.
        if info.kind == BindingKind::TopLevel {
            return global_hover_definition(db, files, &info.name, Some(item));
        }
        let item_offset = item_node(db, item)?.text_range().start();
        return Some(HoverDefinition::Local {
            target: NavigationTarget {
                file: *item.file(db),
                range: info.range + item_offset,
            },
            maybe_undefined: naming.maybe_undefined.contains(&expression),
        });
    }
    if naming.non_locals.contains_key(&expression) {
        if let Some(definition) = global_hover_definition(db, files, name, None) {
            return Some(definition);
        }
        let namespace = semantics::stubs::declaring_namespace(db, name)?;
        let overloads = semantics::stubs::stubs(db)?
            .schemes
            .get(name)
            .map_or(0, Vec::len);
        return Some(HoverDefinition::Stub {
            namespace: namespace.to_owned(),
            overloads,
            declaration: stub_target(db, name),
        });
    }
    None
}

/// The global definition site of `name`: the package winner, the first
/// declaring item across the project, or — for a top-level read of the
/// defining item itself — that item.
fn global_hover_definition(
    db: &dyn Db,
    files: ProjectFiles,
    name: &str,
    own_item: Option<Item<'_>>,
) -> Option<HoverDefinition> {
    let declaring = package_definitions(db, files)
        .get(name)
        .copied()
        .or_else(|| {
            files.files(db).iter().find_map(|&file| {
                item_tree(db, file).iter().copied().find(|item| {
                    matches!(*item.kind(db), ItemKind::Function | ItemKind::Value)
                        && item.name(db).as_deref() == Some(name)
                })
            })
        })
        .or(own_item)?;
    let node = item_node(db, declaring)?;
    let range = declared_name_range(db, declaring, &node).unwrap_or_else(|| node.text_range());
    Some(HoverDefinition::Global {
        target: NavigationTarget {
            file: *declaring.file(db),
            range,
        },
    })
}

/// Per-phase internal facts at the cursor, for hosts with debug hover
/// enabled: the lowered HIR expression, its naming resolution, and the
/// syntax-node chain under the cursor.
pub fn hover_debug(db: &dyn Db, file: SourceFile, offset: TextSize) -> Vec<DebugSection> {
    let mut sections = Vec::new();
    if let Some(position) = position_in_item(db, file, offset)
        && let Some(hir) = item_hir(db, position.item)
        && let Some(expression) = position.expressions_at().into_iter().next()
    {
        sections.push(DebugSection {
            title: "Lowering",
            body: format!("{:#?}", hir.expression(expression)),
        });
        if let Some(naming) = item_naming(db, position.item) {
            let resolution = if let Some(binding) = naming.resolutions.get(&expression) {
                match naming.bindings.get(binding) {
                    Some(info) => format!(
                        "slot `{}` ({:?}, declared at {}..{})",
                        info.name,
                        info.kind,
                        u32::from(info.range.start()),
                        u32::from(info.range.end()),
                    ),
                    None => "slot (missing binding info)".to_owned(),
                }
            } else if let Some(name) = naming.non_locals.get(&expression) {
                format!("non-local `{name}`")
            } else {
                "no resolution recorded".to_owned()
            };
            sections.push(DebugSection {
                title: "Naming",
                body: resolution,
            });
        }
    }
    let parse = semantics::parse(db, file);
    let root = parse.syntax_node();
    if let Some(token) = root
        .token_at_offset(offset)
        .right_biased()
        .or_else(|| root.token_at_offset(offset).left_biased())
    {
        let mut chain = vec![format!(
            "{:?}@{}..{}",
            token.kind(),
            u32::from(token.text_range().start()),
            u32::from(token.text_range().end()),
        )];
        for ancestor in std::iter::successors(token.parent(), syntax::SyntaxNode::parent) {
            chain.push(format!(
                "{:?}@{}..{}",
                ancestor.kind(),
                u32::from(ancestor.text_range().start()),
                u32::from(ancestor.text_range().end()),
            ));
        }
        sections.push(DebugSection {
            title: "Parsing",
            body: chain.join("\n"),
        });
    }
    sections
}

/// Shrinks a range's end past any trailing whitespace/newlines it swallowed.
fn trim_trailing_trivia(text: &str, range: TextRange) -> TextRange {
    let start = usize::from(range.start()).min(text.len());
    let end = usize::from(range.end()).min(text.len());
    let trimmed = text[start..end].trim_end().len();
    TextRange::new(range.start(), TextSize::from((start + trimmed) as u32))
}

/// Goto-definition: a slot goes to its first write; a global goes to the
/// package winner definition's name (or, in a script, the first declaration).
pub fn definition(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<DefinitionTarget> {
    match target_at(db, files, file, offset)? {
        Target::Slot { item, binding } => {
            let naming = item_naming(db, item).as_ref()?;
            let info = naming.bindings.get(&binding)?;
            let item_offset = item_node(db, item)?.text_range().start();
            Some(DefinitionTarget::Project(NavigationTarget {
                file,
                range: info.range + item_offset,
            }))
        }
        // The LAST declaration wins, matching the project definition table's
        // fold order; a name only the stub corpus declares jumps into it.
        Target::TypeName(name) => type_name_occurrences(db, files, &name)
            .into_iter()
            .rfind(|occurrence| occurrence.is_declaration)
            .map(|occurrence| {
                DefinitionTarget::Project(NavigationTarget {
                    file: occurrence.file,
                    range: occurrence.range,
                })
            })
            .or_else(|| stub_target(db, &name).map(DefinitionTarget::Stub)),
        ref target @ Target::S4 { .. } => occurrences(db, files, target)
            .into_iter()
            .find(|occurrence| occurrence.is_declaration)
            .map(|occurrence| {
                DefinitionTarget::Project(NavigationTarget {
                    file: occurrence.file,
                    range: occurrence.range,
                })
            }),
        Target::StubGlobal(name) => stub_target(db, &name).map(DefinitionTarget::Stub),
        Target::Global(name) => {
            if let Some(winner) = package_definitions(db, files).get(&name) {
                let node = item_node(db, *winner)?;
                let range =
                    declared_name_range(db, *winner, &node).unwrap_or_else(|| node.text_range());
                return Some(DefinitionTarget::Project(NavigationTarget {
                    file: *winner.file(db),
                    range,
                }));
            }
            occurrences(db, files, &Target::Global(name.clone()))
                .into_iter()
                .find(|occurrence| occurrence.is_declaration)
                .map(|occurrence| {
                    DefinitionTarget::Project(NavigationTarget {
                        file: occurrence.file,
                        range: occurrence.range,
                    })
                })
        }
    }
}

/// All occurrences of the symbol under the cursor, in file/source order.
pub fn references(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
    include_declaration: bool,
) -> Vec<Occurrence> {
    let Some(target) = target_at(db, files, file, offset) else {
        return Vec::new();
    };
    occurrences(db, files, &target)
        .into_iter()
        .filter(|occurrence| include_declaration || !occurrence.is_declaration)
        .collect()
}

/// Rename: every occurrence becomes an edit site for the new name. `None`
/// when the cursor is not on a renameable symbol (stub and unresolved names
/// have no project declaration to rename).
pub fn rename(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<Vec<Occurrence>> {
    let target = target_at(db, files, file, offset)?;
    let occurrences = occurrences(db, files, &target);
    (!occurrences.is_empty()).then_some(occurrences)
}

/// One inline type hint: `label` (": TYPE") rendered after `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    pub offset: TextSize,
    pub label: String,
}

/// Type hints for the file's plain variable bindings (replacement forms
/// update an existing binding hinted at its own definition; annotated
/// bindings already show their type). A function type is hinted whenever it
/// contains no `Unknown` — its variables generalize into binder names — while
/// any other type must be fully concrete, so partially-inferred values show
/// nothing rather than noise.
pub fn inlay_hints(db: &dyn Db, file: SourceFile, viewport: Option<TextRange>) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        let item_offset = node.text_range().start();
        if let Some(viewport) = viewport {
            let range = node.text_range();
            if range.end() < viewport.start() || viewport.end() < range.start() {
                continue;
            }
        }
        let Some(hir) = item_hir(db, item) else {
            continue;
        };
        let Some(check) = item_check(db, item) else {
            continue;
        };
        // An annotation types its binding only through surviving payload: a
        // refused block (form, ordering, duplicate-parameter, depth errors)
        // drops the payload entirely, and a definitions-only or toggle-only
        // block types nothing — all of those leave the binding hintable.
        let annotated = item_annotation_syntax(db, item).is_some_and(|annotation| {
            let lowered = semantics::annotations::lower_annotation(db, &annotation.syntax_node());
            lowered.declared.is_some() || lowered.new_nominal.is_some() || lowered.trusted
        });
        // Nested statements carry their own annotations; a hint would just
        // restate the line above.
        let annotated_expressions: std::collections::BTreeSet<ExprId> =
            semantics::item_expression_annotations(db, item)
                .into_iter()
                .map(|(id, _)| id)
                .collect();

        for (index, expression) in hir.expressions.iter().enumerate() {
            let ExpressionKind::Assign { target, value, .. } = &expression.kind else {
                continue;
            };
            let target_expression = hir.expression(*target);
            if !matches!(target_expression.kind, ExpressionKind::NameRef(_)) {
                continue;
            }
            let id = ExprId(index as u32);
            let is_root = hir.root == Some(id);
            // The item's annotation binds its top-level statement; nested
            // bindings are unannotated either way.
            if (is_root && annotated) || annotated_expressions.contains(&id) {
                continue;
            }

            let mut renderer = TypeRenderer::default();
            let label = if is_root {
                let Some(scheme) = &check.scheme else {
                    continue;
                };
                if !scheme_is_hintable(db, scheme) {
                    continue;
                }
                renderer.render_scheme(db, scheme)
            } else {
                let Some(ty) = check
                    .expression_types
                    .get(&id)
                    .or_else(|| check.expression_types.get(value))
                else {
                    continue;
                };
                if !is_hintable(db, *ty, matches!(ty.kind(db), TyKind::Function(_))) {
                    continue;
                }
                renderer.render(db, *ty)
            };
            hints.push(InlayHint {
                offset: target_expression.range.end() + item_offset,
                label: format!(": {label}"),
            });
        }
    }
    hints.sort_by_key(|hint| hint.offset);
    hints
}

/// The signature list of the call under the cursor. Most calls carry exactly
/// one entry; a call whose callee committed a candidate of a stub overload
/// set lists every declared candidate so the editor can page through them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureData>,
    /// Index into `signatures` of the committed overload (0 for
    /// single-signature calls).
    pub active_signature: usize,
}

/// A rendered call signature: the label, each parameter's byte span inside
/// it, and the parameter the cursor's argument targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureData {
    pub label: String,
    pub parameters: Vec<TextRange>,
    pub active_parameter: Option<usize>,
}

/// The inferred signature of the call under the cursor. The active parameter
/// follows R's argument matching: a named argument consumes the parameter it
/// names, a positional argument fills the first open positionally-fillable
/// slot (parameters after `...` match by name only).
pub fn signature_help(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<SignatureHelp> {
    let position = position_in_item(db, file, offset)?;
    let hir = item_hir(db, position.item).as_ref()?;
    let check = item_check(db, position.item)?;

    // Containing calls, smallest first; the innermost whose callee has a
    // function type wins, so a cursor inside `list(...)` inside `f(...)`
    // still shows `f`'s signature (call-site builtins have none).
    let mut calls: Vec<(TextRange, ExprId)> = hir
        .expressions
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| match &expression.kind {
            ExpressionKind::Call { .. }
                if !expression.range.is_empty()
                    && expression.range.start() <= position.relative
                    && position.relative <= expression.range.end() =>
            {
                Some((expression.range, ExprId(index as u32)))
            }
            _ => None,
        })
        .collect();
    calls.sort_by_key(|(range, id)| (range.len(), range.start(), id.0));

    let (callee, function, overloads, arguments) = calls.iter().find_map(|(_, id)| {
        let ExpressionKind::Call { callee, arguments } = &hir.expression(*id).kind else {
            return None;
        };
        let function =
            check
                .expression_types
                .get(callee)
                .and_then(|callee_ty| match callee_ty.kind(db) {
                    TyKind::Function(function) => Some(function.clone()),
                    _ => None,
                });
        let overloads = callee_name(hir, *callee)
            .and_then(|name| semantics::stubs::stubs(db)?.schemes.get(&name).cloned())
            .filter(|schemes| schemes.len() > 1);
        (function.is_some() || overloads.is_some())
            .then_some((*callee, function, overloads, arguments))
    })?;

    // A call to an overloaded stub name lists the whole declared set with the
    // committed candidate active — the one checked callee type would otherwise
    // show a single signature and hide the alternatives the name offers. The
    // list does not wait for a commitment: an incomplete call matches no
    // candidate at all, and that is exactly when a reader needs to see the
    // shapes on offer.
    if let Some(schemes) = overloads {
        let mut signatures: Vec<SignatureData> = schemes
            .iter()
            .map(|scheme| overload_signature(db, scheme, arguments, hir, position.relative))
            .collect();
        let selected = check.selected_overloads.get(&callee).copied().unwrap_or(0);
        // The committed candidate is shown with this call site's types filled
        // in (`fn(x: list[integer] | integer[], ...)`, not `<T> fn(x: list[T] |
        // T[], ...)`): the checked callee type is that candidate instantiated,
        // and a reader comparing the alternatives wants the one in force to
        // read as their own call.
        if let Some(function) = &function
            && let Some(entry) = signatures.get_mut(selected)
        {
            let mut renderer = TypeRenderer::default();
            let (label, parameters) = render_signature(db, &mut renderer, String::new(), function);
            entry.active_parameter = active_parameter(
                db,
                function,
                parameters.len(),
                arguments,
                hir,
                position.relative,
            );
            entry.label = label;
            entry.parameters = parameters;
        }
        return Some(SignatureHelp {
            active_signature: selected.min(signatures.len().saturating_sub(1)),
            signatures,
        });
    }

    let function = &function?;
    let mut renderer = TypeRenderer::default();
    let (label, parameters) = render_signature(db, &mut renderer, String::new(), function);
    let active_parameter = active_parameter(
        db,
        function,
        parameters.len(),
        arguments,
        hir,
        position.relative,
    );
    Some(SignatureHelp {
        signatures: vec![SignatureData {
            label,
            parameters,
            active_parameter,
        }],
        active_signature: 0,
    })
}

/// The callee's referenced name, when the callee is a name (`sum(...)`,
/// `base::sum(...)`).
fn callee_name(hir: &semantics::hir::Module, callee: ExprId) -> Option<String> {
    match &hir.expression(callee).kind {
        ExpressionKind::NameRef(name) => Some(name.clone()),
        ExpressionKind::Namespace { name, .. } => name.clone(),
        _ => None,
    }
}

/// One overload candidate rendered for the signature list. The scheme's own
/// binders drive the label's `<T: atomic>` prefix; a non-function declaration
/// (possible in a hand-written override set) still occupies its slot so the
/// committed index stays aligned with the declared set.
fn overload_signature(
    db: &dyn Db,
    scheme: &TypeScheme<'_>,
    arguments: &[Argument],
    hir: &semantics::hir::Module,
    position: TextSize,
) -> SignatureData {
    let mut renderer = TypeRenderer::default();
    match scheme.body.kind(db) {
        TyKind::Function(function) => {
            let mut prefix = renderer
                .render_binder_prefix(&scheme.binders)
                .unwrap_or_default();
            if !prefix.is_empty() {
                prefix.push(' ');
            }
            let (label, parameters) = render_signature(db, &mut renderer, prefix, function);
            let active_parameter =
                active_parameter(db, function, parameters.len(), arguments, hir, position);
            SignatureData {
                label,
                parameters,
                active_parameter,
            }
        }
        _ => SignatureData {
            label: renderer.render_scheme(db, scheme),
            parameters: Vec::new(),
            active_parameter: None,
        },
    }
}

/// The candidate cap; the result marks itself incomplete past it so clients
/// re-query as the prefix narrows instead of filtering a truncated list.
pub const COMPLETION_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionKind {
    Keyword,
    Variable,
    Function,
    Field,
}

/// Where an item came from; the variant order is the ranking order among
/// items of equal match quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionSource {
    Keyword,
    Local,
    Global,
    Stdlib,
    Field,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    pub source: CompletionSource,
    /// The rendered scheme for standard-library entries.
    pub detail: Option<String>,
    pub documentation: Option<String>,
    /// For functions with a known signature: whether it declares any
    /// parameter. Drives the editor's call-snippet cursor placement.
    pub takes_arguments: Option<bool>,
}

/// Whether a scheme is a function that declares at least one parameter;
/// `None` for non-functions.
fn scheme_takes_arguments(db: &dyn Db, scheme: &TypeScheme<'_>) -> Option<bool> {
    match scheme.body.kind(db) {
        TyKind::Function(function) => Some(
            !function.positional.is_empty()
                || !function.named.is_empty()
                || function.variadic.is_some(),
        ),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResult {
    pub items: Vec<CompletionItem>,
    pub is_incomplete: bool,
}

const RESERVED_WORDS: &[&str] = &[
    "if",
    "else",
    "repeat",
    "while",
    "function",
    "for",
    "in",
    "next",
    "break",
    "TRUE",
    "FALSE",
    "NULL",
    "Inf",
    "NaN",
    "NA",
    "NA_integer_",
    "NA_real_",
    "NA_complex_",
    "NA_character_",
];

pub fn completion(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<CompletionResult> {
    let text = file.text(db);
    let (context, query) = completion_context(text, offset)?;

    // A cursor inside a `#:` annotation completes type names, not R values.
    {
        let parse = semantics::parse(db, file);
        if parse.syntax_node().descendants().any(|node| {
            node.kind() == syntax::SyntaxKind::ANNOTATION
                && node.text_range().start() <= offset
                && offset <= node.text_range().end()
        }) {
            return annotation_completion(db, files, file, &query);
        }
        // A cursor inside a string completes typed record fields when the
        // string subscripts a record (`x[["…"]]`) and is otherwise silent —
        // R value names never resolve inside string content.
        if let Some(string_token) = parse
            .syntax_node()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| {
                token.kind() == syntax::SyntaxKind::STRING
                    && token.text_range().start() < offset
                    && offset < token.text_range().end()
            })
        {
            return string_subscript_completion(db, file, offset, &string_token);
        }
    }

    let items = match context {
        CompletionContext::Field => spelled_completions(
            db,
            files,
            &query,
            syntax::SyntaxKind::AT_EXPR,
            CompletionSource::Field,
        ),
        CompletionContext::Item => dollar_completions(db, files, file, offset, &query),
        CompletionContext::Namespace { package } => {
            namespace_completions(db, files, &package, &query)
        }
        CompletionContext::MaybeNamespace => return None,
        CompletionContext::Default => {
            let mut items = Vec::new();
            // Keywords are a small fixed set, prefix-completed: no one
            // searches for `function` by typing `con`.
            for keyword in RESERVED_WORDS {
                if prefix_under_case(keyword, &query, query_is_case_sensitive(&query)) {
                    items.push(CompletionItem {
                        label: (*keyword).to_owned(),
                        kind: CompletionKind::Keyword,
                        source: CompletionSource::Keyword,
                        detail: None,
                        documentation: None,
                        takes_arguments: None,
                    });
                }
            }
            if let Some(position) = position_in_item(db, file, offset)
                && let Some(naming) = item_naming(db, position.item)
                && let Some(item_node) = item_node(db, position.item)
            {
                let item_offset = item_node.text_range().start();
                for info in naming.bindings.values() {
                    if info.kind == BindingKind::TopLevel {
                        continue;
                    }
                    // R scoping is function-granular: a local is visible at
                    // the cursor only when its owning function (the innermost
                    // FUNCTION_DEF enclosing its definition site) encloses
                    // the cursor too — enclosing frames stay visible inside
                    // closures, sibling closures' locals do not leak.
                    if !binding_visible_at(&item_node, info.range.start() + item_offset, offset) {
                        continue;
                    }
                    if search_match(&info.name, &query).is_some() {
                        items.push(CompletionItem {
                            label: info.name.clone(),
                            kind: CompletionKind::Variable,
                            source: CompletionSource::Local,
                            detail: None,
                            documentation: None,
                            takes_arguments: None,
                        });
                    }
                }
            }
            for &project_file in files.files(db) {
                for &item in item_tree(db, project_file) {
                    let kind = match *item.kind(db) {
                        ItemKind::Function => CompletionKind::Function,
                        ItemKind::Value => CompletionKind::Variable,
                        // A statement item still creates top-level variable
                        // slots (a conditional write inside a top-level loop
                        // or `if`), and those names complete like any global.
                        ItemKind::Statement => {
                            if let Some(naming) = item_naming(db, item) {
                                for info in naming.bindings.values() {
                                    if info.kind == BindingKind::TopLevel
                                        && search_match(&info.name, &query).is_some()
                                    {
                                        items.push(CompletionItem {
                                            label: info.name.clone(),
                                            kind: CompletionKind::Variable,
                                            source: CompletionSource::Global,
                                            detail: None,
                                            documentation: None,
                                            takes_arguments: None,
                                        });
                                    }
                                }
                            }
                            continue;
                        }
                    };
                    let Some(name) = item.name(db).clone() else {
                        continue;
                    };
                    if search_match(&name, &query).is_some() {
                        let takes_arguments =
                            scheme_takes_arguments(db, &semantics::global_scheme(db, item));
                        items.push(CompletionItem {
                            label: name,
                            kind,
                            source: CompletionSource::Global,
                            detail: None,
                            documentation: None,
                            takes_arguments,
                        });
                    }
                }
            }
            // The standard-library corpus, with each stub's scheme as the
            // detail. A project global of the same name outranks its stub at
            // the deduplication step, mirroring how resolution shadows.
            if let Some(library) = semantics::stubs::stubs(db) {
                for (name, schemes) in &library.schemes {
                    if search_match(name, &query).is_none() {
                        continue;
                    }
                    let Some(scheme) = schemes.first() else {
                        continue;
                    };
                    let mut renderer = TypeRenderer::default();
                    let namespace = library
                        .exports_by_namespace
                        .iter()
                        .find(|(_, names)| names.contains(name))
                        .map(|(namespace, _)| namespace.clone());
                    items.push(CompletionItem {
                        label: name.clone(),
                        kind: match scheme.body.kind(db) {
                            TyKind::Function(_) => CompletionKind::Function,
                            _ => CompletionKind::Variable,
                        },
                        source: CompletionSource::Stdlib,
                        detail: Some(renderer.render_scheme(db, scheme)),
                        documentation: namespace
                            .map(|namespace| format!("From the `{namespace}` package.")),
                        takes_arguments: scheme_takes_arguments(db, scheme),
                    });
                }
                // Manifest-only exports complete too — untyped, so no
                // detail, and without a scheme the value/function
                // distinction is unknowable statically. Non-syntactic
                // names (replacement functions, operators) are skipped: a
                // bare reference to them needs backticks, so inserting the
                // raw name would produce different syntax entirely.
                for name in &library.known_exports {
                    if library.schemes.contains_key(name)
                        || !syntax::is_syntactic_name(name)
                        || search_match(name, &query).is_none()
                    {
                        continue;
                    }
                    let namespace = library
                        .exports_by_namespace
                        .iter()
                        .find(|(_, names)| names.contains(name))
                        .map(|(namespace, _)| namespace.clone());
                    items.push(CompletionItem {
                        label: name.clone(),
                        kind: CompletionKind::Variable,
                        source: CompletionSource::Stdlib,
                        detail: None,
                        documentation: namespace
                            .map(|namespace| format!("From the `{namespace}` package.")),
                        takes_arguments: None,
                    });
                }
            }
            items
        }
    };

    finish_completions(items, &query)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeActionKind {
    /// Replace `@if-unknown` with `@trust` — the directive's one upgrade
    /// path once the value's type IS determined.
    IfUnknownToTrust,
    RemoveUnusedAssignment,
    /// Prefix the written name with `.` to keep it (dot-names are exempt
    /// from the unused check).
    PrefixDot,
    InsertInferredAnnotation,
    /// The whole-file source action covering every eligible binding.
    AddMissingAnnotations,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: TextRange,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    pub kind: CodeActionKind,
    pub edits: Vec<TextEdit>,
}

/// The static quickfixes and source actions for a document range: rewriting
/// `@if-unknown` to `@trust`, removing or dot-prefixing an unused
/// assignment, and inserting an inferred `#:` annotation above an
/// unannotated binding (the text the inlay hint shows). All edits are
/// computed eagerly — no resolve round-trip.
pub fn code_actions(db: &dyn Db, file: SourceFile, viewport: TextRange) -> Vec<CodeAction> {
    let text = file.text(db);
    let mut actions = Vec::new();

    let parse = semantics::parse(db, file);
    for annotation in parse
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        if !ranges_overlap(annotation.text_range(), viewport) {
            continue;
        }
        for directive in annotation
            .descendants()
            .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION_DIRECTIVE)
        {
            if let Some((name, range)) = directive_name_range(&directive)
                && name == "if-unknown"
            {
                actions.push(CodeAction {
                    title: "Replace `@if-unknown` with `@trust`".to_owned(),
                    kind: CodeActionKind::IfUnknownToTrust,
                    edits: vec![TextEdit {
                        range,
                        replacement: "@trust".to_owned(),
                    }],
                });
            }
        }
    }

    let mut all_annotation_edits = Vec::new();
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        let item_offset = node.text_range().start();
        let Some(naming) = item_naming(db, item) else {
            continue;
        };
        let Some(hir) = item_hir(db, item) else {
            continue;
        };

        for unused in &naming.unused_assignments {
            let target_range = unused.range + item_offset;
            if !ranges_overlap(target_range, viewport) {
                continue;
            }
            let Some(assign_range) = hir.expressions.iter().find_map(|expression| {
                let ExpressionKind::Assign { target, .. } = &expression.kind else {
                    return None;
                };
                (hir.expression(*target).range == unused.range || expression.range == unused.range)
                    .then_some(trim_trailing_trivia(text, expression.range + item_offset))
            }) else {
                continue;
            };
            actions.push(CodeAction {
                title: format!("Remove unused assignment of `{}`", unused.name),
                kind: CodeActionKind::RemoveUnusedAssignment,
                edits: vec![TextEdit {
                    range: removal_range(text, assign_range),
                    replacement: String::new(),
                }],
            });
            actions.push(CodeAction {
                title: format!("Prefix `{}` with `.` to keep it", unused.name),
                kind: CodeActionKind::PrefixDot,
                edits: vec![TextEdit {
                    range: TextRange::empty(target_range.start()),
                    replacement: ".".to_owned(),
                }],
            });
        }

        let Some(check) = item_check(db, item) else {
            continue;
        };
        let annotated = item_annotation_syntax(db, item).is_some_and(|annotation| {
            semantics::annotations::block_refusal(&annotation.syntax_node()).is_none()
        });
        for (index, expression) in hir.expressions.iter().enumerate() {
            let ExpressionKind::Assign { target, value, .. } = &expression.kind else {
                continue;
            };
            let target_expression = hir.expression(*target);
            let ExpressionKind::NameRef(name) = &target_expression.kind else {
                continue;
            };
            let id = ExprId(index as u32);
            let is_root = hir.root == Some(id);
            if is_root && annotated {
                continue;
            }
            let start = usize::from(expression.range.start() + item_offset);
            let line_start = text[..start.min(text.len())]
                .rfind('\n')
                .map_or(0, |at| at + 1);
            if !text[line_start..start].trim().is_empty() {
                continue;
            }

            let mut renderer = TypeRenderer::default();
            let rendered = if is_root {
                let Some(scheme) = &check.scheme else {
                    continue;
                };
                if !scheme_is_hintable(db, scheme) {
                    continue;
                }
                renderer.render_scheme(db, scheme)
            } else {
                let Some(ty) = check
                    .expression_types
                    .get(&id)
                    .or_else(|| check.expression_types.get(value))
                else {
                    continue;
                };
                if !is_hintable(db, *ty, matches!(ty.kind(db), TyKind::Function(_))) {
                    continue;
                }
                renderer.render(db, *ty)
            };
            let indentation = &text[line_start..start];
            let edit = TextEdit {
                range: TextRange::empty(TextSize::from(line_start as u32)),
                replacement: format!("{indentation}#: {rendered}\n"),
            };
            all_annotation_edits.push(edit.clone());
            if ranges_overlap(expression.range + item_offset, viewport) {
                actions.push(CodeAction {
                    title: format!("Add inferred type annotation for `{name}`"),
                    kind: CodeActionKind::InsertInferredAnnotation,
                    edits: vec![edit],
                });
            }
        }
    }
    if !all_annotation_edits.is_empty() {
        actions.push(CodeAction {
            title: "Add inferred type annotations for the whole file".to_owned(),
            kind: CodeActionKind::AddMissingAnnotations,
            edits: all_annotation_edits,
        });
    }

    actions
}

fn ranges_overlap(left: TextRange, right: TextRange) -> bool {
    left.start() <= right.end() && right.start() <= left.end()
}

/// The range removing an assignment deletes: the whole line(s) — trailing
/// newline included — when the assignment is the only content on them,
/// otherwise exactly the assignment's own range.
fn removal_range(text: &str, range: TextRange) -> TextRange {
    let start = usize::from(range.start()).min(text.len());
    let end = usize::from(range.end()).min(text.len());
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at);
    let alone = text[line_start..start].trim().is_empty() && text[end..line_end].trim().is_empty();
    if !alone {
        return range;
    }
    let with_newline = if line_end < text.len() {
        line_end + 1
    } else {
        line_end
    };
    TextRange::new(
        TextSize::from(line_start as u32),
        TextSize::from(with_newline as u32),
    )
}

/// The `@` + joined directive name span (`@if-unknown` spans the `@` through
/// `unknown`), for rewrites that replace the directive keyword.
fn directive_name_range(directive: &syntax::SyntaxNode) -> Option<(String, TextRange)> {
    let name = directive_name(directive)?;
    let at = directive
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == syntax::SyntaxKind::AT)?;
    let start = at.text_range().start();
    let end = start + TextSize::from(1 + name.len() as u32);
    Some((name, TextRange::new(start, end)))
}

/// Goto type definition: when the expression under the cursor has a named
/// (nominal or alias) type, the `@type`/`@alias` declaration of that name.
pub fn type_definition(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<DefinitionTarget> {
    let position = position_in_item(db, file, offset)?;
    let check = item_check(db, position.item)?;
    let (_, ty) = position
        .expressions_at()
        .into_iter()
        .find_map(|id| check.expression_types.get(&id).map(|ty| (id, *ty)))?;
    let TyKind::Named(name, _) = ty.kind(db) else {
        return None;
    };
    let name = name.text(db).to_owned();
    type_name_occurrences(db, files, &name)
        .into_iter()
        .rfind(|occurrence| occurrence.is_declaration)
        .map(|occurrence| {
            DefinitionTarget::Project(NavigationTarget {
                file: occurrence.file,
                range: occurrence.range,
            })
        })
        .or_else(|| stub_target(db, &name).map(DefinitionTarget::Stub))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentSymbolKind {
    Function,
    Value,
    /// A `#: @type` declaration.
    TypeDefinition,
    /// A `#: @alias` declaration.
    AliasDefinition,
    /// Declared by `setClass`.
    S4Class,
    /// Declared by `setGeneric`.
    S4Generic,
    /// Declared by `setMethod`; the signature classes render as the detail.
    S4Method,
    /// An `R6Class(...)` definition; its members are the children.
    R6Class,
    /// A function-valued `public`/`private` R6 member.
    R6Method,
    /// A non-function R6 member, or an `active` binding.
    R6Field,
}

/// One outline entry: the whole construct's range plus the name's own range
/// (the editor highlights the selection range when jumping).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: DocumentSymbolKind,
    /// `fn(parameters)` for functions, the `@type`/`@alias` spelling for
    /// type declarations, the signature classes for S4 methods.
    pub detail: Option<String>,
    pub range: TextRange,
    pub selection: TextRange,
    /// R6 class members; empty for every other kind.
    pub children: Vec<DocumentSymbol>,
}

/// Document symbols: the file's named top-level definitions (S4/R6
/// declarations recognized structurally, R6 members as children) plus its
/// `@type`/`@alias` declarations (invisible to the item tree — they live in
/// `#:` comments), in source order.
pub fn document_symbols(db: &dyn Db, file: SourceFile) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        let range = node.text_range();
        match *item.kind(db) {
            ItemKind::Function | ItemKind::Value => {
                let Some(name) = item.name(db).clone() else {
                    continue;
                };
                // A registration-call item (`setGeneric("name", ...)` is a
                // named Function item without an assignment) presents as its
                // S4 construct, selecting the string name like the bare
                // statement form always did.
                if node.kind() == syntax::SyntaxKind::CALL_EXPR {
                    let Some(construct) = classify_symbol_call(&node) else {
                        continue;
                    };
                    let Some((name, selection)) = construct.name else {
                        continue;
                    };
                    symbols.push(DocumentSymbol {
                        name,
                        kind: construct.kind,
                        detail: construct.detail,
                        range,
                        selection,
                        children: construct.children,
                    });
                    continue;
                }
                let selection = declared_name_range(db, item, &node).unwrap_or(range);
                // An assigned S4/R6 construction (the call is the assigned
                // value, a direct child of the assignment) keeps the assigned
                // name but takes the construct's kind, detail, and members.
                let construct = node
                    .children()
                    .find(|child| child.kind() == syntax::SyntaxKind::CALL_EXPR)
                    .and_then(|call| classify_symbol_call(&call));
                let (kind, detail, children) = match construct {
                    Some(construct) => (construct.kind, construct.detail, construct.children),
                    None => {
                        let kind = match *item.kind(db) {
                            ItemKind::Function => DocumentSymbolKind::Function,
                            _ => DocumentSymbolKind::Value,
                        };
                        (kind, function_detail(&node), Vec::new())
                    }
                };
                symbols.push(DocumentSymbol {
                    name,
                    kind,
                    detail,
                    range,
                    selection,
                    children,
                });
            }
            // A bare `setClass(...)`-style statement names itself through its
            // string argument.
            ItemKind::Statement => {
                if node.kind() != syntax::SyntaxKind::CALL_EXPR {
                    continue;
                }
                let Some(construct) = classify_symbol_call(&node) else {
                    continue;
                };
                let Some((name, selection)) = construct.name else {
                    continue;
                };
                symbols.push(DocumentSymbol {
                    name,
                    kind: construct.kind,
                    detail: construct.detail,
                    range,
                    selection,
                    children: construct.children,
                });
            }
        }
    }
    let parse = semantics::parse(db, file);
    for annotation in parse
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
    {
        for directive in annotation
            .descendants()
            .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION_DIRECTIVE)
        {
            let (kind, detail) = match directive_name(&directive).as_deref() {
                Some("type") => (DocumentSymbolKind::TypeDefinition, "@type"),
                Some("alias") => (DocumentSymbolKind::AliasDefinition, "@alias"),
                _ => continue,
            };
            let Some(name_token) = directive
                .children()
                .find(|child| child.kind() == syntax::SyntaxKind::NAME)
                .and_then(|name| {
                    name.children_with_tokens()
                        .filter_map(|element| element.into_token())
                        .find(|token| token.kind() == syntax::SyntaxKind::IDENT)
                })
            else {
                continue;
            };
            symbols.push(DocumentSymbol {
                name: name_token.text().to_owned(),
                kind,
                detail: Some(detail.to_owned()),
                range: directive.text_range(),
                selection: name_token.text_range(),
                children: Vec::new(),
            });
        }
    }
    symbols.sort_by_key(|symbol| (symbol.range.start(), symbol.range.end()));
    symbols
}

/// A recognized S4/R6 construction call's outline contribution. `name` is
/// the construct's own string-literal name (with its selection range) for
/// bare statements; assigned constructions keep the assigned name instead.
struct SymbolCall {
    kind: DocumentSymbolKind,
    name: Option<(String, TextRange)>,
    detail: Option<String>,
    children: Vec<DocumentSymbol>,
}

fn classify_symbol_call(call: &syntax::SyntaxNode) -> Option<SymbolCall> {
    let callee = s4_callee_name(call)?;
    let arguments = call
        .children()
        .find(|child| child.kind() == syntax::SyntaxKind::ARGUMENT_LIST)?;
    match callee.as_str() {
        "setClass" => Some(SymbolCall {
            kind: DocumentSymbolKind::S4Class,
            name: string_argument_content(s4_argument(&arguments, "Class", 0)),
            detail: None,
            children: Vec::new(),
        }),
        "setGeneric" => Some(SymbolCall {
            kind: DocumentSymbolKind::S4Generic,
            name: string_argument_content(s4_argument(&arguments, "name", 0)),
            detail: None,
            children: Vec::new(),
        }),
        "setMethod" => {
            let signature = s4_argument(&arguments, "signature", 1)
                .map(|signature| {
                    let classes: Vec<String> = s4_signature_strings(&signature)
                        .into_iter()
                        .filter_map(|class| string_argument_content(Some(class)))
                        .map(|(name, _)| name)
                        .collect();
                    if classes.is_empty() {
                        "Unknown".to_owned()
                    } else {
                        classes.join(", ")
                    }
                })
                .unwrap_or_else(|| "Unknown".to_owned());
            Some(SymbolCall {
                kind: DocumentSymbolKind::S4Method,
                name: string_argument_content(s4_argument(&arguments, "f", 0)),
                detail: Some(signature),
                children: Vec::new(),
            })
        }
        "R6Class" => Some(SymbolCall {
            kind: DocumentSymbolKind::R6Class,
            name: string_argument_content(s4_argument(&arguments, "classname", 0)),
            detail: None,
            children: r6_members(&arguments),
        }),
        _ => None,
    }
}

/// The members of an `R6Class` call's `public`/`private`/`active` lists:
/// function values are methods (fields for `active` bindings), everything
/// else a field.
fn r6_members(arguments: &syntax::SyntaxNode) -> Vec<DocumentSymbol> {
    let mut members = Vec::new();
    for (field, position) in [("public", 1), ("private", 2), ("active", 3)] {
        let Some(list) = s4_argument(arguments, field, position) else {
            continue;
        };
        if list.kind() != syntax::SyntaxKind::CALL_EXPR {
            continue;
        }
        let Some(list_arguments) = list
            .children()
            .find(|child| child.kind() == syntax::SyntaxKind::ARGUMENT_LIST)
        else {
            continue;
        };
        for member in list_arguments
            .children()
            .filter(|child| child.kind() == syntax::SyntaxKind::ARGUMENT)
        {
            let Some(name) = member
                .children()
                .find(|child| child.kind() == syntax::SyntaxKind::NAME)
            else {
                continue;
            };
            let value = member
                .children()
                .find(|child| child.kind() != syntax::SyntaxKind::NAME);
            let is_function = value
                .as_ref()
                .is_some_and(|value| value.kind() == syntax::SyntaxKind::FUNCTION_DEF);
            let kind = if is_function && field != "active" {
                DocumentSymbolKind::R6Method
            } else {
                DocumentSymbolKind::R6Field
            };
            let detail = value
                .as_ref()
                .filter(|_| is_function)
                .and_then(function_detail);
            members.push(DocumentSymbol {
                name: name.text().to_string(),
                kind,
                detail,
                range: member.text_range(),
                selection: name.text_range(),
                children: Vec::new(),
            });
        }
    }
    members
}

/// `fn(parameter, ...)` from the first function definition inside `node`.
fn function_detail(node: &syntax::SyntaxNode) -> Option<String> {
    let function = if node.kind() == syntax::SyntaxKind::FUNCTION_DEF {
        node.clone()
    } else {
        node.descendants()
            .find(|child| child.kind() == syntax::SyntaxKind::FUNCTION_DEF)?
    };
    let parameters = function
        .children()
        .find(|child| child.kind() == syntax::SyntaxKind::PARAMETER_LIST)?;
    let names: Vec<String> = parameters
        .children()
        .filter(|child| child.kind() == syntax::SyntaxKind::PARAMETER)
        .filter_map(|parameter| {
            parameter
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| {
                    matches!(
                        token.kind(),
                        syntax::SyntaxKind::IDENT | syntax::SyntaxKind::DOTS
                    )
                })
                .map(|token| token.text().to_owned())
        })
        .collect();
    Some(format!("fn({})", names.join(", ")))
}

/// The content text and range (inside the quotes) of a string-literal node.
fn string_argument_content(node: Option<syntax::SyntaxNode>) -> Option<(String, TextRange)> {
    let token = node?
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == syntax::SyntaxKind::STRING)?;
    let text = token.text();
    if text.len() < 2 || !(text.starts_with('"') || text.starts_with('\'')) {
        return None;
    }
    let name = text[1..text.len() - 1].to_string();
    if name.is_empty() {
        return None;
    }
    let range = token.text_range();
    Some((
        name,
        TextRange::new(
            range.start() + TextSize::from(1),
            range.end() - TextSize::from(1),
        ),
    ))
}

/// One workspace-symbol match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub kind: DocumentSymbolKind,
    pub target: NavigationTarget,
}

/// Workspace symbols: every file's named definitions (R6 members included)
/// matched and ranked by the shared smart-case matcher, capped like
/// completion.
pub fn workspace_symbols(db: &dyn Db, files: ProjectFiles, query: &str) -> Vec<WorkspaceSymbol> {
    let mut symbols: Vec<(MatchScore, WorkspaceSymbol)> = Vec::new();
    for &file in files.files(db) {
        for symbol in document_symbols(db, file) {
            for entry in std::iter::once(&symbol).chain(&symbol.children) {
                if let Some(score) = search_match(&entry.name, query) {
                    symbols.push((
                        score,
                        WorkspaceSymbol {
                            name: entry.name.clone(),
                            kind: entry.kind,
                            target: NavigationTarget {
                                file,
                                range: entry.selection,
                            },
                        },
                    ));
                }
            }
        }
    }
    symbols.sort_by(|(left_score, left), (right_score, right)| {
        (left_score, left.name.to_lowercase(), &left.name).cmp(&(
            right_score,
            right.name.to_lowercase(),
            &right.name,
        ))
    });
    symbols.truncate(COMPLETION_LIMIT);
    symbols.into_iter().map(|(_, symbol)| symbol).collect()
}

// ---- annotation type names ----

/// A type-name token under the cursor inside a `#:` annotation.
struct AnnotationTypeCursor {
    name: String,
    range: TextRange,
    /// Whether the token can resolve to a project `@type`/`@alias`
    /// declaration: binder-shadowed names, binder declarations, and `fn`
    /// cannot — they still hover, showing themselves.
    navigable: bool,
}

/// The type-name token at the cursor: a `TYPE_REF`'s identifier, a
/// `TYPE_APPLY`'s name, the declared name of an `@type`/`@alias` directive,
/// a `<T>` binder name, or the `fn` keyword of a function type.
fn annotation_type_at(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
) -> Option<AnnotationTypeCursor> {
    let parse = semantics::parse(db, file);
    let root = parse.syntax_node();
    let annotation = root.descendants().find(|node| {
        node.kind() == syntax::SyntaxKind::ANNOTATION
            && node.text_range().start() <= offset
            && offset <= node.text_range().end()
    })?;

    let token = annotation
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| {
            matches!(
                token.kind(),
                syntax::SyntaxKind::IDENT | syntax::SyntaxKind::NULL_KW
            ) && token.text_range().start() <= offset
                && offset <= token.text_range().end()
        })?;
    let name = token.text().to_string();
    let range = token.text_range();

    let parent = token.parent()?;
    let (is_type_position, resolvable) = match parent.kind() {
        syntax::SyntaxKind::TYPE_REF => (true, token.kind() == syntax::SyntaxKind::IDENT),
        syntax::SyntaxKind::NAME => match parent.parent().map(|grandparent| grandparent.kind()) {
            Some(syntax::SyntaxKind::TYPE_APPLY) => (true, true),
            Some(syntax::SyntaxKind::ANNOTATION_DIRECTIVE) => (
                parent.parent().is_some_and(|grandparent| {
                    matches!(
                        directive_name(&grandparent).as_deref(),
                        Some("type" | "alias")
                    )
                }),
                true,
            ),
            // Structural keywords (`fn`, `list`, a `<T>` binder) hover as
            // themselves.
            Some(other) if type_kind(other) => (true, false),
            _ => (false, false),
        },
        // `NULL` and other bare tokens directly inside a type node.
        other if type_kind(other) => (true, false),
        _ => (false, false),
    };
    if !is_type_position {
        return None;
    }
    let navigable = resolvable && !binder_names(&annotation).contains(&name);
    Some(AnnotationTypeCursor {
        name,
        range,
        navigable,
    })
}

// ---- S4 navigation ----
//
// S4 class/generic/method names are written as string literals inside
// `setClass` / `setGeneric` / `setMethod` / `new` calls, invisible to the
// naming analysis, so they are recovered structurally from the tree — one
// recognizer shared by goto-definition, references, and rename.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum S4Kind {
    /// Declared by `setClass`, referenced by `setMethod` signatures and `new`.
    Class,
    /// Declared by `setGeneric`, referenced by `setMethod`'s function name.
    Generic,
}

struct S4Occurrence {
    name: String,
    kind: S4Kind,
    /// The string CONTENT range (inside the quotes), absolute.
    range: TextRange,
    is_declaration: bool,
}

/// Every S4 name occurrence in one file, in tree order.
fn s4_occurrences_in(db: &dyn Db, file: SourceFile) -> Vec<S4Occurrence> {
    let parse = semantics::parse(db, file);
    let mut occurrences = Vec::new();
    for call in parse
        .syntax_node()
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::CALL_EXPR)
    {
        let Some(callee) = s4_callee_name(&call) else {
            continue;
        };
        let Some(arguments) = call
            .children()
            .find(|child| child.kind() == syntax::SyntaxKind::ARGUMENT_LIST)
        else {
            continue;
        };
        match callee.as_str() {
            "setClass" => push_s4_string(
                s4_argument(&arguments, "Class", 0),
                S4Kind::Class,
                true,
                &mut occurrences,
            ),
            "setGeneric" => push_s4_string(
                s4_argument(&arguments, "name", 0),
                S4Kind::Generic,
                true,
                &mut occurrences,
            ),
            "setMethod" => {
                push_s4_string(
                    s4_argument(&arguments, "f", 0),
                    S4Kind::Generic,
                    false,
                    &mut occurrences,
                );
                // The signature is a class name or a `c(...)` of class names.
                if let Some(signature) = s4_argument(&arguments, "signature", 1) {
                    for class_string in s4_signature_strings(&signature) {
                        push_s4_string(Some(class_string), S4Kind::Class, false, &mut occurrences);
                    }
                }
            }
            "new" => push_s4_string(
                s4_argument(&arguments, "Class", 0),
                S4Kind::Class,
                false,
                &mut occurrences,
            ),
            _ => {}
        }
    }
    occurrences
}

/// The bare callee name of a call: `f(...)` or `pkg::f(...)`.
fn s4_callee_name(call: &syntax::SyntaxNode) -> Option<String> {
    let callee = call.children().next()?;
    match callee.kind() {
        syntax::SyntaxKind::NAME => Some(callee.text().to_string()),
        syntax::SyntaxKind::NAMESPACE_EXPR => callee
            .children()
            .filter(|child| child.kind() == syntax::SyntaxKind::NAME)
            .last()
            .map(|name| name.text().to_string()),
        _ => None,
    }
}

/// Resolves a call argument's value by name, falling back to the positional
/// slot at `index` when unnamed — R's own argument-matching shape.
fn s4_argument(
    arguments: &syntax::SyntaxNode,
    name: &str,
    index: usize,
) -> Option<syntax::SyntaxNode> {
    let all: Vec<syntax::SyntaxNode> = arguments
        .children()
        .filter(|child| child.kind() == syntax::SyntaxKind::ARGUMENT)
        .collect();
    for argument in &all {
        if let Some(argument_name) = argument
            .children()
            .find(|child| child.kind() == syntax::SyntaxKind::NAME)
            && argument_name.text() == name
        {
            return argument
                .children()
                .find(|child| child.kind() != syntax::SyntaxKind::NAME);
        }
    }
    all.get(index)
        .filter(|argument| {
            // Positional: no `name =` tag. A NAME child followed by EQ is
            // the tag; a bare NAME child is the value itself.
            !argument
                .children_with_tokens()
                .any(|element| element.kind() == syntax::SyntaxKind::EQ)
        })
        .and_then(|argument| argument.children().next())
}

/// The class-name strings of a `setMethod` signature: a single string, or
/// the string elements of a `c(...)` vector.
fn s4_signature_strings(signature: &syntax::SyntaxNode) -> Vec<syntax::SyntaxNode> {
    if is_string_literal(signature) {
        return vec![signature.clone()];
    }
    if signature.kind() == syntax::SyntaxKind::CALL_EXPR
        && let Some(arguments) = signature
            .children()
            .find(|child| child.kind() == syntax::SyntaxKind::ARGUMENT_LIST)
    {
        return arguments
            .children()
            .filter(|child| child.kind() == syntax::SyntaxKind::ARGUMENT)
            .filter_map(|argument| argument.children().next())
            .filter(is_string_literal)
            .collect();
    }
    Vec::new()
}

fn is_string_literal(node: &syntax::SyntaxNode) -> bool {
    node.kind() == syntax::SyntaxKind::LITERAL
        && node
            .children_with_tokens()
            .any(|element| element.kind() == syntax::SyntaxKind::STRING)
}

fn push_s4_string(
    node: Option<syntax::SyntaxNode>,
    kind: S4Kind,
    is_declaration: bool,
    out: &mut Vec<S4Occurrence>,
) {
    // The content between plain quotes; raw strings never carry S4 names.
    let Some((name, range)) = string_argument_content(node) else {
        return;
    };
    out.push(S4Occurrence {
        name,
        kind,
        range,
        is_declaration,
    });
}

/// Whether a node kind belongs to the annotation type grammar.
fn type_kind(kind: syntax::SyntaxKind) -> bool {
    matches!(
        kind,
        syntax::SyntaxKind::TYPE_REF
            | syntax::SyntaxKind::TYPE_APPLY
            | syntax::SyntaxKind::TYPE_VECTOR
            | syntax::SyntaxKind::TYPE_UNION
            | syntax::SyntaxKind::TYPE_FUNCTION
            | syntax::SyntaxKind::TYPE_RECORD
            | syntax::SyntaxKind::TYPE_TUPLE
            | syntax::SyntaxKind::TYPE_LIST
            | syntax::SyntaxKind::TYPE_PAREN
            | syntax::SyntaxKind::TYPE_BINDER_LIST
            | syntax::SyntaxKind::TYPE_BINDER
            | syntax::SyntaxKind::TYPE_PARAMETER_LIST
            | syntax::SyntaxKind::TYPE_ARG_LIST
    )
}

/// The directive's joined name (`type`, `alias`, `if-unknown`, …): adjacent
/// name-ish tokens right after the `@`.
fn directive_name(directive: &syntax::SyntaxNode) -> Option<String> {
    let mut name = String::new();
    let mut end: Option<TextSize> = None;
    for element in directive.children_with_tokens() {
        let Some(token) = element.as_token() else {
            break;
        };
        match token.kind() {
            syntax::SyntaxKind::AT => {
                end = Some(token.text_range().end());
            }
            syntax::SyntaxKind::WHITESPACE | syntax::SyntaxKind::NEWLINE => break,
            // Only tokens touching the previous one join the name
            // (`@if-unknown` lexes as `if` `-` `unknown`).
            _ if end.is_some() && Some(token.text_range().start()) == end => {
                name.push_str(token.text());
                end = Some(token.text_range().end());
            }
            _ => break,
        }
    }
    (!name.is_empty()).then_some(name)
}

/// The `<T>` binder names declared anywhere in one annotation region.
fn binder_names(annotation: &syntax::SyntaxNode) -> Vec<String> {
    annotation
        .descendants()
        .filter(|node| node.kind() == syntax::SyntaxKind::TYPE_BINDER)
        .filter_map(|binder| {
            binder
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.kind() == syntax::SyntaxKind::IDENT)
                .map(|token| token.text().to_string())
        })
        .collect()
}

fn annotation_type_hover(
    db: &dyn Db,
    file: SourceFile,
    cursor: &AnnotationTypeCursor,
) -> Option<Hover> {
    // Resolve against the file's own declarations (matching how the
    // checker's winner table folds files). A builtin or undeclared name has
    // no definition to expand — show the name itself, confirming what the
    // cursor is on.
    let definition = cursor
        .navigable
        .then(|| {
            semantics::file_type_definitions(db, file)
                .into_iter()
                .find(|definition| definition.name.text(db) == cursor.name)
        })
        .flatten();
    let project_declared = definition.is_some();
    let line = match definition {
        Some(definition) => {
            let mut renderer = TypeRenderer::default();
            let parameters = if definition.parameters.is_empty() {
                String::new()
            } else {
                let names: Vec<String> = definition
                    .parameters
                    .iter()
                    .map(|parameter| parameter.text(db).to_owned())
                    .collect();
                format!("<{}>", names.join(", "))
            };
            let keyword = if definition.alias { "@alias" } else { "@type" };
            format!(
                "{keyword} {}{parameters} {{{}}}",
                cursor.name,
                renderer.render(db, definition.body)
            )
        }
        None => cursor.name.clone(),
    };
    // A name the project does not declare but the stub corpus does (a
    // nominal like `data.table`) hovers with its declaring package and the
    // declaration site, mirroring the expression path's stub summaries.
    let stub_definition = if !project_declared && cursor.navigable {
        semantics::stubs::stubs(db)
            .is_some_and(|library| library.nominals.contains(&cursor.name))
            .then(|| {
                semantics::stubs::declaring_namespace(db, &cursor.name).map(|namespace| {
                    HoverDefinition::Stub {
                        namespace: namespace.to_owned(),
                        overloads: 0,
                        declaration: stub_target(db, &cursor.name),
                    }
                })
            })
            .flatten()
    } else {
        None
    };
    Some(Hover {
        range: cursor.range,
        lines: vec![line],
        definition: stub_definition,
    })
}

/// The declaration summary for a cursor anywhere inside an `@type`/`@alias`
/// declaration (the `@`, the keyword, the braces), spanning the
/// declaration's own lines — a stitched block may hold several.
fn annotation_definition_hover(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<Hover> {
    let parse = semantics::parse(db, file);
    let root = parse.syntax_node();
    let annotation = root.descendants().find(|node| {
        node.kind() == syntax::SyntaxKind::ANNOTATION
            && node.text_range().start() <= offset
            && offset <= node.text_range().end()
    })?;
    for directive in annotation.descendants().filter(|node| {
        node.kind() == syntax::SyntaxKind::ANNOTATION_DIRECTIVE
            && matches!(directive_name(node).as_deref(), Some("type" | "alias"))
    }) {
        // The declaration's display range starts at its line's `#:` marker.
        let start = annotation
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| {
                token.kind() == syntax::SyntaxKind::ANNOTATION_MARKER
                    && token.text_range().start() <= directive.text_range().start()
            })
            .map(|token| token.text_range().start())
            .last()
            .unwrap_or_else(|| directive.text_range().start());
        let range = TextRange::new(start, directive.text_range().end());
        if !(range.start() <= offset && offset <= range.end()) {
            continue;
        }
        let Some(name) = directive
            .children()
            .find(|child| child.kind() == syntax::SyntaxKind::NAME)
            .map(|name| name.text().to_string())
        else {
            continue;
        };
        let cursor = AnnotationTypeCursor {
            name,
            range,
            navigable: true,
        };
        return annotation_type_hover(db, file, &cursor);
    }
    None
}

/// Every occurrence of a type name across the project's annotations:
/// declarations (`@type`/`@alias` names) and uses (`TYPE_REF` identifiers,
/// `TYPE_APPLY` names), skipping annotations where a binder shadows it.
fn type_name_occurrences(db: &dyn Db, files: ProjectFiles, name: &str) -> Vec<Occurrence> {
    let mut result = Vec::new();
    for &file in files.files(db) {
        let parse = semantics::parse(db, file);
        for annotation in parse
            .syntax_node()
            .descendants()
            .filter(|node| node.kind() == syntax::SyntaxKind::ANNOTATION)
        {
            let shadowed = binder_names(&annotation)
                .iter()
                .any(|binder| binder == name);
            for node in annotation.descendants() {
                let (token, is_declaration) = match node.kind() {
                    syntax::SyntaxKind::TYPE_REF => {
                        let Some(token) = node
                            .children_with_tokens()
                            .filter_map(|element| element.into_token())
                            .find(|token| token.kind() == syntax::SyntaxKind::IDENT)
                        else {
                            continue;
                        };
                        (token, false)
                    }
                    syntax::SyntaxKind::NAME => {
                        let Some(parent) = node.parent() else {
                            continue;
                        };
                        let is_declaration = match parent.kind() {
                            syntax::SyntaxKind::TYPE_APPLY => false,
                            syntax::SyntaxKind::ANNOTATION_DIRECTIVE => {
                                if !matches!(
                                    directive_name(&parent).as_deref(),
                                    Some("type" | "alias")
                                ) {
                                    continue;
                                }
                                true
                            }
                            _ => continue,
                        };
                        let Some(token) = node
                            .children_with_tokens()
                            .filter_map(|element| element.into_token())
                            .find(|token| token.kind() == syntax::SyntaxKind::IDENT)
                        else {
                            continue;
                        };
                        (token, is_declaration)
                    }
                    _ => continue,
                };
                if token.text() != name || (shadowed && !is_declaration) {
                    continue;
                }
                result.push(Occurrence {
                    file,
                    range: token.text_range(),
                    is_declaration,
                });
            }
        }
    }
    result
}

/// Record-field completion inside the string of `x[["…"]]`: the subscripted
/// value's typed fields matched against the content typed before the cursor.
fn string_subscript_completion(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
    string_token: &syntax::SyntaxToken,
) -> Option<CompletionResult> {
    let position = position_in_item(db, file, offset)?;
    let hir = item_hir(db, position.item).as_ref()?;
    let check = item_check(db, position.item)?;

    let string_range = string_token.text_range() - position.item_offset;
    let target = hir
        .expressions
        .iter()
        .filter_map(|expression| match &expression.kind {
            ExpressionKind::Index {
                double: true,
                target,
                arguments,
            } if arguments.iter().any(|argument| {
                argument
                    .value
                    .is_some_and(|value| hir.expression(value).range == string_range)
            }) =>
            {
                Some(*target)
            }
            _ => None,
        })
        .next()?;
    let ty = *check.expression_types.get(&target)?;
    let TyKind::Record(fields) = ty.kind(db) else {
        return None;
    };

    // The query is the content typed before the cursor, past the open quote.
    let text = string_token.text();
    let typed = usize::from(offset - string_token.text_range().start());
    let query = text.get(1..typed).unwrap_or_default();

    let mut renderer = TypeRenderer::default();
    let items: Vec<CompletionItem> = fields
        .iter()
        .filter(|field| search_match(field.name.text(db), query).is_some())
        .map(|field| CompletionItem {
            label: field.name.text(db).to_owned(),
            kind: CompletionKind::Field,
            source: CompletionSource::Field,
            detail: Some(renderer.render(db, field.ty)),
            documentation: None,
            takes_arguments: None,
        })
        .collect();
    finish_completions(items, query)
}

/// Type-name completion inside a `#:` annotation: the primitive vocabulary
/// plus the project's `@type`/`@alias` declarations.
fn annotation_completion(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    query: &str,
) -> Option<CompletionResult> {
    const PRIMITIVES: &[&str] = &[
        "logical",
        "integer",
        "double",
        "complex",
        "character",
        "raw",
        "list",
        "fn",
        "Any",
        "Unknown",
        "NULL",
    ];
    let mut items = Vec::new();
    for primitive in PRIMITIVES {
        if search_match(primitive, query).is_some() {
            items.push(CompletionItem {
                label: (*primitive).to_owned(),
                kind: CompletionKind::Keyword,
                source: CompletionSource::Keyword,
                detail: None,
                documentation: None,
                takes_arguments: None,
            });
        }
    }
    // The project table covers package files only; a script's own
    // declarations complete too.
    let mut definitions: Vec<(String, semantics::annotations::NamedDefinition<'_>)> =
        semantics::project_type_definitions(db, files)
            .iter()
            .map(|(name, definition)| (name.text(db).to_owned(), definition.clone()))
            .collect();
    for definition in semantics::file_type_definitions(db, file) {
        let label = definition.name.text(db).to_owned();
        if !definitions.iter().any(|(name, _)| *name == label) {
            definitions.push((label, definition));
        }
    }
    for (label, definition) in definitions {
        if search_match(&label, query).is_none() {
            continue;
        }
        let mut renderer = TypeRenderer::default();
        items.push(CompletionItem {
            label,
            kind: CompletionKind::Variable,
            source: CompletionSource::Global,
            detail: Some(renderer.render(db, definition.body)),
            documentation: None,
            takes_arguments: None,
        });
    }
    finish_completions(items, query)
}

// ---- completion internals ----

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionContext {
    Default,
    /// After `@`: S4 slot names.
    Field,
    /// After `$`: record fields of the extracted value.
    Item,
    /// After `::` / `:::`: the package's exports.
    Namespace {
        package: String,
    },
    /// A bare trailing `:` — the range operator or half a `::`; undecided,
    /// so stay silent.
    MaybeNamespace,
}

/// The completion context and query: the identifier chars immediately before
/// the cursor, and the operator (if any) they follow. Works on raw text so
/// completion still fires mid-edit inside broken code.
/// Whether a binding defined at `binding_start` is in scope at `cursor`,
/// by R's function-granular scoping: the binding's innermost enclosing
/// `FUNCTION_DEF` must contain the cursor (a binding outside any function is
/// item-visible).
fn binding_visible_at(
    item_node: &syntax::SyntaxNode,
    binding_start: TextSize,
    cursor: TextSize,
) -> bool {
    if !item_node
        .text_range()
        .contains_range(TextRange::empty(binding_start))
    {
        return true;
    }
    let covering = item_node.covering_element(TextRange::empty(binding_start));
    let covering_node = match covering {
        syntax::SyntaxElement::Node(node) => node,
        syntax::SyntaxElement::Token(token) => token.parent().unwrap_or_else(|| item_node.clone()),
    };
    let owning_function = covering_node
        .ancestors()
        .find(|ancestor| ancestor.kind() == syntax::SyntaxKind::FUNCTION_DEF);
    match owning_function {
        Some(function) => function.text_range().contains(cursor),
        None => true,
    }
}

fn completion_context(text: &str, offset: TextSize) -> Option<(CompletionContext, String)> {
    let at = usize::from(offset).min(text.len());
    let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let prefix = &text[line_start..at];

    let mut context = CompletionContext::Default;
    let mut query = String::new();
    let mut word_start_of_previous: Option<usize> = None;
    let mut current_word_start = 0usize;
    let mut previous_char: Option<char> = None;
    for (index, character) in prefix.char_indices() {
        if character.is_alphabetic()
            || character == '.'
            || character == '_'
            || (!query.is_empty() && character.is_numeric())
        {
            if query.is_empty() {
                current_word_start = index;
            }
            query.push(character);
            // A name after a single `:` is the range operator's operand
            // (`1:n`), not a pending namespace access.
            if context == CompletionContext::MaybeNamespace {
                context = CompletionContext::Default;
            }
        } else {
            if !query.is_empty() {
                word_start_of_previous = Some(current_word_start);
            }
            context = match character {
                '@' => CompletionContext::Field,
                '$' => CompletionContext::Item,
                ':' => {
                    if previous_char == Some(':') {
                        let package = word_start_of_previous
                            .map(|start| {
                                prefix[start..]
                                    .chars()
                                    .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
                                    .collect::<String>()
                            })
                            .unwrap_or_default();
                        CompletionContext::Namespace { package }
                    } else {
                        CompletionContext::MaybeNamespace
                    }
                }
                _ => CompletionContext::Default,
            };
            query.clear();
        }
        previous_char = Some(character);
    }
    Some((context, query))
}

/// Names spelled after the given extract operator anywhere in the project —
/// the syntactic fallback shared by `@` completion and untyped `$` targets.
fn spelled_completions(
    db: &dyn Db,
    files: ProjectFiles,
    query: &str,
    kind: syntax::SyntaxKind,
    source: CompletionSource,
) -> Vec<CompletionItem> {
    let mut labels = std::collections::BTreeSet::new();
    for &project_file in files.files(db) {
        let parse = semantics::parse(db, project_file);
        for node in parse.syntax_node().descendants() {
            if node.kind() != kind {
                continue;
            }
            // The rhs name: the LAST name child (the lhs is a child
            // expression node, so a bare NAME child is the field).
            if let Some(name) = node
                .children()
                .filter(|child| child.kind() == syntax::SyntaxKind::NAME)
                .last()
            {
                labels.insert(name.text().to_string());
            }
        }
    }
    labels
        .into_iter()
        .filter(|label| search_match(label, query).is_some())
        .map(|label| CompletionItem {
            label,
            kind: CompletionKind::Field,
            source,
            detail: None,
            documentation: None,
            takes_arguments: None,
        })
        .collect()
}

/// `$` completion: the record fields of the target's checked type, falling
/// back to every `$name` spelled in the project when the target's type gives
/// no fields.
fn dollar_completions(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
    query: &str,
) -> Vec<CompletionItem> {
    let typed = (|| {
        let position = position_in_item(db, file, offset)?;
        let hir = item_hir(db, position.item).as_ref()?;
        let check = item_check(db, position.item)?;
        // The innermost field access containing the cursor; its target's
        // record fields are the candidates.
        let target = hir
            .expressions
            .iter()
            .filter_map(|expression| match &expression.kind {
                ExpressionKind::Field { target, .. }
                    if !expression.range.is_empty()
                        && expression.range.start() <= position.relative
                        && position.relative <= expression.range.end() =>
                {
                    Some((expression.range, *target))
                }
                _ => None,
            })
            .min_by_key(|(range, _)| range.len())
            .map(|(_, target)| target)?;
        let ty = *check.expression_types.get(&target)?;
        let TyKind::Record(fields) = ty.kind(db) else {
            return None;
        };
        let mut renderer = TypeRenderer::default();
        Some(
            fields
                .iter()
                .map(|field| {
                    // A non-syntactic field name is only insertable after `$`
                    // in its backtick-quoted spelling.
                    let name = field.name.text(db);
                    let label = if syntax::is_syntactic_name(name) {
                        name.to_owned()
                    } else {
                        format!("`{name}`")
                    };
                    CompletionItem {
                        label,
                        kind: CompletionKind::Field,
                        source: CompletionSource::Field,
                        detail: Some(renderer.render(db, field.ty)),
                        documentation: None,
                        takes_arguments: None,
                    }
                })
                .filter(|item| search_match(&item.label, query).is_some())
                .collect::<Vec<_>>(),
        )
    })();
    match typed {
        Some(items) if !items.is_empty() => items,
        _ => spelled_completions(
            db,
            files,
            query,
            syntax::SyntaxKind::DOLLAR_EXPR,
            CompletionSource::Field,
        ),
    }
}

/// `pkg::` completion: the namespace's declared exports when the stub corpus
/// knows the package, otherwise every name spelled after `::` in the project.
fn namespace_completions(
    db: &dyn Db,
    files: ProjectFiles,
    package: &str,
    query: &str,
) -> Vec<CompletionItem> {
    if let Some(library) = semantics::stubs::stubs(db)
        && let Some(exports) = library.exports_by_namespace.get(package)
    {
        let mut items: Vec<CompletionItem> = exports
            .iter()
            .filter(|name| search_match(name, query).is_some())
            .map(|name| {
                let mut renderer = TypeRenderer::default();
                let (kind, detail, takes_arguments) =
                    match library.schemes.get(name).and_then(|s| s.first()) {
                        Some(scheme) => (
                            match scheme.body.kind(db) {
                                TyKind::Function(_) => CompletionKind::Function,
                                _ => CompletionKind::Variable,
                            },
                            Some(renderer.render_scheme(db, scheme)),
                            scheme_takes_arguments(db, scheme),
                        ),
                        None => (CompletionKind::Variable, None, None),
                    };
                // A non-syntactic export (a replacement function, an
                // operator) is only referable after `::` in its
                // backtick-quoted spelling.
                let label = if syntax::is_syntactic_name(name) {
                    name.clone()
                } else {
                    format!("`{name}`")
                };
                CompletionItem {
                    label,
                    kind,
                    source: CompletionSource::Stdlib,
                    detail,
                    documentation: None,
                    takes_arguments,
                }
            })
            .collect();
        items.sort_by(|left, right| left.label.cmp(&right.label));
        return items;
    }
    spelled_completions(
        db,
        files,
        query,
        syntax::SyntaxKind::NAMESPACE_EXPR,
        CompletionSource::Global,
    )
}

/// Dedupe by label, rank by match quality then source/label, cap at
/// `COMPLETION_LIMIT`.
fn finish_completions(items: Vec<CompletionItem>, query: &str) -> Option<CompletionResult> {
    let mut seen = std::collections::BTreeSet::new();
    let mut deduplicated = Vec::new();
    for item in items {
        if seen.insert(item.label.clone()) {
            deduplicated.push(item);
        }
    }
    deduplicated.sort_by(|left, right| {
        (
            search_match(&left.label, query),
            left.source,
            left.label.to_lowercase(),
            left.label.clone(),
            left.kind,
        )
            .cmp(&(
                search_match(&right.label, query),
                right.source,
                right.label.to_lowercase(),
                right.label.clone(),
                right.kind,
            ))
    });
    let is_incomplete = deduplicated.len() > COMPLETION_LIMIT;
    deduplicated.truncate(COMPLETION_LIMIT);
    (!deduplicated.is_empty()).then_some(CompletionResult {
        items: deduplicated,
        is_incomplete,
    })
}

// ---- search matching (shared by completion and symbol search) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchScore {
    tier: u8,
    first_match_index: u32,
}

const TIER_EXACT: u8 = 0;
const TIER_PREFIX: u8 = 1;
const TIER_SUBSTRING: u8 = 2;
const TIER_SUBSEQUENCE: u8 = 3;

/// Shortest query matched as a subsequence; shorter queries fall back to
/// prefix matching so one- or two-character inputs do not surface scattered,
/// low-signal matches (mirrors rust-analyzer).
const MIN_SUBSEQUENCE_QUERY_LEN: usize = 3;

/// Subsequence matching with smart case: every query character must appear
/// in the candidate in order; case-insensitive unless the query contains an
/// uppercase character. An empty query matches everything.
pub fn search_match(candidate: &str, query: &str) -> Option<MatchScore> {
    if query.is_empty() {
        return Some(MatchScore {
            tier: TIER_PREFIX,
            first_match_index: 0,
        });
    }

    let case_sensitive = query_is_case_sensitive(query);
    let equal = |left: char, right: char| {
        if case_sensitive {
            left == right
        } else {
            left.to_lowercase().eq(right.to_lowercase())
        }
    };

    let mut query_chars = query.chars().peekable();
    let mut first_match_index = None;
    for (index, candidate_char) in candidate.chars().enumerate() {
        let Some(&query_char) = query_chars.peek() else {
            break;
        };
        if equal(candidate_char, query_char) {
            if first_match_index.is_none() {
                first_match_index = Some(index as u32);
            }
            query_chars.next();
        }
    }
    if query_chars.peek().is_some() {
        return None;
    }

    let tier = if equal_under_case(candidate, query, case_sensitive) {
        TIER_EXACT
    } else if prefix_under_case(candidate, query, case_sensitive) {
        TIER_PREFIX
    } else if substring_under_case(candidate, query, case_sensitive) {
        TIER_SUBSTRING
    } else {
        TIER_SUBSEQUENCE
    };
    if query.chars().count() < MIN_SUBSEQUENCE_QUERY_LEN && tier > TIER_PREFIX {
        return None;
    }
    Some(MatchScore {
        tier,
        first_match_index: first_match_index.unwrap_or(0),
    })
}

fn query_is_case_sensitive(query: &str) -> bool {
    query.chars().any(|character| character.is_uppercase())
}

fn equal_under_case(candidate: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        candidate == query
    } else {
        candidate.eq_ignore_ascii_case(query)
    }
}

fn prefix_under_case(candidate: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        candidate.starts_with(query)
    } else {
        candidate.to_lowercase().starts_with(&query.to_lowercase())
    }
}

fn substring_under_case(candidate: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        candidate.contains(query)
    } else {
        candidate.to_lowercase().contains(&query.to_lowercase())
    }
}

// ---- signature help internals ----

/// The signature label with each parameter's byte span. `...` renders at its
/// formal position (after the variadic's preceding named parameters), so
/// span indexes line up with `active_parameter`'s display translation.
fn render_signature<'db>(
    db: &'db dyn Db,
    renderer: &mut TypeRenderer<'db>,
    prefix: String,
    function: &FunctionType<'db>,
) -> (String, Vec<TextRange>) {
    let mut parts: Vec<String> = Vec::new();
    for ty in &function.positional {
        parts.push(renderer.render(db, *ty));
    }
    for field in &function.named {
        let name = if field.optional {
            format!("[{}]", field.name.text(db))
        } else {
            field.name.text(db).to_owned()
        };
        parts.push(format!("{name}: {}", renderer.render(db, field.ty)));
    }
    if let Some(rest) = &function.variadic {
        let at = function.positional.len() + rest.preceding_named.min(function.named.len());
        parts.insert(at, format!("...: {}", renderer.render(db, rest.element)));
    }

    let mut label = prefix;
    label.push_str("fn(");
    let mut parameters = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            label.push_str(", ");
        }
        let start = TextSize::from(label.len() as u32);
        label.push_str(part);
        parameters.push(TextRange::new(start, TextSize::from(label.len() as u32)));
    }
    label.push_str(") -> ");
    label.push_str(&renderer.render(db, function.ret));
    (label, parameters)
}

/// The rendered parameter the cursor's argument targets (legacy algorithm:
/// matching works in slot space — positionals, then named, the rest slot
/// last — and translates to the display order, which interleaves `...` at
/// its formal position).
fn active_parameter(
    db: &dyn Db,
    function: &FunctionType<'_>,
    rendered_count: usize,
    arguments: &[Argument],
    hir: &semantics::hir::Module,
    cursor: TextSize,
) -> Option<usize> {
    if rendered_count == 0 {
        return None;
    }

    let positional_count = function.positional.len();
    let matchable_count = positional_count + function.named.len();
    let preceding_named = function
        .variadic
        .as_ref()
        .map(|variadic| variadic.preceding_named.min(function.named.len()));
    let variadic_slot = function.variadic.is_some().then_some(matchable_count);
    let display_index = |slot: usize| match preceding_named {
        Some(preceding) if slot == matchable_count => positional_count + preceding,
        Some(preceding) if slot >= positional_count + preceding => slot + 1,
        _ => slot,
    };
    let slot_for_name = |name: &str| {
        function
            .named
            .iter()
            .position(|field| field.name.text(db) == name)
            .map(|index| positional_count + index)
    };
    let positionally_fillable = |slot: usize| {
        slot < positional_count
            || match preceding_named {
                Some(preceding) => slot - positional_count < preceding,
                None => true,
            }
    };
    let first_open_slot = |consumed: &[bool]| {
        consumed
            .iter()
            .enumerate()
            .find(|(slot, taken)| !**taken && positionally_fillable(*slot))
            .map(|(slot, _)| slot)
    };

    // Every argument ending before the cursor is complete; the cursor sits on
    // the next one (possibly not written yet, right after a comma).
    let cursor_index = arguments
        .iter()
        .filter(|argument| {
            argument
                .value
                .is_some_and(|value| hir.expression(value).range.end() < cursor)
        })
        .count();

    let mut consumed = vec![false; matchable_count];
    for argument in arguments.iter().take(cursor_index) {
        match &argument.name {
            // A name matching no parameter is a named-argument error (named
            // arguments are never routed into `...`); it consumes no slot.
            Some(name) => {
                if let Some(slot) = slot_for_name(name) {
                    consumed[slot] = true;
                }
            }
            None => {
                if let Some(slot) = first_open_slot(&consumed) {
                    consumed[slot] = true;
                }
            }
        }
    }

    // The cursor's own named argument targets the parameter it names; a name
    // matching no declared parameter is absorbed by `...`.
    let named_target = arguments
        .get(cursor_index)
        .and_then(|argument| argument.name.as_deref())
        .and_then(|name| slot_for_name(name).or(variadic_slot));

    Some(
        named_target
            .or_else(|| first_open_slot(&consumed))
            .or(variadic_slot)
            .map(display_index)
            .unwrap_or(rendered_count - 1)
            .min(rendered_count - 1),
    )
}

// ---- inlay hint internals ----

/// Variables are presentable only when the hinted type is a function AT THE
/// TOP — the label generalizes them into binder names. A variable anywhere
/// else (including inside a function nested in a union) would render an
/// unanchored type parameter, so those types show nothing.
fn scheme_is_hintable(db: &dyn Db, scheme: &TypeScheme<'_>) -> bool {
    is_hintable(
        db,
        scheme.body,
        matches!(scheme.body.kind(db), TyKind::Function(_)),
    )
}

/// Whether every leaf of the type is presentable in a hint.
/// `variables_allowed` is true only under a function type, whose variables
/// the label generalizes into binder names.
fn is_hintable(db: &dyn Db, ty: Ty<'_>, variables_allowed: bool) -> bool {
    match ty.kind(db) {
        TyKind::Unknown => false,
        TyKind::Var(_) | TyKind::Rigid(_) => variables_allowed,
        TyKind::Any | TyKind::Null | TyKind::Scalar(_) => true,
        TyKind::Vector(element)
        | TyKind::NamedVector(element)
        | TyKind::List(element)
        | TyKind::NamedList(element) => is_hintable(db, *element, variables_allowed),
        TyKind::Tuple(items) => items
            .iter()
            .all(|&item| is_hintable(db, item, variables_allowed)),
        TyKind::Record(fields) => fields
            .iter()
            .all(|field| is_hintable(db, field.ty, variables_allowed)),
        TyKind::Union(members) => members
            .iter()
            .all(|&member| is_hintable(db, member, variables_allowed)),
        TyKind::Named(_, arguments) => arguments
            .iter()
            .all(|&argument| is_hintable(db, argument, variables_allowed)),
        TyKind::Function(function) => {
            function
                .positional
                .iter()
                .all(|&ty| is_hintable(db, ty, variables_allowed))
                && function
                    .named
                    .iter()
                    .all(|field| is_hintable(db, field.ty, variables_allowed))
                && function
                    .variadic
                    .as_ref()
                    .is_none_or(|rest| is_hintable(db, rest.element, variables_allowed))
                && is_hintable(db, function.ret, variables_allowed)
        }
    }
}

// ---- the occurrence engine ----

/// What the cursor's name resolves to.
enum Target<'db> {
    /// A variable slot inside one item (a local, parameter, or loop
    /// variable).
    Slot { item: Item<'db>, binding: BindingId },
    /// A project-defined global name: a top-level definition read across
    /// items and files.
    Global(String),
    /// A `@type`/`@alias` name inside `#:` annotations.
    TypeName(String),
    /// A name only the stub corpus declares: navigable into the corpus, but
    /// with no project occurrences (references stay empty, rename refuses).
    StubGlobal(String),
    /// An S4 class or generic named in a string literal.
    S4 { name: String, kind: S4Kind },
}

fn target_at<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<Target<'db>> {
    if let Some(cursor) = annotation_type_at(db, file, offset) {
        // Primitive type names have no declaration to navigate to; project
        // `@type`/`@alias` declarations and stub-declared nominals do.
        let declared = cursor.navigable
            && (type_name_occurrences(db, files, &cursor.name)
                .iter()
                .any(|occurrence| occurrence.is_declaration)
                || semantics::stubs::stubs(db)
                    .is_some_and(|library| library.nominals.contains(&cursor.name)));
        return declared.then_some(Target::TypeName(cursor.name));
    }

    let position = position_in_item(db, file, offset)?;
    let naming = item_naming(db, position.item).as_ref()?;

    if let Some(expression) = position.expression_at() {
        if let Some(binding) = naming.resolutions.get(&expression) {
            let info = naming.bindings.get(binding)?;
            // The item's own top-level binding IS the global: cross-item
            // reads resolve to the name, not the slot.
            if info.kind == BindingKind::TopLevel {
                return Some(Target::Global(info.name.clone()));
            }
            return Some(Target::Slot {
                item: position.item,
                binding: *binding,
            });
        }
        // A quiet read (data masking, an opaque operator) resolves like any
        // cross-item read for navigation — only the unresolved diagnostic is
        // withheld for it.
        if let Some(name) = naming
            .non_locals
            .get(&expression)
            .or_else(|| naming.quiet_reads.get(&expression))
        {
            // Project-defined names have occurrences to offer; a name only
            // the stub corpus declares navigates into the corpus (goto only —
            // no occurrences, so references stay empty and rename refuses).
            // Unresolved names resolve nowhere.
            if global_declaration_exists(db, files, name) {
                return Some(Target::Global(name.clone()));
            }
            if semantics::stubs::stub_declaration(db, name).is_some() {
                return Some(Target::StubGlobal(name.clone()));
            }
        }
    }

    // S4 class/generic names live in string literals, invisible to naming.
    if let Some(occurrence) = s4_occurrences_in(db, file)
        .into_iter()
        .find(|occurrence| occurrence.range.start() <= offset && offset <= occurrence.range.end())
    {
        return Some(Target::S4 {
            name: occurrence.name,
            kind: occurrence.kind,
        });
    }

    // The cursor sits on a declared name that the expression walk could not
    // answer for: a parameter or for-loop variable, which is a declaration
    // with no expression of its own, or a name whose neighbour won the
    // end-inclusive ranking — where two siblings abut, the expression ENDING
    // at the cursor is ranked ahead of the declaration STARTING there, and for
    // an unresolved neighbour it has nothing to offer. Reaching a declaration
    // from its own first character is what keeps goto-definition reciprocal
    // with references: a jump lands exactly there.
    //
    // Smallest range first (then binding id) so an item whose recovery left
    // two declarations overlapping still answers deterministically rather than
    // by hash order.
    let (binding, info) = naming
        .bindings
        .iter()
        .filter(|(_, info)| {
            info.range.start() <= position.relative && position.relative <= info.range.end()
        })
        .min_by_key(|(binding, info)| (info.range.len(), binding.0))?;
    if info.kind == BindingKind::TopLevel {
        return Some(Target::Global(info.name.clone()));
    }
    Some(Target::Slot {
        item: position.item,
        binding: *binding,
    })
}

/// Whether any item declares `name` as a top-level slot — named definitions
/// and conditional writes (a `for`-body assignment at the top level) alike,
/// matching the occurrence walk's notion of a declaration.
fn global_declaration_exists(db: &dyn Db, files: ProjectFiles, name: &str) -> bool {
    files.files(db).iter().any(|file| {
        item_tree(db, *file).iter().copied().any(|item| {
            item_naming(db, item).as_ref().is_some_and(|naming| {
                naming
                    .bindings
                    .values()
                    .any(|info| info.kind == BindingKind::TopLevel && info.name == name)
            })
        })
    })
}

fn occurrences(db: &dyn Db, files: ProjectFiles, target: &Target<'_>) -> Vec<Occurrence> {
    let mut result = Vec::new();
    match target {
        Target::StubGlobal(_) => {}
        Target::Slot { item, binding } => {
            if let Some(node) = item_node(db, *item) {
                slot_occurrences(
                    db,
                    *item,
                    node.text_range().start(),
                    *binding,
                    &mut |range, is_declaration| {
                        result.push(Occurrence {
                            file: *item.file(db),
                            range,
                            is_declaration,
                        });
                    },
                );
            }
        }
        Target::TypeName(name) => {
            result = type_name_occurrences(db, files, name);
        }
        Target::S4 { name, kind } => {
            for &file in files.files(db) {
                for occurrence in s4_occurrences_in(db, file) {
                    if occurrence.name == *name && occurrence.kind == *kind {
                        result.push(Occurrence {
                            file,
                            range: occurrence.range,
                            is_declaration: occurrence.is_declaration,
                        });
                    }
                }
            }
        }
        Target::Global(name) => {
            for &file in files.files(db) {
                for &item in item_tree(db, file) {
                    let Some(node) = item_node(db, item) else {
                        continue;
                    };
                    let item_offset = node.text_range().start();
                    // The defining item's own top-level slot carries the
                    // declaration target and any internal (recursive) reads.
                    if let Some(naming) = item_naming(db, item) {
                        for (id, info) in &naming.bindings {
                            if info.kind == BindingKind::TopLevel && info.name == *name {
                                slot_occurrences(
                                    db,
                                    item,
                                    item_offset,
                                    *id,
                                    &mut |range, is_declaration| {
                                        result.push(Occurrence {
                                            file,
                                            range,
                                            is_declaration,
                                        });
                                    },
                                );
                            }
                        }
                        // Reads that resolve outside the item — quiet
                        // (masked, opaque-operator) reads included: the
                        // global's uses from other definitions.
                        if let Some(hir) = item_hir(db, item) {
                            for (expression, non_local) in
                                naming.non_locals.iter().chain(&naming.quiet_reads)
                            {
                                if non_local == name {
                                    result.push(Occurrence {
                                        file,
                                        range: hir.expression(*expression).range + item_offset,
                                        is_declaration: false,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let file_order = |file: SourceFile| {
        files
            .files(db)
            .iter()
            .position(|candidate| *candidate == file)
    };
    result.sort_by_key(|occurrence| (file_order(occurrence.file), occurrence.range.start()));
    result.dedup_by_key(|occurrence| (occurrence.file, occurrence.range));
    result
}

/// Feeds every occurrence of one slot inside its item to `emit`: the
/// binding's own site (parameters and loop variables are not expressions, so
/// resolutions alone would miss their declaration), then every resolved read
/// and assignment target.
fn slot_occurrences(
    db: &dyn Db,
    item: Item<'_>,
    item_offset: TextSize,
    binding: BindingId,
    emit: &mut dyn FnMut(TextRange, bool),
) {
    let Some(naming) = item_naming(db, item) else {
        return;
    };
    let Some(hir) = item_hir(db, item) else {
        return;
    };
    let Some(info) = naming.bindings.get(&binding) else {
        return;
    };

    let mut ranges: Vec<(TextRange, bool)> = Vec::new();
    if matches!(info.kind, BindingKind::Parameter | BindingKind::ForVariable) {
        ranges.push((info.range + item_offset, true));
    }

    let targets = assignment_targets(hir);
    for (expression, resolved) in &naming.resolutions {
        if resolved == &binding {
            ranges.push((
                hir.expression(*expression).range + item_offset,
                targets.contains(expression),
            ));
        }
    }

    ranges.sort_by_key(|(range, _)| range.start());
    ranges.dedup_by_key(|(range, _)| *range);
    for (range, is_declaration) in ranges {
        emit(range, is_declaration);
    }
}

/// The set of expressions that are assignment targets — the write sites a
/// slot counts as declarations.
fn assignment_targets(hir: &semantics::hir::Module) -> std::collections::BTreeSet<ExprId> {
    let mut targets = std::collections::BTreeSet::new();
    for expression in &hir.expressions {
        if let ExpressionKind::Assign { target, .. } = &expression.kind {
            targets.insert(*target);
        }
    }
    targets
}

// ---- positions ----

/// The cursor's item and the item-relative cursor offset.
struct PositionedItem<'db> {
    db: &'db dyn Db,
    item: Item<'db>,
    item_offset: TextSize,
    relative: TextSize,
}

impl PositionedItem<'_> {
    /// The HIR expressions whose range contains the cursor, smallest first —
    /// end-inclusive, so a cursor sitting immediately after a name still hits
    /// it (the editor convention). Name references win ties.
    fn expressions_at(&self) -> Vec<ExprId> {
        let Some(hir) = item_hir(self.db, self.item) else {
            return Vec::new();
        };
        let mut containing: Vec<(TextSize, bool, ExprId)> = Vec::new();
        for (index, expression) in hir.expressions.iter().enumerate() {
            let range = expression.range;
            if range.is_empty() || !(range.start() <= self.relative && self.relative <= range.end())
            {
                continue;
            }
            containing.push((
                range.len(),
                !matches!(expression.kind, ExpressionKind::NameRef(_)),
                ExprId(index as u32),
            ));
        }
        containing.sort_by_key(|(width, not_name, id)| (*width, *not_name, id.0));
        containing.into_iter().map(|(_, _, id)| id).collect()
    }

    /// The smallest containing expression.
    fn expression_at(&self) -> Option<ExprId> {
        self.expressions_at().into_iter().next()
    }
}

fn position_in_item(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<PositionedItem<'_>> {
    // Strict containment wins; a cursor sitting exactly on an item's end
    // offset (typing at the end of the last statement) still belongs to it,
    // unless the next item starts there.
    let mut touching: Option<PositionedItem<'_>> = None;
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        let range = node.text_range();
        if range.start() <= offset && offset < range.end() {
            return Some(PositionedItem {
                db,
                item,
                item_offset: range.start(),
                relative: offset - range.start(),
            });
        }
        if offset == range.end() && touching.is_none() {
            touching = Some(PositionedItem {
                db,
                item,
                item_offset: range.start(),
                relative: offset - range.start(),
            });
        }
    }
    touching
}

/// The range of the name an item declares, for goto targets and outline
/// selections that land on the name rather than on the whole statement.
///
/// Naming owns which token that is, and asking it is the whole point: a syntax
/// scan for the item's first `NAME` node reads `name <- value` correctly and
/// every other shape wrong. A right assignment declares its name LAST, so
/// `compute(1) -> total` sent every jump to `total` into `compute` instead —
/// valid R, and the wrong file position. Items naming binds nothing for (an
/// S4 registration call) still fall back to the scan.
fn declared_name_range(
    db: &dyn Db,
    item: Item<'_>,
    node: &syntax::SyntaxNode,
) -> Option<TextRange> {
    let declared = item.name(db).as_deref();
    let bound = item_naming(db, item).as_ref().and_then(|naming| {
        naming
            .bindings
            .values()
            .filter(|info| {
                info.kind == BindingKind::TopLevel && Some(info.name.as_str()) == declared
            })
            .map(|info| info.range)
            .min_by_key(|range| (range.start(), range.len()))
    });
    match bound {
        Some(range) => Some(range + node.text_range().start()),
        None => node
            .descendants()
            .find(|descendant| descendant.kind() == syntax::SyntaxKind::NAME)
            .map(|name| name.text_range()),
    }
}
