//! What a tree's annotations say before any type is resolved: which statement a `#:` block
//! attaches to, whether a block commits to one form, and which written type governs each
//! parameter and the result of a function literal.
//!
//! The type checker and the runtime both read a function's signature through this module, so a
//! signature written in a `#:` comment, on the name the function is assigned to, or inline on the
//! literal means the same to both. The checker resolves the governing types; the runtime, which
//! also lowers code no checker has seen, takes their storage checks and their text.

use crate::{
    NodeOrToken, SyntaxKind, SyntaxNode, TextRange, TextSize,
    ast::{self, is_expression_kind},
};

/// The written type that governs each formal of a function literal, by formal position, and the
/// one that governs its result. `None` is a slot nothing types.
pub struct Signature {
    pub formals: Vec<Option<SyntaxNode>>,
    pub ret: Option<SyntaxNode>,
}

impl Signature {
    pub fn is_untyped(&self) -> bool {
        self.ret.is_none() && self.formals.iter().all(Option::is_none)
    }

    /// Whether a type is written for every formal, or for the result of a function without
    /// formals. Only such a function has a signature a compiler can rely on.
    pub fn is_complete(&self) -> bool {
        self.formals.iter().all(Option::is_some) && (!self.formals.is_empty() || self.ret.is_some())
    }
}

/// The signature of the function literal `function`, from every place one can be written. A type
/// written on the literal governs its slot. A slot the literal leaves untyped takes the type the
/// statement declares for it: the binding's own function type in `name: fn(...) <- function(...)`,
/// which replaces a `#:` block as a whole, and otherwise the `#:` block attached to the statement
/// that is or assigns the literal. With `inline` off, only the `#:` block counts.
///
/// A coercion on the statement (`@trust`, `@if-unknown`, `@new`) governs the value wherever it
/// is written, and it says how the value gets its type instead of declaring one to check, so
/// such a function has no signature at all, whatever its literal writes.
pub fn signature(function: &SyntaxNode, inline: bool) -> Signature {
    let block = attached_annotation(&statement_of(function));
    signature_from(function, block.as_ref(), inline)
}

/// [`signature`] for a caller that holds the statement's `#:` block apart from the statement's
/// tree.
pub fn signature_from(
    function: &SyntaxNode,
    block: Option<&SyntaxNode>,
    inline: bool,
) -> Signature {
    let formals = formals(function);
    let statement = statement_of(function);
    let binding_type = statement
        .children()
        .find(|child| child.kind() == SyntaxKind::TYPE_ASCRIPTION)
        .filter(|_| inline && statement != *function);
    let coerced = binding_type.as_ref().is_some_and(has_coercion)
        || block.is_some_and(|block| block_refusal(block).is_none() && has_coercion(block));
    if coerced {
        return Signature {
            formals: formals.iter().map(|_| None).collect(),
            ret: None,
        };
    }
    let declaration = match binding_type {
        Some(ascription) => ascription
            .first_child()
            .and_then(|written| written_function_type(&written))
            .map(|function_type| compact_declaration(&function_type)),
        None => block.and_then(block_declaration),
    }
    .unwrap_or_default();

    let named: Vec<(&str, &Option<SyntaxNode>)> = declaration
        .parameters
        .iter()
        .filter_map(|parameter| Some((parameter.name.as_deref()?, &parameter.ty)))
        .collect();
    let positional: Vec<&Option<SyntaxNode>> = declaration
        .parameters
        .iter()
        .filter(|parameter| parameter.name.is_none() && !parameter.rest)
        .map(|parameter| &parameter.ty)
        .collect();
    let names: Vec<&str> = named.iter().map(|(name, _)| *name).collect();
    let paired = pair_formals(
        formals.iter().map(|formal| formal.name.as_str()),
        &names,
        positional.len(),
    );
    let governing = formals
        .iter()
        .zip(paired)
        .map(|(formal, paired)| {
            let written = formal
                .ascription
                .as_ref()
                .filter(|_| inline)
                .and_then(SyntaxNode::first_child);
            written.or_else(|| match paired {
                Paired::Named(index) => named.get(index).and_then(|(_, ty)| (*ty).clone()),
                Paired::Positional(index) => positional.get(index).and_then(|ty| (*ty).clone()),
                Paired::Rest => declaration
                    .parameters
                    .iter()
                    .rev()
                    .find(|parameter| parameter.rest)
                    .and_then(|parameter| parameter.ty.clone()),
                Paired::Undeclared => None,
            })
        })
        .collect();
    let ret = function
        .children()
        .find(|child| child.kind() == SyntaxKind::RETURN_TYPE)
        .filter(|_| inline)
        .as_ref()
        .and_then(SyntaxNode::first_child)
        .or(declaration.ret);
    Signature {
        formals: governing,
        ret,
    }
}

/// The statement a function literal's signature can be declared on: the assignment it is the
/// value of, or else the literal itself.
fn statement_of(function: &SyntaxNode) -> SyntaxNode {
    function
        .parent()
        .filter(|parent| assigned_function(parent).as_ref() == Some(function))
        .unwrap_or_else(|| function.clone())
}

/// The types a function type writes: `fn(x: I64, F64) -> Bool` as a compact type, or a block of
/// `@param` and `@return` lines.
#[derive(Default)]
pub struct Declaration {
    /// In source order.
    pub parameters: Vec<DeclaredParameter>,
    /// `None` when the result is left to inference.
    pub ret: Option<SyntaxNode>,
}

pub struct DeclaredParameter {
    /// `None` for a type written without a name, which goes to a formal by position, and for the
    /// rest parameter.
    pub name: Option<String>,
    /// The type of `...`.
    pub rest: bool,
    pub ty: Option<SyntaxNode>,
}

/// The types the compact function type `function_type` (a `TYPE_FUNCTION` node) writes.
pub fn compact_declaration(function_type: &SyntaxNode) -> Declaration {
    let written_type = |node: &SyntaxNode| node.children().find(|child| is_type_kind(child.kind()));
    let parameters = function_type
        .children()
        .filter(|child| child.kind() == SyntaxKind::TYPE_PARAMETER_LIST)
        .flat_map(|list| list.children())
        .filter(|child| child.kind() == SyntaxKind::TYPE_PARAMETER)
        .map(|parameter| {
            let rest = has_token(&parameter, SyntaxKind::DOTS);
            DeclaredParameter {
                name: name_text(&parameter).filter(|_| !rest),
                rest,
                ty: written_type(&parameter),
            }
        })
        .collect();
    let ret = has_token(function_type, SyntaxKind::MINUS_GREATER)
        .then(|| {
            function_type
                .children()
                .filter(|child| is_type_kind(child.kind()))
                .last()
        })
        .flatten();
    Declaration { parameters, ret }
}

/// The types the `@param` and `@return` lines of the `#:` block `annotation` write.
pub fn expanded_declaration(annotation: &SyntaxNode) -> Declaration {
    let mut declaration = Declaration::default();
    for directive in annotation
        .children()
        .filter(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
    {
        let ty = directive
            .children()
            .find(|child| is_type_kind(child.kind()));
        match directive_name(&directive).as_str() {
            "param" => {
                let name = name_text(&directive);
                let rest = name.as_deref() == Some("...");
                declaration.parameters.push(DeclaredParameter {
                    name: name.filter(|_| !rest),
                    rest,
                    ty,
                });
            }
            "return" | "returns" => declaration.ret = ty,
            _ => {}
        }
    }
    declaration
}

/// The function signature a `#:` block declares for a function literal to be checked against,
/// or `None` for a block that declares none: a refused block, a block whose directives are out
/// of order, a coercion, or a type that is not written as a function type.
pub fn block_declaration(annotation: &SyntaxNode) -> Option<Declaration> {
    if block_refusal(annotation).is_some()
        || has_coercion(annotation)
        || !misplaced_directives(annotation).is_empty()
    {
        return None;
    }
    let mut expanded = false;
    for child in annotation.children() {
        match child.kind() {
            SyntaxKind::ANNOTATION_DIRECTIVE => match directive_name(&child).as_str() {
                "param" | "return" | "returns" => expanded = true,
                _ => {}
            },
            SyntaxKind::TYPE_BINDER_LIST => {}
            kind if is_type_kind(kind) => {
                return written_function_type(&child)
                    .map(|function_type| compact_declaration(&function_type));
            }
            _ => {}
        }
    }
    expanded.then(|| expanded_declaration(annotation))
}

/// Whether a `#:` block or a binding's type holds a coercion directive (`@trust`, `@if-unknown`,
/// `@new`).
fn has_coercion(node: &SyntaxNode) -> bool {
    node.children().any(|child| {
        child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE
            && matches!(
                directive_name(&child).as_str(),
                "trust" | "if-unknown" | "new"
            )
    })
}

/// The directives of the `#:` block `annotation` that break the order an expanded function
/// annotation is written in (`@forall`, then every `@param`, then one `@return`), each with what
/// is wrong with it. A block with one declares nothing.
pub fn misplaced_directives(annotation: &SyntaxNode) -> Vec<(&'static str, TextRange)> {
    let mut misplaced = Vec::new();
    let mut saw_param = false;
    let mut saw_return = false;
    for directive in annotation
        .children()
        .filter(|child| child.kind() == SyntaxKind::ANNOTATION_DIRECTIVE)
    {
        match directive_name(&directive).as_str() {
            "forall" if saw_param || saw_return => misplaced.push((
                "`@forall` directives must appear before `@param`, `@return`, or `@returns` in the same `#:` block.",
                directive.text_range(),
            )),
            "param" => {
                if saw_return {
                    misplaced.push((
                        "`@param` directives must appear before `@return` or `@returns` in the same `#:` block.",
                        directive.text_range(),
                    ));
                }
                saw_param = true;
            }
            "return" | "returns" => {
                if saw_return {
                    misplaced.push((
                        "cannot use more than one `@return` or `@returns` directive in the same `#:` block.",
                        directive.text_range(),
                    ));
                }
                saw_return = true;
            }
            _ => {}
        }
    }
    misplaced
}

/// The function type `written` spells out, looking through parentheses. A type alias names a
/// function type without writing it, so it gives none.
pub fn written_function_type(written: &SyntaxNode) -> Option<SyntaxNode> {
    match written.kind() {
        SyntaxKind::TYPE_FUNCTION => Some(written.clone()),
        SyntaxKind::TYPE_PAREN => written_function_type(&written.first_child()?),
        _ => None,
    }
}

/// What a formal of a function literal takes from a declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paired {
    /// The declared parameter of the formal's name, by its index among the named ones.
    Named(usize),
    /// A type declared without a name, by its index among those.
    Positional(usize),
    /// The formal is `...`, which takes the declared rest type.
    Rest,
    /// The declaration says nothing about the formal.
    Undeclared,
}

/// Pairs the formals of a function literal with a declaration's parameters as R's matcher pairs
/// arguments: a formal takes the declared parameter of its own name, and otherwise the next type
/// declared without a name. `named` holds the names of the declared named parameters and
/// `positional` counts the types declared without one. This is the one statement of the rule,
/// for the checker, which pairs resolved types, and for the runtime, which pairs written ones.
pub fn pair_formals<'a>(
    formals: impl IntoIterator<Item = &'a str>,
    named: &[&str],
    positional: usize,
) -> Vec<Paired> {
    let mut next_positional = 0;
    formals
        .into_iter()
        .map(|formal| {
            if formal == "..." {
                Paired::Rest
            } else if let Some(index) = named.iter().position(|name| *name == formal) {
                Paired::Named(index)
            } else if next_positional < positional {
                next_positional += 1;
                Paired::Positional(next_positional - 1)
            } else {
                Paired::Undeclared
            }
        })
        .collect()
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

/// An annotation applies only to the expression starting on the very next line;
/// a blank line, a plain comment, or the end of the sequence breaks the association.
pub enum AnnotationTarget {
    Attached(SyntaxNode),
    BlankLineSeparated,
    Dangling,
}

/// The single source of the annotation association rule: what the `#:` block `annotation`
/// applies to, read from the elements that follow it in its sequence.
pub fn annotation_target(annotation: &SyntaxNode) -> AnnotationTarget {
    let mut newlines = 0;
    let mut next = annotation.next_sibling_or_token();
    while let Some(element) = next {
        match element.kind() {
            SyntaxKind::WHITESPACE => {}
            SyntaxKind::NEWLINE => newlines += 1,
            kind => {
                return match element {
                    NodeOrToken::Token(_) if kind != SyntaxKind::COMMENT => {
                        AnnotationTarget::Dangling
                    }
                    _ if newlines >= 2 => AnnotationTarget::BlankLineSeparated,
                    NodeOrToken::Node(node)
                        if is_expression_kind(kind) || kind == SyntaxKind::ERROR =>
                    {
                        AnnotationTarget::Attached(node)
                    }
                    _ => AnnotationTarget::Dangling,
                };
            }
        }
        next = element.next_sibling_or_token();
    }
    AnnotationTarget::Dangling
}

/// Every `#:` block directly inside `parent`, in source order, with what it applies to.
pub fn statement_annotations(parent: &SyntaxNode) -> Vec<(SyntaxNode, AnnotationTarget)> {
    parent
        .children()
        .filter(|child| child.kind() == SyntaxKind::ANNOTATION)
        .map(|annotation| {
            let target = annotation_target(&annotation);
            (annotation, target)
        })
        .collect()
}

/// The `#:` block attached to `statement`, found by walking back from it, so that a lowering
/// which meets one statement at a time does not rescan the statement's whole sequence.
pub fn attached_annotation(statement: &SyntaxNode) -> Option<SyntaxNode> {
    let mut previous = statement.prev_sibling_or_token();
    while let Some(element) = previous {
        match element.kind() {
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => {
                previous = element.prev_sibling_or_token();
            }
            SyntaxKind::ANNOTATION => {
                let annotation = element.into_node()?;
                return match annotation_target(&annotation) {
                    AnnotationTarget::Attached(target) if target == *statement => Some(annotation),
                    _ => None,
                };
            }
            _ => return None,
        }
    }
    None
}

pub enum BlockRefusal {
    FormMixing(&'static str),
    /// Silent: the parser already reported what was wrong.
    Unparsed,
}

/// A `#:` block must commit to exactly one form.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockForm {
    Compact,
    Expanded,
    Definition,
}

/// A line's form is that of its first item, and only whole lines are compared. A line with a
/// second item failed to parse (recovery re-parents an unreadable type's pieces to the block), so
/// it is `Unparsed`, not form mixing.
pub fn block_refusal(node: &SyntaxNode) -> Option<BlockRefusal> {
    // A refused region has no trustworthy payload: a refused binder, for instance, leaves type
    // parameters unbound, which would be blamed a second time as unknown types.
    if node.kind() == SyntaxKind::ERROR {
        return Some(BlockRefusal::Unparsed);
    }
    // Markers come from all descendant tokens, not just direct children: a line continuing a
    // still-open node puts its marker inside that node.
    let mut markers: Vec<TextSize> = Vec::new();
    for element in node.node_ref().descendants().skip(1) {
        if element.kind() == SyntaxKind::ERROR {
            return Some(BlockRefusal::Unparsed);
        }
    }
    for token in node.node_ref().tokens() {
        if token.kind() == SyntaxKind::ANNOTATION_MARKER {
            markers.push(token.range().start());
        }
    }
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

/// The word after a directive's `@`. Adjacent tokens join, because `@if-unknown` lexes as `if`
/// `-` `unknown`.
pub fn directive_name(node: &SyntaxNode) -> String {
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

/// The text of `node`'s first `NAME` child, as R reads the name.
pub fn name_text(node: &SyntaxNode) -> Option<String> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::NAME)
        .and_then(ast::Name::cast)
        .and_then(|name| name.text())
}

pub fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.node_ref()
        .child_tokens()
        .any(|token| token.kind() == kind)
}
