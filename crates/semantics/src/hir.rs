//! Spans are item-relative, so an edit elsewhere in the file reproduces an
//! equal `Module` and salsa cuts off before naming or inference rerun.

use rustc_hash::{FxHashMap, FxHasher};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU32;
use std::sync::{LazyLock, Mutex};
use std::{fmt, iter, ops};
use syntax::ast::{BinaryExpr, Name, is_expression_kind};
use syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct PatId(pub u32);

/// An identifier, interned process-wide: the same spelling is one allocation shared by every
/// item that mentions it, and a symbol is a four-byte index into that table, so an
/// `Option<Symbol>` costs no more than the symbol. The table only grows, which is bounded by the
/// distinct spellings a session has seen.
#[derive(Clone, Copy, salsa::SalsaValue)]
pub struct Symbol(NonZeroU32);

const SYMBOL_SHARDS: usize = 16;

/// Spelling to symbol, sharded by the spelling's hash so parallel lowering rarely contends.
static SYMBOL_IDS: LazyLock<[Mutex<FxHashMap<&'static str, Symbol>>; SYMBOL_SHARDS]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(FxHashMap::default())));

/// Symbol to spelling. Appending never moves an entry, so a read needs no lock.
static SYMBOL_TEXT: LazyLock<boxcar::Vec<&'static str>> = LazyLock::new(boxcar::Vec::new);

thread_local! {
    /// Spellings this thread has interned. Lowering interns every identifier it meets, and
    /// the shard lock's atomics bounce cache lines between analysis threads, so a repeat
    /// spelling skips the shared table.
    static LOCAL_SYMBOL_IDS: RefCell<FxHashMap<&'static str, Symbol>> =
        RefCell::new(FxHashMap::default());
}

impl Symbol {
    pub fn new(text: &str) -> Symbol {
        if let Some(symbol) = LOCAL_SYMBOL_IDS.with_borrow(|local| local.get(text).copied()) {
            return symbol;
        }
        let symbol = Symbol::new_shared(text);
        LOCAL_SYMBOL_IDS.with_borrow_mut(|local| local.insert(symbol.as_str(), symbol));
        symbol
    }

    fn new_shared(text: &str) -> Symbol {
        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        let shard = &SYMBOL_IDS[hasher.finish() as usize % SYMBOL_SHARDS];
        // A poisoned shard means another thread died mid-insert; the map is still a valid
        // map of leaked strings, so keep serving it.
        let mut shard = shard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(&symbol) = shard.get(text) {
            return symbol;
        }
        let interned: &'static str = Box::leak(text.into());
        // Pushed under the shard lock, so one spelling can never get two indexes.
        let index = SYMBOL_TEXT.push(interned);
        let symbol = Symbol(
            u32::try_from(index + 1)
                .ok()
                .and_then(NonZeroU32::new)
                .expect("fewer than 2^32 distinct identifiers"),
        );
        shard.insert(interned, symbol);
        symbol
    }

    pub fn as_str(self) -> &'static str {
        SYMBOL_TEXT
            .get(self.0.get() as usize - 1)
            .expect("a symbol indexes the table that minted it")
    }
}

impl ops::Deref for Symbol {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Symbol) -> bool {
        self.0 == other.0
    }
}

impl Eq for Symbol {}

impl PartialEq<str> for Symbol {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Symbol {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Symbol> for String {
    fn eq(&self, other: &Symbol) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Symbol> for &str {
    fn eq(&self, other: &Symbol) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Symbol> for str {
    fn eq(&self, other: &Symbol) -> bool {
        self == other.as_str()
    }
}

impl Default for Symbol {
    fn default() -> Symbol {
        Symbol::new("")
    }
}

impl PartialEq<String> for Symbol {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialOrd for Symbol {
    fn partial_cmp(&self, other: &Symbol) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Symbol {
    fn cmp(&self, other: &Symbol) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl std::borrow::Borrow<str> for Symbol {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Symbol {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for Symbol {
    fn from(text: &str) -> Symbol {
        Symbol::new(text)
    }
}

impl From<String> for Symbol {
    fn from(text: String) -> Symbol {
        Symbol::new(&text)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub expressions: Vec<Expression>,
    pub patterns: Vec<Pattern>,
    pub root: Option<ExprId>,
}

impl Module {
    pub fn expression(&self, id: ExprId) -> &Expression {
        &self.expressions[id.0 as usize]
    }

    pub fn pattern(&self, id: PatId) -> &Pattern {
        &self.patterns[id.0 as usize]
    }

    /// Whether a tag payload pattern is a bare `..`, as in `:Tag(..)`, which matches the tag
    /// with any payload or none.
    pub fn is_any_payload(&self, payload: PatId) -> bool {
        matches!(&self.pattern(payload).kind, PatternKind::Record { fields, rest: true } if fields.is_empty())
    }

    /// The names a pattern binds, as `NameRef` expressions, in source order.
    pub fn pattern_bindings(&self, pattern: PatId) -> Vec<ExprId> {
        let mut bindings = Vec::new();
        self.collect_pattern_bindings(pattern, &mut bindings);
        bindings
    }

    fn collect_pattern_bindings(&self, pattern: PatId, bindings: &mut Vec<ExprId>) {
        match &self.pattern(pattern).kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Literal { .. } => {}
            PatternKind::Bind(name) => bindings.push(*name),
            PatternKind::Tag { payload, .. } => {
                if let Some(payload) = payload {
                    self.collect_pattern_bindings(*payload, bindings);
                }
            }
            PatternKind::Tuple(items) => {
                for item in items {
                    self.collect_pattern_bindings(*item, bindings);
                }
            }
            PatternKind::Record { fields, .. } => {
                for field in fields {
                    self.collect_pattern_bindings(field.pattern, bindings);
                }
            }
            // Every alternative binds the same names (naming reports it otherwise), and each
            // alternative's leaf is its own write site, so all of them are listed.
            PatternKind::Or(alternatives) => {
                for alternative in alternatives {
                    self.collect_pattern_bindings(*alternative, bindings);
                }
            }
        }
    }
}

/// The names a zeallot pattern binds, in order; `None` unless the pattern is `c()` or `list()`
/// of names and nested patterns (a computed element makes it an ordinary operator call).
pub fn destructure_leaves(module: &Module, pattern: ExprId) -> Option<Vec<ExprId>> {
    fn collect(module: &Module, pattern: ExprId, leaves: &mut Vec<ExprId>) -> Option<()> {
        let ExpressionKind::Call { callee, arguments } = &module.expression(pattern).kind else {
            return None;
        };
        let ExpressionKind::NameRef(name) = &module.expression(*callee).kind else {
            return None;
        };
        if name != "c" && name != "list" {
            return None;
        }
        for argument in arguments {
            let value = argument.value?;
            match module.expression(value).kind {
                ExpressionKind::NameRef(_) => leaves.push(value),
                ExpressionKind::Call { .. } => collect(module, value, leaves)?,
                _ => return None,
            }
        }
        Some(())
    }
    let mut leaves = Vec::new();
    collect(module, pattern, &mut leaves)?;
    Some(leaves)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignSpelling {
    /// `<-` and `->`: right assignment is normalized into left form.
    Local,
    Equals,
    Super,
    /// zeallot's `c(a, b) %<-% value` (and `value %->% c(a, b)`): binds every name in the
    /// `c()` or `list()` pattern, so the target is the pattern call, not a single name.
    Destructure,
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
    /// Any `%…%` other than `%%` and `%/%`, including user-defined ones.
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
    /// Source text is kept for literal index positions and the literal-as-integer courtesy.
    Integer(Box<str>),
    Double(Box<str>),
    Complex(Box<str>),
    String(Box<str>),
    Logical(bool),
    Null,
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
    pub name: Option<Symbol>,
    pub name_range: Option<TextRange>,
    /// `None` is a positional hole (`f(, x)`) or an empty tagged value.
    pub value: Option<ExprId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: Symbol,
    pub default: Option<ExprId>,
    /// The name's range, excluding the default.
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionKind {
    /// A broken region: types as Unknown and reports nothing of its own.
    Missing,
    Literal(LiteralKind),
    NameRef(Symbol),
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
        /// Set only for `Special`: R evaluates `a %op% b` as a call to `%op%`,
        /// so naming needs the name to keep a user-defined operator alive.
        special_name: Option<Symbol>,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        arguments: Box<[Argument]>,
    },
    Index {
        double: bool,
        target: ExprId,
        arguments: Box<[Argument]>,
    },
    Field {
        at: bool,
        target: ExprId,
        name: Option<Symbol>,
        name_range: Option<TextRange>,
    },
    Namespace {
        internal: bool,
        package: Option<Symbol>,
        name: Option<Symbol>,
    },
    Function {
        parameters: Box<[Parameter]>,
        body: ExprId,
    },
    If {
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    For {
        variable: Option<Symbol>,
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
    /// Recognized by the bare callee name, like `return` and `stop`; rebinding
    /// `local` is not modeled.
    Local {
        body: ExprId,
    },
    Block {
        statements: Box<[ExprId]>,
    },
    Paren(ExprId),
    Break,
    Next,
    /// `(a, b)` and `(a,)`: a native tuple.
    Tuple(Box<[ExprId]>),
    /// `(x = 1, y = 2)`: a native record, fields in source order.
    Record(Box<[RecordField]>),
    /// `[a, b]`: an Array literal.
    Array(Box<[ExprId]>),
    /// `:Name` or `:Name(...)`. The payload is a `Tuple` or `Record` expression spanning the
    /// parenthesized list, so a payload types exactly like the matching literal.
    Tag {
        name: Symbol,
        name_range: TextRange,
        payload: Option<ExprId>,
    },
    Match {
        scrutinee: ExprId,
        arms: Box<[MatchArm]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    pub name: Symbol,
    pub name_range: TextRange,
    pub value: ExprId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: PatId,
    /// The pattern's `Bind` leaves in source order, kept beside the pattern so `child_ids`
    /// can list them without the pattern arena. Lowering fills both at once and neither
    /// changes afterwards.
    pub bindings: Box<[ExprId]>,
    pub guard: Option<ExprId>,
    pub body: ExprId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternKind {
    /// A broken pattern: matches anything and reports nothing of its own.
    Missing,
    Wildcard,
    /// The name is a `NameRef` expression, a write site exactly like an assignment target.
    Bind(ExprId),
    Literal {
        literal: LiteralKind,
        negative: bool,
    },
    /// The payload is a `Tuple` or `Record` pattern spanning the parenthesized list.
    Tag {
        name: Symbol,
        name_range: TextRange,
        payload: Option<PatId>,
    },
    Tuple(Box<[PatId]>),
    /// Without `rest`, the pattern must list every field of the record it matches.
    Record {
        fields: Box<[PatternField]>,
        rest: bool,
    },
    Or(Box<[PatId]>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternField {
    pub name: Symbol,
    pub name_range: TextRange,
    pub pattern: PatId,
}

impl ExpressionKind {
    /// Source order.
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
            ExpressionKind::Block { statements, .. } => statements.to_vec(),
            ExpressionKind::Paren(inner) => vec![*inner],
            ExpressionKind::Tuple(items) | ExpressionKind::Array(items) => items.to_vec(),
            ExpressionKind::Record(fields) => fields.iter().map(|field| field.value).collect(),
            ExpressionKind::Tag { payload, .. } => payload.iter().copied().collect(),
            ExpressionKind::Match { scrutinee, arms } => {
                let mut ids = vec![*scrutinee];
                for arm in arms {
                    ids.extend(arm.bindings.iter().copied());
                    ids.extend(arm.guard);
                    ids.push(arm.body);
                }
                ids
            }
        }
    }
}

pub fn lower_item(root: &SyntaxNode) -> Module {
    let mut module = Module::default();
    let root_id = module.lower_expression(root);
    module.root = Some(root_id);
    // The arena is kept for the life of the project, so the doubling slack is not free.
    module.expressions.shrink_to_fit();
    module.patterns.shrink_to_fit();
    module
}

impl Module {
    fn allocate(&mut self, kind: ExpressionKind, range: TextRange) -> ExprId {
        let id = ExprId(self.expressions.len() as u32);
        self.expressions.push(Expression { kind, range });
        id
    }

    fn missing(&mut self, range: TextRange) -> ExprId {
        self.allocate(ExpressionKind::Missing, range)
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
                let text = Name::cast(node.clone())
                    .and_then(|name| name.text())
                    .unwrap_or_default();
                self.allocate(ExpressionKind::NameRef(Symbol::new(&text)), range)
            }
            SyntaxKind::LITERAL => {
                let kind = literal_kind(node);
                self.allocate(ExpressionKind::Literal(kind), range)
            }
            // `@Name(value)` is its value in parentheses once the nominal head is erased;
            // the head is an annotation the checker applies to this expression.
            SyntaxKind::PAREN_EXPR | SyntaxKind::NEW_EXPR => {
                let inner = expression_children(node).next();
                let inner = self.lower_optional(inner, range);
                self.allocate(ExpressionKind::Paren(inner), range)
            }
            SyntaxKind::BRACE_EXPR => {
                let statements = expression_children(node)
                    .map(|child| self.lower_expression(&child))
                    .collect::<Box<[ExprId]>>();
                // A trailing `;` adds no expression in R's grammar, so
                // `{ 1L; }` is `1L` and needs no flag here.
                self.allocate(ExpressionKind::Block { statements }, range)
            }
            SyntaxKind::UNARY_EXPR => {
                let operator = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .find_map(|token| match token.kind() {
                        SyntaxKind::MINUS => Some(UnaryOperator::Minus),
                        SyntaxKind::PLUS => Some(UnaryOperator::Plus),
                        SyntaxKind::BANG => Some(UnaryOperator::Not),
                        SyntaxKind::TILDE => Some(UnaryOperator::Tilde),
                        SyntaxKind::QUESTION => Some(UnaryOperator::Help),
                        _ => None,
                    });
                let operand = expression_children(node).next();
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
                let callee_node = expression_children(node).next();
                let is_local = callee_node.as_ref().is_some_and(|callee| {
                    callee.kind() == SyntaxKind::NAME
                        && Name::cast(callee.clone())
                            .and_then(|name| name.text())
                            .is_some_and(|text| text == "local")
                });
                let arguments = self.lower_arguments(node, None);
                if is_local
                    && let [
                        Argument {
                            name: None,
                            value: Some(body),
                            ..
                        },
                    ] = &*arguments
                {
                    return self.allocate(ExpressionKind::Local { body: *body }, range);
                }
                let callee = self.lower_optional(callee_node, range);
                // R evaluates a string call head by looking up the function of that name, so
                // `"%in%.integer64"(x, y)` calls exactly what the bare name would.
                if let Some(callee_expression) = self.expressions.get_mut(callee.0 as usize)
                    && let ExpressionKind::Literal(LiteralKind::String(name)) =
                        &callee_expression.kind
                {
                    callee_expression.kind = ExpressionKind::NameRef(Symbol::new(name));
                }
                self.allocate(
                    ExpressionKind::Call {
                        callee,
                        arguments: arguments.into(),
                    },
                    range,
                )
            }
            SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                let double = node.kind() == SyntaxKind::SUBSET2_EXPR;
                let target = expression_children(node).next();
                let target = self.lower_optional(target, range);
                let arguments = self.lower_arguments(node, None);
                self.allocate(
                    ExpressionKind::Index {
                        double,
                        target,
                        arguments: arguments.into(),
                    },
                    range,
                )
            }
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => {
                let at = node.kind() == SyntaxKind::AT_EXPR;
                let mut children = expression_children(node);
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
                let mut children = expression_children(node);
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
                let body = expression_children(node).next();
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::Function { parameters, body }, range)
            }
            SyntaxKind::IF_EXPR => {
                let mut children = expression_children(node);
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
                let mut children = expression_children(node);
                let variable_node = children.next();
                let (variable, variable_range) = match &variable_node {
                    Some(node) if node.kind() == SyntaxKind::NAME => (
                        Name::cast(node.clone())
                            .and_then(|name| name.text())
                            .map(Symbol::from),
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
                let mut children = expression_children(node);
                let condition = children.next();
                let body = children.next();
                let condition = self.lower_optional(condition, range);
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::While { condition, body }, range)
            }
            SyntaxKind::REPEAT_EXPR => {
                let body = expression_children(node).next();
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::Repeat { body }, range)
            }
            SyntaxKind::BREAK_EXPR => self.allocate(ExpressionKind::Break, range),
            SyntaxKind::NEXT_EXPR => self.allocate(ExpressionKind::Next, range),
            SyntaxKind::TUPLE_EXPR | SyntaxKind::RECORD_EXPR => match node
                .children()
                .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
            {
                Some(list) => self.lower_items(&list, range),
                None => self.missing(range),
            },
            SyntaxKind::ARRAY_EXPR => {
                let items = match node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
                {
                    Some(list) => self
                        .lower_arguments_of(&list, None)
                        .into_iter()
                        .filter_map(|argument| argument.value)
                        .collect(),
                    None => Box::default(),
                };
                self.allocate(ExpressionKind::Array(items), range)
            }
            SyntaxKind::TAG_EXPR => {
                let Some(name_node) = node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::NAME)
                else {
                    return self.missing(range);
                };
                let name = field_name_text(&name_node).unwrap_or_default();
                let payload = node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
                    .map(|list| self.lower_items(&list, list.text_range()));
                self.allocate(
                    ExpressionKind::Tag {
                        name,
                        name_range: name_node.text_range(),
                        payload,
                    },
                    range,
                )
            }
            SyntaxKind::MATCH_EXPR => {
                let scrutinee = expression_children(node).next();
                let scrutinee = self.lower_optional(scrutinee, range);
                let arms = node
                    .children()
                    .filter(|child| child.kind() == SyntaxKind::MATCH_ARM)
                    .map(|arm| self.lower_arm(&arm))
                    .collect();
                self.allocate(ExpressionKind::Match { scrutinee, arms }, range)
            }
            _ => self.missing(range),
        }
    }

    /// The items of a native tuple or record literal. The parser already rejects a list that
    /// mixes named and unnamed items, so the first item decides which one this is.
    fn lower_items(&mut self, list: &SyntaxNode, range: TextRange) -> ExprId {
        let arguments = self.lower_arguments_of(list, None);
        let is_record = arguments
            .first()
            .is_some_and(|argument| argument.name.is_some());
        if is_record {
            let mut fields = Vec::with_capacity(arguments.len());
            for argument in arguments {
                let (Some(name), Some(name_range)) = (argument.name, argument.name_range) else {
                    continue;
                };
                let value = match argument.value {
                    Some(value) => value,
                    None => self.missing(name_range),
                };
                fields.push(RecordField {
                    name,
                    name_range,
                    value,
                });
            }
            return self.allocate(ExpressionKind::Record(fields.into()), range);
        }
        let items = arguments
            .into_iter()
            .filter_map(|argument| argument.value)
            .collect();
        self.allocate(ExpressionKind::Tuple(items), range)
    }

    fn lower_arm(&mut self, arm: &SyntaxNode) -> MatchArm {
        let range = significant_range(arm);
        let pattern = match arm.children().find(|child| is_pattern_kind(child.kind())) {
            Some(node) => self.lower_pattern(&node),
            None => self.allocate_pattern(PatternKind::Missing, range),
        };
        let bindings = self.pattern_bindings(pattern).into();
        let guard = arm
            .children()
            .find(|child| child.kind() == SyntaxKind::MATCH_GUARD)
            .map(|guard| {
                let condition = expression_children(&guard).next();
                self.lower_optional(condition, significant_range(&guard))
            });
        let body = expression_children(arm).next();
        let body = self.lower_optional(body, range);
        MatchArm {
            pattern,
            bindings,
            guard,
            body,
        }
    }

    fn allocate_pattern(&mut self, kind: PatternKind, range: TextRange) -> PatId {
        let id = PatId(self.patterns.len() as u32);
        self.patterns.push(Pattern { kind, range });
        id
    }

    fn lower_pattern(&mut self, node: &SyntaxNode) -> PatId {
        let range = significant_range(node);
        let kind = match node.kind() {
            SyntaxKind::PATTERN_WILDCARD => PatternKind::Wildcard,
            SyntaxKind::PATTERN_BIND => {
                let name = node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::NAME)
                    .and_then(|name| field_name_text(&name))
                    .unwrap_or_default();
                PatternKind::Bind(self.allocate(ExpressionKind::NameRef(name), range))
            }
            SyntaxKind::PATTERN_LITERAL => {
                let negative = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| token.kind() == SyntaxKind::MINUS);
                match node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::LITERAL)
                {
                    Some(literal) => PatternKind::Literal {
                        literal: literal_kind(&literal),
                        negative,
                    },
                    None => PatternKind::Missing,
                }
            }
            SyntaxKind::PATTERN_TAG => {
                match node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::NAME)
                {
                    Some(name_node) => {
                        let payload = node
                            .children()
                            .find(|child| child.kind() == SyntaxKind::PATTERN_LIST)
                            .map(|list| self.lower_pattern(&list));
                        PatternKind::Tag {
                            name: field_name_text(&name_node).unwrap_or_default(),
                            name_range: name_node.text_range(),
                            payload,
                        }
                    }
                    None => PatternKind::Missing,
                }
            }
            SyntaxKind::PATTERN_LIST => {
                let is_record = node.children().any(|child| {
                    matches!(
                        child.kind(),
                        SyntaxKind::PATTERN_FIELD | SyntaxKind::PATTERN_REST
                    )
                });
                if is_record {
                    let rest = node
                        .children()
                        .any(|child| child.kind() == SyntaxKind::PATTERN_REST);
                    let mut fields = Vec::new();
                    for field in node
                        .children()
                        .filter(|child| child.kind() == SyntaxKind::PATTERN_FIELD)
                    {
                        let Some(name_node) = field
                            .children()
                            .find(|child| child.kind() == SyntaxKind::NAME)
                        else {
                            continue;
                        };
                        let Some(name) = field_name_text(&name_node) else {
                            continue;
                        };
                        let pattern = match field
                            .children()
                            .find(|child| is_pattern_kind(child.kind()))
                        {
                            Some(pattern) => self.lower_pattern(&pattern),
                            None => self
                                .allocate_pattern(PatternKind::Missing, significant_range(&field)),
                        };
                        fields.push(PatternField {
                            name,
                            name_range: name_node.text_range(),
                            pattern,
                        });
                    }
                    PatternKind::Record {
                        fields: fields.into(),
                        rest,
                    }
                } else {
                    let items = node
                        .children()
                        .filter(|child| is_pattern_kind(child.kind()))
                        .map(|item| self.lower_pattern(&item))
                        .collect();
                    PatternKind::Tuple(items)
                }
            }
            SyntaxKind::PATTERN_OR => {
                let alternatives = node
                    .children()
                    .filter(|child| is_pattern_kind(child.kind()))
                    .map(|alternative| self.lower_pattern(&alternative))
                    .collect();
                PatternKind::Or(alternatives)
            }
            SyntaxKind::PATTERN_PAREN => {
                return match node.children().find(|child| is_pattern_kind(child.kind())) {
                    Some(inner) => self.lower_pattern(&inner),
                    None => self.allocate_pattern(PatternKind::Missing, range),
                };
            }
            _ => PatternKind::Missing,
        };
        self.allocate_pattern(kind, range)
    }

    fn lower_binary(&mut self, node: &SyntaxNode, range: TextRange) -> ExprId {
        let Some(binary) = BinaryExpr::cast(node.clone()) else {
            return self.missing(range);
        };
        let Some(operator_token) = binary.operator() else {
            return self.missing(range);
        };
        let (lhs, rhs) = (binary.lhs(), binary.rhs());
        let operator = match operator_token.kind() {
            kind @ (SyntaxKind::LESS_MINUS
            | SyntaxKind::EQ
            | SyntaxKind::LESS2_MINUS
            | SyntaxKind::MINUS_GREATER
            | SyntaxKind::MINUS_GREATER2) => {
                let spelling = match kind {
                    SyntaxKind::LESS_MINUS | SyntaxKind::MINUS_GREATER => AssignSpelling::Local,
                    SyntaxKind::EQ => AssignSpelling::Equals,
                    _ => AssignSpelling::Super,
                };
                let lhs = self.lower_optional(lhs, range);
                let rhs = self.lower_optional(rhs, range);
                let (target, value) =
                    if matches!(kind, SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2) {
                        (rhs, lhs)
                    } else {
                        (lhs, rhs)
                    };
                return self.allocate(
                    ExpressionKind::Assign {
                        spelling,
                        target,
                        value,
                    },
                    range,
                );
            }
            // `:=` binds nothing in R: it is a call to a function named `:=` (data.table,
            // rlang). The callee sits on the operator token so an unresolved `:=` reports there.
            SyntaxKind::COLON_EQ => {
                let callee = self.allocate(
                    ExpressionKind::NameRef(Symbol::new(":=")),
                    operator_token.text_range(),
                );
                let arguments = [lhs, rhs]
                    .into_iter()
                    .map(|operand| Argument {
                        name: None,
                        name_range: None,
                        value: Some(self.lower_optional(operand, range)),
                    })
                    .collect();
                return self.allocate(ExpressionKind::Call { callee, arguments }, range);
            }
            SyntaxKind::PIPE_GREATER => {
                if let Some(call) = self.lower_pipe(lhs.clone(), rhs.clone(), range) {
                    return call;
                }
                BinaryOperator::Pipe
            }
            SyntaxKind::SPECIAL => match operator_token.text() {
                "%%" => BinaryOperator::Modulo,
                "%/%" => BinaryOperator::IntegerDivide,
                _ => BinaryOperator::Special,
            },
            SyntaxKind::PLUS => BinaryOperator::Add,
            SyntaxKind::MINUS => BinaryOperator::Subtract,
            SyntaxKind::STAR => BinaryOperator::Multiply,
            SyntaxKind::SLASH => BinaryOperator::Divide,
            SyntaxKind::CARET => BinaryOperator::Power,
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
            _ => return self.missing(range),
        };
        let lhs = self.lower_optional(lhs, range);
        let rhs = self.lower_optional(rhs, range);
        if operator == BinaryOperator::Special {
            let (pattern, value) = match operator_token.text() {
                "%<-%" => (lhs, rhs),
                "%->%" => (rhs, lhs),
                _ => (lhs, lhs),
            };
            if pattern != value && destructure_leaves(self, pattern).is_some() {
                return self.allocate(
                    ExpressionKind::Assign {
                        spelling: AssignSpelling::Destructure,
                        target: pattern,
                        value,
                    },
                    range,
                );
            }
        }
        let special_name =
            (operator == BinaryOperator::Special).then(|| Symbol::new(operator_token.text()));
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

    /// R's parser rewrites `x |> f(y)` to `f(x, y)`, so the pipe lowers as that call.
    /// Shapes R rejects return `None` and keep the opaque-operator lowering.
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
        let mut arguments = self.lower_arguments(&call, substitute);
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
        Some(self.allocate(
            ExpressionKind::Call {
                callee,
                arguments: arguments.into(),
            },
            range,
        ))
    }

    /// The `_` placeholder never becomes an expression, so nothing dangles in the arena.
    fn lower_arguments(
        &mut self,
        node: &SyntaxNode,
        substitute: Option<(usize, ExprId)>,
    ) -> Vec<Argument> {
        match node
            .children()
            .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
        {
            Some(list) => self.lower_arguments_of(&list, substitute),
            None => Vec::new(),
        }
    }

    fn lower_arguments_of(
        &mut self,
        list: &SyntaxNode,
        substitute: Option<(usize, ExprId)>,
    ) -> Vec<Argument> {
        list.children()
            .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
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
                // R's grammar also takes `NULL` as an argument tag: `switch(x, NULL = 1)`
                // labels the case for the string "NULL".
                let name = tag.as_ref().and_then(|tag| {
                    let null_tag = tag.kind() == SyntaxKind::LITERAL
                        && tag
                            .first_token()
                            .is_some_and(|token| token.kind() == SyntaxKind::NULL_KW);
                    if null_tag {
                        Some(Symbol::new("NULL"))
                    } else {
                        field_name_text(tag)
                    }
                });
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
                    let mut expressions = expression_children(&argument);
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

    fn lower_parameters(&mut self, list: &SyntaxNode) -> Box<[Parameter]> {
        list.children()
            .filter(|child| child.kind() == SyntaxKind::PARAMETER)
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
                    .map(Symbol::from)
                    .unwrap_or_default();
                let default = expression_children(&parameter)
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

fn is_pattern_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PATTERN_WILDCARD
            | SyntaxKind::PATTERN_BIND
            | SyntaxKind::PATTERN_LITERAL
            | SyntaxKind::PATTERN_TAG
            | SyntaxKind::PATTERN_LIST
            | SyntaxKind::PATTERN_OR
            | SyntaxKind::PATTERN_PAREN
    )
}

fn expression_children(node: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
    node.children()
        .filter(|child| is_expression_kind(child.kind()))
}

/// Excludes the trailing trivia (including `#:` annotations) rowan attaches
/// inside expression nodes, so ranges cover code only.
pub fn significant_range(node: &SyntaxNode) -> TextRange {
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
        SyntaxKind::INTEGER => LiteralKind::Integer(token.text().into()),
        SyntaxKind::DOUBLE => LiteralKind::Double(token.text().into()),
        SyntaxKind::COMPLEX => LiteralKind::Complex(token.text().into()),
        SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
            LiteralKind::String(string_value(token.text()).into())
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

pub fn is_whole_number_double(text: &str) -> bool {
    text.parse::<f64>()
        .is_ok_and(|value| value.fract() == 0.0 && value.is_finite())
}

/// 0-based. Whole-number doubles count: R indexes `x[[2]]` exactly like `x[[2L]]`.
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

/// Escapes are not decoded; raw strings strip their full delimiters.
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
            closer.extend(iter::repeat_n('-', dashes));
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

fn field_name_text(node: &SyntaxNode) -> Option<Symbol> {
    match node.kind() {
        SyntaxKind::NAME => Name::cast(node.clone())
            .and_then(|name| name.text())
            .map(Symbol::from),
        SyntaxKind::LITERAL => {
            let token = node.first_token()?;
            (token.kind() == SyntaxKind::STRING).then(|| Symbol::new(&string_value(token.text())))
        }
        _ => None,
    }
}

enum PipeShape {
    FirstArgument,
    NamedPlaceholder(usize),
}

/// R's placeholder rules: `_` may appear exactly once, as the whole value of a
/// named argument of this call. Any other use is an R error (`None`).
fn pipe_shape(call: &SyntaxNode) -> Option<PipeShape> {
    let underscores: Vec<SyntaxToken> = call
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
    // With a tag present, the value is the second expression child.
    let is_value = expression_children(&argument).nth(1).as_ref() == Some(&name);
    if !named || !is_value {
        return None;
    }
    let index = list
        .children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .position(|candidate| candidate == argument)?;
    Some(PipeShape::NamedPlaceholder(index))
}
