//! Every feature is a pure read of the database at a byte offset; UTF-16 and protocol concerns
//! live in the server. Per-item query results carry item-relative ranges, while this API speaks
//! file-absolute offsets (`semantics::item_node` anchors an item).

use {
    semantics::{
        Db, Item, ItemKind, ProjectFiles, SourceFile,
        annotations::{block_refusal, statement_inline_site, type_definition_keyword},
        check::{CalleeTarget, ItemCheck},
        diagnostics::TypeRenderer,
        file_type_definitions, global_scheme,
        hir::{Argument, ExprId, ExpressionKind, LiteralKind, Module, PatId, PatternKind, Symbol},
        infer::apply_definition,
        item_annotation_syntax, item_annotations, item_check, item_hir, item_name_range,
        item_naming, item_node, item_tree,
        naming::{BindingId, BindingKind, dot_dot_variable},
        package_definitions, project_global_names, project_type_definitions,
        stubs::{StubSources, declaring_namespace, stub_declaration, stubs},
        types::{
            Constraint, FunctionType, Name, RecordField, Ty, TyKind, TypeScheme,
            contains_inference_var,
        },
        visible_type_definitions,
    },
    std::{
        collections::{BTreeSet, HashMap, HashSet},
        iter::once,
    },
    syntax::{
        SyntaxElement,
        SyntaxKind::{self, *},
        SyntaxNode, SyntaxToken, TextRange, TextSize, is_inline_annotation, is_syntactic_name,
        kind::KEYWORDS,
    },
};

pub mod testing;

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
        /// The declaration's `#'` lines, as Markdown.
        documentation: Option<String>,
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
    if let Some(cursor) = type_parameter_at(db, file, offset) {
        return Some(Hover {
            range: cursor.range,
            lines: vec![format!("type parameter {}", cursor.name)],
            definition: None,
        });
    }
    if let Some(cursor) = annotation_type_at(db, file, offset) {
        return Some(annotation_type_hover(db, file, &cursor));
    }
    if let Some(hover) = annotation_definition_hover(db, file, offset) {
        return Some(hover);
    }

    let position = position_in_item(db, file, offset)?;
    let check = item_check(db, position.item)?;
    let hir = item_hir(db, position.item).as_ref()?;
    if let Some(member) = member_at(db, hir, &check, position.relative) {
        let mut renderer = TypeRenderer::default();
        let line = match &member.label {
            Some(name) => format!("{name}: {}", renderer.render(db, member.ty)),
            None => renderer.render(db, member.ty),
        };
        return Some(Hover {
            range: member.range + position.item_offset,
            lines: vec![line],
            definition: None,
        });
    }
    // Write targets and operators record no type, so the hover widens to the enclosing typed
    // expression.
    let (expression, ty) = position
        .expressions_at()
        .into_iter()
        .find_map(|id| check.expression_types.get(&id).map(|ty| (id, *ty)))?;
    let ty = without_impossible_vectors(db, ty);

    let (lines, definition) = match &hir.kind(expression) {
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
            let mut scheme = exported
                .or_else(referenced)
                .unwrap_or_else(|| TypeScheme::monomorphic(ty));
            scheme.body = without_impossible_vectors(db, scheme.body);
            (
                hover_lines(db, name, &scheme),
                hover_definition(db, files, position.item, expression, name),
            )
        }
        _ => (vec![TypeRenderer::default().render(db, ty)], None),
    };

    Some(Hover {
        range: trim_trailing_trivia(file.text(db), hir.range(expression) + position.item_offset),
        lines,
        definition,
    })
}

/// A name that is not an expression of its own, so the expression-based hover would widen past
/// it: a parameter or loop variable where it is declared, a tag or a field in a `match` pattern,
/// a field name in a record literal or tag payload, the field after `$`, and the column a `:=`
/// writes.
struct Member<'db> {
    /// Item-relative; a tag's range starts at its `:`.
    range: TextRange,
    /// `Some` for a field or column, which hovers as `name: type`; a tag hovers as its type.
    label: Option<String>,
    ty: Ty<'db>,
}

fn member_at<'db>(
    db: &'db dyn Db,
    hir: &Module,
    check: &ItemCheck<'db>,
    relative: TextSize,
) -> Option<Member<'db>> {
    let tag_range = |name_range: TextRange| {
        let start = name_range.start().checked_sub(TextSize::from(1))?;
        Some(TextRange::new(start, name_range.end()))
    };
    for (index, pattern) in hir.patterns().iter().enumerate() {
        if !pattern.range.contains_inclusive(relative) {
            continue;
        }
        match &pattern.kind {
            PatternKind::Tag { name_range, .. } => {
                let range: TextRange = tag_range(*name_range)?;
                if range.contains_inclusive(relative) {
                    let ty = check.pattern_types.get(PatId(index as u32))?;
                    return Some(Member {
                        range,
                        label: None,
                        ty,
                    });
                }
            }
            PatternKind::Record { fields, .. } => {
                if let Some(field) = hir[fields]
                    .iter()
                    .find(|field| field.name_range.contains_inclusive(relative))
                {
                    return Some(Member {
                        range: field.name_range,
                        label: Some(field.name.to_string()),
                        ty: check.pattern_types.get(field.pattern)?,
                    });
                }
            }
            _ => {}
        }
    }
    for (index, (kind, &range)) in hir.kinds().iter().zip(hir.ranges()).enumerate() {
        if !range.contains_inclusive(relative) {
            continue;
        }
        match kind {
            ExpressionKind::Function { parameters, .. } => {
                if let Some(parameter) = hir[parameters]
                    .iter()
                    .find(|parameter| parameter.range.contains_inclusive(relative))
                {
                    let TyKind::Function(function) =
                        check.expression_types.get(&ExprId(index as u32))?.kind(db)
                    else {
                        return None;
                    };
                    let field = function
                        .named
                        .iter()
                        .find(|field| field.name.text(db) == parameter.name.as_str())?;
                    return Some(Member {
                        range: parameter.range,
                        label: Some(parameter.name.to_string()),
                        ty: field.ty,
                    });
                }
            }
            ExpressionKind::For {
                variable: Some(variable),
                variable_range: Some(variable_range),
                ..
            } if variable_range.contains_inclusive(relative) => {
                return Some(Member {
                    range: *variable_range,
                    label: Some(variable.to_string()),
                    ty: *check.loop_variables.get(&ExprId(index as u32))?,
                });
            }
            ExpressionKind::Record(fields) => {
                if let Some(field) = hir[fields]
                    .iter()
                    .find(|field| field.name_range.contains_inclusive(relative))
                {
                    return Some(Member {
                        range: field.name_range,
                        label: Some(field.name.to_string()),
                        ty: *check.expression_types.get(&field.value)?,
                    });
                }
            }
            ExpressionKind::Field {
                target,
                name: Some(name),
                name_range: Some(name_range),
                ..
            } if name_range.contains_inclusive(relative) => {
                // An assignment target records no type of its own, but its record still says
                // what the field holds.
                let ty = check
                    .expression_types
                    .get(&ExprId(index as u32))
                    .copied()
                    .or_else(|| field_type(db, *check.expression_types.get(target)?, name))?;
                return Some(Member {
                    range: *name_range,
                    label: Some(name.to_string()),
                    ty,
                });
            }
            ExpressionKind::Literal(LiteralKind::String(name)) => {
                let name = &hir[name];
                if let Some(columns) = dataframe_columns_for(db, hir, check, range)
                    && let Some(column) = columns.iter().find(|column| column.name.text(db) == name)
                {
                    return Some(Member {
                        range,
                        label: Some(name.to_string()),
                        ty: column.ty,
                    });
                }
            }
            ExpressionKind::Index {
                double: false,
                arguments,
                ..
            } => {
                if let Some(member) = updated_column_at(
                    db,
                    hir,
                    check,
                    ExprId(index as u32),
                    &hir[arguments],
                    relative,
                ) {
                    return Some(member);
                }
            }
            _ => {}
        }
    }
    None
}

fn field_type<'db>(db: &'db dyn Db, ty: Ty<'db>, name: &str) -> Option<Ty<'db>> {
    match ty.kind(db) {
        TyKind::Record(fields) | TyKind::Frame(_, Some(fields)) => fields
            .iter()
            .find(|field| field.name.text(db) == name)
            .map(|field| field.ty),
        TyKind::NativeRecord(fields) => fields
            .iter()
            .find(|field| field.name.text(db) == name)
            .map(|field| field.ty),
        _ => None,
    }
}

/// `x[, name := value]` and `x[, c("a", "b") := ...]` name the columns they write; the query's
/// result frame says what each now holds.
fn updated_column_at<'db>(
    db: &'db dyn Db,
    hir: &Module,
    check: &ItemCheck<'db>,
    query: ExprId,
    arguments: &[Argument],
    relative: TextSize,
) -> Option<Member<'db>> {
    let TyKind::Frame(_, Some(columns)) = check.expression_types.get(&query)?.kind(db) else {
        return None;
    };
    let column_ty = |name: &str| {
        columns
            .iter()
            .find(|column| column.name.text(db) == name)
            .map(|column| column.ty)
    };
    for argument in arguments {
        let Some(value) = argument.value else {
            continue;
        };
        let ExpressionKind::Call {
            callee,
            arguments: update,
        } = &hir.kind(value)
        else {
            continue;
        };
        if !matches!(&hir.kind(*callee), ExpressionKind::NameRef(name) if name.as_str() == ":=") {
            continue;
        }
        let Some(target) = hir[update].first().and_then(|argument| argument.value) else {
            continue;
        };
        if !hir.range(target).contains_inclusive(relative) {
            continue;
        }
        let (name, range) = match hir.kind(target) {
            ExpressionKind::NameRef(name) => (name.to_string(), hir.range(target)),
            // `c("a", "b") := ...`: the string under the cursor names the column.
            ExpressionKind::Call { arguments, .. } => hir[arguments]
                .iter()
                .filter_map(|argument| argument.value)
                .find(|&value| hir.range(value).contains_inclusive(relative))
                .and_then(|value| match hir.kind(value) {
                    ExpressionKind::Literal(LiteralKind::String(text)) => {
                        Some((hir[text].to_string(), hir.range(value)))
                    }
                    _ => None,
                })?,
            _ => return None,
        };
        return Some(Member {
            ty: column_ty(&name)?,
            label: Some(name),
            range,
        });
    }
    None
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
    let callee_target =
        item_check(db, item).and_then(|check| check.callee_targets.get(&expression).copied());
    let binding = match callee_target {
        Some(CalleeTarget::Slot(binding)) => Some(binding),
        Some(CalleeTarget::Project) => return global_hover_definition(db, files, name, None),
        Some(CalleeTarget::Library) => return stub_hover_definition(db, name),
        None => naming.resolutions.get(&expression).copied(),
    };
    if let Some(binding) = &binding {
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
    global_hover_definition(db, files, name, None).or_else(|| stub_hover_definition(db, name))
}

fn stub_hover_definition(db: &dyn Db, name: &str) -> Option<HoverDefinition> {
    Some(HoverDefinition::Stub {
        namespace: declaring_namespace(db, name)?.to_owned(),
        overloads: stubs(db)?.schemes.get(name).map_or(0, Vec::len),
        declaration: stub_target(db, name),
        documentation: stubs(db)?.documentation.get(name).cloned(),
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
            body: format!("{:#?} @ {:?}", hir.kind(expression), hir.range(expression)),
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
        Target::TypeParameter(occurrences) => occurrences
            .into_iter()
            .find(|occurrence| occurrence.is_declaration)
            .map(declaration_target),
        Target::Tag(name) => tag_occurrences(db, files, &name)
            .into_iter()
            .find(|occurrence| occurrence.is_declaration)
            .map(declaration_target)
            .or_else(|| stub_tag_target(db, &name).map(DefinitionTarget::Stub)),
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
    // The library fixes the names it declares: renaming the project's uses of a library tag or
    // type would leave them pointing at nothing.
    match &target {
        Target::Tag(name) if stub_mentions_tag(db, name) => return None,
        _ => {}
    }
    let occurrences = occurrences(db, files, &target);
    if matches!(target, Target::TypeName(_))
        && !occurrences
            .iter()
            .any(|occurrence| occurrence.is_declaration)
    {
        return None;
    }
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
        let annotated = item_annotations(db, item)
            .root
            .as_deref()
            .is_some_and(|annotation| {
                annotation.declared.is_some()
                    || annotation.new_nominal.is_some()
                    || annotation.trusted
            });
        // A hint on an annotated nested statement would restate the line above.
        let annotated_expressions: BTreeSet<ExprId> = item_annotations(db, item)
            .expressions
            .iter()
            .map(|(id, _)| *id)
            .collect();

        for (index, kind) in hir.kinds().iter().enumerate() {
            if let ExpressionKind::For {
                variable_range: Some(variable_range),
                ..
            } = kind
            {
                if let Some(&ty) = check.loop_variables.get(&ExprId(index as u32))
                    && is_hintable(db, ty, false)
                {
                    hints.push(InlayHint {
                        offset: variable_range.end() + node.text_range().start(),
                        label: format!(": {}", TypeRenderer::default().render(db, ty)),
                    });
                }
                continue;
            }
            let ExpressionKind::Assign { target, value, .. } = kind else {
                continue;
            };
            if !matches!(hir.kind(*target), ExpressionKind::NameRef(_)) {
                continue;
            }
            let id = ExprId(index as u32);
            let is_root = hir.root == Some(id);
            if (is_root && annotated)
                || annotated_expressions.contains(&id)
                || spells_its_type(hir, &node, *value)
            {
                continue;
            }
            let Some(label) = assignment_label(db, &check, id, *value, is_root) else {
                continue;
            };
            hints.push(InlayHint {
                offset: hir.range(*target).end() + node.text_range().start(),
                label: format!(": {label}"),
            });
        }
    }
    hints.sort_by_key(|hint| hint.offset);
    hints
}

/// A value that already names its own type: a nominal constructor `@Money(1)`, whose hint would
/// repeat `Money`, and a payload-free tag such as `:None`, whose hint would repeat the tag.
fn spells_its_type(hir: &Module, item_node: &SyntaxNode, value: ExprId) -> bool {
    if matches!(hir.kind(value), ExpressionKind::Tag { payload: None, .. }) {
        return true;
    }
    let range = hir.range(value) + item_node.text_range().start();
    item_node.text_range().contains_range(range)
        && matches!(
            item_node.covering_element(range),
            SyntaxElement::Node(node) if node.kind() == NEW_EXPR
        )
}

/// What a highlighted token is. R grammars already color R, so only ry's own syntax gets a class:
/// annotation and type-definition bodies, tags, `match` and its arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticClass {
    Type,
    TypeParameter,
    /// A field or parameter name inside a type.
    Parameter,
    Operator,
    /// An `@` directive or the `@` of a nominal constructor.
    Decorator,
    Keyword,
    Tag,
}

pub fn semantic_tokens(db: &dyn Db, file: SourceFile) -> Vec<(TextRange, SemanticClass)> {
    let root = semantics::parse(db, file).syntax_node();
    let mut spans = Vec::new();
    // Tokens arrive in order, so one region's binders serve all of its tokens.
    let mut current_region: Option<(SyntaxNode, Vec<String>)> = None;
    for token in root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
    {
        if token.text_range().is_empty() {
            continue;
        }
        let region = token
            .parent_ancestors()
            .find(|node| node.kind() == ANNOTATION || is_inline_annotation(node));
        let class = match region {
            Some(region) => {
                if current_region
                    .as_ref()
                    .is_none_or(|(node, _)| *node != region)
                {
                    let names = binder_names(&region);
                    current_region = Some((region.clone(), names));
                }
                let binders = current_region.as_ref().map_or(&[][..], |(_, names)| names);
                if region.kind() == TYPE_DEFINITION && region.first_token().as_ref() == Some(&token)
                {
                    Some(SemanticClass::Keyword)
                } else if tag_node_of(&token).is_some() {
                    Some(SemanticClass::Tag)
                } else {
                    classify_type_token(&token).map(|class| {
                        let names_binder = class == SemanticClass::Type
                            && is_type_parameter_token(&token)
                            && binders.iter().any(|name| name == token.text());
                        if names_binder {
                            SemanticClass::TypeParameter
                        } else {
                            class
                        }
                    })
                }
            }
            None => classify_ry_token(&token),
        };
        if let Some(class) = class {
            spans.push((token.text_range(), class));
        }
    }
    spans
}

/// A token of type syntax, classified from its place in the tree alone; stub files, which have no
/// project tree, use this directly.
pub fn classify_type_token(token: &SyntaxToken) -> Option<SemanticClass> {
    if tag_node_of(token).is_some() {
        return Some(SemanticClass::Tag);
    }
    // A name may sit in its own `NAME` node; what it names is decided one level up.
    let owner_kind = token.parent().and_then(|parent| {
        if parent.kind() == NAME {
            parent.parent().map(|grandparent| grandparent.kind())
        } else {
            Some(parent.kind())
        }
    });
    match token.kind() {
        IDENT => match owner_kind {
            Some(TYPE_BINDER) => Some(SemanticClass::TypeParameter),
            Some(TYPE_FIELD | TYPE_FUNCTION) => Some(SemanticClass::Parameter),
            Some(ANNOTATION_DIRECTIVE) => Some(SemanticClass::Decorator),
            _ => Some(SemanticClass::Type),
        },
        COMMA | PIPE | COLON | L_BRACKET | R_BRACKET | L_PAREN | R_PAREN | L_BRACE | R_BRACE
        | LESS | GREATER | DOTS | MINUS_GREATER | FAT_ARROW => Some(SemanticClass::Operator),
        AT => Some(SemanticClass::Decorator),
        NULL_KW => Some(SemanticClass::Type),
        _ => None,
    }
}

/// ry's own syntax outside a type: `match`, its `=>`, and tags in values and patterns.
fn classify_ry_token(token: &SyntaxToken) -> Option<SemanticClass> {
    if tag_node_of(token).is_some() {
        return Some(SemanticClass::Tag);
    }
    let parent = token.parent()?;
    match token.kind() {
        FAT_ARROW => Some(SemanticClass::Operator),
        IDENT if parent.kind() == MATCH_EXPR && parent.first_token().as_ref() == Some(token) => {
            Some(SemanticClass::Keyword)
        }
        _ => None,
    }
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
        .expressions()
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
        let ExpressionKind::Call { callee, arguments } = &hir.kind(*id) else {
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
            .map(|scheme| scheme_signature(db, scheme, &hir[arguments], hir, position.relative))
            .collect();
        let selected = check.selected_overloads.get(&callee).copied().unwrap_or(0);
        // The committed candidate renders instantiated with this call site's types.
        if let Some(function) = function
            && let Some(entry) = signatures.get_mut(selected)
        {
            *entry = scheme_signature(
                db,
                &TypeScheme::monomorphic(function),
                &hir[arguments],
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
            &hir[arguments],
            hir,
            position.relative,
        )],
        active_signature: 0,
    })
}

fn callee_name(hir: &Module, callee: ExprId) -> Option<String> {
    match &hir.kind(callee) {
        ExpressionKind::NameRef(name) => Some(name.to_string()),
        ExpressionKind::Namespace { name, .. } => name.map(|name| name.to_string()),
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
    /// A tag name, completed after the `:` that starts a tag.
    Tag,
}

/// Variant order is the ranking order among items of equal match quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionSource {
    /// A parameter name of the call the cursor is an argument of, completed as `name = `.
    Argument,
    Local,
    Global,
    /// After project names: an empty or short prefix would otherwise lead with `break`.
    Keyword,
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

    if let Some(items) = tag_completions(db, files, file, offset, &query) {
        return finish_completions(items, &query);
    }
    if let Some(items) = pattern_completions(db, file, offset, &query) {
        return finish_completions(items, &query);
    }
    if let Some(items) = literal_field_completions(db, file, offset, &query) {
        return finish_completions(items, &query);
    }

    // Resolving a scheme can type-check its item, so call shapes are filled in only for the
    // globals that survive truncation, not for every global that matches the query.
    let mut globals: Vec<(&str, Option<Item<'_>>)> = Vec::new();
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
            items.extend(frame_query_completions(db, file, offset, &query));
            items.extend(argument_name_completions(db, file, offset, &query));
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
                                info.name.to_string(),
                                CompletionKind::Variable,
                                CompletionSource::Local,
                            )
                        }),
                );
            }
            for &project_file in files.files(db) {
                for global in global_names(db, project_file) {
                    if search_match(&global.name, &query).is_none() {
                        continue;
                    }
                    globals.push((global.name.as_str(), global.item));
                    let kind = match global.item {
                        Some(_) => {
                            if global.is_function {
                                CompletionKind::Function
                            } else {
                                CompletionKind::Variable
                            }
                        }
                        None => CompletionKind::Variable,
                    };
                    items.push(CompletionItem::new(
                        global.name.clone(),
                        kind,
                        CompletionSource::Global,
                    ));
                }
            }
            // A project global outranks a same-named stub at deduplication, mirroring resolution.
            if let Some(library) = stubs(db) {
                let documentation = |name: &String| {
                    library
                        .exports_by_namespace
                        .iter()
                        .find(|(_, names)| names.contains(name))
                        .map(|(namespace, _)| {
                            let origin = format!("From the `{namespace}` package.");
                            match library.documentation.get(name) {
                                Some(text) => format!("{text}\n\n{origin}"),
                                None => origin,
                            }
                        })
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

    let mut result = finish_completions(items, &query)?;
    let mut unresolved: HashMap<&str, usize> = result
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.source == CompletionSource::Global)
        .map(|(index, item)| (item.label.as_str(), index))
        .collect();
    let mut shapes = Vec::new();
    // The first writer in project order is the one deduplication kept, and a statement writer
    // (no item) has no call shape, so it must still claim its name ahead of a later function.
    for (name, item) in globals {
        if let Some(index) = unresolved.remove(name)
            && let Some(item) = item
        {
            shapes.push((index, scheme_takes_arguments(db, &global_scheme(db, item))));
        }
    }
    for (index, takes_arguments) in shapes {
        if let Some(item) = result.items.get_mut(index) {
            item.takes_arguments = takes_arguments;
        }
    }
    Some(result)
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
    for annotation in root.descendants().filter(|node| {
        matches!(node.kind(), ANNOTATION | TYPE_ASCRIPTION)
            && ranges_overlap(node.text_range(), viewport)
    }) {
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
            let Some(assign_range) = hir.expressions().find_map(|expression| {
                let ExpressionKind::Assign { target, .. } = &expression.kind else {
                    return None;
                };
                (hir.range(*target) == unused.range || expression.range == unused.range)
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
        let annotated_expressions: BTreeSet<ExprId> = item_annotations(db, item)
            .expressions
            .iter()
            .map(|(id, _)| *id)
            .collect();
        for (index, expression) in hir.expressions().enumerate() {
            let ExpressionKind::Assign { target, value, .. } = &expression.kind else {
                continue;
            };
            let ExpressionKind::NameRef(name) = &hir.kind(*target) else {
                continue;
            };
            let id = ExprId(index as u32);
            let is_root = hir.root == Some(id);
            if (is_root && annotated)
                || annotated_expressions.contains(&id)
                || spells_its_type(hir, &node, *value)
            {
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
    for definition in root
        .children()
        .filter(|node| node.kind() == TYPE_DEFINITION)
    {
        let keyword = type_definition_keyword(&definition);
        let kind = if keyword == "alias" {
            DocumentSymbolKind::AliasDefinition
        } else {
            DocumentSymbolKind::TypeDefinition
        };
        let Some(name_token) =
            child_node(&definition, NAME).and_then(|name| child_token(&name, IDENT))
        else {
            continue;
        };
        symbols.push(DocumentSymbol {
            name: name_token.text().to_owned(),
            kind,
            detail: Some(keyword),
            range: definition.text_range(),
            selection: name_token.text_range(),
            children: Vec::new(),
        });
    }
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
                TYPE_APPLY | TYPE_DEFINITION => true,
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

/// The type parameters a region's types may name: a `#:` block's or a type definition's
/// own binders, or the `<...>` of the function an inline parameter or return type belongs to.
fn binder_names(region: &SyntaxNode) -> Vec<String> {
    let Some(scope) = binder_scope(region) else {
        return Vec::new();
    };
    let binders = if matches!(scope.kind(), ANNOTATION | TYPE_DEFINITION) {
        Some(scope)
    } else {
        child_node(&scope, TYPE_BINDER_LIST)
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

/// The node whose `<...>` binders a region's types see: the region itself for a `#:` block or
/// a type definition, otherwise the function an inline parameter or return type belongs to.
fn binder_scope(region: &SyntaxNode) -> Option<SyntaxNode> {
    if matches!(region.kind(), ANNOTATION | TYPE_DEFINITION) {
        return Some(region.clone());
    }
    // A binding's type and the type of `@Name(value)` see no enclosing function's binders, as
    // in the checker.
    if region.kind() == NEW_TYPE
        || region
            .parent()
            .is_some_and(|parent| parent.kind() == BINARY_EXPR)
    {
        return None;
    }
    region.ancestors().find(|node| node.kind() == FUNCTION_DEF)
}

struct TypeParameterCursor {
    name: String,
    range: TextRange,
    scope: SyntaxNode,
}

/// A `<T>` binder, or a type that names one in scope.
fn type_parameter_at(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
) -> Option<TypeParameterCursor> {
    let root = semantics::parse(db, file).syntax_node();
    let region = annotation_at(&root, offset)?;
    let token = region
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| token.kind() == IDENT && token.text_range().contains_inclusive(offset))?;
    if !is_type_parameter_token(&token) {
        return None;
    }
    let name = token.text().to_owned();
    if !binder_names(&region).contains(&name) {
        return None;
    }
    Some(TypeParameterCursor {
        name,
        range: token.text_range(),
        scope: binder_scope(&region)?,
    })
}

fn is_type_parameter_token(token: &SyntaxToken) -> bool {
    let Some(parent) = token.parent() else {
        return false;
    };
    parent.kind() == TYPE_REF
        || (parent.kind() == NAME
            && parent
                .parent()
                .is_some_and(|grandparent| grandparent.kind() == TYPE_BINDER))
}

fn type_parameter_occurrences(file: SourceFile, cursor: &TypeParameterCursor) -> Vec<Occurrence> {
    let mut result = Vec::new();
    for region in cursor.scope.descendants().filter(is_type_region) {
        if binder_scope(&region).as_ref() != Some(&cursor.scope) {
            continue;
        }
        result.extend(
            region
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|token| {
                    token.kind() == IDENT
                        && token.text() == cursor.name
                        && is_type_parameter_token(token)
                })
                .map(|token| Occurrence {
                    file,
                    range: token.text_range(),
                    is_declaration: token.parent().is_some_and(|parent| parent.kind() == NAME),
                }),
        );
    }
    result.sort_by_key(|occurrence| occurrence.range.start());
    result.dedup_by_key(|occurrence| occurrence.range);
    result
}

/// A tag's name: `:Some` in a value, a pattern, or a type. Tags are structural, so every tag of
/// the same name in the project is one symbol.
fn tag_at(db: &dyn Db, file: SourceFile, offset: TextSize) -> Option<(String, TextRange)> {
    let root = semantics::parse(db, file).syntax_node();
    let token = root.token_at_offset(offset).find_map(|token| {
        let node = tag_node_of(&token)?;
        let name = child_node(&node, NAME)?;
        Some(name.text().to_string()).zip(Some(name.text_range()))
    })?;
    Some(token)
}

/// The tag node a token spells: its `:` or its name.
fn tag_node_of(token: &SyntaxToken) -> Option<SyntaxNode> {
    let parent = token.parent()?;
    let node = match token.kind() {
        COLON => parent,
        IDENT if parent.kind() == NAME => parent.parent()?,
        _ => return None,
    };
    is_tag_node(node.kind()).then_some(node)
}

fn is_tag_node(kind: SyntaxKind) -> bool {
    matches!(kind, TAG_EXPR | PATTERN_TAG | TYPE_TAG)
}

fn tag_occurrences(db: &dyn Db, files: ProjectFiles, name: &str) -> Vec<Occurrence> {
    let mut result = Vec::new();
    for &file in files.files(db) {
        let root = semantics::parse(db, file).syntax_node();
        for node in root.descendants().filter(|node| is_tag_node(node.kind())) {
            let Some(tag_name) = child_node(&node, NAME) else {
                continue;
            };
            if tag_name.text() != name {
                continue;
            }
            // Listing a tag in a named type definition is the closest thing a structural tag has
            // to a declaration.
            let is_declaration = node.kind() == TYPE_TAG
                && node.ancestors().any(|ancestor| {
                    ancestor.kind() == TYPE_DEFINITION
                        || (ancestor.kind() == ANNOTATION_DIRECTIVE && is_type_directive(&ancestor))
                });
            result.push(Occurrence {
                file,
                range: tag_name.text_range(),
                is_declaration,
            });
        }
    }
    result
}

/// Where the stubs list a tag in a `@type` or `@alias` declaration, such as `:Ok` in the
/// standard library's `Result`.
fn stub_tag_target(db: &dyn Db, name: &str) -> Option<StubTarget> {
    let sources = StubSources::try_get(db)?;
    sources
        .sources(db)
        .iter()
        .enumerate()
        .find_map(|(source_index, (_, text))| {
            let mut line_start = 0;
            for line in text.split_inclusive('\n') {
                let trimmed = line.trim_start();
                if (trimmed.starts_with("@alias") || trimmed.starts_with("@type"))
                    && let Some(at) = tag_spelling(line, name)
                {
                    let start = TextSize::from((line_start + at) as u32);
                    return Some(StubTarget {
                        source_index,
                        range: TextRange::at(start, TextSize::from(name.len() as u32)),
                    });
                }
                line_start += line.len();
            }
            None
        })
        .or_else(|| {
            sources
                .sources(db)
                .iter()
                .enumerate()
                .find_map(|(source_index, (_, text))| {
                    let at = tag_spelling(text, name)?;
                    Some(StubTarget {
                        source_index,
                        range: TextRange::at(
                            TextSize::from(at as u32),
                            TextSize::from(name.len() as u32),
                        ),
                    })
                })
        })
}

/// Whether any stub mentions the tag, which fixes its name: renaming it in the project would
/// break every call into the library that produces or expects it.
fn stub_mentions_tag(db: &dyn Db, name: &str) -> bool {
    StubSources::try_get(db).is_some_and(|sources| {
        sources
            .sources(db)
            .iter()
            .any(|(_, text)| tag_spelling(text, name).is_some())
    })
}

/// The offset of `name` in a `:name` spelling inside `text`, matched as a whole word.
fn tag_spelling(text: &str, name: &str) -> Option<usize> {
    let needle = format!(":{name}");
    let mut from = 0;
    while let Some(found) = text.get(from..)?.find(&needle) {
        let at = from + found;
        let end = at + needle.len();
        let continues = text[end..]
            .chars()
            .next()
            .is_some_and(|next| next.is_alphanumeric() || next == '_' || next == '.');
        let preceded = text[..at].ends_with(':');
        if !continues && !preceded {
            return Some(at + 1);
        }
        from = end;
    }
    None
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
    // A project definition shadows the library's, as in the checker.
    let library_definition = || {
        let library = stubs(db)?;
        library
            .type_definitions
            .values()
            .find(|definition| definition.name.text(db) == cursor.name)
            .cloned()
    };
    let line = match declared
        .clone()
        .or_else(|| cursor.navigable.then(library_definition).flatten())
    {
        Some(definition) => {
            // The renderer names the parameters in order, so the body must be rendered through
            // the same renderer for its references to agree with the `<...>` list.
            let mut renderer = TypeRenderer::default();
            let binders: Vec<(Name<'_>, Constraint)> = definition
                .parameters
                .iter()
                .map(|parameter| (*parameter, Constraint::Unconstrained))
                .collect();
            let parameters = renderer.render_binder_prefix(&binders).unwrap_or_default();
            let body = renderer.render(db, definition.body);
            match (definition.inline, definition.alias) {
                (true, true) => format!("alias {}{parameters} = {body}", cursor.name),
                (true, false) => format!("type {}{parameters} = {body}", cursor.name),
                (false, true) => format!("@alias {}{parameters} {{{body}}}", cursor.name),
                (false, false) => format!("@type {}{parameters} {{{body}}}", cursor.name),
            }
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
            documentation: None,
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
                        Some(parent) if parent.kind() == TYPE_DEFINITION => true,
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
        .expressions()
        .find_map(|expression| match &expression.kind {
            ExpressionKind::Index {
                double: true,
                target,
                arguments,
            } if hir[arguments].iter().any(|argument| {
                argument
                    .value
                    .is_some_and(|value| hir.range(value) == string_range)
            }) =>
            {
                Some(*target)
            }
            _ => None,
        });
    let fields = match target
        .map(|target| check.expression_types.get(&target).map(|ty| ty.kind(db)))
    {
        Some(Some(TyKind::Record(fields) | TyKind::Frame(_, Some(fields)))) => fields.as_slice(),
        Some(_) => return None,
        None => dataframe_columns_for(db, hir, &check, string_range)?,
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

/// The columns a string names when it is a column argument of a DataFrame verb, such as
/// `DataFrame.select(sales, "price")`, or the name in `col("price")` inside one. The verb's first
/// argument is the frame whose columns are in scope.
fn dataframe_columns_for<'a, 'db>(
    db: &'db dyn Db,
    hir: &Module,
    check: &'a ItemCheck<'db>,
    string_range: TextRange,
) -> Option<&'a [RecordField<'db>]> {
    let is_column_string = hir.expressions().any(|expression| match &expression.kind {
        ExpressionKind::Call { callee, arguments } => {
            let in_arguments = |arguments: &[Argument]| {
                arguments.iter().any(|argument| {
                    argument
                        .value
                        .is_some_and(|value| hir.range(value) == string_range)
                })
            };
            match &hir.kind(*callee) {
                ExpressionKind::NameRef(name) if name.as_str() == "col" => hir[arguments]
                    .first()
                    .is_some_and(|argument| in_arguments(std::slice::from_ref(argument))),
                ExpressionKind::NameRef(name) if is_dataframe_verb(name) => {
                    hir[arguments].get(1..).is_some_and(in_arguments)
                }
                _ => false,
            }
        }
        _ => false,
    });
    if !is_column_string {
        return None;
    }
    let (_, subject) = hir
        .expressions()
        .filter_map(|expression| match &expression.kind {
            ExpressionKind::Call { callee, arguments }
                if expression.range.contains_range(string_range)
                    && matches!(&hir.kind(*callee), ExpressionKind::NameRef(name) if is_dataframe_verb(name)) =>
            {
                let subject = hir[arguments].first()?.value?;
                (!hir.range(subject).contains_range(string_range))
                    .then_some((expression.range.len(), subject))
            }
            _ => None,
        })
        .min_by_key(|(length, _)| *length)?;
    match check.expression_types.get(&subject)?.kind(db) {
        TyKind::Frame(_, Some(columns)) => Some(columns.as_slice()),
        _ => None,
    }
}

fn is_dataframe_verb(name: &str) -> bool {
    name.split_once('.').is_some_and(|(receiver, _)| {
        matches!(
            receiver,
            "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy"
        )
    })
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
    let mut definitions: Vec<(String, Ty<'_>, CompletionSource)> =
        project_type_definitions(db, files)
            .iter()
            .map(|(name, definition)| {
                (
                    name.text(db).to_owned(),
                    definition.body,
                    CompletionSource::Global,
                )
            })
            .collect();
    for definition in file_type_definitions(db, file) {
        let label = definition.name.text(db).to_owned();
        if !definitions.iter().any(|(name, _, _)| *name == label) {
            definitions.push((label, definition.body, CompletionSource::Global));
        }
    }
    if let Some(library) = stubs(db) {
        for (name, definition) in &library.type_definitions {
            let label = name.text(db).to_owned();
            if !definitions.iter().any(|(known, _, _)| *known == label) {
                definitions.push((label, definition.body, CompletionSource::Stdlib));
            }
        }
    }
    items.extend(
        definitions
            .into_iter()
            .filter(|(label, _, _)| search_match(label, query).is_some())
            .map(|(label, body, source)| CompletionItem {
                detail: Some(TypeRenderer::default().render(db, body)),
                ..CompletionItem::new(label, CompletionKind::Variable, source)
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
        let spelled = spelled_names(db, project_file);
        let names = match kind {
            AT_EXPR => &spelled.at,
            DOLLAR_EXPR => &spelled.dollar,
            _ => &spelled.namespace,
        };
        labels.extend(
            names
                .iter()
                .filter(|label| search_match(label, query).is_some())
                .map(String::as_str),
        );
    }
    labels
        .into_iter()
        .map(|label| CompletionItem::new(label.to_owned(), CompletionKind::Field, source))
        .collect()
}

/// Fills the per-file indexes completion reads, so the first completion in a large project does
/// not build them for every file at once.
pub fn warm_completion_indexes(db: &dyn Db, file: SourceFile) {
    global_names(db, file);
    spelled_names(db, file);
}

/// A top-level name a file writes, in item order.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct GlobalName<'db> {
    name: String,
    is_function: bool,
    /// `None` for a name a statement item writes (a conditional write inside a top-level `if`),
    /// which completes as a plain variable.
    item: Option<Item<'db>>,
}

/// Memoized per file: reading every item's salsa fields on each request costs milliseconds in
/// a large project, and an edit recomputes only the edited file's names.
#[salsa::tracked(returns(ref))]
fn global_names<'db>(db: &'db dyn Db, file: SourceFile) -> Vec<GlobalName<'db>> {
    let mut names = Vec::new();
    for &item in item_tree(db, file) {
        let is_function = match *item.kind(db) {
            ItemKind::Function => true,
            ItemKind::Value => false,
            ItemKind::Statement => {
                if let Some(naming) = item_naming(db, item) {
                    names.extend(
                        naming
                            .bindings
                            .values()
                            .filter(|info| info.kind == BindingKind::TopLevel)
                            .map(|info| GlobalName {
                                name: info.name.to_string(),
                                is_function: false,
                                item: None,
                            }),
                    );
                }
                continue;
            }
        };
        if let Some(name) = item.name(db) {
            names.push(GlobalName {
                name: name.clone(),
                is_function,
                item: Some(item),
            });
        }
    }
    names
}

/// The names a file spells after `@`, `$` and `::`, which untyped completion offers.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct SpelledNames {
    at: Vec<String>,
    dollar: Vec<String>,
    namespace: Vec<String>,
}

/// Memoized per file, so completion does not re-walk every syntax tree in the project.
#[salsa::tracked(returns(ref))]
fn spelled_names(db: &dyn Db, file: SourceFile) -> SpelledNames {
    let mut spelled = SpelledNames {
        at: Vec::new(),
        dollar: Vec::new(),
        namespace: Vec::new(),
    };
    let root = semantics::parse(db, file).syntax_node();
    for node in root.descendants() {
        let names = match node.kind() {
            AT_EXPR => &mut spelled.at,
            DOLLAR_EXPR => &mut spelled.dollar,
            NAMESPACE_EXPR => &mut spelled.namespace,
            _ => continue,
        };
        // The lhs is an expression node, so the last bare NAME child is the spelled name.
        if let Some(name) = node.children().filter(|child| child.kind() == NAME).last() {
            names.push(name.text().to_string());
        }
    }
    for names in [&mut spelled.at, &mut spelled.dollar, &mut spelled.namespace] {
        names.sort_unstable();
        names.dedup();
    }
    spelled
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
            .expressions()
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
        let target_ty = representation(db, file, *check.expression_types.get(&target)?);
        let fields: Vec<(&str, Ty<'_>)> = match target_ty.kind(db) {
            TyKind::Record(fields) | TyKind::Frame(_, Some(fields)) => fields
                .iter()
                .map(|field| (field.name.text(db), field.ty))
                .collect(),
            TyKind::NativeRecord(fields) => fields
                .iter()
                .map(|field| (field.name.text(db), field.ty))
                .collect(),
            // A value whose type says it has no `$` fields completes nothing; only an untyped one
            // falls back to the field names the project spells.
            TyKind::NativeTuple(_) | TyKind::Tag(..) => Vec::new(),
            TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::Function(_) | TyKind::Null => {
                Vec::new()
            }
            _ => return None,
        };
        let mut renderer = TypeRenderer::default();
        Some(
            fields
                .into_iter()
                .map(|(name, ty)| CompletionItem {
                    detail: Some(renderer.render(db, ty)),
                    ..CompletionItem::new(
                        insertable_name(name),
                        CompletionKind::Field,
                        CompletionSource::Field,
                    )
                })
                .filter(|item| search_match(&item.label, query).is_some())
                .collect::<Vec<_>>(),
        )
    })();
    match typed {
        Some(items) => items,
        None => spelled_completions(db, files, query, DOLLAR_EXPR, CompletionSource::Field),
    }
}

/// What `$` reads on a nominal value: its definition's body, as the checker projects it.
fn representation<'db>(db: &'db dyn Db, file: SourceFile, ty: Ty<'db>) -> Ty<'db> {
    let mut current = ty;
    // Bounded, because a definition may name another definition as its whole body.
    for _ in 0..16 {
        let TyKind::Named(name, arguments) = current.kind(db) else {
            break;
        };
        let Some(definition) = visible_type_definitions(db, file).get(name) else {
            break;
        };
        if definition.parameters.len() != arguments.len() {
            break;
        }
        current = apply_definition(db, definition, arguments);
    }
    current
}

/// After a lone `:` that starts a tag. Inside a `match` pattern it offers the tags the value at
/// that position can hold, nested payloads included; elsewhere it offers every tag the project
/// and the standard library spell. `::` is a namespace and `1:n` is a range, so neither
/// completes tags.
fn tag_completions(
    db: &dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
    query: &str,
) -> Option<Vec<CompletionItem>> {
    let text = file.text(db);
    let before = text.get(..usize::from(offset).checked_sub(query.len())?)?;
    let operand = before.strip_suffix(':')?;
    if operand
        .chars()
        .next_back()
        .is_some_and(|previous| previous.is_alphanumeric() || "_.:)]'\"`".contains(previous))
    {
        return None;
    }
    let word_start = TextSize::from(before.len() as u32);
    if let Some(subject) = pattern_subject(db, file, word_start) {
        let mut renderer = TypeRenderer::default();
        let items: Vec<CompletionItem> = subject
            .types
            .into_iter()
            .filter_map(|member| {
                let TyKind::Tag(name, _) = member.kind(db) else {
                    return None;
                };
                Some(CompletionItem {
                    detail: Some(renderer.render(db, member)),
                    ..CompletionItem::new(
                        name.text(db).to_owned(),
                        CompletionKind::Tag,
                        CompletionSource::Field,
                    )
                })
            })
            .filter(|item| search_match(&item.label, query).is_some())
            .collect();
        if !items.is_empty() {
            return Some(items);
        }
    }
    let mut names = BTreeSet::new();
    for &project_file in files.files(db) {
        let root = semantics::parse(db, project_file).syntax_node();
        names.extend(
            root.descendants()
                .filter(|node| is_tag_node(node.kind()))
                .filter_map(|node| child_node(&node, NAME))
                // The half-typed tag under the cursor is not a suggestion for itself.
                .filter(|name| project_file != file || name.text_range().start() != word_start)
                .map(|name| name.text().to_string()),
        );
    }
    if let Some(sources) = StubSources::try_get(db) {
        for (_, source) in sources.sources(db).iter() {
            names.extend(spelled_tags(source));
        }
    }
    Some(
        names
            .into_iter()
            .filter(|name| search_match(name, query).is_some())
            .map(|name| CompletionItem::new(name, CompletionKind::Tag, CompletionSource::Field))
            .collect(),
    )
}

/// Tag names a stub spells as `:Name` in a type: after a space or an opening bracket, so a
/// method name such as `-:base` is not mistaken for one.
fn spelled_tags(text: &str) -> impl Iterator<Item = String> + '_ {
    text.match_indices(':').filter_map(|(at, _)| {
        let preceding = text[..at].chars().next_back()?;
        if !(preceding.is_whitespace() || "({[|,<".contains(preceding)) {
            return None;
        }
        let name: String = text[at + 1..]
            .chars()
            .take_while(|next| next.is_alphanumeric() || *next == '_')
            .collect();
        name.chars()
            .next()
            .is_some_and(char::is_alphabetic)
            .then_some(name)
    })
}

/// Inside a `match` pattern, completion offers only what the pattern can name: field names of the
/// record a payload or record pattern matches. A binding introduces a fresh name, so it offers
/// nothing at all rather than the R names in scope.
fn pattern_completions(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
    query: &str,
) -> Option<Vec<CompletionItem>> {
    let word_start = offset.checked_sub(TextSize::from(query.len() as u32))?;
    let root = semantics::parse(db, file).syntax_node();
    let anchor = pattern_anchor(&root, word_start, query)?;
    pattern_arm(&anchor, word_start)?;
    let Some(list) = anchor
        .parent_ancestors()
        .take_while(|ancestor| ancestor.kind() != MATCH_ARM)
        .find(|ancestor| ancestor.kind() == PATTERN_LIST)
    else {
        return Some(Vec::new());
    };
    let item = list
        .children()
        .find(|child| child.text_range().contains_inclusive(word_start));
    let at_name = match item.as_ref().map(SyntaxNode::kind) {
        None | Some(PATTERN_BIND) => true,
        Some(PATTERN_FIELD) => item
            .as_ref()
            .and_then(|field| child_token(field, EQ))
            .is_none_or(|equals| word_start <= equals.text_range().start()),
        _ => false,
    };
    if !at_name {
        return Some(Vec::new());
    }
    let Some(subject) = pattern_subject_of_list(db, file, &list, word_start) else {
        return Some(Vec::new());
    };
    let named: Vec<String> = list
        .children()
        .filter(|child| child.kind() == PATTERN_FIELD && Some(child) != item.as_ref())
        .filter_map(|field| child_node(&field, NAME))
        .map(|name| name.text().to_string())
        .collect();
    let mut renderer = TypeRenderer::default();
    let mut items = Vec::new();
    for member in subject.types {
        let TyKind::NativeRecord(fields) = member.kind(db) else {
            continue;
        };
        for field in fields {
            let name = field.name.text(db);
            if named.iter().any(|used| used == name) {
                continue;
            }
            items.push(CompletionItem {
                detail: Some(renderer.render(db, field.ty)),
                ..CompletionItem::new(
                    insertable_name(name),
                    CompletionKind::Field,
                    CompletionSource::Field,
                )
            });
        }
    }
    Some(
        items
            .into_iter()
            .filter(|item| search_match(&item.label, query).is_some())
            .collect(),
    )
}

/// The token completion is anchored on: the word being typed, or the token just before an empty
/// one.
fn pattern_anchor(root: &SyntaxNode, word_start: TextSize, query: &str) -> Option<SyntaxToken> {
    let at = root.token_at_offset(word_start);
    if query.is_empty() {
        at.left_biased()
    } else {
        at.right_biased()
    }
}

/// The `match` arm whose pattern holds the position: before its `=>`, or anywhere in an arm that
/// does not have one yet.
fn pattern_arm(anchor: &SyntaxToken, word_start: TextSize) -> Option<SyntaxNode> {
    let arm = anchor
        .parent_ancestors()
        .take_while(|ancestor| !matches!(ancestor.kind(), MATCH_EXPR | FUNCTION_DEF))
        .find(|ancestor| ancestor.kind() == MATCH_ARM)?;
    child_token(&arm, FAT_ARROW)
        .is_none_or(|arrow| word_start <= arrow.text_range().start())
        .then_some(arm)
}

/// The possible types of the value a pattern position matches, as union members.
struct PatternSubject<'db> {
    types: Vec<Ty<'db>>,
}

/// One step from a pattern down to a position nested inside it.
enum PatternStep {
    Payload(String),
    Item(usize),
    Field(String),
}

/// The value a tag typed at `word_start` would match, followed from the scrutinee's type down
/// the pattern's syntax. Following the syntax rather than the checked pattern keeps this working
/// in the half-typed patterns completion runs on, which the checker cannot match.
fn pattern_subject(
    db: &dyn Db,
    file: SourceFile,
    word_start: TextSize,
) -> Option<PatternSubject<'_>> {
    let root = semantics::parse(db, file).syntax_node();
    let anchor = root.token_at_offset(word_start).left_biased()?;
    let arm = pattern_arm(&anchor, word_start)?;
    let mut steps = Vec::new();
    for list in anchor
        .parent_ancestors()
        .take_while(|ancestor| ancestor != &arm)
        .filter(|ancestor| ancestor.kind() == PATTERN_LIST)
    {
        steps.push(list_item_step(&list, word_start));
        steps.extend(list_payload_step(&list));
    }
    steps.reverse();
    subject_along(db, file, &arm, &steps)
}

/// The value a pattern list as a whole matches: the record whose fields its names may spell.
fn pattern_subject_of_list<'db>(
    db: &'db dyn Db,
    file: SourceFile,
    list: &SyntaxNode,
    word_start: TextSize,
) -> Option<PatternSubject<'db>> {
    let arm = list
        .ancestors()
        .find(|ancestor| ancestor.kind() == MATCH_ARM)?;
    let mut steps: Vec<PatternStep> = list_payload_step(list).into_iter().collect();
    for outer in list
        .ancestors()
        .skip(1)
        .take_while(|ancestor| ancestor != &arm)
        .filter(|ancestor| ancestor.kind() == PATTERN_LIST)
    {
        steps.push(list_item_step(&outer, word_start));
        steps.extend(list_payload_step(&outer));
    }
    steps.reverse();
    subject_along(db, file, &arm, &steps)
}

fn list_item_step(list: &SyntaxNode, word_start: TextSize) -> PatternStep {
    let field = list.children().find(|child| {
        child.kind() == PATTERN_FIELD && child.text_range().contains_inclusive(word_start)
    });
    if let Some(name) = field.and_then(|field| child_node(&field, NAME)) {
        return PatternStep::Field(name.text().to_string());
    }
    let index = list
        .children_with_tokens()
        .filter(|element| element.kind() == COMMA && element.text_range().end() <= word_start)
        .count();
    PatternStep::Item(index)
}

fn list_payload_step(list: &SyntaxNode) -> Option<PatternStep> {
    let tag = list
        .parent()
        .filter(|parent| parent.kind() == PATTERN_TAG)?;
    Some(PatternStep::Payload(
        child_node(&tag, NAME)?.text().to_string(),
    ))
}

fn subject_along<'db>(
    db: &'db dyn Db,
    file: SourceFile,
    arm: &SyntaxNode,
    steps: &[PatternStep],
) -> Option<PatternSubject<'db>> {
    let match_node = arm
        .ancestors()
        .find(|ancestor| ancestor.kind() == MATCH_EXPR)?;
    let position = position_in_item(db, file, match_node.text_range().start())?;
    let hir = item_hir(db, position.item).as_ref()?;
    let check = item_check(db, position.item)?;
    let match_range = match_node.text_range() - position.item_offset;
    let scrutinee = hir
        .expressions()
        .find_map(|expression| match &expression.kind {
            ExpressionKind::Match { scrutinee, .. } if expression.range == match_range => {
                Some(*scrutinee)
            }
            _ => None,
        })?;
    let types = descend(db, *check.expression_types.get(&scrutinee)?, steps);
    Some(PatternSubject { types })
}

/// The union members of the value at `steps` below a value of type `ty`, the type a pattern or
/// literal position nested inside it has.
fn descend<'db>(db: &'db dyn Db, ty: Ty<'db>, steps: &[PatternStep]) -> Vec<Ty<'db>> {
    let mut types = union_members(db, ty);
    for step in steps {
        types = types
            .into_iter()
            .filter_map(|member| match (step, member.kind(db)) {
                (PatternStep::Payload(name), TyKind::Tag(tag, payload)) if tag.text(db) == name => {
                    *payload
                }
                (PatternStep::Item(index), TyKind::NativeTuple(items)) => {
                    items.get(*index).copied()
                }
                (PatternStep::Field(name), TyKind::NativeRecord(fields)) => fields
                    .iter()
                    .find(|field| field.name.text(db) == name)
                    .map(|field| field.ty),
                _ => None,
            })
            .flat_map(|ty| union_members(db, ty))
            .collect();
    }
    types
}

/// A nominal type's members are its representation's: `type Shape = :Circle(..) | :Empty`
/// matches and builds the same tags as the union it names.
fn union_members<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Vec<Ty<'db>> {
    match ty.kind(db) {
        TyKind::Union(members) => members
            .iter()
            .flat_map(|&member| union_members(db, member))
            .collect(),
        TyKind::Named(name, arguments) => {
            let representation = ProjectFiles::try_get(db).and_then(|files| {
                let definition = project_type_definitions(db, files).get(name)?;
                Some(apply_definition(db, definition, arguments))
            });
            match representation {
                Some(representation) if representation != ty => union_members(db, representation),
                _ => vec![ty],
            }
        }
        _ => vec![ty],
    }
}

/// With nothing to check it against, `:Circle(|)` builds the payload that the project's type
/// definitions declare for `:Circle`.
fn declared_payloads<'db>(db: &'db dyn Db, container: &SyntaxNode) -> Vec<Ty<'db>> {
    let Some(tag) = container
        .parent()
        .filter(|parent| parent.kind() == TAG_EXPR)
    else {
        return Vec::new();
    };
    let (Some(name), Some(files)) = (child_node(&tag, NAME), ProjectFiles::try_get(db)) else {
        return Vec::new();
    };
    let steps = [PatternStep::Payload(name.text().to_string())];
    project_type_definitions(db, files)
        .values()
        .flat_map(|definition| descend(db, definition.body, &steps))
        .collect()
}

/// Field names inside a record literal, `(amount = 1, |)`, or a tag's payload, `:Circle(|)`,
/// come from the type the literal is checked against: an annotated binding's, a parameter's or
/// a nominal constructor's. The literal is followed by its syntax, as in a pattern, because the
/// half-typed literal completion runs on does not check.
fn literal_field_completions(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
    query: &str,
) -> Option<Vec<CompletionItem>> {
    let word_start = offset.checked_sub(TextSize::from(query.len() as u32))?;
    let root = semantics::parse(db, file).syntax_node();
    let anchor = pattern_anchor(&root, word_start, query)?;
    let container = anchor
        .parent_ancestors()
        .find(|ancestor| matches!(ancestor.kind(), ARGUMENT_LIST | PAREN_EXPR))?;
    let named = match container.kind() {
        // `(am|` is a parenthesized name until the first `=` makes it a record.
        PAREN_EXPR => {
            let inner = container.children().next();
            if inner.as_ref().is_some_and(|inner| inner.kind() != NAME) {
                return None;
            }
            Vec::new()
        }
        _ => {
            let literal = container.parent()?;
            if !matches!(literal.kind(), RECORD_EXPR | TAG_EXPR) {
                return None;
            }
            let current = container
                .children()
                .filter(|child| child.kind() == ARGUMENT)
                .find(|argument| argument.text_range().contains_inclusive(word_start));
            let at_name = current.as_ref().is_none_or(|argument| {
                child_token(argument, EQ)
                    .is_none_or(|equals| word_start <= equals.text_range().start())
            });
            if !at_name {
                return None;
            }
            let named: Vec<String> = container
                .children()
                .filter(|child| child.kind() == ARGUMENT && Some(child) != current.as_ref())
                .filter(|argument| child_token(argument, EQ).is_some())
                .filter_map(|argument| child_node(&argument, NAME))
                .map(|name| name.text().to_string())
                .collect();
            named
        }
    };
    // Every item of a record literal is named, so its name positions never take R's globals;
    // a tag's payload or a parenthesized name may still be a value.
    let in_record = container
        .parent()
        .is_some_and(|parent| parent.kind() == RECORD_EXPR);
    let fields: Vec<CompletionItem> = literal_expected_fields(db, file, &container)
        .into_iter()
        .filter(|item| !named.contains(&item.label))
        .filter(|item| search_match(&item.label, query).is_some())
        .collect();
    if fields.is_empty() && !in_record {
        return None;
    }
    Some(fields)
}

/// The fields the record at `container` (a literal's item list, or a parenthesized name) is
/// expected to have, walking out through enclosing literals to the one the checker recorded an
/// expectation for and back down along the same steps.
fn literal_expected_fields(
    db: &dyn Db,
    file: SourceFile,
    container: &SyntaxNode,
) -> Vec<CompletionItem> {
    let Some(position) = position_in_item(db, file, container.text_range().start()) else {
        return Vec::new();
    };
    let Some(hir) = item_hir(db, position.item).as_ref() else {
        return Vec::new();
    };
    let Some(check) = item_check(db, position.item) else {
        return Vec::new();
    };
    let expectation_at = |node: &SyntaxNode| {
        let range = node.text_range() - position.item_offset;
        hir.expressions()
            .enumerate()
            .filter(|(_, expression)| expression.range == range)
            .find_map(|(index, _)| check.literal_expectations.get(&ExprId(index as u32)))
            .copied()
    };

    let mut steps = Vec::new();
    let mut list = container.clone();
    let expected = (|| loop {
        let literal = if list.kind() == PAREN_EXPR {
            list.clone()
        } else {
            let literal = list.parent()?;
            if literal.kind() == TAG_EXPR {
                steps.push(PatternStep::Payload(
                    child_node(&literal, NAME)?.text().to_string(),
                ));
            }
            literal
        };
        if let Some(expected) = expectation_at(&literal) {
            return Some(expected);
        }
        let mut outer = literal.parent()?;
        while outer.kind() == PAREN_EXPR {
            outer = outer.parent()?;
        }
        if outer.kind() != ARGUMENT {
            return None;
        }
        list = outer.parent()?;
        if !matches!(list.parent()?.kind(), RECORD_EXPR | TUPLE_EXPR | TAG_EXPR) {
            return None;
        }
        steps.push(
            match child_node(&outer, NAME).filter(|_| child_token(&outer, EQ).is_some()) {
                Some(name) => PatternStep::Field(name.text().to_string()),
                None => PatternStep::Item(
                    list.children()
                        .filter(|child| child.kind() == ARGUMENT)
                        .take_while(|child| child != &outer)
                        .count(),
                ),
            },
        );
    })();
    let members = match expected {
        Some(expected) => {
            steps.reverse();
            descend(db, expected, &steps)
        }
        None => declared_payloads(db, container),
    };

    let mut renderer = TypeRenderer::default();
    let mut items = Vec::new();
    for member in members {
        let TyKind::NativeRecord(fields) = member.kind(db) else {
            continue;
        };
        for field in fields {
            let name = field.name.text(db);
            if items.iter().any(|item: &CompletionItem| item.label == name) {
                continue;
            }
            items.push(CompletionItem {
                detail: Some(renderer.render(db, field.ty)),
                ..CompletionItem::new(
                    insertable_name(name),
                    CompletionKind::Field,
                    CompletionSource::Field,
                )
            });
        }
    }
    items
}

/// Inside the brackets of a frame query, `x[i, j, by]`, column names are in scope as variables,
/// and so are the query's own symbols such as `.N` and `.SD`.
fn frame_query_completions(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
    query: &str,
) -> Vec<CompletionItem> {
    let columns = (|| {
        let position = position_in_item(db, file, offset)?;
        let hir = item_hir(db, position.item).as_ref()?;
        let check = item_check(db, position.item)?;
        let target = hir
            .expressions()
            .filter_map(|expression| match &expression.kind {
                ExpressionKind::Index {
                    double: false,
                    target,
                    ..
                } if expression.range.contains(position.relative)
                    && hir.range(*target).end() < position.relative =>
                {
                    Some((expression.range.len(), *target))
                }
                _ => None,
            })
            .min_by_key(|(length, _)| *length)
            .map(|(_, target)| target)?;
        let TyKind::Frame(_, Some(columns)) = check.expression_types.get(&target)?.kind(db) else {
            return None;
        };
        let mut renderer = TypeRenderer::default();
        Some(
            columns
                .iter()
                .map(|column| CompletionItem {
                    detail: Some(renderer.render(db, column.ty)),
                    ..CompletionItem::new(
                        insertable_name(column.name.text(db)),
                        CompletionKind::Field,
                        CompletionSource::Local,
                    )
                })
                .collect::<Vec<_>>(),
        )
    })();
    let Some(mut items) = columns else {
        return Vec::new();
    };
    items.extend(
        FRAME_QUERY_SYMBOLS
            .iter()
            .map(|(name, detail)| CompletionItem {
                detail: Some((*detail).to_owned()),
                ..CompletionItem::new(
                    (*name).to_owned(),
                    CompletionKind::Variable,
                    CompletionSource::Local,
                )
            }),
    );
    items.retain(|item| search_match(&item.label, query).is_some());
    items
}

const FRAME_QUERY_SYMBOLS: &[(&str, &str)] = &[
    (".N", "the number of rows in the group"),
    (".SD", "the group's rows, as a frame"),
    (".I", "the group's row numbers in the whole frame"),
    (".GRP", "the group's number"),
    (".NGRP", "the number of groups"),
    (".BY", "the group's `by` values"),
];

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

/// A one-letter query fuzzy-matches nearly every global in a large project, so only the
/// first [`COMPLETION_LIMIT`] are selected and sorted, without allocating a key per candidate.
fn finish_completions(items: Vec<CompletionItem>, query: &str) -> Option<CompletionResult> {
    let mut ranked: Vec<(Option<MatchScore>, usize)> = {
        let mut seen: HashSet<&str> = HashSet::with_capacity(items.len());
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| seen.insert(item.label.as_str()))
            .map(|(index, item)| (search_match(&item.label, query), index))
            .collect()
    };
    let order = |left: &(Option<MatchScore>, usize), right: &(Option<MatchScore>, usize)| {
        let (Some(left_item), Some(right_item)) = (items.get(left.1), items.get(right.1)) else {
            return left.1.cmp(&right.1);
        };
        left.0
            .cmp(&right.0)
            .then(left_item.source.cmp(&right_item.source))
            .then_with(|| {
                left_item
                    .label
                    .chars()
                    .flat_map(char::to_lowercase)
                    .cmp(right_item.label.chars().flat_map(char::to_lowercase))
            })
            .then_with(|| left_item.label.cmp(&right_item.label))
    };
    let is_incomplete = ranked.len() > COMPLETION_LIMIT;
    if is_incomplete {
        ranked.select_nth_unstable_by(COMPLETION_LIMIT, order);
        ranked.truncate(COMPLETION_LIMIT);
    }
    ranked.sort_by(order);
    let mut slots: Vec<Option<CompletionItem>> = items.into_iter().map(Some).collect();
    let items: Vec<CompletionItem> = ranked
        .into_iter()
        .filter_map(|(_, index)| slots.get_mut(index).and_then(Option::take))
        .collect();
    (!items.is_empty()).then_some(CompletionResult {
        items,
        is_incomplete,
    })
}

/// Offers `name = ` for each parameter of the enclosing call that no argument has claimed yet,
/// following R's matching: named arguments claim their parameter, and positional ones fill the
/// open parameters before `...` in order.
fn argument_name_completions(
    db: &dyn Db,
    file: SourceFile,
    offset: TextSize,
    query: &str,
) -> Vec<CompletionItem> {
    let Some(position) = position_in_item(db, file, offset) else {
        return Vec::new();
    };
    let (Some(hir), Some(check)) = (
        item_hir(db, position.item).as_ref(),
        item_check(db, position.item),
    ) else {
        return Vec::new();
    };
    // `f(y = |)`: the cursor is the value of a named argument, which HIR leaves without a range.
    let before = file
        .text(db)
        .get(..usize::from(offset))
        .unwrap_or_default()
        .trim_end_matches(|character: char| character.is_alphanumeric() || "._".contains(character))
        .trim_end();
    if before.ends_with('=') {
        return Vec::new();
    }
    let relative = position.relative;
    let call = hir
        .expressions()
        .filter_map(|expression| match *expression.kind {
            ExpressionKind::Call { callee, arguments }
                if hir.range(callee).end() < relative && relative < expression.range.end() =>
            {
                Some((expression.range.len(), callee, arguments))
            }
            _ => None,
        })
        .min_by_key(|(length, _, _)| *length);
    let Some((_, callee, arguments)) = call else {
        return Vec::new();
    };
    let arguments = &hir[arguments];
    // The cursor is either on a bare name it is typing as a positional argument, or between
    // arguments; anywhere inside an argument's own expression or name, a name would not fit.
    let mut typed = None;
    for (index, argument) in arguments.iter().enumerate() {
        if argument
            .name_range
            .is_some_and(|range| range.contains_inclusive(relative))
        {
            return Vec::new();
        }
        let Some(value) = argument.value else {
            continue;
        };
        let expression = hir.expression(value);
        if expression.range.contains_inclusive(relative) {
            if argument.name.is_some() || !matches!(expression.kind, ExpressionKind::NameRef(_)) {
                return Vec::new();
            }
            typed = Some(index);
        }
    }
    let Some(ty) = check.expression_types.get(&callee) else {
        return Vec::new();
    };
    let TyKind::Function(function) = ty.kind(db) else {
        return Vec::new();
    };
    let claimed: HashSet<&str> = arguments
        .iter()
        .filter_map(|argument| argument.name.map(Symbol::as_str))
        .collect();
    let positional = arguments
        .iter()
        .enumerate()
        .filter(|(index, argument)| {
            argument.name.is_none() && argument.value.is_some() && typed != Some(*index)
        })
        .count();
    let positional_slots = function
        .variadic
        .as_ref()
        .map_or(function.named.len(), |rest| rest.preceding_named);
    let mut skipped = 0;
    function
        .named
        .iter()
        .enumerate()
        .filter(|(index, field)| {
            let name = field.name.text(db);
            if claimed.contains(name) {
                return false;
            }
            if *index < positional_slots && skipped < positional {
                skipped += 1;
                return false;
            }
            search_match(name, query).is_some()
        })
        .map(|(_, field)| {
            let name = field.name.text(db);
            CompletionItem {
                detail: Some(TypeRenderer::default().render(db, field.ty)),
                ..CompletionItem::new(
                    format!("{} = ", insertable_name(name)),
                    CompletionKind::Variable,
                    CompletionSource::Argument,
                )
            }
        })
        .collect()
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
    let case_sensitive = query_is_case_sensitive(query);
    // Most candidates are settled here, before the character scan below.
    if prefix_under_case(candidate, query, case_sensitive) {
        let tier = if equal_under_case(candidate, query, case_sensitive) {
            MatchTier::Exact
        } else {
            MatchTier::Prefix
        };
        return Some(MatchScore {
            tier,
            first_match_index: 0,
        });
    }
    if query.chars().count() < MIN_SUBSEQUENCE_QUERY_LEN {
        return None;
    }

    let equal = |left: char, right: char| {
        if case_sensitive {
            left == right
        } else if left.is_ascii() && right.is_ascii() {
            left.eq_ignore_ascii_case(&right)
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
    let tier = if substring_under_case(candidate, query, case_sensitive) {
        MatchTier::Substring
    } else {
        MatchTier::Subsequence
    };
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
        let mut candidate = candidate.chars().flat_map(char::to_lowercase);
        query
            .chars()
            .flat_map(char::to_lowercase)
            .all(|character| candidate.next() == Some(character))
    }
}

fn substring_under_case(candidate: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        candidate.contains(query)
    } else if candidate.is_ascii() && query.is_ascii() {
        query.is_empty()
            || candidate
                .as_bytes()
                .windows(query.len())
                .any(|window| window.eq_ignore_ascii_case(query.as_bytes()))
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
                .is_some_and(|value| hir.range(value).end() < cursor)
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
    let mut scheme = if is_root {
        check.scheme.clone()?
    } else {
        let ty = check
            .expression_types
            .get(&id)
            .or_else(|| check.expression_types.get(&value))?;
        TypeScheme::monomorphic(*ty)
    };
    scheme.body = without_impossible_vectors(db, scheme.body);
    is_hintable(
        db,
        scheme.body,
        matches!(scheme.body.kind(db), TyKind::Function(_)),
    )
    .then(|| TypeRenderer::default().render_scheme(db, &scheme))
}

/// Drops the union members that name an atomic vector of non-atomic elements, such as the
/// `Position[]` in `list[Position] | Position[]` that instantiating `vapply`'s `list[T] | T[]`
/// with a record leaves behind. No value has that type, so the hint reads shorter and means
/// the same.
fn without_impossible_vectors<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    match ty.kind(db) {
        TyKind::Union(members) => {
            let kept: Vec<Ty<'db>> = members
                .iter()
                .filter(|member| !is_impossible_vector(db, **member))
                .map(|&member| without_impossible_vectors(db, member))
                .collect();
            match kept.as_slice() {
                [] => ty,
                [single] => *single,
                _ => Ty::new(db, TyKind::Union(kept)),
            }
        }
        TyKind::Function(function) => {
            let mut function = function.clone();
            for positional in &mut function.positional {
                *positional = without_impossible_vectors(db, *positional);
            }
            for parameter in &mut function.named {
                parameter.ty = without_impossible_vectors(db, parameter.ty);
            }
            function.ret = without_impossible_vectors(db, function.ret);
            Ty::new(db, TyKind::Function(function))
        }
        _ => ty,
    }
}

fn is_impossible_vector(db: &dyn Db, ty: Ty<'_>) -> bool {
    let (TyKind::Vector(element) | TyKind::NamedVector(element)) = ty.kind(db) else {
        return false;
    };
    let is_structured = |ty: Ty<'_>| {
        matches!(
            ty.kind(db),
            TyKind::NativeRecord(_)
                | TyKind::NativeTuple(_)
                | TyKind::Tag(..)
                | TyKind::Record(_)
                | TyKind::Tuple(_)
                | TyKind::List(_)
                | TyKind::NamedList(_)
                | TyKind::Frame(..)
                | TyKind::Function(_)
        )
    };
    match element.kind(db) {
        // A nominal is impossible as an element only when its representation is.
        TyKind::Named(name, _) => ProjectFiles::try_get(db).is_some_and(|files| {
            project_type_definitions(db, files)
                .get(name)
                .is_some_and(|definition| is_structured(definition.body))
        }),
        _ => is_structured(*element),
    }
}

fn is_hintable(db: &dyn Db, ty: Ty<'_>, variables_allowed: bool) -> bool {
    match ty.kind(db) {
        TyKind::Unknown => false,
        TyKind::Var(_) | TyKind::Rigid(_) => variables_allowed,
        TyKind::Any | TyKind::Null | TyKind::Scalar(_) | TyKind::OtherTags => true,
        TyKind::NativeTuple(items) => items
            .iter()
            .all(|&item| is_hintable(db, item, variables_allowed)),
        TyKind::NativeRecord(fields) => fields
            .iter()
            .all(|field| is_hintable(db, field.ty, variables_allowed)),
        TyKind::Tag(_, payload) => {
            payload.is_none_or(|payload| is_hintable(db, payload, variables_allowed))
        }
        TyKind::Vector(element)
        | TyKind::NamedVector(element)
        | TyKind::List(element)
        | TyKind::NamedList(element) => is_hintable(db, *element, variables_allowed),
        TyKind::Tuple(items) => items
            .iter()
            .all(|&item| is_hintable(db, item, variables_allowed)),
        TyKind::Record(fields) | TyKind::Frame(_, Some(fields)) => fields
            .iter()
            .all(|field| is_hintable(db, field.ty, variables_allowed)),
        TyKind::Frame(_, None) => true,
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
    Slot {
        item: Item<'db>,
        binding: BindingId,
    },
    /// A `<T>` binder's occurrences, which never leave the function or definition binding it.
    TypeParameter(Vec<Occurrence>),
    Tag(String),
    Global(String),
    TypeName(String),
    StubGlobal(String),
    S4 {
        name: String,
        kind: S4Kind,
    },
}

fn target_at<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    file: SourceFile,
    offset: TextSize,
) -> Option<Target<'db>> {
    if let Some(cursor) = type_parameter_at(db, file, offset) {
        return Some(Target::TypeParameter(type_parameter_occurrences(
            file, &cursor,
        )));
    }
    if let Some((name, _)) = tag_at(db, file, offset) {
        return Some(Target::Tag(name));
    }
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
        // A call head skips bindings that hold no function, so it navigates where the call goes.
        let callee_target = item_check(db, position.item)
            .and_then(|check| check.callee_targets.get(&expression).copied());
        let binding = match callee_target {
            Some(CalleeTarget::Slot(binding)) => Some(binding),
            Some(CalleeTarget::Project | CalleeTarget::Library) => {
                let ExpressionKind::NameRef(name) =
                    &item_hir(db, position.item).as_ref()?.kind(expression)
                else {
                    return None;
                };
                return Some(match callee_target {
                    Some(CalleeTarget::Project) => Target::Global(name.to_string()),
                    _ => Target::StubGlobal(name.to_string()),
                });
            }
            None => naming.resolutions.get(&expression).copied(),
        };
        if let Some(binding) = &binding {
            let info = naming.bindings.get(binding)?;
            // Cross-item reads resolve to the name, so the item's own top-level binding is the
            // global.
            if info.kind == BindingKind::TopLevel {
                return Some(Target::Global(info.name.to_string()));
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
                return Some(Target::Global(name.to_string()));
            }
            if stub_declaration(db, name).is_some() {
                return Some(Target::StubGlobal(name.to_string()));
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
        return Some(Target::Global(info.name.to_string()));
    }
    Some(Target::Slot {
        item: position.item,
        binding: *binding,
    })
}

/// Counts conditional top-level writes too, matching the occurrence walk's notion of a
/// declaration.
fn global_declaration_exists(db: &dyn Db, files: ProjectFiles, name: &str) -> bool {
    project_global_names(db, files).contains(&Symbol::new(name))
}

fn occurrences(db: &dyn Db, files: ProjectFiles, target: &Target<'_>) -> Vec<Occurrence> {
    let mut result = Vec::new();
    match target {
        Target::StubGlobal(_) => {}
        Target::TypeParameter(occurrences) => result = occurrences.clone(),
        Target::Tag(name) => result = tag_occurrences(db, files, name),
        Target::Slot { item, binding } => slot_occurrences(db, *item, *binding, &mut result),
        Target::TypeName(name) => result = type_name_occurrences(db, files, name),
        Target::S4 { name, kind } => {
            for &file in files.files(db) {
                if file.text(db).contains(name.as_str()) {
                    result.extend(s4_occurrences(db, file, name, *kind));
                }
            }
        }
        // Every occurrence spells the name, so a text search skips nearly all of a large
        // project before any item is lowered or named.
        Target::Global(name) => {
            for &file in files.files(db) {
                let text = file.text(db);
                if !text.contains(name.as_str()) {
                    continue;
                }
                result.extend(s4_occurrences(db, file, name, S4Kind::Generic));
                for &item in item_tree(db, file) {
                    let Some(node) = item_node(db, item) else {
                        continue;
                    };
                    let spells_name = text
                        .get(std::ops::Range::<usize>::from(node.text_range()))
                        .is_some_and(|source| source.contains(name.as_str()));
                    if !spells_name {
                        continue;
                    }
                    let Some(naming) = item_naming(db, item) else {
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
                                    range: name_range(hir, *expression, name) + item_offset,
                                    is_declaration: false,
                                }),
                        );
                        // A call head bound locally to a value falls through to the global.
                        let named_here = |expression: &ExprId| {
                            naming
                                .resolutions
                                .get(expression)
                                .and_then(|binding| naming.bindings.get(binding))
                                .is_some_and(|info| info.name == *name)
                        };
                        let calls: Vec<ExprId> = naming
                            .callee_fallbacks
                            .keys()
                            .copied()
                            .filter(named_here)
                            .collect();
                        if !calls.is_empty()
                            && let Some(check) = item_check(db, item)
                        {
                            result.extend(
                                calls
                                    .into_iter()
                                    .filter(|expression| {
                                        check.callee_targets.get(expression)
                                            == Some(&CalleeTarget::Project)
                                    })
                                    .map(|expression| Occurrence {
                                        file,
                                        range: name_range(hir, expression, name) + item_offset,
                                        is_declaration: false,
                                    }),
                            );
                        }
                    }
                }
            }
        }
    }
    let file_order: HashMap<SourceFile, usize> = files
        .files(db)
        .iter()
        .enumerate()
        .map(|(index, file)| (*file, index))
        .collect();
    result.sort_by_key(|occurrence| {
        (
            file_order.get(&occurrence.file).copied(),
            occurrence.range.start(),
        )
    });
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
/// explicitly. A pattern's names are `NameRef` expressions, so they come with the resolutions.
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
        .expressions()
        .flat_map(|expression| match &expression.kind {
            ExpressionKind::Assign { target, .. } => vec![*target],
            ExpressionKind::Match { arms, .. } => hir[arms]
                .iter()
                .flat_map(|arm| hir[arm.bindings].iter().copied())
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    // A call head reaches the binding its function lookup settled on, which may not be the
    // nearest one.
    let callee_targets = (!naming.callee_fallbacks.is_empty())
        .then(|| item_check(db, item))
        .flatten()
        .map(|check| check.callee_targets);
    let reaches = |expression: &ExprId, resolved: BindingId| match callee_targets
        .as_ref()
        .and_then(|targets| targets.get(expression))
    {
        Some(CalleeTarget::Slot(target)) => *target == binding,
        Some(CalleeTarget::Project | CalleeTarget::Library) => false,
        None => resolved == binding,
    };
    slot.extend(
        naming
            .resolutions
            .iter()
            .filter(|(expression, resolved)| reaches(expression, **resolved))
            .map(|(expression, _)| Occurrence {
                file,
                range: name_range(hir, *expression, &info.name) + item_offset,
                is_declaration: targets.contains(expression),
            }),
    );
    slot.sort_by_key(|occurrence| occurrence.range.start());
    slot.dedup_by_key(|occurrence| occurrence.range);
    result.extend(slot);
}

/// The part of a read that spells `name`: a data.table `..name` read inside `[` refers to the
/// variable `name`, so a rename must keep its `..` prefix.
fn name_range(hir: &Module, expression: ExprId, name: &str) -> TextRange {
    let range = hir.range(expression);
    match hir.kind(expression) {
        ExpressionKind::NameRef(spelled) if dot_dot_variable(spelled) == Some(name) => {
            TextRange::new(range.start() + TextSize::from(2), range.end())
        }
        _ => range,
    }
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
            .expressions()
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
