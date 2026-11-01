use {
    crate::{
        SyntaxNode, SyntaxToken,
        kind::SyntaxKind::{self, *},
    },
    std::borrow::Cow,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name(SyntaxNode);

impl Name {
    pub fn cast(node: SyntaxNode) -> Option<Name> {
        (node.kind() == NAME).then_some(Name(node))
    }

    pub fn token(&self) -> Option<SyntaxToken> {
        self.0.first_token()
    }

    /// The name as R sees it (backticks stripped, escapes resolved), so every
    /// spelling of one name compares equal.
    pub fn text(&self) -> Option<String> {
        Some(name_text(self.token()?.text()).into_owned())
    }
}

/// The name a `NAME` token spells, as [`Name::text`] reads it. Only a backticked name with
/// an escape allocates.
pub fn name_text(token: &str) -> Cow<'_, str> {
    match token.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        Some(quoted) if quoted.contains('\\') => Cow::Owned(unescape_name(quoted)),
        Some(quoted) => Cow::Borrowed(quoted),
        None => Cow::Borrowed(token),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryExpr(SyntaxNode);

impl BinaryExpr {
    pub fn cast(node: SyntaxNode) -> Option<BinaryExpr> {
        (node.kind() == BINARY_EXPR).then_some(BinaryExpr(node))
    }

    pub fn lhs(&self) -> Option<SyntaxNode> {
        self.0
            .children()
            .find(|node| is_expression_kind(node.kind()))
    }

    pub fn operator(&self) -> Option<SyntaxToken> {
        self.0
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| !token.kind().is_trivia())
    }

    pub fn rhs(&self) -> Option<SyntaxNode> {
        self.0
            .children()
            .filter(|node| is_expression_kind(node.kind()))
            .nth(1)
    }
}

pub fn is_expression_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        NAME | LITERAL
            | BINARY_EXPR
            | UNARY_EXPR
            | PAREN_EXPR
            | BRACE_EXPR
            | CALL_EXPR
            | SUBSET_EXPR
            | SUBSET2_EXPR
            | DOLLAR_EXPR
            | AT_EXPR
            | NAMESPACE_EXPR
            | FUNCTION_DEF
            | IF_EXPR
            | FOR_EXPR
            | WHILE_EXPR
            | REPEAT_EXPR
            | BREAK_EXPR
            | NEXT_EXPR
            | TUPLE_EXPR
            | RECORD_EXPR
            | ARRAY_EXPR
            | TAG_EXPR
            | MATCH_EXPR
            | NEW_EXPR
    )
}

/// Unknown escapes keep their backslash: R rejects them, and dropping it would
/// make two different names compare equal.
fn unescape_name(quoted: &str) -> String {
    let mut out = String::with_capacity(quoted.len());
    let mut characters = quoted.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(escaped @ ('\\' | '`' | '"' | '\'' | '\n')) => out.push(escaped),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
