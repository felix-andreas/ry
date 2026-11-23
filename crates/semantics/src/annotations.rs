//! Lowering is total: malformed pieces lower to `Unknown` (their syntax errors already mark
//! them), so annotations never cascade. A `#:` block and an inline annotation lower through
//! the same type lowering into the same `Annotation`, so the checker sees one shape whichever
//! way a type was written.

use {
    crate::{
        Db, SourceFile,
        diagnostics::TypeRenderer,
        types::{
            Atomic, Binder, Bound, Constraint, FrameKind, FunctionType, Name, NativeField,
            NativeScalar, Parameter, RecordField, RestParameter, Ty, TyKind, TypeScheme, any,
            column_type, conflicting_tags, is_ry_value_type, native_record, native_record_with_row,
            null, scalar, substitute_rigid, union_of, unknown,
        },
    },
    rustc_hash::FxHashSet,
    std::mem::take,
    syntax::{
        NodeRef, SyntaxKind, SyntaxNode, TextRange,
        annotation::{
            BlockRefusal, Declaration, Paired, assigned_function, attached_annotation,
            block_refusal, compact_declaration, directive_name, expanded_declaration, formals,
            has_token, is_definition_keyword, is_type_kind, misplaced_directives, name_text,
            pair_formals, signature_from, written_function_type,
        },
        ast, is_inline_annotation_kind, node_text,
    },
    thin_vec::ThinVec,
};

#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct Annotation<'db> {
    pub declared: Option<TypeScheme<'db>>,
    pub definitions: Vec<NamedDefinition<'db>>,
    /// Parallel to `definitions`; kept out of `NamedDefinition` so interface maps built from
    /// definitions stay position-independent.
    pub definition_sites: Vec<TextRange>,
    /// `@new Name<ARGS>`, with the range of the referenced type.
    // Sound: `TextRange` is plain offsets, so only the `'db` handles need retaining.
    #[salsa_value(unsafe(prove_safe_to_retain_manually))]
    pub new_nominal: Option<(Name<'db>, Vec<Ty<'db>>, TextRange)>,
    pub strict: Option<bool>,
    /// `@trust TYPE`, where the declared type applies unchecked.
    pub trusted: bool,
    /// `@if-unknown TYPE`: applies only where the checker has nothing better than `Unknown`.
    pub if_unknown: bool,
    /// Parameters typed only by an inline signature. It sits on the formals and so never
    /// restates which parameters are optional; the formals alone decide it.
    pub inline_parameters: Vec<String>,
    /// Whether a type is written for every formal of the annotated function literal, or for
    /// its result when it has no formals, in a place the runtime reads: on the literal, on the
    /// name it is assigned to, or as a function type in the `#:` block. The runtime compiles no
    /// function whose parameter types reach it another way, such as through a type alias. Set
    /// where the annotation meets its statement, from [`has_written_signature`].
    pub written_signature: bool,
    /// Shape violations. A block with any carries no typing payload, so it never cascades.
    pub errors: Vec<(String, TextRange)>,
    /// Violations reported only when type checking is on; like `errors`, they strip the payload.
    pub typing_errors: Vec<(String, TextRange)>,
    /// Judged by the diagnostics, not here: whether a named element resolves to an atomic alias
    /// needs the project vocabulary.
    #[salsa_value(unsafe(prove_safe_to_retain_manually))]
    pub vector_elements: Vec<(Ty<'db>, TextRange)>,
    /// `(name, argument count, range)`; the arity check against the vocabulary lives with the
    /// diagnostics.
    pub applied_references: Vec<(String, usize, TextRange)>,
    /// Excludes primitives and in-scope binders, so each entry must resolve through project
    /// declarations or the stubs.
    pub nominal_references: Vec<(String, TextRange)>,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct NamedDefinition<'db> {
    pub alias: bool,
    /// Written as `type Name = TYPE` rather than in a `#:` comment, so tools show it that way.
    pub inline: bool,
    pub name: Name<'db>,
    /// Type parameters; the body references them as rigid variables.
    pub parameters: Vec<Name<'db>>,
    pub body: Ty<'db>,
    /// Set on an `opaque` definition, whose representation only its home file sees.
    pub opaque: Option<Opaque>,
}

/// `type`, `alias` or `opaque`: the word that declares `definition`.
pub fn definition_keyword(definition: &NamedDefinition<'_>) -> &'static str {
    match (definition.alias, definition.opaque) {
        (true, _) => "alias",
        (false, Some(_)) => "opaque",
        (false, None) => "type",
    }
}

/// Where an `opaque` definition lives. Every other file reads a copy whose body is `Unknown`
/// and whose `hidden` is set, so the opaque-nominal rules apply there unchanged and the
/// checker refuses `@new` and representation access on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub struct Opaque {
    /// The declaring file, filled in by `file_type_definitions`; lowering sees one block.
    pub home: Option<SourceFile>,
    /// The declaration, in the home file's offsets.
    pub site: TextRange,
    pub hidden: bool,
}

pub fn lower_annotation<'db>(db: &'db dyn Db, node: &SyntaxNode) -> Annotation<'db> {
    lower_annotation_within(db, node, &[])
}

/// [`lower_annotation`] inside signatures whose type parameters `outer` names, beyond those
/// the block's own tree reaches (an item's own `#:` block sits outside the item's tree).
pub fn lower_annotation_within<'db>(
    db: &'db dyn Db,
    node: &SyntaxNode,
    outer: &[Name<'db>],
) -> Annotation<'db> {
    debug_assert_eq!(node.kind(), SyntaxKind::ANNOTATION);
    let mut lowering = Lowering::within(db, node, outer);
    let mut annotation = Annotation::default();
    match block_refusal(node) {
        Some(BlockRefusal::FormMixing(message)) => {
            annotation.errors.push((message, node.text_range()));
            return annotation;
        }
        Some(BlockRefusal::Unparsed) => return annotation,
        None => {}
    }

    let mut forall: Vec<Binder<'db>> = Vec::new();
    let mut params: Vec<(String, Ty<'db>, bool)> = Vec::new();
    let mut rest: Option<Ty<'db>> = None;
    let mut ret: Option<Ty<'db>> = None;
    lowering.errors.extend(
        misplaced_directives(node)
            .into_iter()
            .map(|(message, range)| (message.to_owned(), range)),
    );

    for child in node.children() {
        match child.kind() {
            SyntaxKind::ANNOTATION_DIRECTIVE => {
                let directive = directive_name(&child);
                match directive.as_str() {
                    keyword if is_definition_keyword(keyword) => {
                        if let Some(definition) = lowering.lower_named_definition(&child, keyword) {
                            annotation.definitions.push(definition);
                            annotation.definition_sites.push(child.text_range());
                        }
                    }
                    "forall" => {
                        lowering.lower_binders(&child, &mut forall, "expanded function annotation");
                    }
                    "param" => {
                        let optional = has_token(&child, SyntaxKind::L_BRACKET);
                        let ty = lowering
                            .lower_child_type(&child)
                            .unwrap_or_else(|| unknown(db));
                        match name_text(&child) {
                            Some(name) if name == "..." => rest = Some(ty),
                            Some(name) => params.push((name, ty, optional)),
                            None => {}
                        }
                    }
                    "return" | "returns" => {
                        ret = lowering.lower_child_type(&child);
                    }
                    "strict" => annotation.strict = Some(!has_word(&child, "off")),
                    "trust" | "if-unknown" | "new" => {
                        lowering.lower_coercion(&mut annotation, &child, &directive);
                    }
                    _ => {}
                }
            }
            SyntaxKind::TYPE_BINDER_LIST => {
                lowering.lower_binders(&child, &mut forall, "`<...>`");
            }
            kind if is_type_kind(kind) => {
                lowering.definition_type = true;
                let body = lowering.lower_type(&child);
                lowering.definition_type = false;
                annotation.declared = Some(TypeScheme {
                    binders: lowering.scheme_binders(take(&mut forall)),
                    body,
                });
            }
            _ => {}
        }
    }

    if annotation.declared.is_none() && (!params.is_empty() || ret.is_some()) {
        // An annotation states a parameter's type, never its default: the default lives in the
        // definition, which may not be in view.
        let named = params
            .iter()
            .map(|(name, ty, optional)| Parameter {
                name: Name::new(db, name.clone()),
                ty: *ty,
                optional: *optional,
                default: None,
            })
            .collect::<Vec<_>>();
        let function = FunctionType::declared(
            &[],
            &named,
            rest.map(|element| RestParameter {
                element,
                preceding_named: params.len(),
            }),
            ret.unwrap_or_else(|| unknown(db)),
        );
        annotation.declared = Some(TypeScheme {
            binders: lowering.scheme_binders(forall),
            body: Ty::new(db, TyKind::Function(&function)),
        });
    }
    lowering.finish(annotation, node.text_range())
}

/// A binding (`name: TYPE <- value`), a function literal with any of `<...>`, a
/// parameter's `: TYPE`, or `-> TYPE`, a type definition (`type Name = TYPE`), or a
/// nominal constructor (`@Name(value)`).
pub fn is_inline_site(node: &SyntaxNode) -> bool {
    is_inline_site_ref(node.node_ref())
}

/// Whether a subtree holding an element of `kind` may hold a `#:` annotation or an inline
/// site: every site is or contains one of these kinds, so a subtree without any has none.
pub fn may_mark_site(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::ANNOTATION
            | SyntaxKind::TYPE_DEFINITION
            | SyntaxKind::NEW_EXPR
            | SyntaxKind::TYPE_ASCRIPTION
            | SyntaxKind::TYPE_BINDER_LIST
            | SyntaxKind::RETURN_TYPE
    )
}

/// [`is_inline_site`] on a borrowed node, for whole-file searches.
pub fn is_inline_site_ref(node: NodeRef<'_>) -> bool {
    match node.kind() {
        SyntaxKind::TYPE_DEFINITION | SyntaxKind::NEW_EXPR => true,
        SyntaxKind::BINARY_EXPR => node.has_child(SyntaxKind::TYPE_ASCRIPTION),
        SyntaxKind::FUNCTION_DEF => node.child_nodes().any(|child| match child.kind() {
            SyntaxKind::TYPE_BINDER_LIST | SyntaxKind::RETURN_TYPE => true,
            SyntaxKind::PARAMETER_LIST => child
                .child_nodes()
                .any(|parameter| parameter.has_child(SyntaxKind::TYPE_ASCRIPTION)),
            _ => false,
        }),
        _ => false,
    }
}

/// An inline site lowers exactly as the equivalent `#:` block would: a binding's type as
/// a checked annotation, and a function's signature as a compact `fn(...)` naming the
/// annotated parameters, with an elided return inferred from the body.
pub fn lower_inline_annotation<'db>(db: &'db dyn Db, site: &SyntaxNode) -> Annotation<'db> {
    lower_inline_annotation_within(db, site, &[])
}

/// [`lower_inline_annotation`] inside signatures whose type parameters `outer` names.
pub fn lower_inline_annotation_within<'db>(
    db: &'db dyn Db,
    site: &SyntaxNode,
    outer: &[Name<'db>],
) -> Annotation<'db> {
    debug_assert!(is_inline_site(site));
    let mut lowering = Lowering::within(db, site, outer);
    let mut annotation = Annotation::default();
    match site.kind() {
        SyntaxKind::TYPE_DEFINITION => {
            let keyword = type_definition_keyword(site);
            if let Some(definition) = lowering.lower_named_definition(site, &keyword) {
                annotation.definitions.push(definition);
                annotation.definition_sites.push(site.text_range());
            }
            return lowering.finish(annotation, site.text_range());
        }
        SyntaxKind::NEW_EXPR => {
            if let Some(head) = site
                .children()
                .find(|child| child.kind() == SyntaxKind::NEW_TYPE)
            {
                lowering.lower_coercion(&mut annotation, &head, "new");
            }
            return lowering.finish(annotation, inline_range(site));
        }
        SyntaxKind::BINARY_EXPR => {
            let ascription = site
                .children()
                .find(|child| child.kind() == SyntaxKind::TYPE_ASCRIPTION);
            if let Some(directive) = ascription.as_ref().and_then(|ascription| {
                ascription
                    .children()
                    .find(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
            }) {
                lowering.lower_coercion(&mut annotation, &directive, &directive_name(&directive));
                return lowering.finish(annotation, inline_range(site));
            }
            lowering.definition_type = true;
            let ty = ascription
                .and_then(|ascription| lowering.lower_child_type(&ascription))
                .unwrap_or_else(|| unknown(db));
            lowering.definition_type = false;
            annotation.declared = Some(TypeScheme::monomorphic(ty));
            return lowering.finish(annotation, inline_range(site));
        }
        _ => {}
    }
    let mut forall = Vec::new();
    if let Some(list) = site
        .children()
        .find(|child| child.kind() == SyntaxKind::TYPE_BINDER_LIST)
    {
        lowering.lower_binders(&list, &mut forall, "a function's type parameters");
    }
    let mut named = Vec::new();
    let mut variadic = None;
    for (index, formal) in formals(site).into_iter().enumerate() {
        let Some(ascription) = formal.ascription else {
            continue;
        };
        let ty = lowering
            .lower_child_type(&ascription)
            .unwrap_or_else(|| unknown(db));
        if formal.name == "..." {
            // Every formal before `...` counts, annotated or not: R places the rest
            // parameter by the formal list, which is what the definition checks against.
            variadic = Some(RestParameter {
                element: ty,
                preceding_named: index,
            });
        } else {
            named.push(Parameter {
                name: Name::new(db, formal.name),
                ty,
                optional: formal.has_default,
                default: None,
            });
        }
    }
    let written_return = site
        .children()
        .find(|child| child.kind() == SyntaxKind::RETURN_TYPE);
    let ret = written_return
        .and_then(|ret| lowering.lower_child_type(&ret))
        .unwrap_or_else(|| unknown(db));
    annotation.inline_parameters = named
        .iter()
        .map(|parameter: &Parameter<'db>| parameter.name.text(db).to_owned())
        .collect();
    annotation.declared = Some(TypeScheme {
        binders: lowering.scheme_binders(forall),
        body: Ty::new(
            db,
            TyKind::Function(&FunctionType::declared(&[], &named, variadic, ret)),
        ),
    });
    lowering.finish(annotation, inline_range(site))
}

/// The type parameters of the signatures around `node`, innermost last: each enclosing
/// function literal's own `<...>` and the `<...>` of the `#:` block on the statement binding
/// it. An annotation in a function's body names them as its signature does.
pub fn enclosing_type_parameters<'db>(db: &'db dyn Db, node: &SyntaxNode) -> Vec<Name<'db>> {
    let mut names = Vec::new();
    for function in node
        .ancestors()
        .skip(1)
        .filter(|ancestor| ancestor.kind() == SyntaxKind::FUNCTION_DEF)
    {
        let statement = function
            .parent()
            .filter(|parent| assigned_function(parent).as_ref() == Some(&function))
            .unwrap_or_else(|| function.clone());
        let mut written = attached_annotation(&statement)
            .map(|block| block_type_parameters(db, &block))
            .unwrap_or_default();
        if let Some(list) = function
            .children()
            .find(|child| child.kind() == SyntaxKind::TYPE_BINDER_LIST)
        {
            written.extend(binder_names(db, &list));
        }
        written.extend(names);
        names = written;
    }
    names
}

/// The type parameters a `#:` block's signature declares, in `<...>` or `@forall`.
pub fn block_type_parameters<'db>(db: &'db dyn Db, block: &SyntaxNode) -> Vec<Name<'db>> {
    block
        .children()
        .filter(|child| {
            child.kind() == SyntaxKind::TYPE_BINDER_LIST
                || (child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE
                    && directive_name(child) == "forall")
        })
        .flat_map(|list| binder_names(db, &list))
        .collect()
}

/// The names of the `TYPE_BINDER`s of `list`.
fn binder_names<'db>(db: &'db dyn Db, list: &SyntaxNode) -> Vec<Name<'db>> {
    list.children()
        .filter(|child| child.kind() == SyntaxKind::TYPE_BINDER)
        .filter_map(|binder| Some(Name::new(db, name_text(&binder)?)))
        .collect()
}

/// `type`, `alias` or `opaque`, the word that opens an inline type definition.
pub fn type_definition_keyword(definition: &SyntaxNode) -> String {
    definition
        .first_token()
        .map(|token| token.text().to_owned())
        .unwrap_or_default()
}

/// The written inline annotations of a site, from the first to the last, so a finding
/// about the whole annotation never covers the value or the body. Every part is a child of
/// the site except a parameter's `: TYPE`, so the walk never enters a value or a body.
fn inline_range(site: &SyntaxNode) -> TextRange {
    let top = site.node_ref();
    if is_inline_annotation_kind(top.kind(), site.parent().map(|parent| parent.kind())) {
        return site.text_range();
    }
    let mut covered: Option<TextRange> = None;
    let mut cover = |part: TextRange| {
        covered = Some(covered.map_or(part, |covered| covered.cover(part)));
    };
    for child in top.child_nodes() {
        if child.kind() == SyntaxKind::PARAMETER_LIST {
            for parameter in child.child_nodes() {
                for part in parameter.child_nodes() {
                    if part.kind() == SyntaxKind::TYPE_ASCRIPTION {
                        cover(part.range());
                    }
                }
            }
        } else if is_inline_annotation_kind(child.kind(), Some(top.kind())) {
            cover(child.range());
        }
    }
    covered.unwrap_or_else(|| site.text_range())
}

/// What a statement's two annotations mean together. A coercion form (`@trust`,
/// `@if-unknown`, `@new`) says how the value gets its type, so it governs wherever it
/// is written, and the inline one wins when both are coercions;
/// otherwise both are declarations, merged one slot (the value, a parameter, the rest
/// parameter, the return) at a time, with the inline type winning where both give one.
/// Where they differ, the disagreement warning has already said so.
pub fn merge_annotations<'db>(
    db: &'db dyn Db,
    comment: Option<Annotation<'db>>,
    inline: Option<Annotation<'db>>,
    signature: Option<&SyntaxNode>,
) -> Option<Annotation<'db>> {
    let (comment, inline) = match (comment, inline) {
        (comment, None) => return comment,
        (None, inline) => return inline,
        (Some(comment), Some(inline)) => (comment, inline),
    };
    if comment.declared.is_none() && comment.new_nominal.is_none() {
        return Some(inline);
    }
    if is_coercion(&inline) {
        return Some(inline);
    }
    if is_coercion(&comment) {
        return Some(comment);
    }
    let (Some(comment_scheme), Some(inline_scheme)) = (&comment.declared, &inline.declared) else {
        return Some(comment);
    };
    // A binding's `: TYPE` replaces the whole declared type; only a signature merges by slot.
    let (TyKind::Function(_), TyKind::Function(inline_function), Some(signature)) = (
        comment_scheme.body.kind(db),
        inline_scheme.body.kind(db),
        signature.filter(|site| site.kind() == SyntaxKind::FUNCTION_DEF),
    ) else {
        return Some(inline);
    };
    // Type parameters pair by position, so the comment's types are read in the inline
    // names before slots mix.
    let renaming: Vec<_> = comment_scheme
        .binders
        .iter()
        .zip(&inline_scheme.binders)
        .map(|(comment, inline)| (comment.name, Ty::new(db, TyKind::Rigid(inline.name))))
        .collect();
    let renamed = substitute_rigid(db, comment_scheme.body, &renaming);
    let TyKind::Function(comment_function) = renamed.kind(db) else {
        return Some(inline);
    };
    let formals = formals(signature);
    let comment_names: Vec<&str> = comment_function
        .named
        .iter()
        .map(|parameter| parameter.name.text(db))
        .collect();
    let paired = pair_formals(
        formals.iter().map(|formal| formal.name.as_str()),
        &comment_names,
        comment_function.positional.len(),
    );
    let mut used = vec![false; comment_function.named.len()];
    let mut surplus = 0;
    let mut named = Vec::new();
    for (formal, paired) in formals.iter().zip(paired) {
        let from_comment = match paired {
            Paired::Rest => continue,
            Paired::Named(index) => {
                used[index] = true;
                Some(comment_function.named[index])
            }
            Paired::Positional(index) => {
                surplus = index + 1;
                Some(Parameter {
                    name: Name::new(db, formal.name.clone()),
                    ty: comment_function.positional[index],
                    optional: formal.has_default,
                    default: None,
                })
            }
            Paired::Undeclared => None,
        };
        let from_inline = inline_function
            .named
            .iter()
            .find(|parameter| parameter.name.text(db) == formal.name)
            .cloned();
        // Where the comment restates optionality, its `[name]` is checked as written.
        named.extend(match (from_inline, from_comment) {
            (Some(inline), Some(comment)) => Some(Parameter {
                optional: comment.optional,
                ..inline
            }),
            (inline, comment) => inline.or(comment),
        });
    }
    // Comment entries no formal receives are kept, so the definition check still reports them.
    named.extend(
        comment_function
            .named
            .iter()
            .zip(&used)
            .filter(|(_, used)| !**used)
            .map(|(parameter, _)| *parameter),
    );
    let surplus: Vec<Ty<'db>> = comment_function.positional[surplus..].to_vec();
    let variadic = match (&inline_function.variadic, &comment_function.variadic) {
        (Some(rest), _) => Some(RestParameter {
            element: rest.element,
            preceding_named: rest.preceding_named.saturating_sub(surplus.len()),
        }),
        (None, Some(rest)) => Some(RestParameter {
            element: rest.element,
            preceding_named: (comment_function.positional.len() + rest.preceding_named)
                .saturating_sub(surplus.len()),
        }),
        (None, None) => None,
    };
    let ret = if matches!(inline_function.ret.kind(db), TyKind::Unknown) {
        comment_function.ret
    } else {
        inline_function.ret
    };
    // An inline binder wins over the comment binder in its position, constraint included.
    let mut binders = inline_scheme.binders.clone();
    binders.extend(
        comment_scheme
            .binders
            .iter()
            .skip(inline_scheme.binders.len())
            .copied(),
    );
    Some(Annotation {
        declared: Some(TypeScheme {
            binders,
            body: Ty::new(
                db,
                TyKind::Function(&FunctionType::declared(&surplus, &named, variadic, ret)),
            ),
        }),
        inline_parameters: inline
            .inline_parameters
            .iter()
            .filter(|name| {
                !comment_function
                    .named
                    .iter()
                    .any(|parameter| parameter.name.text(db) == name.as_str())
            })
            .cloned()
            .collect(),
        ..inline
    })
}

/// `@trust`, `@if-unknown` or `@new`: the annotation says how the value gets its type
/// rather than declaring one to check.
fn is_coercion(annotation: &Annotation) -> bool {
    annotation.trusted || annotation.if_unknown || annotation.new_nominal.is_some()
}

/// A place an annotation gives a type. Parameters are keyed by formal name, so a
/// positional comment type is resolved against the definition's formals first.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum Slot {
    Value,
    Parameter(String),
    Rest,
    Return,
}

#[derive(Debug, Clone)]
pub struct SlotType<'db> {
    pub slot: Slot,
    pub ty: Ty<'db>,
    /// The written type, for pointing at it.
    pub range: TextRange,
}

/// The slots a `#:` block gives a type, with the range of each written type. A block with
/// errors has no trustworthy slots, and an elided return is no slot at all. Against an
/// inline binding type (`signature` is `None`), the whole declared type is one value slot;
/// against a signature, a function annotation splits into parameter and return slots.
pub fn comment_slots<'db>(
    db: &'db dyn Db,
    node: &SyntaxNode,
    signature: Option<&SyntaxNode>,
) -> Vec<SlotType<'db>> {
    let annotation = lower_annotation(db, node);
    if !annotation.errors.is_empty() || !annotation.typing_errors.is_empty() {
        return Vec::new();
    }
    if let Some((name, arguments, range)) = annotation.new_nominal {
        let ty = Ty::new(db, TyKind::Named(name, &arguments));
        return vec![SlotType {
            slot: Slot::Value,
            ty,
            range,
        }];
    }
    let Some(declared) = annotation.declared else {
        return Vec::new();
    };
    // A coercion's type sits after its directive; a compact type is a direct child.
    let value_node = node
        .children()
        .find(|child| is_type_kind(child.kind()) && child.kind() != SyntaxKind::TYPE_BINDER_LIST)
        .or_else(|| {
            node.children()
                .filter(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
                .find_map(|directive| {
                    directive.children().find(|child| {
                        is_type_kind(child.kind()) && child.kind() != SyntaxKind::TYPE_BINDER_LIST
                    })
                })
        });
    let (TyKind::Function(declared_function), Some(function)) = (declared.body.kind(db), signature)
    else {
        return value_node
            .map(|value| SlotType {
                slot: Slot::Value,
                ty: declared.body,
                range: value.text_range(),
            })
            .into_iter()
            .collect();
    };
    let declaration = match value_node.as_ref().and_then(written_function_type) {
        Some(compact) => compact_declaration(&compact),
        None => expanded_declaration(node),
    };
    let Declaration { parameters, ret } = declaration;
    let formals = formals(function);
    let declared_names: Vec<&str> = declared_function
        .named
        .iter()
        .map(|parameter| parameter.name.text(db))
        .collect();
    let paired = pair_formals(
        formals.iter().map(|formal| formal.name.as_str()),
        &declared_names,
        declared_function.positional.len(),
    );
    let mut slots = Vec::new();
    // The lowered function holds the written parameters in the same order, the named ones
    // apart from those written without a name.
    let mut named = declared_function.named.iter();
    let mut positional = 0;
    for parameter in parameters {
        let slot = if parameter.rest {
            declared_function
                .variadic
                .as_ref()
                .map(|rest| (Slot::Rest, rest.element))
        } else if parameter.name.is_some() {
            named.next().map(|declared| {
                (
                    Slot::Parameter(declared.name.text(db).to_owned()),
                    declared.ty,
                )
            })
        } else {
            let index = positional;
            positional += 1;
            formals
                .iter()
                .zip(&paired)
                .find(|(_, paired)| **paired == Paired::Positional(index))
                .zip(declared_function.positional.get(index))
                .map(|((formal, _), ty)| (Slot::Parameter(formal.name.clone()), *ty))
        };
        if let (Some((slot, ty)), Some(written)) = (slot, parameter.ty) {
            slots.push(SlotType {
                slot,
                ty,
                range: written.text_range(),
            });
        }
    }
    if let Some(written) = ret {
        slots.push(SlotType {
            slot: Slot::Return,
            ty: declared_function.ret,
            range: written.text_range(),
        });
    }
    slots
}

/// The slots an inline site gives a type. A site with lowering errors has none.
pub fn inline_slots<'db>(db: &'db dyn Db, site: &SyntaxNode) -> Vec<SlotType<'db>> {
    let annotation = lower_inline_annotation(db, site);
    if let Some((name, arguments, range)) = annotation.new_nominal {
        return vec![SlotType {
            slot: Slot::Value,
            ty: Ty::new(db, TyKind::Named(name, &arguments)),
            range,
        }];
    }
    let Some(declared) = annotation.declared else {
        return Vec::new();
    };
    let type_range = |node: &SyntaxNode| {
        node.children()
            .find(|child| is_type_kind(child.kind()))
            .map(|ty| ty.text_range())
    };
    if site.kind() == SyntaxKind::BINARY_EXPR {
        // A coercion's type sits after its directive.
        return site
            .children()
            .find(|child| child.kind() == SyntaxKind::TYPE_ASCRIPTION)
            .and_then(|ascription| {
                type_range(&ascription).or_else(|| {
                    ascription
                        .children()
                        .find(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
                        .as_ref()
                        .and_then(type_range)
                })
            })
            .map(|range| SlotType {
                slot: Slot::Value,
                ty: declared.body,
                range,
            })
            .into_iter()
            .collect();
    }
    let TyKind::Function(function) = declared.body.kind(db) else {
        return Vec::new();
    };
    let mut slots = Vec::new();
    for formal in formals(site) {
        let Some(range) = formal.ascription.as_ref().and_then(type_range) else {
            continue;
        };
        if formal.name == "..." {
            if let Some(rest) = &function.variadic {
                slots.push(SlotType {
                    slot: Slot::Rest,
                    ty: rest.element,
                    range,
                });
            }
        } else if let Some(parameter) = function
            .named
            .iter()
            .find(|parameter| parameter.name.text(db) == formal.name)
        {
            slots.push(SlotType {
                slot: Slot::Parameter(formal.name),
                ty: parameter.ty,
                range,
            });
        }
    }
    if let Some(range) = site
        .children()
        .find(|child| child.kind() == SyntaxKind::RETURN_TYPE)
        .as_ref()
        .and_then(type_range)
    {
        slots.push(SlotType {
            slot: Slot::Return,
            ty: function.ret,
            range,
        });
    }
    slots
}

/// The type the written type `node` declares, read on its own with the type parameters of the
/// blocks, function types and literals around it in scope. `None` when lowering it reports an
/// error, which leaves the written type to the diagnostics.
pub fn lower_written_type<'db>(db: &'db dyn Db, node: &SyntaxNode) -> Option<Ty<'db>> {
    let mut lowering = Lowering::within(db, node, &[]);
    let mut bound = Vec::new();
    for ancestor in node.ancestors().skip(1) {
        for list in ancestor
            .children()
            .filter(|child| child.kind() == SyntaxKind::TYPE_BINDER_LIST)
        {
            lowering.lower_binders(&list, &mut bound, "a signature");
        }
    }
    let ty = lowering.lower_type(node);
    (lowering.errors.is_empty() && !lowering.beyond_check_depth).then_some(ty)
}

/// Whether the function literal that `statement` is or assigns has a signature the runtime
/// reads (see [`Annotation::written_signature`]). `block` is the `#:` block attached to the
/// statement.
pub fn has_written_signature(
    statement: &SyntaxNode,
    block: Option<&SyntaxNode>,
    inline: bool,
) -> bool {
    assigned_function(statement)
        .is_some_and(|function| signature_from(&function, block, inline).is_complete())
}

/// The inline site a statement carries: its own `: TYPE`, or else the signature on the
/// function literal it assigns or is.
pub fn statement_inline_site(statement: &SyntaxNode) -> Option<SyntaxNode> {
    if is_inline_site(statement) {
        return Some(statement.clone());
    }
    let function = assigned_function(statement)?;
    is_inline_site(&function).then_some(function)
}

/// `Record{...}` and `Tuple{...}` share the brace grammar of `list{...}` too.
fn has_row(node: &SyntaxNode) -> bool {
    node.children().any(|c| c.kind() == SyntaxKind::TYPE_ROW)
}

fn native_head(node: &SyntaxNode) -> Option<NativeHead> {
    match name_text(node).as_deref() {
        Some("Record") => Some(NativeHead::Record),
        Some("Tuple") => Some(NativeHead::Tuple),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeHead {
    Record,
    Tuple,
}

/// `DataFrame{...}` and the other table types share the brace grammar of `list{...}`; the head
/// word tells them apart.
fn frame_kind(node: &SyntaxNode) -> Option<FrameKind> {
    FrameKind::from_head(name_text(node).as_deref()?)
}

/// Whether a `#:` block carries `@pure`. Read apart from the lowered annotation, so a block
/// refused for its type still declares purity and the fact stays a per-item `bool`.
pub fn declares_pure(annotation: &SyntaxNode) -> bool {
    annotation.children().any(|child| {
        child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE && directive_name(&child) == "pure"
    })
}

fn has_word(node: &SyntaxNode, word: &str) -> bool {
    node.node_ref()
        .child_tokens()
        .any(|token| token.kind() == SyntaxKind::IDENT && token.text() == word)
}

// Past the check limit a type is refused only when type checking is on; past the parse limit the
// annotation shape itself is refused.
const CHECK_DEPTH_LIMIT: usize = 128;
const PARSE_DEPTH_LIMIT: usize = 160;

struct Lowering<'db> {
    db: &'db dyn Db,
    binders: Vec<Name<'db>>,
    /// Type parameters written as a `List`'s item (`List<T>`), which only a ry value can fill.
    list_item_binders: Vec<Name<'db>>,
    nominal_references: Vec<(String, TextRange)>,
    applied_references: Vec<(String, usize, TextRange)>,
    errors: Vec<(String, TextRange)>,
    vector_elements: Vec<(Ty<'db>, TextRange)>,
    depth: usize,
    beyond_check_depth: bool,
    beyond_parse_depth: bool,
    /// Set while lowering the definition's own compact type, the only place an elided `->` on the
    /// outermost function means "inferred from the body"; elsewhere it means `NULL`.
    definition_type: bool,
}

impl<'db> Lowering<'db> {
    /// A lowering of `node`, where the type parameters of the signatures around it are in
    /// scope as rigid variables.
    fn within(db: &'db dyn Db, node: &SyntaxNode, outer: &[Name<'db>]) -> Lowering<'db> {
        let mut binders = outer.to_vec();
        binders.extend(enclosing_type_parameters(db, node));
        Lowering {
            binders,
            ..Lowering::new(db)
        }
    }

    fn new(db: &'db dyn Db) -> Lowering<'db> {
        Lowering {
            db,
            binders: Vec::new(),
            list_item_binders: Vec::new(),
            nominal_references: Vec::new(),
            applied_references: Vec::new(),
            errors: Vec::new(),
            vector_elements: Vec::new(),
            depth: 0,
            beyond_check_depth: false,
            beyond_parse_depth: false,
            definition_type: false,
        }
    }

    /// Attaches what lowering collected. Any error strips the typing payload, so a broken
    /// annotation reports once and never cascades.
    fn finish(self, mut annotation: Annotation<'db>, range: TextRange) -> Annotation<'db> {
        let mut errors = self.errors;
        if self.beyond_parse_depth {
            errors.push((
                "this type annotation is nested too deeply (more than 160 levels)".to_owned(),
                range,
            ));
        } else if self.beyond_check_depth {
            annotation.typing_errors.push((
                "this type is nested too deeply to check (more than 128 levels)".to_owned(),
                range,
            ));
        }
        // Declared type names stay declared, with `Unknown` bodies, or every use of one would
        // add an unknown-type finding to the one already made.
        if !errors.is_empty() || !annotation.typing_errors.is_empty() {
            for definition in &mut annotation.definitions {
                definition.body = unknown(self.db);
            }
            return Annotation {
                errors,
                typing_errors: annotation.typing_errors,
                definitions: annotation.definitions,
                definition_sites: annotation.definition_sites,
                ..Annotation::default()
            };
        }
        annotation.nominal_references = self.nominal_references;
        annotation.applied_references = self.applied_references;
        annotation.vector_elements = self.vector_elements;
        annotation
    }

    /// `bound` spans every binder list of the block, so a duplicate across lists is caught too.
    /// A parameter that fills a `List`'s item is a ry value even when it is written `<T>`, so
    /// that a literal passed for it becomes a ry value rather than R's `double`.
    fn scheme_binders(&self, forall: Vec<Binder<'db>>) -> ThinVec<Binder<'db>> {
        forall
            .into_iter()
            .map(|binder| {
                if binder.bound == Bound::Constraint(Constraint::Unconstrained)
                    && self.list_item_binders.contains(&binder.name)
                {
                    Binder::constrained(binder.name, Constraint::ArrayItem)
                } else {
                    binder
                }
            })
            .collect()
    }

    fn lower_binders(&mut self, list: &SyntaxNode, bound: &mut Vec<Binder<'db>>, place: &str) {
        for binder in list
            .children()
            .filter(|c| c.kind() == SyntaxKind::TYPE_BINDER)
        {
            let Some(name) = name_text(&binder) else {
                continue;
            };
            let name = Name::new(self.db, name);
            // A constraint may span several words (`scalar numeric`).
            let words: Vec<SyntaxNode> = binder
                .children()
                .filter(|c| c.kind() == SyntaxKind::NAME)
                .skip(1)
                .collect();
            let constraint = match (words.first(), words.last()) {
                (Some(first), Some(last)) => {
                    let spelling = words
                        .iter()
                        .filter_map(|word| ast::Name::cast(word.clone())?.text())
                        .collect::<Vec<_>>()
                        .join(" ");
                    Constraint::from_spelling(&spelling).unwrap_or_else(|| {
                        let available: Vec<String> = Constraint::SPELLINGS
                            .iter()
                            .map(|(spelling, _)| format!("`{spelling}`"))
                            .collect();
                        let available = match available.split_last() {
                            Some((last, leading)) if !leading.is_empty() => {
                                format!("{}, or {last}", leading.join(", "))
                            }
                            _ => available.join(""),
                        };
                        // Blame the whole spelling, not just its first word.
                        self.errors.push((
                            format!(
                                "unknown type-parameter constraint `{spelling}`; the available constraints are {available}"
                            ),
                            first.text_range().cover(last.text_range()),
                        ));
                        Constraint::Unconstrained
                    })
                }
                _ => Constraint::Unconstrained,
            };
            // A type bound reads the binders before it, so `<T, U: list[T] | T[]>` lowers `T`
            // first.
            let type_bound = binder
                .children()
                .find(|child| is_type_kind(child.kind()))
                .map(|ty| self.lower_type(&ty));
            if bound.iter().any(|existing| existing.name == name) {
                self.errors.push((
                    format!(
                        "duplicate type parameter name `{}` in {place}",
                        name.text(self.db)
                    ),
                    binder.text_range(),
                ));
            }
            self.binders.push(name);
            bound.push(Binder {
                name,
                bound: match type_bound {
                    Some(ty) => Bound::Type(ty),
                    None => Bound::Constraint(constraint),
                },
            });
        }
    }

    fn lower_named_definition(
        &mut self,
        node: &SyntaxNode,
        keyword: &str,
    ) -> Option<NamedDefinition<'db>> {
        let name = name_text(node)?;
        let saved = self.binders.len();
        let mut parameters = Vec::new();
        if let Some(list) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::TYPE_BINDER_LIST)
        {
            self.lower_binders(&list, &mut parameters, "named type definition");
        }
        let body = node
            .children()
            .find(|c| is_type_kind(c.kind()) && c.kind() != SyntaxKind::TYPE_BINDER_LIST)
            .map(|ty| self.lower_type(&ty))
            .unwrap_or_else(|| unknown(self.db));
        self.binders.truncate(saved);
        Some(NamedDefinition {
            alias: keyword == "alias",
            inline: node.kind() == SyntaxKind::TYPE_DEFINITION,
            name: Name::new(self.db, name),
            parameters: parameters.into_iter().map(|binder| binder.name).collect(),
            body,
            opaque: (keyword == "opaque").then(|| Opaque {
                home: None,
                site: node.text_range(),
                hidden: false,
            }),
        })
    }

    /// `@trust TYPE`, `@if-unknown TYPE` or `@new Name<ARGS>`, from a `#:` directive, an
    /// inline binding's directive, or the head of `@Name(value)`.
    fn lower_coercion(
        &mut self,
        annotation: &mut Annotation<'db>,
        node: &SyntaxNode,
        directive: &str,
    ) {
        match directive {
            "trust" | "if-unknown" => {
                if directive == "trust" {
                    annotation.trusted = true;
                } else {
                    annotation.if_unknown = true;
                }
                // A coercion's own type is a definition's: an elided `->` takes the value's return.
                self.definition_type = true;
                let body = self.lower_child_type(node);
                self.definition_type = false;
                if let Some(body) = body {
                    annotation.declared = Some(TypeScheme::monomorphic(body));
                }
            }
            "new" => {
                let ty_node = node.children().find(|c| is_type_kind(c.kind()));
                let range = ty_node
                    .as_ref()
                    .map_or(node.text_range(), |ty| ty.text_range());
                let lowered = ty_node.map(|ty| self.lower_type(&ty));
                match lowered.map(|ty| ty.kind(self.db)) {
                    Some(TyKind::Named(name, arguments)) => {
                        annotation.new_nominal = Some((*name, arguments.to_vec(), range));
                    }
                    _ => self.errors.push((
                        "expected a nominal type reference after `@new`".to_owned(),
                        range,
                    )),
                }
            }
            _ => {}
        }
    }

    fn lower_child_type(&mut self, node: &SyntaxNode) -> Option<Ty<'db>> {
        let child = node.children().find(|c| is_type_kind(c.kind()))?;
        Some(self.lower_type(&child))
    }

    fn lower_type(&mut self, node: &SyntaxNode) -> Ty<'db> {
        self.depth += 1;
        if self.depth > CHECK_DEPTH_LIMIT {
            self.beyond_check_depth = true;
        }
        if self.depth > PARSE_DEPTH_LIMIT {
            self.beyond_parse_depth = true;
            self.depth -= 1;
            return unknown(self.db);
        }
        let lowered = self.lower_type_inner(node);
        self.depth -= 1;
        lowered
    }

    fn lower_type_inner(&mut self, node: &SyntaxNode) -> Ty<'db> {
        match node.kind() {
            SyntaxKind::TYPE_REF => self.lower_type_ref(node),
            SyntaxKind::TYPE_PAREN => self
                .lower_child_type(node)
                .unwrap_or_else(|| unknown(self.db)),
            SyntaxKind::TYPE_UNION => {
                let members: Vec<Ty<'db>> = node
                    .children()
                    .filter(|c| is_type_kind(c.kind()))
                    .map(|member| self.lower_type(&member))
                    .collect();
                let union = union_of(self.db, members);
                if let Some((first, second)) = conflicting_tags(self.db, union) {
                    let mut renderer = TypeRenderer::default();
                    let first = renderer.render(self.db, first);
                    let second = renderer.render(self.db, second);
                    self.errors.push((
                        format!(
                            "`{first}` and `{second}` give one tag two payload shapes. Within a union a tag has one shape, so that a `match` can tell its cases apart"
                        ),
                        node.text_range(),
                    ));
                    return unknown(self.db);
                }
                union
            }
            SyntaxKind::TYPE_OTHER_TAGS => Ty::new(self.db, TyKind::OtherTags),
            SyntaxKind::TYPE_ROW => {
                self.errors.push((
                    "`..R` stands for the other columns of a `DataFrame{...}` or the other fields of a `Record{...}`, so it can only end one of those types".to_owned(),
                    node.text_range(),
                ));
                unknown(self.db)
            }
            SyntaxKind::TYPE_TAG => {
                let name = Name::new(self.db, name_text(node).unwrap_or_default());
                let payload = match node
                    .children()
                    .find(|c| c.kind() == SyntaxKind::TYPE_TAG_PAYLOAD)
                {
                    Some(payload) => {
                        if let Some(row) = payload
                            .children()
                            .find(|c| c.kind() == SyntaxKind::TYPE_ROW)
                        {
                            self.errors.push((
                                "a tag's payload lists every field, so it cannot end in a row such as `..R`".to_owned(),
                                row.text_range(),
                            ));
                            return unknown(self.db);
                        }
                        let named = payload
                            .children()
                            .any(|c| c.kind() == SyntaxKind::TYPE_FIELD);
                        let positional = payload.children().any(|c| is_type_kind(c.kind()));
                        match (named, positional) {
                            (true, true) => {
                                self.errors.push((
                                    "a tag's payload is all named or all positional: write every item as `name: TYPE`, or none of them".to_owned(),
                                    payload.text_range(),
                                ));
                                return unknown(self.db);
                            }
                            (true, false) => {
                                Some(self.lower_native_fields(&payload, "this tag's payload"))
                            }
                            (false, true) => Some(self.lower_native_items(&payload)),
                            (false, false) => None,
                        }
                    }
                    None => None,
                };
                Ty::new(self.db, TyKind::Tag(name, payload))
            }
            SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_RECORD if native_head(node).is_some() => {
                let named = node.children().any(|c| c.kind() == SyntaxKind::TYPE_FIELD);
                let positional = node
                    .children()
                    .any(|c| is_type_kind(c.kind()) && c.kind() != SyntaxKind::TYPE_ROW);
                match native_head(node) {
                    Some(NativeHead::Record) if positional => {
                        self.errors.push((
                            "a `Record{...}` type names every field: write each one as `name: TYPE`, or use `Tuple{...}` for unnamed items".to_owned(),
                            node.text_range(),
                        ));
                        unknown(self.db)
                    }
                    Some(NativeHead::Tuple) if has_row(node) => {
                        self.errors.push((
                            "a `Tuple{...}` type lists a fixed number of items, so it cannot end in a row such as `..R`. Use `Record{...}` for named fields".to_owned(),
                            node.text_range(),
                        ));
                        unknown(self.db)
                    }
                    Some(NativeHead::Tuple) if named => {
                        self.errors.push((
                            "a `Tuple{...}` type lists its items without names: drop the names, or use `Record{...}` for named fields".to_owned(),
                            node.text_range(),
                        ));
                        unknown(self.db)
                    }
                    Some(NativeHead::Record) => {
                        let Ok(row) = self.lower_row(node) else {
                            return unknown(self.db);
                        };
                        let record = self.lower_native_fields(node, "this `Record{...}` type");
                        match (row, record.kind(self.db)) {
                            (Some(row), TyKind::NativeRecord(fields, None)) => {
                                native_record_with_row(self.db, fields.iter().copied(), Some(row))
                            }
                            _ => record,
                        }
                    }
                    _ => self.lower_native_items(node),
                }
            }
            SyntaxKind::TYPE_VECTOR | SyntaxKind::TYPE_LIST => {
                let element = self
                    .lower_child_type(node)
                    .unwrap_or_else(|| unknown(self.db));
                let vector = node.kind() == SyntaxKind::TYPE_VECTOR;
                if vector {
                    self.vector_elements.push((element, node.text_range()));
                }
                let kind = match (vector, has_word(node, "named")) {
                    (true, false) => TyKind::Vector(element),
                    (true, true) => TyKind::NamedVector(element),
                    (false, false) => TyKind::List(element),
                    (false, true) => TyKind::NamedList(element),
                };
                Ty::new(self.db, kind)
            }
            SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_RECORD
                if name_text(node).as_deref() == Some("environment") =>
            {
                self.lower_environment(node)
            }
            SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_RECORD
                if node
                    .children()
                    .any(|c| c.kind() == SyntaxKind::TYPE_OTHER_TAGS) =>
            {
                self.lower_open_record(node)
            }
            SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_RECORD
                if has_row(node) && frame_kind(node).is_none_or(|kind| kind.class().is_some()) =>
            {
                // A data.table or data.frame type comes from R code, whose verbs build every
                // column of their result, so it lists them all.
                let hint = match frame_kind(node).and_then(FrameKind::class) {
                    Some(class) => format!(". A `{class}{{...}}` type lists every column"),
                    None => ". A `list{...}` type ends in `..` to accept more fields".to_owned(),
                };
                self.errors.push((
                    format!("only a `DataFrame{{...}}`, `LazyFrame{{...}}` or `Record{{...}}` type can end in a row such as `..R`{hint}"),
                    node.text_range(),
                ));
                unknown(self.db)
            }
            SyntaxKind::TYPE_TUPLE | SyntaxKind::TYPE_RECORD
                if node.children().any(|c| c.kind() == SyntaxKind::TYPE_FIELD)
                    && node
                        .children()
                        .any(|c| is_type_kind(c.kind()) && c.kind() != SyntaxKind::TYPE_ROW) =>
            {
                self.errors.push((
                    if let Some(kind) = frame_kind(node) {
                        format!("a `{}{{...}}` type names every column: write each one as `name: TYPE`", kind.head())
                    } else {
                        "a `list{...}` type cannot mix named and unnamed items: write every item as `name: TYPE`, or none of them".to_owned()
                    },
                    node.text_range(),
                ));
                unknown(self.db)
            }
            SyntaxKind::TYPE_TUPLE if frame_kind(node).is_some() => {
                let kind = frame_kind(node).unwrap_or(FrameKind::DataFrame);
                if node
                    .children()
                    .any(|c| is_type_kind(c.kind()) && c.kind() != SyntaxKind::TYPE_ROW)
                {
                    self.errors.push((
                        format!(
                            "a `{}{{...}}` type names every column: write each one as `name: TYPE`",
                            kind.head()
                        ),
                        node.text_range(),
                    ));
                    return unknown(self.db);
                }
                let Ok(row) = self.lower_row(node) else {
                    return unknown(self.db);
                };
                Ty::new(self.db, TyKind::Frame(kind, Some(&[]), row))
            }
            SyntaxKind::TYPE_TUPLE => {
                let items = node
                    .children()
                    .filter(|c| is_type_kind(c.kind()))
                    .map(|item| self.lower_type(&item))
                    .collect::<Vec<_>>();
                Ty::new(self.db, TyKind::Tuple(&items))
            }
            SyntaxKind::TYPE_RECORD => {
                let frame = frame_kind(node);
                let Ok(row) = self.lower_row(node) else {
                    return unknown(self.db);
                };
                let fields: Vec<(String, SyntaxNode)> = node
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::TYPE_FIELD)
                    .filter_map(|field| Some((name_text(&field)?, field)))
                    .collect();
                let mut seen = FxHashSet::default();
                for (name, field) in &fields {
                    if !seen.insert(name.as_str()) {
                        self.errors.push((
                            if let Some(kind) = frame {
                                format!(
                                    "the column `{name}` appears twice in this `{}{{...}}` type",
                                    kind.head()
                                )
                            } else {
                                format!(
                                    "the field `{name}` appears twice in this `list{{...}}` type"
                                )
                            },
                            field.text_range(),
                        ));
                    }
                }
                // In `GroupBy{g: character[]; x: double[]}` the fields before the `;` are keys.
                let frame = match frame {
                    Some(FrameKind::GroupBy { lazy, .. }) => {
                        let keys = node
                            .children_with_tokens()
                            .find(|element| element.kind() == SyntaxKind::SEMICOLON)
                            .map_or(0, |semicolon| {
                                fields
                                    .iter()
                                    .filter(|(_, field)| {
                                        field.text_range().end() <= semicolon.text_range().start()
                                    })
                                    .count()
                            });
                        Some(FrameKind::GroupBy {
                            lazy,
                            keys: keys as u32,
                        })
                    }
                    other => other,
                };
                let fields = fields
                    .into_iter()
                    .map(|(name, field)| {
                        let ty = self
                            .lower_child_type(&field)
                            .unwrap_or_else(|| unknown(self.db));
                        RecordField {
                            name: Name::new(self.db, name),
                            ty: if frame.is_some() {
                                column_type(self.db, ty)
                            } else {
                                ty
                            },
                            optional: false,
                        }
                    })
                    .collect::<Vec<_>>();
                Ty::new(
                    self.db,
                    if let Some(kind) = frame {
                        TyKind::Frame(kind, Some(&fields), row)
                    } else {
                        TyKind::Record(&fields)
                    },
                )
            }
            SyntaxKind::TYPE_APPLY => {
                let name_node = node.children().find(|c| c.kind() == SyntaxKind::NAME);
                let text = name_node
                    .clone()
                    .and_then(ast::Name::cast)
                    .and_then(|name| name.text())
                    .unwrap_or_default();
                let name = Name::new(self.db, text.clone());
                let reference = name_node
                    .filter(|_| !text.is_empty())
                    .map(|name_node| name_node.text_range());
                if reference.is_some() && self.binders.contains(&name) {
                    self.errors.push((
                        format!("`{text}` is a type parameter here and takes no type arguments"),
                        node.text_range(),
                    ));
                    return unknown(self.db);
                }
                if let Some(range) = reference {
                    self.nominal_references.push((text.clone(), range));
                }
                let arguments: Vec<Ty<'db>> = node
                    .children()
                    .find(|c| c.kind() == SyntaxKind::TYPE_ARG_LIST)
                    .map(|list| {
                        list.children()
                            .filter(|c| is_type_kind(c.kind()))
                            .map(|argument| self.lower_type(&argument))
                            .collect()
                    })
                    .unwrap_or_default();
                // A `List` holds ry values only, so an R vector, R scalar, `NULL` or function item
                // has no place in it.
                if text == "List"
                    && let [item] = arguments.as_slice()
                    && let TyKind::Rigid(binder) = item.kind(self.db)
                {
                    self.list_item_binders.push(*binder);
                }
                if text == "List"
                    && let [item] = arguments.as_slice()
                    && !array_item_annotation(self.db, *item)
                    && let Some(argument) = node
                        .children()
                        .find(|c| c.kind() == SyntaxKind::TYPE_ARG_LIST)
                        .and_then(|list| list.children().find(|c| is_type_kind(c.kind())))
                {
                    self.errors.push((
                        format!(
                            "a `List` cannot hold `{}`. Its items are ry values: numbers such as `I64` and `F64`, `Bool`, `Str`, records, tuples, tags and collections",
                            node_text(&argument).trim()
                        ),
                        argument.text_range(),
                    ));
                }
                if let Some(range) = reference {
                    self.applied_references.push((text, arguments.len(), range));
                }
                Ty::new(self.db, TyKind::Named(name, &arguments))
            }
            SyntaxKind::TYPE_FUNCTION => self.lower_function_type(node),
            _ => unknown(self.db),
        }
    }

    /// The type parameter a brace type's closing `..R` names, `None` without one, or `Err` after
    /// reporting a row that is not last or names no type parameter in scope.
    fn lower_row(&mut self, node: &SyntaxNode) -> Result<Option<Ty<'db>>, ()> {
        let Some(row) = node.children().find(|c| c.kind() == SyntaxKind::TYPE_ROW) else {
            return Ok(None);
        };
        if row
            .next_sibling()
            .is_some_and(|next| next.kind() == SyntaxKind::TYPE_FIELD || is_type_kind(next.kind()))
        {
            self.errors.push((
                "a row such as `..R` stands for the remaining columns or fields, so it comes last"
                    .to_owned(),
                row.text_range(),
            ));
            return Err(());
        }
        let text = row.text().to_string();
        let name = text.trim().trim_start_matches("..");
        let binder = Name::new(self.db, name.to_owned());
        if !self.binders.contains(&binder) {
            self.errors.push((
                format!(
                    "`{name}` in `..{name}` must be a type parameter: declare it, as in `<{name}> fn(...)` or `function<{name}>(...)`"
                ),
                row.text_range(),
            ));
            return Err(());
        }
        Ok(Some(Ty::new(self.db, TyKind::Rigid(binder))))
    }

    fn lower_native_items(&mut self, node: &SyntaxNode) -> Ty<'db> {
        let items = node
            .children()
            .filter(|c| is_type_kind(c.kind()))
            .map(|item| self.lower_type(&item))
            .collect::<Vec<_>>();
        Ty::new(self.db, TyKind::NativeTuple(&items))
    }

    fn lower_native_fields(&mut self, node: &SyntaxNode, container: &str) -> Ty<'db> {
        let mut seen = FxHashSet::default();
        let mut fields = Vec::new();
        for field in node
            .children()
            .filter(|c| c.kind() == SyntaxKind::TYPE_FIELD)
        {
            let Some(name) = name_text(&field) else {
                continue;
            };
            if !seen.insert(name.clone()) {
                self.errors.push((
                    format!("the field `{name}` appears twice in {container}"),
                    field.text_range(),
                ));
                continue;
            }
            let ty = self
                .lower_child_type(&field)
                .unwrap_or_else(|| unknown(self.db));
            fields.push(NativeField {
                name: Name::new(self.db, name),
                ty,
            });
        }
        native_record(self.db, fields)
    }

    /// `list{a: A, ..}`: the fields a record must have at least. `..` stands for the rest, so
    /// it pairs with named fields only.
    fn lower_open_record(&mut self, node: &SyntaxNode) -> Ty<'db> {
        if let Some(kind) = frame_kind(node) {
            self.errors.push((
                format!(
                    "a `{}{{...}}` type lists every column, so it cannot end in `..`",
                    kind.head()
                ),
                node.text_range(),
            ));
            return unknown(self.db);
        }
        match self.lower_named_fields(node, "`..` stands for more named fields, so every item of this `list{...}` type needs a name: write it as `name: TYPE`") {
            Some(fields) => Ty::new(self.db, TyKind::OpenRecord(&fields)),
            None => unknown(self.db),
        }
    }

    /// `environment{count: integer}`: the bindings an environment holds, each with its type.
    fn lower_environment(&mut self, node: &SyntaxNode) -> Ty<'db> {
        if let Some(other) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::TYPE_OTHER_TAGS)
        {
            self.errors.push((
                "an `environment{...}` type lists every binding the environment may hold, so it cannot end in `..`. Write plain `environment` for one whose bindings are not typed".to_owned(),
                other.text_range(),
            ));
            return unknown(self.db);
        }
        match self.lower_named_fields(
            node,
            "an `environment{...}` type names every binding: write it as `name: TYPE`",
        ) {
            Some(fields) => Ty::new(self.db, TyKind::Environment(&fields)),
            None => unknown(self.db),
        }
    }

    /// The `name: TYPE` fields of a brace type, or `None` after reporting an unnamed item.
    fn lower_named_fields(
        &mut self,
        node: &SyntaxNode,
        unnamed_message: &str,
    ) -> Option<Vec<RecordField<'db>>> {
        let mut seen = FxHashSet::default();
        let mut fields = Vec::new();
        for child in node.children() {
            match child.kind() {
                SyntaxKind::TYPE_OTHER_TAGS => {}
                SyntaxKind::TYPE_FIELD => {
                    let Some(name) = name_text(&child) else {
                        continue;
                    };
                    if !seen.insert(name.clone()) {
                        self.errors.push((
                            format!("the field `{name}` appears twice in this type"),
                            child.text_range(),
                        ));
                        continue;
                    }
                    let ty = self
                        .lower_child_type(&child)
                        .unwrap_or_else(|| unknown(self.db));
                    fields.push(RecordField {
                        name: Name::new(self.db, name),
                        ty,
                        optional: false,
                    });
                }
                kind if is_type_kind(kind) => {
                    self.errors
                        .push((unnamed_message.to_owned(), child.text_range()));
                    return None;
                }
                _ => {}
            }
        }
        Some(fields)
    }

    fn lower_type_ref(&mut self, node: &SyntaxNode) -> Ty<'db> {
        let Some(token) = node.first_token() else {
            return unknown(self.db);
        };
        let text = token.text();
        match text {
            "Any" | "any" => return any(self.db),
            "Unknown" | "unknown" => return unknown(self.db),
            "NULL" | "null" => return null(self.db),
            "logical" => return scalar(self.db, Atomic::Logical),
            "integer" => return scalar(self.db, Atomic::Integer),
            "double" => return scalar(self.db, Atomic::Double),
            "complex" => return scalar(self.db, Atomic::Complex),
            "character" => return scalar(self.db, Atomic::Character),
            "raw" => return scalar(self.db, Atomic::Raw),
            _ if let Some(native) = NativeScalar::from_name(text) => {
                return Ty::new(self.db, TyKind::Native(native));
            }
            "DataFrame" => {
                return Ty::new(self.db, TyKind::Frame(FrameKind::DataFrame, None, None));
            }
            "LazyFrame" => {
                return Ty::new(self.db, TyKind::Frame(FrameKind::LazyFrame, None, None));
            }
            "GroupBy" => {
                return Ty::new(
                    self.db,
                    TyKind::Frame(
                        FrameKind::GroupBy {
                            lazy: false,
                            keys: 0,
                        },
                        None,
                        None,
                    ),
                );
            }
            "LazyGroupBy" => {
                return Ty::new(
                    self.db,
                    TyKind::Frame(
                        FrameKind::GroupBy {
                            lazy: true,
                            keys: 0,
                        },
                        None,
                        None,
                    ),
                );
            }
            _ => {}
        }
        let name = Name::new(self.db, text);
        if self.binders.contains(&name) {
            return Ty::new(self.db, TyKind::Rigid(name));
        }
        self.nominal_references
            .push((text.to_owned(), token.text_range()));
        Ty::new(self.db, TyKind::Named(name, &[]))
    }

    fn lower_function_type(&mut self, node: &SyntaxNode) -> Ty<'db> {
        let mut positional = Vec::new();
        let mut named = Vec::new();
        let mut variadic = None;
        if let Some(list) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::TYPE_PARAMETER_LIST)
        {
            for parameter in list
                .children()
                .filter(|c| c.kind() == SyntaxKind::TYPE_PARAMETER)
            {
                let has_dots = has_token(&parameter, SyntaxKind::DOTS);
                let optional = has_token(&parameter, SyntaxKind::L_BRACKET);
                let name = name_text(&parameter);
                let ty = self.lower_child_type(&parameter);
                if variadic.is_some() && (has_dots || name.is_none()) {
                    self.errors.push((
                        if has_dots {
                            "a function type has at most one `...` rest parameter".to_owned()
                        } else {
                            "a parameter after `...` is matched by name only, so it needs a name: write `name: TYPE`".to_owned()
                        },
                        parameter.text_range(),
                    ));
                }
                if has_dots {
                    variadic = Some(RestParameter {
                        element: ty.unwrap_or_else(|| any(self.db)),
                        preceding_named: named.len(),
                    });
                } else if let Some(name) = name {
                    named.push(Parameter {
                        name: Name::new(self.db, name),
                        ty: ty.unwrap_or_else(|| unknown(self.db)),
                        optional,
                        default: None,
                    });
                } else {
                    positional.push(ty.unwrap_or_else(|| unknown(self.db)));
                }
            }
        }
        let ret = node
            .children()
            .filter(|c| is_type_kind(c.kind()))
            .last()
            .filter(|_| has_token(node, SyntaxKind::MINUS_GREATER))
            .map(|ty| self.lower_type(&ty))
            .unwrap_or_else(|| {
                if self.definition_type && self.depth == 1 {
                    unknown(self.db)
                } else {
                    null(self.db)
                }
            });
        Ty::new(
            self.db,
            TyKind::Function(&FunctionType::declared(&positional, &named, variadic, ret)),
        )
    }
}

/// Whether an annotation may name `item` as a List's item type. A type name, a type parameter or
/// a union of tags may; an R vector, list, `NULL`, function, `complex` or `raw` may not.
fn array_item_annotation(db: &dyn Db, item: Ty<'_>) -> bool {
    match item.kind(db) {
        TyKind::Union(members) => members
            .iter()
            .all(|&member| array_item_annotation(db, member)),
        TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => true,
        // A collection, an alias or a project's own nominal type is checked where it is used.
        // Asking the stub library which `List` this is would make lowering a stub depend on the
        // library it builds.
        TyKind::Named(..) => true,
        _ => is_ry_value_type(db, item),
    }
}
