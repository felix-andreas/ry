use crate::kind::SyntaxKind;
use crate::{SyntaxNode, SyntaxToken};

pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            fn cast(node: SyntaxNode) -> Option<Self> {
                (node.kind() == SyntaxKind::$kind).then_some($name(node))
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(SourceFile, SOURCE_FILE);
ast_node!(Name, NAME);
ast_node!(Literal, LITERAL);
ast_node!(BinaryExpr, BINARY_EXPR);
ast_node!(UnaryExpr, UNARY_EXPR);
ast_node!(ParenExpr, PAREN_EXPR);
ast_node!(BraceExpr, BRACE_EXPR);
ast_node!(CallExpr, CALL_EXPR);
ast_node!(ArgumentList, ARGUMENT_LIST);
ast_node!(Argument, ARGUMENT);
ast_node!(FunctionDef, FUNCTION_DEF);
ast_node!(ParameterList, PARAMETER_LIST);
ast_node!(Parameter, PARAMETER);
ast_node!(IfExpr, IF_EXPR);
ast_node!(ForExpr, FOR_EXPR);
ast_node!(WhileExpr, WHILE_EXPR);
ast_node!(RepeatExpr, REPEAT_EXPR);
ast_node!(Annotation, ANNOTATION);

impl SourceFile {
    /// Error regions are skipped.
    pub fn statements(&self) -> impl Iterator<Item = SyntaxNode> + '_ {
        self.0
            .children()
            .filter(|node| is_expression_kind(node.kind()))
    }
}

impl Name {
    pub fn token(&self) -> Option<SyntaxToken> {
        self.0.first_token()
    }

    /// The name as R sees it (backticks stripped, escapes resolved), so every
    /// spelling of one name compares equal.
    pub fn text(&self) -> Option<String> {
        let token = self.token()?;
        let text = token.text();
        let Some(quoted) = text.strip_prefix('`').and_then(|t| t.strip_suffix('`')) else {
            return Some(text.to_owned());
        };
        Some(unescape_name(quoted))
    }
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

impl BinaryExpr {
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

impl FunctionDef {
    pub fn parameters(&self) -> Option<ParameterList> {
        self.0.children().find_map(ParameterList::cast)
    }

    pub fn body(&self) -> Option<SyntaxNode> {
        self.0
            .children()
            .filter(|node| is_expression_kind(node.kind()))
            .last()
    }
}

pub fn is_expression_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::NAME
            | SyntaxKind::LITERAL
            | SyntaxKind::BINARY_EXPR
            | SyntaxKind::UNARY_EXPR
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::BRACE_EXPR
            | SyntaxKind::CALL_EXPR
            | SyntaxKind::SUBSET_EXPR
            | SyntaxKind::SUBSET2_EXPR
            | SyntaxKind::DOLLAR_EXPR
            | SyntaxKind::AT_EXPR
            | SyntaxKind::NAMESPACE_EXPR
            | SyntaxKind::FUNCTION_DEF
            | SyntaxKind::IF_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::REPEAT_EXPR
            | SyntaxKind::BREAK_EXPR
            | SyntaxKind::NEXT_EXPR
    )
}
