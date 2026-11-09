//! Highlighting from the syntax tree alone. The language server's semantic tokens and the
//! console's colors share these classifiers, so a name reads the same in both.

use crate::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, ast::BinaryExpr,
    is_inline_annotation, kind::SyntaxKind::*,
};

/// What a highlighted token is. R grammars already color R's own tokens, so plain R gets a class
/// only where the grammar cannot know it: a function parameter wherever it is read, a named
/// argument, a called or defined function, a namespace, and member names. ry's own syntax is
/// classified in full: annotation and type-definition bodies, tags, `match` and its arms. The
/// tree decides most classes; the language server adds the ones only resolution can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticClass {
    Type,
    TypeParameter,
    /// A field or parameter name inside a type, a function's parameter and its reads, or the
    /// name of a named argument.
    Parameter,
    Operator,
    /// An `@` directive or the `@` of a nominal constructor.
    Decorator,
    Keyword,
    Tag,
    /// A called name, or a name assigned a function literal.
    Function,
    /// A called function that comes from a package or base R rather than the project: the
    /// function half of `pkg::name`, or a call head that resolves only to a stub. Editors render
    /// it as a function with the `defaultLibrary` modifier.
    LibraryFunction,
    /// The package of `pkg::name`.
    Namespace,
    /// A member name: the field of `x$field` or `x@slot`, a record literal's or pattern's field,
    /// a non-function R6 member, or an S4 slot.
    Property,
    /// A called member, `x$method()`, or an R6 member defined as a function.
    Method,
}

/// The class a console colors a token by: the [`SemanticClass`] the tree decides, and otherwise
/// the token's lexical class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Highlight {
    Keyword,
    /// `TRUE`, `FALSE`, `NULL`, `NA` and its typed forms, `Inf` and `NaN`.
    Constant,
    Number,
    String,
    Comment,
    /// A `#:` annotation comment or an inline type annotation, whole.
    Annotation,
    /// An assignment, the pipe `|>`, a `%op%` operator, or `match`'s `=>`.
    Operator,
    /// A called name, a name assigned a function literal, or a called member.
    Function,
    /// The package of `pkg::name` and its `::` or `:::`.
    Namespace,
    /// A function's parameter or a named argument.
    Parameter,
    Property,
    Tag,
    /// A name written in backticks.
    QuotedName,
    /// A token the lexer cannot read, or a closing bracket that closes nothing.
    Error,
}

/// Every token of `root` that has a class, in text order.
pub fn highlights(root: &SyntaxNode) -> Vec<(TextRange, Highlight)> {
    let mut spans = Vec::new();
    for token in root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
    {
        if token.text_range().is_empty() {
            continue;
        }
        if let Some(highlight) = highlight_token(&token) {
            spans.push((token.text_range(), highlight));
        }
    }
    for range in brackets(root).unmatched {
        if let Err(index) = spans.binary_search_by_key(&range.start(), |(span, _)| span.start()) {
            spans.insert(index, (range, Highlight::Error));
        }
    }
    spans
}

/// The bracket pairs of `root`, and the closing brackets that close nothing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Brackets {
    /// Each pair as its opening and closing bracket, in the order the pairs close.
    pub pairs: Vec<(TextRange, TextRange)>,
    pub unmatched: Vec<TextRange>,
}

/// Pairs `(` `)`, `{` `}`, `[` `]` and `[[` `]]`, the last closing on two `]` tokens. A closer
/// that does not match the innermost opener closes nothing, and the opener stays open.
pub fn brackets(root: &SyntaxNode) -> Brackets {
    let tokens: Vec<SyntaxToken> = root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| is_bracket(token.kind()))
        .collect();
    let mut result = Brackets::default();
    let mut open: Vec<&SyntaxToken> = Vec::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        index += 1;
        let opener = match token.kind() {
            L_PAREN | L_BRACE | L_BRACKET | L_BRACKET2 => {
                open.push(token);
                continue;
            }
            R_PAREN => L_PAREN,
            R_BRACE => L_BRACE,
            _ => match open.last().map(|token| token.kind()) {
                Some(L_BRACKET2) => L_BRACKET2,
                _ => L_BRACKET,
            },
        };
        let mut range = token.text_range();
        let matches = open.last().is_some_and(|last| last.kind() == opener);
        let closes = matches
            && (opener != L_BRACKET2
                || tokens.get(index).is_some_and(|next| {
                    next.kind() == R_BRACKET && next.text_range().start() == range.end()
                }));
        if !closes {
            result.unmatched.push(range);
            continue;
        }
        if opener == L_BRACKET2 {
            if let Some(next) = tokens.get(index) {
                range = range.cover(next.text_range());
            }
            index += 1;
        }
        if let Some(last) = open.pop() {
            result.pairs.push((last.text_range(), range));
        }
    }
    result
}

fn is_bracket(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        L_PAREN | R_PAREN | L_BRACE | R_BRACE | L_BRACKET | L_BRACKET2 | R_BRACKET
    )
}

fn highlight_token(token: &SyntaxToken) -> Option<Highlight> {
    let in_annotation = token
        .parent_ancestors()
        .any(|node| node.kind() == ANNOTATION || is_inline_annotation(&node));
    if in_annotation || token.kind() == ANNOTATION_MARKER {
        return Some(Highlight::Annotation);
    }
    let semantic = classify_ry_token(token).or_else(|| classify_r_name(token));
    if let Some(class) = semantic {
        return Some(match class {
            SemanticClass::Keyword => Highlight::Keyword,
            SemanticClass::Operator => Highlight::Operator,
            SemanticClass::Tag => Highlight::Tag,
            SemanticClass::Function | SemanticClass::LibraryFunction | SemanticClass::Method => {
                Highlight::Function
            }
            SemanticClass::Namespace => Highlight::Namespace,
            SemanticClass::Parameter => Highlight::Parameter,
            SemanticClass::Property => Highlight::Property,
            SemanticClass::Type | SemanticClass::TypeParameter | SemanticClass::Decorator => {
                Highlight::Annotation
            }
        });
    }
    Some(match token.kind() {
        FUNCTION_KW | IF_KW | ELSE_KW | REPEAT_KW | WHILE_KW | FOR_KW | IN_KW | NEXT_KW
        | BREAK_KW => Highlight::Keyword,
        TRUE_KW | FALSE_KW | NULL_KW | INF_KW | NAN_KW | NA_KW | NA_INTEGER_KW | NA_REAL_KW
        | NA_COMPLEX_KW | NA_CHARACTER_KW => Highlight::Constant,
        INTEGER | DOUBLE | COMPLEX | SUFFIXED_NUMBER => Highlight::Number,
        STRING | RAW_STRING | FSTRING_START | FSTRING_TEXT | FSTRING_END => Highlight::String,
        COMMENT => Highlight::Comment,
        LESS_MINUS | LESS2_MINUS | MINUS_GREATER | MINUS_GREATER2 | COLON_EQ | PIPE_GREATER
        | SPECIAL => Highlight::Operator,
        EQ if token.parent().is_some_and(|parent| parent.kind() == BINARY_EXPR) => {
            Highlight::Operator
        }
        COLON2 | COLON3 => Highlight::Namespace,
        IDENT if token.text().starts_with('`') => Highlight::QuotedName,
        ERROR_TOKEN => Highlight::Error,
        _ => return None,
    })
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
            // The only name a function type owns directly is its `fn`.
            Some(TYPE_FUNCTION) => Some(SemanticClass::Keyword),
            Some(TYPE_FIELD | TYPE_PARAMETER) => Some(SemanticClass::Parameter),
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
pub fn classify_ry_token(token: &SyntaxToken) -> Option<SemanticClass> {
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

/// The classes the syntax decides on its own: a parameter's declaration, a named argument, a
/// call's head, a name assigned a function literal, both halves of `pkg::name`, the member of
/// `x$name` and `x@name`, and the field names of record literals and patterns.
pub fn classify_r_name(token: &SyntaxToken) -> Option<SemanticClass> {
    if token.kind() != IDENT {
        return None;
    }
    let parent = token.parent()?;
    if parent.kind() == PARAMETER {
        return Some(SemanticClass::Parameter);
    }
    if parent.kind() != NAME {
        return None;
    }
    let holder = parent.parent()?;
    let is_first_child = holder.first_child().as_ref() == Some(&parent);
    match holder.kind() {
        NAMESPACE_EXPR if is_first_child => Some(SemanticClass::Namespace),
        NAMESPACE_EXPR => holder
            .parent()
            .filter(|call| call.kind() == CALL_EXPR && call.first_child().as_ref() == Some(&holder))
            .map(|_| SemanticClass::LibraryFunction),
        CALL_EXPR if is_first_child => Some(SemanticClass::Function),
        DOLLAR_EXPR | AT_EXPR if !is_first_child => {
            let called = holder.parent().is_some_and(|call| {
                call.kind() == CALL_EXPR && call.first_child().as_ref() == Some(&holder)
            });
            Some(if called {
                SemanticClass::Method
            } else {
                SemanticClass::Property
            })
        }
        PATTERN_FIELD if is_first_child => Some(SemanticClass::Property),
        ARGUMENT if is_first_child => {
            let in_record = holder
                .parent()
                .and_then(|list| list.parent())
                .is_some_and(|record| record.kind() == RECORD_EXPR);
            if in_record {
                Some(SemanticClass::Property)
            } else {
                // `f(x)` passes the name `x` as a value; only `f(x = 1)` names an argument.
                holder
                    .children_with_tokens()
                    .any(|element| element.kind() == EQ)
                    .then_some(SemanticClass::Parameter)
            }
        }
        BINARY_EXPR if is_first_child => {
            let binary = BinaryExpr::cast(holder)?;
            let assigns = binary
                .operator()
                .is_some_and(|operator| matches!(operator.kind(), LESS_MINUS | EQ | COLON_EQ));
            (assigns
                && binary
                    .rhs()
                    .is_some_and(|value| value.kind() == FUNCTION_DEF))
            .then_some(SemanticClass::Function)
        }
        _ => None,
    }
}

/// The tag node a token spells: its `:` or its name.
pub fn tag_node_of(token: &SyntaxToken) -> Option<SyntaxNode> {
    let parent = token.parent()?;
    let node = match token.kind() {
        COLON => parent,
        IDENT if parent.kind() == NAME => parent.parent()?,
        _ => return None,
    };
    is_tag_node(node.kind()).then_some(node)
}

pub fn is_tag_node(kind: SyntaxKind) -> bool {
    matches!(kind, TAG_EXPR | PATTERN_TAG | TYPE_TAG)
}
