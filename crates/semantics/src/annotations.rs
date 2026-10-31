//! Lowering is total: malformed pieces lower to `Unknown` (their syntax errors already mark
//! them), so annotations never cascade.

use crate::Db;
use crate::types::{
    Atomic, Constraint, FunctionType, Name, Parameter, RecordField, RestParameter, Ty, TyKind,
    TypeScheme, any, null, scalar, union_of, unknown,
};
use rustc_hash::FxHashSet;
use std::mem::take;
use syntax::{SyntaxKind, SyntaxNode, TextRange, TextSize, ast};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Annotation<'db> {
    pub declared: Option<TypeScheme<'db>>,
    /// Named parameter types, for name-aware matching against the definition's formals.
    pub parameter_names: Vec<(String, Ty<'db>, bool)>,
    pub definitions: Vec<NamedDefinition<'db>>,
    /// Parallel to `definitions`; kept out of `NamedDefinition` so interface maps built from
    /// definitions stay position-independent.
    pub definition_sites: Vec<TextRange>,
    /// `@new Name<ARGS>`, with the range of the referenced type.
    pub new_nominal: Option<(Name<'db>, Vec<Ty<'db>>, TextRange)>,
    pub strict: Option<bool>,
    /// `@trust TYPE`, where the declared type applies unchecked.
    pub trusted: bool,
    /// `@if-unknown TYPE`: applies only where the checker has nothing better than `Unknown`.
    pub if_unknown: bool,
    /// Shape violations. A block with any carries no typing payload, so it never cascades.
    pub errors: Vec<(String, TextRange)>,
    /// Violations reported only when type checking is on; like `errors`, they strip the payload.
    pub typing_errors: Vec<(String, TextRange)>,
    /// Judged by the diagnostics, not here: whether a named element resolves to an atomic alias
    /// needs the project vocabulary.
    pub vector_elements: Vec<(Ty<'db>, TextRange)>,
    /// `(name, argument count, range)`; the arity check against the vocabulary lives with the
    /// diagnostics.
    pub applied_references: Vec<(String, usize, TextRange)>,
    /// Excludes primitives and in-scope binders, so each entry must resolve through project
    /// declarations or the stubs.
    pub nominal_references: Vec<(String, TextRange)>,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct NamedDefinition<'db> {
    pub alias: bool,
    pub name: Name<'db>,
    /// Type parameters; the body references them as rigid variables.
    pub parameters: Vec<Name<'db>>,
    pub body: Ty<'db>,
}

pub fn lower_annotation<'db>(db: &'db dyn Db, node: &SyntaxNode) -> Annotation<'db> {
    debug_assert_eq!(node.kind(), SyntaxKind::ANNOTATION);
    let mut lowering = Lowering {
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
    };
    let mut annotation = Annotation {
        range: node.text_range(),
        ..Annotation::default()
    };
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
                    "trust" | "if-unknown" => {
                        if directive == "trust" {
                            annotation.trusted = true;
                        } else {
                            annotation.if_unknown = true;
                        }
                        if let Some(body) = lowering.lower_child_type(&child) {
                            annotation.declared = Some(TypeScheme::monomorphic(body));
                        }
                    }
                    "new" => {
                        let ty_node = child.children().find(|c| is_type_kind(c.kind()));
                        let range = ty_node
                            .as_ref()
                            .map_or(child.text_range(), |ty| ty.text_range());
                        let lowered = ty_node.map(|ty| lowering.lower_type(&ty));
                        match lowered.map(|ty| ty.kind(db)) {
                            Some(TyKind::Named(name, arguments)) => {
                                annotation.new_nominal = Some((*name, arguments.clone(), range));
                            }
                            _ => lowering.errors.push((
                                "expected a nominal type reference after `@new`.".to_owned(),
                                range,
                            )),
                        }
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
                if let TyKind::Function(function) = body.kind(db) {
                    annotation.parameter_names = function
                        .named
                        .iter()
                        .map(|field| (field.name.text(db).to_owned(), field.ty, field.optional))
                        .collect();
                }
                annotation.declared = Some(TypeScheme {
                    binders: take(&mut forall),
                    body,
                });
            }
            _ => {}
        }
    }

    if lowering.beyond_parse_depth {
        lowering.errors.push((
            "This type annotation is nested too deeply (more than 160 levels).".to_owned(),
            node.text_range(),
        ));
    } else if lowering.beyond_check_depth {
        annotation.typing_errors.push((
            "This type is nested too deeply to check (more than 128 levels).".to_owned(),
            node.text_range(),
        ));
    }
    // Declared type names stay declared, with `Unknown` bodies, or every use of one would add an
    // unknown-type finding to the one already made.
    if !lowering.errors.is_empty() || !annotation.typing_errors.is_empty() {
        for definition in &mut annotation.definitions {
            definition.body = unknown(db);
        }
        return Annotation {
            errors: lowering.errors,
            typing_errors: annotation.typing_errors,
            definitions: annotation.definitions,
            definition_sites: annotation.definition_sites,
            range: annotation.range,
            ..Annotation::default()
        };
    }

    annotation.nominal_references = lowering.nominal_references;
    annotation.applied_references = lowering.applied_references;
    annotation.vector_elements = lowering.vector_elements;

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
        annotation.parameter_names = params;
        annotation.declared = Some(TypeScheme {
            binders: forall,
            body: Ty::new(db, TyKind::Function(function)),
        });
    }

    annotation
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
    )
}

/// Joins adjacent tokens after `@`: `@if-unknown` lexes as `if` `-` `unknown`.
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
            name: Name::new(self.db, name),
            parameters: parameters.into_iter().map(|(name, _)| name).collect(),
            body,
        })
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
                union_of(self.db, members)
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
                    "a `list{...}` type cannot mix named and unnamed items: write every item as `name: TYPE`, or none of them".to_owned(),
                    node.text_range(),
                ));
                unknown(self.db)
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
                let fields: Vec<(String, SyntaxNode)> = node
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::TYPE_FIELD)
                    .filter_map(|field| Some((name_text(&field)?, field)))
                    .collect();
                let mut seen = FxHashSet::default();
                for (name, field) in &fields {
                    if !seen.insert(name.as_str()) {
                        self.errors.push((
                            format!("the field `{name}` appears twice in this `list{{...}}` type"),
                            field.text_range(),
                        ));
                    }
                }
                let fields = fields
                    .into_iter()
                    .map(|(name, field)| RecordField {
                        name: Name::new(self.db, name),
                        ty: self
                            .lower_child_type(&field)
                            .unwrap_or_else(|| unknown(self.db)),
                        optional: false,
                    })
                    .collect();
                Ty::new(self.db, TyKind::Record(fields))
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
                if let Some(range) = reference {
                    self.applied_references.push((text, arguments.len(), range));
                }
                Ty::new(self.db, TyKind::Named(name, arguments))
            }
            SyntaxKind::TYPE_FUNCTION => self.lower_function_type(node),
            _ => unknown(self.db),
        }
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
            _ => {}
        }
        let name = Name::new(self.db, text.to_owned());
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
