//! Every feature is a pure read of the database at a byte offset; UTF-16 and protocol concerns
//! live in the server. Per-item query results carry item-relative ranges, while this API speaks
//! file-absolute offsets (`semantics::item_node` anchors an item).

use semantics::annotations::{block_refusal, statement_inline_site};
use semantics::check::ItemCheck;
use semantics::diagnostics::TypeRenderer;
use semantics::hir::{Argument, ExprId, ExpressionKind, Module};
use semantics::naming::{BindingId, BindingKind};
use semantics::stubs::{declaring_namespace, stub_declaration, stubs};
use semantics::types::{FunctionType, Ty, TyKind, TypeScheme, contains_inference_var};
use semantics::{
    Db, Item, ItemKind, ProjectFiles, SourceFile, file_type_definitions, global_scheme,
    item_annotation, item_annotation_syntax, item_check, item_expression_annotations, item_hir,
    item_name_range, item_naming, item_node, item_tree, package_definitions,
    project_type_definitions,
};
use std::collections::BTreeSet;
use std::iter::once;
use syntax::SyntaxKind::{self, *};
use syntax::kind::KEYWORDS;
use syntax::{
    SyntaxElement, SyntaxNode, SyntaxToken, TextRange, TextSize, is_inline_annotation,
    is_syntactic_name,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    pub range: TextRange,
    pub lines: Vec<String>,
    pub definition: Option<HoverDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverDefinition {
    /// `maybe_undefined` marks reads not dominated by a write.
    Local {
        target: NavigationTarget,
        maybe_undefined: bool,
    },
    /// The name's winning project top-level definition.
    Global { target: NavigationTarget },
    Stub {
        namespace: String,
        overloads: usize,
        declaration: Option<StubTarget>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSection {
    pub title: &'static str,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavigationTarget {
    pub file: SourceFile,
    pub range: TextRange,
}

/// Stub sources are not project files: the host maps `source_index` to a file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionTarget {
    Project(NavigationTarget),
    Stub(StubTarget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StubTarget {
    pub source_index: usize,
    pub range: TextRange,
}

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
    if let Some(cursor) = annotation_type_at(db, file, offset) {
        return Some(annotation_type_hover(db, file, &cursor));
    }
    if let Some(hover) = annotation_definition_hover(db, file, offset) {
        return Some(hover);
    }

    let position = position_in_item(db, file, offset)?;
    let check = item_check(db, position.item)?;
    let hir = item_hir(db, position.item).as_ref()?;
    // Write targets and operators record no type, so the hover widens to the enclosing typed
    // expression.
    let (expression, ty) = position
        .expressions_at()
        .into_iter()
        .find_map(|id| check.expression_types.get(&id).map(|ty| (id, *ty)))?;

    let (lines, definition) = match &hir.expression(expression).kind {
        ExpressionKind::NameRef(name) => {
            // The item's own top-level name renders the exported scheme, not the initializer's
            // type: `#: @new` brands the scheme even when the initializer is `Unknown`.
            let naming = item_naming(db, position.item).as_ref();
            let exported = naming.and_then(|naming| {
                let binding = naming.resolutions.get(&expression)?;
                (naming.bindings.get(binding)?.kind == BindingKind::TopLevel).then_some(())?;
                check.scheme.clone()
            });
            // An instantiated use has lost its binders and constraints; the scheme it
            // instantiated says what it accepts.
            let referenced = || {
                naming.filter(|naming| naming.non_locals.contains_key(&expression))?;
                contains_inference_var(db, ty).then_some(())?;
                referenced_scheme(db, files, name, &check, expression)
            };
            // Render the whole scheme so the `<T>` prefix explains `T`, matching the inlay hint.
            let scheme = exported
                .or_else(referenced)
                .unwrap_or_else(|| TypeScheme::monomorphic(ty));
            (
                hover_lines(db, name, &scheme),
                hover_definition(db, files, position.item, expression, name),
            )
        }
        _ => (vec![TypeRenderer::default().render(db, ty)], None),
    };

    Some(Hover {
        range: trim_trailing_trivia(
            file.text(db),
            hir.expression(expression).range + position.item_offset,
        ),
        lines,
        definition,
    })
}

fn hover_lines(db: &dyn Db, name: &str, scheme: &TypeScheme<'_>) -> Vec<String> {
    const WIDTH: usize = 80;
    let mut renderer = TypeRenderer::default();
    let line = format!("{name}: {}", renderer.render_scheme(db, scheme));
    let TyKind::Function(function) = scheme.body.kind(db) else {
        return vec![line];
    };
    if line.chars().count() <= WIDTH {
        return vec![line];
    }
    let mut renderer = TypeRenderer::default();
    let binders = renderer
        .render_binder_prefix(&scheme.binders)
        .map(|prefix| format!("{prefix} "))
        .unwrap_or_default();
    let (label, parameters) =
        render_signature(db, &mut renderer, format!("{name}: {binders}"), function);
    let (Some(first), Some(last)) = (parameters.first(), parameters.last()) else {
        return vec![line];
    };
    let mut lines = vec![label[..usize::from(first.start())].to_owned()];
    for parameter in &parameters {
        lines.push(format!(
            "  {},",
            &label[usize::from(parameter.start())..usize::from(parameter.end())]
        ));
    }
    lines.push(label[usize::from(last.end())..].to_owned());
    lines
}

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
    if !naming.non_locals.contains_key(&expression) {
        return None;
    }
    global_hover_definition(db, files, name, None).or_else(|| {
        Some(HoverDefinition::Stub {
            namespace: declaring_namespace(db, name)?.to_owned(),
            overloads: stubs(db)?.schemes.get(name).map_or(0, Vec::len),
            declaration: stub_target(db, name),
        })
    })
}

fn global_hover_definition(
    db: &dyn Db,
    files: ProjectFiles,
    name: &str,
    own_item: Option<Item<'_>>,
) -> Option<HoverDefinition> {
    let declaring = declaring_item(db, files, name).or(own_item)?;
    item_target(db, declaring).map(|target| HoverDefinition::Global { target })
}

fn item_target(db: &dyn Db, item: Item<'_>) -> Option<NavigationTarget> {
    let node = item_node(db, item)?;
    Some(NavigationTarget {
        file: *item.file(db),
        range: item_name_range(db, item).unwrap_or_else(|| node.text_range()),
    })
}

fn stub_target(db: &dyn Db, name: &str) -> Option<StubTarget> {
    stub_declaration(db, name).map(|declaration| StubTarget {
        source_index: declaration.source_index,
        range: declaration.range,
    })
}

fn declaring_item<'db>(db: &'db dyn Db, files: ProjectFiles, name: &str) -> Option<Item<'db>> {
    package_definitions(db, files)
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
}

/// Without a committed overload, the last stub candidate: by corpus convention the most general.
fn referenced_scheme<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    name: &str,
    check: &ItemCheck<'db>,
    reference: ExprId,
) -> Option<TypeScheme<'db>> {
    if let Some(item) = declaring_item(db, files, name) {
        return Some(global_scheme(db, item));
    }
    let candidates = stubs(db)?.schemes.get(name)?;
    let index = check
        .selected_overloads
        .get(&reference)
        .copied()
        .unwrap_or(candidates.len().saturating_sub(1));
    candidates.get(index).cloned()
}

pub fn hover_debug(db: &dyn Db, file: SourceFile, offset: TextSize) -> Vec<DebugSection> {
    let mut sections = Vec::new();
    if let Some(position) = position_in_item(db, file, offset)
        && let Some(hir) = item_hir(db, position.item)
        && let Some(expression) = position.expression_at()
    {
        sections.push(DebugSection {
            title: "Lowering",
            body: format!("{:#?}", hir.expression(expression)),
        });
        if let Some(naming) = item_naming(db, position.item) {
            let body = match naming.resolutions.get(&expression) {
                Some(binding) => match naming.bindings.get(binding) {
                    Some(info) => format!(
                        "slot `{}` ({:?}, declared at {:?})",
                        info.name, info.kind, info.range
                    ),
                    None => "slot (missing binding info)".to_owned(),
                },
                None => match naming.non_locals.get(&expression) {
                    Some(name) => format!("non-local `{name}`"),
                    None => "no resolution recorded".to_owned(),
                },
            };
            sections.push(DebugSection {
                title: "Naming",
                body,
            });
        }
    }
    if let Some(token) = semantics::parse(db, file)
        .syntax_node()
        .token_at_offset(offset)
        .right_biased()
    {
        let chain: Vec<String> = once((token.kind(), token.text_range()))
            .chain(
                token
                    .parent_ancestors()
                    .map(|node| (node.kind(), node.text_range())),
            )
            .map(|(kind, range)| format!("{kind:?}@{range:?}"))
            .collect();
        sections.push(DebugSection {
            title: "Parsing",
            body: chain.join("\n"),
        });
    }
    sections
}

fn trim_trailing_trivia(text: &str, range: TextRange) -> TextRange {
    let start = usize::from(range.start()).min(text.len());
    let end = usize::from(range.end()).min(text.len());
    let trimmed = text[start..end].trim_end().len();
    TextRange::new(range.start(), TextSize::from((start + trimmed) as u32))
}

pub fn definition(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<DefinitionTarget> {
    match target_at(db, files, file, offset)? {
        Target::Slot { item, binding } => {
            let info = item_naming(db, item).as_ref()?.bindings.get(&binding)?;
            let item_offset = item_node(db, item)?.text_range().start();
            Some(DefinitionTarget::Project(NavigationTarget {
                file,
                range: info.range + item_offset,
            }))
        }
        Target::TypeName(name) => type_name_definition(db, files, &name),
        Target::StubGlobal(name) => stub_target(db, &name).map(DefinitionTarget::Stub),
        target @ (Target::Global(_) | Target::S4 { .. }) => {
            if let Target::Global(name) = &target
                && let Some(winner) = package_definitions(db, files).get(name)
            {
                return item_target(db, *winner).map(DefinitionTarget::Project);
            }
            occurrences(db, files, &target)
                .into_iter()
                .find(|occurrence| occurrence.is_declaration)
                .map(declaration_target)
        }
    }
}

/// The last declaration wins, matching the project definition table's fold order.
fn type_name_definition(db: &dyn Db, files: ProjectFiles, name: &str) -> Option<DefinitionTarget> {
    type_name_occurrences(db, files, name)
        .into_iter()
        .rfind(|occurrence| occurrence.is_declaration)
        .map(declaration_target)
        .or_else(|| stub_target(db, name).map(DefinitionTarget::Stub))
}

fn declaration_target(occurrence: Occurrence) -> DefinitionTarget {
    DefinitionTarget::Project(NavigationTarget {
        file: occurrence.file,
        range: occurrence.range,
    })
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    pub offset: TextSize,
    pub label: String,
}

/// A function type is hinted when it contains no `Unknown`; any other type must be fully
/// concrete, so partially inferred values show nothing.
pub fn inlay_hints(db: &dyn Db, file: SourceFile, viewport: Option<TextRange>) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        if viewport.is_some_and(|viewport| !ranges_overlap(node.text_range(), viewport)) {
            continue;
        }
        let Some(hir) = item_hir(db, item) else {
            continue;
        };
        let Some(check) = item_check(db, item) else {
            continue;
        };
        // Only a surviving payload types the binding: refused, definitions-only, and toggle-only
        // blocks leave it hintable.
        let annotated = item_annotation(db, item).is_some_and(|annotation| {
            annotation.declared.is_some() || annotation.new_nominal.is_some() || annotation.trusted
        });
        // A hint on an annotated nested statement would restate the line above.
        let annotated_expressions: BTreeSet<ExprId> = item_expression_annotations(db, item)
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
            if (is_root && annotated) || annotated_expressions.contains(&id) {
                continue;
            }
            let Some(label) = assignment_label(db, &check, id, *value, is_root) else {
                continue;
            };
            hints.push(InlayHint {
                offset: target_expression.range.end() + node.text_range().start(),
                label: format!(": {label}"),
            });
        }
    }
    hints.sort_by_key(|hint| hint.offset);
    hints
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureData>,
    pub active_signature: usize,
}

/// `parameters` are byte spans inside `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureData {
    pub label: String,
    pub parameters: Vec<TextRange>,
    pub active_parameter: Option<usize>,
}

/// The active parameter follows R's argument matching: a named argument takes the parameter it
/// names, a positional one the first open slot (parameters after `...` match by name only).
pub fn signature_help(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<SignatureHelp> {
    let position = position_in_item(db, file, offset)?;
    let hir = item_hir(db, position.item).as_ref()?;
    let check = item_check(db, position.item)?;

    // The innermost call whose callee has a function type wins, so a cursor in `list(...)`
    // inside `f(...)` still shows `f`.
    let mut calls: Vec<(TextRange, ExprId)> = hir
        .expressions
        .iter()
        .enumerate()
        .filter(|(_, expression)| {
            matches!(expression.kind, ExpressionKind::Call { .. })
                && !expression.range.is_empty()
                && expression.range.contains_inclusive(position.relative)
        })
        .map(|(index, expression)| (expression.range, ExprId(index as u32)))
        .collect();
    calls.sort_by_key(|(range, id)| (range.len(), range.start(), id.0));

    let (callee, function, overloads, arguments) = calls.iter().find_map(|(_, id)| {
        let ExpressionKind::Call { callee, arguments } = &hir.expression(*id).kind else {
            return None;
        };
        let function = check
            .expression_types
            .get(callee)
            .copied()
            .filter(|ty| matches!(ty.kind(db), TyKind::Function(_)));
        let overloads = callee_name(hir, *callee)
            .and_then(|name| stubs(db)?.schemes.get(&name))
            .filter(|schemes| schemes.len() > 1);
        (function.is_some() || overloads.is_some())
            .then_some((*callee, function, overloads, arguments))
    })?;

    // An overloaded stub name lists every candidate, even before a commitment: an incomplete call
    // matches none, which is exactly when a reader needs the shapes on offer.
    if let Some(schemes) = overloads {
        let mut signatures: Vec<SignatureData> = schemes
            .iter()
            .map(|scheme| scheme_signature(db, scheme, arguments, hir, position.relative))
            .collect();
        let selected = check.selected_overloads.get(&callee).copied().unwrap_or(0);
        // The committed candidate renders instantiated with this call site's types.
        if let Some(function) = function
            && let Some(entry) = signatures.get_mut(selected)
        {
            *entry = scheme_signature(
                db,
                &TypeScheme::monomorphic(function),
                arguments,
                hir,
                position.relative,
            );
        }
        return Some(SignatureHelp {
            active_signature: selected.min(signatures.len().saturating_sub(1)),
            signatures,
        });
    }

    let function = function?;
    // An open callee renders the scheme it instantiates rather than variables nothing binds.
    let naming = item_naming(db, position.item).as_ref();
    let scheme = if let Some(name) = callee_name(hir, callee)
        && naming.is_some_and(|naming| naming.non_locals.contains_key(&callee))
        && contains_inference_var(db, function)
        && let Some(files) = ProjectFiles::try_get(db)
        && let Some(scheme) = referenced_scheme(db, files, &name, &check, callee)
    {
        scheme
    } else {
        TypeScheme::monomorphic(function)
    };
    Some(SignatureHelp {
        signatures: vec![scheme_signature(
            db,
            &scheme,
            arguments,
            hir,
            position.relative,
        )],
        active_signature: 0,
    })
}

fn callee_name(hir: &Module, callee: ExprId) -> Option<String> {
    match &hir.expression(callee).kind {
        ExpressionKind::NameRef(name) => Some(name.clone()),
        ExpressionKind::Namespace { name, .. } => name.clone(),
        _ => None,
    }
}

/// A non-function declaration still occupies its slot, so the committed index stays aligned
/// with the declared set.
fn scheme_signature(
    db: &dyn Db,
    scheme: &TypeScheme<'_>,
    arguments: &[Argument],
    hir: &Module,
    position: TextSize,
) -> SignatureData {
    let mut renderer = TypeRenderer::default();
    let TyKind::Function(function) = scheme.body.kind(db) else {
        return SignatureData {
            label: renderer.render_scheme(db, scheme),
            parameters: Vec::new(),
            active_parameter: None,
        };
    };
    let prefix = renderer
        .render_binder_prefix(&scheme.binders)
        .map(|prefix| format!("{prefix} "))
        .unwrap_or_default();
    let (label, parameters) = render_signature(db, &mut renderer, prefix, function);
    let active_parameter =
        active_parameter(db, function, parameters.len(), arguments, hir, position);
    SignatureData {
        label,
        parameters,
        active_parameter,
    }
}

/// Past the cap the result is marked incomplete, so clients re-query as the prefix narrows.
pub const COMPLETION_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionKind {
    Keyword,
    Variable,
    Function,
    Field,
}

/// Variant order is the ranking order among items of equal match quality.
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
    pub detail: Option<String>,
    pub documentation: Option<String>,
    /// `None` for non-functions; drives the editor's call-snippet cursor placement.
    pub takes_arguments: Option<bool>,
}

impl CompletionItem {
    fn new(label: String, kind: CompletionKind, source: CompletionSource) -> Self {
        Self {
            label,
            kind,
            source,
            detail: None,
            documentation: None,
            takes_arguments: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResult {
    pub items: Vec<CompletionItem>,
    pub is_incomplete: bool,
}

pub fn completion(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<CompletionResult> {
    let (context, query) = completion_context(file.text(db), offset)?;
    let root = semantics::parse(db, file).syntax_node();
    if annotation_at(&root, offset).is_some() || at_inline_type_start(&root, offset) {
        return annotation_completion(db, files, file, &query);
    }
    // R value names never resolve inside string content; only record-field subscripts complete
    // there.
    if let Some(string_token) = root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| {
            token.kind() == STRING
                && token.text_range().start() < offset
                && offset < token.text_range().end()
        })
    {
        return string_subscript_completion(db, file, offset, &string_token);
    }

    let items = match context {
        CompletionContext::Field => {
            spelled_completions(db, files, &query, AT_EXPR, CompletionSource::Field)
        }
        CompletionContext::Item => dollar_completions(db, files, file, offset, &query),
        CompletionContext::Namespace { package } => {
            namespace_completions(db, files, &package, &query)
        }
        CompletionContext::MaybeNamespace => return None,
        CompletionContext::Default => {
            let case_sensitive = query_is_case_sensitive(&query);
            // Keywords are prefix-completed only: no one searches for `function` by typing `con`.
            let mut items: Vec<CompletionItem> = KEYWORDS
                .iter()
                .filter(|(keyword, _)| prefix_under_case(keyword, &query, case_sensitive))
                .map(|(keyword, _)| {
                    CompletionItem::new(
                        (*keyword).to_owned(),
                        CompletionKind::Keyword,
                        CompletionSource::Keyword,
                    )
                })
                .collect();
            if let Some(position) = position_in_item(db, file, offset)
                && let Some(naming) = item_naming(db, position.item)
                && let Some(node) = item_node(db, position.item)
            {
                let item_offset = node.text_range().start();
                items.extend(
                    naming
                        .bindings
                        .values()
                        .filter(|info| {
                            info.kind != BindingKind::TopLevel
                                && binding_visible_at(
                                    &node,
                                    info.range.start() + item_offset,
                                    offset,
                                )
                                && search_match(&info.name, &query).is_some()
                        })
                        .map(|info| {
                            CompletionItem::new(
                                info.name.clone(),
                                CompletionKind::Variable,
                                CompletionSource::Local,
                            )
                        }),
                );
            }
            for &project_file in files.files(db) {
                for &item in item_tree(db, project_file) {
                    let kind = match *item.kind(db) {
                        ItemKind::Function => CompletionKind::Function,
                        ItemKind::Value => CompletionKind::Variable,
                        // A statement item still creates top-level slots (a conditional write
                        // inside a top-level `if`).
                        ItemKind::Statement => {
                            if let Some(naming) = item_naming(db, item) {
                                items.extend(
                                    naming
                                        .bindings
                                        .values()
                                        .filter(|info| {
                                            info.kind == BindingKind::TopLevel
                                                && search_match(&info.name, &query).is_some()
                                        })
                                        .map(|info| {
                                            CompletionItem::new(
                                                info.name.clone(),
                                                CompletionKind::Variable,
                                                CompletionSource::Global,
                                            )
                                        }),
                                );
                            }
                            continue;
                        }
                    };
                    if let Some(name) = item.name(db)
                        && search_match(name, &query).is_some()
                    {
                        items.push(CompletionItem {
                            takes_arguments: scheme_takes_arguments(db, &global_scheme(db, item)),
                            ..CompletionItem::new(name.clone(), kind, CompletionSource::Global)
                        });
                    }
                }
            }
            // A project global outranks a same-named stub at deduplication, mirroring resolution.
            if let Some(library) = stubs(db) {
                let documentation = |name: &String| {
                    library
                        .exports_by_namespace
                        .iter()
                        .find(|(_, names)| names.contains(name))
                        .map(|(namespace, _)| format!("From the `{namespace}` package."))
                };
                items.extend(
                    library
                        .schemes
                        .iter()
                        .filter(|(name, _)| search_match(name, &query).is_some())
                        .filter_map(|(name, schemes)| {
                            Some(CompletionItem {
                                documentation: documentation(name),
                                ..scheme_completion(db, name.clone(), schemes.first()?)
                            })
                        }),
                );
                // Manifest-only exports complete untyped. Non-syntactic names are skipped: a bare
                // reference needs backticks, so inserting the raw name changes the syntax.
                items.extend(
                    library
                        .known_exports
                        .iter()
                        .filter(|name| {
                            !library.schemes.contains_key(*name)
                                && is_syntactic_name(name)
                                && search_match(name, &query).is_some()
                        })
                        .map(|name| CompletionItem {
                            documentation: documentation(name),
                            ..CompletionItem::new(
                                name.clone(),
                                CompletionKind::Variable,
                                CompletionSource::Stdlib,
                            )
                        }),
                );
            }
            items
        }
    };

    finish_completions(items, &query)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeActionKind {
    IfUnknownToTrust,
    RemoveUnusedAssignment,
    /// Dot-names are exempt from the unused check.
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

/// All edits are computed eagerly, with no resolve round-trip.
pub fn code_actions(db: &dyn Db, file: SourceFile, viewport: TextRange) -> Vec<CodeAction> {
    let text = file.text(db);
    let mut actions = Vec::new();

    let root = semantics::parse(db, file).syntax_node();
    for annotation in root
        .descendants()
        .filter(|node| node.kind() == ANNOTATION && ranges_overlap(node.text_range(), viewport))
    {
        for directive in annotation
            .descendants()
            .filter(|node| node.kind() == ANNOTATION_DIRECTIVE)
        {
            if directive_name(&directive).as_deref() == Some("if-unknown")
                && let Some(at) = child_token(&directive, AT)
            {
                actions.push(CodeAction {
                    title: "Replace `@if-unknown` with `@trust`".to_owned(),
                    kind: CodeActionKind::IfUnknownToTrust,
                    edits: vec![TextEdit {
                        range: TextRange::at(at.text_range().start(), TextSize::of("@if-unknown")),
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
        // A statement typed inline, or by a `#:` block that survived, needs no second annotation.
        let annotated = item_annotation_syntax(db, item)
            .is_some_and(|annotation| block_refusal(&annotation.syntax_node()).is_none())
            || statement_inline_site(&node).is_some();
        let annotated_expressions: BTreeSet<ExprId> = item_expression_annotations(db, item)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        for (index, expression) in hir.expressions.iter().enumerate() {
            let ExpressionKind::Assign { target, value, .. } = &expression.kind else {
                continue;
            };
            let ExpressionKind::NameRef(name) = &hir.expression(*target).kind else {
                continue;
            };
            let id = ExprId(index as u32);
            let is_root = hir.root == Some(id);
            if (is_root && annotated) || annotated_expressions.contains(&id) {
                continue;
            }
            let start = usize::from(expression.range.start() + item_offset);
            let line_start = text[..start.min(text.len())]
                .rfind('\n')
                .map_or(0, |at| at + 1);
            let Some(indentation) = text
                .get(line_start..start)
                .filter(|indentation| indentation.trim().is_empty())
            else {
                continue;
            };
            let Some(label) = assignment_label(db, &check, id, *value, is_root) else {
                continue;
            };
            let edit = TextEdit {
                range: TextRange::empty(TextSize::from(line_start as u32)),
                replacement: format!("{indentation}#: {label}\n"),
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

/// Whole lines, trailing newline included, when the assignment is alone on them.
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

pub fn type_definition(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<DefinitionTarget> {
    let position = position_in_item(db, file, offset)?;
    let check = item_check(db, position.item)?;
    let ty = position
        .expressions_at()
        .into_iter()
        .find_map(|id| check.expression_types.get(&id).copied())?;
    let TyKind::Named(name, _) = ty.kind(db) else {
        return None;
    };
    type_name_definition(db, files, name.text(db))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentSymbolKind {
    Function,
    Value,
    TypeDefinition,
    AliasDefinition,
    S4Class,
    S4Generic,
    S4Method,
    R6Class,
    R6Method,
    /// A non-function R6 member, or an `active` binding.
    R6Field,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: DocumentSymbolKind,
    pub detail: Option<String>,
    pub range: TextRange,
    pub selection: TextRange,
    /// R6 class members; empty for every other kind.
    pub children: Vec<DocumentSymbol>,
}

/// Includes `@type`/`@alias` declarations, which live in `#:` comments outside the item tree.
pub fn document_symbols(db: &dyn Db, file: SourceFile) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        let range = node.text_range();
        let name = match *item.kind(db) {
            ItemKind::Function | ItemKind::Value => {
                let Some(name) = item.name(db) else {
                    continue;
                };
                Some(name)
            }
            ItemKind::Statement => None,
        };
        // A registration call (`setGeneric("name", ...)`) is a named item without an assignment;
        // it selects the string name.
        if node.kind() == CALL_EXPR {
            if let Some(SymbolCall {
                kind,
                name: Some((name, selection)),
                detail,
                children,
            }) = classify_symbol_call(&node)
            {
                symbols.push(DocumentSymbol {
                    name,
                    kind,
                    detail,
                    range,
                    selection,
                    children,
                });
            }
            continue;
        }
        let Some(name) = name else {
            continue;
        };
        let selection = item_name_range(db, item).unwrap_or(range);
        // An assigned S4/R6 construction keeps the assigned name but takes the construct's kind,
        // detail, and members.
        let (kind, detail, children) =
            match child_node(&node, CALL_EXPR).and_then(|call| classify_symbol_call(&call)) {
                Some(construct) => (construct.kind, construct.detail, construct.children),
                None => {
                    let kind = if matches!(*item.kind(db), ItemKind::Function) {
                        DocumentSymbolKind::Function
                    } else {
                        DocumentSymbolKind::Value
                    };
                    (kind, function_detail(&node), Vec::new())
                }
            };
        symbols.push(DocumentSymbol {
            name: name.clone(),
            kind,
            detail,
            range,
            selection,
            children,
        });
    }
    let root = semantics::parse(db, file).syntax_node();
    for directive in root
        .descendants()
        .filter(|node| node.kind() == ANNOTATION_DIRECTIVE)
    {
        let (kind, detail) = match directive_name(&directive).as_deref() {
            Some("type") => (DocumentSymbolKind::TypeDefinition, "@type"),
            Some("alias") => (DocumentSymbolKind::AliasDefinition, "@alias"),
            _ => continue,
        };
        let Some(name_token) =
            child_node(&directive, NAME).and_then(|name| child_token(&name, IDENT))
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
    symbols.sort_by_key(|symbol| (symbol.range.start(), symbol.range.end()));
    symbols
}

/// `name` is used only for bare statements; assigned constructions keep the assigned name.
struct SymbolCall {
    kind: DocumentSymbolKind,
    name: Option<(String, TextRange)>,
    detail: Option<String>,
    children: Vec<DocumentSymbol>,
}

fn classify_symbol_call(call: &SyntaxNode) -> Option<SymbolCall> {
    let callee = s4_callee_name(call)?;
    let arguments = child_node(call, ARGUMENT_LIST)?;
    let (kind, name_parameter, detail, children) = match callee.as_str() {
        "setClass" => (DocumentSymbolKind::S4Class, "Class", None, Vec::new()),
        "setGeneric" => (DocumentSymbolKind::S4Generic, "name", None, Vec::new()),
        "setMethod" => {
            let classes: Vec<String> = s4_argument(&arguments, "signature", 1)
                .iter()
                .flat_map(s4_signature_strings)
                .filter_map(|class| string_argument_content(Some(class)))
                .map(|(name, _)| name)
                .collect();
            let signature = if classes.is_empty() {
                "Unknown".to_owned()
            } else {
                classes.join(", ")
            };
            (
                DocumentSymbolKind::S4Method,
                "f",
                Some(signature),
                Vec::new(),
            )
        }
        "R6Class" => (
            DocumentSymbolKind::R6Class,
            "classname",
            None,
            r6_members(&arguments),
        ),
        _ => return None,
    };
    Some(SymbolCall {
        kind,
        name: string_argument_content(s4_argument(&arguments, name_parameter, 0)),
        detail,
        children,
    })
}

fn r6_members(arguments: &SyntaxNode) -> Vec<DocumentSymbol> {
    let mut members = Vec::new();
    for (field, position) in [("public", 1), ("private", 2), ("active", 3)] {
        let Some(list_arguments) = s4_argument(arguments, field, position)
            .filter(|list| list.kind() == CALL_EXPR)
            .and_then(|list| child_node(&list, ARGUMENT_LIST))
        else {
            continue;
        };
        for member in list_arguments
            .children()
            .filter(|child| child.kind() == ARGUMENT)
        {
            let Some(name) = child_node(&member, NAME) else {
                continue;
            };
            let value = member.children().find(|child| child.kind() != NAME);
            let is_function = value
                .as_ref()
                .is_some_and(|value| value.kind() == FUNCTION_DEF);
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

fn function_detail(node: &SyntaxNode) -> Option<String> {
    let function = node
        .descendants()
        .find(|child| child.kind() == FUNCTION_DEF)?;
    let parameters = child_node(&function, PARAMETER_LIST)?;
    let names: Vec<String> = parameters
        .children()
        .filter(|child| child.kind() == PARAMETER)
        .filter_map(|parameter| {
            parameter
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|token| matches!(token.kind(), IDENT | DOTS))
                .map(|token| token.text().to_owned())
        })
        .collect();
    Some(format!("fn({})", names.join(", ")))
}

/// The content and range inside the quotes of a string-literal node.
fn string_argument_content(node: Option<SyntaxNode>) -> Option<(String, TextRange)> {
    let token = child_token(&node?, STRING)?;
    let text = token.text();
    if !text.starts_with(['"', '\'']) {
        return None;
    }
    let name = text
        .get(1..text.len() - 1)
        .filter(|name| !name.is_empty())?;
    Some((
        name.to_owned(),
        TextRange::at(
            token.text_range().start() + TextSize::from(1),
            TextSize::of(name),
        ),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub kind: DocumentSymbolKind,
    pub target: NavigationTarget,
}

pub fn workspace_symbols(db: &dyn Db, files: ProjectFiles, query: &str) -> Vec<WorkspaceSymbol> {
    let mut symbols: Vec<(MatchScore, WorkspaceSymbol)> = Vec::new();
    for &file in files.files(db) {
        for symbol in document_symbols(db, file) {
            symbols.extend(once(&symbol).chain(&symbol.children).filter_map(|entry| {
                Some((
                    search_match(&entry.name, query)?,
                    WorkspaceSymbol {
                        name: entry.name.clone(),
                        kind: entry.kind,
                        target: NavigationTarget {
                            file,
                            range: entry.selection,
                        },
                    },
                ))
            }));
        }
    }
    symbols.sort_by_cached_key(|(score, symbol)| {
        (*score, symbol.name.to_lowercase(), symbol.name.clone())
    });
    symbols.truncate(COMPLETION_LIMIT);
    symbols.into_iter().map(|(_, symbol)| symbol).collect()
}

struct AnnotationTypeCursor {
    name: String,
    range: TextRange,
    /// Binder names, binder declarations, and `fn` hover as themselves but navigate nowhere.
    navigable: bool,
}

fn annotation_type_at(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
) -> Option<AnnotationTypeCursor> {
    let annotation = annotation_at(&semantics::parse(db, file).syntax_node(), offset)?;
    let token = annotation
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| {
            matches!(token.kind(), IDENT | NULL_KW) && token.text_range().contains_inclusive(offset)
        })?;
    let parent = token.parent()?;
    let resolvable = match parent.kind() {
        TYPE_REF => token.kind() == IDENT,
        NAME => {
            let grandparent = parent.parent()?;
            match grandparent.kind() {
                TYPE_APPLY => true,
                ANNOTATION_DIRECTIVE if is_type_directive(&grandparent) => true,
                other if type_kind(other) => false,
                _ => return None,
            }
        }
        other if type_kind(other) => false,
        _ => return None,
    };
    let name = token.text().to_owned();
    let navigable = resolvable && !binder_names(&annotation).contains(&name);
    Some(AnnotationTypeCursor {
        name,
        range: token.text_range(),
        navigable,
    })
}

// S4 names are string literals inside `setClass`/`setGeneric`/`setMethod`/`new` calls, invisible
// to naming analysis, so they are recovered structurally.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum S4Kind {
    Class,
    Generic,
}

struct S4Occurrence {
    name: String,
    kind: S4Kind,
    /// The content range inside the quotes, absolute.
    range: TextRange,
    is_declaration: bool,
}

fn s4_occurrences_in(db: &dyn Db, file: SourceFile) -> Vec<S4Occurrence> {
    let root = semantics::parse(db, file).syntax_node();
    let mut occurrences = Vec::new();
    for call in root.descendants().filter(|node| node.kind() == CALL_EXPR) {
        let (Some(callee), Some(arguments)) =
            (s4_callee_name(&call), child_node(&call, ARGUMENT_LIST))
        else {
            continue;
        };
        let strings: Vec<(Option<SyntaxNode>, S4Kind, bool)> = match callee.as_str() {
            "setClass" => vec![(s4_argument(&arguments, "Class", 0), S4Kind::Class, true)],
            "setGeneric" => vec![(s4_argument(&arguments, "name", 0), S4Kind::Generic, true)],
            // The generic's body dispatches by name, so a rename must cover it.
            "standardGeneric" => vec![(s4_argument(&arguments, "f", 0), S4Kind::Generic, false)],
            "setMethod" => once((s4_argument(&arguments, "f", 0), S4Kind::Generic, false))
                .chain(
                    s4_argument(&arguments, "signature", 1)
                        .iter()
                        .flat_map(s4_signature_strings)
                        .map(|class| (Some(class), S4Kind::Class, false)),
                )
                .collect(),
            "new" => vec![(s4_argument(&arguments, "Class", 0), S4Kind::Class, false)],
            _ => continue,
        };
        // Raw strings never carry S4 names.
        occurrences.extend(
            strings
                .into_iter()
                .filter_map(|(node, kind, is_declaration)| {
                    let (name, range) = string_argument_content(node)?;
                    Some(S4Occurrence {
                        name,
                        kind,
                        range,
                        is_declaration,
                    })
                }),
        );
    }
    occurrences
}

fn s4_callee_name(call: &SyntaxNode) -> Option<String> {
    let callee = call.children().next()?;
    match callee.kind() {
        NAME => Some(callee.text().to_string()),
        NAMESPACE_EXPR => callee
            .children()
            .filter(|child| child.kind() == NAME)
            .last()
            .map(|name| name.text().to_string()),
        _ => None,
    }
}

/// By name, else the unnamed argument at `index`.
fn s4_argument(arguments: &SyntaxNode, name: &str, index: usize) -> Option<SyntaxNode> {
    let argument_nodes = || {
        arguments
            .children()
            .filter(|child| child.kind() == ARGUMENT)
    };
    if let Some(named) = argument_nodes().find(|argument| {
        child_node(argument, NAME).is_some_and(|argument_name| argument_name.text() == name)
    }) {
        return named.children().find(|child| child.kind() != NAME);
    }
    argument_nodes()
        .nth(index)
        .filter(|argument| {
            // A NAME child followed by EQ is a `name =` tag; a bare NAME child is the value.
            !argument
                .children_with_tokens()
                .any(|element| element.kind() == EQ)
        })
        .and_then(|argument| argument.children().next())
}

fn s4_signature_strings(signature: &SyntaxNode) -> Vec<SyntaxNode> {
    if is_string_literal(signature) {
        return vec![signature.clone()];
    }
    if signature.kind() != CALL_EXPR {
        return Vec::new();
    }
    child_node(signature, ARGUMENT_LIST)
        .into_iter()
        .flat_map(|arguments| arguments.children())
        .filter(|child| child.kind() == ARGUMENT)
        .filter_map(|argument| argument.children().last())
        .filter(is_string_literal)
        .collect()
}

fn is_string_literal(node: &SyntaxNode) -> bool {
    node.kind() == LITERAL
        && node
            .children_with_tokens()
            .any(|element| element.kind() == STRING)
}

fn type_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        TYPE_REF
            | TYPE_APPLY
            | TYPE_VECTOR
            | TYPE_UNION
            | TYPE_FUNCTION
            | TYPE_RECORD
            | TYPE_TUPLE
            | TYPE_LIST
            | TYPE_PAREN
            | TYPE_BINDER_LIST
            | TYPE_BINDER
            | TYPE_PARAMETER_LIST
            | TYPE_ARG_LIST
    )
}

fn directive_name(directive: &SyntaxNode) -> Option<String> {
    let mut name = String::new();
    let mut end: Option<TextSize> = None;
    for element in directive.children_with_tokens() {
        let Some(token) = element.as_token() else {
            break;
        };
        match token.kind() {
            AT => {
                end = Some(token.text_range().end());
            }
            WHITESPACE | NEWLINE => break,
            // Only touching tokens join: `@if-unknown` lexes as `if` `-` `unknown`.
            _ if end.is_some() && Some(token.text_range().start()) == end => {
                name.push_str(token.text());
                end = Some(token.text_range().end());
            }
            _ => break,
        }
    }
    (!name.is_empty()).then_some(name)
}

fn is_type_directive(directive: &SyntaxNode) -> bool {
    matches!(directive_name(directive).as_deref(), Some("type" | "alias"))
}

/// The type parameters a region's types may name: a `#:` block's own binders, or the
/// `<...>` of the function an inline parameter or return type belongs to.
fn binder_names(region: &SyntaxNode) -> Vec<String> {
    // A binding's type sees no enclosing function's binders, as in the checker.
    let binders = if region.kind() == ANNOTATION {
        Some(region.clone())
    } else if region
        .parent()
        .is_some_and(|parent| parent.kind() == BINARY_EXPR)
    {
        None
    } else {
        region
            .ancestors()
            .find(|node| node.kind() == FUNCTION_DEF)
            .and_then(|function| child_node(&function, TYPE_BINDER_LIST))
    };
    let Some(binders) = binders else {
        return Vec::new();
    };
    binders
        .descendants()
        .filter(|node| node.kind() == TYPE_BINDER)
        .filter_map(|binder| {
            binder
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|token| token.kind() == IDENT)
                .map(|token| token.text().to_string())
        })
        .collect()
}

/// The type region holding `offset`: a `#:` block, or an inline annotation.
fn annotation_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxNode> {
    root.descendants()
        .find(|node| is_type_region(node) && node.text_range().contains_inclusive(offset))
}

fn is_type_region(node: &SyntaxNode) -> bool {
    node.kind() == ANNOTATION || is_inline_annotation(node)
}

/// Whether `offset` is where a type is being typed inline: right after a parameter's or a
/// binding's `:`, or a function's `->`, before any type exists to form a region.
fn at_inline_type_start(root: &SyntaxNode, offset: TextSize) -> bool {
    let Some(mut token) = root.token_at_offset(offset).left_biased() else {
        return false;
    };
    // Step back over the word being typed and the spacing before it.
    if token.kind() == IDENT {
        let Some(previous) = token.prev_token() else {
            return false;
        };
        token = previous;
    }
    if token.kind() == WHITESPACE {
        let Some(previous) = token.prev_token() else {
            return false;
        };
        token = previous;
    }
    if token
        .parent_ancestors()
        .any(|ancestor| is_inline_annotation(&ancestor))
    {
        return true;
    }
    // `count: ` at the start of a statement still reads as R's `:` until a type and an
    // assignment follow.
    token.kind() == COLON
        && token.parent().is_some_and(|parent| {
            parent.kind() == BINARY_EXPR
                && parent.first_child().is_some_and(|lhs| lhs.kind() == NAME)
                && parent
                    .parent()
                    .is_some_and(|statement| matches!(statement.kind(), SOURCE_FILE | BRACE_EXPR))
        })
}

fn child_node(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|child| child.kind() == kind)
}

fn child_token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| token.kind() == kind)
}

fn annotation_type_hover(db: &dyn Db, file: SourceFile, cursor: &AnnotationTypeCursor) -> Hover {
    // Resolves against the file's own declarations, matching how the checker's winner table
    // folds files.
    let declared = if cursor.navigable {
        file_type_definitions(db, file)
            .into_iter()
            .find(|definition| definition.name.text(db) == cursor.name)
    } else {
        None
    };
    let line = match &declared {
        Some(definition) => {
            let parameters = if definition.parameters.is_empty() {
                String::new()
            } else {
                let names: Vec<&str> = definition
                    .parameters
                    .iter()
                    .map(|parameter| parameter.text(db))
                    .collect();
                format!("<{}>", names.join(", "))
            };
            let keyword = if definition.alias { "@alias" } else { "@type" };
            format!(
                "{keyword} {}{parameters} {{{}}}",
                cursor.name,
                TypeRenderer::default().render(db, definition.body)
            )
        }
        None => cursor.name.clone(),
    };
    let definition = if declared.is_none()
        && cursor.navigable
        && stubs(db).is_some_and(|library| library.nominals.contains(&cursor.name))
        && let Some(namespace) = declaring_namespace(db, &cursor.name)
    {
        Some(HoverDefinition::Stub {
            namespace: namespace.to_owned(),
            overloads: 0,
            declaration: stub_target(db, &cursor.name),
        })
    } else {
        None
    };
    Hover {
        range: cursor.range,
        lines: vec![line],
        definition,
    }
}

/// The hover range spans only the declaration's own lines: a stitched block may hold several.
fn annotation_definition_hover(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<Hover> {
    let annotation = annotation_at(&semantics::parse(db, file).syntax_node(), offset)?;
    annotation
        .descendants()
        .filter(|node| node.kind() == ANNOTATION_DIRECTIVE && is_type_directive(node))
        .find_map(|directive| {
            let start = annotation
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|token| {
                    token.kind() == ANNOTATION_MARKER
                        && token.text_range().start() <= directive.text_range().start()
                })
                .map(|token| token.text_range().start())
                .last()
                .unwrap_or_else(|| directive.text_range().start());
            let range = TextRange::new(start, directive.text_range().end());
            if !range.contains_inclusive(offset) {
                return None;
            }
            let name = child_node(&directive, NAME)?.text().to_string();
            Some(annotation_type_hover(
                db,
                file,
                &AnnotationTypeCursor {
                    name,
                    range,
                    navigable: true,
                },
            ))
        })
}

fn type_name_occurrences(db: &dyn Db, files: ProjectFiles, name: &str) -> Vec<Occurrence> {
    let mut result = Vec::new();
    for &file in files.files(db) {
        let root = semantics::parse(db, file).syntax_node();
        for annotation in root.descendants().filter(is_type_region) {
            let shadowed = binder_names(&annotation)
                .iter()
                .any(|binder| binder == name);
            for node in annotation.descendants() {
                let is_declaration = match node.kind() {
                    TYPE_REF => false,
                    NAME => match node.parent() {
                        Some(parent) if parent.kind() == TYPE_APPLY => false,
                        Some(parent)
                            if parent.kind() == ANNOTATION_DIRECTIVE
                                && is_type_directive(&parent) =>
                        {
                            true
                        }
                        _ => continue,
                    },
                    _ => continue,
                };
                let Some(token) = child_token(&node, IDENT) else {
                    continue;
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

fn string_subscript_completion(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
    string_token: &SyntaxToken,
) -> Option<CompletionResult> {
    let position = position_in_item(db, file, offset)?;
    let hir = item_hir(db, position.item).as_ref()?;
    let check = item_check(db, position.item)?;

    let string_range = string_token.text_range() - position.item_offset;
    let target = hir
        .expressions
        .iter()
        .find_map(|expression| match &expression.kind {
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
        })?;
    let (TyKind::Record(fields) | TyKind::Frame(Some(fields))) =
        check.expression_types.get(&target)?.kind(db)
    else {
        return None;
    };

    let typed = usize::from(offset - string_token.text_range().start());
    let query = string_token.text().get(1..typed).unwrap_or_default();

    let mut renderer = TypeRenderer::default();
    let items = fields
        .iter()
        .filter(|field| search_match(field.name.text(db), query).is_some())
        .map(|field| CompletionItem {
            detail: Some(renderer.render(db, field.ty)),
            ..CompletionItem::new(
                field.name.text(db).to_owned(),
                CompletionKind::Field,
                CompletionSource::Field,
            )
        })
        .collect();
    finish_completions(items, query)
}

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
    let mut items: Vec<CompletionItem> = PRIMITIVES
        .iter()
        .filter(|primitive| search_match(primitive, query).is_some())
        .map(|primitive| {
            CompletionItem::new(
                (*primitive).to_owned(),
                CompletionKind::Keyword,
                CompletionSource::Keyword,
            )
        })
        .collect();
    // The project table covers package files only, so a script's own declarations are added.
    let mut definitions: Vec<(String, Ty<'_>)> = project_type_definitions(db, files)
        .iter()
        .map(|(name, definition)| (name.text(db).to_owned(), definition.body))
        .collect();
    for definition in file_type_definitions(db, file) {
        let label = definition.name.text(db).to_owned();
        if !definitions.iter().any(|(name, _)| *name == label) {
            definitions.push((label, definition.body));
        }
    }
    items.extend(
        definitions
            .into_iter()
            .filter(|(label, _)| search_match(label, query).is_some())
            .map(|(label, body)| CompletionItem {
                detail: Some(TypeRenderer::default().render(db, body)),
                ..CompletionItem::new(label, CompletionKind::Variable, CompletionSource::Global)
            }),
    );
    finish_completions(items, query)
}

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
    /// A bare trailing `:`, either the range operator or half a `::`; stays silent.
    MaybeNamespace,
}

/// R scoping is function-granular: a binding is visible only where its innermost enclosing
/// `FUNCTION_DEF` contains the cursor, so sibling closures' locals do not leak.
fn binding_visible_at(item_node: &SyntaxNode, binding_start: TextSize, cursor: TextSize) -> bool {
    if !item_node.text_range().contains_inclusive(binding_start) {
        return true;
    }
    let covering = match item_node.covering_element(TextRange::empty(binding_start)) {
        SyntaxElement::Node(node) => node,
        SyntaxElement::Token(token) => token.parent().unwrap_or_else(|| item_node.clone()),
    };
    covering
        .ancestors()
        .find(|ancestor| ancestor.kind() == FUNCTION_DEF)
        .is_none_or(|function| function.text_range().contains(cursor))
}

/// Works on raw text so completion still fires mid-edit inside broken code.
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
            // A name after a single `:` is a range operand (`1:n`), not a pending `::`.
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
                                    .take_while(|next| {
                                        next.is_alphanumeric() || *next == '.' || *next == '_'
                                    })
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

/// The syntactic fallback shared by `@` completion and untyped `$` targets.
fn spelled_completions(
    db: &dyn Db,
    files: ProjectFiles,
    query: &str,
    kind: SyntaxKind,
    source: CompletionSource,
) -> Vec<CompletionItem> {
    let mut labels = BTreeSet::new();
    for &project_file in files.files(db) {
        let root = semantics::parse(db, project_file).syntax_node();
        // The lhs is an expression node, so the last bare NAME child is the field.
        labels.extend(
            root.descendants()
                .filter(|node| node.kind() == kind)
                .filter_map(|node| node.children().filter(|child| child.kind() == NAME).last())
                .map(|name| name.text().to_string()),
        );
    }
    labels
        .into_iter()
        .filter(|label| search_match(label, query).is_some())
        .map(|label| CompletionItem::new(label, CompletionKind::Field, source))
        .collect()
}

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
        let (_, target) = hir
            .expressions
            .iter()
            .filter_map(|expression| match &expression.kind {
                ExpressionKind::Field { target, .. }
                    if !expression.range.is_empty()
                        && expression.range.contains_inclusive(position.relative) =>
                {
                    Some((expression.range, *target))
                }
                _ => None,
            })
            .min_by_key(|(range, _)| range.len())?;
        let (TyKind::Record(fields) | TyKind::Frame(Some(fields))) =
            check.expression_types.get(&target)?.kind(db)
        else {
            return None;
        };
        let mut renderer = TypeRenderer::default();
        Some(
            fields
                .iter()
                .map(|field| CompletionItem {
                    detail: Some(renderer.render(db, field.ty)),
                    ..CompletionItem::new(
                        insertable_name(field.name.text(db)),
                        CompletionKind::Field,
                        CompletionSource::Field,
                    )
                })
                .filter(|item| search_match(&item.label, query).is_some())
                .collect::<Vec<_>>(),
        )
    })();
    match typed {
        Some(items) if !items.is_empty() => items,
        _ => spelled_completions(db, files, query, DOLLAR_EXPR, CompletionSource::Field),
    }
}

fn namespace_completions(
    db: &dyn Db,
    files: ProjectFiles,
    package: &str,
    query: &str,
) -> Vec<CompletionItem> {
    if let Some(library) = stubs(db)
        && let Some(exports) = library.exports_by_namespace.get(package)
    {
        return exports
            .iter()
            .filter(|name| search_match(name, query).is_some())
            .map(|name| {
                let label = insertable_name(name);
                match library
                    .schemes
                    .get(name)
                    .and_then(|schemes| schemes.first())
                {
                    Some(scheme) => scheme_completion(db, label, scheme),
                    None => CompletionItem::new(
                        label,
                        CompletionKind::Variable,
                        CompletionSource::Stdlib,
                    ),
                }
            })
            .collect();
    }
    spelled_completions(db, files, query, NAMESPACE_EXPR, CompletionSource::Global)
}

fn finish_completions(mut items: Vec<CompletionItem>, query: &str) -> Option<CompletionResult> {
    let mut seen = BTreeSet::new();
    items.retain(|item| seen.insert(item.label.clone()));
    items.sort_by_cached_key(|item| {
        (
            search_match(&item.label, query),
            item.source,
            item.label.to_lowercase(),
            item.label.clone(),
        )
    });
    let is_incomplete = items.len() > COMPLETION_LIMIT;
    items.truncate(COMPLETION_LIMIT);
    (!items.is_empty()).then_some(CompletionResult {
        items,
        is_incomplete,
    })
}

fn scheme_completion(db: &dyn Db, label: String, scheme: &TypeScheme<'_>) -> CompletionItem {
    let kind = match scheme.body.kind(db) {
        TyKind::Function(_) => CompletionKind::Function,
        _ => CompletionKind::Variable,
    };
    CompletionItem {
        detail: Some(TypeRenderer::default().render_scheme(db, scheme)),
        takes_arguments: scheme_takes_arguments(db, scheme),
        ..CompletionItem::new(label, kind, CompletionSource::Stdlib)
    }
}

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

/// A non-syntactic name is insertable after `$` or `::` only backtick-quoted.
fn insertable_name(name: &str) -> String {
    if is_syntactic_name(name) {
        name.to_owned()
    } else {
        format!("`{name}`")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchScore {
    tier: MatchTier,
    first_match_index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchTier {
    Exact,
    Prefix,
    Substring,
    Subsequence,
}

/// Shorter queries match by prefix only, so one- or two-character inputs do not surface
/// scattered matches.
const MIN_SUBSEQUENCE_QUERY_LEN: usize = 3;

/// Smart case: case-insensitive unless the query has an uppercase character. An empty query
/// matches everything.
pub fn search_match(candidate: &str, query: &str) -> Option<MatchScore> {
    if query.is_empty() {
        return Some(MatchScore {
            tier: MatchTier::Prefix,
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
        MatchTier::Exact
    } else if prefix_under_case(candidate, query, case_sensitive) {
        MatchTier::Prefix
    } else if substring_under_case(candidate, query, case_sensitive) {
        MatchTier::Substring
    } else {
        MatchTier::Subsequence
    };
    if query.chars().count() < MIN_SUBSEQUENCE_QUERY_LEN && tier > MatchTier::Prefix {
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

/// `...` renders at its formal position, so span indexes line up with `active_parameter`'s
/// display translation.
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

/// Matching works in slot space (positionals, named, rest last) and translates to display order,
/// which interleaves `...` at its formal position.
fn active_parameter(
    db: &dyn Db,
    function: &FunctionType<'_>,
    rendered_count: usize,
    arguments: &[Argument],
    hir: &Module,
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

    // Arguments ending before the cursor are complete; the cursor sits on the next, possibly
    // unwritten, one.
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
            // Named arguments are never routed into `...`, so an unmatched name consumes no slot.
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

    // The cursor's own named argument targets the parameter it names, else `...`.
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

/// Variables are presentable only under a top-level function type, whose label generalizes them
/// into binders; anywhere else they would render as unanchored type parameters.
fn assignment_label(
    db: &dyn Db,
    check: &ItemCheck<'_>,
    id: ExprId,
    value: ExprId,
    is_root: bool,
) -> Option<String> {
    let monomorphic;
    let scheme = if is_root {
        check.scheme.as_ref()?
    } else {
        let ty = check
            .expression_types
            .get(&id)
            .or_else(|| check.expression_types.get(&value))?;
        monomorphic = TypeScheme::monomorphic(*ty);
        &monomorphic
    };
    is_hintable(
        db,
        scheme.body,
        matches!(scheme.body.kind(db), TyKind::Function(_)),
    )
    .then(|| TypeRenderer::default().render_scheme(db, scheme))
}

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
        TyKind::Record(fields) | TyKind::Frame(Some(fields)) => fields
            .iter()
            .all(|field| is_hintable(db, field.ty, variables_allowed)),
        TyKind::Frame(None) => true,
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

/// `StubGlobal` names are declared only by the stubs: navigable, but references stay empty and
/// rename refuses.
enum Target<'db> {
    Slot { item: Item<'db>, binding: BindingId },
    Global(String),
    TypeName(String),
    StubGlobal(String),
    S4 { name: String, kind: S4Kind },
}

fn target_at<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<Target<'db>> {
    if let Some(cursor) = annotation_type_at(db, file, offset) {
        let declared = cursor.navigable
            && (type_name_occurrences(db, files, &cursor.name)
                .iter()
                .any(|occurrence| occurrence.is_declaration)
                || stubs(db).is_some_and(|library| library.nominals.contains(&cursor.name)));
        return declared.then_some(Target::TypeName(cursor.name));
    }

    let position = position_in_item(db, file, offset)?;
    let naming = item_naming(db, position.item).as_ref()?;

    if let Some(expression) = position.expression_at() {
        if let Some(binding) = naming.resolutions.get(&expression) {
            let info = naming.bindings.get(binding)?;
            // Cross-item reads resolve to the name, so the item's own top-level binding is the
            // global.
            if info.kind == BindingKind::TopLevel {
                return Some(Target::Global(info.name.clone()));
            }
            return Some(Target::Slot {
                item: position.item,
                binding: *binding,
            });
        }
        // A quiet read (data masking, an opaque operator) navigates like any cross-item read;
        // only its diagnostic is withheld.
        if let Some(name) = naming
            .non_locals
            .get(&expression)
            .or_else(|| naming.quiet_reads.get(&expression))
        {
            if global_declaration_exists(db, files, name) {
                return Some(Target::Global(name.clone()));
            }
            if stub_declaration(db, name).is_some() {
                return Some(Target::StubGlobal(name.clone()));
            }
        }
    }

    // A generic the project defines is also an ordinary global function (`setGeneric` binds its
    // name), so its strings and its calls are one symbol.
    if let Some(occurrence) = s4_occurrences_in(db, file)
        .into_iter()
        .find(|occurrence| occurrence.range.contains_inclusive(offset))
    {
        if occurrence.kind == S4Kind::Generic
            && global_declaration_exists(db, files, &occurrence.name)
        {
            return Some(Target::Global(occurrence.name));
        }
        return Some(Target::S4 {
            name: occurrence.name,
            kind: occurrence.kind,
        });
    }

    // Parameters and loop variables have no expression, and where siblings abut, the expression
    // ending at the cursor outranks the declaration starting there; finding the declaration here
    // keeps goto-definition reciprocal with references. Smallest range first for determinism.
    let (binding, info) = naming
        .bindings
        .iter()
        .filter(|(_, info)| info.range.contains_inclusive(position.relative))
        .min_by_key(|(binding, info)| (info.range.len(), binding.0))?;
    if info.kind == BindingKind::TopLevel {
        return Some(Target::Global(info.name.clone()));
    }
    Some(Target::Slot {
        item: position.item,
        binding: *binding,
    })
}

/// Counts conditional top-level writes too, matching the occurrence walk's notion of a
/// declaration.
fn global_declaration_exists(db: &dyn Db, files: ProjectFiles, name: &str) -> bool {
    files.files(db).iter().any(|file| {
        item_tree(db, *file).iter().copied().any(|item| {
            // A named definition declares its name even without a slot: `setGeneric("name", …)`
            // binds through a string.
            (matches!(*item.kind(db), ItemKind::Function | ItemKind::Value)
                && item.name(db).as_deref() == Some(name))
                || item_naming(db, item).as_ref().is_some_and(|naming| {
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
        Target::Slot { item, binding } => slot_occurrences(db, *item, *binding, &mut result),
        Target::TypeName(name) => result = type_name_occurrences(db, files, name),
        Target::S4 { name, kind } => {
            for &file in files.files(db) {
                result.extend(s4_occurrences(db, file, name, *kind));
            }
        }
        Target::Global(name) => {
            for &file in files.files(db) {
                result.extend(s4_occurrences(db, file, name, S4Kind::Generic));
                for &item in item_tree(db, file) {
                    let (Some(node), Some(naming)) = (item_node(db, item), item_naming(db, item))
                    else {
                        continue;
                    };
                    for (binding, info) in &naming.bindings {
                        if info.kind == BindingKind::TopLevel && info.name == *name {
                            slot_occurrences(db, item, *binding, &mut result);
                        }
                    }
                    if let Some(hir) = item_hir(db, item) {
                        let item_offset = node.text_range().start();
                        result.extend(
                            naming
                                .non_locals
                                .iter()
                                .chain(&naming.quiet_reads)
                                .filter(|(_, non_local)| *non_local == name)
                                .map(|(expression, _)| Occurrence {
                                    file,
                                    range: hir.expression(*expression).range + item_offset,
                                    is_declaration: false,
                                }),
                        );
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

fn s4_occurrences(
    db: &dyn Db,
    file: SourceFile,
    name: &str,
    kind: S4Kind,
) -> impl Iterator<Item = Occurrence> {
    s4_occurrences_in(db, file)
        .into_iter()
        .filter(move |occurrence| occurrence.name == name && occurrence.kind == kind)
        .map(move |occurrence| Occurrence {
            file,
            range: occurrence.range,
            is_declaration: occurrence.is_declaration,
        })
}

/// Parameters and loop variables are not expressions, so their declaration site is emitted
/// explicitly.
fn slot_occurrences(db: &dyn Db, item: Item<'_>, binding: BindingId, result: &mut Vec<Occurrence>) {
    let (Some(node), Some(naming), Some(hir)) = (
        item_node(db, item),
        item_naming(db, item),
        item_hir(db, item),
    ) else {
        return;
    };
    let Some(info) = naming.bindings.get(&binding) else {
        return;
    };
    let file = *item.file(db);
    let item_offset = node.text_range().start();

    let mut slot = Vec::new();
    if matches!(info.kind, BindingKind::Parameter | BindingKind::ForVariable) {
        slot.push(Occurrence {
            file,
            range: info.range + item_offset,
            is_declaration: true,
        });
    }
    let targets: BTreeSet<ExprId> = hir
        .expressions
        .iter()
        .filter_map(|expression| match &expression.kind {
            ExpressionKind::Assign { target, .. } => Some(*target),
            _ => None,
        })
        .collect();
    slot.extend(
        naming
            .resolutions
            .iter()
            .filter(|(_, resolved)| **resolved == binding)
            .map(|(expression, _)| Occurrence {
                file,
                range: hir.expression(*expression).range + item_offset,
                is_declaration: targets.contains(expression),
            }),
    );
    slot.sort_by_key(|occurrence| occurrence.range.start());
    slot.dedup_by_key(|occurrence| occurrence.range);
    result.extend(slot);
}

struct PositionedItem<'db> {
    db: &'db dyn Db,
    item: Item<'db>,
    item_offset: TextSize,
    relative: TextSize,
}

impl PositionedItem<'_> {
    /// Smallest first and end-inclusive, so a cursor right after a name still hits it; name
    /// references win ties.
    fn expressions_at(&self) -> Vec<ExprId> {
        let Some(hir) = item_hir(self.db, self.item) else {
            return Vec::new();
        };
        let mut containing: Vec<(TextSize, bool, ExprId)> = hir
            .expressions
            .iter()
            .enumerate()
            .filter(|(_, expression)| {
                !expression.range.is_empty() && expression.range.contains_inclusive(self.relative)
            })
            .map(|(index, expression)| {
                (
                    expression.range.len(),
                    !matches!(expression.kind, ExpressionKind::NameRef(_)),
                    ExprId(index as u32),
                )
            })
            .collect();
        containing.sort_unstable();
        containing.into_iter().map(|(_, _, id)| id).collect()
    }

    fn expression_at(&self) -> Option<ExprId> {
        self.expressions_at().into_iter().next()
    }
}

fn position_in_item(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<PositionedItem<'_>> {
    let positioned = |item, range: TextRange| PositionedItem {
        db,
        item,
        item_offset: range.start(),
        relative: offset - range.start(),
    };
    // A cursor exactly at an item's end (typing after its last token) belongs to it unless the
    // next item starts there.
    let mut touching = None;
    for &item in item_tree(db, file) {
        let Some(node) = item_node(db, item) else {
            continue;
        };
        let range = node.text_range();
        if range.contains(offset) {
            return Some(positioned(item, range));
        }
        if offset == range.end() && touching.is_none() {
            touching = Some(positioned(item, range));
        }
    }
    touching
}
