//! Lowering is total: malformed pieces lower to `Unknown` (their syntax errors already mark
//! them), so annotations never cascade. A `#:` block and an inline annotation lower through
//! the same type lowering into the same `Annotation`, so the checker sees one shape whichever
//! way a type was written.

use {
    crate::{
        Db,
        diagnostics::TypeRenderer,
        types::{
            Atomic, Constraint, FrameKind, FunctionType, Name, NativeField, Parameter, RecordField,
            RestParameter, Ty, TyKind, TypeScheme, any, column_type, conflicting_tags,
            native_record, null, scalar, substitute_rigid, union_of, unknown,
        },
    },
    rustc_hash::{FxHashMap, FxHashSet},
    std::mem::take,
    syntax::{
        GreenRef, SyntaxKind, SyntaxNode, TextRange, TextSize, ast, is_green_inline_annotation,
        matching_nodes,
    },
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
}

pub fn lower_annotation<'db>(db: &'db dyn Db, node: &SyntaxNode) -> Annotation<'db> {
    debug_assert_eq!(node.kind(), SyntaxKind::ANNOTATION);
    let mut lowering = Lowering::new(db);
    let mut annotation = Annotation::default();
    match block_refusal(node) {
        Some(BlockRefusal::FormMixing(message)) => {
            annotation
                .errors
                .push((message.to_owned(), node.text_range()));
            return annotation;
        }
        Some(BlockRefusal::Unparsed) => return annotation,
        None => {}
    }

    let mut forall: Vec<(Name<'db>, Constraint)> = Vec::new();
    let mut params: Vec<(String, Ty<'db>, bool)> = Vec::new();
    let mut rest: Option<Ty<'db>> = None;
    let mut ret: Option<Ty<'db>> = None;
    let mut saw_param = false;
    let mut saw_return = false;

    for child in node.children() {
        match child.kind() {
            SyntaxKind::ANNOTATION_DIRECTIVE => {
                let directive = directive_name(&child);
                match directive.as_str() {
                    "type" | "alias" => {
                        if let Some(definition) =
                            lowering.lower_named_definition(&child, directive == "alias")
                        {
                            annotation.definitions.push(definition);
                            annotation.definition_sites.push(child.text_range());
                        }
                    }
                    "forall" => {
                        if saw_param || saw_return {
                            lowering.errors.push((
                                "`@forall` directives must appear before `@param`, `@return`, or `@returns` in the same `#:` block.".to_owned(),
                                child.text_range(),
                            ));
                        }
                        lowering.lower_binders(&child, &mut forall, "expanded function annotation");
                    }
                    "param" => {
                        if saw_return {
                            lowering.errors.push((
                                "`@param` directives must appear before `@return` or `@returns` in the same `#:` block.".to_owned(),
                                child.text_range(),
                            ));
                        }
                        saw_param = true;
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
                        if saw_return {
                            lowering.errors.push((
                                "cannot use more than one `@return` or `@returns` directive in the same `#:` block.".to_owned(),
                                child.text_range(),
                            ));
                        }
                        saw_return = true;
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
                    binders: take(&mut forall),
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
            .collect();
        let function = FunctionType {
            positional: Vec::new(),
            named,
            variadic: rest.map(|element| RestParameter {
                element,
                preceding_named: params.len(),
            }),
            ret: ret.unwrap_or_else(|| unknown(db)),
        };
        annotation.declared = Some(TypeScheme {
            binders: forall,
            body: Ty::new(db, TyKind::Function(function)),
        });
    }
    lowering.finish(annotation, node.text_range())
}

/// A binding (`name: TYPE <- value`), a function literal with any of `<...>`, a
/// parameter's `: TYPE`, or `-> TYPE`, a type definition (`type Name = TYPE`), or a
/// nominal constructor (`@Name(value)`).
pub fn is_inline_site(node: &SyntaxNode) -> bool {
    is_green_inline_site(GreenRef::new(&node.green()))
}

/// [`is_inline_site`] on the green tree, for searches that skip building red nodes.
pub fn is_green_inline_site(node: GreenRef<'_>) -> bool {
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
    debug_assert!(is_inline_site(site));
    let mut lowering = Lowering::new(db);
    let mut annotation = Annotation::default();
    match site.kind() {
        SyntaxKind::TYPE_DEFINITION => {
            let alias = type_definition_keyword(site) == "alias";
            if let Some(definition) = lowering.lower_named_definition(site, alias) {
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
    let ret = site
        .children()
        .find(|child| child.kind() == SyntaxKind::RETURN_TYPE)
        .and_then(|ret| lowering.lower_child_type(&ret))
        .unwrap_or_else(|| unknown(db));
    annotation.inline_parameters = named
        .iter()
        .map(|parameter: &Parameter<'db>| parameter.name.text(db).to_owned())
        .collect();
    annotation.declared = Some(TypeScheme {
        binders: forall,
        body: Ty::new(
            db,
            TyKind::Function(FunctionType {
                positional: Vec::new(),
                named,
                variadic,
                ret,
            }),
        ),
    });
    lowering.finish(annotation, inline_range(site))
}

/// `type` or `alias`, the word that opens an inline type definition.
pub fn type_definition_keyword(definition: &SyntaxNode) -> String {
    definition
        .first_token()
        .map(|token| token.text().to_owned())
        .unwrap_or_default()
}

/// The written inline annotations of a site, from the first to the last, so a finding
/// about the whole annotation never covers the value or the body.
fn inline_range(site: &SyntaxNode) -> TextRange {
    let mut parts = matching_nodes(site, |node, parent| {
        is_green_inline_annotation(node.kind(), parent.map(GreenRef::kind))
    })
    .into_iter()
    .filter(|node| node.ancestors().find(is_inline_site).as_ref() == Some(site))
    .map(|node| node.text_range());
    let Some(first) = parts.next() else {
        return site.text_range();
    };
    parts.fold(first, |covered, part| covered.cover(part))
}

/// One formal of a `function(...)` literal, as the parser wrote it.
pub struct Formal {
    pub name: String,
    pub has_default: bool,
    pub ascription: Option<SyntaxNode>,
}

pub fn formals(function: &SyntaxNode) -> Vec<Formal> {
    let Some(list) = function
        .children()
        .find(|child| child.kind() == SyntaxKind::PARAMETER_LIST)
    else {
        return Vec::new();
    };
    list.children()
        .filter(|child| child.kind() == SyntaxKind::PARAMETER)
        .filter_map(|parameter| {
            let token = parameter.first_token()?;
            let text = token.text();
            let name = text
                .strip_prefix('`')
                .and_then(|text| text.strip_suffix('`'))
                .unwrap_or(text)
                .to_owned();
            Some(Formal {
                name,
                has_default: has_token(&parameter, SyntaxKind::EQ),
                ascription: parameter
                    .children()
                    .find(|part| part.kind() == SyntaxKind::TYPE_ASCRIPTION),
            })
        })
        .collect()
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
    let renaming: FxHashMap<Name<'db>, Ty<'db>> = comment_scheme
        .binders
        .iter()
        .zip(&inline_scheme.binders)
        .map(|((comment_name, _), (inline_name, _))| {
            (*comment_name, Ty::new(db, TyKind::Rigid(*inline_name)))
        })
        .collect();
    let renamed = substitute_rigid(db, comment_scheme.body, &renaming);
    let TyKind::Function(comment_function) = renamed.kind(db) else {
        return Some(inline);
    };
    let formals = formals(signature);
    let mut used = vec![false; comment_function.named.len()];
    let mut positional = comment_function.positional.iter();
    let mut named = Vec::new();
    for formal in formals.iter().filter(|formal| formal.name != "...") {
        let from_comment = match comment_function
            .named
            .iter()
            .position(|parameter| parameter.name.text(db) == formal.name)
        {
            Some(index) => {
                used[index] = true;
                Some(comment_function.named[index].clone())
            }
            None => positional.next().map(|&ty| Parameter {
                name: Name::new(db, formal.name.clone()),
                ty,
                optional: formal.has_default,
                default: None,
            }),
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
            .map(|(parameter, _)| parameter.clone()),
    );
    let surplus: Vec<Ty<'db>> = positional.copied().collect();
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
                TyKind::Function(FunctionType {
                    positional: surplus,
                    named,
                    variadic,
                    ret,
                }),
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
        let ty = Ty::new(db, TyKind::Named(name, arguments));
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
    // Each written parameter type, in source order, with its name (`None` when positional).
    let mut written: Vec<(Option<String>, bool, Option<TextRange>)> = Vec::new();
    let mut written_return = None;
    match value_node.filter(|value| value.kind() == SyntaxKind::TYPE_FUNCTION) {
        Some(compact) => {
            for parameter in compact
                .children()
                .filter(|child| child.kind() == SyntaxKind::TYPE_PARAMETER_LIST)
                .flat_map(|list| list.children())
                .filter(|child| child.kind() == SyntaxKind::TYPE_PARAMETER)
            {
                let range = parameter
                    .children()
                    .find(|child| is_type_kind(child.kind()))
                    .map(|ty| ty.text_range());
                written.push((
                    name_text(&parameter),
                    has_token(&parameter, SyntaxKind::DOTS),
                    range,
                ));
            }
            if has_token(&compact, SyntaxKind::MINUS_GREATER) {
                written_return = compact
                    .children()
                    .filter(|child| is_type_kind(child.kind()))
                    .last()
                    .map(|ty| ty.text_range());
            }
        }
        None => {
            for directive in node
                .children()
                .filter(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
            {
                let range = directive
                    .children()
                    .find(|child| is_type_kind(child.kind()))
                    .map(|ty| ty.text_range());
                match directive_name(&directive).as_str() {
                    "param" => {
                        let name = name_text(&directive);
                        let dots = name.as_deref() == Some("...");
                        written.push((name.filter(|_| !dots), dots, range));
                    }
                    "return" | "returns" => written_return = range,
                    _ => {}
                }
            }
        }
    }
    let formal_names: Vec<String> = formals(function)
        .into_iter()
        .map(|formal| formal.name)
        .filter(|name| name != "...")
        .collect();
    let mut slots = Vec::new();
    let mut named = declared_function.named.iter();
    let mut positional = declared_function.positional.iter();
    let mut positional_names = formal_names.iter().filter(|name| {
        !declared_function
            .named
            .iter()
            .any(|parameter| parameter.name.text(db) == name.as_str())
    });
    for (name, dots, range) in written {
        let Some(range) = range else {
            continue;
        };
        if dots {
            if let Some(rest) = &declared_function.variadic {
                slots.push(SlotType {
                    slot: Slot::Rest,
                    ty: rest.element,
                    range,
                });
            }
        } else if name.is_some() {
            if let Some(parameter) = named.next() {
                slots.push(SlotType {
                    slot: Slot::Parameter(parameter.name.text(db).to_owned()),
                    ty: parameter.ty,
                    range,
                });
            }
        } else if let (Some(&ty), Some(formal)) = (positional.next(), positional_names.next()) {
            slots.push(SlotType {
                slot: Slot::Parameter(formal.clone()),
                ty,
                range,
            });
        }
    }
    if let Some(range) = written_return {
        slots.push(SlotType {
            slot: Slot::Return,
            ty: declared_function.ret,
            range,
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
            ty: Ty::new(db, TyKind::Named(name, arguments)),
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

/// The inline site a statement carries: its own `: TYPE`, or else the signature on the
/// function literal it assigns or is.
pub fn statement_inline_site(statement: &SyntaxNode) -> Option<SyntaxNode> {
    if is_inline_site(statement) {
        return Some(statement.clone());
    }
    let function = assigned_function(statement)?;
    is_inline_site(&function).then_some(function)
}

/// The function literal a statement assigns (either direction) or is.
pub fn assigned_function(statement: &SyntaxNode) -> Option<SyntaxNode> {
    match statement.kind() {
        SyntaxKind::FUNCTION_DEF => Some(statement.clone()),
        SyntaxKind::BINARY_EXPR => {
            let binary = ast::BinaryExpr::cast(statement.clone())?;
            let value = match binary.operator()?.kind() {
                SyntaxKind::LESS_MINUS | SyntaxKind::LESS2_MINUS | SyntaxKind::EQ => binary.rhs(),
                SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => binary.lhs(),
                _ => None,
            }?;
            (value.kind() == SyntaxKind::FUNCTION_DEF).then_some(value)
        }
        _ => None,
    }
}

/// A `#:` block must commit to exactly one form.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockForm {
    Compact,
    Expanded,
    Definition,
}

pub enum BlockRefusal {
    FormMixing(&'static str),
    /// Silent: the parser already reported what was wrong.
    Unparsed,
}

/// A line's form is that of its first item, and only whole lines are compared. A line with a
/// second item failed to parse (recovery re-parents an unreadable type's pieces to the block), so
/// it is `Unparsed`, not form mixing.
pub fn block_refusal(node: &SyntaxNode) -> Option<BlockRefusal> {
    // A refused region has no trustworthy payload: a refused binder, for instance, leaves type
    // parameters unbound, which would be blamed a second time as unknown types.
    if node
        .descendants()
        .any(|descendant| descendant.kind() == SyntaxKind::ERROR)
    {
        return Some(BlockRefusal::Unparsed);
    }
    // Markers come from all descendant tokens, not just direct children: a line continuing a
    // still-open node puts its marker inside that node.
    let markers: Vec<TextSize> = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::ANNOTATION_MARKER)
        .map(|token| token.text_range().start())
        .collect();
    let line_of = |start: TextSize| markers.iter().filter(|&&at| at < start).count();

    let mut previous: Option<BlockForm> = None;
    let mut current: Option<(usize, BlockForm)> = None;
    // A `<T, U>` binder list prefixes the compact type after it, so the pair is one item.
    let mut awaiting_bound_type = false;
    for child in node.children() {
        let line = line_of(child.text_range().start());
        if current.is_some_and(|(at, _)| at != line) {
            current = None;
            awaiting_bound_type = false;
        }
        if awaiting_bound_type && is_type_kind(child.kind()) {
            awaiting_bound_type = false;
            continue;
        }
        let form = match child.kind() {
            SyntaxKind::ANNOTATION_DIRECTIVE => match directive_name(&child).as_str() {
                // A flag on the function, not a type, so it fits beside any form.
                "pure" => continue,
                "type" | "alias" => BlockForm::Definition,
                "param" | "return" | "returns" | "forall" => BlockForm::Expanded,
                _ => BlockForm::Compact,
            },
            SyntaxKind::TYPE_BINDER_LIST => {
                awaiting_bound_type = true;
                BlockForm::Compact
            }
            kind if is_type_kind(kind) => BlockForm::Compact,
            _ => continue,
        };
        if current.is_some() {
            return Some(BlockRefusal::Unparsed);
        }
        current = Some((line, form));
        let mixing = match (previous, form) {
            (None, _)
            | (Some(BlockForm::Definition), BlockForm::Definition)
            | (Some(BlockForm::Expanded), BlockForm::Expanded) => None,
            (Some(BlockForm::Compact), BlockForm::Compact) => Some(
                "only one compact annotation fits in a `#:` block. Separate the annotations with a blank line so each gets its own block.",
            ),
            (Some(BlockForm::Definition), _) | (_, BlockForm::Definition) => Some(
                "`@type` and `@alias` declarations need their own `#:` block. Separate them from other annotations with a blank line.",
            ),
            (Some(BlockForm::Compact), BlockForm::Expanded)
            | (Some(BlockForm::Expanded), BlockForm::Compact) => Some(
                "a `#:` block uses either one compact annotation or `@param`/`@return` lines, not both. Separate the two forms with a blank line.",
            ),
        };
        if let Some(message) = mixing {
            return Some(BlockRefusal::FormMixing(message));
        }
        previous = Some(form);
    }
    None
}

pub fn is_type_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::TYPE_REF
            | SyntaxKind::TYPE_APPLY
            | SyntaxKind::TYPE_VECTOR
            | SyntaxKind::TYPE_UNION
            | SyntaxKind::TYPE_FUNCTION
            | SyntaxKind::TYPE_RECORD
            | SyntaxKind::TYPE_TUPLE
            | SyntaxKind::TYPE_LIST
            | SyntaxKind::TYPE_PAREN
            | SyntaxKind::TYPE_BINDER_LIST
            | SyntaxKind::TYPE_TAG
            | SyntaxKind::TYPE_OTHER_TAGS
    )
}

/// `Record{...}` and `Tuple{...}` share the brace grammar of `list{...}` too.
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

/// Joins adjacent tokens after `@`: `@if-unknown` lexes as `if` `-` `unknown`.
/// Whether a `#:` block carries `@pure`. Read apart from the lowered annotation, so a block
/// refused for its type still declares purity and the fact stays a per-item `bool`.
pub fn declares_pure(annotation: &SyntaxNode) -> bool {
    annotation.children().any(|child| {
        child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE && directive_name(&child) == "pure"
    })
}

fn directive_name(node: &SyntaxNode) -> String {
    let mut name = String::new();
    let mut cursor: Option<TextSize> = None;
    for element in node.children_with_tokens() {
        let Some(token) = element.into_token() else {
            break;
        };
        match token.kind() {
            SyntaxKind::AT => {
                cursor = Some(token.text_range().end());
                continue;
            }
            SyntaxKind::IDENT | SyntaxKind::MINUS => {}
            kind if kind.is_keyword() => {}
            _ => break,
        }
        if cursor != Some(token.text_range().start()) {
            break;
        }
        name.push_str(token.text());
        cursor = Some(token.text_range().end());
    }
    name
}

fn name_text(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|c| c.kind() == SyntaxKind::NAME)
        .and_then(ast::Name::cast)
        .and_then(|name| name.text())
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.kind() == kind)
}

fn has_word(node: &SyntaxNode, word: &str) -> bool {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.kind() == SyntaxKind::IDENT && token.text() == word)
}

// Past the check limit a type is refused only when type checking is on; past the parse limit the
// annotation shape itself is refused.
const CHECK_DEPTH_LIMIT: usize = 128;
const PARSE_DEPTH_LIMIT: usize = 160;

struct Lowering<'db> {
    db: &'db dyn Db,
    binders: Vec<Name<'db>>,
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
    fn new(db: &'db dyn Db) -> Lowering<'db> {
        Lowering {
            db,
            binders: Vec::new(),
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
                "This type annotation is nested too deeply (more than 160 levels).".to_owned(),
                range,
            ));
        } else if self.beyond_check_depth {
            annotation.typing_errors.push((
                "This type is nested too deeply to check (more than 128 levels).".to_owned(),
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
    fn lower_binders(
        &mut self,
        list: &SyntaxNode,
        bound: &mut Vec<(Name<'db>, Constraint)>,
        place: &str,
    ) {
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
                                "unknown type-parameter constraint `{spelling}`; the available constraints are {available}."
                            ),
                            first.text_range().cover(last.text_range()),
                        ));
                        Constraint::Unconstrained
                    })
                }
                _ => Constraint::Unconstrained,
            };
            if bound.iter().any(|(existing, _)| *existing == name) {
                self.errors.push((
                    format!(
                        "duplicate type parameter name `{}` in {place}.",
                        name.text(self.db)
                    ),
                    binder.text_range(),
                ));
            }
            self.binders.push(name);
            bound.push((name, constraint));
        }
    }

    fn lower_named_definition(
        &mut self,
        node: &SyntaxNode,
        alias: bool,
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
            alias,
            inline: node.kind() == SyntaxKind::TYPE_DEFINITION,
            name: Name::new(self.db, name),
            parameters: parameters.into_iter().map(|(name, _)| name).collect(),
            body,
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
                if let Some(body) = self.lower_child_type(node) {
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
                        annotation.new_nominal = Some((*name, arguments.clone(), range));
                    }
                    _ => self.errors.push((
                        "expected a nominal type reference after `@new`.".to_owned(),
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
            SyntaxKind::TYPE_TAG => {
                let name = Name::new(self.db, name_text(node).unwrap_or_default());
                let payload = match node
                    .children()
                    .find(|c| c.kind() == SyntaxKind::TYPE_TAG_PAYLOAD)
                {
                    Some(payload) => {
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
                let positional = node.children().any(|c| is_type_kind(c.kind()));
                match native_head(node) {
                    Some(NativeHead::Record) if positional => {
                        self.errors.push((
                            "a `Record{...}` type names every field: write each one as `name: TYPE`, or use `Tuple{...}` for unnamed items".to_owned(),
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
                        self.lower_native_fields(node, "this `Record{...}` type")
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
                if node.children().any(|c| c.kind() == SyntaxKind::TYPE_FIELD)
                    && node.children().any(|c| is_type_kind(c.kind())) =>
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
                if node.children().any(|c| is_type_kind(c.kind())) {
                    self.errors.push((
                        format!(
                            "a `{}{{...}}` type names every column: write each one as `name: TYPE`",
                            kind.head()
                        ),
                        node.text_range(),
                    ));
                    return unknown(self.db);
                }
                Ty::new(self.db, TyKind::Frame(kind, Some(Vec::new())))
            }
            SyntaxKind::TYPE_TUPLE => {
                let items = node
                    .children()
                    .filter(|c| is_type_kind(c.kind()))
                    .map(|item| self.lower_type(&item))
                    .collect();
                Ty::new(self.db, TyKind::Tuple(items))
            }
            SyntaxKind::TYPE_RECORD => {
                let frame = frame_kind(node);
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
                    .collect();
                Ty::new(
                    self.db,
                    if let Some(kind) = frame {
                        TyKind::Frame(kind, Some(fields))
                    } else {
                        TyKind::Record(fields)
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
                        format!("`{text}` is a type parameter here and takes no type arguments."),
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
                // An Array is one-dimensional and NA-free, so a vector, list, `NULL` or function
                // item would make it ragged or missing.
                if text == "Array"
                    && let [item] = arguments.as_slice()
                    && !array_item_annotation(self.db, *item)
                    && let Some(argument) = node
                        .children()
                        .find(|c| c.kind() == SyntaxKind::TYPE_ARG_LIST)
                        .and_then(|list| list.children().find(|c| is_type_kind(c.kind())))
                {
                    self.errors.push((
                        format!(
                            "an Array cannot hold `{}`. Its items are single `logical`, `integer`, `double` or `character` values, or ry values such as records, tuples and tags",
                            argument.text().to_string().trim()
                        ),
                        argument.text_range(),
                    ));
                }
                if let Some(range) = reference {
                    self.applied_references.push((text, arguments.len(), range));
                }
                Ty::new(self.db, TyKind::Named(name, arguments))
            }
            SyntaxKind::TYPE_FUNCTION => self.lower_function_type(node),
            _ => unknown(self.db),
        }
    }

    fn lower_native_items(&mut self, node: &SyntaxNode) -> Ty<'db> {
        let items = node
            .children()
            .filter(|c| is_type_kind(c.kind()))
            .map(|item| self.lower_type(&item))
            .collect();
        Ty::new(self.db, TyKind::NativeTuple(items))
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
            "DataFrame" => return Ty::new(self.db, TyKind::Frame(FrameKind::DataFrame, None)),
            "LazyFrame" => return Ty::new(self.db, TyKind::Frame(FrameKind::LazyFrame, None)),
            "GroupBy" => {
                return Ty::new(
                    self.db,
                    TyKind::Frame(
                        FrameKind::GroupBy {
                            lazy: false,
                            keys: 0,
                        },
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
        Ty::new(self.db, TyKind::Named(name, Vec::new()))
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
            TyKind::Function(FunctionType {
                positional,
                named,
                variadic,
                ret,
            }),
        )
    }
}

/// Whether an annotation may name `item` as an Array's item type. A type name, a type parameter or
/// a union of tags may; an R vector, list, `NULL`, function, `complex` or `raw` may not.
fn array_item_annotation(db: &dyn Db, item: Ty<'_>) -> bool {
    match item.kind(db) {
        TyKind::Scalar(atomic) => !matches!(atomic, Atomic::Complex | Atomic::Raw),
        TyKind::Union(members) => members.iter().all(|&member| {
            !matches!(member.kind(db), TyKind::Null) && array_item_annotation(db, member)
        }),
        TyKind::Vector(_)
        | TyKind::NamedVector(_)
        | TyKind::List(_)
        | TyKind::NamedList(_)
        | TyKind::Tuple(_)
        | TyKind::Record(_)
        | TyKind::Frame(..)
        | TyKind::Function(_)
        | TyKind::Null => false,
        _ => true,
    }
}
