//! Lowering `#:` annotation syntax onto interned types.
//!
//! The parser already produced first-class `TYPE_*` nodes with real spans;
//! this module turns them into `Ty`/`TypeScheme` values. Explicit `<T>`
//! binders lower to rigid (skolem) types that refuse to bind while the
//! annotated definition's body is checked and generalize back out afterwards.
//! Lowering is total: malformed pieces lower to `Unknown` (their syntax errors
//! already mark them), so annotations never cascade.

use crate::Db;
use crate::types::{
    Atomic, Constraint, FunctionType, Name, Parameter, RecordField, RestParameter, Ty, TyKind,
    TypeScheme, any, null, scalar, union_of, unknown,
};
use syntax::ast::AstNode as _;
use syntax::{SyntaxKind, SyntaxNode, TextRange};

/// One lowered annotation region.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Annotation<'db> {
    /// The declared type of the annotated definition: a compact type, or the
    /// assembly of `@forall` / `@param` / `@return` directives.
    pub declared: Option<TypeScheme<'db>>,
    /// Named parameter types by name (from `fn(name: T)` or `@param`), for
    /// name-aware matching against the definition's formals.
    pub parameter_names: Vec<(String, Ty<'db>, bool)>,
    /// `@type` / `@alias` definitions carried by this annotation.
    pub definitions: Vec<NamedDefinition<'db>>,
    /// Each definition's declaring directive range, parallel to
    /// `definitions`. It is kept out of `NamedDefinition` so the interface maps
    /// built from definitions stay position-independent.
    pub definition_sites: Vec<TextRange>,
    /// `@new Name` or `@new Name<ARGS>`. The annotated value checks against
    /// the nominal's representation, and the binding takes the nominal type.
    /// The range is the referenced type's, for diagnostics about the name.
    pub new_nominal: Option<(Name<'db>, Vec<Ty<'db>>, TextRange)>,
    /// A `#: @strict` / `#: @strict off` toggle.
    pub strict: Option<bool>,
    /// `@trust TYPE`, where the declared type applies unchecked.
    pub trusted: bool,
    /// `@if-unknown TYPE`, where the declared type applies only if the checker
    /// has nothing better than `Unknown`. Using it on a value whose type is
    /// already known is an error, so a stale annotation cannot hide.
    pub if_unknown: bool,
    /// Annotation-shape violations: form mixing, directive ordering,
    /// duplicate or unknown type parameters, a non-nominal `@new` payload,
    /// nesting beyond the parse cap. A violating block carries no typing
    /// payload, and carries only its errors, so a broken annotation never
    /// cascades.
    pub errors: Vec<(String, TextRange)>,
    /// Typing-gated violations (reported only when type checking is on):
    /// today the check-depth cap. Like `errors`, these strip the payload.
    pub typing_errors: Vec<(String, TextRange)>,
    /// Every `[]` / `[named]` vector type lowered, as (element type, vector
    /// node range): vectors hold atomic elements only, but whether a named
    /// element resolves to an atomic alias needs the project vocabulary, so
    /// the judgment lives with the diagnostics, not the lowering.
    pub vector_elements: Vec<(Ty<'db>, TextRange)>,
    /// Generic applications (`Name<ARGS>`) the block lowers, as (name,
    /// argument count, name-token range): the vocabulary-side arity check
    /// lives with the diagnostics, like the vector-element rule.
    pub applied_references: Vec<(String, usize, TextRange)>,
    /// Every named-type reference the block lowers (`TyKind::Named` mints),
    /// with the referencing token's range: primitives and in-scope binders
    /// are excluded here already, so an entry is exactly a name that must
    /// resolve through the project's `@type`/`@alias` declarations or the
    /// stub corpus's nominal vocabulary.
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

/// Lower one `ANNOTATION` node.
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
    // A refused block carries no typing payload, so a broken annotation never
    // produces follow-on findings.
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

    // Expanded-form accumulation.
    let mut forall: Vec<(Name<'db>, Constraint)> = Vec::new();
    let mut params: Vec<(String, Ty<'db>, bool)> = Vec::new();
    let mut rest: Option<Ty<'db>> = None;
    let mut ret: Option<Ty<'db>> = None;
    let mut saw_expanded = false;
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
                        saw_expanded = true;
                        if saw_param || saw_return {
                            lowering.errors.push((
                                "`@forall` directives must appear before `@param`, `@return`, or `@returns` in the same `#:` block.".to_owned(),
                                child.text_range(),
                            ));
                        }
                        for binder in child
                            .children()
                            .filter(|c| c.kind() == SyntaxKind::TYPE_BINDER)
                        {
                            if let Some((name, constraint)) = lowering.lower_binder(&binder) {
                                if forall.iter().any(|(existing, _)| *existing == name) {
                                    lowering.errors.push((
                                        format!(
                                            "duplicate type parameter name `{}` in expanded function annotation.",
                                            name.text(db)
                                        ),
                                        binder.text_range(),
                                    ));
                                }
                                lowering.binders.push(name);
                                forall.push((name, constraint));
                            }
                        }
                    }
                    "param" => {
                        saw_expanded = true;
                        if saw_return {
                            lowering.errors.push((
                                "`@param` directives must appear before `@return` or `@returns` in the same `#:` block.".to_owned(),
                                child.text_range(),
                            ));
                        }
                        saw_param = true;
                        let name = child
                            .children()
                            .find(|c| c.kind() == SyntaxKind::NAME)
                            .and_then(syntax::ast::Name::cast)
                            .and_then(|name| name.text());
                        let optional = child
                            .children_with_tokens()
                            .filter_map(|element| element.into_token())
                            .any(|token| token.kind() == SyntaxKind::L_BRACKET);
                        let ty = child
                            .children()
                            .find(|c| is_type_kind(c.kind()))
                            .map(|ty| lowering.lower_type(&ty))
                            .unwrap_or_else(|| unknown(db));
                        if let Some(name) = name {
                            if name == "..." {
                                rest = Some(ty);
                            } else {
                                params.push((name, ty, optional));
                            }
                        }
                    }
                    "return" | "returns" => {
                        saw_expanded = true;
                        if saw_return {
                            lowering.errors.push((
                                "cannot use more than one `@return` or `@returns` directive in the same `#:` block.".to_owned(),
                                child.text_range(),
                            ));
                        }
                        saw_return = true;
                        ret = child
                            .children()
                            .find(|c| is_type_kind(c.kind()))
                            .map(|ty| lowering.lower_type(&ty));
                    }
                    "strict" => {
                        let off = child
                            .children_with_tokens()
                            .filter_map(|element| element.into_token())
                            .any(|token| {
                                token.kind() == SyntaxKind::IDENT && token.text() == "off"
                            });
                        annotation.strict = Some(!off);
                    }
                    "trust" => {
                        annotation.trusted = true;
                        if let Some(ty) = child.children().find(|c| is_type_kind(c.kind())) {
                            lowering.definition_type = true;
                            let lowered = lowering.lower_type(&ty);
                            lowering.definition_type = false;
                            annotation.declared = Some(TypeScheme {
                                binders: Vec::new(),
                                body: lowered,
                            });
                        }
                    }
                    "new" => {
                        if let Some(ty_node) = child.children().find(|c| is_type_kind(c.kind())) {
                            let lowered = lowering.lower_type(&ty_node);
                            if let TyKind::Named(name, arguments) = lowered.kind(db) {
                                annotation.new_nominal =
                                    Some((*name, arguments.clone(), ty_node.text_range()));
                            } else {
                                lowering.errors.push((
                                    "expected a nominal type reference after `@new`.".to_owned(),
                                    ty_node.text_range(),
                                ));
                            }
                        } else {
                            lowering.errors.push((
                                "expected a nominal type reference after `@new`.".to_owned(),
                                child.text_range(),
                            ));
                        }
                    }
                    "if-unknown" => {
                        annotation.if_unknown = true;
                        if let Some(ty) = child.children().find(|c| is_type_kind(c.kind())) {
                            lowering.definition_type = true;
                            let lowered = lowering.lower_type(&ty);
                            lowering.definition_type = false;
                            annotation.declared = Some(TypeScheme {
                                binders: Vec::new(),
                                body: lowered,
                            });
                        }
                    }
                    // Unknown directives carry no typing payload at this layer.
                    _ => {}
                }
            }
            SyntaxKind::TYPE_BINDER_LIST => {
                for binder in child
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::TYPE_BINDER)
                {
                    if let Some((name, constraint)) = lowering.lower_binder(&binder) {
                        if forall.iter().any(|(existing, _)| *existing == name) {
                            lowering.errors.push((
                                format!(
                                    "duplicate type parameter name `{}` in `<...>`.",
                                    name.text(db)
                                ),
                                binder.text_range(),
                            ));
                        }
                        lowering.binders.push(name);
                        forall.push((name, constraint));
                    }
                }
            }
            kind if is_type_kind(kind) => {
                // Compact form: the annotation IS the declared type.
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
                    binders: forall.clone(),
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
    // A violating block keeps only its errors: applying half-understood
    // typing payload would cascade follow-on findings from one mistake.
    if !lowering.errors.is_empty() || !annotation.typing_errors.is_empty() {
        return Annotation {
            errors: lowering.errors,
            typing_errors: std::mem::take(&mut annotation.typing_errors),
            range: annotation.range,
            ..Annotation::default()
        };
    }

    // Assemble the expanded form into a function scheme when directives
    // declared one and no compact type did.
    annotation.nominal_references = std::mem::take(&mut lowering.nominal_references);
    annotation.applied_references = std::mem::take(&mut lowering.applied_references);
    annotation.vector_elements = std::mem::take(&mut lowering.vector_elements);

    if saw_expanded && annotation.declared.is_none() && (!params.is_empty() || ret.is_some()) {
        // An annotation states the parameter's type, never its default: the
        // default lives in the definition, which this may not have in view.
        let named: Vec<Parameter<'db>> = params
            .iter()
            .map(|(name, ty, optional)| Parameter {
                name: Name::new(db, name.clone()),
                ty: *ty,
                optional: *optional,
                default: None,
            })
            .collect();
        annotation.parameter_names = params.clone();
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

    annotation
}

/// The three annotation block forms. A block must commit to one: `@type` /
/// `@alias` definitions, the expanded directives (`@forall` / `@param` /
/// `@return`), or a single compact annotation (a type, optionally behind
/// `@trust` / `@new` / `@if-unknown`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockForm {
    Compact,
    Expanded,
    Definition,
}

/// Why a `#:` block carries no typing payload, from [`block_refusal`].
pub enum BlockRefusal {
    /// Two `#:` lines in the block commit to different forms; the wording tells
    /// the reader which separation to make.
    FormMixing(&'static str),
    /// A line did not parse as the form it committed to. Silent: the parser has
    /// already said what was wrong, and a second opinion from here would be
    /// guessing at a block nobody could read.
    Unparsed,
}

/// Checks the block's `#:` lines against the form-mixing rules.
///
/// **One `#:` line commits to one form, and only whole lines are compared.**
/// The rules are about lines, because every refusal here tells the reader to
/// separate something with a blank line. A line's form is the form of the first
/// item on it.
///
/// A line carrying a *second* form item did not parse as the form it committed
/// to: recovery re-parents the pieces of a type it could not read to the block,
/// so `#: fn(f: <T> fn(T) -> T) -> integer` leaves three top-level types
/// behind, and `#: @param count integer` leaves the unbraced type beside the
/// directive. Reading those as separate block items claimed "only one compact
/// annotation fits in a `#:` block" against a block holding exactly one, and
/// prescribed a blank line that would not have helped.
pub fn block_refusal(node: &SyntaxNode) -> Option<BlockRefusal> {
    // The parser wraps a region it refused in `ERROR`, so the block has no
    // trustworthy payload. A refused higher-rank binder, for instance, leaves
    // its type parameters with nothing to bind them, and reporting each as an
    // unknown type would blame the author twice for one mistake.
    if node
        .descendants()
        .any(|descendant| descendant.kind() == SyntaxKind::ERROR)
    {
        return Some(BlockRefusal::Unparsed);
    }
    // Each `#:` keeps its own marker token, so the markers are the line
    // boundaries. They are collected from every token in the block, not just
    // its direct children: a stitched line's marker lands inside whatever node
    // was still open across the break, so `#: fn(x: integer) -> integer` /
    // `#: @return {integer}` keeps its second marker inside the function type.
    let markers: Vec<syntax::TextSize> = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::ANNOTATION_MARKER)
        .map(|token| token.text_range().start())
        .collect();
    let line_of = |start: syntax::TextSize| markers.iter().filter(|&&at| at < start).count();

    let mut previous: Option<BlockForm> = None;
    let mut current: Option<(usize, BlockForm)> = None;
    // A `<T, U>` binder list is a prefix of the compact type that follows it,
    // not an item of its own, so the pair is one item on its line.
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

/// The joined directive name: adjacent IDENT/keyword/`-` tokens right after
/// the `@` (`@if-unknown` lexes as `if` `-` `unknown`).
fn directive_name(node: &SyntaxNode) -> String {
    let mut name = String::new();
    let mut cursor: Option<rowan::TextSize> = None;
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

/// The recursion ceiling past which a declared type is refused for checking,
/// and the deeper ceiling past which the annotation shape itself is refused.
/// The split mirrors how the two refusals gate: the shape refusal always
/// reports, the check refusal only when type checking is on.
const CHECK_DEPTH_LIMIT: usize = 128;
const PARSE_DEPTH_LIMIT: usize = 160;

struct Lowering<'db> {
    db: &'db dyn Db,
    /// In-scope rigid binder names.
    binders: Vec<Name<'db>>,
    /// Named-type references minted so far, with the referencing token's
    /// range (see [`Annotation::nominal_references`]).
    nominal_references: Vec<(String, TextRange)>,
    /// Generic applications (see [`Annotation::applied_references`]).
    applied_references: Vec<(String, usize, TextRange)>,
    /// Shape violations found while lowering (see [`Annotation::errors`]).
    errors: Vec<(String, TextRange)>,
    /// Vector element types with their vector node's range (see
    /// [`Annotation::vector_elements`]).
    vector_elements: Vec<(Ty<'db>, TextRange)>,
    depth: usize,
    beyond_check_depth: bool,
    beyond_parse_depth: bool,
    /// Lowering the annotated definition's own declared type (a compact or
    /// `@trust` annotation): only there an elided `->` on the OUTERMOST
    /// function type means "inferred from the body". Everywhere else an elided
    /// return means `NULL`, which is R's default for a function that returns
    /// nothing declared. A nested function type, a `@param` or `@return`
    /// payload, and a `@type` or `@alias` body are all such places.
    definition_type: bool,
}

impl<'db> Lowering<'db> {
    fn lower_binder(&mut self, node: &SyntaxNode) -> Option<(Name<'db>, Constraint)> {
        let mut names = node.children().filter(|c| c.kind() == SyntaxKind::NAME);
        let binder = names.next()?;
        let binder_name = syntax::ast::Name::cast(binder)?.text()?;
        // A constraint may be spelled in several words (`scalar numeric`), so
        // the whole remaining run of names is one spelling.
        let constraint_nodes: Vec<SyntaxNode> = names.collect();
        let constraint = match constraint_nodes.split_first() {
            Some((first, rest)) => {
                let spelling: Vec<String> = constraint_nodes
                    .iter()
                    .filter_map(|node| syntax::ast::Name::cast(node.clone()))
                    .filter_map(|name| name.text())
                    .collect();
                let spelling = spelling.join(" ");
                match Constraint::from_spelling(&spelling) {
                    Some(constraint) => constraint,
                    None => {
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
                        // The whole spelling is the mistake, so blame all of it
                        // rather than only the word the reader happened to
                        // write first.
                        let blamed = rest.last().map_or(first.text_range(), |last| {
                            first.text_range().cover(last.text_range())
                        });
                        self.errors.push((
                            format!(
                                "unknown type-parameter constraint `{spelling}`; the available constraints are {available}."
                            ),
                            blamed,
                        ));
                        Constraint::Unconstrained
                    }
                }
            }
            None => Constraint::Unconstrained,
        };
        Some((Name::new(self.db, binder_name), constraint))
    }

    fn lower_named_definition(
        &mut self,
        node: &SyntaxNode,
        alias: bool,
    ) -> Option<NamedDefinition<'db>> {
        let name = node
            .children()
            .find(|c| c.kind() == SyntaxKind::NAME)
            .and_then(syntax::ast::Name::cast)
            .and_then(|name| name.text())?;
        let saved = self.binders.len();
        let mut parameters = Vec::new();
        if let Some(list) = node
            .children()
            .find(|c| c.kind() == SyntaxKind::TYPE_BINDER_LIST)
        {
            for binder in list
                .children()
                .filter(|c| c.kind() == SyntaxKind::TYPE_BINDER)
            {
                if let Some((binder_name, _)) = self.lower_binder(&binder) {
                    if parameters.contains(&binder_name) {
                        self.errors.push((
                            format!(
                                "duplicate type parameter name `{}` in named type definition.",
                                binder_name.text(self.db)
                            ),
                            binder.text_range(),
                        ));
                    }
                    self.binders.push(binder_name);
                    parameters.push(binder_name);
                }
            }
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
            parameters,
            body,
        })
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
            SyntaxKind::TYPE_PAREN => node
                .children()
                .find(|c| is_type_kind(c.kind()))
                .map(|inner| self.lower_type(&inner))
                .unwrap_or_else(|| unknown(self.db)),
            SyntaxKind::TYPE_UNION => {
                let members: Vec<Ty<'db>> = node
                    .children()
                    .filter(|c| is_type_kind(c.kind()))
                    .map(|member| self.lower_type(&member))
                    .collect();
                union_of(self.db, members)
            }
            SyntaxKind::TYPE_VECTOR => {
                let element = node
                    .children()
                    .find(|c| is_type_kind(c.kind()))
                    .map(|inner| self.lower_type(&inner))
                    .unwrap_or_else(|| unknown(self.db));
                self.vector_elements.push((element, node.text_range()));
                let named = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::IDENT && token.text() == "named");
                if named {
                    Ty::new(self.db, TyKind::NamedVector(element))
                } else {
                    Ty::new(self.db, TyKind::Vector(element))
                }
            }
            SyntaxKind::TYPE_LIST => {
                let element = node
                    .children()
                    .find(|c| is_type_kind(c.kind()))
                    .map(|inner| self.lower_type(&inner))
                    .unwrap_or_else(|| unknown(self.db));
                let named = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::IDENT && token.text() == "named");
                if named {
                    Ty::new(self.db, TyKind::NamedList(element))
                } else {
                    Ty::new(self.db, TyKind::List(element))
                }
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
                let fields = node
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::TYPE_FIELD)
                    .filter_map(|field| {
                        let name = field
                            .children()
                            .find(|c| c.kind() == SyntaxKind::NAME)
                            .and_then(syntax::ast::Name::cast)
                            .and_then(|name| name.text())?;
                        let ty = field
                            .children()
                            .find(|c| is_type_kind(c.kind()))
                            .map(|ty| self.lower_type(&ty))
                            .unwrap_or_else(|| unknown(self.db));
                        Some(RecordField {
                            name: Name::new(self.db, name),
                            ty,
                            optional: false,
                        })
                    })
                    .collect();
                Ty::new(self.db, TyKind::Record(fields))
            }
            SyntaxKind::TYPE_APPLY => {
                let name_node = node.children().find(|c| c.kind() == SyntaxKind::NAME);
                let name = name_node
                    .clone()
                    .and_then(syntax::ast::Name::cast)
                    .and_then(|name| name.text())
                    .unwrap_or_default();
                // A type parameter is not a generic: applying arguments to
                // it is a semantic error, not a reference to resolve.
                if !name.is_empty() && self.binders.contains(&Name::new(self.db, name.clone())) {
                    self.errors.push((
                        format!("`{name}` is a type parameter here and takes no type arguments."),
                        node.text_range(),
                    ));
                    return unknown(self.db);
                }
                if let Some(name_node) = &name_node
                    && !name.is_empty()
                {
                    self.nominal_references
                        .push((name.clone(), name_node.text_range()));
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
                if let Some(name_node) = &name_node
                    && !name.is_empty()
                {
                    self.applied_references.push((
                        name.clone(),
                        arguments.len(),
                        name_node.text_range(),
                    ));
                }
                Ty::new(self.db, TyKind::Named(Name::new(self.db, name), arguments))
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
                let has_dots = parameter
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::DOTS);
                let optional = parameter
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::L_BRACKET);
                let name = parameter
                    .children()
                    .find(|c| c.kind() == SyntaxKind::NAME)
                    .and_then(syntax::ast::Name::cast)
                    .and_then(|name| name.text());
                let ty = parameter
                    .children()
                    .find(|c| is_type_kind(c.kind()))
                    .map(|ty| self.lower_type(&ty));
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
            .filter(|_| {
                node.children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::MINUS_GREATER)
            })
            .map(|ty| self.lower_type(&ty))
            .unwrap_or_else(|| {
                // The definition's own outermost function type elides to
                // "inferred from the body"; any other elided return means
                // `NULL` (see the `definition_type` field).
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
