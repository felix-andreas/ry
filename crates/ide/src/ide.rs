//! Every feature is a pure read of the database at a byte offset; UTF-16 and protocol concerns
//! live in the server. Per-item query results carry item-relative ranges, while this API speaks
//! file-absolute offsets (`semantics::item_node` anchors an item).

use semantics::check::ItemCheck;
use semantics::diagnostics::TypeRenderer;
use semantics::hir::{Argument, ExprId, ExpressionKind};
use semantics::naming::{BindingId, BindingKind};
use semantics::types::{FunctionType, Ty, TyKind, TypeScheme, contains_inference_var};
use semantics::{
    Db, Item, ItemKind, ProjectFiles, SourceFile, global_scheme, item_annotation_syntax,
    item_check, item_hir, item_name_range, item_naming, item_node, item_tree, package_definitions,
};
use syntax::{TextRange, TextSize};

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

fn stub_target(db: &dyn Db, name: &str) -> Option<StubTarget> {
    semantics::stubs::stub_declaration(db, name).map(|declaration| StubTarget {
        source_index: declaration.source_index,
        range: declaration.range,
    })
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
        return annotation_type_hover(db, file, &cursor);
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

    let mut renderer = TypeRenderer::default();
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
        _ => (vec![renderer.render(db, ty)], None),
    };

    let range = trim_trailing_trivia(
        file.text(db),
        hir.expression(expression).range + position.item_offset,
    );
    Some(Hover {
        range,
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

fn global_hover_definition(
    db: &dyn Db,
    files: ProjectFiles,
    name: &str,
    own_item: Option<Item<'_>>,
) -> Option<HoverDefinition> {
    let declaring = declaring_item(db, files, name).or(own_item)?;
    let node = item_node(db, declaring)?;
    let range = item_name_range(db, declaring).unwrap_or_else(|| node.text_range());
    Some(HoverDefinition::Global {
        target: NavigationTarget {
            file: *declaring.file(db),
            range,
        },
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
    let candidates = semantics::stubs::stubs(db)?.schemes.get(name)?;
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
            let naming = item_naming(db, item).as_ref()?;
            let info = naming.bindings.get(&binding)?;
            let item_offset = item_node(db, item)?.text_range().start();
            Some(DefinitionTarget::Project(NavigationTarget {
                file,
                range: info.range + item_offset,
            }))
        }
        // The last declaration wins, matching the project definition table's fold order.
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
                let range = item_name_range(db, *winner).unwrap_or_else(|| node.text_range());
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
        // Only a surviving payload types the binding: refused, definitions-only, and toggle-only
        // blocks leave it hintable.
        let annotated = item_annotation_syntax(db, item).is_some_and(|annotation| {
            let lowered = semantics::annotations::lower_annotation(db, &annotation.syntax_node());
            lowered.declared.is_some() || lowered.new_nominal.is_some() || lowered.trusted
        });
        // A hint on an annotated nested statement would restate the line above.
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

    // An overloaded stub name lists every candidate, even before a commitment: an incomplete call
    // matches none, which is exactly when a reader needs the shapes on offer.
    if let Some(schemes) = overloads {
        let mut signatures: Vec<SignatureData> = schemes
            .iter()
            .map(|scheme| overload_signature(db, scheme, arguments, hir, position.relative))
            .collect();
        let selected = check.selected_overloads.get(&callee).copied().unwrap_or(0);
        // The committed candidate renders instantiated with this call site's types.
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
    // An open callee renders the scheme it instantiates rather than variables nothing binds.
    let naming = item_naming(db, position.item).as_ref();
    if let Some(name) = callee_name(hir, callee)
        && naming.is_some_and(|naming| naming.non_locals.contains_key(&callee))
        && contains_inference_var(db, Ty::new(db, TyKind::Function(function.clone())))
        && let Some(files) = ProjectFiles::try_get(db)
        && let Some(scheme) = referenced_scheme(db, files, &name, &check, callee)
    {
        return Some(SignatureHelp {
            signatures: vec![overload_signature(
                db,
                &scheme,
                arguments,
                hir,
                position.relative,
            )],
            active_signature: 0,
        });
    }
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

fn callee_name(hir: &semantics::hir::Module, callee: ExprId) -> Option<String> {
    match &hir.expression(callee).kind {
        ExpressionKind::NameRef(name) => Some(name.clone()),
        ExpressionKind::Namespace { name, .. } => name.clone(),
        _ => None,
    }
}

/// A non-function declaration still occupies its slot, so the committed index stays aligned
/// with the declared set.
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

    {
        let parse = semantics::parse(db, file);
        if parse.syntax_node().descendants().any(|node| {
            node.kind() == syntax::SyntaxKind::ANNOTATION
                && node.text_range().start() <= offset
                && offset <= node.text_range().end()
        }) {
            return annotation_completion(db, files, file, &query);
        }
        // R value names never resolve inside string content; only record-field subscripts
        // complete there.
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
            // Keywords are prefix-completed only: no one searches for `function` by typing `con`.
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
                        // A statement item still creates top-level slots (a conditional write
                        // inside a top-level `if`).
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
            // A project global outranks a same-named stub at deduplication, mirroring resolution.
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
                // Manifest-only exports complete untyped. Non-syntactic names are skipped: a bare
                // reference needs backticks, so inserting the raw name changes the syntax.
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
        match *item.kind(db) {
            ItemKind::Function | ItemKind::Value => {
                let Some(name) = item.name(db).clone() else {
                    continue;
                };
                // A registration call (`setGeneric("name", ...)`) is a named item without an
                // assignment; it selects the string name.
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
                let selection = item_name_range(db, item).unwrap_or(range);
                // An assigned S4/R6 construction keeps the assigned name but takes the
                // construct's kind, detail, and members.
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

/// `name` is used only for bare statements; assigned constructions keep the assigned name.
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

/// The content and range inside the quotes of a string-literal node.
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
            Some(other) if type_kind(other) => (true, false),
            _ => (false, false),
        },
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
            // The generic's body dispatches by name, so a rename must cover it.
            "standardGeneric" => push_s4_string(
                s4_argument(&arguments, "f", 0),
                S4Kind::Generic,
                false,
                &mut occurrences,
            ),
            "setMethod" => {
                push_s4_string(
                    s4_argument(&arguments, "f", 0),
                    S4Kind::Generic,
                    false,
                    &mut occurrences,
                );
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

/// By name, else the unnamed argument at `index`.
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
            // A NAME child followed by EQ is a `name =` tag; a bare NAME child is the value.
            !argument
                .children_with_tokens()
                .any(|element| element.kind() == syntax::SyntaxKind::EQ)
        })
        .and_then(|argument| argument.children().next())
}

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
            .filter_map(|argument| argument.children().last())
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
    // Raw strings never carry S4 names.
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
    // Resolves against the file's own declarations, matching how the checker's winner table
    // folds files.
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

/// The hover range spans only the declaration's own lines: a stitched block may hold several.
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
    // The project table covers package files only, so a script's own declarations are added.
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

/// The syntactic fallback shared by `@` completion and untyped `$` targets.
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
            // The lhs is an expression node, so the last bare NAME child is the field.
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
                    // A non-syntactic field name is insertable after `$` only backtick-quoted.
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
                // A non-syntactic export is referable after `::` only backtick-quoted.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchScore {
    tier: u8,
    first_match_index: u32,
}

const TIER_EXACT: u8 = 0;
const TIER_PREFIX: u8 = 1;
const TIER_SUBSTRING: u8 = 2;
const TIER_SUBSEQUENCE: u8 = 3;

/// Shorter queries match by prefix only, so one- or two-character inputs do not surface
/// scattered matches.
const MIN_SUBSEQUENCE_QUERY_LEN: usize = 3;

/// Smart case: case-insensitive unless the query has an uppercase character. An empty query
/// matches everything.
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
fn scheme_is_hintable(db: &dyn Db, scheme: &TypeScheme<'_>) -> bool {
    is_hintable(
        db,
        scheme.body,
        matches!(scheme.body.kind(db), TyKind::Function(_)),
    )
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
                || semantics::stubs::stubs(db)
                    .is_some_and(|library| library.nominals.contains(&cursor.name)));
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
            if semantics::stubs::stub_declaration(db, name).is_some() {
                return Some(Target::StubGlobal(name.clone()));
            }
        }
    }

    // A generic the project defines is also an ordinary global function (`setGeneric` binds its
    // name), so its strings and its calls are one symbol.
    if let Some(occurrence) = s4_occurrences_in(db, file)
        .into_iter()
        .find(|occurrence| occurrence.range.start() <= offset && offset <= occurrence.range.end())
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
                for occurrence in s4_occurrences_in(db, file) {
                    if occurrence.kind == S4Kind::Generic && occurrence.name == *name {
                        result.push(Occurrence {
                            file,
                            range: occurrence.range,
                            is_declaration: occurrence.is_declaration,
                        });
                    }
                }
                for &item in item_tree(db, file) {
                    let Some(node) = item_node(db, item) else {
                        continue;
                    };
                    let item_offset = node.text_range().start();
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

/// Parameters and loop variables are not expressions, so their declaration site is emitted
/// explicitly.
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

fn assignment_targets(hir: &semantics::hir::Module) -> std::collections::BTreeSet<ExprId> {
    let mut targets = std::collections::BTreeSet::new();
    for expression in &hir.expressions {
        if let ExpressionKind::Assign { target, .. } = &expression.kind {
            targets.insert(*target);
        }
    }
    targets
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

    fn expression_at(&self) -> Option<ExprId> {
        self.expressions_at().into_iter().next()
    }
}

fn position_in_item(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<PositionedItem<'_>> {
    // A cursor exactly at an item's end (typing after its last token) belongs to it unless the
    // next item starts there.
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
