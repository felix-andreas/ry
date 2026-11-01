//! Spans are item-relative, so an edit elsewhere in the file reproduces an
//! equal `Module` and salsa cuts off before naming or inference rerun.

use {
    rustc_hash::{FxHashMap, FxHasher},
    std::{
        cell::{Cell, RefCell},
        fmt,
        hash::{Hash, Hasher},
        marker::PhantomData,
        num::NonZeroU32,
        ops,
        sync::{LazyLock, Mutex},
    },
    syntax::{
        GreenElementAt, GreenNodeAt, SyntaxKind, SyntaxNode, TextRange, TextSize,
        ast::{is_expression_kind, name_text},
    },
    thin_vec::ThinVec,
};

/// An expression's index in its [`Module`]. It stores the index plus one, so an
/// `Option<ExprId>` is four bytes like the id itself; that keeps every `ExpressionKind` at
/// sixteen bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ExprId(NonZeroU32);

/// A pattern's index in its [`Module`], stored like [`ExprId`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct PatId(NonZeroU32);

/// A name written at a site, such as the `b` of `a$b`, an argument's tag or a `for`
/// variable, stored like [`ExprId`] and resolved with `module[id]`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NameId(NonZeroU32);

macro_rules! dense_id {
    ($id:ident) => {
        impl $id {
            pub fn new(index: usize) -> $id {
                // Spans are `u32` offsets into the item's text, so no module comes near
                // `u32::MAX` entries; past it ids would alias, which is worse than stopping.
                u32::try_from(index)
                    .ok()
                    .and_then(|index| NonZeroU32::MIN.checked_add(index))
                    .map($id)
                    .expect("an item holds fewer than u32::MAX HIR entries")
            }

            pub fn index(self) -> usize {
                (self.0.get() - 1) as usize
            }
        }

        impl fmt::Debug for $id {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", stringify!($id), self.index())
            }
        }
    };
}

dense_id!(ExprId);
dense_id!(PatId);
dense_id!(NameId);

/// The spelling and range of a name written at a site; see [`NameId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameSite {
    pub name: Symbol,
    pub range: TextRange,
}

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

    /// The symbol for `text` if any item has interned it, without interning it: a spelling
    /// nothing has interned cannot key a map of symbols, and a probe must not grow the
    /// process-wide table.
    pub fn lookup(text: &str) -> Option<Symbol> {
        if let Some(symbol) = LOCAL_SYMBOL_IDS.with_borrow(|local| local.get(text).copied()) {
            return Some(symbol);
        }
        symbol_shard(text)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(text)
            .copied()
    }

    fn new_shared(text: &str) -> Symbol {
        let shard = symbol_shard(text);
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

    /// The symbol's position in the table: stable for the process, but it depends on the
    /// order in which threads first met each spelling.
    pub fn index(self) -> u32 {
        self.0.get()
    }

    pub fn as_str(self) -> &'static str {
        SYMBOL_TEXT
            .get(self.0.get() as usize - 1)
            .expect("a symbol indexes the table that minted it")
    }
}

fn symbol_shard(text: &str) -> &'static Mutex<FxHashMap<&'static str, Symbol>> {
    let mut hasher = FxHasher::default();
    text.hash(&mut hasher);
    &SYMBOL_IDS[hasher.finish() as usize % SYMBOL_SHARDS]
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

// Hashing the index keeps a lookup off the spelling's bytes. It makes hash-map iteration
// order depend on interning order, which varies between runs, so anything that reports in
// map order must sort first; `Ord` stays by spelling for exactly that.
impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
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

/// One item's expressions and patterns as dense arrays. An expression is an `ExprId` index
/// into `kinds` and `ranges`, which are split because inference walks kinds and only
/// diagnostics read ranges. Every variable-length child list lives in one side array per
/// element type and an expression holds a [`Span`] into it, so lowering an item makes a
/// handful of allocations rather than one per list, and every kind is `Copy`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    kinds: ThinVec<ExpressionKind>,
    ranges: ThinVec<TextRange>,
    patterns: ThinVec<Pattern>,
    arguments: ThinVec<Argument>,
    parameters: ThinVec<Parameter>,
    expression_lists: ThinVec<ExprId>,
    record_fields: ThinVec<RecordField>,
    arms: ThinVec<MatchArm>,
    pattern_lists: ThinVec<PatId>,
    pattern_fields: ThinVec<PatternField>,
    name_sites: ThinVec<NameSite>,
    text: String,
    pub root: Option<ExprId>,
}

/// One expression of a [`Module`], as a scan over [`Module::expressions`] yields it.
#[derive(Debug, Clone, Copy)]
pub struct Expression<'a> {
    pub kind: &'a ExpressionKind,
    pub range: TextRange,
}

/// A run of a [`Module`] side array: `module[span]` is the slice.
pub struct Span<T> {
    start: u32,
    len: u32,
    element: PhantomData<fn() -> T>,
}

impl<T> Span<T> {
    pub fn len(self) -> usize {
        self.len as usize
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    fn range(self) -> ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

impl<T> Clone for Span<T> {
    fn clone(&self) -> Span<T> {
        *self
    }
}

impl<T> Copy for Span<T> {}

impl<T> PartialEq for Span<T> {
    fn eq(&self, other: &Span<T>) -> bool {
        (self.start, self.len) == (other.start, other.len)
    }
}

impl<T> Eq for Span<T> {}

impl<T> Hash for Span<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.start, self.len).hash(state);
    }
}

impl<T> Default for Span<T> {
    fn default() -> Span<T> {
        Span {
            start: 0,
            len: 0,
            element: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Span<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}..{}", self.start, self.start + self.len)
    }
}

/// Literal source text in the module's text buffer: `module[text]` is the string. Unlike a
/// [`Symbol`], which is interned for the life of the process, it is freed with the module, so
/// editing a string literal does not keep every spelling it passed through.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Text {
    start: u32,
    len: u32,
}

impl Text {
    fn range(self) -> ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "text {}..{}", self.start, self.start + self.len)
    }
}

impl ops::Index<NameId> for Module {
    type Output = NameSite;

    fn index(&self, id: NameId) -> &NameSite {
        &self.name_sites[id.index()]
    }
}

impl ops::Index<&NameId> for Module {
    type Output = NameSite;

    fn index(&self, id: &NameId) -> &NameSite {
        &self.name_sites[id.index()]
    }
}

impl ops::Index<Text> for Module {
    type Output = str;

    fn index(&self, text: Text) -> &str {
        &self.text[text.range()]
    }
}

impl ops::Index<&Text> for Module {
    type Output = str;

    fn index(&self, text: &Text) -> &str {
        &self.text[text.range()]
    }
}

macro_rules! side_array {
    ($element:ty, $field:ident) => {
        impl ops::Index<Span<$element>> for Module {
            type Output = [$element];

            fn index(&self, span: Span<$element>) -> &[$element] {
                &self.$field[span.range()]
            }
        }

        impl ops::Index<&Span<$element>> for Module {
            type Output = [$element];

            fn index(&self, span: &Span<$element>) -> &[$element] {
                &self.$field[span.range()]
            }
        }
    };
}

side_array!(Argument, arguments);
side_array!(Parameter, parameters);
side_array!(ExprId, expression_lists);
side_array!(RecordField, record_fields);
side_array!(MatchArm, arms);
side_array!(PatId, pattern_lists);
side_array!(PatternField, pattern_fields);

impl Module {
    fn clear(&mut self) {
        self.kinds.clear();
        self.ranges.clear();
        self.patterns.clear();
        self.arguments.clear();
        self.parameters.clear();
        self.expression_lists.clear();
        self.record_fields.clear();
        self.arms.clear();
        self.pattern_lists.clear();
        self.pattern_fields.clear();
        self.name_sites.clear();
        self.text.clear();
        self.root = None;
    }

    pub fn kind(&self, id: ExprId) -> &ExpressionKind {
        &self.kinds[id.index()]
    }

    pub fn expression(&self, id: ExprId) -> Expression<'_> {
        Expression {
            kind: self.kind(id),
            range: self.range(id),
        }
    }

    pub fn range(&self, id: ExprId) -> TextRange {
        self.ranges[id.index()]
    }

    /// Every expression's kind, indexed by `ExprId`.
    pub fn kinds(&self) -> &[ExpressionKind] {
        &self.kinds
    }

    /// Every expression's significant source range, indexed by `ExprId`.
    pub fn ranges(&self) -> &[TextRange] {
        &self.ranges
    }

    /// Every expression with its id order, for a scan that reads both kind and range.
    pub fn expressions(&self) -> impl ExactSizeIterator<Item = Expression<'_>> + Clone {
        self.kinds
            .iter()
            .zip(&self.ranges)
            .map(|(kind, &range)| Expression { kind, range })
    }

    pub fn name(&self, id: NameId) -> Symbol {
        self[id].name
    }

    pub fn name_range(&self, id: NameId) -> TextRange {
        self[id].range
    }

    pub fn argument_name(&self, argument: &Argument) -> Option<Symbol> {
        argument.name.map(|name| self[name].name)
    }

    pub fn argument_name_range(&self, argument: &Argument) -> Option<TextRange> {
        argument.name.map(|name| self[name].range)
    }

    pub fn expression_count(&self) -> usize {
        self.kinds.len()
    }

    pub fn expression_ids(&self) -> impl Iterator<Item = ExprId> + use<> {
        (0..self.kinds.len()).map(ExprId::new)
    }

    /// Every pattern, indexed by `PatId`.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    pub fn pattern(&self, id: PatId) -> &Pattern {
        &self.patterns[id.index()]
    }

    /// The direct child expressions of `id`, in source order.
    pub fn for_each_child(&self, id: ExprId, mut visit: impl FnMut(ExprId)) {
        match *self.kind(id) {
            ExpressionKind::Missing
            | ExpressionKind::Literal(_)
            | ExpressionKind::NameRef(_)
            | ExpressionKind::Namespace { .. }
            | ExpressionKind::Break
            | ExpressionKind::Next => {}
            ExpressionKind::Assign { target, value, .. } => {
                visit(target);
                visit(value);
            }
            ExpressionKind::Unary { operand, .. } => visit(operand),
            ExpressionKind::Binary { lhs, rhs, .. } => {
                visit(lhs);
                visit(rhs);
            }
            ExpressionKind::Call {
                callee: target,
                arguments,
            }
            | ExpressionKind::Index {
                target, arguments, ..
            } => {
                visit(target);
                self[arguments]
                    .iter()
                    .filter_map(|argument| argument.value)
                    .for_each(visit);
            }
            ExpressionKind::Field { target, .. } => visit(target),
            ExpressionKind::Function { parameters, body } => {
                self[parameters]
                    .iter()
                    .filter_map(|parameter| parameter.default)
                    .for_each(&mut visit);
                visit(body);
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                visit(condition);
                visit(then_branch);
                else_branch.into_iter().for_each(visit);
            }
            ExpressionKind::For { sequence, body, .. } => {
                visit(sequence);
                visit(body);
            }
            ExpressionKind::While { condition, body } => {
                visit(condition);
                visit(body);
            }
            ExpressionKind::Repeat { body } | ExpressionKind::Local { body } => visit(body),
            ExpressionKind::Block { statements: items }
            | ExpressionKind::Tuple(items)
            | ExpressionKind::Array(items) => self[items].iter().copied().for_each(visit),
            ExpressionKind::Paren(inner) => visit(inner),
            ExpressionKind::Record(fields) => {
                self[fields].iter().map(|field| field.value).for_each(visit)
            }
            ExpressionKind::Tag { payload, .. } => payload.into_iter().for_each(visit),
            ExpressionKind::Match { scrutinee, arms } => {
                visit(scrutinee);
                for arm in &self[arms] {
                    self[arm.bindings].iter().copied().for_each(&mut visit);
                    arm.guard.into_iter().for_each(&mut visit);
                    visit(arm.body);
                }
            }
        }
    }

    /// Whether any direct child of `id` satisfies `test`, stopping at the first that does.
    pub fn any_child(&self, id: ExprId, mut test: impl FnMut(ExprId) -> bool) -> bool {
        let mut found = false;
        self.for_each_child(id, |child| {
            if !found {
                found = test(child);
            }
        });
        found
    }

    /// Whether a tag payload pattern is a bare `..`, as in `:Tag(..)`, which matches the tag
    /// with any payload or none.
    pub fn is_any_payload(&self, payload: PatId) -> bool {
        matches!(self.pattern(payload).kind, PatternKind::Record { fields, rest: true } if fields.is_empty())
    }

    /// The names a pattern binds, as `NameRef` expressions, in source order.
    pub fn pattern_bindings(&self, pattern: PatId) -> Vec<ExprId> {
        let mut bindings = Vec::new();
        self.collect_pattern_bindings(pattern, &mut bindings);
        bindings
    }

    fn collect_pattern_bindings(&self, pattern: PatId, bindings: &mut Vec<ExprId>) {
        match self.pattern(pattern).kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Literal { .. } => {}
            PatternKind::Bind(name) => bindings.push(name),
            PatternKind::Tag { payload, .. } => {
                if let Some(payload) = payload {
                    self.collect_pattern_bindings(payload, bindings);
                }
            }
            // Every alternative binds the same names (naming reports it otherwise), and each
            // alternative's leaf is its own write site, so all of them are listed.
            PatternKind::Tuple(items) | PatternKind::Or(items) => {
                for &item in &self[items] {
                    self.collect_pattern_bindings(item, bindings);
                }
            }
            PatternKind::Record { fields, .. } => {
                for field in &self[fields] {
                    self.collect_pattern_bindings(field.pattern, bindings);
                }
            }
        }
    }
}

/// The names a zeallot pattern binds, in order; `None` unless the pattern is `c()` or `list()`
/// of names and nested patterns (a computed element makes it an ordinary operator call).
pub fn destructure_leaves(module: &Module, pattern: ExprId) -> Option<Vec<ExprId>> {
    fn collect(module: &Module, pattern: ExprId, leaves: &mut Vec<ExprId>) -> Option<()> {
        let ExpressionKind::Call { callee, arguments } = *module.kind(pattern) else {
            return None;
        };
        let ExpressionKind::NameRef(name) = module.kind(callee) else {
            return None;
        };
        if name != "c" && name != "list" {
            return None;
        }
        for argument in &module[arguments] {
            let value = argument.value?;
            match module.kind(value) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralKind {
    /// Source text is kept for literal index positions and the literal-as-integer courtesy.
    Integer(Text),
    Double(Text),
    Complex(Text),
    /// The value without its quotes; escapes are not decoded.
    String(Text),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argument {
    pub name: Option<NameId>,
    /// `None` is a positional hole (`f(, x)`) or an empty tagged value.
    pub value: Option<ExprId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub name: Symbol,
    pub default: Option<ExprId>,
    /// The name's range, excluding the default.
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        arguments: Span<Argument>,
    },
    Index {
        double: bool,
        target: ExprId,
        arguments: Span<Argument>,
    },
    Field {
        at: bool,
        target: ExprId,
        name: Option<NameId>,
    },
    Namespace {
        internal: bool,
        package: Option<Symbol>,
        name: Option<Symbol>,
    },
    Function {
        parameters: Span<Parameter>,
        body: ExprId,
    },
    If {
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    For {
        variable: Option<NameId>,
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
        statements: Span<ExprId>,
    },
    Paren(ExprId),
    Break,
    Next,
    /// `(a, b)` and `(a,)`: a native tuple.
    Tuple(Span<ExprId>),
    /// `(x = 1, y = 2)`: a native record, fields in source order.
    Record(Span<RecordField>),
    /// `[a, b]`: an Array literal.
    Array(Span<ExprId>),
    /// `:Name` or `:Name(...)`. The payload is a `Tuple` or `Record` expression spanning the
    /// parenthesized list, so a payload types exactly like the matching literal.
    Tag {
        name: NameId,
        payload: Option<ExprId>,
    },
    Match {
        scrutinee: ExprId,
        arms: Span<MatchArm>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordField {
    pub name: Symbol,
    pub name_range: TextRange,
    pub value: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: PatId,
    /// The pattern's `Bind` leaves in source order, so a walk over expressions reaches them
    /// without reading patterns. Lowering fills both at once and neither changes afterwards.
    pub bindings: Span<ExprId>,
    pub guard: Option<ExprId>,
    pub body: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        name: NameId,
        payload: Option<PatId>,
    },
    Tuple(Span<PatId>),
    /// Without `rest`, the pattern must list every field of the record it matches.
    Record {
        fields: Span<PatternField>,
        rest: bool,
    },
    Or(Span<PatId>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternField {
    pub name: Symbol,
    pub name_range: TextRange,
    pub pattern: PatId,
}

pub fn lower_item(root: &SyntaxNode) -> Module {
    let green = root.green();
    let mut lowering = LOWERING.take();
    let root_id = lowering.lower_expression(GreenNodeAt::new(&green, root.text_range().start()));
    let scratch = &mut lowering.module;
    // The module lives as long as the project, so it gets exact-size copies; the scratch
    // arrays keep their capacity for the next item this thread lowers, which saves both the
    // doubling slack and the reallocation churn of growing fresh vectors per item.
    let module = Module {
        kinds: scratch.kinds.as_slice().into(),
        ranges: scratch.ranges.as_slice().into(),
        patterns: scratch.patterns.as_slice().into(),
        arguments: scratch.arguments.as_slice().into(),
        parameters: scratch.parameters.as_slice().into(),
        expression_lists: scratch.expression_lists.as_slice().into(),
        record_fields: scratch.record_fields.as_slice().into(),
        arms: scratch.arms.as_slice().into(),
        pattern_lists: scratch.pattern_lists.as_slice().into(),
        pattern_fields: scratch.pattern_fields.as_slice().into(),
        name_sites: scratch.name_sites.as_slice().into(),
        text: scratch.text.as_str().into(),
        root: Some(root_id),
    };
    scratch.clear();
    LOWERING.set(lowering);
    module
}

thread_local! {
    static LOWERING: Cell<Lowering> = Cell::default();
}

/// Lowering reads the green tree, so it builds no red nodes. A list's elements are lowered
/// before the list is complete, and lowering them appends their own lists first, so each list
/// gathers on a stack and moves into its side array once its last element is done. A nested
/// list is always finished and popped before the enclosing one takes its next element, which
/// keeps every list contiguous at the top of its stack.
#[derive(Default)]
struct Lowering {
    module: Module,
    expression_stack: Vec<ExprId>,
    argument_stack: Vec<Argument>,
    parameter_stack: Vec<Parameter>,
    field_stack: Vec<RecordField>,
    arm_stack: Vec<MatchArm>,
    pattern_stack: Vec<PatId>,
    pattern_field_stack: Vec<PatternField>,
}

/// Moves `stack[mark..]` to the end of `store`.
fn seal<T>(stack: &mut Vec<T>, mark: usize, store: &mut ThinVec<T>) -> Span<T> {
    let start = store.len() as u32;
    store.extend(stack.drain(mark..));
    Span {
        start,
        len: store.len() as u32 - start,
        element: PhantomData,
    }
}

impl Lowering {
    fn allocate(&mut self, kind: ExpressionKind, range: TextRange) -> ExprId {
        let id = ExprId::new(self.module.kinds.len());
        self.module.kinds.push(kind);
        self.module.ranges.push(range);
        id
    }

    fn name_site(&mut self, name: Symbol, range: TextRange) -> NameId {
        let id = NameId::new(self.module.name_sites.len());
        self.module.name_sites.push(NameSite { name, range });
        id
    }

    fn missing(&mut self, range: TextRange) -> ExprId {
        self.allocate(ExpressionKind::Missing, range)
    }

    fn lower_optional(&mut self, node: Option<GreenNodeAt<'_>>, fallback: TextRange) -> ExprId {
        match node {
            Some(node) => self.lower_expression(node),
            None => self.missing(fallback),
        }
    }

    fn lower_expression(&mut self, node: GreenNodeAt<'_>) -> ExprId {
        let range = significant_range_green(node);
        match node.kind() {
            SyntaxKind::NAME => {
                let name = name_symbol(node).unwrap_or_default();
                self.allocate(ExpressionKind::NameRef(name), range)
            }
            SyntaxKind::LITERAL => {
                let kind = literal_kind(node, &mut self.module.text);
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
                let mark = self.expression_stack.len();
                for child in expression_children(node) {
                    let statement = self.lower_expression(child);
                    self.expression_stack.push(statement);
                }
                let statements = seal(
                    &mut self.expression_stack,
                    mark,
                    &mut self.module.expression_lists,
                );
                // A trailing `;` adds no expression in R's grammar, so
                // `{ 1L; }` is `1L` and needs no flag here.
                self.allocate(ExpressionKind::Block { statements }, range)
            }
            SyntaxKind::UNARY_EXPR => {
                let operator = node.child_tokens().find_map(|token| match token.kind {
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
                let is_local = callee_node.is_some_and(|callee| {
                    callee.kind() == SyntaxKind::NAME
                        && name_symbol(callee).is_some_and(|name| name == "local")
                });
                let mark = self.argument_stack.len();
                self.lower_arguments(node, None);
                if is_local
                    && let [
                        Argument {
                            name: None,
                            value: Some(body),
                            ..
                        },
                    ] = self.argument_stack[mark..]
                {
                    self.argument_stack.truncate(mark);
                    return self.allocate(ExpressionKind::Local { body }, range);
                }
                let arguments = seal(&mut self.argument_stack, mark, &mut self.module.arguments);
                let callee = self.lower_optional(callee_node, range);
                // R evaluates a string call head by looking up the function of that name, so
                // `"%in%.integer64"(x, y)` calls exactly what the bare name would.
                if let Some(callee_kind) = self.module.kinds.get_mut(callee.index())
                    && let ExpressionKind::Literal(LiteralKind::String(name)) = *callee_kind
                {
                    *callee_kind =
                        ExpressionKind::NameRef(Symbol::new(&self.module.text[name.range()]));
                }
                self.allocate(ExpressionKind::Call { callee, arguments }, range)
            }
            SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                let double = node.kind() == SyntaxKind::SUBSET2_EXPR;
                let target = expression_children(node).next();
                let target = self.lower_optional(target, range);
                let mark = self.argument_stack.len();
                self.lower_arguments(node, None);
                let arguments = seal(&mut self.argument_stack, mark, &mut self.module.arguments);
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
                let mut children = expression_children(node);
                let target = children.next();
                let target = self.lower_optional(target, range);
                let name = children.next().and_then(|name_node| {
                    let name = field_name_text(name_node)?;
                    Some(self.name_site(name, name_node.range()))
                });
                self.allocate(ExpressionKind::Field { at, target, name }, range)
            }
            SyntaxKind::NAMESPACE_EXPR => {
                let internal = node
                    .child_tokens()
                    .any(|token| token.kind == SyntaxKind::COLON3);
                let mut children = expression_children(node);
                let package = children.next().and_then(field_name_text);
                let name = children.next().and_then(field_name_text);
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
                let mark = self.parameter_stack.len();
                if let Some(list) = child_of_kind(node, SyntaxKind::PARAMETER_LIST) {
                    self.lower_parameters(list);
                }
                let parameters = seal(&mut self.parameter_stack, mark, &mut self.module.parameters);
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
                let else_branch = else_branch.map(|node| self.lower_expression(node));
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
                let variable = match children.next() {
                    Some(variable) if variable.kind() == SyntaxKind::NAME => {
                        name_symbol(variable).map(|name| self.name_site(name, variable.range()))
                    }
                    _ => None,
                };
                let sequence = children.next();
                let body = children.next();
                let sequence = self.lower_optional(sequence, range);
                let body = self.lower_optional(body, range);
                self.allocate(
                    ExpressionKind::For {
                        variable,
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
            SyntaxKind::TUPLE_EXPR | SyntaxKind::RECORD_EXPR => {
                match child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                    Some(list) => self.lower_items(list, range),
                    None => self.missing(range),
                }
            }
            SyntaxKind::ARRAY_EXPR => {
                let items = match child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                    Some(list) => {
                        let mark = self.argument_stack.len();
                        self.lower_arguments_of(list, None);
                        let item_mark = self.expression_stack.len();
                        for argument in self.argument_stack.drain(mark..) {
                            self.expression_stack.extend(argument.value);
                        }
                        seal(
                            &mut self.expression_stack,
                            item_mark,
                            &mut self.module.expression_lists,
                        )
                    }
                    None => Span::default(),
                };
                self.allocate(ExpressionKind::Array(items), range)
            }
            SyntaxKind::TAG_EXPR => {
                let Some(name_node) = child_of_kind(node, SyntaxKind::NAME) else {
                    return self.missing(range);
                };
                let name = field_name_text(name_node).unwrap_or_default();
                let name = self.name_site(name, name_node.range());
                let payload = child_of_kind(node, SyntaxKind::ARGUMENT_LIST)
                    .map(|list| self.lower_items(list, list.range()));
                self.allocate(ExpressionKind::Tag { name, payload }, range)
            }
            SyntaxKind::MATCH_EXPR => {
                let scrutinee = expression_children(node).next();
                let scrutinee = self.lower_optional(scrutinee, range);
                let mark = self.arm_stack.len();
                for arm in node
                    .child_nodes()
                    .filter(|child| child.kind() == SyntaxKind::MATCH_ARM)
                {
                    let arm = self.lower_arm(arm);
                    self.arm_stack.push(arm);
                }
                let arms = seal(&mut self.arm_stack, mark, &mut self.module.arms);
                self.allocate(ExpressionKind::Match { scrutinee, arms }, range)
            }
            _ => self.missing(range),
        }
    }

    /// The items of a native tuple or record literal. The parser already rejects a list that
    /// mixes named and unnamed items, so the first item decides which one this is.
    fn lower_items(&mut self, list: GreenNodeAt<'_>, range: TextRange) -> ExprId {
        let mark = self.argument_stack.len();
        self.lower_arguments_of(list, None);
        let is_record = self
            .argument_stack
            .get(mark)
            .is_some_and(|argument| argument.name.is_some());
        if is_record {
            let field_mark = self.field_stack.len();
            for index in mark..self.argument_stack.len() {
                let argument = self.argument_stack[index];
                let Some(name) = argument.name else {
                    continue;
                };
                let NameSite {
                    name,
                    range: name_range,
                } = self.module[name];
                let value = match argument.value {
                    Some(value) => value,
                    None => self.missing(name_range),
                };
                self.field_stack.push(RecordField {
                    name,
                    name_range,
                    value,
                });
            }
            self.argument_stack.truncate(mark);
            let fields = seal(
                &mut self.field_stack,
                field_mark,
                &mut self.module.record_fields,
            );
            return self.allocate(ExpressionKind::Record(fields), range);
        }
        let item_mark = self.expression_stack.len();
        for argument in self.argument_stack.drain(mark..) {
            self.expression_stack.extend(argument.value);
        }
        let items = seal(
            &mut self.expression_stack,
            item_mark,
            &mut self.module.expression_lists,
        );
        self.allocate(ExpressionKind::Tuple(items), range)
    }

    fn lower_arm(&mut self, arm: GreenNodeAt<'_>) -> MatchArm {
        let range = significant_range_green(arm);
        let pattern = match arm
            .child_nodes()
            .find(|child| is_pattern_kind(child.kind()))
        {
            Some(node) => self.lower_pattern(node),
            None => self.allocate_pattern(PatternKind::Missing, range),
        };
        let bindings = self.module.pattern_bindings(pattern);
        let start = self.module.expression_lists.len() as u32;
        self.module.expression_lists.extend(bindings);
        let bindings = Span {
            start,
            len: self.module.expression_lists.len() as u32 - start,
            element: PhantomData,
        };
        let guard = child_of_kind(arm, SyntaxKind::MATCH_GUARD).map(|guard| {
            let condition = expression_children(guard).next();
            self.lower_optional(condition, significant_range_green(guard))
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
        let id = PatId::new(self.module.patterns.len());
        self.module.patterns.push(Pattern { kind, range });
        id
    }

    fn lower_pattern(&mut self, node: GreenNodeAt<'_>) -> PatId {
        let range = significant_range_green(node);
        let kind = match node.kind() {
            SyntaxKind::PATTERN_WILDCARD => PatternKind::Wildcard,
            SyntaxKind::PATTERN_BIND => {
                let name = child_of_kind(node, SyntaxKind::NAME)
                    .and_then(field_name_text)
                    .unwrap_or_default();
                PatternKind::Bind(self.allocate(ExpressionKind::NameRef(name), range))
            }
            SyntaxKind::PATTERN_LITERAL => {
                let negative = node
                    .child_tokens()
                    .any(|token| token.kind == SyntaxKind::MINUS);
                match child_of_kind(node, SyntaxKind::LITERAL) {
                    Some(literal) => PatternKind::Literal {
                        literal: literal_kind(literal, &mut self.module.text),
                        negative,
                    },
                    None => PatternKind::Missing,
                }
            }
            SyntaxKind::PATTERN_TAG => match child_of_kind(node, SyntaxKind::NAME) {
                Some(name_node) => {
                    let payload = child_of_kind(node, SyntaxKind::PATTERN_LIST)
                        .map(|list| self.lower_pattern(list));
                    let name = field_name_text(name_node).unwrap_or_default();
                    PatternKind::Tag {
                        name: self.name_site(name, name_node.range()),
                        payload,
                    }
                }
                None => PatternKind::Missing,
            },
            SyntaxKind::PATTERN_LIST => {
                let is_record = node.child_nodes().any(|child| {
                    matches!(
                        child.kind(),
                        SyntaxKind::PATTERN_FIELD | SyntaxKind::PATTERN_REST
                    )
                });
                if is_record {
                    let rest = child_of_kind(node, SyntaxKind::PATTERN_REST).is_some();
                    let mark = self.pattern_field_stack.len();
                    for field in node
                        .child_nodes()
                        .filter(|child| child.kind() == SyntaxKind::PATTERN_FIELD)
                    {
                        let Some(name_node) = child_of_kind(field, SyntaxKind::NAME) else {
                            continue;
                        };
                        let Some(name) = field_name_text(name_node) else {
                            continue;
                        };
                        let pattern = match field
                            .child_nodes()
                            .find(|child| is_pattern_kind(child.kind()))
                        {
                            Some(pattern) => self.lower_pattern(pattern),
                            None => self.allocate_pattern(
                                PatternKind::Missing,
                                significant_range_green(field),
                            ),
                        };
                        self.pattern_field_stack.push(PatternField {
                            name,
                            name_range: name_node.range(),
                            pattern,
                        });
                    }
                    let fields = seal(
                        &mut self.pattern_field_stack,
                        mark,
                        &mut self.module.pattern_fields,
                    );
                    PatternKind::Record { fields, rest }
                } else {
                    PatternKind::Tuple(self.lower_pattern_list(node))
                }
            }
            SyntaxKind::PATTERN_OR => PatternKind::Or(self.lower_pattern_list(node)),
            SyntaxKind::PATTERN_PAREN => {
                return match node
                    .child_nodes()
                    .find(|child| is_pattern_kind(child.kind()))
                {
                    Some(inner) => self.lower_pattern(inner),
                    None => self.allocate_pattern(PatternKind::Missing, range),
                };
            }
            _ => PatternKind::Missing,
        };
        self.allocate_pattern(kind, range)
    }

    fn lower_pattern_list(&mut self, node: GreenNodeAt<'_>) -> Span<PatId> {
        let mark = self.pattern_stack.len();
        for item in node
            .child_nodes()
            .filter(|child| is_pattern_kind(child.kind()))
        {
            let item = self.lower_pattern(item);
            self.pattern_stack.push(item);
        }
        seal(
            &mut self.pattern_stack,
            mark,
            &mut self.module.pattern_lists,
        )
    }

    fn lower_binary(&mut self, node: GreenNodeAt<'_>, range: TextRange) -> ExprId {
        let Some(operator_token) = node.child_tokens().find(|token| !token.kind.is_trivia()) else {
            return self.missing(range);
        };
        let mut operands = expression_children(node);
        let (lhs, rhs) = (operands.next(), operands.next());
        let operator = match operator_token.kind {
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
                    operator_token.range,
                );
                let mark = self.argument_stack.len();
                for operand in [lhs, rhs] {
                    let value = self.lower_optional(operand, range);
                    self.argument_stack.push(Argument {
                        name: None,
                        value: Some(value),
                    });
                }
                let arguments = seal(&mut self.argument_stack, mark, &mut self.module.arguments);
                return self.allocate(ExpressionKind::Call { callee, arguments }, range);
            }
            SyntaxKind::PIPE_GREATER => {
                if let Some(call) = self.lower_pipe(lhs, rhs, range) {
                    return call;
                }
                BinaryOperator::Pipe
            }
            SyntaxKind::SPECIAL => match operator_token.text {
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
            let (pattern, value) = match operator_token.text {
                "%<-%" => (lhs, rhs),
                "%->%" => (rhs, lhs),
                _ => (lhs, lhs),
            };
            if pattern != value && destructure_leaves(&self.module, pattern).is_some() {
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
            (operator == BinaryOperator::Special).then(|| Symbol::new(operator_token.text));
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
        lhs: Option<GreenNodeAt<'_>>,
        rhs: Option<GreenNodeAt<'_>>,
        range: TextRange,
    ) -> Option<ExprId> {
        let call = rhs.filter(|node| node.kind() == SyntaxKind::CALL_EXPR)?;
        let shape = pipe_shape(call)?;
        let piped = self.lower_optional(lhs, range);
        let callee = call
            .child_nodes()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST);
        let callee = self.lower_optional(callee, range);
        let mark = self.argument_stack.len();
        let substitute = match shape {
            PipeShape::FirstArgument => {
                self.argument_stack.push(Argument {
                    name: None,
                    value: Some(piped),
                });
                None
            }
            PipeShape::NamedPlaceholder(index) => Some((index, piped)),
        };
        self.lower_arguments(call, substitute);
        let arguments = seal(&mut self.argument_stack, mark, &mut self.module.arguments);
        Some(self.allocate(ExpressionKind::Call { callee, arguments }, range))
    }

    /// Pushes the arguments of `node`'s argument list onto the argument stack. The `_`
    /// placeholder never becomes an expression, so nothing dangles in the module.
    fn lower_arguments(&mut self, node: GreenNodeAt<'_>, substitute: Option<(usize, ExprId)>) {
        if let Some(list) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
            self.lower_arguments_of(list, substitute);
        }
    }

    fn lower_arguments_of(&mut self, list: GreenNodeAt<'_>, substitute: Option<(usize, ExprId)>) {
        for (index, argument) in list
            .child_nodes()
            .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
            .enumerate()
        {
            let tag = argument
                .child_nodes()
                .find(|child| {
                    child.kind() == SyntaxKind::NAME || child.kind() == SyntaxKind::LITERAL
                })
                .filter(|_| {
                    argument
                        .child_tokens()
                        .any(|token| token.kind == SyntaxKind::EQ)
                });
            // R's grammar also takes `NULL` as an argument tag: `switch(x, NULL = 1)`
            // labels the case for the string "NULL".
            let name = tag.and_then(|tag| {
                let null_tag = tag.kind() == SyntaxKind::LITERAL
                    && tag
                        .first_token()
                        .is_some_and(|token| token.kind == SyntaxKind::NULL_KW);
                let name = if null_tag {
                    Symbol::new("NULL")
                } else {
                    field_name_text(tag)?
                };
                Some(self.name_site(name, tag.range()))
            });
            let value = match substitute {
                Some((placeholder, piped)) if placeholder == index => Some(piped),
                _ => {
                    let mut expressions = expression_children(argument);
                    if name.is_some() {
                        // The tag itself is the first expression child.
                        expressions.next();
                    }
                    expressions.next().map(|node| self.lower_expression(node))
                }
            };
            self.argument_stack.push(Argument { name, value });
        }
    }

    fn lower_parameters(&mut self, list: GreenNodeAt<'_>) {
        for parameter in list
            .child_nodes()
            .filter(|child| child.kind() == SyntaxKind::PARAMETER)
        {
            let first = parameter.first_token();
            let name = first
                .map(|token| {
                    let text = token.text;
                    Symbol::new(
                        text.strip_prefix('`')
                            .and_then(|t| t.strip_suffix('`'))
                            .unwrap_or(text),
                    )
                })
                .unwrap_or_default();
            let default = expression_children(parameter)
                .next()
                .map(|node| self.lower_expression(node));
            let range = first.map_or_else(|| parameter.range(), |token| token.range);
            self.parameter_stack.push(Parameter {
                name,
                default,
                range,
            });
        }
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

fn expression_children<'a>(node: GreenNodeAt<'a>) -> impl Iterator<Item = GreenNodeAt<'a>> {
    node.child_nodes()
        .filter(|child| is_expression_kind(child.kind()))
}

fn child_of_kind(node: GreenNodeAt<'_>, kind: SyntaxKind) -> Option<GreenNodeAt<'_>> {
    node.child_nodes().find(|child| child.kind() == kind)
}

/// Excludes the trailing trivia (including `#:` annotations) rowan attaches
/// inside expression nodes, so ranges cover code only.
pub fn significant_range(node: &SyntaxNode) -> TextRange {
    let green = node.green();
    significant_range_green(GreenNodeAt::new(&green, node.text_range().start()))
}

fn significant_range_green(node: GreenNodeAt<'_>) -> TextRange {
    let start = node.range().start();
    TextRange::new(start, significant_end(node).unwrap_or(start))
}

/// Where the last token that is neither trivia nor inside an `ANNOTATION` ends.
fn significant_end(node: GreenNodeAt<'_>) -> Option<TextSize> {
    node.children().rev().find_map(|child| match child {
        GreenElementAt::Token(token) => (!token.kind.is_trivia()).then_some(token.range.end()),
        GreenElementAt::Node(child) if child.kind() == SyntaxKind::ANNOTATION => None,
        GreenElementAt::Node(child) => significant_end(child),
    })
}

/// The name a `NAME` node spells, read from its first token.
fn name_symbol(node: GreenNodeAt<'_>) -> Option<Symbol> {
    Some(Symbol::new(&name_text(node.first_token()?.text)))
}

fn literal_kind(node: GreenNodeAt<'_>, buffer: &mut String) -> LiteralKind {
    let mut text = |value: &str| {
        let start = buffer.len() as u32;
        buffer.push_str(value);
        Text {
            start,
            len: value.len() as u32,
        }
    };
    let Some(token) = node.first_token() else {
        return LiteralKind::Null;
    };
    match token.kind {
        SyntaxKind::INTEGER => LiteralKind::Integer(text(token.text)),
        SyntaxKind::DOUBLE => LiteralKind::Double(text(token.text)),
        SyntaxKind::COMPLEX => LiteralKind::Complex(text(token.text)),
        SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
            LiteralKind::String(text(string_value(token.text)))
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
pub fn integer_literal_position(module: &Module, kind: &ExpressionKind) -> Option<usize> {
    let one_based_index = match kind {
        ExpressionKind::Literal(LiteralKind::Integer(text)) => {
            module[text].trim_end_matches('L').parse::<usize>().ok()?
        }
        ExpressionKind::Literal(LiteralKind::Double(text)) => {
            let value = module[text].parse::<f64>().ok()?;
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
fn string_value(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix(['r', 'R']) {
        let Some(quote) = rest.chars().next() else {
            return text;
        };
        let inner = &rest[quote.len_utf8()..];
        let dashes = inner.chars().take_while(|&c| c == '-').count();
        let Some(open) = inner[dashes..].chars().next() else {
            return text;
        };
        let body = &inner[dashes + open.len_utf8()..];
        let close = match open {
            '(' => ')',
            '[' => ']',
            _ => '}',
        };
        // The closer is the bracket, the same dashes, then the quote.
        return body
            .strip_suffix(quote)
            .and_then(|body| body.strip_suffix(&inner[..dashes]))
            .and_then(|body| body.strip_suffix(close))
            .unwrap_or(body);
    }
    text.trim_matches(['"', '\''])
}

fn field_name_text(node: GreenNodeAt<'_>) -> Option<Symbol> {
    match node.kind() {
        SyntaxKind::NAME => name_symbol(node),
        SyntaxKind::LITERAL => {
            let token = node.first_token()?;
            (token.kind == SyntaxKind::STRING).then(|| Symbol::new(string_value(token.text)))
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
fn pipe_shape(call: GreenNodeAt<'_>) -> Option<PipeShape> {
    match count_placeholders(call) {
        0 => return Some(PipeShape::FirstArgument),
        1 => {}
        _ => return None,
    }
    let list = child_of_kind(call, SyntaxKind::ARGUMENT_LIST)?;
    list.child_nodes()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .position(|argument| {
            let named = argument
                .child_tokens()
                .any(|token| token.kind == SyntaxKind::EQ);
            // With a tag present, the value is the second expression child.
            named
                && expression_children(argument).nth(1).is_some_and(|value| {
                    value.kind() == SyntaxKind::NAME
                        && value
                            .child_tokens()
                            .any(|token| token.kind == SyntaxKind::UNDERSCORE)
                })
        })
        .map(PipeShape::NamedPlaceholder)
}

fn count_placeholders(node: GreenNodeAt<'_>) -> usize {
    node.children()
        .map(|child| match child {
            GreenElementAt::Token(token) => usize::from(token.kind == SyntaxKind::UNDERSCORE),
            GreenElementAt::Node(child) => count_placeholders(child),
        })
        .sum()
}
