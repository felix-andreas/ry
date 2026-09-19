//! Per-item HIR: the desugared expression tree inference walks.
//!
//! Lowered from an item's green subtree (`item_syntax`), never from the whole
//! file, so every span here is **item-relative** (the subtree's offsets start
//! at 0). That keeps the HIR position-independent: an edit elsewhere in the
//! file reproduces an equal `Module`, and salsa's early cutoff stops
//! recomputation before naming or inference run. Absolute positions are
//! reintroduced only at the diagnostic-rendering edge.
//!
//! Error tolerance mirrors the project contract: a broken region lowers to
//! `Missing`, which cascades no diagnostic because its syntax error already
//! marks it. A well-formed sibling lowers normally.

use syntax::ast::AstNode as _;
use syntax::{SyntaxKind, SyntaxNode, TextRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ExprId(pub u32);

/// One item's lowered body: an expression arena plus the root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub expressions: Vec<Expression>,
    pub root: Option<ExprId>,
}

impl Module {
    pub fn expression(&self, id: ExprId) -> &Expression {
        &self.expressions[id.0 as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    pub kind: ExpressionKind,
    /// Item-relative source range.
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignSpelling {
    /// `<-` / `->` (the HIR normalizes right assignment into left form).
    Local,
    /// `=`
    Equals,
    /// `<<-` / `->>`
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOperator {
    Minus,
    Plus,
    Not,
    Tilde,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Modulo,
    IntegerDivide,
    /// Any other `%…%` operator (including user-defined).
    Special,
    Sequence,
    Less,
    Greater,
    LessEq,
    GreaterEq,
    Equal,
    NotEqual,
    And,
    And2,
    Or,
    Or2,
    Pipe,
    Tilde,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralKind {
    /// Numeric literals keep their source text: the checker needs the value
    /// for literal index positions (`x[[2L]]`) and the literal-as-integer
    /// courtesy (`seq_len(10)`).
    Integer(String),
    Double(String),
    Complex,
    String(String),
    Logical(bool),
    Null,
    /// `NA` and its typed variants: each is a scalar of a fixed atom (`NA`
    /// is logical; `NA_integer_`, `NA_real_`, `NA_complex_`, and
    /// `NA_character_` name theirs).
    Na(NaAtom),
    Inf,
    NaN,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NaAtom {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    pub name: Option<String>,
    /// The argument NAME's range, when the argument is tagged. The name is a
    /// distinct source token, so a finding about it points at the name rather
    /// than at the value it is attached to, as `Parameter::range` does.
    pub name_range: Option<TextRange>,
    /// `None` is a positional hole (`f(, x)`) or an empty tagged value.
    pub value: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub default: Option<ExprId>,
    /// The parameter NAME's range, which is the binding site rather than the
    /// default, so navigation and rename target the name alone.
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionKind {
    /// A hole from a broken region: types as Unknown, records no strict
    /// origin, produces no diagnostics of its own.
    Missing,
    Literal(LiteralKind),
    /// A name reference (`x`, backticks stripped, `...`, `..1`, `_`).
    NameRef(String),
    Assign {
        spelling: AssignSpelling,
        target: ExprId,
        value: ExprId,
    },
    Unary {
        operator: UnaryOperator,
        operand: ExprId,
    },
    Binary {
        operator: BinaryOperator,
        /// A `%…%` operator's spelling, carried for [`BinaryOperator::Special`]
        /// only. R evaluates `a %op% b` as a call to a function of that name,
        /// so naming needs the name to keep a user-defined operator's
        /// definition alive; the checker still treats the construct as opaque.
        special_name: Option<String>,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        arguments: Vec<Argument>,
    },
    /// `x[…]` / `x[[…]]`.
    Index {
        double: bool,
        target: ExprId,
        arguments: Vec<Argument>,
    },
    /// `x$name` / `x@name`.
    Field {
        at: bool,
        target: ExprId,
        name: Option<String>,
        /// The name token's own range, so a finding about the FIELD points at
        /// the field rather than at the whole access chain it ends. Not
        /// derivable from the expression's range without re-parsing.
        name_range: Option<TextRange>,
    },
    /// `pkg::name` / `pkg:::name`.
    Namespace {
        internal: bool,
        package: Option<String>,
        name: Option<String>,
    },
    Function {
        parameters: Vec<Parameter>,
        body: ExprId,
    },
    If {
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    For {
        variable: Option<String>,
        variable_range: Option<TextRange>,
        sequence: ExprId,
        body: ExprId,
    },
    While {
        condition: ExprId,
        body: ExprId,
    },
    Repeat {
        body: ExprId,
    },
    /// `local(expr)`: the body evaluates immediately in a fresh environment.
    /// Recognized by the bare callee name during lowering (rebinding `local`
    /// is not modeled), like `return` and `stop`.
    Local {
        body: ExprId,
    },
    Block {
        statements: Vec<ExprId>,
        /// The last expression is terminated with `;`, so the block's value
        /// is `NULL`.
        trailing_semicolon: bool,
    },
    Paren(ExprId),
    Break,
    Next,
}

impl ExpressionKind {
    /// The direct child expression ids, in source order.
    pub fn child_ids(&self) -> Vec<ExprId> {
        match self {
            ExpressionKind::Missing
            | ExpressionKind::Literal(_)
            | ExpressionKind::NameRef(_)
            | ExpressionKind::Namespace { .. }
            | ExpressionKind::Break
            | ExpressionKind::Next => Vec::new(),
            ExpressionKind::Assign { target, value, .. } => vec![*target, *value],
            ExpressionKind::Unary { operand, .. } => vec![*operand],
            ExpressionKind::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
            ExpressionKind::Call { callee, arguments } => {
                let mut ids = vec![*callee];
                ids.extend(arguments.iter().filter_map(|argument| argument.value));
                ids
            }
            ExpressionKind::Index {
                target, arguments, ..
            } => {
                let mut ids = vec![*target];
                ids.extend(arguments.iter().filter_map(|argument| argument.value));
                ids
            }
            ExpressionKind::Field { target, .. } => vec![*target],
            ExpressionKind::Function { parameters, body } => {
                let mut ids: Vec<ExprId> = parameters
                    .iter()
                    .filter_map(|parameter| parameter.default)
                    .collect();
                ids.push(*body);
                ids
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let mut ids = vec![*condition, *then_branch];
                ids.extend(*else_branch);
                ids
            }
            ExpressionKind::For { sequence, body, .. } => vec![*sequence, *body],
            ExpressionKind::While { condition, body } => vec![*condition, *body],
            ExpressionKind::Repeat { body } | ExpressionKind::Local { body } => vec![*body],
            ExpressionKind::Block { statements, .. } => statements.clone(),
            ExpressionKind::Paren(inner) => vec![*inner],
        }
    }
}

/// Lower one item subtree (rooted at offset 0) into a `Module`.
pub fn lower_item(root: &SyntaxNode) -> Module {
    let mut lowering = Lowering {
        module: Module::default(),
    };
    let root_id = lowering.lower_expression(root);
    lowering.module.root = Some(root_id);
    lowering.module
}

struct Lowering {
    module: Module,
}

impl Lowering {
    fn allocate(&mut self, kind: ExpressionKind, range: TextRange) -> ExprId {
        let id = ExprId(self.module.expressions.len() as u32);
        self.module.expressions.push(Expression { kind, range });
        id
    }

    fn missing(&mut self, range: TextRange) -> ExprId {
        self.allocate(ExpressionKind::Missing, range)
    }

    /// Child expressions of a node, trivia and error regions skipped.
    fn child_expressions(node: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
        node.children()
            .filter(|child| syntax::ast::is_expression_kind(child.kind()))
    }

    fn lower_optional(&mut self, node: Option<SyntaxNode>, fallback: TextRange) -> ExprId {
        match node {
            Some(node) => self.lower_expression(&node),
            None => self.missing(fallback),
        }
    }

    fn lower_expression(&mut self, node: &SyntaxNode) -> ExprId {
        let range = significant_range(node);
        match node.kind() {
            SyntaxKind::NAME => {
                let text = syntax::ast::Name::cast(node.clone())
                    .and_then(|name| name.text())
                    .unwrap_or_default();
                self.allocate(ExpressionKind::NameRef(text), range)
            }
            SyntaxKind::LITERAL => {
                let kind = literal_kind(node);
                self.allocate(ExpressionKind::Literal(kind), range)
            }
            SyntaxKind::PAREN_EXPR => {
                let inner = Self::child_expressions(node).next();
                let inner = self.lower_optional(inner, range);
                self.allocate(ExpressionKind::Paren(inner), range)
            }
            SyntaxKind::BRACE_EXPR => {
                let statements: Vec<ExprId> = Self::child_expressions(node)
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|child| self.lower_expression(child))
                    .collect();
                // Walking backwards over the block's direct tokens: hitting a
                // `;` before any statement node means the last expression is
                // terminated (statement content lives in child nodes, so the
                // direct tokens are only braces, separators, and trivia).
                let trailing_semicolon = node
                    .children_with_tokens()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .find_map(|element| match element {
                        rowan::NodeOrToken::Token(token) => match token.kind() {
                            SyntaxKind::WHITESPACE
                            | SyntaxKind::NEWLINE
                            | SyntaxKind::COMMENT
                            | SyntaxKind::R_BRACE => None,
                            SyntaxKind::SEMICOLON => Some(true),
                            _ => Some(false),
                        },
                        rowan::NodeOrToken::Node(_) => Some(false),
                    })
                    .unwrap_or(false);
                self.allocate(
                    ExpressionKind::Block {
                        statements,
                        trailing_semicolon,
                    },
                    range,
                )
            }
            SyntaxKind::UNARY_EXPR => {
                let operator = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .find_map(|token| unary_operator(token.kind()));
                let operand = Self::child_expressions(node).next();
                let operand = self.lower_optional(operand, range);
                match operator {
                    Some(operator) => {
                        self.allocate(ExpressionKind::Unary { operator, operand }, range)
                    }
                    None => self.missing(range),
                }
            }
            SyntaxKind::BINARY_EXPR => self.lower_binary(node, range),
            SyntaxKind::CALL_EXPR => {
                let callee_node = Self::child_expressions(node).next();
                let is_local = callee_node.as_ref().is_some_and(|callee| {
                    callee.kind() == SyntaxKind::NAME
                        && syntax::ast::Name::cast(callee.clone())
                            .and_then(|name| name.text())
                            .is_some_and(|text| text == "local")
                });
                let arguments = self.lower_arguments(node);
                if is_local
                    && let [
                        Argument {
                            name: None,
                            value: Some(body),
                            ..
                        },
                    ] = arguments.as_slice()
                {
                    return self.allocate(ExpressionKind::Local { body: *body }, range);
                }
                let callee = self.lower_optional(callee_node, range);
                self.allocate(ExpressionKind::Call { callee, arguments }, range)
            }
            SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                let double = node.kind() == SyntaxKind::SUBSET2_EXPR;
                let target = Self::child_expressions(node).next();
                let target = self.lower_optional(target, range);
                let arguments = self.lower_arguments(node);
                self.allocate(
                    ExpressionKind::Index {
                        double,
                        target,
                        arguments,
                    },
                    range,
                )
            }
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => {
                let at = node.kind() == SyntaxKind::AT_EXPR;
                let mut children = Self::child_expressions(node);
                let target = children.next();
                let target = self.lower_optional(target, range);
                let name_node = children.next();
                let name_range = name_node.as_ref().map(|child| child.text_range());
                let name = name_node.and_then(|child| field_name_text(&child));
                let name_range = name_range.filter(|_| name.is_some());
                self.allocate(
                    ExpressionKind::Field {
                        at,
                        target,
                        name,
                        name_range,
                    },
                    range,
                )
            }
            SyntaxKind::NAMESPACE_EXPR => {
                let internal = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::COLON3);
                let mut children = Self::child_expressions(node);
                let package = children.next().and_then(|child| field_name_text(&child));
                let name = children.next().and_then(|child| field_name_text(&child));
                self.allocate(
                    ExpressionKind::Namespace {
                        internal,
                        package,
                        name,
                    },
                    range,
                )
            }
            SyntaxKind::FUNCTION_DEF => {
                let parameters = node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::PARAMETER_LIST)
                    .map(|list| self.lower_parameters(&list))
                    .unwrap_or_default();
                let body = Self::child_expressions(node).next();
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::Function { parameters, body }, range)
            }
            SyntaxKind::IF_EXPR => {
                let mut children = Self::child_expressions(node);
                let condition = children.next();
                let then_branch = children.next();
                let else_branch = children.next();
                let condition = self.lower_optional(condition, range);
                let then_branch = self.lower_optional(then_branch, range);
                let else_branch = else_branch.map(|node| self.lower_expression(&node));
                self.allocate(
                    ExpressionKind::If {
                        condition,
                        then_branch,
                        else_branch,
                    },
                    range,
                )
            }
            SyntaxKind::FOR_EXPR => {
                let mut children = Self::child_expressions(node);
                // The loop variable is the first NAME child; sequence and body follow.
                let variable_node = children.next();
                let (variable, variable_range) = match &variable_node {
                    Some(node) if node.kind() == SyntaxKind::NAME => (
                        syntax::ast::Name::cast(node.clone()).and_then(|name| name.text()),
                        Some(node.text_range()),
                    ),
                    _ => (None, None),
                };
                let sequence = children.next();
                let body = children.next();
                let sequence = self.lower_optional(sequence, range);
                let body = self.lower_optional(body, range);
                self.allocate(
                    ExpressionKind::For {
                        variable,
                        variable_range,
                        sequence,
                        body,
                    },
                    range,
                )
            }
            SyntaxKind::WHILE_EXPR => {
                let mut children = Self::child_expressions(node);
                let condition = children.next();
                let body = children.next();
                let condition = self.lower_optional(condition, range);
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::While { condition, body }, range)
            }
            SyntaxKind::REPEAT_EXPR => {
                let body = Self::child_expressions(node).next();
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::Repeat { body }, range)
            }
            SyntaxKind::BREAK_EXPR => self.allocate(ExpressionKind::Break, range),
            SyntaxKind::NEXT_EXPR => self.allocate(ExpressionKind::Next, range),
            _ => self.missing(range),
        }
    }

    fn lower_binary(&mut self, node: &SyntaxNode, range: TextRange) -> ExprId {
        let Some(binary) = syntax::ast::BinaryExpr::cast(node.clone()) else {
            return self.missing(range);
        };
        let operator_token = binary.operator();
        let lhs = binary.lhs();
        let rhs = binary.rhs();
        let Some(operator_token) = operator_token else {
            return self.missing(range);
        };

        if operator_token.kind() == SyntaxKind::PIPE_GREATER
            && let Some(call) = self.lower_pipe(lhs.clone(), rhs.clone(), range)
        {
            return call;
        }

        match operator_token.kind() {
            // Assignments normalize to target/value regardless of spelling.
            SyntaxKind::LESS_MINUS | SyntaxKind::EQ | SyntaxKind::LESS2_MINUS => {
                let spelling = match operator_token.kind() {
                    SyntaxKind::LESS_MINUS => AssignSpelling::Local,
                    SyntaxKind::EQ => AssignSpelling::Equals,
                    _ => AssignSpelling::Super,
                };
                let target = self.lower_optional(lhs, range);
                let value = self.lower_optional(rhs, range);
                self.allocate(
                    ExpressionKind::Assign {
                        spelling,
                        target,
                        value,
                    },
                    range,
                )
            }
            // `:=` binds no variable anywhere in R: it is a call to the
            // `:=` function (data.table defines it for its brackets, rlang
            // for quoting), spelled as an operator. The callee sits on the
            // operator token, so an unresolved `:=` reports exactly there.
            SyntaxKind::COLON_EQ => {
                let callee = self.allocate(
                    ExpressionKind::NameRef(":=".to_owned()),
                    operator_token.text_range(),
                );
                let left = self.lower_optional(lhs, range);
                let right = self.lower_optional(rhs, range);
                self.allocate(
                    ExpressionKind::Call {
                        callee,
                        arguments: vec![
                            Argument {
                                name: None,
                                name_range: None,
                                value: Some(left),
                            },
                            Argument {
                                name: None,
                                name_range: None,
                                value: Some(right),
                            },
                        ],
                    },
                    range,
                )
            }
            SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2 => {
                let spelling = if operator_token.kind() == SyntaxKind::MINUS_GREATER {
                    AssignSpelling::Local
                } else {
                    AssignSpelling::Super
                };
                let value = self.lower_optional(lhs, range);
                let target = self.lower_optional(rhs, range);
                self.allocate(
                    ExpressionKind::Assign {
                        spelling,
                        target,
                        value,
                    },
                    range,
                )
            }
            kind => {
                let operator = if kind == SyntaxKind::SPECIAL {
                    // `%%` and `%/%` are arithmetic; any other `%…%` special
                    // (including user-defined operators) stays opaque.
                    match operator_token.text() {
                        "%%" => Some(BinaryOperator::Modulo),
                        "%/%" => Some(BinaryOperator::IntegerDivide),
                        _ => Some(BinaryOperator::Special),
                    }
                } else {
                    binary_operator(kind)
                };
                let Some(operator) = operator else {
                    return self.missing(range);
                };
                let lhs = self.lower_optional(lhs, range);
                let rhs = self.lower_optional(rhs, range);
                let special_name =
                    (operator == BinaryOperator::Special).then(|| operator_token.text().to_owned());
                self.allocate(
                    ExpressionKind::Binary {
                        operator,
                        special_name,
                        lhs,
                        rhs,
                    },
                    range,
                )
            }
        }
    }

    /// `x |> f(y)` is sugar R's own parser rewrites to `f(x, y)` before
    /// evaluation, so the pipe lowers AS that call and naming, typing, and
    /// every IDE feature see the real semantics. R accepts only a call on
    /// the right-hand side, and a `_` placeholder only as the whole value of
    /// exactly one named argument (which then receives the piped value
    /// instead of the first positional slot). Anything else is an R error, so
    /// such a pipe keeps the opaque-operator lowering, which is `None`, rather
    /// than guessing. A non-call right-hand side and a positional or repeated
    /// `_` are both errors.
    fn lower_pipe(
        &mut self,
        lhs: Option<SyntaxNode>,
        rhs: Option<SyntaxNode>,
        range: TextRange,
    ) -> Option<ExprId> {
        let call = rhs.filter(|node| node.kind() == SyntaxKind::CALL_EXPR)?;
        let shape = pipe_shape(&call)?;
        let piped = self.lower_optional(lhs, range);
        let callee = call
            .children()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST);
        let callee = self.lower_optional(callee, range);
        let substitute = match shape {
            PipeShape::FirstArgument => None,
            PipeShape::NamedPlaceholder(index) => Some((index, piped)),
        };
        let mut arguments = self.lower_arguments_substituting(&call, substitute);
        if matches!(shape, PipeShape::FirstArgument) {
            arguments.insert(
                0,
                Argument {
                    name: None,
                    name_range: None,
                    value: Some(piped),
                },
            );
        }
        Some(self.allocate(ExpressionKind::Call { callee, arguments }, range))
    }

    fn lower_arguments(&mut self, node: &SyntaxNode) -> Vec<Argument> {
        self.lower_arguments_substituting(node, None)
    }

    /// Argument lowering with an optional pipe-placeholder substitution: the
    /// argument at `substitute`'s index takes the given (already lowered)
    /// value instead of lowering its own. The `_` token never becomes an
    /// expression, so nothing dangles in the arena.
    fn lower_arguments_substituting(
        &mut self,
        node: &SyntaxNode,
        substitute: Option<(usize, ExprId)>,
    ) -> Vec<Argument> {
        let Some(list) = node
            .children()
            .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
        else {
            return Vec::new();
        };
        list.children()
            .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
            .collect::<Vec<_>>()
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                let tag = argument
                    .children()
                    .find(|child| {
                        child.kind() == SyntaxKind::NAME || child.kind() == SyntaxKind::LITERAL
                    })
                    .filter(|_| {
                        argument
                            .children_with_tokens()
                            .filter_map(|element| element.into_token())
                            .any(|token| token.kind() == SyntaxKind::EQ)
                    });
                let name_range = tag.as_ref().map(|tag| tag.text_range());
                let name = tag.as_ref().and_then(field_name_text);
                if let Some((placeholder, piped)) = substitute
                    && placeholder == index
                {
                    return Argument {
                        name,
                        name_range,
                        value: Some(piped),
                    };
                }
                let value_node = {
                    let mut expressions = Self::child_expressions(argument);
                    if name.is_some() {
                        // The tag itself is the first expression child.
                        expressions.next();
                    }
                    expressions.next()
                };
                let value = value_node.map(|node| self.lower_expression(&node));
                Argument {
                    name,
                    name_range,
                    value,
                }
            })
            .collect()
    }

    fn lower_parameters(&mut self, list: &SyntaxNode) -> Vec<Parameter> {
        list.children()
            .filter(|child| child.kind() == SyntaxKind::PARAMETER)
            .collect::<Vec<_>>()
            .iter()
            .map(|parameter| {
                let name = parameter
                    .first_token()
                    .map(|token| {
                        let text = token.text();
                        text.strip_prefix('`')
                            .and_then(|t| t.strip_suffix('`'))
                            .unwrap_or(text)
                            .to_owned()
                    })
                    .unwrap_or_default();
                let default = Self::child_expressions(parameter)
                    .next()
                    .map(|node| self.lower_expression(&node));
                let range = parameter
                    .first_token()
                    .map(|token| token.text_range())
                    .unwrap_or_else(|| parameter.text_range());
                Parameter {
                    name,
                    default,
                    range,
                }
            })
            .collect()
    }
}

/// A node's range without the trailing trivia rowan attaches inside
/// expression nodes (whitespace, newlines, comments, `#:` annotations):
/// blame and hover ranges must cover code, not the trivia that happened to
/// stick to the node.
fn significant_range(node: &SyntaxNode) -> TextRange {
    let full = node.text_range();
    let mut end = full.end();
    let mut token = node.last_token();
    while let Some(current) = token {
        if current.text_range().start() < full.start() {
            break;
        }
        let trivia = matches!(
            current.kind(),
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE | SyntaxKind::COMMENT
        ) || current
            .parent_ancestors()
            .any(|ancestor| ancestor.kind() == SyntaxKind::ANNOTATION);
        if !trivia {
            break;
        }
        end = current.text_range().start();
        token = current.prev_token();
    }
    TextRange::new(full.start(), end.max(full.start()))
}

fn literal_kind(node: &SyntaxNode) -> LiteralKind {
    let Some(token) = node.first_token() else {
        return LiteralKind::Null;
    };
    match token.kind() {
        SyntaxKind::INTEGER => LiteralKind::Integer(token.text().to_owned()),
        SyntaxKind::DOUBLE => LiteralKind::Double(token.text().to_owned()),
        SyntaxKind::COMPLEX => LiteralKind::Complex,
        SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
            LiteralKind::String(string_value(token.text()))
        }
        SyntaxKind::TRUE_KW => LiteralKind::Logical(true),
        SyntaxKind::FALSE_KW => LiteralKind::Logical(false),
        SyntaxKind::NULL_KW => LiteralKind::Null,
        SyntaxKind::INF_KW => LiteralKind::Inf,
        SyntaxKind::NAN_KW => LiteralKind::NaN,
        SyntaxKind::NA_INTEGER_KW => LiteralKind::Na(NaAtom::Integer),
        SyntaxKind::NA_REAL_KW => LiteralKind::Na(NaAtom::Double),
        SyntaxKind::NA_COMPLEX_KW => LiteralKind::Na(NaAtom::Complex),
        SyntaxKind::NA_CHARACTER_KW => LiteralKind::Na(NaAtom::Character),
        _ => LiteralKind::Na(NaAtom::Logical),
    }
}

/// A finite whole-number double literal, such as `1` or `2.0`. Such a literal
/// may fill an integer parameter.
pub fn is_whole_number_double(text: &str) -> bool {
    text.parse::<f64>()
        .is_ok_and(|value| value.fract() == 0.0 && value.is_finite())
}

/// The 0-based position of a statically known index literal. R indexes
/// identically with `x[[2]]` and `x[[2L]]`, so a whole-number double literal
/// is just as valid a position as an integer literal.
pub fn integer_literal_position(kind: &ExpressionKind) -> Option<usize> {
    let one_based_index = match kind {
        ExpressionKind::Literal(LiteralKind::Integer(text)) => {
            text.trim_end_matches('L').parse::<usize>().ok()?
        }
        ExpressionKind::Literal(LiteralKind::Double(text)) => {
            let value = text.parse::<f64>().ok()?;
            if value.fract() != 0.0 || value < 1.0 || !value.is_finite() {
                return None;
            }
            value as usize
        }
        _ => return None,
    };
    one_based_index.checked_sub(1)
}

/// The value of a string literal with quotes stripped. Escape *decoding* is a
/// later slice; raw strings strip their full delimiters.
fn string_value(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(['r', 'R']) {
        let Some(quote) = rest.chars().next() else {
            return text.to_owned();
        };
        let inner = &rest[quote.len_utf8()..];
        let dashes = inner.chars().take_while(|&c| c == '-').count();
        let open = inner[dashes..].chars().next();
        if open.is_some() {
            let body_start = dashes + 1;
            let mut closer = String::new();
            closer.push(match open {
                Some('(') => ')',
                Some('[') => ']',
                _ => '}',
            });
            closer.extend(std::iter::repeat_n('-', dashes));
            closer.push(quote);
            let body = &inner[body_start..];
            return body
                .strip_suffix(closer.as_str())
                .unwrap_or(body)
                .to_owned();
        }
        return text.to_owned();
    }
    text.trim_matches(['"', '\'']).to_owned()
}

fn field_name_text(node: &SyntaxNode) -> Option<String> {
    match node.kind() {
        SyntaxKind::NAME => syntax::ast::Name::cast(node.clone()).and_then(|name| name.text()),
        SyntaxKind::LITERAL => {
            let token = node.first_token()?;
            (token.kind() == SyntaxKind::STRING).then(|| string_value(token.text()))
        }
        _ => None,
    }
}

fn unary_operator(kind: SyntaxKind) -> Option<UnaryOperator> {
    let operator = match kind {
        SyntaxKind::MINUS => UnaryOperator::Minus,
        SyntaxKind::PLUS => UnaryOperator::Plus,
        SyntaxKind::BANG => UnaryOperator::Not,
        SyntaxKind::TILDE => UnaryOperator::Tilde,
        SyntaxKind::QUESTION => UnaryOperator::Help,
        _ => return None,
    };
    Some(operator)
}

/// Where a pipe's piped value goes in the right-hand call.
enum PipeShape {
    FirstArgument,
    /// The argument index whose `_` placeholder value the piped value
    /// replaces.
    NamedPlaceholder(usize),
}

/// Classifies the right-hand call of a `|>` by R's placeholder rules:
/// no `_` anywhere means first-argument insertion; exactly one `_`, sitting
/// as the whole value of a named argument of THIS call, receives the piped
/// value; every other `_` use (positional, repeated, nested in a
/// subexpression, or as an argument tag) is an R error. The result is `None`,
/// and the pipe stays an opaque operator.
fn pipe_shape(call: &SyntaxNode) -> Option<PipeShape> {
    let underscores: Vec<syntax::SyntaxToken> = call
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::UNDERSCORE)
        .collect();
    let [token] = underscores.as_slice() else {
        return underscores.is_empty().then_some(PipeShape::FirstArgument);
    };
    let name = token
        .parent()
        .filter(|node| node.kind() == SyntaxKind::NAME)?;
    let argument = name
        .parent()
        .filter(|node| node.kind() == SyntaxKind::ARGUMENT)?;
    let list = argument
        .parent()
        .filter(|node| node.kind() == SyntaxKind::ARGUMENT_LIST)?;
    if list.parent().as_ref() != Some(call) {
        return None;
    }
    let named = argument
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.kind() == SyntaxKind::EQ);
    // The `_` must be the argument's VALUE: with a tag present, the value is
    // the second expression child.
    let is_value = argument
        .children()
        .filter(|child| syntax::ast::is_expression_kind(child.kind()))
        .nth(1)
        .as_ref()
        == Some(&name);
    if !named || !is_value {
        return None;
    }
    let index = list
        .children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .position(|candidate| candidate == argument)?;
    Some(PipeShape::NamedPlaceholder(index))
}

fn binary_operator(kind: SyntaxKind) -> Option<BinaryOperator> {
    let operator = match kind {
        SyntaxKind::PLUS => BinaryOperator::Add,
        SyntaxKind::MINUS => BinaryOperator::Subtract,
        SyntaxKind::STAR => BinaryOperator::Multiply,
        SyntaxKind::SLASH => BinaryOperator::Divide,
        SyntaxKind::CARET => BinaryOperator::Power,
        SyntaxKind::SPECIAL => BinaryOperator::Special,
        SyntaxKind::PIPE_GREATER => BinaryOperator::Pipe,
        SyntaxKind::COLON => BinaryOperator::Sequence,
        SyntaxKind::LESS => BinaryOperator::Less,
        SyntaxKind::GREATER => BinaryOperator::Greater,
        SyntaxKind::LESS_EQ => BinaryOperator::LessEq,
        SyntaxKind::GREATER_EQ => BinaryOperator::GreaterEq,
        SyntaxKind::EQ2 => BinaryOperator::Equal,
        SyntaxKind::BANG_EQ => BinaryOperator::NotEqual,
        SyntaxKind::AMP => BinaryOperator::And,
        SyntaxKind::AMP2 => BinaryOperator::And2,
        SyntaxKind::PIPE => BinaryOperator::Or,
        SyntaxKind::PIPE2 => BinaryOperator::Or2,
        SyntaxKind::TILDE => BinaryOperator::Tilde,
        SyntaxKind::QUESTION => BinaryOperator::Help,
        _ => return None,
    };
    Some(operator)
}
