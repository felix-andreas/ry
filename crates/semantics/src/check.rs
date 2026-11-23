//! Expression inference walk: Hindley–Milner over one item's HIR. Branches roll
//! back through an undo log instead of cloning the environment.

use {
    crate::{
        DataPath, Db, TypeDefinitions,
        annotations::{Annotation, Opaque},
        data_file_layout,
        dense::{Bitset, Column},
        diagnostics::{STANDARD_MODULES, annotation_misuses_vocabulary, nearest_name},
        exhaustiveness::{MatchFinding, match_findings},
        hir::{
            Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind,
            MatchArm, Module, NaAtom, NameSite, Parameter as Formal, PatId, PatternKind,
            RecordField as HirRecordField, Rest, Symbol, UnaryOperator, integer_literal_position,
            is_whole_number_double,
        },
        infer::{
            Entry, FunctionMismatch, InferenceTable, RecordMismatch, UNCHECKED_WITNESS, UnifyError,
            apply_definition, constraint_rejects, numeric_join,
        },
        metadata::{
            imported_by_name, imports_every_name, is_own_package, own_package_reaches,
            registered_methods,
        },
        naming::{
            BindingId, BindingKind, ForeachLoop, ItemNaming, MATCH_FUN_CALLERS, dot_dot_variable,
            foreach_iterators, foreach_loop, is_column_update, query_index_arguments,
        },
        native::{Judged, NativeBlocker, has_r_bound, native_blockers},
        sorted::{SortedMap, SortedSet},
        stubs::{NATIVE_NAMESPACES, StubLibrary, SymbolSet, is_ry_type, lower_type_text, stubs},
        top_level::super_assigned_names,
        types::{
            Atomic, Binder, Bound, Constraint, DefaultRead, FlagReturn, FrameKind, FunctionType,
            InferenceVar, Name, NativeField, NativeScalar, NullUnless, NumberLiteral, Parameter,
            QueryName, RecordField, ReduceForward, RestParameter, TYPE_SIZE_CEILING, Ty, TyKind,
            TypeList, TypeScheme, any, array, array_element, column_type, conflicting_tags,
            contains_inference_var, contains_rigid, erase_vars, for_each_child_type,
            is_ry_value_type, is_tag_type, map_child_types, native_record, native_scalar, null,
            scalar, substitute_rigid, type_size, union_of, unknown,
        },
    },
    bumpalo::{Bump, collections::Vec as BumpVec},
    hashbrown::{HashMap, HashSet},
    rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet},
    smallvec::{SmallVec, smallvec},
    std::{
        alloc::{AllocError, Allocator, Layout},
        borrow::Cow,
        cell::RefCell,
        mem,
        ops::Range,
        ptr::NonNull,
        rc::Rc,
        sync::{
            LazyLock,
            atomic::{AtomicUsize, Ordering},
        },
    },
    syntax::{
        NumberType, TextRange, TextSize,
        annotation::{Paired, pair_formals},
        is_syntactic_name,
        native_values::accepts_native,
    },
    thin_vec::ThinVec,
};

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct TypeError<'db> {
    pub range: TextRange,
    pub kind: TypeErrorKind<'db>,
}

/// Where a function literal's untyped formals took their types from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum LambdaContext {
    /// The call the literal is passed to.
    Call,
    /// The declared type of the parameter the literal is the default of.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum TypeErrorKind<'db> {
    Mismatch {
        expected: Ty<'db>,
        found: Ty<'db>,
    },
    /// `(name = value)` where a one-field record `Record{name: T}` is expected: R reads it as
    /// an assignment, and the record needs a trailing comma.
    AssignmentForRecord {
        field: String,
        expected: Ty<'db>,
    },
    /// A value that fits a nominal type's representation but was never made one, so the
    /// message can say how to make it.
    UnwrappedNominal {
        nominal: Ty<'db>,
        found: Ty<'db>,
    },
    /// A function's name where a function that takes a ry value expects a callback. Only R's
    /// `match.fun` turns a name into a function, and a ry function never calls it.
    FunctionName {
        expected: Ty<'db>,
        /// The name, when the argument is a string literal.
        name: Option<String>,
    },
    /// A value that may be `NULL` where `NULL` is not accepted, read through an expression no
    /// guard narrows (`x$a$b`, `x[["a"]]`, a call), so the message can say how to narrow it.
    UnnarrowedNull {
        expected: Ty<'db>,
        found: Ty<'db>,
    },
    /// Separate from `Mismatch` so the differing field is named instead of printing
    /// two near-identical record types.
    RecordShape {
        mismatch: Box<RecordMismatch<'db>>,
    },
    NotAFunction {
        found: Ty<'db>,
    },
    /// `vapply`'s `FUN.VALUE` disagrees with what the callback returns, which names the
    /// callback rather than printing the stub's `V[] | V` template.
    TemplateDisagreesWithCallback {
        callback: String,
        returns: Ty<'db>,
        template: Ty<'db>,
    },
    /// A read of a no-default formal on the branch where `missing(name)` held.
    MissingFormalRead {
        name: String,
    },
    ArityMismatch {
        expected: usize,
        found: usize,
    },
    /// Named formals with no default that the call leaves out and the body forces.
    MissingArguments {
        names: Vec<String>,
        /// A named argument that matched no formal and went to `...`, with the missing formal
        /// it most likely misspells.
        misspelled: Option<(String, String)>,
    },
    NamedArgumentMismatch {
        argument: String,
        duplicate: bool,
        suggestion: Option<String>,
        expected_parameters: Vec<String>,
    },
    MaybeNullCallee {
        found: Ty<'db>,
    },
    KnownTypeUnderIfUnknown {
        found: Ty<'db>,
    },
    /// zeallot's `%<-%` with a pattern whose name count differs from the value's item count,
    /// which zeallot stops on.
    DestructureCount {
        names: usize,
        items: usize,
    },
    /// R decides optionality, so a defaulted formal annotated as required is an
    /// annotation error, reported once here rather than at every caller.
    AnnotationRequiredButDefaulted {
        name: String,
    },
    AnnotationParameterMismatch {
        name: String,
    },
    NullDefaultNotAdmitted {
        name: String,
        declared: Ty<'db>,
    },
    ConstraintViolation {
        constraint: Constraint,
        found: Ty<'db>,
    },
    /// An `InvalidOperand` on a use of a function literal's formal whose type came from
    /// the call the literal is passed to or the parameter it is the default of, so the
    /// finding names the formal and that context.
    ContextualParameter {
        name: String,
        expected: OperandExpectation,
        passed: Ty<'db>,
        context: LambdaContext,
    },
    /// Separate from `Mismatch` because rendered types drop constraints, so two whole
    /// signatures can print identically; this names the failing position.
    CallbackShape {
        mismatch: Box<FunctionMismatch<'db>>,
    },
    NoMatchingOverload {
        name: String,
        candidates: usize,
        first: Option<Box<TypeError<'db>>>,
    },
    NotAList {
        found: Ty<'db>,
    },
    BadVectorIndex {
        index: Ty<'db>,
    },
    UnsupportedSubset {
        found: Ty<'db>,
    },
    /// `position` is 1-based.
    PositionDoesNotExist {
        position: usize,
        container: Ty<'db>,
    },
    FieldDoesNotExist {
        suggestion: Option<String>,
        field: String,
        container: Ty<'db>,
    },
    /// `@new` on, or access to the representation of, an `opaque` type outside the file that
    /// declares it.
    OpaqueOutsideHome {
        name: Name<'db>,
        opaque: Opaque,
        creating: bool,
    },
    /// A column named by a literal (`f$x`, `f[["x"]]`, `by = "x"`, an `on` key) that the
    /// frame lacks. `frame` is the frame variable's name when there is one.
    ColumnDoesNotExist {
        column: String,
        frame: Option<Symbol>,
        columns: Vec<String>,
        suggestion: Option<String>,
    },
    /// A column in `GroupBy.agg` that gives one value per row instead of one per group.
    NotAnAggregation {
        column: String,
    },
    /// `Expr.head(n)` inside a larger expression in `GroupBy.agg`, which accepts it only as a
    /// whole item.
    HeadInsideExpression {
        column: String,
    },
    /// An operand of a DataFrame expression whose column type the operation does not take, such
    /// as `Expr.col("region") * 2` on a column of strings. `column` names the column when the operand
    /// reads one directly.
    ExpressionOperand {
        operation: String,
        column: Option<String>,
        needs: ExpressionNeeds,
        found: Ty<'db>,
    },
    /// A comparison in a DataFrame expression between strings (or factors) and numbers.
    ExpressionComparison {
        operator: &'static str,
        left: Ty<'db>,
        right: Ty<'db>,
    },
    /// An operator in a DataFrame expression that dates do not support, such as `Expr.col("day") * 2`
    /// or a date compared with a number.
    DateOperation {
        operator: &'static str,
        left: Ty<'db>,
        right: Ty<'db>,
    },
    /// `setnames(f, new)` with as many names as the frame has columns, which data.table
    /// requires.
    ColumnNameCount {
        names: usize,
        columns: usize,
    },
    /// Values a data.table function needs of one type, such as `fifelse`'s `yes` and `no`.
    BranchTypesDiffer {
        function: &'static str,
        first: String,
        first_ty: Ty<'db>,
        second: String,
        second_ty: Ty<'db>,
    },
    /// A join condition whose two columns hold values the join cannot compare, such as
    /// integers and strings.
    IncompatibleJoinColumns {
        condition: String,
        x: Ty<'db>,
        i: Ty<'db>,
    },
    /// An ordering join condition (`<`, `<=`, `>`, `>=`) on a column of strings.
    OrderedJoinOnText {
        condition: String,
        column: String,
        found: Ty<'db>,
    },
    /// `roll =` on a join with a condition other than `==`.
    RollingNonEquiJoin,
    /// `:=` writing into part of a column, or recycling one value into it, a value of a type the
    /// column converts with loss: strings into a logical or numeric column, or a fractional
    /// number into an integer one.
    LossyColumnWrite {
        column: String,
        column_ty: Ty<'db>,
        found: Ty<'db>,
    },
    /// A finding of `lapply(.SD, f)` that only one column of `.SD` causes.
    InSubsetColumn {
        column: String,
        inner: Box<TypeErrorKind<'db>>,
    },
    /// A query's `i` that is neither a filter nor row positions.
    BadRowIndex {
        found: Ty<'db>,
    },
    /// R rejects `$` on every atomic vector, named ones included.
    DollarOnAtomicVector {
        found: Ty<'db>,
    },
    InvalidOperand {
        expected: OperandExpectation,
        found: Ty<'db>,
    },
    /// The class declares the operator, but not for this operand pair (`Date + Date`).
    UnsupportedOperandPair {
        operator: &'static str,
        left: Ty<'db>,
        right: Ty<'db>,
    },
    NotIterable {
        found: Ty<'db>,
    },
    /// `try x` where `x` is not a `:Ok(T) | :Err(E)` result.
    TryNotResult {
        found: Ty<'db>,
    },
    /// `try` outside a function, where there is nothing to return the error from.
    TryOutsideFunction,
    /// `(..base, x = 1)` where `base` is not a ry record.
    RecordUpdateBase {
        found: Ty<'db>,
    },
    InfiniteType {
        variable: Ty<'db>,
        container: Ty<'db>,
    },
    /// A `match` pattern that no value of the scrutinee's type fits.
    PatternNeverMatches {
        found: Ty<'db>,
        /// The likely mistake in a tag pattern: an unknown tag, or an unknown payload field.
        hint: Option<String>,
    },
    /// Two branches build one tag with two payload shapes.
    ConflictingTags {
        first: Ty<'db>,
        second: Ty<'db>,
    },
    /// A record, tuple or tag passed to an R function that refuses one at run time.
    NativeValueToR {
        function: String,
        kind: NativeValueKind,
    },
    /// `[1L, 2.5]`: an Array's items share one type and are never converted into each other.
    ArrayItemsDiffer {
        first: Ty<'db>,
        found: Ty<'db>,
    },
    /// An item an Array cannot hold, such as a vector, an R list, `NULL` or a function.
    ArrayItemType {
        found: Ty<'db>,
    },
    /// `NA` in an Array literal: an Array holds no missing values.
    ArrayMissingValue,
    /// An index an Array does not take: a name, several indexes, or a non-numeric position.
    ArrayIndex {
        double: bool,
        found: Option<Ty<'db>>,
    },
    /// An elementwise operator on an Array whose operands do not share one item type, or whose
    /// item type the operator does not take (`["a"] + ["b"]`).
    ArrayOperands {
        operator: &'static str,
        left: Ty<'db>,
        right: Ty<'db>,
    },
    /// An operator on ry scalars whose operands differ in type, or whose type it does not take
    /// (`"a" + "b"` on `Str`, `-x` on a `Bool`). `right` is `None` for a unary operator.
    NumberOperands {
        operator: &'static str,
        left: Ty<'db>,
        right: Option<Ty<'db>>,
    },
    /// `/` on a ry integer type, which has no exact quotient.
    IntegerDivision {
        ty: Ty<'db>,
    },
    /// A number literal that the ry number type it is asked to take cannot hold (`1.5` or `2^63`
    /// as an `I64`).
    LiteralOutOfRange {
        expected: Ty<'db>,
    },
    /// `point$z <- value` on a record without a field `z`: a record's fields are fixed.
    FieldCannotBeAdded {
        field: String,
        container: Ty<'db>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum NativeValueKind {
    Record,
    Tuple,
    Tag,
    Dict,
    Set,
    /// An `I64`, which no R type represents.
    I64,
    /// A `List` whose items are not `F64`, `Bool` or `Str`, so it is no R vector.
    List,
}

/// A type only the checker knows that the runtime needs: the storage of a list's items for a list
/// the runtime may build empty, the key and value types of a dictionary built from no entries,
/// or the number a `Num.from_r` gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum CheckedType {
    List(ListItems),
    /// A `Dict` of `I64`, `F64` or `Str` keys and values, the ones typed code holds unboxed.
    Dict {
        key: ListItems,
        value: ListItems,
    },
    /// A `Num.from_r(x)` of an `x` the checker typed `integer` (`I64`) or `double` (`F64`).
    Number(NumberType),
}

impl CheckedType {
    /// The name `.ry_items` takes: `"I64"` for a list or a number, `"Dict<Str, I64>"` for a
    /// dictionary.
    pub fn name(self) -> std::borrow::Cow<'static, str> {
        match self {
            CheckedType::List(items) => items.name().into(),
            CheckedType::Dict { key, value } => {
                format!("Dict<{}, {}>", key.name(), value.name()).into()
            }
            CheckedType::Number(NumberType::I64) => "I64".into(),
            CheckedType::Number(NumberType::F64) => "F64".into(),
        }
    }
}

/// The run-time storage of a ry list's items, for a list the runtime may build empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ListItems {
    I64,
    F64,
    Str,
    /// Records, tuples, tags, dictionaries, sets and lists, stored in an R list.
    RyValues,
}

impl ListItems {
    /// The name `.ry_items` takes.
    pub fn name(self) -> &'static str {
        match self {
            ListItems::I64 => "I64",
            ListItems::F64 => "F64",
            ListItems::Str => "Str",
            ListItems::RyValues => "list",
        }
    }
}

/// What an operation inside a DataFrame expression takes, as the table engine's own typing
/// decides before it runs a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ExpressionNeeds {
    /// Arithmetic and numeric aggregations: logical, integer or double columns.
    Numbers,
    /// `!`, `&` and `|`: logical columns, or numbers with zero as `FALSE`.
    LogicalOrNumbers,
    /// The test of `Expr.if_else`.
    Logical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum OperandExpectation {
    Numeric,
    ScalarNumeric,
    Logical,
    Comparable,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemCheck<'db> {
    pub errors: ThinVec<TypeError<'db>>,
    pub strict_origins: ThinVec<StrictOrigin>,
    /// What keeps the item's native functions off native code, in source order. Recorded only
    /// where strict origins are, because the finding is a strict-mode one.
    pub native_blockers: ThinVec<NativeBlocker>,
    pub scheme: Option<TypeScheme<'db>>,
    /// Reads inside a `data.table` bracket's index arguments: they evaluate in the
    /// data's own frame, so bare names are columns and skip the unresolved-name check.
    pub masked_reads: SortedSet<ExprId>,
    /// Bare names inside a query on a frame with known columns that name no column and
    /// no lexical binding. Reported only when nothing else (a global, a builtin) defines
    /// the name, because R falls back to the enclosing scope for a non-column.
    pub column_misses: SortedMap<ExprId, ColumnMiss>,
    /// Settled scheme of every name the top-level frame binds, so a statement item's
    /// conditional write serves cross-item reads; a definition resolves via `scheme`.
    pub top_level_bindings: ThinVec<(Symbol, TypeScheme<'db>)>,
    /// Coverage of each `match`, kept apart from `errors` because a missing case does not
    /// make the item's exported type untrustworthy.
    pub match_findings: ThinVec<MatchFinding>,
    /// The unsuffixed number literals this check typed as `I64`, in id order. R reads `5` as a
    /// `double` whatever the checker decided, so the code ry's runtime runs spells each one with
    /// its suffix (`preflight` in the `ry` crate); without it, `-> I64` could return a `double`.
    pub i64_literals: ThinVec<ExprId>,
    /// The expressions whose types only this check knows and the runtime needs, in id order: an
    /// empty `[]` (stored by its items' storage, `logical` when there are none), a library call
    /// whose result items come from a callback (`List.map`), one building a dictionary from no
    /// entries (`Dict.empty()`, laid out by key and value types), and `Num.from_r`, whose number
    /// otherwise depends on the R value. The runtime receives each through `.ry_items` (`preflight`
    /// in `ry`).
    pub checked_types: ThinVec<(ExprId, CheckedType)>,
    /// What only editor features read, absent when the host asked for no editor facts, so a
    /// command's check keeps no per-item slots for them.
    pub editor: Option<Box<EditorFacts<'db>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct EditorFacts<'db> {
    pub expression_types: ExpressionTypes<'db>,
    /// The type of the value each `match` pattern sees, narrowed to the members it can match,
    /// so a tag or field inside a pattern hovers as the case it names. Indexed by pattern id;
    /// a pattern that cannot match records nothing.
    pub pattern_types: PatternTypes<'db>,
    /// The declared type each native record, tuple or tag literal was checked against, at an
    /// annotated binding, a call argument or a nominal constructor, so completion can offer
    /// the fields a literal still lacks. Nested literals record nothing: their expectation is
    /// the enclosing one's field or payload.
    pub literal_expectations: SortedMap<ExprId, Ty<'db>>,
    /// The element type each `for` loop binds its variable to, keyed by the loop. The
    /// variable is a declaration, not an expression, so `expression_types` has no slot for it.
    pub loop_variables: SortedMap<ExprId, Ty<'db>>,
    /// Index into the declared overload set; absent when no candidate matched.
    pub selected_overloads: SortedMap<ExprId, usize>,
    /// Call heads whose lookup skipped the binding naming resolved, because that binding
    /// cannot hold a function. Editor features navigate to the binding the call reaches.
    pub callee_targets: SortedMap<ExprId, CalleeTarget>,
}

static NO_EDITOR_FACTS: LazyLock<EditorFacts<'static>> = LazyLock::new(EditorFacts::default);

impl<'db> ItemCheck<'db> {
    /// The editor facts, empty when the host asked for none.
    pub fn editor(&self) -> &EditorFacts<'db> {
        self.editor.as_deref().unwrap_or(&NO_EDITOR_FACTS)
    }
}

/// A DataFrame function whose result the checker computes from its arguments.
#[derive(Clone, Copy)]
enum DataFrameVerb {
    Select {
        lazy: bool,
    },
    WithColumns {
        lazy: bool,
    },
    /// `filter`, `sort`, `head` and `tail`, which keep the columns as they are.
    KeepColumns {
        lazy: bool,
    },
    GroupBy {
        lazy: bool,
    },
    Agg,
    Join {
        lazy: bool,
        asof: bool,
    },
    /// `lazy` and `collect`, which keep the columns and change the kind.
    Convert(FrameKind),
    ReadCsv(FrameKind),
}

/// The column a DataFrame expression computes. `reads_column` tells an expression from a
/// constant, and `shape` how many values it gives per group.
struct ComputedColumn<'db> {
    name: String,
    ty: Ty<'db>,
    reads_column: bool,
    shape: Shape,
}

/// How many values a DataFrame expression gives inside `GroupBy.agg`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// One per row, which `agg` rejects.
    PerRow,
    /// One per group.
    PerGroup,
    /// `Expr.head(n)`: up to `n` per group, which `agg` accepts only as a whole item, exploding
    /// them into rows.
    Head,
    /// An expression that combines `Expr.head(n)` with something else, which `agg` rejects.
    NestedHead,
}

impl Shape {
    /// The shape of an expression combining two others: one value per group only when both give
    /// one. A head inside wins, so that `agg` can say what is wrong.
    fn combine(self, other: Shape) -> Shape {
        match (self, other) {
            (Shape::Head | Shape::NestedHead, _) | (_, Shape::Head | Shape::NestedHead) => {
                Shape::NestedHead
            }
            (Shape::PerGroup, Shape::PerGroup) => Shape::PerGroup,
            _ => Shape::PerRow,
        }
    }
}

/// The binding a call head reaches after skipping non-function bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum CalleeTarget {
    /// An enclosing slot of the same name.
    Slot(BindingId),
    /// The project's top-level binding of the name.
    Project,
    /// The library function of the name, past every project binding.
    Library,
}

/// Settled type of every expression the checker recorded, indexed by expression id. Ids are
/// dense within an item, so each expression has a slot. An item uses few distinct types, so a
/// slot holds a narrow index into the item's palette of them rather than a pointer: this is
/// the most numerous memoized table, and the narrow slots make it a quarter of the size.
#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::SalsaValue)]
pub struct ExpressionTypes<'db> {
    palette: ThinVec<Ty<'db>>,
    slots: PaletteSlots,
}

/// One past the palette index, so zero is an expression with no recorded type.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
enum PaletteSlots {
    Narrow(ThinVec<u16>),
    /// For an item with more distinct types than a `u16` can count.
    Wide(ThinVec<u32>),
}

impl Default for PaletteSlots {
    fn default() -> Self {
        PaletteSlots::Narrow(ThinVec::new())
    }
}

impl<'db> ExpressionTypes<'db> {
    pub fn get(&self, id: &ExprId) -> Option<&Ty<'db>> {
        let slot = match &self.slots {
            PaletteSlots::Narrow(slots) => u32::from(*slots.get(id.index())?),
            PaletteSlots::Wide(slots) => *slots.get(id.index())?,
        };
        self.palette
            .get(usize::try_from(slot).ok()?.checked_sub(1)?)
    }
}

impl<'db> FromIterator<Option<Ty<'db>>> for ExpressionTypes<'db> {
    fn from_iter<I: IntoIterator<Item = Option<Ty<'db>>>>(types: I) -> Self {
        let mut palette = Vec::new();
        let mut positions: FxHashMap<Ty<'db>, u32> = FxHashMap::default();
        let slots: Vec<u32> = types
            .into_iter()
            .map(|ty| {
                ty.map_or(0, |ty| {
                    *positions.entry(ty).or_insert_with(|| {
                        palette.push(ty);
                        u32::try_from(palette.len()).unwrap_or(u32::MAX)
                    })
                })
            })
            .collect();
        let slots = match slots
            .iter()
            .map(|&slot| u16::try_from(slot))
            .collect::<Result<ThinVec<u16>, _>>()
        {
            Ok(narrow) => PaletteSlots::Narrow(narrow),
            Err(_) => PaletteSlots::Wide(ThinVec::from(slots)),
        };
        ExpressionTypes {
            palette: ThinVec::from(palette),
            slots,
        }
    }
}

/// The checker's working copy of [`ExpressionTypes`], before the inference table resolves it.
struct RecordedTypes<'db>(Vec<Option<Ty<'db>>>);

impl<'db> RecordedTypes<'db> {
    fn get(&self, id: &ExprId) -> Option<&Ty<'db>> {
        self.0.get(id.index())?.as_ref()
    }

    fn insert(&mut self, id: ExprId, ty: Ty<'db>) {
        let slot = self
            .0
            .get_mut(id.index())
            .unwrap_or_else(|| panic!("recorded type for {id:?} outside the item's module"));
        *slot = Some(ty);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::SalsaValue)]
pub struct PatternTypes<'db>(ThinVec<Option<Ty<'db>>>);

impl<'db> PatternTypes<'db> {
    pub fn get(&self, id: PatId) -> Option<Ty<'db>> {
        *self.0.get(id.index())?
    }
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ColumnMiss {
    /// The queried variable's name, when the frame is a plain variable.
    pub frame: Option<Symbol>,
    pub columns: Vec<String>,
}

/// Where an undetermined type was first introduced; propagation is never re-reported.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct StrictOrigin {
    pub expression: ExprId,
    pub range: TextRange,
    pub kind: StrictOriginKind,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum StrictOriginKind {
    UnsupportedConstruct,
    UndeterminedReference(String),
    LoopWidened(String),
    /// A recursive definition that checked clean yet exports `Unknown`: the reference
    /// cycle is the only possible source.
    RecursiveUnknown(String),
    /// A query naming `column` of a frame whose columns are unknown, so nothing checks it.
    /// `frame` is the queried variable's name, when the frame is a plain variable.
    UnknownColumns {
        frame: Option<String>,
        column: String,
    },
    /// An unannotated parameter nothing constrains, so it accepts any value unchecked.
    /// `expression` is the function and `range` the parameter's name.
    UntypedParameter(Symbol),
    /// A checked annotation over a value whose type is undetermined, so it checks nothing.
    UncheckedAnnotation,
}

/// No method has a default body: adding a fact must be a compile error at every
/// implementor rather than a silent fallback.
pub trait GlobalEnv<'db> {
    /// `deferred`: a read inside a nested function runs after the document frame
    /// settled, so the LAST binding wins; an immediate read sees only earlier ones.
    fn scheme(&self, name: Symbol, deferred: bool) -> Option<TypeScheme<'db>>;

    /// `None` when the name has at most one candidate or a package/local definition
    /// shadows the stub set.
    fn overloads(&self, name: Symbol, deferred: bool) -> Option<&'db [TypeScheme<'db>]>;

    /// A project definition has its own attributable site, so a reference propagates
    /// its `Unknown` instead of re-originating one.
    fn defined_in_project(&self, name: Symbol, deferred: bool) -> bool;

    /// Never a stub, and never the project-wide conditional slot, whose join includes
    /// this item's own write.
    fn earlier_binding(&self, name: Symbol) -> Option<TypeScheme<'db>>;

    /// Every stub candidate for `name`, ignoring project definitions: what `pkg::name` or a
    /// call that skipped the project's non-function binding reaches. Empty for an export
    /// the stubs list without a type.
    fn library_candidates(&self, name: Symbol) -> Option<&'db [TypeScheme<'db>]>;

    /// What a call reaches once its lookup has left the file's top-level frame without finding
    /// a function, the library aside. A package file's frame is the namespace itself, so it
    /// reaches nothing; a test or helper file's frame encloses in the helpers' environment and
    /// the namespace, and a script's in the namespace.
    fn enclosing_scheme(&self, name: Symbol) -> Option<TypeScheme<'db>>;

    /// What the project's own `pkg::name` reads: the package namespace, then the library.
    /// A test file's or a script's own bindings and the helpers' are not in the namespace.
    fn namespace_scheme(&self, name: Symbol) -> Option<TypeScheme<'db>>;

    /// Borrowed: per-file facts consulted once per item; an owned copy would make every
    /// item pay for the whole table.
    fn type_definitions(&self) -> TypeDefinitions<'db>;

    fn arithmetic_classes(&self) -> crate::ArithmeticClasses<'db>;

    /// Whether the project defines an arithmetic or `Ops` method for a class that no type
    /// declares, so a plain list operand may carry that class.
    fn has_untracked_arithmetic_class(&self) -> bool;

    /// Whether the project or the file defines a `$` method (`$.vs`), which R dispatches on a
    /// class attribute that `class(x) <- "vs"` sets on an atomic vector no type tracks.
    fn defines_dollar_method(&self) -> bool;

    /// How many reads so far served a cyclic-group export that may still hold the fixpoint's
    /// seed, so an `Unknown` read after it may be a value the fixpoint has not computed yet.
    fn pending_reads(&self) -> usize;
}

/// Perf-witness instrument; fixpoint re-runs make it exceed the item count.
pub static CHECK_EXECUTIONS: AtomicUsize = AtomicUsize::new(0);

#[allow(clippy::too_many_arguments)]
pub fn check_item_with_annotation<'db>(
    db: &'db dyn Db,
    module: &Module,
    naming: &ItemNaming,
    annotation: Option<&Annotation<'db>>,
    expression_annotations: &[(ExprId, Annotation<'db>)],
    globals: Option<&dyn GlobalEnv<'db>>,
    record_strict_origins: bool,
    top_level_ry_values: bool,
    template: bool,
) -> ItemCheck<'db> {
    CHECK_EXECUTIONS.fetch_add(1, Ordering::Relaxed);
    // A check can fetch another item's scheme and so run a nested check on the same thread,
    // which takes its own arena from the top of the stack.
    let mut arena = CHECK_ARENAS.with_borrow_mut(Vec::pop).unwrap_or_default();
    let check = check_item_in(
        &arena,
        db,
        module,
        naming,
        annotation,
        expression_annotations,
        globals,
        record_strict_origins,
        top_level_ry_values,
        template,
    );
    arena.reset();
    CHECK_ARENAS.with_borrow_mut(|arenas| arenas.push(arena));
    check
}

thread_local! {
    /// Reset arenas kept for the next check on this thread, so a check's working state costs
    /// no allocator calls once the thread has warmed up.
    static CHECK_ARENAS: RefCell<Vec<Bump>> = const { RefCell::new(Vec::new()) };
}

#[allow(clippy::too_many_arguments)]
fn check_item_in<'db>(
    arena: &Bump,
    db: &'db dyn Db,
    module: &Module,
    naming: &ItemNaming,
    annotation: Option<&Annotation<'db>>,
    expression_annotations: &[(ExprId, Annotation<'db>)],
    globals: Option<&dyn GlobalEnv<'db>>,
    record_strict_origins: bool,
    top_level_ry_values: bool,
    template: bool,
) -> ItemCheck<'db> {
    let mut table = InferenceTable::default();
    let expressions = module.expression_count();
    let bindings = naming.bindings.len();
    if let Some(globals) = globals {
        table.definitions = Some(globals.type_definitions());
        table.arithmetic_classes = Some(globals.arithmetic_classes());
    }
    let mut context = Checker {
        db,
        module,
        naming,
        globals,
        arena,
        library: stubs(db),
        table,
        frame_level: 0,
        callback_frame_level: None,
        discarded_if: None,
        parameter_hints: Column::new_in(arena, expressions),
        hinted_parameters: HashSet::with_hasher_in(FxBuildHasher, Arena(arena)),
        expression_annotations: {
            let mut annotations = Column::new_in(arena, expressions);
            for (expression, annotation) in expression_annotations {
                if !refused_by_vocabulary(db, globals, annotation) {
                    annotations.insert(*expression, annotation.clone());
                }
            }
            annotations
        },
        environment: Environment {
            entries: Column::new_in(arena, bindings),
            facts: Vec::new(),
            undo: Vec::new(),
            written: Bitset::new_in(arena, bindings),
        },
        scheme_arena: Vec::new(),
        literal_reads: Bitset::new_in(arena, expressions),
        recorded: RecordedTypes(vec![None; expressions]),
        recorded_patterns: Column::new_in(arena, module.pattern_count()),
        recorded_expectations: Column::new_in(arena, expressions),
        recorded_loop_variables: Column::new_in(arena, expressions),
        root_binders: FxHashMap::default(),
        selected_overloads: Column::new_in(arena, expressions),
        callee_targets: Column::new_in(arena, expressions),
        masked_reads: Bitset::new_in(arena, expressions),
        undecided_calls: Bitset::new_in(arena, expressions),
        unseen_frames: 0,
        column_scopes: Vec::new(),
        column_misses: Column::new_in(arena, expressions),
        query_depth: 0,
        errors: Vec::new(),
        contained_errors: Vec::new(),
        record_strict_origins,
        strict_origins: Vec::new(),
        // A statement with its own annotation is typed by the annotation, as in a function.
        top_level_ry_values: top_level_ry_values
            && annotation.is_none_or(|annotation| annotation.declared.is_none()),
        overload_probe_depth: 0,
        ry_callee: false,
        operator_method_depth: 0,
        call_first_var: 0,
        return_frames: Vec::new(),
        bare_na_returns: FxHashSet::default(),
        pending_enclosing_writes: Vec::new(),
        forward_captured: Bitset::new_in(arena, bindings),
        capture_joins: Column::new_in(arena, bindings),
        capture_writes: Column::new_in(arena, bindings),
        late_reads: Column::new_in(arena, bindings),
        late_triggers: SmallVec::new(),
        body_range: TextRange::default(),
        capture_repass_needed: false,
        placeholder_reads: 0,
        superassign_seeds: SmallVec::new(),
        pre_materialized: Column::new_in(arena, bindings),
        loop_jumps: Vec::new(),
        no_default_formals: Bitset::new_in(arena, bindings),
        defaulted_formals: Bitset::new_in(arena, bindings),
        missing_path_functions: Bitset::new_in(arena, bindings),
        non_local_slots: HashMap::with_hasher_in(FxBuildHasher, Arena(arena)),
        string_values: Column::new_in(arena, bindings),
        value_read_slots: None,
        computed_slots: Bitset::new_in(arena, bindings),
        lookup_slots: Bitset::new_in(arena, bindings),
        valued_slots: Bitset::new_in(arena, bindings),
        super_assigned: super_assigned_names(module),
        template_call: module
            .root
            .filter(|_| template)
            .and_then(|root| match module.kind(root) {
                ExpressionKind::Assign { value, .. } => Some(*value),
                _ => None,
            })
            .filter(|value| matches!(module.kind(*value), ExpressionKind::Call { .. }))
            .map(|value| module.range(value)),
        next_non_local_slot: naming
            .bindings
            .keys()
            .map(|slot| slot.0 + 1)
            .max()
            .unwrap_or(0),
    };
    let mut annotation = annotation;
    if let Some(present) = annotation {
        let mut mentioned = present.declared.iter().map(|declared| declared.body).chain(
            present
                .new_nominal
                .iter()
                .flat_map(|(_, arguments, _)| arguments.iter().copied()),
        );
        let cycle = context.table.definitions.and_then(|definitions| {
            mentioned
                .find_map(|ty| alias_cycle_walk(db, definitions, ty, &mut FxHashSet::default()))
        });
        // The cycle is reported at the alias declarations, so a use only stops trusting it.
        if cycle.is_some() || refused_by_vocabulary(db, globals, present) {
            annotation = None;
        }
    }
    let mut scheme = None;
    // A declared scheme survives a failing body: it is the author's contract, so call
    // sites keep being checked.
    let mut scheme_is_declared = false;
    if let Some(root) = module.root {
        let declared_fn = annotation.filter(|a| !a.trusted).and_then(|a| {
            let declared = a.declared.as_ref()?;
            match declared.body.kind(db) {
                TyKind::Function(function) => Some((declared.clone(), *function)),
                _ => None,
            }
        });
        match (module.kind(root), declared_fn) {
            // A function literal with its own signature is typed by it, and the binding's
            // declared type is then checked against that like any other value.
            (ExpressionKind::Assign { target, value, .. }, Some((declared, function)))
                if matches!(module.kind(*value), ExpressionKind::Function { .. })
                    && !context.expression_annotations.contains_key(*value) =>
            {
                context.table.enter_rigid_binders(&declared.binders);
                // Export the reconciled signature, not the annotation as written: R matches
                // arguments against the definition's formals, so annotation mistakes report once.
                let declared = TypeScheme {
                    binders: declared.binders,
                    body: context.check_declared_function(
                        *value,
                        function,
                        annotation.map_or(&[], |annotation| &annotation.inline_parameters),
                    ),
                };
                for expression in [*target, *value, root] {
                    context.recorded.insert(expression, declared.body);
                }
                scheme = Some(declared);
                scheme_is_declared = true;
            }
            (ExpressionKind::Assign { value, .. }, _) => {
                let root_ty = context.infer(root);
                let value_ty = context.recorded.get(value).copied().unwrap_or(root_ty);
                if annotation.is_some_and(|a| a.declared.is_some() || a.new_nominal.is_some()) {
                    context.annotation_covers(*value);
                }
                scheme = if let Some((new_name, new_arguments, _)) =
                    annotation.and_then(|a| a.new_nominal.clone())
                {
                    Some(context.check_new_nominal(new_name, &new_arguments, *value, value_ty))
                } else {
                    match annotation.and_then(|a| a.declared.clone()) {
                        Some(declared) if annotation.is_some_and(|a| a.if_unknown) => {
                            context.default_literal_vars();
                            let resolved_value = context.table.resolve(db, value_ty);
                            if matches!(resolved_value.kind(db), TyKind::Unknown) {
                                Some(declared)
                            } else {
                                context.errors.push(TypeError {
                                    range: module.range(*value),
                                    kind: TypeErrorKind::KnownTypeUnderIfUnknown {
                                        found: resolved_value,
                                    },
                                });
                                Some(context.generalize(value_ty))
                            }
                        }
                        // The value flows into the declaration like an argument into a parameter, but a
                        // declared nominal still rejects a structural value (`@new` is the only nominal
                        // introduction).
                        Some(mut declared) => {
                            context.table.enter_rigid_binders(&declared.binders);
                            context.record_expectation(*value, declared.body);
                            if annotation.is_some_and(|a| !a.trusted)
                                && !matches!(declared.body.kind(db), TyKind::Unknown | TyKind::Any)
                            {
                                let expected = declared.body;
                                let range = module.range(*value);
                                let mut adapted = context.adapt_literal(*value, expected)
                                    != LiteralFit::NotLiteral;
                                // `total: F64 <- List.sum([3])`: the declaration is the use
                                // that decides a ry number the value left open, so it binds
                                // before the open number takes its default.
                                let open = context.table.resolve(db, value_ty);
                                if !adapted
                                    && holds_ry_scalar(db, expected)
                                    && context.table.contains_unbound_var(db, open)
                                {
                                    let snapshot = context.table.snapshot();
                                    match context.table.unify(db, value_ty, expected) {
                                        Ok(_) => adapted = true,
                                        Err(_) => context.table.rollback(snapshot),
                                    }
                                }
                                // A literal the declaration does not adapt takes its default
                                // type first, as R's `1L` is an `integer` an `integer[]` admits.
                                context.default_literal_vars();
                                let resolved_value = context.table.resolve(db, value_ty);
                                if !adapted
                                    && !context.table.compatible(db, resolved_value, expected)
                                {
                                    let error = context.type_mismatch(
                                        range,
                                        expected,
                                        resolved_value,
                                        Some(*value),
                                    );
                                    context.errors.push(error);
                                }
                            }
                            declared.body =
                                context.elided_return_from_value(declared.body, value_ty);
                            scheme_is_declared = true;
                            Some(declared)
                        }
                        None => Some(context.generalize_root(value_ty)),
                    }
                };
            }
            _ => {
                if let Some(present) = annotation {
                    context.expression_annotations.insert(root, present.clone());
                }
                context.infer(root);
            }
        }
    }
    context.default_literal_vars();
    let recorded = mem::take(&mut context.recorded.0);
    let i64_literals = recorded
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let id = ExprId::new(index);
            let is_unsuffixed = matches!(
                module.kind(id),
                ExpressionKind::Literal(LiteralKind::Integer(_) | LiteralKind::Double(_))
            );
            let ty = context.table.resolve(db, (*slot)?);
            (is_unsuffixed && matches!(ty.kind(db), TyKind::Native(NativeScalar::I64)))
                .then_some(id)
        })
        .collect();
    let num_from_r = Symbol::lookup("Num.from_r");
    let mut checked_types: ThinVec<(ExprId, CheckedType)> = recorded
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            let id = ExprId::new(index);
            let library = |callee: &ExprId, name: Symbol| {
                !naming.resolutions.contains_key(callee)
                    && context
                        .globals
                        .and_then(|globals| globals.library_candidates(name))
                        .is_some()
            };
            let candidate = match module.kind(id) {
                ExpressionKind::Array(items) => module[items].is_empty(),
                ExpressionKind::Call { callee, .. } => match module.kind(*callee) {
                    // `.ry_items` checks a `Num.from_r` by the call's text, so it wraps only the
                    // standard library's.
                    ExpressionKind::NameRef(name) if Some(*name) == num_from_r => {
                        if !library(callee, *name) {
                            return None;
                        }
                        let ty = context.table.resolve(db, (*slot)?);
                        let number = match ty.kind(db) {
                            TyKind::Native(NativeScalar::I64) => NumberType::I64,
                            TyKind::Native(NativeScalar::F64) => NumberType::F64,
                            _ => return None,
                        };
                        return Some((id, CheckedType::Number(number)));
                    }
                    // `.ry_items` returns any other value unchanged, so a library name a project
                    // function shadows is wrapped harmlessly.
                    ExpressionKind::NameRef(name) => {
                        library(callee, *name)
                            && context
                                .globals
                                .and_then(|globals| globals.library_candidates(*name))
                                .is_some_and(|candidates| {
                                    candidates.iter().any(|scheme| {
                                        builds_fresh_items(db, scheme)
                                            || builds_empty_dict(db, scheme)
                                    })
                                })
                    }
                    _ => false,
                },
                _ => false,
            };
            if !candidate {
                return None;
            }
            let ty = context.table.resolve(db, (*slot)?);
            if let Some((key, value)) = crate::types::dict_entries(db, ty) {
                let scalar = |ty| list_items(db, ty).filter(|&items| items != ListItems::RyValues);
                // A dictionary key is always a single value, so an R string key such as `"a"` is
                // held as a `Str`, the value the runtime hashes it as.
                let key = match key.kind(db) {
                    TyKind::Scalar(Atomic::Character) => Some(ListItems::Str),
                    _ => scalar(key),
                };
                return Some((
                    id,
                    CheckedType::Dict {
                        key: key?,
                        value: scalar(value)?,
                    },
                ));
            }
            let item = crate::types::array_element(db, ty)?;
            // A nominal such as `type Tree = :Leaf(I64) | ...` is stored as its representation.
            let item = match item.kind(db) {
                TyKind::Named(name, arguments) => context
                    .table
                    .representation(db, *name, arguments)
                    .unwrap_or(item),
                _ => item,
            };
            list_items(db, item).map(|items| (id, CheckedType::List(items)))
        })
        .collect();
    checked_types.sort_unstable_by_key(|(id, _)| id.index());
    let match_findings = match_findings(db, context.table.definitions, module, |scrutinee| {
        let ty = (*recorded.get(scrutinee.index())?)?;
        Some(context.table.resolve(db, ty))
    });
    let native_blockers = if context.record_strict_origins {
        // The declaration as written decides whether a function is judged, so a formal the
        // author left untyped keeps the function out.
        let mut signatures: Column<ExprId, &'db FunctionType<'db>> =
            Column::new_in(arena, expressions);
        let mut aliased_signatures: Bitset<ExprId> = Bitset::new_in(arena, expressions);
        let declared_function = |annotation: &Annotation<'db>| {
            if annotation.trusted || annotation.if_unknown || annotation.new_nominal.is_some() {
                return None;
            }
            let declared = annotation.declared.as_ref()?;
            // An alias of a function type declares the signature too, without writing it where
            // the runtime reads it.
            let mut body = declared.body;
            let mut through_alias = false;
            for _ in 0..8 {
                let TyKind::Named(name, arguments) = body.kind(db) else {
                    break;
                };
                let alias = context
                    .table
                    .definitions
                    .and_then(|definitions| definitions.get(db, *name))
                    .filter(|definition| definition.alias && arguments.is_empty());
                let Some(alias) = alias else {
                    break;
                };
                body = alias.body;
                through_alias = true;
            }
            match body.kind(db) {
                TyKind::Function(function) if !has_r_bound(declared) => {
                    Some((*function, through_alias && !annotation.written_signature))
                }
                _ => None,
            }
        };
        for id in module.expression_ids() {
            match *module.kind(id) {
                ExpressionKind::Function { .. } => {
                    if let Some((function, aliased)) = context
                        .expression_annotations
                        .get(id)
                        .and_then(declared_function)
                    {
                        signatures.insert(id, function);
                        if aliased {
                            aliased_signatures.insert(id);
                        }
                    }
                }
                ExpressionKind::Assign { value, .. }
                    if matches!(module.kind(value), ExpressionKind::Function { .. })
                        && !context.expression_annotations.contains_key(value) =>
                {
                    let item_annotation = if Some(id) == module.root {
                        annotation
                    } else {
                        None
                    };
                    if let Some((function, aliased)) = context
                        .expression_annotations
                        .get(id)
                        .or(item_annotation)
                        .and_then(declared_function)
                    {
                        signatures.insert(value, function);
                        if aliased {
                            aliased_signatures.insert(value);
                        }
                    }
                }
                _ => {}
            }
        }
        if signatures.is_empty() {
            ThinVec::new()
        } else {
            native_blockers(Judged {
                db,
                module,
                naming,
                definitions: context.table.definitions,
                globals: context.globals,
                types: &|id| {
                    let ty = (*recorded.get(id.index())?)?;
                    Some(context.table.resolve(db, ty))
                },
                signatures: &signatures,
                aliased: &aliased_signatures,
            })
        }
    } else {
        ThinVec::new()
    };
    let editor_facts = db.editor_facts();
    let mut editor = None;
    if editor_facts {
        let mut binder_types = FxHashMap::default();
        let expression_types = recorded
            .into_iter()
            .map(|slot| slot.map(|ty| context.editor_type(ty, &mut binder_types)))
            .collect();
        let mut patterns = vec![None; module.pattern_count()];
        for (id, &ty) in context.recorded_patterns.iter() {
            if let Some(slot) = patterns.get_mut(id.index()) {
                *slot = Some(context.editor_type(ty, &mut binder_types));
            }
        }
        let pattern_types = PatternTypes(ThinVec::from(patterns));
        let literal_expectations = context
            .recorded_expectations
            .iter()
            .map(|(id, &ty)| (id, context.editor_type(ty, &mut binder_types)))
            .collect();
        let loop_variables = context
            .recorded_loop_variables
            .iter()
            .map(|(id, &ty)| (id, context.editor_type(ty, &mut binder_types)))
            .collect();
        editor = Some(Box::new(EditorFacts {
            expression_types,
            pattern_types,
            literal_expectations,
            loop_variables,
            selected_overloads: context
                .selected_overloads
                .iter()
                .map(|(callee, &index)| (callee, index))
                .collect(),
            callee_targets: context
                .callee_targets
                .iter()
                .map(|(callee, &target)| (callee, target))
                .collect(),
        }));
    }
    // A failed item exports `Unknown` so downstream items do not cascade off an
    // untrustworthy shape. Failing expressions record `Unknown`, so later checks inside
    // the item read them as absent facts rather than cascading. An error whose slot took a
    // fixed type anyway leaves the shape trustworthy.
    let mut errors = context.errors;
    let trusted = context.contained_errors.len() == errors.len();
    let scheme = if trusted || scheme_is_declared {
        // Inference variables are table-scoped: a scheme crossing the item boundary must
        // never carry one.
        scheme.map(|scheme| close_scheme(db, &mut context.table, scheme))
    } else {
        scheme.map(|_| TypeScheme::monomorphic(unknown(db)))
    };
    // Speculative paths (overload probing, guard edges) can record the same failure twice.
    errors.sort_by_key(|error| (error.range.start(), error.range.end()));
    errors.dedup();
    let mut top_level_bindings = Vec::new();
    for (&id, info) in &naming.bindings {
        if info.kind != BindingKind::TopLevel && !naming.extras().loop_bound_slots.contains(&id) {
            continue;
        }
        let Some(entry) = context.environment.get(id) else {
            continue;
        };
        let binding_scheme = if trusted {
            let open = match entry {
                EnvEntry::Mono(ty) | EnvEntry::MissingFormal(ty) => TypeScheme::monomorphic(ty),
                EnvEntry::Scheme(index) => context.scheme_arena[index as usize].clone(),
            };
            close_scheme(db, &mut context.table, open)
        } else {
            TypeScheme::monomorphic(unknown(db))
        };
        top_level_bindings.push((id, info.name, binding_scheme));
    }
    // A loop variable that shares its name with another exported slot leaves no record of
    // which slot is live at the end of the item, so that name exports as undetermined.
    let ambiguous: FxHashSet<_> = top_level_bindings
        .iter()
        .filter(|(id, name, _)| {
            naming.extras().loop_bound_slots.contains(id)
                && top_level_bindings
                    .iter()
                    .any(|(other, other_name, _)| other != id && other_name == name)
        })
        .map(|(_, name, _)| *name)
        .collect();
    let mut exported: FxHashSet<_> = FxHashSet::default();
    let top_level_bindings: Vec<_> = top_level_bindings
        .into_iter()
        .filter_map(|(_, name, scheme)| {
            if !ambiguous.contains(&name) {
                return Some((name, scheme));
            }
            exported
                .insert(name)
                .then(|| (name, TypeScheme::monomorphic(unknown(db))))
        })
        .collect();
    ItemCheck {
        match_findings: ThinVec::from(match_findings),
        i64_literals,
        checked_types,
        errors: ThinVec::from(errors),
        strict_origins: ThinVec::from(context.strict_origins),
        native_blockers,
        scheme,
        editor,
        masked_reads: context.masked_reads.iter().collect(),
        column_misses: context.column_misses.drain().collect(),
        top_level_bindings: ThinVec::from(top_level_bindings),
    }
}

/// Unbound constrained variables generalize into fresh binders; unconstrained ones
/// erase to `Unknown`. The only exit edge for a scheme, so it asserts the invariant
/// that no table-scoped variable leaks (a leaked id silently names a foreign variable).
fn close_scheme<'db>(
    db: &'db dyn Db,
    table: &mut InferenceTable<'db>,
    scheme: TypeScheme<'db>,
) -> TypeScheme<'db> {
    let mut abstracted = FxHashSet::default();
    collect_parameter_vars(db, table, scheme.body, &mut abstracted);
    let mut closer = ResidualCloser {
        binders: scheme.binders,
        generalized: FxHashMap::default(),
        abstracted,
    };
    let body = erase_residual_vars_at(db, table, scheme.body, 0, Some(&mut closer));
    debug_assert!(
        !contains_inference_var(db, body),
        "a closed scheme must carry no inference variable"
    );
    TypeScheme {
        binders: closer.binders,
        body,
    }
}

struct ResidualCloser<'db> {
    binders: ThinVec<Binder<'db>>,
    generalized: FxHashMap<InferenceVar, Ty<'db>>,
    /// Only variables some parameter abstracts generalize; any other constrained
    /// variable is one fixed type nobody chooses, so it takes the constraint's default.
    abstracted: FxHashSet<InferenceVar>,
}

fn collect_parameter_vars<'db>(
    db: &'db dyn Db,
    table: &InferenceTable<'db>,
    ty: Ty<'db>,
    found: &mut FxHashSet<InferenceVar>,
) {
    let resolved = table.shallow_resolve(db, ty);
    if let TyKind::Function(function) = resolved.kind(db) {
        let parameters = function
            .positional
            .iter()
            .copied()
            .chain(
                function
                    .named
                    .iter()
                    .flat_map(|parameter| parameter.types()),
            )
            .chain(function.variadic.iter().map(|rest| rest.element));
        for parameter in parameters {
            let mut vars = FxHashSet::default();
            table.collect_unbound_vars(db, parameter, &mut vars);
            found.extend(vars.into_iter().map(|var| table.find(var)));
        }
    }
    for_each_child_type(db, resolved, &mut |child| {
        collect_parameter_vars(db, table, child, found)
    });
}

/// Follows variable bindings only: named types stay unexpanded so an exported alias
/// displays by name.
fn erase_residual_vars_at<'db>(
    db: &'db dyn Db,
    table: &mut InferenceTable<'db>,
    ty: Ty<'db>,
    depth: usize,
    mut closer: Option<&mut ResidualCloser<'db>>,
) -> Ty<'db> {
    const ERASE_DEPTH_LIMIT: usize = 64;
    if depth >= ERASE_DEPTH_LIMIT {
        return unknown(db);
    }
    let resolved = table.shallow_resolve(db, ty);
    let TyKind::Var(var) = *resolved.kind(db) else {
        return map_child_types(db, resolved, &mut |child| {
            erase_residual_vars_at(db, table, child, depth + 1, closer.as_deref_mut())
        });
    };
    let Some(closer) = closer else {
        return unknown(db);
    };
    let Entry::Unbound {
        constraint, bound, ..
    } = *table.entry(var)
    else {
        return unknown(db);
    };
    if constraint == Constraint::Unconstrained {
        // A residual bounded variable is one type nobody chooses, so it is its bound.
        return match bound {
            Some(bound) => erase_residual_vars_at(db, table, bound, depth + 1, Some(closer)),
            None => unknown(db),
        };
    }
    // `Int` and `Frac` each admit one type, so a literal they describe is that type rather than
    // a type parameter.
    match constraint {
        Constraint::Int => return native_scalar(db, NativeScalar::I64),
        Constraint::Frac => return native_scalar(db, NativeScalar::F64),
        _ => {}
    }
    if !closer.abstracted.contains(&table.find(var)) {
        return match constraint {
            Constraint::Numeric | Constraint::Arithmetic | Constraint::ScalarNumeric => {
                scalar(db, Atomic::Double)
            }
            Constraint::WholeLiteral { .. }
            | Constraint::FracLiteral
            | Constraint::StrLiteral
            | Constraint::BoolLiteral => constraint
                .literal_default()
                .map_or(unknown(db), |atomic| scalar(db, atomic)),
            Constraint::AtomicElement
            | Constraint::Key
            | Constraint::ArrayItem
            | Constraint::Number
            | Constraint::Int
            | Constraint::Frac
            | Constraint::Eq
            | Constraint::Unconstrained => unknown(db),
        };
    }
    if let Some(&rigid) = closer.generalized.get(&var) {
        return rigid;
    }
    let mut ordinal = closer.binders.len() + 1;
    let name = loop {
        let candidate = Name::new(db, format!("#R{ordinal}"));
        if !closer
            .binders
            .iter()
            .any(|existing| existing.name == candidate)
        {
            break candidate;
        }
        ordinal += 1;
    };
    let rigid = Ty::new(db, TyKind::Rigid(name));
    closer.binders.push(Binder::constrained(name, constraint));
    closer.generalized.insert(var, rigid);
    rigid
}

struct Environment<'db, 'a> {
    entries: Column<'a, BindingId, EnvEntry<'db>>,
    /// What dominating tests showed about slots' values beyond their types. A write to a slot
    /// drops every fact that mentions it.
    facts: Vec<Fact<'db>>,
    undo: Vec<Undo<'db>>,
    /// Scratch for [`Environment::writes_since`], empty between calls.
    written: Bitset<'a, BindingId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fact<'db> {
    /// After `if (!is_loaded(package)) stop()`, `is_loaded(package)` holds.
    Holds(BindingId, Symbol),
    /// After `if (is.null(A) != is.null(b)) stop()`, the two are `NULL` together, so a test
    /// of one narrows the other.
    NullTogether(BindingId, BindingId),
    /// After `if (case == "a") s <- f(s)`, `s` holds `when_true` wherever a later test of `key`
    /// implies `condition`, and `when_false` wherever it implies the opposite.
    Correlated {
        slot: BindingId,
        key: BindingId,
        condition: ExprId,
        when_true: Ty<'db>,
        when_false: Ty<'db>,
    },
    /// Inside `if (case %in% c("a", "b"))`, `case` is one of those values (`negated` on the
    /// false edge), so a nested test of `case` decides an earlier correlated write together
    /// with this one.
    KeyDomain {
        key: BindingId,
        condition: ExprId,
        negated: bool,
    },
    /// After `if (is.null(node$left)) stop()` on a nominal `node`, `node$left` reads `ty`. A
    /// narrowed representation would no longer be the nominal, so the slot keeps its type.
    FieldNarrowed {
        slot: BindingId,
        field: Symbol,
        ty: Ty<'db>,
    },
}

impl Fact<'_> {
    fn mentions(self, slot: BindingId) -> bool {
        match self {
            Fact::Holds(held, _) => held == slot,
            Fact::NullTogether(first, second) => first == slot || second == slot,
            Fact::Correlated {
                slot: written, key, ..
            } => written == slot || key == slot,
            Fact::KeyDomain { key, .. } => key == slot,
            Fact::FieldNarrowed { slot: narrowed, .. } => narrowed == slot,
        }
    }
}

#[derive(Debug)]
enum Undo<'db> {
    Entry(BindingId, Option<EnvEntry<'db>>),
    Established(Fact<'db>),
    Dropped(Fact<'db>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvEntry<'db> {
    Mono(Ty<'db>),
    /// Index into `scheme_arena`, keeping the entry `Copy`.
    Scheme(u32),
    /// A no-default formal where `missing(name)` held, so a read errors. Carries the
    /// supplied-state type; any write restores an ordinary entry.
    MissingFormal(Ty<'db>),
}

impl<'db> Environment<'db, '_> {
    fn get(&self, slot: BindingId) -> Option<EnvEntry<'db>> {
        self.entries.get(slot).copied()
    }

    fn set(&mut self, slot: BindingId, entry: EnvEntry<'db>) {
        let previous = self.entries.insert(slot, entry);
        self.undo.push(Undo::Entry(slot, previous));
        self.forget(slot);
    }

    /// Drops the facts about `slot`, returning whether there were any. A joined write must
    /// call it even when the joined entry is unchanged, because the value is a different one.
    fn forget(&mut self, slot: BindingId) -> bool {
        if !self.facts.iter().any(|fact| fact.mentions(slot)) {
            return false;
        }
        let undo = &mut self.undo;
        self.facts.retain(|&fact| {
            let kept = !fact.mentions(slot);
            if !kept {
                undo.push(Undo::Dropped(fact));
            }
            kept
        });
        true
    }

    fn establish(&mut self, fact: Fact<'db>) {
        if !self.facts.contains(&fact) {
            self.facts.push(fact);
            self.undo.push(Undo::Established(fact));
        }
    }

    fn retract(&mut self, fact: Fact<'db>) {
        if let Some(position) = self.facts.iter().position(|&held| held == fact) {
            self.facts.remove(position);
            self.undo.push(Undo::Dropped(fact));
        }
    }

    fn holds_of(&self, slot: BindingId, predicate: Symbol) -> bool {
        self.facts.contains(&Fact::Holds(slot, predicate))
    }

    fn null_partners(&self, slot: BindingId) -> impl Iterator<Item = BindingId> + '_ {
        self.facts.iter().filter_map(move |fact| match *fact {
            Fact::NullTogether(first, second) if first == slot => Some(second),
            Fact::NullTogether(first, second) if second == slot => Some(first),
            _ => None,
        })
    }

    fn mark(&self) -> usize {
        self.undo.len()
    }

    fn rollback(&mut self, mark: usize) {
        while self.undo.len() > mark {
            match self.undo.pop().expect("undo length checked") {
                Undo::Entry(slot, Some(entry)) => {
                    self.entries.insert(slot, entry);
                }
                Undo::Entry(slot, None) => {
                    self.entries.remove(slot);
                }
                Undo::Established(fact) => self.facts.retain(|&held| held != fact),
                Undo::Dropped(fact) => self.facts.push(fact),
            }
        }
    }

    fn writes_since(&mut self, mark: usize) -> Vec<(BindingId, Option<EnvEntry<'db>>)> {
        let mut writes = Vec::new();
        for undo in &self.undo[mark..] {
            let Undo::Entry(slot, _) = undo else {
                continue;
            };
            if self.written.insert(*slot) {
                writes.push((*slot, self.entries.get(*slot).copied()));
            }
        }
        for (slot, _) in &writes {
            self.written.remove(*slot);
        }
        writes
    }
}

/// A map whose table lives in the check's arena, for the few facts whose key is not one of
/// the item's dense ids. The arena frees every superseded table in one step when the check ends.
type ArenaMap<'a, K, V> = HashMap<K, V, FxBuildHasher, Arena<'a>>;
type ArenaSet<'a, T> = HashSet<T, FxBuildHasher, Arena<'a>>;

/// The check's bump arena as a std allocator, whose frees are no-ops because the arena is reset
/// when the check ends. It implements std's trait rather than relying on bumpalo's
/// `allocator-api2` one, because the workspace's JIT turns on hashbrown's `nightly` feature,
/// which switches every hashbrown map in the build to std's trait.
#[derive(Clone, Copy)]
struct Arena<'a>(&'a Bump);

unsafe impl Allocator for Arena<'_> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let pointer = self.0.try_alloc_layout(layout).map_err(|_| AllocError)?;
        Ok(NonNull::slice_from_raw_parts(pointer, layout.size()))
    }

    unsafe fn deallocate(&self, _pointer: NonNull<u8>, _layout: Layout) {}
}

struct Checker<'db, 'a> {
    db: &'db dyn Db,
    module: &'a Module,
    naming: &'a ItemNaming,
    globals: Option<&'a dyn GlobalEnv<'db>>,
    /// Scratch space for the short-lived vectors of argument matching, which runs for every
    /// call and every overload probe. It is freed in one step when the check ends.
    arena: &'a Bump,
    /// Fetched once per check: the checker consults the stubs at most calls, and each salsa
    /// fetch records a dependency edge on the running query.
    library: Option<&'db StubLibrary<'db>>,
    table: InferenceTable<'db>,
    /// A binding's right-hand side infers one level deeper so a function generalizes at
    /// its binding; a stored monotype lowers back to this level, or a later
    /// generalization in the frame would quantify a variable the environment still holds.
    frame_level: u32,
    /// The frame level of the innermost anonymous function passed as a call argument. Its
    /// parameters are values the callee supplies, so the callee's signature, not the body,
    /// decides whether two of them are independent.
    callback_frame_level: Option<u32>,
    /// The `if` statement about to infer whose value its block discards.
    discarded_if: Option<ExprId>,
    /// Parameter types an unannotated function literal takes from the function type its call
    /// argument position expects, by formal position; `None` leaves a formal to inference.
    parameter_hints: Column<'a, ExprId, Vec<Option<Ty<'db>>>>,
    /// The ranges of the formals that took their type from a call's expected function type, so
    /// only findings on those say the call supplied the type.
    hinted_parameters: ArenaSet<'a, TextRange>,
    expression_annotations: Column<'a, ExprId, Annotation<'db>>,
    environment: Environment<'db, 'a>,
    scheme_arena: Vec<TypeScheme<'db>>,
    /// The reads that may keep a literal variable open, because they are an operand whose
    /// other operand may decide it. Every other read decides it as R's type.
    literal_reads: Bitset<'a, ExprId>,
    recorded: RecordedTypes<'db>,
    recorded_patterns: Column<'a, PatId, Ty<'db>>,
    recorded_expectations: Column<'a, ExprId, Ty<'db>>,
    recorded_loop_variables: Column<'a, ExprId, Ty<'db>>,
    /// The variables the item's own generalization turned into binders, so a local the
    /// signature's `T` types reads as that `T` in the editor rather than as a fresh variable.
    root_binders: FxHashMap<InferenceVar, Name<'db>>,
    selected_overloads: Column<'a, ExprId, usize>,
    callee_targets: Column<'a, ExprId, CalleeTarget>,
    masked_reads: Bitset<'a, ExprId>,
    /// Library calls whose untyped arguments left the candidates disagreeing.
    undecided_calls: Bitset<'a, ExprId>,
    /// How many enclosing `[` index a value that may be a data.table of unknown columns.
    unseen_frames: u32,
    /// Queries being inferred, innermost last: a bare name inside one reads a column.
    column_scopes: Vec<ColumnScope<'db>>,
    column_misses: Column<'a, ExprId, ColumnMiss>,
    /// Frame queries being inferred. data.table syntax makes naming resolve a whole
    /// bracket quietly, but inside a frame query ry knows the columns, so a quiet read
    /// that is no column still reads its global.
    query_depth: u32,
    errors: Vec<TypeError<'db>>,
    /// The indices into `errors` of errors whose value was checked against a fixed type, an
    /// annotation's or a ry record field's: the written slot takes that type whatever the
    /// value, so the error leaves the item's export trustworthy. Kept in step with `errors`
    /// by `truncate_errors` and `remove_error`.
    contained_errors: Vec<usize>,
    /// Off outside strict files, where nothing reports the origins.
    record_strict_origins: bool,
    strict_origins: Vec<StrictOrigin>,
    /// The file is strict or written with ry syntax, so a literal bound outside any function is
    /// a ry value: nothing outside its own top-level statement can decide its type.
    top_level_ry_values: bool,
    /// Non-zero while probing an overload candidate: the literal-as-integer courtesy is
    /// off so it cannot decide the winner (exact matches outrank conversions).
    overload_probe_depth: u32,
    /// The arguments being matched belong to a function that takes a ry value, so a number
    /// literal passed to one of its unbounded type parameters (a fold's initial state) is a
    /// literal its use decides, as `List.walk([1, 2], 0, Num.add)` decides `0` is an `I64`.
    ry_callee: bool,
    /// Non-zero while matching an operator method's candidates. An open operand's arithmetic
    /// must not fit `-.Date`'s `Date` parameter there, or `day - 2 * x` would be a `difftime`.
    operator_method_depth: u32,
    /// The first variable the ordinary call being dispatched created. An older variable in
    /// its callee's type belongs to an enclosing scope, as in a closure that returns one.
    call_first_var: u32,
    /// Per enclosing function, each early `return` value with its range, so a
    /// declared-return mismatch blames the offending `return`.
    return_frames: Vec<Vec<EarlyReturn<'db>>>,
    /// The ranges of `return(NA)` values, whose `logical` type stands for a missing value of
    /// whatever atomic type the function's other returns have.
    bare_na_returns: FxHashSet<TextRange>,
    /// `<<-` writes to enclosing slots, re-applied after each enclosing body's rollback
    /// so the join survives at the definition site.
    pending_enclosing_writes: Vec<(BindingId, Ty<'db>)>,
    /// Slots a closure read before any write reached them (the letrec shape).
    forward_captured: Bitset<'a, BindingId>,
    /// Variable-erased join of every write to a captured slot, surviving rollbacks;
    /// forward-capture reads resolve here instead of the empty definition-point entry.
    capture_joins: Column<'a, BindingId, Ty<'db>>,
    /// Each write to a captured slot by source position, with its variable-erased type.
    capture_writes: Column<'a, BindingId, SmallVec<[(TextSize, LateWrite<'db>); 2]>>,
    /// The earliest closure read of each slot that found an entry; a write after it widens it.
    late_reads: Column<'a, BindingId, TextSize>,
    /// Closure reads a newly seen later write widens: the body holding each one re-checks.
    late_triggers: SmallVec<[TextSize; 2]>,
    /// The function body being inferred.
    body_range: TextRange,
    /// Closures were inferred against an incomplete capture join, so the body re-checks once.
    capture_repass_needed: bool,
    /// Reads of a captured slot before any write, whose `Unknown` is a placeholder the
    /// capture repass replaces, like a pending group export ([`GlobalEnv::pending_reads`]).
    placeholder_reads: usize,
    /// Per function body: the variable each `<<-`-written enclosing slot reads beside its
    /// enclosing type on entry, and whether the body's first write has bound it yet. A body
    /// seeds the few slots it writes with `<<-`, so the list is searched, not indexed.
    superassign_seeds: SmallVec<[SuperassignSeed<'db>; 2]>,
    /// Top-level slots whose unwritten path read the cross-item binding: a pre-state
    /// that loop passes re-establish after their rollback.
    pre_materialized: Column<'a, BindingId, Ty<'db>>,
    /// Per loop pass being checked, innermost last: the states `break` and `next` leave.
    loop_jumps: Vec<LoopJumps<'db>>,
    no_default_formals: Bitset<'a, BindingId>,
    /// Unannotated formals with a default; a condition on one does not pin its type.
    defaulted_formals: Bitset<'a, BindingId>,
    /// Formals given a function on their `missing()` path, which the branch join reads as a
    /// default rather than a rewrite.
    missing_path_functions: Bitset<'a, BindingId>,
    /// Slots for names this item reads but another item binds, so guard refinements live
    /// in the environment like any slot's. Ids are minted above naming's so they never collide.
    non_local_slots: ArenaMap<'a, Symbol, BindingId>,
    next_non_local_slot: u32,
    /// The write of a binding whose only write is literal strings (`"a"` or `c("a", "b")`),
    /// compared by site so a re-pass over the same write keeps it. `None` once a second write
    /// site, or a write of anything else, appears. Queries read it for column names given through a variable
    /// (`.SDcols = cols`, `(cols) := ...`).
    string_values: Column<'a, BindingId, Option<StringWrite>>,
    /// Slots read somewhere other than as a call head, built on first need.
    value_read_slots: Option<Bitset<'a, BindingId>>,
    /// Slots whose type the code inferred rather than a declaration stated: those an
    /// unannotated assignment in the body has written so far, and undeclared formals, whose
    /// `NULL` is a placeholder default (NlcOptim's `solqp(H = NULL, ...)`) or the shape an
    /// `is.null()` test gave them (Rsolnp's `if (is.null(eqfun)) ...`).
    computed_slots: Bitset<'a, BindingId>,
    /// Slots whose latest write so far is a lookup in a collection (`handler <- handlers[[name]]`),
    /// so a call through them must suit every member the collection holds.
    lookup_slots: Bitset<'a, BindingId>,
    /// Slots that some write so far gave a type other than `NULL`, so a `NULL` read from them
    /// is a path the code takes rather than a placeholder.
    valued_slots: Bitset<'a, BindingId>,
    /// The item's `<<-` target names, sorted.
    super_assigned: Vec<Symbol>,
    /// The call building the item's value when a later top-level `body(name) <-` replaces
    /// that value's code: the closure it builds never runs as written, so a function argument
    /// is not held to what that closure would do with it.
    template_call: Option<TextRange>,
}

/// How a call head's function lookup went; see [`Checker::function_lookup`].
enum FunctionLookup<'db> {
    /// The ordinary path applies: the head is no bare name, or nothing was skipped.
    Ordinary,
    /// The callee's type, already recorded for the head.
    Found(Ty<'db>),
    /// The library's candidates, past every project binding of the name.
    Library(Symbol, &'db [TypeScheme<'db>]),
    /// The nearest binding is untyped and also read as a value, and an outer binding of the
    /// name holds a function, so the call may reach either one and is not checked.
    Undetermined,
}

/// Ordered so that a union is as callable as its most callable member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Callable {
    No,
    Untyped,
    Yes,
}

/// A write to a captured slot, as a closure defined before it may observe it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct LateWrite<'db> {
    /// Variable-erased; an open R literal's default.
    ty: Ty<'db>,
    /// The constraint of an open R literal, which may take the reading type instead.
    literal: Option<Constraint>,
}

/// The variable an enclosing slot reads on entry to a body that writes it with `<<-`.
#[derive(Clone, Copy)]
struct SuperassignSeed<'db> {
    slot: BindingId,
    variable: Ty<'db>,
    /// The body's first write has bound the variable.
    written: bool,
}

/// The evaluation frame of one query `f[i, j, by]`.
struct ColumnScope<'db> {
    /// `None` when the frame's columns are not known, which makes every bare name silent.
    columns: Option<Vec<RecordField<'db>>>,
    frame: Option<Symbol>,
    /// A binding made inside the bracket (a closure's parameter in `j`) stays lexical.
    bracket: TextRange,
    subset_data: Ty<'db>,
    /// Called names inside the bracket, which never read a column, in ascending order.
    callees: SmallVec<[ExprId; 8]>,
    /// The two sides of a join, which `j` can also read as `x.col` and `i.col`.
    join: Option<JoinSides<'db>>,
}

/// The queried frame `x` and the joined frame `i` of `x[i, on = keys]`, each `None` when its
/// columns are not known.
struct JoinSides<'db> {
    x: Option<Vec<RecordField<'db>>>,
    i: Option<Vec<RecordField<'db>>>,
}

/// One `on =` condition: `x` names the queried frame's column and `y` the joined frame's.
struct JoinKey {
    x: String,
    y: String,
    operator: JoinOperator,
    x_range: TextRange,
    y_range: TextRange,
}

impl JoinKey {
    /// The condition as written in data.table's string form, for a diagnostic.
    fn text(&self) -> String {
        format!("{} {} {}", self.x, self.operator.symbol(), self.y)
    }
}

/// The comparison of a join condition. Only `==` compares strings; an ordering compares numbers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JoinOperator {
    Equal,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl JoinOperator {
    fn symbol(self) -> &'static str {
        match self {
            JoinOperator::Equal => "==",
            JoinOperator::Less => "<",
            JoinOperator::LessEqual => "<=",
            JoinOperator::Greater => ">",
            JoinOperator::GreaterEqual => ">=",
        }
    }
}

/// One column of a join's output. The layout mirrors the engine's `join_layout` in
/// `crates/table/src/query.rs`, which is the authority on names and order.
struct JoinedColumn<'db> {
    field: RecordField<'db>,
    /// Whether the values come from the joined frame `i`.
    from_i: bool,
    /// The index of the `on` condition whose `i` values the column carries.
    condition: Option<usize>,
}

/// Inferred once before signature matching, so an overload probe re-matches without
/// re-running inference.
#[derive(Clone)]
struct CallArgument<'db> {
    name: Option<Symbol>,
    name_range: Option<TextRange>,
    /// `None` is a positional hole (`f(, x)`).
    ty: Option<Ty<'db>>,
    range: TextRange,
    /// Types carry no ranges, so a per-field finding re-walks the producing expression.
    value: Option<ExprId>,
    /// A whole-number double literal (`1`, `2.0`): eligible for the literal-as-integer courtesy.
    whole_double: bool,
    /// The enclosing function's bare `...`: forwards an unknown number of arguments, so
    /// the call cannot be arity-checked.
    forwards_dots: bool,
    /// What a character argument stands for where the callee expects a function: the
    /// callee passes it through `match.fun`, so `"toupper"` means the function `toupper`.
    /// `Unknown` when the string is not a literal or names no known function.
    named_function: Option<Ty<'db>>,
}

/// The packages R attaches in every session, whose exports a call can reach without `library()`.
pub const DEFAULT_SEARCH_PATH: &[&str] = &[
    "base",
    "methods",
    "datasets",
    "utils",
    "grDevices",
    "graphics",
    "stats",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandShape {
    Scalar,
    Vector,
}

/// How one level of a replacement spine (`x$a`, `x[["a"]]`, `x[[1]]`, `x[[i]]`) reaches
/// into its container.
#[derive(Clone)]
enum ReplacementStep {
    Field(String),
    Position(usize),
    Element,
    /// `x[i]` or `x[i, j]`, which coerces an atomic container to the wider atomic type.
    Slice,
    /// `x@a`, or a replacement call such as `names(x)`: not modeled.
    Other,
    /// `formals(f) <- v` or `body(f) <- v` rewrites the function itself, so its prior
    /// signature no longer describes it.
    Rewrite,
    /// `names(x) <- v`, which names a list's items.
    Names,
}

/// Where a callee came from. A lookup in a collection (`handlers[[name]]`, `handlers$add`)
/// may yield any member, so a call must suit them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalleeSource {
    Lookup,
    Value,
}

/// Which control-flow edge a join merges. A branch that writes an untyped value leaves the
/// variable untyped, but a loop's back edge keeps the other side: a pass that recovered from
/// an error with `Unknown` must not erase the type the error was reported against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinEdge {
    Branch,
    LoopBack,
}

/// The writes a loop pass's `break`s and `next`s carry: a `break` leaves the loop with them and a
/// `next` starts the next iteration with them, so each joins where its jump lands rather than
/// at the end of the body. `mark` is the pass's environment mark; `breaks` keeps one state per
/// `break`, because a `repeat` leaves with exactly the states its `break`s saw.
#[derive(Default)]
struct LoopJumps<'db> {
    mark: usize,
    breaks: Vec<Vec<(BindingId, Option<EnvEntry<'db>>)>>,
    nexts: Vec<(BindingId, Option<EnvEntry<'db>>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VectorIndexShape {
    ScalarLike,
    VectorLike,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogicalOperand {
    Scalar,
    Vector,
    /// `Any`, the deliberate opt-out, which the result inherits.
    Any,
    /// `Unknown`: the result is undetermined too, with no origin of its own, because the
    /// operand's type was lost where it was computed.
    Unknown,
}

enum NumericOperand<'db> {
    Concrete(OperandShape, Atomic),
    ConcreteUnion(Vec<(OperandShape, Atomic)>),
    Flexible(Ty<'db>),
    /// Vector shape known, element not: a generic variable/rigid to constrain, or `None`
    /// for `Any`/`Unknown`.
    FlexibleVector(Option<Ty<'db>>),
    /// A union of numbers and members whose arithmetic the checker does not compute: a class
    /// with operator methods (`matrix | double`) or an open parameter (`T | integer[]`). The
    /// open members are bound to numbers, and the result is `Unknown`.
    OpenUnion(Vec<Ty<'db>>),
    AnyUnknown,
    Invalid,
}

impl NumericOperand<'_> {
    fn concrete_parts(&self) -> Option<Vec<(OperandShape, Atomic)>> {
        match self {
            NumericOperand::Concrete(shape, atomic) => Some(vec![(*shape, *atomic)]),
            NumericOperand::ConcreteUnion(parts) => Some(parts.clone()),
            _ => None,
        }
    }
}

/// A recognized type-guard condition: what each edge refines. `a && b` refines its true
/// edge by both tests and `a || b` its false edge, so one condition can refine several slots.
#[derive(Default)]
struct GuardRefinement<'db> {
    true_edge: Vec<SlotRefinement<'db>>,
    false_edge: Vec<SlotRefinement<'db>>,
}

#[derive(Clone, Copy)]
enum SlotRefinement<'db> {
    Narrow(BindingId, Ty<'db>),
    /// The slot holds that very polymorphic function (an index into the scheme arena), so
    /// each read instantiates it afresh.
    NarrowScheme(BindingId, u32),
    /// `missing(name)` on a no-default formal: the edge marks the slot read-erroring
    /// instead of refining it.
    Missing(BindingId),
    /// An edge that records what the test showed beyond a type: `is_loaded(package)` holds,
    /// which a `null_unless` callee reads, or two slots are `NULL` together.
    Establish(Fact<'db>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardKind {
    Null,
    Family(GuardFamily),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardFamily {
    Character,
    Logical,
    Integer,
    Double,
    Numeric,
    Function,
    List,
}

fn is_type_probe(name: &str) -> bool {
    match name {
        "typeof" | "class" | "mode" | "storage.mode" | "inherits" | "is.object" | "oldClass" => {
            true
        }
        "is.null" | "is.na" | "is.nan" | "is.finite" | "is.infinite" | "is.element"
        | "is.unsorted" | "is.loaded" => false,
        _ => name.starts_with("is."),
    }
}

/// rlang's type predicates (`is_string`, `is_function`, `is_bare_list`, `is_scalar_integer`),
/// which rlang, purrr and the packages that vendor rlang's standalone files define themselves.
/// They test a type like `is.character` does, whether the project defines them or imports them.
fn is_tidy_type_predicate(name: &str) -> bool {
    if matches!(
        name,
        "is_condition" | "is_error" | "is_warning" | "is_message"
    ) {
        return true;
    }
    let Some(rest) = name.strip_prefix("is_") else {
        return false;
    };
    let rest = rest
        .strip_prefix("scalar_")
        .or_else(|| rest.strip_prefix("bare_"))
        .unwrap_or(rest);
    matches!(
        rest,
        "string"
            | "bool"
            | "character"
            | "logical"
            | "integer"
            | "integerish"
            | "double"
            | "numeric"
            | "complex"
            | "raw"
            | "list"
            | "vector"
            | "atomic"
            | "function"
            | "closure"
            | "primitive"
            | "formula"
            | "quosure"
            | "environment"
            | "symbol"
            | "call"
            | "lang"
            | "expression"
            | "pairlist"
            | "node"
            | "node_list"
            | "lambda"
            | "callable"
            | "symbolic"
            | "syntactic_literal"
            | "quosures"
            | "namespace"
            | "weakref"
    )
}

fn guard_kind(name: &str) -> Option<GuardKind> {
    Some(match name {
        "is.null" => GuardKind::Null,
        "is.character" => GuardKind::Family(GuardFamily::Character),
        // `isTRUE(x)` and `isFALSE(x)` hold only for a single logical, and code that tests
        // `isFALSE(result)` on a `FALSE | list` result reads the list on the other edge, as car
        // does throughout. A logical that fails the test is `TRUE`, which correct R reads as no
        // list, so the other edge drops the logical as `is.logical` does.
        "is.logical" | "isTRUE" | "isFALSE" => GuardKind::Family(GuardFamily::Logical),
        "is.integer" => GuardKind::Family(GuardFamily::Integer),
        "is.double" => GuardKind::Family(GuardFamily::Double),
        "is.numeric" => GuardKind::Family(GuardFamily::Numeric),
        "is.function" => GuardKind::Family(GuardFamily::Function),
        "is.list" => GuardKind::Family(GuardFamily::List),
        _ => return None,
    })
}

/// `fits` with each candidate's result in place of its narrowings, the shape the unchecked
/// round records.
fn fits_with_results<'db>(
    fits: &[(usize, bool, Vec<Ty<'db>>)],
    fit_results: &[Ty<'db>],
) -> Vec<(usize, bool, Vec<Ty<'db>>)> {
    fits.iter()
        .zip(fit_results)
        .map(|(&(index, free, _), &result)| (index, free, vec![result]))
        .collect()
}

/// The formal `UseMethod` dispatches on when the body calls it as a statement: the one named
/// by its `object` argument, or else the first formal, unless that is `...`.
fn dispatch_formal(module: &Module, body: ExprId, parameters: &[Formal]) -> Option<Symbol> {
    let use_method_arguments = |id: ExprId| match module.kind(id) {
        ExpressionKind::Call { callee, arguments } if matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == "UseMethod") => {
            Some(&module[arguments])
        }
        _ => None,
    };
    let arguments = match module.kind(body) {
        ExpressionKind::Block { statements } => module[statements]
            .iter()
            .find_map(|&statement| use_method_arguments(statement)),
        _ => use_method_arguments(body),
    }?;
    let named = |formal: &str| {
        arguments.iter().find(|argument| {
            argument
                .name
                .is_some_and(|name| module.name(name) == formal)
        })
    };
    // Positional arguments fill `generic`, then `object`, whichever is not named.
    let object = named("object").or_else(|| {
        let skip = usize::from(named("generic").is_none());
        arguments
            .iter()
            .filter(|argument| argument.name.is_none())
            .nth(skip)
    });
    let name = match object {
        // Any other expression dispatches on a value no formal names.
        Some(argument) => match argument.value.map(|value| module.kind(value)) {
            Some(ExpressionKind::NameRef(name)) => *name,
            _ => return None,
        },
        None => parameters.first()?.name,
    };
    (name != "..." && parameters.iter().any(|parameter| parameter.name == name)).then_some(name)
}

/// The callee and `x` of a call `length(x)`.
fn length_operand(module: &Module, id: ExprId) -> Option<(ExprId, ExprId)> {
    let ExpressionKind::Call { callee, arguments } = module.kind(id) else {
        return None;
    };
    if !matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == "length") {
        return None;
    }
    match &module[arguments][..] {
        [argument] if argument.name.is_none() => Some((*callee, argument.value?)),
        _ => None,
    }
}

/// A test of one local against literal values: `case == "a"`, `case %in% c("a", "b")`, their
/// negations, or a bare flag.
#[derive(Debug, Clone)]
struct KeyTest {
    key: BindingId,
    values: SmallVec<[KeyValue; 4]>,
    /// The test holds when the local is none of `values`.
    excluded: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum KeyValue {
    Text(String),
    Number(f64),
    Logical(bool),
}

impl KeyTest {
    fn negated(&self) -> KeyTest {
        normalized_key_test(KeyTest {
            key: self.key,
            values: self.values.clone(),
            excluded: !self.excluded,
        })
    }

    /// The values passing both `self` and `domain`, a test of the same local that holds where
    /// `self` is asked. Literals of mixed kinds compare by coercion, so they keep `self`.
    fn within(&self, domain: &KeyTest) -> KeyTest {
        let kind = |value: &KeyValue| std::mem::discriminant(value);
        let mut kinds = self.values.iter().chain(&domain.values).map(kind);
        if kinds
            .next()
            .is_some_and(|first| kinds.any(|other| other != first))
        {
            return self.clone();
        }
        let (values, excluded) = match (self.excluded, domain.excluded) {
            (false, false) => (
                self.values
                    .iter()
                    .filter(|value| domain.values.contains(value))
                    .cloned()
                    .collect(),
                false,
            ),
            (false, true) => (
                self.values
                    .iter()
                    .filter(|value| !domain.values.contains(value))
                    .cloned()
                    .collect(),
                false,
            ),
            (true, false) => (
                domain
                    .values
                    .iter()
                    .filter(|value| !self.values.contains(value))
                    .cloned()
                    .collect(),
                false,
            ),
            (true, true) => {
                let mut values = self.values.clone();
                values.extend(
                    domain
                        .values
                        .iter()
                        .filter(|value| !self.values.contains(value))
                        .cloned(),
                );
                (values, true)
            }
        };
        KeyTest {
            key: self.key,
            values,
            excluded,
        }
    }

    /// Whether every value passing `self` passes `other`, for tests of the same local.
    fn implies(&self, other: &KeyTest) -> bool {
        // R coerces before it compares, so `"1" == 1` and `TRUE == 1` hold: only literals of
        // one kind are told apart by their values.
        let kind = |value: &KeyValue| std::mem::discriminant(value);
        let mut values = self.values.iter().chain(&other.values);
        let Some(first) = values.next().map(kind) else {
            return false;
        };
        if values.any(|value| kind(value) != first) {
            return false;
        }
        match (self.excluded, other.excluded) {
            (false, false) => self.values.iter().all(|value| other.values.contains(value)),
            (false, true) => self
                .values
                .iter()
                .all(|value| !other.values.contains(value)),
            (true, true) => other.values.iter().all(|value| self.values.contains(value)),
            // An exclusion leaves infinitely many values, which no finite set holds.
            (true, false) => false,
        }
    }
}

fn key_test(
    module: &Module,
    resolutions: &SortedMap<ExprId, BindingId>,
    id: ExprId,
) -> Option<KeyTest> {
    let local = |id: ExprId| match module.kind(id) {
        ExpressionKind::NameRef(_) => resolutions.get(&id).copied(),
        _ => None,
    };
    let test = match module.kind(id) {
        ExpressionKind::Paren(inner) => return key_test(module, resolutions, *inner),
        ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => return Some(key_test(module, resolutions, *operand)?.negated()),
        ExpressionKind::NameRef(_) => KeyTest {
            key: local(id)?,
            values: smallvec![KeyValue::Logical(true)],
            excluded: false,
        },
        ExpressionKind::Binary {
            operator: operator @ (BinaryOperator::Equal | BinaryOperator::NotEqual),
            lhs,
            rhs,
            ..
        } => {
            let (key, value) = match (local(*lhs), local(*rhs)) {
                (Some(key), None) => (key, *rhs),
                (None, Some(key)) => (key, *lhs),
                _ => return None,
            };
            KeyTest {
                key,
                values: smallvec![key_value(module, value)?],
                excluded: *operator == BinaryOperator::NotEqual,
            }
        }
        ExpressionKind::Binary {
            operator: BinaryOperator::Special,
            special_name: Some(name),
            lhs,
            rhs,
        } if name.as_str() == "%in%" => KeyTest {
            key: local(*lhs)?,
            values: key_values(module, resolutions, *rhs)?,
            excluded: false,
        },
        _ => return None,
    };
    Some(normalized_key_test(test))
}

/// A flag is `TRUE` or `FALSE` in an `if` (`NA` stops it), so excluding one is the other.
fn normalized_key_test(test: KeyTest) -> KeyTest {
    let logical = |value: &KeyValue| matches!(value, KeyValue::Logical(_));
    if !test.excluded || test.values.is_empty() || !test.values.iter().all(logical) {
        return test;
    }
    let values = [true, false]
        .into_iter()
        .map(KeyValue::Logical)
        .filter(|value| !test.values.contains(value))
        .collect();
    KeyTest {
        key: test.key,
        values,
        excluded: false,
    }
}

fn key_value(module: &Module, id: ExprId) -> Option<KeyValue> {
    match module.kind(id) {
        // The text keeps its escapes, and a raw string's backslashes are literal, so a string
        // with one may equal a differently spelled literal: it is no key.
        ExpressionKind::Literal(LiteralKind::String(text)) => {
            let text = &module[text];
            (!text.contains('\\')).then(|| KeyValue::Text(text.to_string()))
        }
        ExpressionKind::Literal(LiteralKind::Logical(value)) => Some(KeyValue::Logical(*value)),
        _ => numeric_literal(module, id).map(KeyValue::Number),
    }
}

/// The literals of `"a"` or `c("a", "b")`.
fn key_values(
    module: &Module,
    resolutions: &SortedMap<ExprId, BindingId>,
    id: ExprId,
) -> Option<SmallVec<[KeyValue; 4]>> {
    let ExpressionKind::Call { callee, arguments } = module.kind(id) else {
        return Some(smallvec![key_value(module, id)?]);
    };
    if !matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == "c")
        || resolutions.contains_key(callee)
    {
        return None;
    }
    module[arguments]
        .iter()
        .map(|argument| match argument.name {
            None => key_value(module, argument.value?),
            Some(_) => None,
        })
        .collect()
}

fn numeric_literal(module: &Module, id: ExprId) -> Option<f64> {
    match module.kind(id) {
        ExpressionKind::Literal(LiteralKind::Integer(text) | LiteralKind::Double(text)) => {
            module[text].trim_end_matches('L').parse().ok()
        }
        _ => None,
    }
}

/// The operator that keeps `a op b` true with its operands swapped.
fn mirrored(operator: BinaryOperator) -> BinaryOperator {
    match operator {
        BinaryOperator::Less => BinaryOperator::Greater,
        BinaryOperator::Greater => BinaryOperator::Less,
        BinaryOperator::LessEq => BinaryOperator::GreaterEq,
        BinaryOperator::GreaterEq => BinaryOperator::LessEq,
        other => other,
    }
}

fn guard_members<'db>(db: &'db dyn Db, resolved: Ty<'db>) -> Vec<Ty<'db>> {
    match resolved.kind(db) {
        TyKind::Union(members) => members.to_vec(),
        _ => vec![resolved],
    }
}

/// `None` when not statically decidable; the member then stays on both edges.
fn family_membership<'db>(db: &'db dyn Db, member: Ty<'db>, family: GuardFamily) -> Option<bool> {
    match member.kind(db) {
        TyKind::Null => Some(false),
        TyKind::Scalar(atomic) => Some(atomic_in_family(*atomic, family)),
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Scalar(atomic) => Some(atomic_in_family(*atomic, family)),
            _ => None,
        },
        TyKind::List(_) | TyKind::NamedList(_) | TyKind::Tuple(_) | TyKind::Record(_) => {
            Some(family == GuardFamily::List)
        }
        TyKind::Function(_) => Some(family == GuardFamily::Function),
        _ => None,
    }
}

/// Whether `typeof` of a value of type `member` can be `label`; `None` when not decidable.
fn typeof_label_membership<'db>(db: &'db dyn Db, member: Ty<'db>, label: Symbol) -> Option<bool> {
    let family = match label.as_str() {
        "NULL" => {
            return match member.kind(db) {
                TyKind::Null => Some(true),
                TyKind::Var(_)
                | TyKind::Rigid(_)
                | TyKind::Named(..)
                | TyKind::Any
                | TyKind::Unknown => None,
                _ => Some(false),
            };
        }
        "logical" => GuardFamily::Logical,
        "integer" => GuardFamily::Integer,
        "double" => GuardFamily::Double,
        "character" => GuardFamily::Character,
        "list" => GuardFamily::List,
        "closure" | "builtin" | "special" => GuardFamily::Function,
        _ => return None,
    };
    family_membership(db, member, family)
}

fn atomic_in_family(atomic: Atomic, family: GuardFamily) -> bool {
    match family {
        GuardFamily::Character => atomic == Atomic::Character,
        GuardFamily::Logical => atomic == Atomic::Logical,
        GuardFamily::Integer => atomic == Atomic::Integer,
        GuardFamily::Double => atomic == Atomic::Double,
        GuardFamily::Numeric => matches!(atomic, Atomic::Integer | Atomic::Double),
        GuardFamily::Function | GuardFamily::List => false,
    }
}

/// How a function body touches names, for deciding which no-default formals a call must
/// supply. R evaluates arguments lazily, so omitting one is an error only when the body
/// forces it.
#[derive(Default)]
struct FormalUses {
    /// The formals of the function being walked. Only their reads and writes are recorded,
    /// because every question asked of the walk is about a formal, and every other name
    /// would only grow the sets that each branch clones and joins.
    tracked: Rc<[Symbol]>,
    missing_tested: FxHashSet<Symbol>,
    /// Names read on every path before any write of them on that path.
    forced: FxHashSet<Symbol>,
    /// Names some path wrote before forcing them. A later read does not force such a name,
    /// because on that path the call never needed the argument.
    assigned: FxHashSet<Symbol>,
    /// The names forced on every path that leaves through `return()`, or `None` when none
    /// does. A formal read only after `if (fo == "A1") return("")` is not forced on the
    /// path that returns.
    returned: Option<FxHashSet<Symbol>>,
    /// Names some path reads before writing them, so the argument's value is used.
    evaluated: FxHashSet<Symbol>,
    /// Names passed bare to `substitute()`, which reads the argument's code, not its value.
    substituted: FxHashSet<Symbol>,
    /// For each local bound to a function literal, the formals its body forces on every path.
    /// They count as forced where a call through the local runs the body.
    closures: FxHashMap<Symbol, Rc<[Symbol]>>,
    /// The expressions whose value the walked function returns. A closure there is a
    /// factory's product, made to be called, so its reads count as forced.
    results: Vec<ExprId>,
}

impl FormalUses {
    /// withr's `wrap <- function(f, pre) { pre <- substitute(pre); ... }` splices `pre` into a
    /// function built at run time, so the argument never runs in the caller's frame.
    fn quotes(&self, name: Symbol) -> bool {
        self.substituted.contains(&name) && !self.evaluated.contains(&name)
    }

    fn makes_optional(&self, name: Symbol) -> bool {
        self.missing_tested.contains(&name) || !self.forces_on_every_path(name)
    }

    fn forces_on_every_path(&self, name: Symbol) -> bool {
        self.forced.contains(&name)
            && self
                .returned
                .as_ref()
                .is_none_or(|returned| returned.contains(&name))
    }

    fn nested(&self) -> FormalUses {
        FormalUses {
            tracked: self.tracked.clone(),
            ..FormalUses::default()
        }
    }

    fn record_read(&mut self, name: Symbol) {
        if self.tracked.contains(&name) && !self.assigned.contains(&name) {
            self.forced.insert(name);
            self.evaluated.insert(name);
        }
    }

    /// Any write rebinds the name, so a closure it held no longer runs through it.
    fn record_write(&mut self, name: Symbol) {
        self.closures.remove(&name);
        if self.tracked.contains(&name) && !self.forced.contains(&name) {
            self.assigned.insert(name);
        }
    }

    /// A call runs a closure whose body forces `forced`, unless this path wrote the name first.
    fn record_closure_run(&mut self, forced: &[Symbol]) {
        for &name in forced {
            if !self.assigned.contains(&name) {
                self.forced.insert(name);
            }
        }
    }

    fn record_return(&mut self) {
        self.returned = Some(match self.returned.take() {
            None => self.forced.clone(),
            Some(returned) => returned.intersection(&self.forced).copied().collect(),
        });
    }
}

/// Whether base R's `function` reads its parameter `parameter` only after testing it with
/// `missing()`, and gives it no default, so a missing formal passed to it by name stays
/// unforced: `Reduce(f, x, init = .init)` and `tryCatch(expr, finally = finally)`. Stubs
/// record only that a parameter is optional, which a default also makes it, and passing a
/// missing formal to a defaulted parameter that the callee reads is still an error.
fn tests_missing_without_default(function: &str, parameter: &str) -> bool {
    matches!(
        (function, parameter),
        ("Reduce", "init") | ("tryCatch", "finally")
    )
}

/// The names any non-diverging branch assigned before forcing them. When every branch
/// diverges, the code after the join is unreachable and keeps `before`.
fn join_assigned(
    before: FxHashSet<Symbol>,
    branches: impl IntoIterator<Item = Option<FxHashSet<Symbol>>>,
) -> FxHashSet<Symbol> {
    branches
        .into_iter()
        .flatten()
        .reduce(|mut joined, branch| {
            joined.extend(branch);
            joined
        })
        .unwrap_or(before)
}

fn is_empty_list(db: &dyn Db, ty: Ty<'_>) -> bool {
    matches!(ty.kind(db), TyKind::Tuple(items) if items.is_empty())
}

/// The names every branch forces, diverging branches included: R reports a missing
/// argument only on the path that reads it, so a formal one branch never touches is
/// optional.
fn join_forced(branches: impl IntoIterator<Item = FxHashSet<Symbol>>) -> FxHashSet<Symbol> {
    branches
        .into_iter()
        .reduce(|joined, branch| joined.intersection(&branch).copied().collect())
        .unwrap_or_default()
}

/// The `x` of rlang's splice `!!!x`: exactly three negations, since `!!x` injects one value.
fn splice_operand(module: &Module, value: ExprId) -> Option<ExprId> {
    let mut operand = value;
    for _ in 0..3 {
        let ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand: inner,
        } = module.kind(operand)
        else {
            return None;
        };
        operand = *inner;
    }
    match module.kind(operand) {
        ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            ..
        } => None,
        _ => Some(operand),
    }
}

/// What argument matching needs to know about one argument of a call.
#[derive(Debug, Clone, Copy)]
pub struct ArgumentShape<'a> {
    pub name: Option<&'a str>,
    /// The enclosing function's bare `...`, which forwards an unknown number of arguments.
    pub forwards_dots: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentTarget {
    Positional(usize),
    /// Index into `named`, whether claimed by name or filled positionally.
    Named(usize),
    Rest,
    /// A name no formal declares on a non-variadic function, or a duplicate of one
    /// already supplied.
    UnmatchedName,
    /// A name on a shape inferred from a call, which declared no names: it takes a positional
    /// slot whose type is not checked.
    UndeclaredName,
    Overflow,
}

/// R matches exact names before placing positionals, so `vapply(xs, character(1),
/// FUN = f)` sends `character(1)` to `FUN.VALUE`. Positionals then fill fixed
/// parameters, then unclaimed named formals before `...`. `None` marks a `...` forward.
/// The checker and the editor's parameter hints share it, so both agree on where an argument goes.
pub fn argument_targets<'arena>(
    db: &dyn Db,
    arena: &'arena Bump,
    function: &FunctionType<'_>,
    arguments: &[ArgumentShape<'_>],
) -> &'arena mut [Option<ArgumentTarget>] {
    let targets = arena.alloc_slice_fill_copy(arguments.len(), None);
    let claimed = arena.alloc_slice_fill_copy(function.named.len(), false);

    for (index, argument) in arguments.iter().enumerate() {
        if argument.forwards_dots {
            continue;
        }
        let Some(name) = argument.name else { continue };
        let formal = function
            .named
            .iter()
            .enumerate()
            .position(|(formal, field)| field.name.text(db) == name && !claimed[formal]);
        if let Some(formal) = formal {
            claimed[formal] = true;
            targets[index] = Some(ArgumentTarget::Named(formal));
        }
    }

    let pre_rest = match &function.variadic {
        Some(rest) => rest.preceding_named.min(function.named.len()),
        None => function.named.len(),
    };
    // R then matches a name that abbreviates exactly one unclaimed formal before `...`;
    // a formal after `...` matches only exactly.
    for (index, argument) in arguments.iter().enumerate() {
        if argument.forwards_dots || targets[index].is_some() {
            continue;
        }
        let Some(name) = argument.name else { continue };
        let mut candidates = function.named[..pre_rest]
            .iter()
            .enumerate()
            .filter(|(formal, field)| !claimed[*formal] && field.name.text(db).starts_with(name))
            .map(|(formal, _)| formal);
        if let (Some(formal), None) = (candidates.next(), candidates.next()) {
            claimed[formal] = true;
            targets[index] = Some(ArgumentTarget::Named(formal));
        }
    }

    // A shape inferred from an earlier call (`f(pars, arglist)` on a parameter `f`) has
    // positional slots whose names nobody declared, so a later `f(pars = x, arglist)`
    // names one of them. R matches names before positions, so such a name reserves the
    // first free slot, unchecked because which slot it names is unknown. On a variadic
    // shape (`f(x, ...)`) a name may also go to `...`, so it takes only the slots the
    // positional arguments leave free, which keeps those arguments checked.
    let mut next_positional = 0usize;
    if function.inferred {
        let open_slots = match function.variadic {
            None => function.positional.len(),
            Some(_) => {
                let positional_arguments = arguments
                    .iter()
                    .filter(|argument| !argument.forwards_dots && argument.name.is_none())
                    .count();
                function
                    .positional
                    .len()
                    .saturating_sub(positional_arguments)
            }
        };
        for (index, argument) in arguments.iter().enumerate() {
            if argument.forwards_dots
                || targets[index].is_some()
                || argument.name.is_none()
                || next_positional >= open_slots
            {
                continue;
            }
            targets[index] = Some(ArgumentTarget::UndeclaredName);
            next_positional += 1;
        }
    }
    let mut next_named = 0usize;
    for (index, argument) in arguments.iter().enumerate() {
        if argument.forwards_dots || targets[index].is_some() {
            continue;
        }
        targets[index] = Some(match argument.name {
            // R collects unmatched keywords into `...`; a duplicate of a supplied formal, or
            // any unmatched name on a non-variadic function, stays an error.
            Some(name) => {
                let duplicates_declared = function
                    .named
                    .iter()
                    .any(|field| field.name.text(db) == name);
                match (function.variadic.is_some(), duplicates_declared) {
                    (true, false) => ArgumentTarget::Rest,
                    _ => ArgumentTarget::UnmatchedName,
                }
            }
            None => {
                if next_positional < function.positional.len() {
                    next_positional += 1;
                    ArgumentTarget::Positional(next_positional - 1)
                } else {
                    while next_named < pre_rest && claimed[next_named] {
                        next_named += 1;
                    }
                    if next_named < pre_rest {
                        claimed[next_named] = true;
                        next_named += 1;
                        ArgumentTarget::Named(next_named - 1)
                    } else if function.variadic.is_some() {
                        ArgumentTarget::Rest
                    } else {
                        ArgumentTarget::Overflow
                    }
                }
            }
        });
    }
    // Which slot such a name took is unknown, so the slot each positional argument fills is
    // unknown too: `f(b = "x", 1)` after `f(1, "x")` puts the `1` first, not second.
    if function.inferred
        && function.variadic.is_none()
        && targets.contains(&Some(ArgumentTarget::UndeclaredName))
    {
        for target in targets.iter_mut() {
            if let Some(ArgumentTarget::Positional(_)) = target {
                *target = Some(ArgumentTarget::UndeclaredName);
            }
        }
    }
    targets
}

/// The finding a call earns from where its arguments land alone, before any argument's type is
/// read: an argument with no formal left, an unfilled positional slot, or a missing required
/// formal. Forwarded `...` may fill anything, and an unknown name throws off positional
/// accounting, so neither gets an arity verdict. An overload probe asks this before it
/// instantiates a candidate, because the verdict is the same for every instantiation.
fn shape_verdict<'db>(
    db: &'db dyn Db,
    callee_range: TextRange,
    function: &FunctionType<'db>,
    arguments: &[CallArgument<'db>],
    targets: &[Option<ArgumentTarget>],
) -> Option<TypeError<'db>> {
    if arguments.iter().any(|argument| argument.forwards_dots) {
        return None;
    }
    let named = |argument: &CallArgument<'db>| argument.name.is_some();
    if let Some(index) = targets
        .iter()
        .position(|target| *target == Some(ArgumentTarget::Overflow))
    {
        let claimed_by_name = targets
            .iter()
            .zip(arguments)
            .filter(|(target, argument)| {
                named(argument)
                    && matches!(
                        target,
                        Some(ArgumentTarget::Named(_) | ArgumentTarget::UndeclaredName)
                    )
            })
            .count();
        return Some(TypeError {
            // The first argument with no formal left is the one to remove.
            range: arguments.get(index)?.range,
            kind: TypeErrorKind::ArityMismatch {
                expected: (function.positional.len() + function.named.len())
                    .saturating_sub(claimed_by_name),
                found: arguments.iter().filter(|argument| !named(argument)).count(),
            },
        });
    }
    if targets.contains(&Some(ArgumentTarget::UnmatchedName)) {
        return None;
    }
    // Overflow is handled above, so a mismatch here leaves a positional slot empty. A named
    // argument that went to `...` fills none, so only the filled slots count.
    let positional_filled = targets
        .iter()
        .filter(|target| {
            matches!(
                target,
                Some(ArgumentTarget::Positional(_) | ArgumentTarget::UndeclaredName)
            )
        })
        .count();
    if positional_filled != function.positional.len() {
        let filled_by_name = targets
            .iter()
            .zip(arguments)
            .filter(|(target, argument)| {
                named(argument) && **target == Some(ArgumentTarget::UndeclaredName)
            })
            .count();
        return Some(TypeError {
            range: callee_range,
            kind: TypeErrorKind::ArityMismatch {
                expected: function.positional.len() - filled_by_name,
                found: positional_filled - filled_by_name,
            },
        });
    }
    let filled = |formal: usize| targets.contains(&Some(ArgumentTarget::Named(formal)));
    let mut missing: Vec<String> = function
        .named
        .iter()
        .enumerate()
        .filter(|&(formal, field)| !field.optional && !filled(formal))
        .map(|(_, field)| field.name.text(db).to_owned())
        .collect();
    let supplied = |name: Name<'db>| {
        function
            .named
            .iter()
            .position(|field| field.name == name)
            .is_some_and(filled)
    };
    for read in function.default_reads.iter() {
        let name = read.formal.text(db);
        if !supplied(read.formal)
            && !supplied(read.default_of)
            && !missing.iter().any(|missing| missing == name)
        {
            missing.push(name.to_owned());
        }
    }
    if missing.is_empty() {
        return None;
    }
    // A misspelled name on a function with `...` is absorbed by the dots, so the missing
    // formal it was meant for names it as the likely typo.
    let misspelled = targets
        .iter()
        .zip(arguments)
        .find_map(|(target, argument)| {
            let name = argument.name.as_ref()?;
            if *target != Some(ArgumentTarget::Rest) {
                return None;
            }
            let nearest = nearest_name(name, missing.iter().map(String::as_str))?;
            Some((name.as_str().to_owned(), nearest))
        });
    Some(TypeError {
        range: callee_range,
        kind: TypeErrorKind::MissingArguments {
            names: missing,
            misspelled,
        },
    })
}

/// Where a value is read as a condition: `if` and `while` read a string through `as.logical`,
/// while `&&`, `||` and a match guard reject one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionSite {
    IfOrWhile,
    Operand,
}

fn argument_shapes<'arena, 'a>(
    arena: &'arena Bump,
    arguments: &'a [CallArgument<'_>],
) -> &'arena [ArgumentShape<'a>] {
    arena.alloc_slice_fill_iter(arguments.iter().map(|argument| ArgumentShape {
        name: argument.name.as_deref(),
        forwards_dots: argument.forwards_dots,
    }))
}

fn operator_spelling(operator: BinaryOperator) -> Option<&'static str> {
    use BinaryOperator::*;
    Some(match operator {
        Add => "+",
        Subtract => "-",
        Multiply => "*",
        Divide => "/",
        Power => "^",
        Modulo => "%%",
        IntegerDivide => "%/%",
        Less => "<",
        Greater => ">",
        LessEq => "<=",
        GreaterEq => ">=",
        Equal => "==",
        NotEqual => "!=",
        _ => return None,
    })
}

/// A `logical` scalar or vector as the `integer` one R's arithmetic reads it as.
fn promote_logical<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Ty<'db>> {
    let integer = scalar(db, Atomic::Integer);
    match ty.kind(db) {
        TyKind::Scalar(Atomic::Logical) => Some(integer),
        TyKind::Vector(element) if matches!(element.kind(db), TyKind::Scalar(Atomic::Logical)) => {
            Some(Ty::new(db, TyKind::Vector(integer)))
        }
        TyKind::NamedVector(element)
            if matches!(element.kind(db), TyKind::Scalar(Atomic::Logical)) =>
        {
            Some(Ty::new(db, TyKind::NamedVector(integer)))
        }
        _ => None,
    }
}

/// Whether some value of `ty` is a `logical` scalar or vector.
fn admits_logical(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::Scalar(Atomic::Logical) => true,
        TyKind::Vector(element) | TyKind::NamedVector(element) => {
            matches!(element.kind(db), TyKind::Scalar(Atomic::Logical))
        }
        TyKind::Union(members) => members.iter().any(|&member| admits_logical(db, member)),
        _ => false,
    }
}

fn is_character(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::Scalar(Atomic::Character) => true,
        TyKind::Vector(element) | TyKind::NamedVector(element) => {
            matches!(element.kind(db), TyKind::Scalar(Atomic::Character))
        }
        _ => false,
    }
}

/// The single source for both `operator_group`'s `Arith` and the method prefixes
/// the numeric constraint admits; restating it lets the two drift apart.
const ARITHMETIC_OPERATORS: [BinaryOperator; 7] = [
    BinaryOperator::Add,
    BinaryOperator::Subtract,
    BinaryOperator::Multiply,
    BinaryOperator::Divide,
    BinaryOperator::Power,
    BinaryOperator::Modulo,
    BinaryOperator::IntegerDivide,
];

/// In the order [`Checker::operator_method_result`] tries them: the operator's own
/// method, then the group generic, then `Ops`.
pub fn arithmetic_method_prefixes() -> impl Iterator<Item = String> {
    ARITHMETIC_OPERATORS
        .iter()
        .filter_map(|&operator| operator_spelling(operator))
        .map(|spelling| format!("{spelling}."))
        .chain(["Arith.".to_owned(), "Ops.".to_owned()])
}

fn operator_group(operator: BinaryOperator) -> Option<&'static str> {
    use BinaryOperator::*;
    if ARITHMETIC_OPERATORS.contains(&operator) {
        return Some("Arith");
    }
    Some(match operator {
        Less | Greater | LessEq | GreaterEq | Equal | NotEqual => "Compare",
        _ => return None,
    })
}

fn numeric_operand_parts<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<(OperandShape, Atomic)> {
    let arithmetic_atomic = |atomic: &Atomic| match atomic {
        // R promotes logical to integer before arithmetic (`TRUE + TRUE` is `2L`).
        Atomic::Logical => Some(Atomic::Integer),
        Atomic::Integer | Atomic::Double | Atomic::Complex => Some(*atomic),
        _ => None,
    };
    match ty.kind(db) {
        TyKind::Scalar(atomic) => Some((OperandShape::Scalar, arithmetic_atomic(atomic)?)),
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Scalar(atomic) => Some((OperandShape::Vector, arithmetic_atomic(atomic)?)),
            _ => None,
        },
        _ => None,
    }
}

fn comparison_operand_shapes<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Vec<OperandShape>> {
    match ty.kind(db) {
        TyKind::Union(members) => members
            .iter()
            .map(|&member| comparison_operand_shape(db, member))
            .collect(),
        _ => comparison_operand_shape(db, ty).map(|shape| vec![shape]),
    }
}

/// A comparison coerces across atomic types, so an operand only needs to be atomic, and an
/// open one (a variable, or a vector of one) constrains nothing.
fn comparison_operand_shape<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<OperandShape> {
    match ty.kind(db) {
        TyKind::Var(_) | TyKind::Rigid(_) => Some(OperandShape::Scalar),
        TyKind::Scalar(atomic) => comparable_atomic(*atomic).then_some(OperandShape::Scalar),
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Scalar(atomic) => comparable_atomic(*atomic).then_some(OperandShape::Vector),
            TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => {
                Some(OperandShape::Vector)
            }
            TyKind::Union(members) => members
                .iter()
                .all(|&member| {
                    matches!(member.kind(db), TyKind::Null)
                        || comparison_operand_shape(db, member).is_some()
                })
                .then_some(OperandShape::Vector),
            _ => None,
        },
        // R compares a list with an atomic value element by element once each element is a
        // length-one atomic, which is how code tests a parsed config or a `lapply()` result.
        // Two lists never compare; `infer_compare` reports that pair.
        TyKind::List(_) | TyKind::NamedList(_) | TyKind::Tuple(_) | TyKind::Record(_) => {
            Some(OperandShape::Vector)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RyClass {
    Scalar(NativeScalar),
    /// A variable or type parameter bounded by `number`, `int` or `frac`.
    Open(Constraint),
}

impl RyClass {
    fn is_number(self) -> bool {
        match self {
            RyClass::Scalar(native) => native.is_number(),
            RyClass::Open(_) => true,
        }
    }
}

/// Whether a literal took the ry type it was checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiteralFit {
    Fits,
    /// A number literal the type cannot hold; the error is already reported.
    OutOfRange,
    /// Not a literal the type adapts, so the value's inferred type decides.
    NotLiteral,
}

/// The number an unsuffixed number literal writes, with a `-` written directly before it
/// folded in, as in `-128`.
/// Whether a number literal can take a type bounded by `constraint`: an `int` bound refuses a
/// fraction and a `frac` bound an `L` literal, whose R type says it is whole.
/// Whether `ty` is or holds one of ry's own scalars or collections, as opposed to a tag or
/// record that only carries R values.
fn holds_ry_scalar(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::Native(_) => true,
        TyKind::Named(..) => is_ry_value_type(db, ty),
        TyKind::Tag(_, Some(payload)) => holds_ry_scalar(db, *payload),
        TyKind::NativeTuple(items) | TyKind::Union(items) => {
            items.iter().any(|&item| holds_ry_scalar(db, item))
        }
        TyKind::NativeRecord(fields) => fields.iter().any(|field| holds_ry_scalar(db, field.ty)),
        _ => false,
    }
}

/// The ry scalar a number suffix such as `i64` names.
fn suffix_scalar(ty: NumberType) -> NativeScalar {
    match ty {
        NumberType::I64 => NativeScalar::I64,
        NumberType::F64 => NativeScalar::F64,
    }
}

fn literal_meets(literal: &NumberLiteral, constraint: Constraint) -> bool {
    match constraint {
        Constraint::Int => !literal.is_fraction(),
        Constraint::Frac => !literal.integer,
        _ => true,
    }
}

fn number_literal(module: &Module, id: ExprId) -> Option<NumberLiteral> {
    let (id, negative) = match module.kind(id) {
        ExpressionKind::Unary {
            operator: UnaryOperator::Minus,
            operand,
        } => (*operand, true),
        _ => (id, false),
    };
    match module.kind(id) {
        ExpressionKind::Literal(LiteralKind::Integer(text) | LiteralKind::Double(text)) => {
            NumberLiteral::parse(&module[text], negative)
        }
        _ => None,
    }
}

/// Whether `id` is arithmetic on number literals alone, such as `2 * 3.14159` or `(1 + 2) / 4`,
/// which adapts to a ry number type as a lone literal does. An operand may be an `if` of number
/// literals too.
fn is_literal_arithmetic(module: &Module, id: ExprId) -> bool {
    let mut id = id;
    while let ExpressionKind::Paren(inner) = module.kind(id) {
        id = *inner;
    }
    matches!(module.kind(id), ExpressionKind::Binary { operator, .. } if is_arithmetic(*operator))
        && is_number_value(module, id)
}

fn is_arithmetic(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Add
            | BinaryOperator::Subtract
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Modulo
    )
}

fn is_number_value(module: &Module, id: ExprId) -> bool {
    matches!(
        literal_value_scalar(module, id),
        Some(NativeScalar::I64 | NativeScalar::F64)
    )
}

/// Whether `id` is a logical expression of literals alone: `TRUE`, `!`, `&&` and `||` over such
/// expressions, or a comparison of number literals or literal arithmetic.
fn is_literal_logic(module: &Module, id: ExprId) -> bool {
    match module.kind(id) {
        ExpressionKind::Paren(inner) => is_literal_logic(module, *inner),
        ExpressionKind::Literal(LiteralKind::Logical(_)) => true,
        ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => is_literal_logic(module, *operand),
        ExpressionKind::Binary {
            operator: BinaryOperator::And2 | BinaryOperator::Or2,
            lhs,
            rhs,
            ..
        } => is_literal_logic(module, *lhs) && is_literal_logic(module, *rhs),
        ExpressionKind::Binary {
            operator:
                BinaryOperator::Less
                | BinaryOperator::Greater
                | BinaryOperator::LessEq
                | BinaryOperator::GreaterEq
                | BinaryOperator::Equal
                | BinaryOperator::NotEqual,
            lhs,
            rhs,
            ..
        } => is_number_value(module, *lhs) && is_number_value(module, *rhs),
        _ => false,
    }
}

/// Whether literal arithmetic takes `F64`: it divides, or a literal in it is written as a
/// fraction (`0.1`, `2.0`, `1e3`).
fn is_fractional_arithmetic(module: &Module, id: ExprId) -> bool {
    literal_value_scalar(module, id) == Some(NativeScalar::F64)
}

/// The ry scalar a value written with literals alone takes when nothing else decides it: a
/// number literal or literal arithmetic its `I64` or `F64`, a string `Str`, literal logic
/// `Bool`, and an `if` whose branches are such values, or a `{}` holding one, the type of its
/// value. `None` when a part of the value is not a literal.
fn literal_value_scalar(module: &Module, id: ExprId) -> Option<NativeScalar> {
    let number = |fractional: bool| match fractional {
        true => NativeScalar::F64,
        false => NativeScalar::I64,
    };
    if let Some(literal) = number_literal(module, id) {
        return Some(literal.default_native());
    }
    match module.kind(id) {
        ExpressionKind::Binary {
            operator, lhs, rhs, ..
        } if is_arithmetic(*operator) => {
            let lhs = literal_value_scalar(module, *lhs)?;
            let rhs = literal_value_scalar(module, *rhs)?;
            match (lhs, rhs) {
                (NativeScalar::I64 | NativeScalar::F64, NativeScalar::I64 | NativeScalar::F64) => {
                    Some(number(
                        *operator == BinaryOperator::Divide
                            || lhs == NativeScalar::F64
                            || rhs == NativeScalar::F64,
                    ))
                }
                _ => None,
            }
        }
        _ if is_literal_logic(module, id) => Some(NativeScalar::Bool),
        ExpressionKind::Paren(inner) => literal_value_scalar(module, *inner),
        ExpressionKind::Literal(LiteralKind::String(_)) => Some(NativeScalar::Str),
        ExpressionKind::Block { statements } => match &module[*statements] {
            [statement] => literal_value_scalar(module, *statement),
            _ => None,
        },
        ExpressionKind::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } => {
            let then_scalar = literal_value_scalar(module, *then_branch)?;
            let else_scalar = literal_value_scalar(module, *else_branch)?;
            match (then_scalar, else_scalar) {
                (then_scalar, else_scalar) if then_scalar == else_scalar => Some(then_scalar),
                // `if (c) 1 else 2.5` holds a fraction, and `1` is an `F64` too.
                (NativeScalar::I64, NativeScalar::F64) | (NativeScalar::F64, NativeScalar::I64) => {
                    Some(NativeScalar::F64)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether every literal in a value of literals alone fits `target` as [`Checker::adapt_literal`]
/// would adapt it, so the adaptation can run without leaving some leaves adapted when a later
/// one fails.
fn literal_leaves_fit(module: &Module, id: ExprId, target: NativeScalar) -> bool {
    if let Some(literal) = number_literal(module, id) {
        return target.is_number() && target.holds(&literal);
    }
    match module.kind(id) {
        ExpressionKind::Paren(inner) => literal_leaves_fit(module, *inner, target),
        ExpressionKind::Literal(LiteralKind::String(_)) => target == NativeScalar::Str,
        ExpressionKind::Literal(LiteralKind::Logical(_)) => target == NativeScalar::Bool,
        ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => target == NativeScalar::Bool && literal_leaves_fit(module, *operand, target),
        ExpressionKind::Binary {
            operator, lhs, rhs, ..
        } if is_arithmetic(*operator) => {
            target.is_number()
                && (*operator != BinaryOperator::Divide || target.is_fraction())
                && literal_leaves_fit(module, *lhs, target)
                && literal_leaves_fit(module, *rhs, target)
        }
        ExpressionKind::Binary {
            operator: BinaryOperator::And2 | BinaryOperator::Or2,
            lhs,
            rhs,
            ..
        } => {
            target == NativeScalar::Bool
                && literal_leaves_fit(module, *lhs, target)
                && literal_leaves_fit(module, *rhs, target)
        }
        ExpressionKind::Binary { lhs, rhs, .. } if is_literal_logic(module, id) => {
            let number = if is_fractional_arithmetic(module, *lhs)
                || is_fractional_arithmetic(module, *rhs)
            {
                NativeScalar::F64
            } else {
                NativeScalar::I64
            };
            target == NativeScalar::Bool
                && literal_leaves_fit(module, *lhs, number)
                && literal_leaves_fit(module, *rhs, number)
        }
        ExpressionKind::Block { statements } => match &module[*statements] {
            [statement] => literal_leaves_fit(module, *statement, target),
            _ => false,
        },
        ExpressionKind::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } => {
            literal_leaves_fit(module, *then_branch, target)
                && literal_leaves_fit(module, *else_branch, target)
        }
        _ => false,
    }
}

fn comparable_atomic(atomic: Atomic) -> bool {
    !matches!(atomic, Atomic::Complex | Atomic::Raw)
}

/// A `return(value)` or a `try` that leaves its function early, with the range a mismatch
/// blames. `value` is the returned expression, absent for a `try`.
#[derive(Clone, Copy)]
struct EarlyReturn<'db> {
    ty: Ty<'db>,
    range: TextRange,
    value: Option<ExprId>,
}

impl<'db, 'a> Checker<'db, 'a> {
    fn recorded_type(&self, id: ExprId) -> Ty<'db> {
        self.recorded
            .get(&id)
            .copied()
            .unwrap_or_else(|| unknown(self.db))
    }

    fn record(&mut self, id: ExprId, ty: Ty<'db>) -> Ty<'db> {
        // A self-referential value can grow its type exponentially; past the ceiling the
        // expression takes `Unknown`. Only composites are measured so scalars pay nothing.
        let ty = if matches!(
            ty.kind(self.db),
            TyKind::Record(_) | TyKind::Function(_) | TyKind::Union(_) | TyKind::Tuple(_)
        ) && type_size(self.db, ty) >= TYPE_SIZE_CEILING
        {
            unknown(self.db)
        } else {
            ty
        };
        self.recorded.insert(id, ty);
        ty
    }

    /// Idempotent: loop re-walks and re-passes must not duplicate an origin.
    fn record_strict_origin(&mut self, expression: ExprId, kind: StrictOriginKind) {
        if !self.record_strict_origins {
            return;
        }
        let range = self.module.range(expression);
        let origin = StrictOrigin {
            expression,
            range,
            kind,
        };
        if !self.strict_origins.contains(&origin) {
            self.strict_origins.push(origin);
        }
    }

    fn fresh(&mut self, constraint: Constraint) -> Ty<'db> {
        self.table.fresh_ty(self.db, constraint)
    }

    /// Whether a literal bound to a name here is a ry value: outside any function in a file
    /// that is strict or written with ry syntax. Each top-level statement is checked on its
    /// own, so a later statement cannot decide the literal the way a use in a function can,
    /// and R's type would make every such binding an R value.
    fn binds_ry_value(&self) -> bool {
        self.top_level_ry_values && self.return_frames.is_empty()
    }

    /// Nothing quantifies a literal variable: a function generalizes over its parameters,
    /// never over a literal it wrote.
    fn fresh_literal(&mut self, constraint: Constraint, native: bool) -> Ty<'db> {
        let var = self.table.fresh_literal(constraint, native);
        Ty::new(self.db, TyKind::Var(var))
    }

    /// Binds every literal variable nothing decided to its default: a ry number for a `List`
    /// item, R's type for a variable written with a literal.
    fn default_literal_vars(&mut self) {
        for index in 0..self.table.literals().len() {
            let Some(&(var, native)) = self.table.literals().get(index) else {
                break;
            };
            self.default_literal_var(Ty::new(self.db, TyKind::Var(var)), native);
        }
    }

    fn default_literal_var(&mut self, ty: Ty<'db>, native: bool) {
        let resolved = self.table.shallow_resolve(self.db, ty);
        let TyKind::Var(var) = *resolved.kind(self.db) else {
            return;
        };
        let Entry::Unbound { constraint, .. } = *self.table.entry(self.table.find(var)) else {
            return;
        };
        let default = match (native, constraint) {
            (_, Constraint::Frac | Constraint::FracLiteral) if native => {
                native_scalar(self.db, NativeScalar::F64)
            }
            (true, Constraint::Number | Constraint::Int | Constraint::WholeLiteral { .. }) => {
                native_scalar(self.db, NativeScalar::I64)
            }
            (true, Constraint::StrLiteral) => native_scalar(self.db, NativeScalar::Str),
            (true, Constraint::BoolLiteral) => native_scalar(self.db, NativeScalar::Bool),
            (
                false,
                Constraint::WholeLiteral { .. }
                | Constraint::FracLiteral
                | Constraint::StrLiteral
                | Constraint::BoolLiteral,
            ) => match constraint.literal_default() {
                Some(atomic) => scalar(self.db, atomic),
                None => return,
            },
            _ => return,
        };
        // The constraint admits its default, so this cannot fail.
        if self.table.unify(self.db, resolved, default).is_err() {
            unreachable!("a literal variable admits its default type");
        }
    }

    /// A join is not a use that decides a literal variable, so both sides take R's type.
    fn default_join_literals(&mut self, left: Ty<'db>, right: Ty<'db>) {
        for ty in [left, right] {
            if self.open_literal_var(ty) {
                self.default_literal_var(ty, false);
            }
        }
    }

    /// An open R literal's constraint and the atomic type it defaults to, without deciding it.
    fn open_literal_constraint(&mut self, ty: Ty<'db>) -> Option<(Constraint, Atomic)> {
        if !self.open_literal_var(ty) {
            return None;
        }
        let resolved = self.table.shallow_resolve(self.db, ty);
        let TyKind::Var(var) = *resolved.kind(self.db) else {
            return None;
        };
        match *self.table.entry(self.table.find(var)) {
            Entry::Unbound { constraint, .. } => Some((constraint, constraint.literal_default()?)),
            _ => None,
        }
    }

    /// Whether `ty` is a literal variable that a use may still decide.
    fn open_literal_var(&mut self, ty: Ty<'db>) -> bool {
        let resolved = self.table.shallow_resolve(self.db, ty);
        let TyKind::Var(var) = *resolved.kind(self.db) else {
            return false;
        };
        let representative = self.table.find(var);
        if !matches!(
            self.table.entry(representative),
            Entry::Unbound {
                constraint: Constraint::WholeLiteral { .. }
                    | Constraint::FracLiteral
                    | Constraint::StrLiteral
                    | Constraint::BoolLiteral,
                ..
            }
        ) {
            return false;
        }
        self.table
            .literal_of(representative)
            .is_some_and(|(_, native)| !native)
    }

    fn unify_or_report(&mut self, range: TextRange, expected: Ty<'db>, found: Ty<'db>) {
        if let Err(error) = self.table.unify(self.db, expected, found) {
            self.report_unify(range, error);
        }
    }

    /// `ty` with every undecided literal in it given its default and every bounded variable
    /// settled to its bound, for a message that should name `I64` rather than a type variable,
    /// or a use that fixes the type, as a frame column does. It decides the variables, so only
    /// such a use calls it.
    fn settle_literals(&mut self, ty: Ty<'db>) -> Ty<'db> {
        let mut open = FxHashSet::default();
        self.table.collect_unbound_vars(self.db, ty, &mut open);
        let mut bounded = Vec::new();
        for var in open {
            let representative = self.table.find(var);
            if let Some((literal, native)) = self.table.literal_of(representative) {
                self.default_literal_var(Ty::new(self.db, TyKind::Var(literal)), native);
            } else if self.table.bound_of(representative).is_some() {
                bounded.push(representative);
            }
        }
        if bounded.is_empty() {
            return self.table.resolve(self.db, ty);
        }
        // A report names what a bounded value can be, never a bare variable, but wording it
        // must not change what later uses of the variable see.
        let snapshot = self.table.snapshot();
        for var in bounded {
            if self.table.settle_bound(self.db, var).is_err() {
                break;
            }
        }
        let settled = self.table.resolve(self.db, ty);
        self.table.rollback(snapshot);
        settled
    }

    /// Every two-type report goes through here so record mismatches narrow to the field
    /// (message and range) everywhere. `value` is the expression `found` came from.
    fn type_mismatch(
        &mut self,
        range: TextRange,
        expected: Ty<'db>,
        found: Ty<'db>,
        value: Option<ExprId>,
    ) -> TypeError<'db> {
        let expected = self.settle_literals(expected);
        let found = self.settle_literals(found);
        if let Some(field) = value.and_then(|value| self.assignment_for_record(value, expected)) {
            return TypeError {
                range,
                kind: TypeErrorKind::AssignmentForRecord { field, expected },
            };
        }
        if self.fits_representation(expected, found) {
            return TypeError {
                range,
                kind: TypeErrorKind::UnwrappedNominal {
                    nominal: expected,
                    found,
                },
            };
        }
        let Some(mismatch) = self.table.explain_record_mismatch(self.db, found, expected) else {
            if let Some((component, expected, found)) =
                value.and_then(|value| self.native_component_mismatch(value, expected, found))
            {
                return TypeError {
                    range: self.blame_range(component),
                    kind: TypeErrorKind::Mismatch { expected, found },
                };
            }
            if value.is_some_and(|value| self.unnarrowed_null(value, expected, found)) {
                return TypeError {
                    range,
                    kind: TypeErrorKind::UnnarrowedNull { expected, found },
                };
            }
            return TypeError {
                range,
                kind: TypeErrorKind::Mismatch { expected, found },
            };
        };
        let range = value
            .and_then(|value| self.record_field_range(value, &mismatch))
            .unwrap_or(range);
        TypeError {
            range,
            kind: TypeErrorKind::RecordShape {
                mismatch: Box::new(mismatch),
            },
        }
    }

    /// Whether `found` fails `expected` only by its `NULL` member and `value` is an expression
    /// that `is.null()` cannot narrow: a guard narrows a variable and a field of a variable,
    /// so a deeper `$` chain, an index or a call needs a variable first.
    fn unnarrowed_null(&mut self, value: ExprId, expected: Ty<'db>, found: Ty<'db>) -> bool {
        let db = self.db;
        let is_null = |member: &Ty<'db>| matches!(member.kind(db), TyKind::Null);
        let expected = self.table.resolve(db, expected);
        let found = self.table.resolve(db, found);
        let found_members = guard_members(db, found);
        if guard_members(db, expected).iter().any(is_null) || !found_members.iter().any(is_null) {
            return false;
        }
        let rest = union_of(
            db,
            found_members
                .iter()
                .copied()
                .filter(|member| !is_null(member)),
        );
        let snapshot = self.table.snapshot();
        let fits = self.table.compatible(db, rest, expected);
        self.table.rollback(snapshot);
        if !fits {
            return false;
        }
        let mut current = value;
        while let ExpressionKind::Paren(inner) = self.module.kind(current) {
            current = *inner;
        }
        match self.module.kind(current) {
            ExpressionKind::Field { target, .. } => {
                !matches!(self.module.kind(*target), ExpressionKind::NameRef(_))
            }
            ExpressionKind::Index { .. } | ExpressionKind::Call { .. } => true,
            _ => false,
        }
    }

    /// The field name when `value` is `(name = ...)` and `expected` is a one-field record of
    /// that name, which is the record literal written without its trailing comma.
    fn assignment_for_record(&self, value: ExprId, expected: Ty<'db>) -> Option<String> {
        let TyKind::NativeRecord(fields) = expected.kind(self.db) else {
            return None;
        };
        let [field] = fields else {
            return None;
        };
        let mut current = value;
        while let ExpressionKind::Paren(inner) = self.module.kind(current) {
            current = *inner;
        }
        let ExpressionKind::Assign { target, .. } = self.module.kind(current) else {
            return None;
        };
        let ExpressionKind::NameRef(name) = self.module.kind(*target) else {
            return None;
        };
        (name.as_str() == field.name.text(self.db)).then(|| name.as_str().to_owned())
    }

    /// Whether `found` is a structural value that `@Name(...)` would accept for the nominal
    /// `expected`.
    fn fits_representation(&mut self, expected: Ty<'db>, found: Ty<'db>) -> bool {
        let TyKind::Named(name, arguments) = expected.kind(self.db) else {
            return false;
        };
        if matches!(
            found.kind(self.db),
            TyKind::Named(..) | TyKind::Unknown | TyKind::Any
        ) {
            return false;
        }
        // A standard-library type such as `data.frame` comes from its own constructors, so the
        // advice to wrap a value with `@new` would be wrong there.
        let declared = self.table.definition(self.db, *name);
        let from_stubs = self
            .library
            .and_then(|library| library.type_definitions.get(name));
        if declared.is_some() && declared == from_stubs {
            return false;
        }
        let Some(representation) = self.table.representation(self.db, *name, arguments) else {
            return false;
        };
        let snapshot = self.table.snapshot();
        let fits = self.table.compatible(self.db, found, representation);
        self.table.rollback(snapshot);
        fits
    }

    /// A losing overload candidate's parameter is not what the literal is checked against, so
    /// probes record nothing. A parenthesized value records too, because `(amount` is how a
    /// record literal starts before its first `=`.
    fn record_expectation(&mut self, value: ExprId, expected: Ty<'db>) {
        if self.overload_probe_depth > 0 {
            return;
        }
        let module = self.module;
        let mut inner = value;
        while let ExpressionKind::Paren(next) = module.kind(inner) {
            inner = *next;
        }
        let is_literal = matches!(
            module.kind(inner),
            ExpressionKind::Record(_) | ExpressionKind::Tuple(_) | ExpressionKind::Tag { .. }
        );
        if is_literal {
            self.recorded_expectations.insert(inner, expected);
        } else if inner != value {
            self.recorded_expectations.insert(value, expected);
        }
    }

    /// Follows a native record, tuple or tag literal down to the one component whose type
    /// differs from what is expected, so the mismatch blames that value rather than the whole
    /// literal. `None` when the value is not such a literal or more than one component differs.
    fn native_component_mismatch(
        &mut self,
        value: ExprId,
        expected: Ty<'db>,
        found: Ty<'db>,
    ) -> Option<(ExprId, Ty<'db>, Ty<'db>)> {
        let module = self.module;
        let mut value = value;
        while let ExpressionKind::Paren(inner) = module.kind(value) {
            value = *inner;
        }
        let found = self.table.resolve(self.db, found);
        let expected = self.structural(expected);
        // Of a union such as `Record{...} | NULL`, only a member of the literal's own shape can
        // be the one the literal was meant to be.
        let candidates: Vec<Ty<'db>> = match expected.kind(self.db) {
            TyKind::Union(members) => members.to_vec(),
            _ => vec![expected],
        };
        let (component, expected, found) = match module.kind(value) {
            ExpressionKind::Record(fields) => {
                let TyKind::NativeRecord(found_fields) = *found.kind(self.db) else {
                    return None;
                };
                let expected_fields = unique_member(self.db, &candidates, |kind| match kind {
                    TyKind::NativeRecord(fields) => Some(*fields),
                    _ => None,
                })?;
                if expected_fields.len() != found_fields.len() {
                    return None;
                }
                let mut differing = Vec::new();
                for found_field in found_fields.iter() {
                    let expected_field = expected_fields
                        .iter()
                        .find(|field| field.name == found_field.name)?;
                    let expected_ty = self.table.resolve(self.db, expected_field.ty);
                    let found_ty = self.table.resolve(self.db, found_field.ty);
                    if expected_ty != found_ty {
                        let component = module[fields]
                            .iter()
                            .find(|field| field.name.as_str() == found_field.name.text(self.db))?
                            .value;
                        differing.push((component, expected_ty, found_ty));
                    }
                }
                single(differing)?
            }
            ExpressionKind::Tuple(items) => {
                let TyKind::NativeTuple(found_items) = *found.kind(self.db) else {
                    return None;
                };
                let expected_items = unique_member(self.db, &candidates, |kind| match kind {
                    TyKind::NativeTuple(items) => Some(*items),
                    _ => None,
                })?;
                if expected_items.len() != found_items.len() || items.len() != found_items.len() {
                    return None;
                }
                let mut differing = Vec::new();
                for ((&item, &expected_ty), &found_ty) in module[items]
                    .iter()
                    .zip(expected_items.iter())
                    .zip(found_items.iter())
                {
                    let expected_ty = self.table.resolve(self.db, expected_ty);
                    let found_ty = self.table.resolve(self.db, found_ty);
                    if expected_ty != found_ty {
                        differing.push((item, expected_ty, found_ty));
                    }
                }
                single(differing)?
            }
            // A tag's payload is itself a record or tuple literal, matched against the payload
            // of the expected tag of the same name.
            ExpressionKind::Tag {
                name,
                payload: Some(payload),
                ..
            } => {
                let TyKind::Tag(_, Some(found_payload)) = found.kind(self.db) else {
                    return None;
                };
                let expected_payload = unique_member(self.db, &candidates, |kind| match kind {
                    TyKind::Tag(tag, Some(payload))
                        if tag.text(self.db) == self.module.name(*name).as_str() =>
                    {
                        Some(*payload)
                    }
                    _ => None,
                })?;
                return self.native_component_mismatch(*payload, expected_payload, *found_payload);
            }
            _ => return None,
        };
        Some(
            self.native_component_mismatch(component, expected, found)
                .unwrap_or((component, expected, found)),
        )
    }

    /// Walks the path against the `list(...)` call that produced the record; `None`
    /// when the expression is not that shape, leaving the whole value as the blame.
    fn record_field_range(
        &self,
        value: ExprId,
        mismatch: &RecordMismatch<'db>,
    ) -> Option<TextRange> {
        let (RecordMismatch::Field { path, .. }
        | RecordMismatch::Missing { path, .. }
        | RecordMismatch::Unexpected { path, .. }) = mismatch;
        let (last, enclosing) = path.split_last()?;
        let mut current = value;
        for segment in enclosing {
            current = self.tagged_argument(current, segment)?.value?;
        }
        match mismatch {
            RecordMismatch::Missing { .. } => Some(self.blame_range(current)),
            RecordMismatch::Field { .. } => {
                Some(self.blame_range(self.tagged_argument(current, last)?.value?))
            }
            RecordMismatch::Unexpected { .. } => {
                let argument = self.tagged_argument(current, last)?;
                self.module
                    .argument_name_range(argument)
                    .or_else(|| argument.value.map(|value| self.blame_range(value)))
            }
        }
    }

    fn tagged_argument(&self, call: ExprId, name: &str) -> Option<&Argument> {
        let ExpressionKind::Call { arguments, .. } = self.module.kind(call) else {
            return None;
        };
        self.module[arguments]
            .iter()
            .find(|argument| self.module.argument_name(argument).as_deref() == Some(name))
    }

    fn report_unify(&mut self, range: TextRange, error: UnifyError<'db>) {
        let kind = match error {
            UnifyError::Mismatch(expected, found) => {
                let error = self.type_mismatch(range, expected, found, None);
                self.errors.push(error);
                return;
            }
            UnifyError::Occurs(variable, container) => TypeErrorKind::InfiniteType {
                variable: Ty::new(self.db, TyKind::Var(variable)),
                container: self.table.resolve(self.db, container),
            },
            UnifyError::ConstraintRejected(constraint, found) => {
                TypeErrorKind::ConstraintViolation {
                    constraint,
                    found: self.table.resolve(self.db, found),
                }
            }
        };
        self.errors.push(TypeError { range, kind });
    }

    fn infer(&mut self, id: ExprId) -> Ty<'db> {
        let module = self.module;
        let expression = id;
        let range = module.range(expression);
        let ty = match module.kind(expression) {
            ExpressionKind::Missing => unknown(self.db),
            ExpressionKind::Literal(literal) => {
                if let LiteralKind::Suffixed { number, ty } = literal
                    && let Some(value) = NumberLiteral::parse(&module[number], false)
                    && !suffix_scalar(*ty).holds(&value)
                {
                    let expected = native_scalar(self.db, suffix_scalar(*ty));
                    self.errors.push(TypeError {
                        range,
                        kind: TypeErrorKind::LiteralOutOfRange { expected },
                    });
                }
                self.literal_ty(literal)
            }
            ExpressionKind::NameRef(_) => self.infer_read(id),
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                let (spelling, target, value) = (*spelling, *target, *value);
                let declares_function = self.declared_function_annotation(id).filter(|_| {
                    matches!(self.module.kind(value), ExpressionKind::Function { .. })
                        && !self.expression_annotations.contains_key(value)
                });
                let computed =
                    declares_function.is_none() && !self.expression_annotations.contains_key(id);
                // An annotated value (`@UserId(5L)`) is typed by its annotation, so it is
                // never one of the bare literals below.
                let value_annotated = self.expression_annotations.contains_key(value);
                let value_ty = match declares_function {
                    Some(declared) => {
                        self.table.enter_rigid_binders(&declared.binders);
                        let body = match declared.body.kind(self.db) {
                            TyKind::Function(function) => {
                                let inline = self.inline_parameters(id);
                                self.check_declared_function(value, function, &inline)
                            }
                            _ => declared.body,
                        };
                        let signature = self.record(value, body);
                        // The body is checked with the binders rigid, but each use of the name
                        // takes fresh variables: created a level up, they generalize with the
                        // binding into the declared scheme.
                        let binds_name = spelling != AssignSpelling::Super
                            && matches!(self.module.kind(target), ExpressionKind::NameRef(_));
                        if declared.binders.is_empty() || !binds_name {
                            signature
                        } else {
                            self.table.level += 1;
                            let open = self.instantiate(&TypeScheme {
                                binders: declared.binders,
                                body: signature,
                            });
                            self.table.level -= 1;
                            open
                        }
                    }
                    None => {
                        let binds_name = spelling != AssignSpelling::Super
                            && matches!(
                                self.module.kind(target),
                                ExpressionKind::NameRef(_)
                                    | ExpressionKind::Literal(LiteralKind::String(_))
                            );
                        if binds_name {
                            self.table.level += 1;
                        }
                        let literal = number_literal(self.module, value).filter(|_| {
                            binds_name
                                && !value_annotated
                                && !self.expression_annotations.contains_key(id)
                        });
                        let value_ty = match literal {
                            Some(literal) => {
                                let native = self.binds_ry_value();
                                let constraint = if literal.is_fraction()
                                    || (native && literal.default_native() == NativeScalar::F64)
                                {
                                    Constraint::FracLiteral
                                } else {
                                    Constraint::WholeLiteral {
                                        integer: literal.integer,
                                    }
                                };
                                let ty = self.fresh_literal(constraint, native);
                                self.record_literal(value, ty);
                                ty
                            }
                            None => match self.module.kind(value) {
                                ExpressionKind::Literal(
                                    kind @ (LiteralKind::String(_) | LiteralKind::Logical(_)),
                                ) if binds_name
                                    && !value_annotated
                                    && !self.expression_annotations.contains_key(id) =>
                                {
                                    let constraint = match kind {
                                        LiteralKind::String(_) => Constraint::StrLiteral,
                                        _ => Constraint::BoolLiteral,
                                    };
                                    let native = self.binds_ry_value();
                                    let ty = self.fresh_literal(constraint, native);
                                    self.record(value, ty)
                                }
                                // An annotated binding of a variable may decide the variable's
                                // literal, so its read stays undecided until the annotation.
                                ExpressionKind::NameRef(_)
                                    if self.expression_annotations.contains_key(id) =>
                                {
                                    let fresh = self.literal_reads.insert(value);
                                    let ty = self.infer(value);
                                    if fresh {
                                        self.literal_reads.remove(value);
                                    }
                                    ty
                                }
                                // A write into a ry list's item or a ry record's field may
                                // decide the variable's literal (`lookup[[i]] <- j`,
                                // `r$x <- j`), which the write itself does.
                                ExpressionKind::NameRef(_)
                                    if matches!(
                                        self.module.kind(target),
                                        ExpressionKind::Index { double: true, .. }
                                            | ExpressionKind::Field { .. }
                                    ) =>
                                {
                                    let fresh = self.literal_reads.insert(value);
                                    let ty = self.infer(value);
                                    if fresh {
                                        self.literal_reads.remove(value);
                                    }
                                    ty
                                }
                                // Literal arithmetic and logic, and an `if` or `{}` whose
                                // value is a literal, are one literal, so at a top-level
                                // binding in a typed file they are a ry value too.
                                _ if binds_name
                                    && !value_annotated
                                    && !self.expression_annotations.contains_key(id)
                                    && self.binds_ry_value()
                                    && let Some(scalar) =
                                        literal_value_scalar(self.module, value) =>
                                {
                                    let ty = self.infer(value);
                                    let target = native_scalar(self.db, scalar);
                                    match self.adapt_literal(value, target) {
                                        LiteralFit::Fits => self.record(value, target),
                                        _ => ty,
                                    }
                                }
                                _ => self.infer(value),
                            },
                        };
                        if binds_name {
                            self.table.level -= 1;
                        }
                        self.apply_expression_annotation(id, value, value_ty)
                    }
                };
                if let Some(&slot) = self.naming.resolutions.get(&target) {
                    match computed {
                        true => self.computed_slots.insert(slot),
                        false => self.computed_slots.remove(slot),
                    };
                    match self.is_lookup(value) {
                        true => self.lookup_slots.insert(slot),
                        false => self.lookup_slots.remove(slot),
                    };
                    // A `<<-` write reaches the readers in sibling closures only once the
                    // defining frame joins it, so until then they still read the `NULL`.
                    if spelling != AssignSpelling::Super
                        && !matches!(
                            self.table.resolve(self.db, value_ty).kind(self.db),
                            TyKind::Null
                        )
                        && !matches!(
                            self.module.kind(value),
                            ExpressionKind::Literal(LiteralKind::Na(NaAtom::Logical))
                        )
                    {
                        self.valued_slots.insert(slot);
                    }
                }
                self.write_target(spelling, target, value, value_ty);
                value_ty
            }
            ExpressionKind::Unary { operator, operand } => {
                let (operator, operand) = (*operator, *operand);
                self.infer_unary(id, operator, operand)
            }
            ExpressionKind::Binary {
                operator,
                special_name,
                lhs,
                rhs,
            } => {
                let (operator, lhs, rhs) = (*operator, *lhs, *rhs);
                let special_name = *special_name;
                self.infer_binary(id, range, operator, special_name.as_deref(), lhs, rhs)
            }
            ExpressionKind::Call { callee, arguments } => {
                let (callee, arguments) = (*callee, *arguments);
                // A quoting form returns its arguments as code without running them, so
                // `expression("density " * f[1] - f[2])` multiplies nothing. A project binding
                // of the same name is an ordinary function.
                if let ExpressionKind::NameRef(name) = module.kind(callee)
                    && matches!(
                        name.as_str(),
                        "quote" | "bquote" | "substitute" | "expression"
                    )
                    && !self.naming.resolutions.contains_key(&callee)
                    && !self
                        .globals
                        .is_some_and(|globals| globals.defined_in_project(*name, true))
                {
                    return any(self.db);
                }
                let first_call_error = self.errors.len();
                // A variable written with a literal is decided by the parameter it is passed
                // to, so `rate <- 2.0; half(rate)` makes `rate` the `F64` that `half` takes.
                // An argument the call leaves undecided takes R's type afterwards.
                let arena = self.arena;
                let literal_arguments = BumpVec::from_iter_in(
                    module[arguments]
                        .iter()
                        .filter_map(|argument| argument.value)
                        .filter(|&value| {
                            matches!(module.kind(value), ExpressionKind::NameRef(_))
                                && self.literal_reads.insert(value)
                        }),
                    arena,
                );
                let ty = self.infer_call_expression(id, range, callee, &module[arguments]);
                for &value in &literal_arguments {
                    self.literal_reads.remove(value);
                    let argument = self.recorded_type(value);
                    if self.open_literal_var(argument) {
                        self.default_literal_var(argument, false);
                    }
                }
                self.check_native_arguments(range, callee, &module[arguments], first_call_error);
                if let Some(&slot) = self.naming.extras().run_time_writes.get(&id) {
                    let written = unknown(self.db);
                    self.environment.set(slot, EnvEntry::Mono(written));
                    self.note_captured_write(id, slot, written);
                }
                ty
            }
            ExpressionKind::Index {
                double,
                target,
                arguments,
            } => {
                let double = *double;
                let target = *target;
                let arguments = *arguments;
                self.infer_index(id, range, double, target, &module[arguments])
            }
            ExpressionKind::Field { at, target, name } => {
                let at = *at;
                let target = *target;
                let name_range = name.map_or(range, |name| module.name_range(name));
                let name = name.map(|name| module.name(name));
                self.infer_field(id, range, name_range, at, target, name)
            }
            // Validated against the stub namespaces (`:::` skips the export check). Only the
            // project's own package reads the project's bindings; `base::date` is base's
            // `date` even when the project defines one.
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => {
                let validated =
                    validated_namespace_name(self.db, self.library, *internal, package, name);
                let reaches_project = namespace_reaches_project(self.db, package);
                let name = *name;
                match validated.and_then(|name| {
                    let globals = self.globals?;
                    if reaches_project {
                        globals.namespace_scheme(name)
                    } else {
                        let package = (*package)?;
                        self.library?
                            .namespace_schemes(package, name)?
                            .last()
                            .cloned()
                    }
                }) {
                    Some(namespace_scheme) => self.instantiate(&namespace_scheme),
                    None => {
                        if let Some(name) = name {
                            self.record_strict_origin(
                                id,
                                StrictOriginKind::UndeterminedReference(name.to_string()),
                            );
                        }
                        unknown(self.db)
                    }
                }
            }
            // R formals are matchable by name and position, so every formal is a named
            // parameter. Defaults are inferred but do not pin an unannotated parameter's type.
            ExpressionKind::Function { parameters, body } => {
                // Declared parameter types push into the body; inferring freely and comparing
                // afterwards would report a shape mismatch and miss the real error inside the body.
                if let Some(declared) = self.declared_function_annotation(id) {
                    self.table.enter_rigid_binders(&declared.binders);
                    let TyKind::Function(function) = declared.body.kind(self.db) else {
                        return unknown(self.db);
                    };
                    let inline = self.inline_parameters(id);
                    let signature = self.check_declared_function(id, function, &inline);
                    let signature = self.record(id, signature);
                    // The body is checked with the binders rigid, but a literal no name binds is
                    // used right here, so each use takes fresh variables as a call of it would.
                    if declared.binders.is_empty() {
                        return signature;
                    }
                    return self.instantiate(&TypeScheme {
                        binders: declared.binders,
                        body: signature,
                    });
                }
                let parameters = *parameters;
                // R hints apply only when they type every formal without a default: a partly
                // typed lambda (`Reduce(function(a, b) a + b, xs)` fixes `b` but not the
                // accumulator `a`) infers better with all formals open, where `a + b` ties
                // the two together. A ry hint applies alone, because a ry operator passes its
                // type to an open operand: in `List.walk(xs, 0, \(acc, x) acc + x)`, `x` is the
                // item type and `acc + x` makes the accumulator that type too.
                let hints_entry = self.parameter_hints.remove(id);
                let is_callback = hints_entry.is_some();
                let hints: Vec<Option<Ty<'db>>> = hints_entry
                    .filter(|hints| {
                        hints.iter().flatten().any(|&hint| {
                            let hint = self.table.resolve(self.db, hint);
                            is_ry_value_type(self.db, hint) || self.ry_number_class(hint).is_some()
                        }) || module[parameters]
                            .iter()
                            .filter(|parameter| parameter.name != "...")
                            .enumerate()
                            .all(|(index, parameter)| {
                                parameter.default.is_some()
                                    || hints.get(index).is_some_and(Option::is_some)
                            })
                    })
                    .unwrap_or_default();
                self.table.level += 1;
                let enclosing_frame_level = mem::replace(&mut self.frame_level, self.table.level);
                let enclosing_callback_frame_level = match is_callback {
                    true => self.callback_frame_level.replace(self.table.level),
                    false => self.callback_frame_level,
                };
                let pending_mark = self.pending_enclosing_writes.len();
                let mark = self.environment.mark();
                let uses = self.formal_uses(&module[parameters], *body);
                let mut probed = FxHashSet::default();
                self.collect_type_probed(*body, &mut probed);
                let mut named = Vec::new();
                let mut variadic = None;
                for parameter in &module[parameters] {
                    if parameter.name == "..." {
                        variadic = Some(RestParameter {
                            element: any(self.db),
                            preceding_named: named.len(),
                        });
                        continue;
                    }
                    let hint = hints.get(named.len()).copied().flatten();
                    let parameter_ty = if probed.contains(&parameter.name) {
                        unknown(self.db)
                    } else if let Some(hint) = hint
                        && parameter.default.is_none()
                    {
                        self.hinted_parameters.insert(parameter.range);
                        hint
                    } else {
                        self.fresh(Constraint::Unconstrained)
                    };
                    let entry = named.len();
                    named.push(Parameter {
                        name: Name::new(self.db, parameter.name),
                        ty: parameter_ty,
                        optional: parameter.default.is_some()
                            || uses.makes_optional(parameter.name),
                        default: None,
                    });
                    let no_default = parameter.default.is_none();
                    let formal_slot = self
                        .naming
                        .bindings
                        .iter()
                        .find(|(_, info)| info.range == parameter.range)
                        .map(|(id, _)| *id);
                    if let Some(slot) = formal_slot {
                        self.environment.set(slot, EnvEntry::Mono(parameter_ty));
                        self.computed_slots.insert(slot);
                        if no_default {
                            self.no_default_formals.insert(slot);
                        } else {
                            self.defaulted_formals.insert(slot);
                        }
                    }
                    // Inferred only now: a default may read its own and earlier parameters' slots.
                    if let Some(default) = parameter.default {
                        let default_ty = self.infer(default);
                        named[entry].default = Some(default_ty);
                        // A default naming a function ry cannot type (bit64's `timefun =
                        // repeat.time`, from a package without stubs) is what the body calls
                        // whenever the caller omits the argument, so the body's calls say nothing
                        // about the parameters a callback must take: the formal reads as `Unknown`.
                        if matches!(
                            self.table.resolve(self.db, default_ty).kind(self.db),
                            TyKind::Unknown | TyKind::Any
                        ) && matches!(
                            module.kind(default),
                            ExpressionKind::NameRef(_) | ExpressionKind::Namespace { .. }
                        ) && let Some(slot) = formal_slot
                        {
                            self.environment.set(slot, EnvEntry::Mono(unknown(self.db)));
                        }
                    }
                }
                self.return_frames.push(Vec::new());
                let trailing_ty = self.infer_body_with_capture_discovery(*body, pending_mark);
                let early_returns = self
                    .return_frames
                    .pop()
                    .expect("return frames stay balanced around body inference");
                let return_ty = self.join_early_returns(&early_returns, trailing_ty, *body);
                let default_reads = self.default_reads(&module[parameters], &uses, &named);
                let flag_return = self.flag_return(&module[parameters], *body, &early_returns);
                let null_unless =
                    self.null_unless(&module[parameters], *body, &early_returns, trailing_ty);
                let reduces = if early_returns.is_empty() {
                    self.reduce_forward(&module[parameters], *body)
                } else {
                    None
                };
                // The body may settle a formal on a type its default does not have
                // (`function(class = NULL) { if (!length(class)) class <- ""; ... }`), yet a call
                // that passes the default's own value is always fine.
                for parameter in &mut named {
                    let Some(default) = parameter.default else {
                        continue;
                    };
                    let settled = self.table.resolve(self.db, parameter.ty);
                    let default = self.table.resolve(self.db, default);
                    if self.table.contains_unbound_var(self.db, settled)
                        || self.table.contains_unbound_var(self.db, default)
                        || matches!(default.kind(self.db), TyKind::Any | TyKind::Unknown)
                    {
                        continue;
                    }
                    let snapshot = self.table.snapshot();
                    let admitted = self.table.compatible(self.db, default, settled);
                    self.table.rollback(snapshot);
                    if !admitted {
                        parameter.ty = union_of(self.db, [settled, default]);
                    }
                }
                let signature: Vec<(&Formal, Option<Ty<'db>>)> = module[parameters]
                    .iter()
                    .filter(|parameter| parameter.name != "...")
                    .zip(&named)
                    .map(|(parameter, typed)| (parameter, Some(typed.ty)))
                    .collect();
                self.record_untyped_parameters(id, &signature, return_ty);
                self.environment.rollback(mark);
                self.reapply_enclosing_writes(pending_mark);
                self.frame_level = enclosing_frame_level;
                self.callback_frame_level = enclosing_callback_frame_level;
                self.table.level -= 1;
                Ty::new(
                    self.db,
                    TyKind::Function(&FunctionType {
                        positional: &[],
                        named: &named,
                        variadic,
                        ret: return_ty,
                        inferred: true,
                        flag_return,
                        quoted: &module[parameters]
                            .iter()
                            .filter(|parameter| uses.quotes(parameter.name))
                            .map(|parameter| Name::new(self.db, parameter.name))
                            .collect::<Vec<_>>(),
                        default_reads: &default_reads,
                        missing_tested: &module[parameters]
                            .iter()
                            .filter(|parameter| uses.missing_tested.contains(&parameter.name))
                            .map(|parameter| Name::new(self.db, parameter.name))
                            .collect::<Vec<_>>(),
                        condition_only: &self
                            .condition_only_names(*body, &module[parameters])
                            .into_iter()
                            .map(|name| Name::new(self.db, name))
                            .collect::<Vec<_>>(),
                        dispatch: dispatch_formal(module, *body, &module[parameters])
                            .map(|name| Name::new(self.db, name)),
                        null_unless,
                        reduces,
                        from_calls: false,
                    }),
                )
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let (condition, then_branch, else_branch) =
                    (*condition, *then_branch, *else_branch);
                let discarded = self.discarded_if.take() == Some(id);
                self.infer_if(condition, then_branch, else_branch, discarded)
            }
            ExpressionKind::For {
                variable,
                sequence,
                body,
            } => {
                let variable_range = variable.map(|variable| module.name_range(variable));
                let (sequence, body) = (*sequence, *body);
                self.infer_for(id, variable_range, sequence, body)
            }
            ExpressionKind::While { condition, body } => {
                let (condition, body) = (*condition, *body);
                self.check_loop_body(id, Some(condition), body, false, None);
                null(self.db)
            }
            ExpressionKind::Repeat { body } => {
                let body = *body;
                self.check_loop_body(id, None, body, true, None);
                null(self.db)
            }
            ExpressionKind::Local { body } => {
                let body = *body;
                let pending_mark = self.pending_enclosing_writes.len();
                let mark = self.environment.mark();
                let value = self.infer(body);
                self.environment.rollback(mark);
                self.reapply_enclosing_writes(pending_mark);
                value
            }
            ExpressionKind::Block { statements } => {
                let statements = *statements;
                let mut last = null(self.db);
                let count = statements.len();
                for (position, &statement) in module[statements].iter().enumerate() {
                    if position + 1 < count
                        && matches!(self.module.kind(statement), ExpressionKind::If { .. })
                    {
                        self.discarded_if = Some(statement);
                    }
                    last = self.infer(statement);
                }
                last
            }
            ExpressionKind::Paren(inner) => self.infer(*inner),
            ExpressionKind::Break | ExpressionKind::Next => {
                if let Some(mark) = self.loop_jumps.last().map(|jumps| jumps.mark) {
                    let state = self.environment.writes_since(mark);
                    if let Some(jumps) = self.loop_jumps.last_mut() {
                        match module.kind(id) {
                            ExpressionKind::Break => jumps.breaks.push(state),
                            _ => jumps.nexts.extend(state),
                        }
                    }
                }
                unknown(self.db)
            }
            ExpressionKind::Tuple(items) => {
                let items = *items;
                let items = module[items]
                    .iter()
                    .map(|&item| self.component(item))
                    .collect::<TypeList<'db>>();
                Ty::new(self.db, TyKind::NativeTuple(&items))
            }
            ExpressionKind::Array(items) => self.infer_array_literal(&module[items]),
            ExpressionKind::Record(fields) => {
                let fields = *fields;
                let fields = module[fields]
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name),
                        ty: self.component(field.value),
                    })
                    .collect::<Vec<_>>();
                native_record(self.db, fields)
            }
            ExpressionKind::Tag { name, payload, .. } => {
                let (name, payload) = (*name, *payload);
                // A payload's literals are decided now: tags join by their payload types,
                // which an open literal would leave undecided.
                let payload = payload.map(|payload| self.infer_payload(payload));
                Ty::new(
                    self.db,
                    TyKind::Tag(Name::new(self.db, module.name(name).as_str()), payload),
                )
            }
            ExpressionKind::Match { scrutinee, arms } => {
                let (scrutinee, arms) = (*scrutinee, *arms);
                self.infer_match(range, scrutinee, &module[arms])
            }
            ExpressionKind::RecordUpdate { base, fields } => {
                let (base, fields) = (*base, *fields);
                self.infer_record_update(base, &module[fields])
            }
            ExpressionKind::FormatString(pieces) => {
                let pieces = *pieces;
                for &piece in module[pieces].iter() {
                    self.infer(piece);
                }
                native_scalar(self.db, NativeScalar::Str)
            }
            ExpressionKind::Try(operand) => {
                let operand = *operand;
                self.infer_try(range, operand)
            }
        };
        // Assignments applied their annotation before the slot write above.
        let ty = if matches!(module.kind(expression), ExpressionKind::Assign { .. }) {
            ty
        } else {
            self.apply_expression_annotation(id, id, ty)
        };
        self.record(id, ty)
    }

    /// R functions and C code refuse a record, tuple or tag at run time, apart from the few in
    /// [`accepts_native`], so passing one to a function that is not the project's own is an
    /// error here too. A value only some of whose types are native may be R data at run time,
    /// so it is not reported. The native-value finding replaces the call's own mismatch for
    /// that argument, because a stub signature never describes a ry value and its complaint
    /// would only restate the same problem less precisely.
    fn check_native_arguments(
        &mut self,
        call_range: TextRange,
        callee: ExprId,
        arguments: &[Argument],
        first_call_error: usize,
    ) {
        // A library function accepts ry values, but only a bare name, `std::` or `ry::` reaches
        // it: `pkg::List.len` is the package's own function, whatever its name.
        let (function, may_be_library) = match self.module.kind(callee) {
            ExpressionKind::NameRef(name) => {
                let deferred = self.naming.deferred_non_locals.contains(&callee);
                if self.naming.resolutions.contains_key(&callee)
                    || self
                        .globals
                        .is_some_and(|globals| globals.defined_in_project(*name, deferred))
                {
                    return;
                }
                // `List.length` spells a library function that does not exist, which the
                // unresolved finding reports with the right name.
                if name
                    .as_str()
                    .split_once('.')
                    .is_some_and(|(module, _)| STANDARD_MODULES.contains(&module))
                {
                    return;
                }
                (name.as_str(), true)
            }
            ExpressionKind::Namespace {
                package,
                name: Some(name),
                ..
            } => (
                name.as_str(),
                package.is_some_and(|package| NATIVE_NAMESPACES.contains(&package.as_str())),
            ),
            _ => return,
        };
        if accepts_native(function)
            || (may_be_library
                && self.library.is_some_and(|library| {
                    NATIVE_NAMESPACES
                        .iter()
                        .any(|namespace| library.exports(*namespace, function))
                }))
        {
            return;
        }
        // Any namespace reaches the library here: `testthat::expect_error` is testthat's.
        let opaque = self.opaque_arguments(function, arguments);
        for (index, argument) in arguments.iter().enumerate() {
            let Some(value) = argument.value else {
                continue;
            };
            if opaque.contains(&index) {
                continue;
            }
            let Some(&ty) = self.recorded.get(&value) else {
                continue;
            };
            // A nominal record or tag is still a ry value when the code runs.
            let resolved = self.structural(ty);
            if let Some(kind) = native_value_kind(self.db, resolved) {
                let blamed = self.blame_range(value);
                let mut index = first_call_error;
                while index < self.errors.len() {
                    let restated = self.errors[index].range == blamed
                        || (self.errors[index].range == call_range
                            && matches!(
                                self.errors[index].kind,
                                TypeErrorKind::NoMatchingOverload { .. }
                            ));
                    if restated {
                        self.remove_error(index);
                    } else {
                        index += 1;
                    }
                }
                self.errors.push(TypeError {
                    range: blamed,
                    kind: TypeErrorKind::NativeValueToR {
                        function: function.to_owned(),
                        kind,
                    },
                });
            }
        }
    }

    /// The positions of the arguments that reach a library function's `@opaque` parameters,
    /// matched as R matches them against the declaration's first candidate.
    fn opaque_arguments(&self, function: &str, arguments: &[Argument]) -> SmallVec<[usize; 2]> {
        let mut positions = SmallVec::new();
        let Some(library) = self.library else {
            return positions;
        };
        let Some(parameters) = library.opaque.get(function) else {
            return positions;
        };
        let Some(TyKind::Function(declared)) = library
            .schemes
            .get(function)
            .and_then(|candidates| candidates.first())
            .map(|scheme| scheme.body.kind(self.db))
        else {
            return positions;
        };
        let shapes = self
            .arena
            .alloc_slice_fill_iter(arguments.iter().map(|argument| {
                let name = self.module.argument_name(argument);
                ArgumentShape {
                    forwards_dots: name.is_none_or(|name| name == "...")
                        && argument.value.is_some_and(|value| {
                            matches!(
                                self.module.kind(value),
                                ExpressionKind::NameRef(name) if name == "..."
                            )
                        }),
                    name: name.map(Symbol::as_str),
                }
            }));
        for (index, target) in argument_targets(self.db, self.arena, declared, shapes)
            .iter()
            .enumerate()
        {
            if let Some(ArgumentTarget::Named(formal)) = target
                && let Some(field) = declared.named.get(*formal)
                && parameters
                    .iter()
                    .any(|parameter| parameter == field.name.text(self.db))
            {
                positions.push(index);
            }
        }
        positions
    }

    /// `@trust` and `@if-unknown` do not check the value, and `@new` mints a nominal
    /// rather than declaring a signature.
    fn declared_function_annotation(&self, annotated: ExprId) -> Option<TypeScheme<'db>> {
        let annotation = self.expression_annotations.get(annotated)?;
        if annotation.trusted || annotation.if_unknown || annotation.new_nominal.is_some() {
            return None;
        }
        let declared = annotation.declared.clone()?;
        matches!(declared.body.kind(self.db), TyKind::Function(_)).then_some(declared)
    }

    /// An annotation on a value that is not a function literal has no body to infer an elided
    /// `->` from, so the return comes from the value's own function type: `#: fn(integer)` on
    /// `g <- f` returns what `f` returns for an `integer`. An open return stays `Unknown`.
    fn elided_return_from_value(&mut self, declared: Ty<'db>, value_ty: Ty<'db>) -> Ty<'db> {
        let db = self.db;
        let TyKind::Function(function) = declared.kind(db) else {
            return declared;
        };
        let TyKind::Function(value) = self.table.resolve(db, value_ty).kind(db) else {
            return declared;
        };
        if !matches!(function.ret.kind(db), TyKind::Unknown) {
            return declared;
        }
        // Parameters pair by position, as a call binds them: `fn(integer)` names no formal.
        let declared_parameters = parameter_types(function);
        let value_parameters = parameter_types(value);
        if declared_parameters.len() > value_parameters.len() {
            return declared;
        }
        let snapshot = self.table.snapshot();
        let fits = declared_parameters
            .iter()
            .zip(&value_parameters)
            .all(|(&written, &formal)| self.table.unify(db, formal, written).is_ok());
        let returned = self.table.resolve(db, value.ret);
        self.table.rollback(snapshot);
        if !fits
            || contains_inference_var(db, returned)
            || matches!(returned.kind(db), TyKind::Unknown)
        {
            return declared;
        }
        Ty::new(
            db,
            TyKind::Function(&FunctionType {
                ret: returned,
                ..**function
            }),
        )
    }

    fn inline_parameters(&self, annotated: ExprId) -> Vec<String> {
        self.expression_annotations
            .get(annotated)
            .map(|annotation| annotation.inline_parameters.clone())
            .unwrap_or_default()
    }

    fn apply_expression_annotation(
        &mut self,
        annotated: ExprId,
        value: ExprId,
        value_ty: Ty<'db>,
    ) -> Ty<'db> {
        let Some(annotation) = self.expression_annotations.get(annotated).cloned() else {
            return value_ty;
        };
        if annotation.declared.is_some() || annotation.new_nominal.is_some() {
            // A checked annotation over a value of undetermined type checks nothing, so strict
            // mode says so at the annotated value instead of accepting it as a declaration.
            let unchecked = annotation.new_nominal.is_none()
                && !annotation.trusted
                && !annotation.if_unknown
                && annotation.declared.as_ref().is_some_and(|declared| {
                    !matches!(declared.body.kind(self.db), TyKind::Unknown | TyKind::Any)
                })
                && matches!(
                    self.table.resolve(self.db, value_ty).kind(self.db),
                    TyKind::Unknown
                );
            if self.annotation_covers(value) && unchecked {
                self.record_strict_origin(value, StrictOriginKind::UncheckedAnnotation);
            }
        }
        if let Some((name, arguments, _)) = &annotation.new_nominal {
            let scheme = self.check_new_nominal(*name, arguments, value, value_ty);
            return self.instantiate(&scheme);
        }
        let Some(declared) = &annotation.declared else {
            return value_ty;
        };
        if annotation.trusted {
            return self.elided_return_from_value(declared.body, value_ty);
        }
        // `@if-unknown` must report when the type is known: an annotation that silently
        // outlived a changed inferred type would be indistinguishable from a stale one.
        if annotation.if_unknown {
            let resolved_value = self.table.resolve(self.db, value_ty);
            if matches!(resolved_value.kind(self.db), TyKind::Unknown) {
                return declared.body;
            }
            self.errors.push(TypeError {
                range: self.blame_range(value),
                kind: TypeErrorKind::KnownTypeUnderIfUnknown {
                    found: resolved_value,
                },
            });
            return resolved_value;
        }
        if matches!(declared.body.kind(self.db), TyKind::Unknown | TyKind::Any) {
            return declared.body;
        }
        self.table.enter_rigid_binders(&declared.binders);
        self.record_expectation(value, declared.body);
        if self.adapt_literal(value, declared.body) != LiteralFit::NotLiteral {
            return declared.body;
        }
        // `greeting <- "hi"; message: Str <- greeting`: the annotation is the use that decides a
        // variable written with a literal.
        if self.open_literal_var(value_ty)
            && self.table.unify(self.db, value_ty, declared.body).is_ok()
        {
            return declared.body;
        }
        let resolved_value = self.table.resolve(self.db, value_ty);
        if !self
            .table
            .compatible(self.db, resolved_value, declared.body)
        {
            let range = self.blame_range(value);
            let error = self.type_mismatch(range, declared.body, resolved_value, Some(value));
            self.contained_errors.push(self.errors.len());
            self.errors.push(error);
        }
        declared.body
    }

    /// Strict mode's untyped parameters, judged once the body is inferred. A parameter is
    /// untyped when nothing constrains it: its type is `Unknown`, or still a fresh type
    /// variable of this function that no other parameter and not the return mentions, so
    /// any value is accepted unchecked. A variable shared with another part of the
    /// signature is genuine polymorphism (`function(x) x`). `parameters` pairs each formal
    /// with its type, or with `None` where an annotation declared it.
    fn record_untyped_parameters(
        &mut self,
        function: ExprId,
        parameters: &[(&Formal, Option<Ty<'db>>)],
        ret: Ty<'db>,
    ) {
        if !self.record_strict_origins {
            return;
        }
        let db = self.db;
        let level = self.table.level;
        let mut mentioned: FxHashMap<InferenceVar, usize> = FxHashMap::default();
        for ty in parameters.iter().filter_map(|(_, ty)| *ty).chain([ret]) {
            let mut found = FxHashSet::default();
            self.table.collect_unbound_vars(db, ty, &mut found);
            for var in found {
                *mentioned.entry(var).or_default() += 1;
            }
        }
        for (parameter, ty) in parameters {
            let Some(ty) = ty else {
                continue;
            };
            let untyped = match self.table.shallow_resolve(db, *ty).kind(db) {
                TyKind::Unknown => true,
                TyKind::Var(var) => {
                    let representative = self.table.find(*var);
                    matches!(
                        self.table.entry(representative),
                        Entry::Unbound {
                            level: var_level,
                            constraint: Constraint::Unconstrained,
                            bound: None,
                        } if *var_level >= level
                    ) && mentioned.get(&representative) == Some(&1)
                }
                _ => false,
            };
            if !untyped {
                continue;
            }
            // A lambda passed as an argument gets its parameters' types from the call, which
            // unifies only after the lambda is inferred, so it is not judged here. An R6 method
            // gets no types from `R6Class`, so it is judged like a definition.
            let enclosing_call = |value: ExprId| {
                self.module.kinds().iter().find_map(|kind| match kind {
                    ExpressionKind::Call { callee, arguments }
                        if self.module[*arguments]
                            .iter()
                            .any(|argument| argument.value == Some(value)) =>
                    {
                        Some(Some(*callee))
                    }
                    ExpressionKind::Index { arguments, .. }
                        if self.module[*arguments]
                            .iter()
                            .any(|argument| argument.value == Some(value)) =>
                    {
                        Some(None)
                    }
                    _ => None,
                })
            };
            if let Some(callee) = enclosing_call(function)
                && !self.is_r6_method(callee, enclosing_call)
            {
                return;
            }
            let origin = StrictOrigin {
                expression: function,
                range: parameter.range,
                kind: StrictOriginKind::UntypedParameter(parameter.name),
            };
            if !self.strict_origins.contains(&origin) {
                self.strict_origins.push(origin);
            }
        }
    }

    /// Whether a lambda whose call is `callee` is an R6 method: an item of a `list(...)` that is
    /// itself an argument of `R6Class(...)`.
    fn is_r6_method(
        &self,
        callee: Option<ExprId>,
        enclosing_call: impl Fn(ExprId) -> Option<Option<ExprId>>,
    ) -> bool {
        let names = |callee: Option<ExprId>, wanted: &str| {
            callee.is_some_and(|callee| match self.module.kind(callee) {
                ExpressionKind::NameRef(name) => name == wanted,
                ExpressionKind::Namespace { name, .. } => name.as_deref() == Some(wanted),
                _ => false,
            })
        };
        if !names(callee, "list") {
            return false;
        }
        let Some(list) = callee.and_then(|callee| {
            self.module
                .kinds()
                .iter()
                .enumerate()
                .find_map(|(index, kind)| match kind {
                    ExpressionKind::Call {
                        callee: list_callee,
                        ..
                    } if *list_callee == callee => Some(ExprId::new(index)),
                    _ => None,
                })
        }) else {
            return false;
        };
        enclosing_call(list).is_some_and(|outer| names(outer, "R6Class"))
    }

    /// Annotating is exactly what a strict finding asks for, so it clears the origins inside
    /// the value it types: `#: double[]` over `as.numeric(d$age)` covers `d$age`. A function
    /// literal or braced block inside keeps its findings, because its own statements can be
    /// annotated where they are.
    /// Drops the strict origins inside an annotated value, and says whether there were any.
    fn annotation_covers(&mut self, value: ExprId) -> bool {
        if self.strict_origins.is_empty() {
            return false;
        }
        let mut covered = FxHashSet::default();
        let mut pending = vec![value];
        while let Some(id) = pending.pop() {
            covered.insert(id);
            if id != value
                && matches!(
                    self.module.kind(id),
                    ExpressionKind::Function { .. } | ExpressionKind::Block { .. }
                )
            {
                continue;
            }
            self.module.for_each_child(id, |child| pending.push(child));
        }
        let before = self.strict_origins.len();
        self.strict_origins
            .retain(|origin| !covered.contains(&origin.expression));
        self.strict_origins.len() < before
    }

    fn literal_ty(&mut self, literal: &LiteralKind) -> Ty<'db> {
        match literal {
            LiteralKind::Integer(_) => scalar(self.db, Atomic::Integer),
            LiteralKind::Double(_) => scalar(self.db, Atomic::Double),
            LiteralKind::Complex(_) => scalar(self.db, Atomic::Complex),
            LiteralKind::Suffixed { ty, .. } => native_scalar(self.db, suffix_scalar(*ty)),
            LiteralKind::String(_) => scalar(self.db, Atomic::Character),
            LiteralKind::Logical(_) => scalar(self.db, Atomic::Logical),
            LiteralKind::Null => null(self.db),
            LiteralKind::Na(atom) => scalar(
                self.db,
                match atom {
                    NaAtom::Logical => Atomic::Logical,
                    NaAtom::Integer => Atomic::Integer,
                    NaAtom::Double => Atomic::Double,
                    NaAtom::Complex => Atomic::Complex,
                    NaAtom::Character => Atomic::Character,
                },
            ),
            LiteralKind::Inf | LiteralKind::NaN => scalar(self.db, Atomic::Double),
        }
    }

    fn infer_read(&mut self, id: ExprId) -> Ty<'db> {
        if !self.column_scopes.is_empty()
            && let Some(ty) = self.read_column(id)
        {
            return ty;
        }
        let Some(&slot) = self.naming.resolutions.get(&id) else {
            return self.read_non_local(id);
        };
        let mut ty = self.read_slot(id, slot);
        if let Some(fallback) = self.unassigned_path_read(id, slot) {
            self.default_join_literals(ty, fallback);
            let db = self.db;
            ty = union_of(
                db,
                [self.table.resolve(db, ty), self.table.resolve(db, fallback)],
            );
        }
        // Only an operand whose other operand is a ry number may still decide a variable
        // written with a literal; every other read makes it R's type.
        if !self.literal_reads.contains(id) && self.open_literal_var(ty) {
            self.default_literal_var(ty, false);
        }
        if self.unseen_frames > 0
            && matches!(self.table.resolve(self.db, ty).kind(self.db), TyKind::Null)
        {
            return unknown(self.db);
        }
        ty
    }

    fn read_non_local(&mut self, id: ExprId) -> Ty<'db> {
        // Unresolved reads stay a silent `Unknown` (naming owns that diagnostic). A guard
        // may have refined this name earlier; reading the binding again would discard it.
        if let Some(name) = self.naming.non_locals.get(&id)
            && let Some(&slot) = self.non_local_slots.get(name)
            && let Some(EnvEntry::Mono(refined)) = self.environment.get(slot)
        {
            return refined;
        }
        let naming = self.naming;
        let deferred = naming.deferred_non_locals.contains(&id);
        // The item writes this name with `<<-`, so the binding it reads is the one it keeps
        // updating (a Reference Class field, a closure's state), not a shipped function that
        // happens to share the name, such as `t`.
        if let Some(name) = naming.non_locals.get(&id)
            && self
                .super_assigned
                .binary_search_by_key(&name.as_str(), |written| written.as_str())
                .is_ok()
            && !self
                .globals
                .is_some_and(|globals| globals.defined_in_project(*name, deferred))
        {
            self.record_strict_origin(
                id,
                StrictOriginKind::UndeterminedReference(name.to_string()),
            );
            return unknown(self.db);
        }
        if let Some(name) = naming.non_locals.get(&id)
            && let Some(scheme) = self
                .globals
                .and_then(|globals| globals.scheme(*name, naming.deferred_non_locals.contains(&id)))
        {
            let instantiated = self.instantiate(&scheme);
            // A project-defined name owns its `Unknown` at its defining site, so reading it
            // propagates rather than re-originating.
            if matches!(
                self.table.resolve(self.db, instantiated).kind(self.db),
                TyKind::Unknown
            ) && !self
                .globals
                .is_some_and(|globals| globals.defined_in_project(*name, deferred))
            {
                self.record_strict_origin(
                    id,
                    StrictOriginKind::UndeterminedReference(name.to_string()),
                );
            }
            return instantiated;
        }
        if self.query_depth > 0
            && let Some(name) = naming.quiet_reads.get(&id)
            && let Some(ty) = self.quiet_global_read(id, name)
        {
            return ty;
        }
        if let Some(name) = naming.non_locals.get(&id)
            && let Some(scheme) = operator_value(self.db, name)
        {
            return self.instantiate(&scheme);
        }
        // A package's export list or the project's imports name the binding, but no stub
        // types it. An unresolved name is not an origin: naming reports it. A base name with no
        // stub is an intrinsic such as `c`, `list` or `[[` whose calls are typed specially, so
        // reading it as a value is left unchecked rather than reported.
        if let Some(name) = naming.non_locals.get(&id)
            && !is_base_export(self.db, name)
            && (self
                .globals
                .is_some_and(|globals| globals.library_candidates(*name).is_some())
                || imported_by_name(self.db, name))
        {
            self.record_strict_origin(
                id,
                StrictOriginKind::UndeterminedReference(name.to_string()),
            );
        }
        unknown(self.db)
    }

    fn truncate_errors(&mut self, length: usize) {
        self.errors.truncate(length);
        self.contained_errors.retain(|&index| index < length);
    }

    /// The errors from `at` on, which lose their containment: a caller that puts them back
    /// leaves the export untrusted, the safe side.
    fn split_off_errors(&mut self, at: usize) -> Vec<TypeError<'db>> {
        self.contained_errors.retain(|&index| index < at);
        self.errors.split_off(at)
    }

    fn remove_error(&mut self, index: usize) {
        self.errors.remove(index);
        self.contained_errors
            .retain(|&contained| contained != index);
        for contained in &mut self.contained_errors {
            if *contained > index {
                *contained -= 1;
            }
        }
    }

    /// What a function's read of its own `slot` finds on a path that has not written the slot:
    /// R looks the name up outside the function, in an enclosing function or the package.
    fn unassigned_path_read(&mut self, id: ExprId, slot: BindingId) -> Option<Ty<'db>> {
        let naming = self.naming;
        let ty = if let Some(&enclosing) = naming.extras().enclosing_fallbacks.get(&id) {
            self.read_slot(id, enclosing)
        } else {
            if !naming.maybe_undefined.contains(&id) {
                return None;
            }
            let binding = naming.bindings.get(&slot)?;
            if binding.kind == BindingKind::TopLevel {
                return None;
            }
            let scheme = self.globals?.scheme(binding.name, true)?;
            self.instantiate(&scheme)
        };
        // A function under a local variable, as base's `max` under `max <- 3` or a package's
        // `t2` under `if (trace) t2 <- proc.time()`, is left out: a variable named like a
        // function is the common case, a read guarded like its write never reaches the
        // function. A read that does reach it goes unreported (`backlog.md`, "A local
        // variable named like a function").
        let function = matches!(
            self.table.resolve(self.db, ty).kind(self.db),
            TyKind::Function(_)
        );
        (!function).then_some(ty)
    }

    /// The value `slot` holds at the read `id`.
    fn read_slot(&mut self, id: ExprId, slot: BindingId) -> Ty<'db> {
        match self.environment.get(slot) {
            Some(EnvEntry::Mono(ty)) => self.widen_captured_read(id, slot, ty),
            Some(EnvEntry::Scheme(index)) => {
                let scheme = self.scheme_arena[index as usize].clone();
                let ty = self.instantiate(&scheme);
                self.widen_captured_read(id, slot, ty)
            }
            Some(EnvEntry::MissingFormal(_)) => {
                let name = self
                    .naming
                    .bindings
                    .get(&slot)
                    .map(|info| info.name.to_string())
                    .unwrap_or_default();
                let range = self.module.range(id);
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::MissingFormalRead { name },
                });
                unknown(self.db)
            }
            // A read before any write: a captured slot reads the running join of the frame's
            // writes (the closure runs later); a top-level slot reads the cross-item binding,
            // materialized so a loop join keeps it as the pre-state.
            None => {
                if self.naming.extras().captured_slots.contains(&slot) {
                    self.forward_captured.insert(slot);
                    if let Some(&join) = self.capture_joins.get(slot) {
                        return join;
                    }
                    // A local function's read of itself or of a later local function: the
                    // repass fills the slot in, so this `Unknown` is a placeholder.
                    self.placeholder_reads += 1;
                } else if let Some(observed) = self.materialize_pre_state(slot, false) {
                    return observed;
                }
                unknown(self.db)
            }
        }
    }

    /// A closure runs after its definition, so its read of an enclosing variable also sees the
    /// writes that come later in the frame: `get <- function() x; x <- NULL` reads `x` as
    /// possibly `NULL`, whatever narrowing held where `get` was defined.
    fn widen_captured_read(&mut self, id: ExprId, slot: BindingId, ty: Ty<'db>) -> Ty<'db> {
        let Some(&horizon) = self.naming.extras().captured_reads.get(&id) else {
            return ty;
        };
        let position = self.module.range(id).start();
        let resolved = self.table.shallow_resolve(self.db, ty);
        let open = resolved.is_var();
        let read_literal = self.open_literal_constraint(ty).map(|(_, default)| default);
        // Only a ry scalar decides a literal: in R, `x <- 2.5` makes an `integer` `x` a `double`.
        let native = matches!(resolved.kind(self.db), TyKind::Native(_));
        if self
            .late_reads
            .get(slot)
            .is_none_or(|&earliest| position < earliest)
        {
            self.late_reads.insert(slot, position);
        }
        // A literal written later takes a ry scalar read's type when it can, as `x <- 2` would,
        // and one with the default of a literal read takes whatever type that read takes.
        let later: SmallVec<[Ty<'db>; 2]> = self
            .capture_writes
            .get(slot)
            .into_iter()
            .flatten()
            // The reading body's own `<<-` writes are left to its superassignment seeds.
            .filter(|(at, late)| {
                *at > position
                    && *at <= horizon
                    && !self.body_range.contains(*at)
                    && late.literal.is_none_or(|constraint| match read_literal {
                        Some(default) => constraint.literal_default() != Some(default),
                        None => {
                            !open
                                && !(native
                                    && constraint_rejects(self.db, &self.table, constraint, ty)
                                        .is_none())
                        }
                    })
            })
            .map(|(_, late)| late.ty)
            .collect();
        later.into_iter().fold(ty, |joined, written| {
            self.join_closure_write(joined, written)
        })
    }

    /// A bare name inside a query reads the frame's column before any lexical binding, as
    /// data.table evaluates `i`, `j` and `by` in the data's own frame. `None` falls back to
    /// the lexical read.
    fn read_column(&mut self, id: ExprId) -> Option<Ty<'db>> {
        let module = self.module;
        let ExpressionKind::NameRef(name) = module.kind(id) else {
            return None;
        };
        let resolved = self.naming.resolutions.get(&id).copied();
        // R looks a called name up as a function, which skips every column.
        if self
            .column_scopes
            .iter()
            .any(|scope| scope.callees.binary_search(&id).is_ok())
        {
            return match resolved {
                Some(_) => None,
                None => self.quiet_global_read(id, name),
            };
        }
        let scope = self.column_scopes.last()?;
        if let Some(slot) = resolved
            && self
                .naming
                .bindings
                .get(&slot)
                .is_some_and(|binding| scope.bracket.contains_range(binding.range))
        {
            return None;
        }
        // `..name` is the variable `name` from the enclosing scope, which naming resolved.
        if dot_dot_variable(name).is_some() {
            return None;
        }
        let special = match name.as_str() {
            ".N" | ".GRP" | ".NGRP" => Some(scalar(self.db, Atomic::Integer)),
            ".I" => Some(Ty::new(
                self.db,
                TyKind::Vector(scalar(self.db, Atomic::Integer)),
            )),
            ".SD" => Some(scope.subset_data),
            ".BY" | ".EACHI" => Some(unknown(self.db)),
            _ => None,
        };
        if let Some(ty) = special {
            self.masked_reads.insert(id);
            return Some(ty);
        }
        // Inner queries see the outer frames' columns too, since `j` runs in the outer data.
        let mut all_known = true;
        for scope in self.column_scopes.iter().rev() {
            match &scope.columns {
                Some(columns) => {
                    if let Some(column) = columns
                        .iter()
                        .find(|column| column.name.text(self.db) == name)
                    {
                        let ty = column.ty;
                        self.masked_reads.insert(id);
                        return Some(ty);
                    }
                }
                None => all_known = false,
            }
        }
        // After a join, `x.col` and `i.col` read a column of one side, including a key or a
        // column whose name does not clash.
        let prefixed = name
            .strip_prefix("x.")
            .map(|column| (true, column))
            .or_else(|| name.strip_prefix("i.").map(|column| (false, column)));
        let mut side_columns = Vec::new();
        if let Some((from_x, column)) = prefixed
            && let Some(join) = self
                .column_scopes
                .iter()
                .rev()
                .find_map(|scope| scope.join.as_ref())
        {
            let (prefix, side) = if from_x {
                ("x.", &join.x)
            } else {
                ("i.", &join.i)
            };
            match side {
                Some(columns) => {
                    if let Some(found) = columns
                        .iter()
                        .find(|candidate| candidate.name.text(self.db) == column)
                    {
                        let ty = found.ty;
                        self.masked_reads.insert(id);
                        return Some(ty);
                    }
                    side_columns = columns
                        .iter()
                        .map(|candidate| format!("{prefix}{}", candidate.name.text(self.db)))
                        .collect();
                }
                None => all_known = false,
            }
        }
        if let Some(slot) = resolved {
            // `col <- NULL` before a query is the idiom that silences R CMD check's "no
            // visible binding" note, so when the columns are unknown a `NULL` local is the
            // column, not the value data.table would read.
            if !all_known {
                let ty = self.read_slot(id, slot);
                if matches!(self.table.resolve(self.db, ty).kind(self.db), TyKind::Null) {
                    self.masked_reads.insert(id);
                    return Some(unknown(self.db));
                }
                return Some(ty);
            }
            return None;
        }
        // A prefixed name outside a join ry can see is data.table syntax ry cannot check.
        if !all_known || prefixed.is_some() && side_columns.is_empty() {
            self.masked_reads.insert(id);
            return Some(unknown(self.db));
        }
        let scope = self.column_scopes.last()?;
        let miss = ColumnMiss {
            frame: scope.frame,
            columns: scope
                .columns
                .iter()
                .flatten()
                .map(|column| column.name.text(self.db).to_owned())
                .chain(side_columns)
                .collect(),
        };
        self.column_misses.insert(id, miss);
        self.quiet_global_read(id, name)
    }

    /// The plain read gives a quiet read no global, but inside a query a name that is no
    /// column does fall back to the enclosing scope at run time.
    fn quiet_global_read(&mut self, id: ExprId, name: &str) -> Option<Ty<'db>> {
        let naming = self.naming;
        if !naming.quiet_reads.contains_key(&id) {
            return None;
        }
        let deferred = naming.extras().deferred_quiet_reads.contains(&id);
        let scheme = self
            .globals
            .and_then(|globals| globals.scheme(Symbol::new(name), deferred));
        Some(match scheme {
            Some(scheme) => self.instantiate(&scheme),
            None => unknown(self.db),
        })
    }

    /// A top-level slot's cross-item binding, materialized as the slot's pre-state. With
    /// `earlier`, it is the binding as it stood before this item, for a branch or loop path
    /// that did not write the slot: a name only conditional statements write has no earlier
    /// value, so that path is maybe-undefined.
    fn materialize_pre_state(&mut self, slot: BindingId, earlier: bool) -> Option<Ty<'db>> {
        let globals = self.globals?;
        let scheme = match self.naming.bindings.get(&slot) {
            Some(binding) if binding.kind != BindingKind::TopLevel => return None,
            Some(binding) if earlier => globals.earlier_binding(binding.name)?,
            Some(binding) => globals.scheme(binding.name, false)?,
            // A `<<-` slot: the path that did not write it still reads the top-level binding,
            // which a function body reads in its final state whatever the collation order. A
            // top-level `NULL` is the placeholder of a lazily filled cache, and the path that
            // skips `if (is.null(cache)) cache <<- ...` is the one that already filled it.
            None => {
                let (&name, _) = self
                    .non_local_slots
                    .iter()
                    .find(|(_, non_local)| **non_local == slot)?;
                let scheme = globals.scheme(name, false)?;
                if matches!(scheme.body.kind(self.db), TyKind::Null) {
                    return None;
                }
                scheme
            }
        };
        let instantiated = self.instantiate(&scheme);
        // An `Unknown` binding adds nothing over the tolerant read, and materializing it
        // would absorb the real body writes at the loop join.
        if matches!(
            self.table.resolve(self.db, instantiated).kind(self.db),
            TyKind::Unknown
        ) {
            return None;
        }
        self.environment.set(slot, EnvEntry::Mono(instantiated));
        self.pre_materialized.insert(slot, instantiated);
        Some(instantiated)
    }

    /// zeallot's `c(a, b) %<-% value` gives each name the item at its position: a tuple's or
    /// record's item, a list's element, or an atomic vector's element. `.` skips a position, and a
    /// `...rest` name takes what is left (names after it read from the end), so with one the item
    /// count is not checked. A shape with
    /// no positions to read leaves the names `Unknown`, which strict mode reports at each name.
    fn destructure(&mut self, pattern: ExprId, value_ty: Ty<'db>) {
        let db = self.db;
        let ExpressionKind::Call { arguments, .. } = self.module.kind(pattern) else {
            return;
        };
        let elements: SmallVec<[ExprId; 4]> = self.module[arguments]
            .iter()
            .filter_map(|argument| argument.value)
            .collect();
        let rest_at = elements.iter().position(|&element| {
            matches!(self.module.kind(element), ExpressionKind::NameRef(name) if name.starts_with("..."))
        });
        let items: Option<SmallVec<[Ty<'db>; 4]>> = match value_ty.kind(db) {
            TyKind::Tuple(items) => Some(items.iter().copied().collect()),
            TyKind::Record(fields) => Some(fields.iter().map(|field| field.ty).collect()),
            _ => None,
        };
        if let Some(items) = &items
            && rest_at.is_none()
            && items.len() != elements.len()
        {
            self.errors.push(TypeError {
                range: self.module.range(pattern),
                kind: TypeErrorKind::DestructureCount {
                    names: elements.len(),
                    items: items.len(),
                },
            });
        }
        for (position, &element) in elements.iter().enumerate() {
            let past_rest = rest_at.is_some_and(|rest| position >= rest);
            let ty = match (&items, value_ty.kind(db)) {
                (_, TyKind::Unknown | TyKind::Any) => value_ty,
                (Some(items), _) => match rest_at {
                    Some(rest) if position == rest => {
                        let trailing = elements.len() - rest - 1;
                        let taken = items.get(rest..items.len().saturating_sub(trailing));
                        taken.map_or_else(|| unknown(db), |taken| Ty::new(db, TyKind::Tuple(taken)))
                    }
                    Some(_) if past_rest => (items.len() + position)
                        .checked_sub(elements.len())
                        .and_then(|index| items.get(index).copied())
                        .unwrap_or_else(|| unknown(db)),
                    _ => items.get(position).copied().unwrap_or_else(|| unknown(db)),
                },
                (None, TyKind::List(element) | TyKind::NamedList(element)) if !past_rest => {
                    *element
                }
                (None, TyKind::Vector(element) | TyKind::NamedVector(element)) if !past_rest => {
                    *element
                }
                (None, TyKind::Scalar(_)) if !past_rest => value_ty,
                (
                    None,
                    TyKind::List(_)
                    | TyKind::NamedList(_)
                    | TyKind::Vector(_)
                    | TyKind::NamedVector(_),
                ) if rest_at == Some(position) => value_ty,
                _ => unknown(db),
            };
            match self.module.kind(element) {
                ExpressionKind::NameRef(name) if name == "." => {}
                ExpressionKind::NameRef(_) => {
                    if let Some(&slot) = self.naming.resolutions.get(&element) {
                        self.environment.set(slot, EnvEntry::Mono(ty));
                        self.note_captured_write(element, slot, ty);
                    }
                    self.recorded.insert(element, ty);
                    if matches!(ty.kind(db), TyKind::Unknown)
                        && !matches!(value_ty.kind(db), TyKind::Unknown)
                    {
                        self.record_strict_origin(element, StrictOriginKind::UnsupportedConstruct);
                    }
                }
                ExpressionKind::Call { .. } => {
                    let ty = self.table.resolve(db, ty);
                    self.destructure(element, ty);
                }
                _ => {}
            }
        }
    }

    fn write_target(
        &mut self,
        spelling: AssignSpelling,
        target: ExprId,
        value: ExprId,
        value_ty: Ty<'db>,
    ) {
        if spelling == AssignSpelling::Destructure {
            let value_ty = self.table.resolve(self.db, value_ty);
            self.destructure(target, value_ty);
            return;
        }
        // `"x" <- 1` binds `x` as `x <- 1` does.
        if matches!(
            self.module.kind(target),
            ExpressionKind::NameRef(_) | ExpressionKind::Literal(LiteralKind::String(_))
        ) {
            if let Some(&slot) = self.naming.resolutions.get(&target) {
                let literal = spelling != AssignSpelling::Super
                    && literal_strings(self.module, value).is_some();
                let write = StringWrite { target, value };
                let tracked = match self.string_values.get(slot) {
                    None if literal => Some(write),
                    Some(Some(existing)) if literal && *existing == write => Some(write),
                    _ => None,
                };
                self.string_values.insert(slot, tracked);
                if spelling == AssignSpelling::Super {
                    // The rest of this body reads the value just written. The scopes that
                    // hold the variable join it with their own writes once the body ends.
                    let written = self.table.resolve(self.db, value_ty);
                    if let Some(position) = self
                        .superassign_seeds
                        .iter()
                        .position(|seed| seed.slot == slot && !seed.written)
                    {
                        let seed = self.superassign_seeds[position].variable;
                        // A read before this first write already used the variable as a
                        // value of some type, so a write of another type breaks that read.
                        let snapshot = self.table.snapshot();
                        match self.table.unify(self.db, seed, written) {
                            Ok(()) => self.superassign_seeds[position].written = true,
                            // An accumulator `x <<- c(x, list(item))` reads itself inside the
                            // write, which the occurs check rejects; the seed stays open.
                            Err(UnifyError::Occurs(..)) => self.table.rollback(snapshot),
                            Err(error) => {
                                self.table.rollback(snapshot);
                                let range = self.module.range(value);
                                self.report_unify(range, error);
                            }
                        }
                    }
                    self.environment.set(slot, EnvEntry::Mono(written));
                    self.pending_enclosing_writes.push((slot, written));
                    self.note_captured_write(target, slot, value_ty);
                } else {
                    // Let-polymorphism: only function values generalize.
                    let resolved = self.table.shallow_resolve(self.db, value_ty);
                    if matches!(resolved.kind(self.db), TyKind::Function(_)) {
                        if matches!(self.environment.get(slot), Some(EnvEntry::MissingFormal(_))) {
                            self.missing_path_functions.insert(slot);
                        }
                        let scheme = self.generalize(value_ty);
                        let index = self.scheme_arena.len() as u32;
                        self.scheme_arena.push(scheme);
                        self.environment.set(slot, EnvEntry::Scheme(index));
                    } else {
                        self.table
                            .adjust_levels(self.db, self.frame_level, value_ty);
                        self.environment.set(slot, EnvEntry::Mono(value_ty));
                    }
                }
                self.note_captured_write(target, slot, value_ty);
            } else if spelling == AssignSpelling::Super
                && let Some(&name) = self.naming.non_locals.get(&target)
            {
                // `<<-` to a variable no enclosing function binds writes the global one, and
                // the rest of this flow reads what it wrote, not the top-level binding.
                let written = self.table.resolve(self.db, value_ty);
                let slot = self.non_local_slot(name);
                self.environment.set(slot, EnvEntry::Mono(written));
            }
            self.recorded.insert(target, value_ty);
        } else {
            // `cols[2] <- "x"` and `names(cols) <- ...` change the strings `cols` holds.
            let mut root = target;
            loop {
                match self.module.kind(root) {
                    ExpressionKind::Index { target, .. } | ExpressionKind::Field { target, .. } => {
                        root = *target;
                    }
                    ExpressionKind::Call { arguments, .. } => {
                        match self.module[arguments]
                            .first()
                            .and_then(|argument| argument.value)
                        {
                            Some(first) => root = first,
                            None => break,
                        }
                    }
                    ExpressionKind::Paren(inner) => root = *inner,
                    _ => break,
                }
            }
            if let Some(&slot) = self.naming.resolutions.get(&root) {
                self.string_values.insert(slot, None);
            }
            self.write_replacement_target(spelling, target, value, value_ty);
        }
    }

    /// The column names `id` spells: a string, `c()` of strings, or a variable whose only
    /// write is one of those. Each name carries the range of the literal that spells it.
    fn column_names(&self, id: ExprId) -> Option<Vec<(String, TextRange)>> {
        let module = self.module;
        match module.kind(id) {
            ExpressionKind::NameRef(_) => {
                let slot = self.naming.resolutions.get(&id)?;
                let write = self.string_values.get(*slot)?.as_ref()?;
                literal_strings(module, write.value)
            }
            ExpressionKind::Paren(inner) => self.column_names(*inner),
            _ => literal_strings(module, id),
        }
    }

    fn note_captured_write(&mut self, write: ExprId, slot: BindingId, value_ty: Ty<'db>) {
        if !self.naming.extras().captured_slots.contains(&slot) {
            return;
        }
        let erased = erase_vars(self.db, self.table.resolve(self.db, value_ty));
        // An R literal decides its type only later, after this record has outlived the table
        // it was decided in, so `x <- 5` keeps the literal's constraint and its default.
        let late = match self.open_literal_constraint(value_ty) {
            Some((constraint, atomic)) => LateWrite {
                ty: scalar(self.db, atomic),
                literal: Some(constraint),
            },
            None => LateWrite {
                ty: erased,
                literal: None,
            },
        };
        let position = self.module.range(write).start();
        let mut writes = self.capture_writes.remove(slot).unwrap_or_default();
        let late_changed = match writes.iter_mut().find(|(at, _)| *at == position) {
            Some((_, existing)) if *existing == late => false,
            Some((_, existing)) => {
                *existing = late;
                true
            }
            None => {
                writes.push((position, late));
                true
            }
        };
        self.capture_writes.insert(slot, writes);
        if late_changed
            && let Some(&read) = self.late_reads.get(slot)
            && read < position
        {
            self.late_triggers.push(read);
        }
        let joined = match self.capture_joins.get(slot) {
            Some(&existing) if existing != erased => self.join_types(existing, erased),
            Some(&existing) => existing,
            None => erased,
        };
        let changed = self.capture_joins.get(slot) != Some(&joined);
        self.capture_joins.insert(slot, joined);
        if changed && self.forward_captured.contains(slot) {
            self.capture_repass_needed = true;
        }
    }

    fn join_enclosing_write(&mut self, slot: BindingId, written: Ty<'db>) {
        let entry = match self.environment.get(slot) {
            Some(EnvEntry::Mono(existing)) if existing != written => {
                EnvEntry::Mono(self.join_closure_write(existing, written))
            }
            Some(entry @ EnvEntry::Mono(_)) => entry,
            Some(EnvEntry::Scheme(index)) => {
                let scheme = self.scheme_arena[index as usize].clone();
                let instantiated = self.instantiate(&scheme);
                EnvEntry::Mono(self.join_types(instantiated, written))
            }
            // A write on the missing branch supplies the formal.
            Some(EnvEntry::MissingFormal(existing)) => {
                EnvEntry::Mono(self.join_types(existing, written))
            }
            None => EnvEntry::Mono(written),
        };
        self.environment.set(slot, entry);
    }

    /// Closures that write one variable with `<<-` run in any order, so no write constrains
    /// another: `cache <<- fun()` beside a reset `cache <<- TRUE` leaves `fun` returning
    /// anything. A side that is still an unconstrained variable therefore joins as a union
    /// member instead of being bound to the other side.
    fn join_closure_write(&mut self, existing: Ty<'db>, written: Ty<'db>) -> Ty<'db> {
        self.default_join_literals(existing, written);
        let open_variable = |checker: &Self, ty: Ty<'db>| {
            let resolved = checker.table.resolve(checker.db, ty);
            guard_members(checker.db, resolved)
                .iter()
                .any(|member| match member.kind(checker.db) {
                    TyKind::Var(var) => matches!(
                        checker.table.entry(*var),
                        Entry::Unbound {
                            constraint: Constraint::Unconstrained,
                            ..
                        }
                    ),
                    _ => false,
                })
        };
        if open_variable(self, existing) || open_variable(self, written) {
            let existing = self.table.resolve(self.db, existing);
            let written = self.table.resolve(self.db, written);
            return union_of(self.db, [existing, written]);
        }
        self.join_types(existing, written)
    }

    /// Kept recorded so each further enclosing scope re-applies them too (the join is idempotent).
    fn reapply_enclosing_writes(&mut self, mark: usize) {
        let writes: Vec<(BindingId, Ty<'db>)> = self.pending_enclosing_writes[mark..].to_vec();
        for (slot, written) in writes {
            self.join_enclosing_write(slot, written);
        }
    }

    fn write_replacement_target(
        &mut self,
        spelling: AssignSpelling,
        target: ExprId,
        value: ExprId,
        value_ty: Ty<'db>,
    ) {
        let base = self.replacement_base(target);
        self.infer_replacement_spine(target, base);
        let Some(base) = base else {
            // No variable at the spine's root (`f(x)$a <- v`): R rejects this at run time.
            self.record_strict_origin(target, StrictOriginKind::UnsupportedConstruct);
            return;
        };
        let prior = self.infer(base);
        let prior = self.table.resolve(self.db, prior);
        let value_resolved = self.table.resolve(self.db, value_ty);
        let written = self.replacement_written_type(target, base, value, prior, value_resolved);
        // Only a ry list decides a literal it is written; any other container gives it R's type.
        if self.open_literal_var(value_ty) {
            self.default_literal_var(value_ty, false);
        }
        if let Some(&slot) = self.naming.resolutions.get(&base) {
            self.environment.set(slot, EnvEntry::Mono(written));
            // `x$a <<- v` in a closure writes the enclosing `x`, as `x <<- v` does, so the
            // defining frame joins it and drops a narrowing of `x` or of `x$a`.
            if spelling == AssignSpelling::Super {
                self.pending_enclosing_writes.push((slot, written));
            }
            self.note_captured_write(base, slot, written);
        }
    }

    /// An abstract nominal or a non-record representation admits nothing statically.
    fn check_nominal_field_write(
        &mut self,
        nominal: Ty<'db>,
        field_name: &str,
        value_expression: ExprId,
        value: Ty<'db>,
    ) {
        let representation = self.structural(nominal);
        let TyKind::Record(fields) = representation.kind(self.db) else {
            return;
        };
        let range = self.blame_range(value_expression);
        match fields
            .iter()
            .find(|field| field.name.text(self.db) == field_name)
        {
            Some(field) => {
                if !matches!(value.kind(self.db), TyKind::Unknown)
                    && !self.table.compatible(self.db, value, field.ty)
                {
                    let kind = self.type_mismatch(range, field.ty, value, None).kind;
                    self.errors.push(TypeError { range, kind });
                }
            }
            None => self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::FieldDoesNotExist {
                    suggestion: nearest_name(
                        field_name,
                        fields.iter().map(|field| field.name.text(self.db)),
                    ),
                    field: field_name.to_owned(),
                    container: nominal,
                },
            }),
        }
    }

    /// `names(x) <- v` calls `names<-` on `x`, so a call's first argument continues the spine.
    fn replacement_base(&self, id: ExprId) -> Option<ExprId> {
        match self.module.kind(id) {
            ExpressionKind::NameRef(_) => Some(id),
            ExpressionKind::Index { target, .. } | ExpressionKind::Field { target, .. } => {
                self.replacement_base(*target)
            }
            ExpressionKind::Call { arguments, .. } => self.module[arguments]
                .first()
                .and_then(|argument| argument.value)
                .and_then(|value| self.replacement_base(value)),
            _ => None,
        }
    }

    /// Skips a replacement call's callee (`names<-` is called, not `names`) and the base
    /// variable, whose read supplies the prior type separately.
    fn infer_replacement_spine(&mut self, id: ExprId, base: Option<ExprId>) {
        if Some(id) == base {
            return;
        }
        let module = self.module;
        match module.kind(id) {
            ExpressionKind::Index {
                target, arguments, ..
            } => {
                self.infer_replacement_spine(*target, base);
                for argument in &module[arguments] {
                    if let Some(value) = argument.value {
                        self.infer(value);
                    }
                }
            }
            ExpressionKind::Field { target, .. } => {
                self.infer_replacement_spine(*target, base);
            }
            ExpressionKind::Call { arguments, .. } => {
                let mut argument_iter = module[arguments].iter();
                if let Some(first) = argument_iter.next()
                    && let Some(value) = first.value
                {
                    self.infer_replacement_spine(value, base);
                }
                for argument in argument_iter {
                    if let Some(value) = argument.value {
                        self.infer(value);
                    }
                }
            }
            _ => {
                self.infer(id);
            }
        }
    }

    /// A nested write (`x[[1]]$a <- v`) rewrites each container on the way back up the
    /// spine: the innermost one takes the value, and each enclosing one takes the
    /// rewritten child. An intermediate container the spine cannot describe reads as
    /// `Unknown`, and a step it cannot model (`x@a`, `x[i]` on a list) leaves its container as
    /// it was.
    fn replacement_written_type(
        &mut self,
        lhs: ExprId,
        base: ExprId,
        value_expression: ExprId,
        prior: Ty<'db>,
        value: Ty<'db>,
    ) -> Ty<'db> {
        let Some((target, step)) = self.replacement_step(lhs) else {
            return prior;
        };
        if target == base {
            return self.write_replacement_step(lhs, step, value_expression, prior, value);
        }
        let parent_prior = self.project_replacement(target, base, prior);
        let parent = self.write_replacement_step(lhs, step, value_expression, parent_prior, value);
        self.replacement_written_type(target, base, value_expression, prior, parent)
    }

    /// The step `lhs` takes from its container, and that container's expression.
    fn replacement_step(&self, lhs: ExprId) -> Option<(ExprId, ReplacementStep)> {
        match self.module.kind(lhs) {
            ExpressionKind::Field {
                at: false,
                target,
                name: Some(name),
                ..
            } => Some((
                *target,
                ReplacementStep::Field(self.module.name(*name).to_string()),
            )),
            ExpressionKind::Index {
                double: true,
                target,
                arguments,
            } => Some((
                *target,
                match &self.module[arguments][..] {
                    [argument] if argument.name.is_none() => {
                        let index = argument.value.map(|value| self.module.kind(value));
                        match index {
                            Some(ExpressionKind::Literal(LiteralKind::String(name))) => {
                                ReplacementStep::Field(self.module[name].to_string())
                            }
                            Some(kind) => match integer_literal_position(self.module, kind) {
                                Some(position) => ReplacementStep::Position(position),
                                None => ReplacementStep::Element,
                            },
                            None => ReplacementStep::Element,
                        }
                    }
                    _ => ReplacementStep::Other,
                },
            )),
            ExpressionKind::Index {
                double: false,
                target,
                ..
            } => Some((*target, ReplacementStep::Slice)),
            ExpressionKind::Field { target, .. } => Some((*target, ReplacementStep::Other)),
            ExpressionKind::Call { callee, arguments } => {
                let step = match self.module.kind(*callee) {
                    ExpressionKind::NameRef(name) => match name.as_str() {
                        "formals" | "body" => ReplacementStep::Rewrite,
                        "names" => ReplacementStep::Names,
                        _ => ReplacementStep::Other,
                    },
                    _ => ReplacementStep::Other,
                };
                self.module[arguments]
                    .first()
                    .and_then(|argument| argument.value)
                    .map(|value| (value, step))
            }
            _ => None,
        }
    }

    /// A write into a field whose type is fixed: checked against it, or refused when the field
    /// does not exist.
    fn check_fixed_field_write(
        &mut self,
        lhs: ExprId,
        field_name: String,
        field: Option<Ty<'db>>,
        value_expression: ExprId,
        value: Ty<'db>,
        prior: Ty<'db>,
    ) {
        let module = self.module;
        let kind = module.kind(lhs);
        match field {
            // A literal written into the field takes the field's type, as one
            // written into a list's item does.
            Some(field_ty) => {
                if self.adapt_literal(value_expression, field_ty) == LiteralFit::NotLiteral
                    && !(self.open_literal_var(value)
                        && self.table.unify(self.db, value, field_ty).is_ok())
                    && !matches!(value.kind(self.db), TyKind::Unknown)
                    && !self.table.compatible(self.db, value, field_ty)
                {
                    let range = self.blame_range(value_expression);
                    let kind = self.type_mismatch(range, field_ty, value, None).kind;
                    self.contained_errors.push(self.errors.len());
                    self.errors.push(TypeError { range, kind });
                }
            }
            None => {
                let range = match kind {
                    ExpressionKind::Field {
                        name: Some(name), ..
                    } => module.name_range(*name),
                    _ => module.range(lhs),
                };
                self.contained_errors.push(self.errors.len());
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::FieldCannotBeAdded {
                        field: field_name,
                        container: prior,
                    },
                });
            }
        }
    }

    /// The type an intermediate container of a replacement spine holds before the write.
    fn project_replacement(&self, expression: ExprId, base: ExprId, prior: Ty<'db>) -> Ty<'db> {
        if expression == base {
            return prior;
        }
        let Some((target, step)) = self.replacement_step(expression) else {
            return unknown(self.db);
        };
        let container = self.project_replacement(target, base, prior);
        let container = self.table.resolve(self.db, container);
        match (step, container.kind(self.db)) {
            (
                ReplacementStep::Field(name),
                TyKind::Record(fields) | TyKind::Environment(fields),
            ) => fields
                .iter()
                .find(|field| field.name.text(self.db) == name)
                .map_or_else(|| null(self.db), |field| field.ty),
            (ReplacementStep::Position(position), TyKind::Tuple(items)) => {
                items.get(position).copied().unwrap_or(unknown(self.db))
            }
            (
                ReplacementStep::Field(_) | ReplacementStep::Position(_) | ReplacementStep::Element,
                TyKind::List(element) | TyKind::NamedList(element),
            ) => *element,
            _ => unknown(self.db),
        }
    }

    fn write_replacement_step(
        &mut self,
        lhs: ExprId,
        step: ReplacementStep,
        value_expression: ExprId,
        prior: Ty<'db>,
        value: Ty<'db>,
    ) -> Ty<'db> {
        if matches!(step, ReplacementStep::Rewrite) {
            return unknown(self.db);
        }
        // `attr(x, "a") <- v` and other replacement calls stay R's escape hatch.
        if !matches!(step, ReplacementStep::Other)
            && let Some(target) = self.replacement_step(lhs).map(|(target, _)| target)
            && self.refuse_hidden_representation(target, prior)
        {
            return prior;
        }
        // `names(f) <- v`, `f[, "w"] <- v` and `f[[i]] <- v` may rename, add or retype any
        // column of an R frame, so only a write to a named column keeps the columns known.
        if let TyKind::Frame(kind, Some(_)) = *prior.kind(self.db)
            && kind.class().is_some()
            && !matches!(step, ReplacementStep::Field(_))
        {
            return Ty::new(self.db, TyKind::Frame(kind, None));
        }
        if matches!(step, ReplacementStep::Names) {
            return self.write_names(value_expression, prior);
        }
        // Each member a path may hold takes the write, so `ret <- list()` that a loop may or
        // may not have filled still records `ret$count <- n` on both shapes.
        if let TyKind::Union(members) = prior.kind(self.db)
            && matches!(
                step,
                ReplacementStep::Field(_) | ReplacementStep::Element | ReplacementStep::Position(_)
            )
        {
            let written = members
                .iter()
                .map(|&member| {
                    self.write_replacement_step(lhs, step.clone(), value_expression, member, value)
                })
                .collect::<Vec<_>>();
            return union_of(self.db, written);
        }
        let module = self.module;
        let kind = module.kind(lhs);
        let field_name = match &step {
            ReplacementStep::Field(name) => Some(name.clone()),
            _ => None,
        };
        if let Some(field_name) = field_name {
            return match *prior.kind(self.db) {
                TyKind::Record(fields) => {
                    let mut fields = fields.to_vec();
                    match fields
                        .iter_mut()
                        .find(|field| field.name.text(self.db) == field_name)
                    {
                        Some(field) => field.ty = value,
                        None => fields.push(RecordField {
                            name: Name::new(self.db, field_name),
                            ty: value,
                            optional: false,
                        }),
                    }
                    Ty::new(self.db, TyKind::Record(&fields))
                }
                // R's `f$x <- NULL` drops the column, as `:=` does.
                TyKind::Frame(
                    kind @ (FrameKind::DataFrame | FrameKind::DataTable | FrameKind::RDataFrame),
                    Some(columns),
                ) => {
                    let mut columns = columns.to_vec();
                    let position = columns
                        .iter()
                        .position(|column| column.name.text(self.db) == field_name);
                    match (value.kind(self.db), position) {
                        (TyKind::Null, Some(position)) => {
                            columns.remove(position);
                        }
                        (TyKind::Null, None) => {}
                        (_, Some(position)) => {
                            if let Some(column) = columns.get_mut(position) {
                                column.ty = column_type(self.db, value);
                            }
                        }
                        (_, None) => columns.push(RecordField {
                            name: Name::new(self.db, field_name),
                            ty: column_type(self.db, value),
                            optional: false,
                        }),
                    }
                    Ty::new(self.db, TyKind::Frame(kind, Some(&columns)))
                }
                // `x <- NULL; x$a <- 1` and `list()` both grow into a one-field list.
                TyKind::Null => Ty::new(
                    self.db,
                    TyKind::Record(&[RecordField {
                        name: Name::new(self.db, field_name),
                        ty: value,
                        optional: false,
                    }]),
                ),
                TyKind::Tuple([]) => Ty::new(
                    self.db,
                    TyKind::Record(&[RecordField {
                        name: Name::new(self.db, field_name),
                        ty: value,
                        optional: false,
                    }]),
                ),
                // A ry record's fields and a typed environment's bindings are fixed, so the write
                // is checked against the field and the value keeps its type.
                TyKind::NativeRecord(fields) => {
                    let field = fields
                        .iter()
                        .find(|field| field.name.text(self.db) == field_name)
                        .map(|field| field.ty);
                    self.check_fixed_field_write(
                        lhs,
                        field_name,
                        field,
                        value_expression,
                        value,
                        prior,
                    );
                    prior
                }
                TyKind::Environment(fields) => {
                    let field = fields
                        .iter()
                        .find(|field| field.name.text(self.db) == field_name)
                        .map(|field| field.ty);
                    self.check_fixed_field_write(
                        lhs,
                        field_name,
                        field,
                        value_expression,
                        value,
                        prior,
                    );
                    prior
                }
                // A nominal's representation is fixed (that makes `@type` an invariant), so the
                // write is checked against it and the value keeps its nominal type.
                TyKind::Named(..) => {
                    let representation = self.structural(prior);
                    if let TyKind::NativeRecord(_) = representation.kind(self.db) {
                        self.write_replacement_step(
                            lhs,
                            ReplacementStep::Field(field_name),
                            value_expression,
                            representation,
                            value,
                        );
                    } else {
                        self.check_nominal_field_write(prior, &field_name, value_expression, value);
                    }
                    prior
                }
                // A list whose names are not tracked gains a field of the written type, so
                // `ret[[i]] <- part; ret$count <- n` leaves `ret` holding parts and the count.
                // Writing `NULL` removes a field and adds no type.
                TyKind::NamedList(element) | TyKind::List(element)
                    if !matches!(value.kind(self.db), TyKind::Null) =>
                {
                    Ty::new(
                        self.db,
                        TyKind::NamedList(union_of(self.db, [element, value])),
                    )
                }
                // `x[["b"]] <- 2.5` on an atomic vector writes an element, as `x[[2]]` does.
                _ if matches!(kind, ExpressionKind::Index { double: true, .. }) => {
                    atomic_write(self.db, prior, value).unwrap_or(prior)
                }
                _ => prior,
            };
        }
        if let (ReplacementStep::Position(position), TyKind::Tuple(items)) =
            (&step, prior.kind(self.db))
            && *position < items.len()
        {
            let mut items = TypeList::from_slice(items);
            items[*position] = value;
            return Ty::new(self.db, TyKind::Tuple(&items));
        }
        if matches!(step, ReplacementStep::Slice) {
            return atomic_write(self.db, prior, value).unwrap_or(prior);
        }
        if !matches!(
            step,
            ReplacementStep::Position(_) | ReplacementStep::Element
        ) {
            return prior;
        }
        if let Some(written) = atomic_write(self.db, prior, value) {
            return written;
        }
        // A ry list's item type is fixed, so the write is checked against it, and it is the use
        // that decides a literal: `j <- 1; lookup[[i]] <- j` makes `j` an `I64` in a `List<I64>`.
        if let Some(item) = array_element(self.db, prior) {
            if self.adapt_literal(value_expression, item) == LiteralFit::NotLiteral
                && !(self.open_literal_var(value) && self.table.unify(self.db, value, item).is_ok())
            {
                let value = self.table.resolve(self.db, value);
                if !matches!(value.kind(self.db), TyKind::Unknown | TyKind::Any)
                    && !self.table.compatible(self.db, value, item)
                {
                    let range = self.blame_range(value_expression);
                    let error = self.type_mismatch(range, item, value, Some(value_expression));
                    self.errors.push(error);
                }
            }
            return prior;
        }
        match prior.kind(self.db) {
            TyKind::Tuple([]) => Ty::new(self.db, TyKind::NamedList(value)),
            // `globals[[key]] <- value` under a computed key may add a field or replace any
            // one, so the record's fields are no longer known and a later `globals[[key]]` may
            // be `value` as much as an original field.
            TyKind::Record(fields) if matches!(step, ReplacementStep::Element) => {
                let members = fields
                    .iter()
                    .map(|field| field.ty)
                    .chain([value])
                    .collect::<Vec<_>>();
                Ty::new(self.db, TyKind::NamedList(union_of(self.db, members)))
            }
            TyKind::NamedList(element) => Ty::new(
                self.db,
                TyKind::NamedList(union_of(self.db, [*element, value])),
            ),
            TyKind::List(element) => {
                Ty::new(self.db, TyKind::List(union_of(self.db, [*element, value])))
            }
            _ => prior,
        }
    }

    /// `names(x) <- c("a", "b")` turns a list of two items into a record with those fields,
    /// and renames a record's fields. Names the code computes leave a list whose fields are
    /// not tracked, so `x$a` may read any item.
    fn write_names(&mut self, value_expression: ExprId, prior: Ty<'db>) -> Ty<'db> {
        let items: Vec<Ty<'db>> = match self.table.resolve(self.db, prior).kind(self.db) {
            TyKind::Tuple(items) => items.to_vec(),
            TyKind::Record(fields) => fields.iter().map(|field| field.ty).collect(),
            _ => return prior,
        };
        match literal_strings(self.module, value_expression) {
            Some(names) if names.len() == items.len() => {
                let fields: Vec<RecordField<'db>> = names
                    .into_iter()
                    .zip(items)
                    .map(|((name, _), ty)| RecordField {
                        name: Name::new(self.db, name),
                        ty,
                        optional: false,
                    })
                    .collect();
                Ty::new(self.db, TyKind::Record(&fields))
            }
            _ if items.is_empty() => prior,
            _ => Ty::new(self.db, TyKind::NamedList(union_of(self.db, items))),
        }
    }

    /// A diverging branch contributes neither its value nor its slot state, so the
    /// surviving edge's refinement persists after the `if` (the early-exit guard idiom).
    fn infer_if(
        &mut self,
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
        discarded: bool,
    ) -> Ty<'db> {
        self.expect_scalar_logical(condition, ConditionSite::IfOrWhile);
        let guard = self.recognize_guard(condition);
        let guard = self.correlated_edges(condition, guard);

        let mark = self.environment.mark();
        let pre_guard: Vec<(BindingId, EnvEntry<'db>, Option<EnvEntry<'db>>)> = guard
            .iter()
            .flat_map(|guard| &guard.true_edge)
            .filter_map(|refinement| match *refinement {
                SlotRefinement::Narrow(slot, ty) => {
                    Some((slot, EnvEntry::Mono(ty), self.environment.get(slot)))
                }
                SlotRefinement::NarrowScheme(slot, index) => {
                    Some((slot, EnvEntry::Scheme(index), self.environment.get(slot)))
                }
                SlotRefinement::Missing(_) | SlotRefinement::Establish(_) => None,
            })
            .collect();
        if let Some(guard) = &guard {
            self.apply_refinements(&guard.true_edge);
        }
        let pending_before = self.pending_reads();
        let then_ty = self.infer(then_branch);
        let then_pending = self.pending_reads() != pending_before;
        let then_diverges = self.diverges(then_branch);
        let mut then_writes = self.environment.writes_since(mark);
        // A refinement the branch never overwrote is the value it narrowed, not a new one.
        for (slot, entry) in &mut then_writes {
            if let Some(&(_, _, before)) = pre_guard.iter().find(|(narrowed, narrowed_entry, _)| {
                narrowed == slot && *entry == Some(*narrowed_entry)
            }) {
                *entry = before;
            }
        }
        self.environment.rollback(mark);
        // The then branch holds the pre-guard value of a slot only the false edge narrows, so
        // `if (!is.null(x)) ...` on an untyped `x` does not leave `x` as `NULL` afterwards.
        if let Some(guard) = &guard {
            for refinement in &guard.false_edge {
                let SlotRefinement::Narrow(slot, _) = *refinement else {
                    continue;
                };
                if then_writes.iter().all(|(written, _)| *written != slot) {
                    then_writes.push((slot, self.environment.get(slot)));
                }
            }
        }

        // The false edge applies to the else branch and, when the then branch diverges,
        // to everything after the `if`.
        let false_mark = self.environment.mark();
        if let Some(guard) = &guard {
            self.apply_refinements(&guard.false_edge);
        }
        let pending_before = self.pending_reads();
        let (else_ty, else_diverges) = match else_branch {
            Some(else_branch) => (self.infer(else_branch), self.diverges(else_branch)),
            None => (null(self.db), false),
        };
        let else_pending = self.pending_reads() != pending_before;

        // `if (exists("emptyenv")) emptyenv() else NULL` probes for a function the library
        // ships, so the else branch is a fallback for older R that never runs here.
        let else_diverges = else_diverges || self.probes_a_shipped_function(condition);
        let (then_ty, else_ty) = match else_branch {
            Some(else_branch) if !then_diverges && !else_diverges && !discarded => (
                self.adapt_literal_branch(then_branch, then_ty, else_ty),
                self.adapt_literal_branch(else_branch, else_ty, then_ty),
            ),
            _ => (then_ty, else_ty),
        };
        match (then_diverges, else_diverges) {
            (true, false) => else_ty,
            (false, true) => {
                self.environment.rollback(false_mark);
                for (slot, entry) in then_writes {
                    if let Some(entry) = entry {
                        self.environment.set(slot, entry);
                    }
                }
                then_ty
            }
            // A value nobody reads must not relate its branches: unifying them would tie
            // `nm(x, value)` to `attr(x, name) <- value` and so `x` to `value`.
            (false, false) if discarded => {
                self.retract_false_domain(condition);
                self.retract_false_facts(guard.as_ref());
                let correlations = self.branch_correlations(condition, &then_writes, false_mark);
                self.join_writes(&then_writes, JoinEdge::Branch);
                for fact in correlations {
                    self.environment.establish(fact);
                }
                let then_ty = self.table.resolve(self.db, then_ty);
                let else_ty = self.table.resolve(self.db, else_ty);
                union_of(self.db, [then_ty, else_ty])
            }
            (false, false) => {
                // `if (is.null(x)) x <- 1` normalizes an input the test inspected: afterwards
                // the slot holds the written type, and the formal's own variable stays open
                // instead of claiming every caller passes that type.
                let mut then_writes = then_writes;
                then_writes.retain(|&(slot, entry)| {
                    let (Some(EnvEntry::Mono(written)), Some(EnvEntry::Mono(before))) =
                        (entry, self.environment.get(slot))
                    else {
                        return true;
                    };
                    // A `NULL` write already joins by union without binding the variable.
                    let before = self.table.resolve(self.db, before);
                    let written_null = matches!(
                        self.table.resolve(self.db, written).kind(self.db),
                        TyKind::Null
                    );
                    let normalizes = !written_null
                        && self.is_open_parameter(before)
                        && self.reads_slot(condition, slot);
                    if normalizes {
                        self.environment.set(slot, EnvEntry::Mono(written));
                    }
                    !normalizes
                });
                self.retract_false_domain(condition);
                self.retract_false_facts(guard.as_ref());
                let correlations = self.branch_correlations(condition, &then_writes, false_mark);
                self.join_writes(&then_writes, JoinEdge::Branch);
                for fact in correlations {
                    self.environment.establish(fact);
                }
                // An `Unknown` branch may be anything, so the `if` may be too. The exception is
                // a branch that read a recursive group's export the fixpoint has not computed
                // yet: that `Unknown` is a placeholder, and joining it away is what lets
                // `if (n <= 1) 1L else n * fact(n - 1)` converge. A library call its untyped
                // arguments left undecided is `Unknown` whatever it read.
                let unknown_branch = |checker: &mut Self, branch: ExprId, ty: Ty<'db>, pending| {
                    let ty = checker.table.resolve(checker.db, ty);
                    matches!(ty.kind(checker.db), TyKind::Unknown)
                        && (!pending || checker.ends_in_undecided_call(branch))
                };
                let joined = if unknown_branch(self, then_branch, then_ty, then_pending)
                    || else_branch
                        .is_some_and(|branch| unknown_branch(self, branch, else_ty, else_pending))
                {
                    self.join_branch_values(then_ty, else_ty);
                    unknown(self.db)
                } else {
                    self.join_branch_values(then_ty, else_ty)
                };
                let mut range = self.module.range(then_branch);
                if let Some(else_branch) = else_branch {
                    range = range.cover(self.module.range(else_branch));
                }
                self.report_conflicting_tags(range, joined)
            }
            (true, true) => {
                self.environment.rollback(false_mark);
                self.join_branch_values(then_ty, else_ty)
            }
        }
    }

    /// Whether the condition is `exists("name")` for a function an attached package ships,
    /// whose `else` is a fallback for an R version ry never presents.
    fn probes_a_shipped_function(&self, condition: ExprId) -> bool {
        let ExpressionKind::Call { callee, arguments } = self.module.kind(condition) else {
            return false;
        };
        if variable_name(self.module, *callee).as_deref() != Some("exists")
            || self.naming.resolutions.contains_key(callee)
        {
            return false;
        }
        let [argument] = &self.module[arguments][..] else {
            return false;
        };
        let Some(value) = argument.value.filter(|_| argument.name.is_none()) else {
            return false;
        };
        let ExpressionKind::Literal(LiteralKind::String(text)) = self.module.kind(value) else {
            return false;
        };
        let probed = &self.module[text];
        self.globals.is_some_and(|globals| {
            Symbol::lookup(probed)
                .and_then(|probed| globals.library_candidates(probed))
                .is_some_and(|candidates| !candidates.is_empty())
                && !globals.defined_in_project(Symbol::new(probed), false)
        })
    }

    /// Each arm checks from the state before the `match`, and the arms that complete join
    /// like the branches of an `if`. Pattern bindings are ordinary writes to the frame.
    fn infer_match(&mut self, range: TextRange, scrutinee: ExprId, arms: &[MatchArm]) -> Ty<'db> {
        let mut scrutinee_ty = self.infer(scrutinee);
        // A pattern with a shape reads the representation; a name or `_` does not.
        let destructures = arms.iter().any(|arm| {
            !matches!(
                self.module.pattern(arm.pattern).kind,
                PatternKind::Wildcard | PatternKind::Bind(_) | PatternKind::Missing
            )
        });
        if destructures && self.refuse_hidden_representation(scrutinee, scrutinee_ty) {
            scrutinee_ty = unknown(self.db);
        }
        let resolved = self.table.shallow_resolve(self.db, scrutinee_ty);
        if matches!(resolved.kind(self.db), TyKind::Var(_))
            && let Some(shape) = self.scrutinee_shape(arms)
        {
            let snapshot = self.table.snapshot();
            if self.table.unify(self.db, scrutinee_ty, shape).is_err() {
                self.table.rollback(snapshot);
            }
        }
        let mut values = Vec::with_capacity(arms.len());
        let mut all_values = Vec::with_capacity(arms.len());
        let mut arm_writes = Vec::with_capacity(arms.len());
        for arm in arms {
            let mark = self.environment.mark();
            let mut bindings = Vec::new();
            self.check_pattern(arm.pattern, scrutinee_ty, &mut bindings);
            for (slot, ty) in bindings {
                self.environment.set(slot, EnvEntry::Mono(ty));
                self.note_captured_write(arm.body, slot, ty);
            }
            if let Some(guard) = arm.guard {
                self.expect_scalar_logical(guard, ConditionSite::Operand);
            }
            let value = self.infer(arm.body);
            all_values.push((arm.body, value));
            if !self.diverges(arm.body) {
                values.push((arm.body, value));
                arm_writes.push(self.environment.writes_since(mark));
            }
            self.environment.rollback(mark);
        }
        // An arm that leaves a slot alone keeps its state from before the `match`, so that
        // state joins too; with no arm completing, nothing after the `match` runs.
        let mut written: Vec<BindingId> = Vec::new();
        for writes in &arm_writes {
            for (slot, _) in writes {
                if !written.contains(slot) {
                    written.push(*slot);
                }
            }
        }
        let before: Vec<(BindingId, Option<EnvEntry<'db>>)> = written
            .iter()
            .map(|&slot| (slot, self.environment.get(slot)))
            .collect();
        for (index, writes) in arm_writes.iter().enumerate() {
            let complete: Vec<(BindingId, Option<EnvEntry<'db>>)> = before
                .iter()
                .map(|&(slot, entry)| {
                    writes
                        .iter()
                        .find(|(written, _)| *written == slot)
                        .map_or((slot, entry), |&write| write)
                })
                .collect();
            if index == 0 {
                for (slot, entry) in complete {
                    if let Some(entry) = entry {
                        self.environment.set(slot, entry);
                    }
                }
            } else {
                self.join_writes(&complete, JoinEdge::Branch);
            }
        }
        let mut joined_values = if values.is_empty() {
            all_values
        } else {
            values
        };
        for index in 0..joined_values.len() {
            let (body, value) = joined_values[index];
            let others: Vec<Ty<'db>> = joined_values
                .iter()
                .enumerate()
                .filter(|&(other, _)| other != index)
                .map(|(_, &(_, other))| other)
                .collect();
            for other in others {
                let adapted = self.adapt_literal_branch(body, value, other);
                if adapted != value {
                    joined_values[index].1 = adapted;
                    break;
                }
            }
        }
        let mut joined_values = joined_values.into_iter().map(|(_, value)| value);
        let Some(first) = joined_values.next() else {
            return unknown(self.db);
        };
        let joined = joined_values.fold(first, |joined, value| {
            self.join_branch_values(joined, value)
        });
        self.report_conflicting_tags(range, joined)
    }

    /// The type an unannotated scrutinee must have for its patterns to make sense, as
    /// `is.null(x)` shapes `x`: the arms' tags form a closed union, and tuple or record
    /// patterns give a tuple or record of fresh variables. A catch-all arm accepts a value of
    /// any type, so it leaves the scrutinee unshaped.
    fn scrutinee_shape(&mut self, arms: &[MatchArm]) -> Option<Ty<'db>> {
        let module = self.module;
        let mut tops = Vec::new();
        for arm in arms {
            collect_alternatives(module, arm.pattern, &mut tops);
        }
        let catch_all = arms.iter().any(|arm| {
            arm.guard.is_none()
                && matches!(
                    module.pattern(arm.pattern).kind,
                    PatternKind::Wildcard | PatternKind::Bind(_)
                )
        });
        if catch_all {
            return None;
        }
        let shaped: Vec<PatId> = tops
            .into_iter()
            .filter(|&pattern| {
                !matches!(
                    module.pattern(pattern).kind,
                    PatternKind::Wildcard | PatternKind::Bind(_) | PatternKind::Missing
                )
            })
            .collect();
        let first = *shaped.first()?;
        match &module.pattern(first).kind {
            PatternKind::Tag { .. } => {
                let mut members = Vec::new();
                for pattern in shaped {
                    let PatternKind::Tag { name, payload, .. } = &module.pattern(pattern).kind
                    else {
                        return None;
                    };
                    let payload = match payload {
                        Some(payload) => Some(self.pattern_shape(*payload)?),
                        None => None,
                    };
                    members.push(Ty::new(
                        self.db,
                        TyKind::Tag(Name::new(self.db, module.name(*name).as_str()), payload),
                    ));
                }
                let union = union_of(self.db, members);
                conflicting_tags(self.db, union).is_none().then_some(union)
            }
            PatternKind::Tuple(_) | PatternKind::Record { .. } if shaped.len() == 1 => {
                self.pattern_shape(first)
            }
            _ => None,
        }
    }

    /// Fresh variables in the shape of a tuple or record pattern; a record pattern with a
    /// rest names fields it cannot see, so it has no one shape.
    fn pattern_shape(&mut self, pattern: PatId) -> Option<Ty<'db>> {
        match &self.module.pattern(pattern).kind {
            PatternKind::Tuple(items) => {
                let items = self.module[items]
                    .iter()
                    .map(|_| self.fresh(Constraint::Unconstrained))
                    .collect::<TypeList<'db>>();
                Some(Ty::new(self.db, TyKind::NativeTuple(&items)))
            }
            PatternKind::Record { fields, rest: None } => {
                let fields = self.module[fields]
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name),
                        ty: self.fresh(Constraint::Unconstrained),
                    })
                    .collect::<Vec<_>>();
                Some(native_record(self.db, fields))
            }
            _ => None,
        }
    }

    /// Checks that `pattern` can match a value of `subject`'s type and collects the type
    /// of each name it binds. A name bound by both sides of `|` gets the union.
    fn check_pattern(
        &mut self,
        pattern: PatId,
        subject: Ty<'db>,
        bindings: &mut Vec<(BindingId, Ty<'db>)>,
    ) {
        let module = self.module;
        let node = module.pattern(pattern);
        // A name binds the value as it is, so a payload typed `Tree<T>` stays a `Tree<T>`;
        // only a pattern that looks inside needs the representation.
        if let PatternKind::Wildcard | PatternKind::Bind(_) | PatternKind::Missing = node.kind {
            return self.check_pattern_components(pattern, subject, bindings);
        }
        let subject = self.structural(subject);
        if let TyKind::Union(members) = subject.kind(self.db)
            && !matches!(
                node.kind,
                PatternKind::Wildcard
                    | PatternKind::Bind(_)
                    | PatternKind::Missing
                    | PatternKind::Or(_)
            )
        {
            // A union narrows to the members the pattern can match.
            let mut members: Vec<Ty<'db>> = members
                .iter()
                .copied()
                .filter(|&member| self.pattern_admits(pattern, member))
                .collect();
            // A tag the union lists is never among the tags its `..` stands for.
            if members
                .iter()
                .any(|member| matches!(member.kind(self.db), TyKind::Tag(..)))
            {
                members.retain(|member| !matches!(member.kind(self.db), TyKind::OtherTags));
            }
            if members.is_empty() {
                let (hint, blamed) = self.pattern_mismatch_hint(pattern, subject).unzip();
                self.errors.push(TypeError {
                    range: blamed.flatten().unwrap_or(node.range),
                    kind: TypeErrorKind::PatternNeverMatches {
                        found: subject,
                        hint,
                    },
                });
                self.bind_unknown(pattern, bindings);
                return;
            }
            let narrowed = union_of(self.db, members);
            if !matches!(narrowed.kind(self.db), TyKind::Union(_)) {
                return self.check_pattern(pattern, narrowed, bindings);
            }
            // Several members fit (records with a rest, numbers of two kinds): the
            // components bind to what is known, positionally or by name.
            return self.check_pattern_components(pattern, narrowed, bindings);
        }
        if !self.pattern_admits(pattern, subject) {
            let (hint, blamed) = self.pattern_mismatch_hint(pattern, subject).unzip();
            self.errors.push(TypeError {
                range: blamed.flatten().unwrap_or(node.range),
                kind: TypeErrorKind::PatternNeverMatches {
                    found: subject,
                    hint,
                },
            });
            self.bind_unknown(pattern, bindings);
            return;
        }
        self.check_pattern_components(pattern, subject, bindings);
    }

    /// The hint, and the part of the pattern it is about when that is narrower than the whole
    /// pattern: a misspelled tag or field name.
    fn pattern_mismatch_hint(
        &self,
        pattern: PatId,
        subject: Ty<'db>,
    ) -> Option<(String, Option<TextRange>)> {
        let PatternKind::Tag { name, payload } = &self.module.pattern(pattern).kind else {
            return None;
        };
        let NameSite {
            name,
            range: name_range,
        } = self.module[name];
        let subject = self.table.resolve(self.db, subject);
        let single = [subject];
        let members: &[Ty<'db>] = match subject.kind(self.db) {
            TyKind::Union(members) => members,
            _ => &single,
        };
        let tags: Vec<(&str, Option<Ty<'db>>)> = members
            .iter()
            .filter_map(|member| match member.kind(self.db) {
                TyKind::Tag(tag, payload) => Some((tag.text(self.db), *payload)),
                _ => None,
            })
            .collect();
        let found = tags.iter().find(|(tag, _)| *tag == name.as_str());
        if let Some((_, None)) = found {
            return Some((format!("`:{}` carries no payload", name.as_str()), None));
        }
        let Some((_, Some(tag_payload))) = found else {
            let nearest = nearest_name(name.as_str(), tags.iter().map(|(tag, _)| *tag))?;
            let tag_range = name_range
                .start()
                .checked_sub(TextSize::from(1))
                .map(|start| TextRange::new(start, name_range.end()));
            return Some((
                format!(
                    "the value has no tag `:{}`. Did you mean `:{nearest}`?",
                    name.as_str()
                ),
                tag_range,
            ));
        };
        let payload = (*payload)?;
        let PatternKind::Record { fields, .. } = &self.module.pattern(payload).kind else {
            return None;
        };
        let TyKind::NativeRecord(native) = tag_payload.kind(self.db) else {
            return None;
        };
        let known: Vec<&str> = native
            .iter()
            .map(|field| field.name.text(self.db))
            .collect();
        let unknown = self.module[fields]
            .iter()
            .find(|field| !known.contains(&field.name.as_str()))?;
        let suggestion = nearest_name(unknown.name.as_str(), known.iter().copied())
            .map(|nearest| format!(". Did you mean `{nearest}`?"))
            .unwrap_or_default();
        Some((
            format!(
                "`:{}` has no field `{}`{suggestion}",
                name.as_str(),
                unknown.name.as_str()
            ),
            Some(unknown.name_range),
        ))
    }

    fn check_pattern_components(
        &mut self,
        pattern: PatId,
        subject: Ty<'db>,
        bindings: &mut Vec<(BindingId, Ty<'db>)>,
    ) {
        let module = self.module;
        let node = module.pattern(pattern);
        self.recorded_patterns.insert(pattern, subject);
        match &node.kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Literal { .. } => {}
            PatternKind::Bind(leaf) => self.bind_leaf(*leaf, subject, bindings),
            PatternKind::List { items, rest } => {
                let (items, rest) = (*items, *rest);
                let element = self.component_union(subject, array_element);
                for &item in module[items].iter() {
                    self.check_pattern(item, element, bindings);
                }
                if let Some(binding) = rest.and_then(|rest| rest.rest.binding) {
                    self.bind_leaf(binding, array(self.db, element), bindings);
                }
            }
            PatternKind::As {
                pattern: inner,
                binding,
            } => {
                let (inner, binding) = (*inner, *binding);
                self.check_pattern(inner, subject, bindings);
                self.bind_leaf(binding, subject, bindings);
            }
            PatternKind::Or(alternatives) => {
                for &alternative in module[alternatives].iter() {
                    self.check_pattern(alternative, subject, bindings);
                }
            }
            PatternKind::Tag { payload, .. } => {
                let Some(payload) = payload.filter(|&payload| !module.is_any_payload(payload))
                else {
                    return;
                };
                let payload_ty =
                    self.component_union(subject, |db, member| match member.kind(db) {
                        TyKind::Tag(_, Some(payload)) => Some(*payload),
                        _ => None,
                    });
                self.check_pattern(payload, payload_ty, bindings);
            }
            PatternKind::Tuple(items) => {
                for (position, &item) in module[items].iter().enumerate() {
                    let item_ty =
                        self.component_union(subject, |db, member| match member.kind(db) {
                            TyKind::NativeTuple(types) => types.get(position).copied(),
                            _ => None,
                        });
                    self.check_pattern(item, item_ty, bindings);
                }
            }
            PatternKind::Record {
                rest: Some(Rest {
                    binding: Some(_), ..
                }),
                ..
            } => self.bind_unknown(pattern, bindings),
            PatternKind::Record { fields, .. } => {
                for field in module[fields].iter() {
                    let name = field.name;
                    let field_ty =
                        self.component_union(subject, |db, member| match member.kind(db) {
                            TyKind::NativeRecord(native) => native
                                .iter()
                                .find(|native| native.name.text(db) == name.as_str())
                                .map(|native| native.ty),
                            _ => None,
                        });
                    self.check_pattern(field.pattern, field_ty, bindings);
                }
            }
        }
    }

    /// The union of one component over a subject's members; a member that does not say,
    /// such as `Unknown` or the `..` of an open union, makes the component `Unknown`.
    fn component_union(
        &self,
        subject: Ty<'db>,
        component: impl Fn(&'db dyn Db, Ty<'db>) -> Option<Ty<'db>>,
    ) -> Ty<'db> {
        let single = [subject];
        let members: &[Ty<'db>] = match subject.kind(self.db) {
            TyKind::Union(members) => members,
            _ => &single,
        };
        let mut found = Vec::with_capacity(members.len());
        for &member in members {
            match component(self.db, member) {
                Some(ty) => found.push(ty),
                None => return unknown(self.db),
            }
        }
        union_of(self.db, found)
    }

    /// Whether a value of `subject`'s type, which is not a union, could match `pattern`
    /// at its top level. Components are checked separately, so a mismatch is reported
    /// where it is.
    fn pattern_admits(&mut self, pattern: PatId, subject: Ty<'db>) -> bool {
        let module = self.module;
        let subject = self.table.resolve(self.db, subject);
        if matches!(
            subject.kind(self.db),
            TyKind::Unknown | TyKind::Any | TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Named(..)
        ) {
            return true;
        }
        if let TyKind::Union(members) = subject.kind(self.db) {
            return members
                .iter()
                .any(|&member| self.pattern_admits(pattern, member));
        }
        match &module.pattern(pattern).kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Bind(_) => true,
            PatternKind::Or(alternatives) => {
                let alternatives = *alternatives;
                module[alternatives]
                    .iter()
                    .any(|&alternative| self.pattern_admits(alternative, subject))
            }
            PatternKind::Literal { literal, .. } => {
                let literal_ty = self.literal_ty(literal);
                literal_overlaps(self.db, literal, literal_ty, subject)
            }
            PatternKind::Tag { name, payload, .. } => match subject.kind(self.db) {
                TyKind::OtherTags => true,
                TyKind::Tag(subject_name, subject_payload) => {
                    subject_name.text(self.db) == self.module.name(*name).as_str()
                        && match (payload, subject_payload) {
                            (Some(payload), _) if self.module.is_any_payload(*payload) => true,
                            (None, None) => true,
                            (Some(payload), Some(subject_payload)) => {
                                self.pattern_admits(*payload, *subject_payload)
                            }
                            _ => false,
                        }
                }
                _ => false,
            },
            PatternKind::Tuple(items) => {
                matches!(subject.kind(self.db), TyKind::NativeTuple(types) if types.len() == items.len())
            }
            PatternKind::Record { fields, rest } => match subject.kind(self.db) {
                TyKind::NativeRecord(native) => {
                    (rest.is_some() || native.len() == fields.len())
                        && module[fields].iter().all(|field| {
                            native
                                .iter()
                                .any(|native| native.name.text(self.db) == field.name.as_str())
                        })
                }
                _ => false,
            },
            PatternKind::List { .. } => array_element(self.db, subject).is_some(),
            PatternKind::As { pattern: inner, .. } => {
                let inner = *inner;
                self.pattern_admits(inner, subject)
            }
        }
    }

    /// Binds the name a pattern writes, joining the types when an or-pattern binds it twice.
    fn bind_leaf(&mut self, leaf: ExprId, ty: Ty<'db>, bindings: &mut Vec<(BindingId, Ty<'db>)>) {
        self.recorded.insert(leaf, ty);
        if let Some(&slot) = self.naming.resolutions.get(&leaf) {
            match bindings.iter_mut().find(|(bound, _)| *bound == slot) {
                Some((_, bound)) => *bound = union_of(self.db, [*bound, ty]),
                None => bindings.push((slot, ty)),
            }
        }
    }

    fn bind_unknown(&mut self, pattern: PatId, bindings: &mut Vec<(BindingId, Ty<'db>)>) {
        let unknown = unknown(self.db);
        for leaf in self.module.pattern_bindings(pattern) {
            self.recorded.insert(leaf, unknown);
            if let Some(&slot) = self.naming.resolutions.get(&leaf)
                && !bindings.iter().any(|(bound, _)| *bound == slot)
            {
                bindings.push((slot, unknown));
            }
        }
    }

    /// The items share one type exactly, with no coercion between them, except that tags join
    /// into one tag union as they do across branches. An empty literal takes its item type from
    /// its first use.
    fn infer_array_literal(&mut self, items: &[ExprId]) -> Ty<'db> {
        let mut element: Option<Ty<'db>> = None;
        for &item in items {
            let found = match self.list_item_literal(item) {
                Some(literal) => literal,
                None => self.infer_list_item(item),
            };
            let range = self.blame_range(item);
            if matches!(
                self.module.kind(item),
                ExpressionKind::Literal(LiteralKind::Na(_))
            ) {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::ArrayMissingValue,
                });
                continue;
            }
            if !self.array_item_allowed(found) {
                let found = self.table.resolve(self.db, found);
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::ArrayItemType { found },
                });
                continue;
            }
            let Some(first) = element else {
                element = Some(found);
                continue;
            };
            let snapshot = self.table.snapshot();
            if self.table.unify(self.db, first, found).is_ok() {
                continue;
            }
            self.table.rollback(snapshot);
            let first = self.settle_literals(first);
            let found = self.settle_literals(found);
            if is_tag_type(self.db, first) && is_tag_type(self.db, found) {
                let joined = union_of(self.db, [first, found]);
                element = Some(self.report_conflicting_tags(range, joined));
                continue;
            }
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::ArrayItemsDiffer { first, found },
            });
        }
        let element = match element {
            Some(element) => element,
            None => self.fresh(Constraint::ArrayItem),
        };
        array(self.db, element)
    }

    /// A `List` item that is not a lone literal. One written with literals alone, such as
    /// `if (c) 0.1 else 3.0` or `1 + 2`, is a ry value like a lone literal item, taking the ry
    /// scalar its literals are written as.
    fn infer_list_item(&mut self, item: ExprId) -> Ty<'db> {
        let ty = self.infer(item);
        let Some(scalar) = literal_value_scalar(self.module, item) else {
            return ty;
        };
        let resolved = self.table.resolve(self.db, ty);
        if holds_ry_scalar(self.db, resolved) {
            return ty;
        }
        let target = native_scalar(self.db, scalar);
        match self.adapt_literal(item, target) {
            LiteralFit::Fits => target,
            LiteralFit::OutOfRange | LiteralFit::NotLiteral => ty,
        }
    }

    /// A literal written as a `List` item is a ry value: a number literal is a ry number its use
    /// decides (`I64` or `F64` when nothing does), a string `Str` and `TRUE` or `FALSE` `Bool`.
    fn list_item_literal(&mut self, item: ExprId) -> Option<Ty<'db>> {
        let module = self.module;
        let mut inner = item;
        while let ExpressionKind::Paren(paren) = module.kind(inner) {
            inner = *paren;
        }
        let ty = if let Some(literal) = number_literal(module, inner) {
            let constraint = if literal.integer {
                Constraint::Int
            } else if literal.default_native() == NativeScalar::F64 {
                Constraint::Frac
            } else {
                Constraint::Number
            };
            self.fresh_literal(constraint, true)
        } else {
            match module.kind(inner) {
                ExpressionKind::Literal(LiteralKind::String(_)) => {
                    native_scalar(self.db, NativeScalar::Str)
                }
                ExpressionKind::Literal(LiteralKind::Logical(_)) => {
                    native_scalar(self.db, NativeScalar::Bool)
                }
                _ => return None,
            }
        };
        self.record_literal(inner, ty);
        if inner != item {
            self.record(item, ty);
        }
        Some(ty)
    }

    /// A tag's payload, a tuple or a record whose own items are inferred as written.
    fn infer_payload(&mut self, payload: ExprId) -> Ty<'db> {
        let module = self.module;
        let ty = match module.kind(payload) {
            ExpressionKind::Tuple(items) => {
                let items = module[*items]
                    .iter()
                    .map(|&item| self.infer(item))
                    .collect::<TypeList<'db>>();
                Ty::new(self.db, TyKind::NativeTuple(&items))
            }
            ExpressionKind::Record(fields) => {
                let fields = module[*fields]
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name),
                        ty: self.infer(field.value),
                    })
                    .collect::<Vec<_>>();
                native_record(self.db, fields)
            }
            _ => return self.infer(payload),
        };
        self.record(payload, ty)
    }

    /// A tuple item or a record field. A literal there is open, as a literal bound
    /// to a variable is: a later use decides it (`Str.concat(s, pair[[1]])` makes `"a"` in
    /// `pair <- ("a", 1)` a `Str`), and otherwise it has R's type.
    fn component(&mut self, value: ExprId) -> Ty<'db> {
        let module = self.module;
        let mut inner = value;
        while let ExpressionKind::Paren(paren) = module.kind(inner) {
            inner = *paren;
        }
        let ty = if let Some(literal) = number_literal(module, inner) {
            let constraint = if literal.is_fraction() {
                Constraint::FracLiteral
            } else {
                Constraint::WholeLiteral {
                    integer: literal.integer,
                }
            };
            let ty = self.fresh_literal(constraint, false);
            self.record_literal(inner, ty);
            ty
        } else {
            let constraint = match module.kind(inner) {
                ExpressionKind::Literal(LiteralKind::String(_)) => Constraint::StrLiteral,
                ExpressionKind::Literal(LiteralKind::Logical(_)) => Constraint::BoolLiteral,
                _ => return self.infer(value),
            };
            let ty = self.fresh_literal(constraint, false);
            self.record(inner, ty)
        };
        if inner != value {
            self.record(value, ty);
        }
        ty
    }

    /// A `List` holds ry values only. An R value would bring `NA`, attributes and recycling
    /// into a type that has none, so it crosses into a `List` through `List.from_r`.
    fn array_item_allowed(&mut self, ty: Ty<'db>) -> bool {
        let structural = self.structural(ty);
        match structural.kind(self.db) {
            TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => true,
            _ => is_ry_value_type(self.db, structural),
        }
    }

    /// A joined value holding one tag in two payload shapes is not a type a `match` could
    /// take apart, so the join is reported and the value becomes `Unknown`.
    fn report_conflicting_tags(&mut self, range: TextRange, joined: Ty<'db>) -> Ty<'db> {
        let resolved = self.table.resolve(self.db, joined);
        match conflicting_tags(self.db, resolved) {
            Some((first, second)) => {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::ConflictingTags { first, second },
                });
                unknown(self.db)
            }
            None => joined,
        }
    }

    /// `try x`: the payload of `x`'s `:Ok` members is the value, and its `:Err` members join the
    /// enclosing function's return type, as `return(:Err(e))` would.
    fn infer_try(&mut self, range: TextRange, operand: ExprId) -> Ty<'db> {
        let operand_range = self.blame_range(operand);
        let inferred = self.infer(operand);
        let resolved = self.settle_literals(inferred);
        let single = [resolved];
        let members: &[Ty<'db>] = match resolved.kind(self.db) {
            TyKind::Union(members) => members,
            TyKind::Unknown | TyKind::Any => return resolved,
            _ => &single,
        };
        let mut oks = Vec::new();
        let mut errs = Vec::new();
        for &member in members {
            match member.kind(self.db) {
                TyKind::Tag(name, Some(payload)) if name.text(self.db) == "Ok" => {
                    match payload.kind(self.db) {
                        TyKind::NativeTuple(items) if items.len() == 1 => oks.push(items[0]),
                        _ => oks.push(*payload),
                    }
                }
                TyKind::Tag(name, Some(_)) if name.text(self.db) == "Err" => errs.push(member),
                _ => {
                    self.errors.push(TypeError {
                        range: operand_range,
                        kind: TypeErrorKind::TryNotResult { found: resolved },
                    });
                    return unknown(self.db);
                }
            }
        }
        if oks.is_empty() {
            self.errors.push(TypeError {
                range: operand_range,
                kind: TypeErrorKind::TryNotResult { found: resolved },
            });
            return unknown(self.db);
        }
        match self.return_frames.last_mut() {
            Some(frame) => {
                if !errs.is_empty() {
                    frame.push(EarlyReturn {
                        ty: union_of(self.db, errs),
                        range: operand_range,
                        value: None,
                    });
                }
            }
            None => self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::TryOutsideFunction,
            }),
        }
        union_of(self.db, oks)
    }

    /// `(..base, x = v)`: a copy of the record `base` with the listed fields replaced. Each field
    /// must exist and keep its type, so the copy has `base`'s type.
    fn infer_record_update(&mut self, base: ExprId, fields: &[HirRecordField]) -> Ty<'db> {
        let base_range = self.blame_range(base);
        let inferred = self.infer(base);
        let resolved = self.table.resolve(self.db, inferred);
        let native = match resolved.kind(self.db) {
            TyKind::NativeRecord(native) => Some(*native),
            TyKind::Unknown | TyKind::Any | TyKind::Var(_) => None,
            _ => {
                for field in fields {
                    self.infer(field.value);
                }
                self.errors.push(TypeError {
                    range: base_range,
                    kind: TypeErrorKind::RecordUpdateBase { found: resolved },
                });
                return unknown(self.db);
            }
        };
        for field in fields {
            let value_range = self.blame_range(field.value);
            let value = self.infer(field.value);
            let Some(native) = &native else { continue };
            let name = field.name.as_str();
            match native
                .iter()
                .find(|native| native.name.text(self.db) == name)
            {
                Some(existing) => {
                    let fits = match self.adapt_literal(field.value, existing.ty) {
                        LiteralFit::Fits | LiteralFit::OutOfRange => true,
                        LiteralFit::NotLiteral => {
                            matches!(value.kind(self.db), TyKind::Unknown)
                                || self.table.compatible(self.db, value, existing.ty)
                        }
                    };
                    if !fits {
                        let error =
                            self.type_mismatch(value_range, existing.ty, value, Some(field.value));
                        self.errors.push(error);
                    }
                }
                None => self.errors.push(TypeError {
                    range: field.name_range,
                    kind: TypeErrorKind::FieldDoesNotExist {
                        suggestion: nearest_name(
                            name,
                            native.iter().map(|native| native.name.text(self.db)),
                        ),
                        field: name.to_owned(),
                        container: resolved,
                    },
                }),
            }
        }
        resolved
    }

    fn infer_for(
        &mut self,
        loop_expression: ExprId,
        variable_range: Option<TextRange>,
        sequence: ExprId,
        body: ExprId,
    ) -> Ty<'db> {
        let sequence_range = self.blame_range(sequence);
        let inferred = self.infer(sequence);
        let resolved = self.structural(inferred);
        let element = if self.refuse_hidden_representation(sequence, inferred) {
            unknown(self.db)
        } else {
            match self.iteration_element(resolved) {
                Ok(element) => element,
                Err(found) => {
                    self.errors.push(TypeError {
                        range: sequence_range,
                        kind: TypeErrorKind::NotIterable { found },
                    });
                    unknown(self.db)
                }
            }
        };
        if variable_range.is_some() {
            self.recorded_loop_variables
                .insert(loop_expression, element);
        }
        let loop_slot = self
            .naming
            .extras()
            .for_variables
            .get(&loop_expression)
            .copied();
        let variable_written = self.check_loop_body(
            loop_expression,
            None,
            body,
            false,
            loop_slot.map(|slot| (slot, element)),
        );
        // R leaves the variable at the last element, or sets it to `NULL` when the sequence
        // is empty, whatever it held before the loop. A `from:to` range always has an element.
        // A body that assigns the variable leaves its state at the end of the body instead.
        if let Some(slot) = loop_slot {
            let mut range = sequence;
            while let ExpressionKind::Paren(inner) = self.module.kind(range) {
                range = *inner;
            }
            let never_empty = matches!(
                self.module.kind(range),
                ExpressionKind::Binary {
                    operator: BinaryOperator::Sequence,
                    ..
                }
            );
            let last =
                variable_written.map_or(element, |written| union_of(self.db, [element, written]));
            let after = match never_empty {
                true => last,
                false => union_of(self.db, [last, null(self.db)]),
            };
            self.environment.set(slot, EnvEntry::Mono(after));
        }
        null(self.db)
    }

    /// A `repeat` leaves only through its `break`s, so the state after it is the union of the
    /// states they saw: each `break`'s writes over the converged loop-head state.
    fn leave_through_breaks(&mut self, breaks: &[Vec<(BindingId, Option<EnvEntry<'db>>)>]) {
        let mut slots: Vec<BindingId> = breaks.iter().flatten().map(|(slot, _)| *slot).collect();
        slots.sort_unstable();
        slots.dedup();
        let head: Vec<_> = slots
            .iter()
            .map(|&slot| (slot, self.environment.get(slot)))
            .collect();
        for (index, taken) in breaks.iter().enumerate() {
            if index > 0 {
                let state: Vec<_> = head
                    .iter()
                    .map(|&(slot, at_head)| {
                        let written = taken.iter().find(|(written, _)| *written == slot);
                        (slot, written.map_or(at_head, |(_, entry)| *entry))
                    })
                    .collect();
                self.join_writes(&state, JoinEdge::Branch);
                continue;
            }
            // A slot the first `break` did not write already holds its head state.
            for (slot, entry) in taken {
                if let Some(entry) = *entry {
                    self.environment.set(*slot, entry);
                }
            }
        }
    }

    fn iteration_element(&mut self, resolved: Ty<'db>) -> Result<Ty<'db>, Ty<'db>> {
        match resolved.kind(self.db) {
            TyKind::Union(members) => {
                // A `NULL` or `list()` member runs the body zero times, so it adds no element:
                // `for (f in if (a) list(g))` binds `f` to `g`'s type, never `NULL`.
                let mut elements = Vec::with_capacity(members.len());
                for &member in members.iter() {
                    let element = self.iteration_element(member).map_err(|_| resolved)?;
                    if !matches!(member.kind(self.db), TyKind::Null)
                        && !is_empty_list(self.db, member)
                    {
                        elements.push(element);
                    }
                }
                Ok(union_of(self.db, elements))
            }
            TyKind::Scalar(atomic) => Ok(scalar(self.db, *atomic)),
            TyKind::Vector(element)
            | TyKind::NamedVector(element)
            | TyKind::List(element)
            | TyKind::NamedList(element) => Ok(*element),
            TyKind::Tuple(items) => Ok(union_of(self.db, items.iter().copied())),
            TyKind::Record(fields) => Ok(union_of(self.db, fields.iter().map(|field| field.ty))),
            // `NULL` iterates zero times (legal R).
            TyKind::Null => Ok(null(self.db)),
            TyKind::Any => Ok(any(self.db)),
            // An already-failed source does not produce a second error.
            TyKind::Unknown => Ok(unknown(self.db)),
            TyKind::Named(..) => {
                Ok(array_element(self.db, resolved).unwrap_or_else(|| any(self.db)))
            }
            // Iteration commits neither vector nor list, so a variable stays unconstrained.
            TyKind::Var(_) | TyKind::Rigid(_) => Ok(unknown(self.db)),
            _ => Err(resolved),
        }
    }

    /// Iterates to a fixed point from the join of the pre-loop state and the previous
    /// pass's exit writes; diagnostics count only on the final pass, and a slot still
    /// changing at the cap widens to `Unknown`. Returns the loop variable's type at the end
    /// of the body when the body assigns it.
    fn check_loop_body(
        &mut self,
        loop_expression: ExprId,
        condition: Option<ExprId>,
        body: ExprId,
        leaves_only_by_break: bool,
        loop_variable: Option<(BindingId, Ty<'db>)>,
    ) -> Option<Ty<'db>> {
        const LOOP_PASS_CAP: usize = 3;
        let mut passes = 0;
        loop {
            passes += 1;
            let errors_mark = self.errors.len();
            let origins_mark = self.strict_origins.len();
            let mark = self.environment.mark();
            if let Some((slot, element)) = loop_variable {
                self.environment.set(slot, EnvEntry::Mono(element));
            }
            if let Some(condition) = condition {
                self.expect_scalar_logical(condition, ConditionSite::IfOrWhile);
            }
            self.loop_jumps.push(LoopJumps {
                mark,
                ..LoopJumps::default()
            });
            self.infer(body);
            let mut writes = self.environment.writes_since(mark);
            let jumps = self.loop_jumps.pop().unwrap_or_default();
            // The loop variable's own writes are what the body leaves it holding after the loop.
            let mut variable_written: Option<Ty<'db>> = None;
            let mut take_variable = |writes: &mut Vec<(BindingId, Option<EnvEntry<'db>>)>| {
                let Some((loop_slot, _)) = loop_variable else {
                    return;
                };
                writes.retain(|(slot, entry)| {
                    if *slot != loop_slot {
                        return true;
                    }
                    let ty = match entry {
                        Some(EnvEntry::Mono(ty) | EnvEntry::MissingFormal(ty)) => *ty,
                        Some(EnvEntry::Scheme(_)) | None => unknown(self.db),
                    };
                    variable_written = Some(match variable_written {
                        Some(earlier) => union_of(self.db, [earlier, ty]),
                        None => ty,
                    });
                    false
                });
            };
            let (mut breaks, mut nexts) = (jumps.breaks, jumps.nexts);
            take_variable(&mut writes);
            take_variable(&mut nexts);
            breaks.iter_mut().for_each(&mut take_variable);
            self.environment.rollback(mark);
            // A read this pass discovered the slot's pre-loop state through the cross-item
            // binding; establish it so the body's writes join into it.
            for (slot, &ty) in self.pre_materialized.iter() {
                if self.environment.get(slot).is_none() {
                    self.environment.set(slot, EnvEntry::Mono(ty));
                }
            }
            let mut changed = self.join_writes(&writes, JoinEdge::LoopBack);
            changed.extend(self.join_writes(&nexts, JoinEdge::LoopBack));
            if changed.is_empty() {
                if leaves_only_by_break {
                    self.leave_through_breaks(&breaks);
                } else {
                    // A `break` leaves with what it saw, beside the state the loop exits with
                    // at its head.
                    for taken in &breaks {
                        self.join_writes(taken, JoinEdge::Branch);
                    }
                }
                return variable_written;
            }
            if passes >= LOOP_PASS_CAP {
                for slot in changed {
                    self.environment.set(slot, EnvEntry::Mono(unknown(self.db)));
                    if let Some(name) = self.naming.bindings.get(&slot).map(|info| info.name) {
                        self.record_strict_origin(
                            loop_expression,
                            StrictOriginKind::LoopWidened(name.to_string()),
                        );
                    }
                }
                // The pass that wrote the variable never converged either.
                return variable_written.map(|_| unknown(self.db));
            }
            // Not yet stable: this pass ran under a stale entry state, so its diagnostics are
            // discarded.
            self.truncate_errors(errors_mark);
            self.strict_origins.truncate(origins_mark);
        }
    }

    /// The names whose runtime type the body inspects (`typeof(x)`, `class(x)`,
    /// `is.character(x)`, `inherits(x, ...)`, rlang's `is_string(x)`). Such a value is dynamically typed by
    /// design: each branch uses it as a different type, so no one type describes it.
    /// `is.null` and `is.na` test a value, not its type, and keep their narrowing.
    fn collect_type_probed(&self, id: ExprId, probed: &mut FxHashSet<Symbol>) {
        let kind = self.module.kind(id);
        if matches!(kind, ExpressionKind::Function { .. }) {
            return;
        }
        if let ExpressionKind::Call { callee, arguments } = kind
            && let ExpressionKind::NameRef(name) = self.module.kind(*callee)
            && ((!self.naming.resolutions.contains_key(callee) && is_type_probe(name.as_str()))
                || is_tidy_type_predicate(name.as_str()))
            && let Some(first) = self.module[arguments].first()
            && first.name.is_none()
            && let Some(value) = first.value
            && let ExpressionKind::NameRef(probed_name) = self.module.kind(value)
        {
            probed.insert(*probed_name);
        }
        let module = self.module;
        module.for_each_child(id, |child| self.collect_type_probed(child, probed));
    }

    /// Walks in evaluation order, so a read counts as forcing a formal only before the body
    /// first assigns that name. A nested function's `missing()` tests and writes cover its
    /// own frame, and its reads of an outer formal force it only where the body runs: an
    /// inline call, a call through the local the literal is bound to, or the function's
    /// result, since a factory's closure is made to be called. A callback handed to `lapply`
    /// may run zero times, so passing a closure to a call forces nothing.
    fn formal_uses(&self, parameters: &[Formal], body: ExprId) -> FormalUses {
        let mut results = Vec::new();
        self.result_expressions(body, &mut results);
        let mut uses = FormalUses {
            tracked: parameters.iter().map(|parameter| parameter.name).collect(),
            results,
            ..FormalUses::default()
        };
        self.collect_formal_uses(body, &mut uses);
        uses
    }

    fn collect_formal_uses(&self, id: ExprId, uses: &mut FormalUses) {
        let kind = self.module.kind(id);
        match kind {
            ExpressionKind::Function { .. } => {
                let forced = self.closure_forced(id, uses);
                if uses.results.contains(&id) {
                    uses.record_closure_run(&forced);
                }
                return;
            }
            ExpressionKind::NameRef(name) => {
                uses.record_read(*name);
                if uses.results.contains(&id)
                    && let Some(forced) = uses.closures.get(name).cloned()
                {
                    uses.record_closure_run(&forced);
                }
                return;
            }
            // `<<-` writes an enclosing frame, never the local formal, and reads its target
            // there too.
            ExpressionKind::Assign {
                spelling: AssignSpelling::Super,
                value,
                ..
            } => {
                self.collect_formal_uses(*value, uses);
                return;
            }
            ExpressionKind::Assign {
                spelling: AssignSpelling::Destructure,
                target,
                value,
            } => {
                self.collect_formal_uses(*value, uses);
                if let ExpressionKind::Call { arguments, .. } = self.module.kind(*target) {
                    for value in self.module[arguments]
                        .iter()
                        .filter_map(|argument| argument.value)
                    {
                        if let ExpressionKind::NameRef(name) = self.module.kind(value) {
                            uses.record_write(*name);
                        }
                    }
                }
                return;
            }
            ExpressionKind::Assign { target, value, .. }
                if let ExpressionKind::NameRef(name) = self.module.kind(*target) =>
            {
                if matches!(self.module.kind(*value), ExpressionKind::Function { .. }) {
                    let forced = self.closure_forced(*value, uses);
                    if uses.results.contains(value) {
                        uses.record_closure_run(&forced);
                    }
                    uses.record_write(*name);
                    uses.closures.insert(*name, forced);
                } else {
                    self.collect_formal_uses(*value, uses);
                    uses.record_write(*name);
                }
                return;
            }
            ExpressionKind::Assign { target, value, .. } => {
                self.collect_formal_uses(*value, uses);
                self.collect_formal_uses(*target, uses);
                return;
            }
            // A write on one arm of a branch, in a loop body, or in an operand that may not be
            // evaluated covers only the paths that run it, so a later read still forces the
            // formal on the others, and is forced on every path only when no path wrote it.
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.collect_formal_uses(*condition, uses);
                let before = uses.assigned.clone();
                // The true edge of `a && b` has evaluated `b`, so the then branch has forced
                // what `b` reads, as `if (allow_null && is.null(x)) return(NULL)` reads `x`.
                let outer_forced = uses.forced.clone();
                self.collect_true_edge_reads(*condition, uses);
                let (then_assigned, then_forced) =
                    self.collect_branch_uses(*then_branch, uses, &before);
                uses.forced = outer_forced;
                let (else_assigned, else_forced) = match else_branch {
                    Some(else_branch) => self.collect_branch_uses(*else_branch, uses, &before),
                    None => (Some(before.clone()), FxHashSet::default()),
                };
                uses.assigned = join_assigned(before, [then_assigned, else_assigned]);
                uses.forced.extend(join_forced([then_forced, else_forced]));
                return;
            }
            // `%||%` evaluates its right operand only when the left one is `NULL`.
            ExpressionKind::Binary {
                operator,
                special_name,
                lhs,
                rhs,
            } if matches!(operator, BinaryOperator::And2 | BinaryOperator::Or2)
                || special_name.is_some_and(|name| name == "%||%") =>
            {
                self.collect_formal_uses(*lhs, uses);
                let before = uses.assigned.clone();
                let (after, _) = self.collect_branch_uses(*rhs, uses, &before);
                uses.assigned = after.unwrap_or(before);
                return;
            }
            ExpressionKind::While { condition, body } => {
                self.collect_formal_uses(*condition, uses);
                let before = uses.assigned.clone();
                let (after, _) = self.collect_branch_uses(*body, uses, &before);
                uses.assigned = after.unwrap_or(before);
                return;
            }
            ExpressionKind::For {
                variable,
                sequence,
                body,
                ..
            } => {
                self.collect_formal_uses(*sequence, uses);
                if let Some(variable) = variable {
                    uses.record_write(self.module.name(*variable));
                }
                let before = uses.assigned.clone();
                let (after, _) = self.collect_branch_uses(*body, uses, &before);
                uses.assigned = after.unwrap_or(before);
                return;
            }
            // `local()` evaluates in a new frame, so its writes bind nothing here.
            ExpressionKind::Local { body } => {
                let before = uses.assigned.clone();
                let outer_closures = uses.closures.clone();
                self.collect_formal_uses(*body, uses);
                uses.assigned = before;
                uses.closures = outer_closures;
                return;
            }
            ExpressionKind::Match { scrutinee, arms } => {
                self.collect_formal_uses(*scrutinee, uses);
                let before = uses.assigned.clone();
                let mut arm_assigned = Vec::with_capacity(arms.len());
                let mut arm_forced = Vec::with_capacity(arms.len());
                for arm in &self.module[arms] {
                    let mut entry = before.clone();
                    for &binding in &self.module[arm.bindings] {
                        if let ExpressionKind::NameRef(name) = self.module.kind(binding) {
                            uses.closures.remove(name);
                        }
                        if let ExpressionKind::NameRef(name) = self.module.kind(binding)
                            && uses.tracked.contains(name)
                            && !uses.forced.contains(name)
                        {
                            entry.insert(*name);
                        }
                    }
                    uses.assigned = entry;
                    if let Some(guard) = arm.guard {
                        self.collect_formal_uses(guard, uses);
                    }
                    let entry = mem::take(&mut uses.assigned);
                    let (assigned, forced) = self.collect_branch_uses(arm.body, uses, &entry);
                    arm_assigned.push(assigned);
                    arm_forced.push(forced);
                }
                uses.assigned = join_assigned(before, arm_assigned);
                uses.forced.extend(join_forced(arm_forced));
                return;
            }
            ExpressionKind::Call { callee, arguments } => {
                // rlang's `is_missing()` tests a formal the way `missing()` does.
                let tester = match self.module.kind(*callee) {
                    ExpressionKind::NameRef(name) => Some(name.as_str()),
                    ExpressionKind::Namespace {
                        package: Some(package),
                        name: Some(name),
                        ..
                    } if package == "base" || package == "rlang" => Some(name.as_str()),
                    _ => None,
                };
                // rlang's `enquo()` family captures the argument's code as `substitute()` does,
                // which is how tidyselect reads `columns = -icon` as a column name.
                if matches!(
                    tester,
                    Some("substitute" | "enquo" | "enquo0" | "enexpr" | "ensym")
                ) && let [argument] = &self.module[arguments][..]
                    && argument.name.is_none()
                    && let Some(value) = argument.value
                    && let ExpressionKind::NameRef(name) = self.module.kind(value)
                    && uses.tracked.contains(name)
                {
                    if !uses.assigned.contains(name) {
                        uses.substituted.insert(*name);
                    }
                    return;
                }
                if matches!(tester, Some("missing" | "is_missing")) {
                    if let [argument] = &self.module[arguments][..]
                        && argument.name.is_none()
                        && let Some(value) = argument.value
                        && let ExpressionKind::NameRef(name) = self.module.kind(value)
                    {
                        uses.missing_tested.insert(*name);
                    }
                    return;
                }
                if let ExpressionKind::NameRef(callee_name) = self.module.kind(*callee) {
                    // Only the first argument always runs; one arm at most runs after it, and
                    // none when no arm matches and there is no unnamed default.
                    if callee_name == "switch"
                        && let Some((subject, alternatives)) = self.module[arguments].split_first()
                    {
                        if let Some(subject) = subject.value {
                            self.collect_formal_uses(subject, uses);
                        }
                        let before = uses.assigned.clone();
                        let (mut arm_assigned, mut arm_forced): (Vec<_>, Vec<_>) = alternatives
                            .iter()
                            .filter_map(|argument| argument.value)
                            .map(|value| self.collect_branch_uses(value, uses, &before))
                            .unzip();
                        if alternatives.last().is_none_or(|last| last.name.is_some()) {
                            arm_assigned.push(Some(before.clone()));
                            arm_forced.push(FxHashSet::default());
                        }
                        uses.assigned = join_assigned(before, arm_assigned);
                        uses.forced.extend(join_forced(arm_forced));
                        return;
                    }
                    if callee_name == "return" {
                        for value in self.module[arguments]
                            .iter()
                            .filter_map(|argument| argument.value)
                        {
                            self.result_expressions(value, &mut uses.results);
                            self.collect_formal_uses(value, uses);
                        }
                        uses.record_return();
                        return;
                    }
                    // These read their argument's expression, never its value, so a missing
                    // formal passed to them is not forced. rlang's defusers capture the promise
                    // (`quo <- function(expr) enquo(expr)`), and `maybe_missing()` exists to
                    // pass a missing argument on.
                    if matches!(
                        callee_name.as_str(),
                        "substitute"
                            | "quote"
                            | "enquo"
                            | "enquos"
                            | "enexpr"
                            | "enexprs"
                            | "ensym"
                            | "ensyms"
                            | "maybe_missing"
                    ) {
                        return;
                    }
                }
                let mut called = *callee;
                while let ExpressionKind::Paren(inner) = self.module.kind(called) {
                    called = *inner;
                }
                let runs = match self.module.kind(called) {
                    ExpressionKind::Function { .. } => Some(self.closure_forced(called, uses)),
                    ExpressionKind::NameRef(name) => uses.closures.get(name).cloned(),
                    _ => None,
                };
                // `f(x = x)` hands the promise on unforced, and a closure the project defines
                // decides for itself whether to read it, so forwarding a formal under its own
                // name does not force it. A bare formal passed to a project parameter the callee
                // may leave unread, as `fast.svd(m, tol)` passes `tol` to a callee that tests
                // `missing(tol)`, is not forced either. A library function such as `list`
                // forces every argument.
                if !matches!(self.module.kind(called), ExpressionKind::Function { .. }) {
                    self.collect_formal_uses(*callee, uses);
                }
                let (forwards, optional_targets) = match self.module.kind(*callee) {
                    ExpressionKind::NameRef(name) => {
                        // A library function stubbed as `Any` (graphics' `rect`) declares no
                        // parameters, so nothing says it forces one: `rect(angle = angle)`
                        // reads `angle` only when `density` is set.
                        let forwards = self.globals.is_none_or(|globals| {
                            globals.library_candidates(*name).is_none_or(|candidates| {
                                candidates
                                    .iter()
                                    .all(|scheme| matches!(scheme.body.kind(self.db), TyKind::Any))
                            })
                        });
                        let optional_targets = match forwards {
                            true => self.optional_argument_targets(
                                *callee,
                                name,
                                &self.module[arguments],
                            ),
                            false => Bitset::new_in(self.arena, 0),
                        };
                        (forwards, optional_targets)
                    }
                    _ => (false, Bitset::new_in(self.arena, 0)),
                };
                // A function read from a field or a list element (`dist$quantile(p, parms)`) is
                // one the checker cannot see, and R forces a promise only when the callee reads it.
                let opaque_callee = matches!(
                    self.module.kind(called),
                    ExpressionKind::Field { .. } | ExpressionKind::Index { double: true, .. }
                );
                let library_name = match self.module.kind(*callee) {
                    ExpressionKind::NameRef(name) if !forwards => Some(name.as_str()),
                    ExpressionKind::Namespace {
                        package: Some(package),
                        name: Some(name),
                        ..
                    } if package == "base" => Some(name.as_str()),
                    _ => None,
                };
                for (index, argument) in self.module[arguments].iter().enumerate() {
                    let Some(value) = argument.value else {
                        continue;
                    };
                    let argument_name = self.module.argument_name(argument);
                    if let ExpressionKind::NameRef(value_name) = self.module.kind(value)
                        && (opaque_callee
                            || (forwards
                                && (argument_name == Some(*value_name)
                                    || optional_targets.contains(index)))
                            || library_name.zip(argument_name).is_some_and(
                                |(function, parameter)| {
                                    tests_missing_without_default(function, parameter.as_str())
                                },
                            ))
                    {
                        // The callee may still evaluate it, so the argument is not only code.
                        if uses.tracked.contains(value_name) && !uses.assigned.contains(value_name)
                        {
                            uses.evaluated.insert(*value_name);
                        }
                        continue;
                    }
                    self.collect_formal_uses(value, uses);
                }
                if let Some(forced) = runs {
                    uses.record_closure_run(&forced);
                }
                return;
            }
            // `[` reads a missing formal as an empty index, so `x$data[i, j]` passes the whole
            // dimension through and leaves `i` and `j` optional. `[[` has no empty index, and a
            // one-position `x[i]` stays forcing because the checker types it as one element.
            ExpressionKind::Index {
                double: false,
                target,
                arguments,
            } if self.module[arguments]
                .iter()
                .filter(|argument| argument.name.is_none())
                .count()
                >= 2 =>
            {
                self.collect_formal_uses(*target, uses);
                for argument in self.module[arguments].iter() {
                    let Some(value) = argument.value else {
                        continue;
                    };
                    if argument.name.is_none()
                        && let ExpressionKind::NameRef(name) = self.module.kind(value)
                        && uses.tracked.contains(name)
                        && !uses.assigned.contains(name)
                    {
                        uses.evaluated.insert(*name);
                        continue;
                    }
                    self.collect_formal_uses(value, uses);
                }
                return;
            }
            _ => {}
        }
        let module = self.module;
        module.for_each_child(id, |child| self.collect_formal_uses(child, uses));
    }

    /// Collects the expressions whose value `id` yields: the last statement of a block, both
    /// arms of an `if`, and the value of an assignment.
    fn result_expressions(&self, id: ExprId, results: &mut Vec<ExprId>) {
        match self.module.kind(id) {
            ExpressionKind::Paren(inner) => self.result_expressions(*inner, results),
            ExpressionKind::Assign { value, .. } => self.result_expressions(*value, results),
            ExpressionKind::Block { statements } => {
                if let Some(&last) = self.module[statements].last() {
                    self.result_expressions(last, results);
                }
            }
            ExpressionKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.result_expressions(*then_branch, results);
                if let Some(else_branch) = else_branch {
                    self.result_expressions(*else_branch, results);
                }
            }
            _ => results.push(id),
        }
    }

    /// Walks a nested function literal: its evaluations and `substitute()` reads of outer
    /// formals count where it is written, and the formals its body forces on every path are
    /// returned for the call that runs it. The literal's own parameters shadow outer formals.
    fn closure_forced(&self, function: ExprId, uses: &mut FormalUses) -> Rc<[Symbol]> {
        let ExpressionKind::Function { parameters, body } = self.module.kind(function) else {
            return Rc::from([]);
        };
        let mut nested = uses.nested();
        nested.assigned = self.module[parameters]
            .iter()
            .map(|parameter| parameter.name)
            .filter(|name| uses.tracked.contains(name))
            .collect();
        self.collect_formal_uses(*body, &mut nested);
        uses.evaluated.extend(
            nested
                .evaluated
                .iter()
                .filter(|name| !uses.assigned.contains(*name)),
        );
        uses.substituted.extend(nested.substituted.iter().copied());
        let mut forced: Vec<Symbol> = nested
            .forced
            .iter()
            .copied()
            .filter(|&name| nested.forces_on_every_path(name))
            .collect();
        forced.sort_unstable();
        Rc::from(forced)
    }

    /// For each argument of a call to the project function `name`, whether it binds a parameter the
    /// callee may leave unread: optional without a default, or tested with `missing()` (knitr's
    /// `kable_format(format = NULL)`). A missing formal passed on does not take the callee's
    /// default, so a defaulted parameter read without a test still errors. Names match exactly, the
    /// rest fill the unclaimed formals before `...` in order, and an unknown callee yields none, so
    /// its arguments stay forced. A
    /// local function already bound counts too (callr's `format_advice(x)` hands `x` to a helper
    /// that never reads it).
    fn optional_argument_targets(
        &self,
        callee: ExprId,
        name: &str,
        arguments: &[Argument],
    ) -> Bitset<'a, usize> {
        let callee_ty = match self.naming.resolutions.get(&callee) {
            Some(&slot) => match self.environment.get(slot) {
                Some(EnvEntry::Mono(ty)) => Some(ty),
                Some(EnvEntry::Scheme(index)) => self
                    .scheme_arena
                    .get(index as usize)
                    .map(|scheme| scheme.body),
                Some(EnvEntry::MissingFormal(_)) => None,
                // A sibling the enclosing body defines further down runs only after its
                // definition, and it decides for itself which arguments it reads.
                None if self.naming.bindings.get(&slot).is_some_and(|info| {
                    info.kind == BindingKind::Local
                        && info.range.start() > self.module.range(callee).start()
                }) =>
                {
                    let mut targets = Bitset::new_in(self.arena, arguments.len());
                    for index in 0..arguments.len() {
                        targets.insert(index);
                    }
                    return targets;
                }
                None => None,
            },
            None => self
                .globals
                .and_then(|globals| globals.scheme(Symbol::new(name), true))
                .map(|scheme| scheme.body),
        };
        let mut targets = Bitset::new_in(self.arena, arguments.len());
        let Some(TyKind::Function(function)) = callee_ty.map(|ty| ty.kind(self.db)) else {
            return targets;
        };
        let leaves_unread = |parameter: &Parameter<'db>| {
            parameter.optional
                && function.dispatch != Some(parameter.name)
                && (parameter.default.is_none()
                    || function.missing_tested.contains(&parameter.name))
        };
        let mut claimed = Bitset::new_in(self.arena, function.named.len());
        for (index, argument) in arguments.iter().enumerate() {
            let Some(argument_name) = self.module.argument_name(argument) else {
                continue;
            };
            if let Some(formal) = function
                .named
                .iter()
                .position(|parameter| parameter.name.text(self.db) == argument_name.as_str())
            {
                claimed.insert(formal);
                if leaves_unread(&function.named[formal]) {
                    targets.insert(index);
                }
            }
        }
        let pre_rest = function
            .variadic
            .as_ref()
            .map_or(function.named.len(), |rest| {
                rest.preceding_named.min(function.named.len())
            });
        let mut unclaimed = (0..pre_rest).filter(|formal| !claimed.contains(*formal));
        for (index, argument) in arguments.iter().enumerate() {
            if argument.name.is_some() {
                continue;
            }
            let Some(formal) = unclaimed.next() else {
                break;
            };
            if leaves_unread(&function.named[formal]) {
                targets.insert(index);
            }
        }
        targets
    }

    /// Walks one branch from `before` and returns what it leaves assigned, or `None` when it
    /// diverges, so a join ignores it, together with the names it forces. The branch's forced
    /// names stay out of `uses.forced`: the caller decides whether every path forces them.
    fn collect_branch_uses(
        &self,
        id: ExprId,
        uses: &mut FormalUses,
        before: &FxHashSet<Symbol>,
    ) -> (Option<FxHashSet<Symbol>>, FxHashSet<Symbol>) {
        uses.assigned = before.clone();
        // The branch starts from the names already forced, so a `return()` inside it
        // records every name its path has forced.
        let outer_forced = uses.forced.clone();
        let outer_closures = uses.closures.clone();
        self.collect_formal_uses(id, uses);
        let forced = mem::replace(&mut uses.forced, outer_forced);
        // A local closure bound on one branch only may be another value after the join.
        let branch_closures = mem::replace(&mut uses.closures, outer_closures);
        uses.closures
            .retain(|name, forced| branch_closures.get(name) == Some(forced));
        let after = mem::take(&mut uses.assigned);
        ((!self.diverges(id)).then_some(after), forced)
    }

    /// Adds to `uses.forced` what the right operands of a condition's `&&` chain read, which
    /// only the true edge has evaluated.
    fn collect_true_edge_reads(&self, condition: ExprId, uses: &mut FormalUses) {
        let mut condition = condition;
        while let ExpressionKind::Paren(inner) = self.module.kind(condition) {
            condition = *inner;
        }
        if let ExpressionKind::Binary {
            operator: BinaryOperator::And2,
            lhs,
            rhs,
            ..
        } = self.module.kind(condition)
        {
            self.collect_true_edge_reads(*lhs, uses);
            let mut right = uses.nested();
            right.assigned = uses.assigned.clone();
            self.collect_formal_uses(*rhs, &mut right);
            self.collect_true_edge_reads(*rhs, &mut right);
            uses.forced.extend(right.forced);
        }
    }

    /// One arbitrary instantiation of the in-scope rigid binders, to ask whether a value
    /// inhabits the declared type at *some* choice of its parameters.
    fn instantiate_rigids(&mut self, ty: Ty<'db>) -> Ty<'db> {
        if self.table.rigid_constraints.is_empty() {
            return ty;
        }
        let binders: Vec<(Name<'db>, Constraint)> = self
            .table
            .rigid_constraints
            .iter()
            .map(|(name, constraint)| (*name, *constraint))
            .collect();
        let substitution: Vec<_> = binders
            .into_iter()
            .map(|(name, constraint)| (name, self.fresh(constraint)))
            .collect();
        for &(name, fresh) in &substitution {
            if let Some(&bound) = self.table.rigid_bounds.get(&name)
                && let Some(var) = fresh.as_var(self.db)
            {
                let bound = substitute_rigid(self.db, bound, &substitution);
                self.table.set_bound(var, bound);
            }
        }
        substitute_rigid(self.db, ty, &substitution)
    }

    // `return`/`stop` are recognized by bare name; rebinding them is not modeled.
    fn diverges(&self, id: ExprId) -> bool {
        match self.module.kind(id) {
            ExpressionKind::Break | ExpressionKind::Next => true,
            ExpressionKind::Call { callee, .. } => matches!(
                self.module.kind(*callee),
                ExpressionKind::NameRef(name) if name == "return" || name == "stop"
            ),
            // Code after a `stop()` never runs, as psych's placeholder definitions after
            // `stop("needs Rgraphviz")` never bind.
            ExpressionKind::Block { statements, .. } => self.module[statements]
                .iter()
                .any(|&statement| self.diverges(statement)),
            ExpressionKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => self.diverges(*then_branch) && self.diverges(*else_branch),
            ExpressionKind::Paren(inner) => self.diverges(*inner),
            ExpressionKind::Match { arms, .. } => {
                !arms.is_empty() && self.module[arms].iter().all(|arm| self.diverges(arm.body))
            }
            ExpressionKind::Repeat { body } => !self.breaks_out(*body),
            _ => false,
        }
    }

    /// The facts an `if` leaves about the slots its then branch wrote, when its condition tests
    /// a local that neither branch writes (`if (case %in% c("a", "b")) s <- f(s)`).
    fn branch_correlations(
        &mut self,
        condition: ExprId,
        then_writes: &[(BindingId, Option<EnvEntry<'db>>)],
        false_mark: usize,
    ) -> Vec<Fact<'db>> {
        let Some(test) = key_test(self.module, &self.naming.resolutions, condition) else {
            return Vec::new();
        };
        let else_writes = self.environment.writes_since(false_mark);
        if then_writes
            .iter()
            .chain(&else_writes)
            .any(|(slot, _)| *slot == test.key)
        {
            return Vec::new();
        }
        then_writes
            .iter()
            .filter_map(|&(slot, entry)| {
                let (Some(EnvEntry::Mono(when_true)), Some(EnvEntry::Mono(when_false))) =
                    (entry, self.environment.get(slot))
                else {
                    return None;
                };
                (when_true != when_false).then_some(Fact::Correlated {
                    slot,
                    key: test.key,
                    condition,
                    when_true,
                    when_false,
                })
            })
            .collect()
    }

    /// Adds to `guard` the slot values that earlier correlated writes imply on each edge: after
    /// `if (case == "a") s <- f(s)`, the true edge of `if (case == "b")` holds `s` as it was
    /// before, because the two tests exclude each other. Each edge also records what it shows
    /// about the tested local, and an edge reads its test within the enclosing ones, so two
    /// `if`s that together cover an enclosing `case %in% ...` leave no path that skips both.
    fn correlated_edges(
        &mut self,
        condition: ExprId,
        guard: Option<GuardRefinement<'db>>,
    ) -> Option<GuardRefinement<'db>> {
        let Some(test) = key_test(self.module, &self.naming.resolutions, condition) else {
            return guard;
        };
        let mut guard = guard.unwrap_or_default();
        for negated in [false, true] {
            let edge = match negated {
                false => &mut guard.true_edge,
                true => &mut guard.false_edge,
            };
            edge.push(SlotRefinement::Establish(Fact::KeyDomain {
                key: test.key,
                condition,
                negated,
            }));
        }
        let mut negated = test.negated();
        let mut test = test;
        for fact in &self.environment.facts {
            let Fact::KeyDomain {
                key,
                condition: enclosing,
                negated: enclosing_negated,
            } = *fact
            else {
                continue;
            };
            if key != test.key {
                continue;
            }
            let Some(enclosing) = key_test(self.module, &self.naming.resolutions, enclosing) else {
                continue;
            };
            let enclosing = match enclosing_negated {
                true => enclosing.negated(),
                false => enclosing,
            };
            test = test.within(&enclosing);
            negated = negated.within(&enclosing);
        }
        for fact in &self.environment.facts {
            let Fact::Correlated {
                slot,
                key,
                condition: earlier,
                when_true,
                when_false,
            } = *fact
            else {
                continue;
            };
            if key != test.key {
                continue;
            }
            let Some(earlier) = key_test(self.module, &self.naming.resolutions, earlier) else {
                continue;
            };
            let earlier_negated = earlier.negated();
            for (edge, edge_test) in [
                (&mut guard.true_edge, &test),
                (&mut guard.false_edge, &negated),
            ] {
                if edge_test.implies(&earlier) {
                    edge.push(SlotRefinement::Narrow(slot, when_true));
                } else if edge_test.implies(&earlier_negated) {
                    edge.push(SlotRefinement::Narrow(slot, when_false));
                }
            }
        }
        if guard.true_edge.is_empty() && guard.false_edge.is_empty() {
            return None;
        }
        Some(guard)
    }

    /// A false-edge fact holds after the `if` only when the then branch diverged.
    fn retract_false_facts(&mut self, guard: Option<&GuardRefinement<'db>>) {
        for refinement in guard.into_iter().flat_map(|guard| &guard.false_edge) {
            if let SlotRefinement::Establish(fact @ Fact::FieldNarrowed { .. }) = *refinement {
                self.environment.retract(fact);
            }
        }
    }

    /// After an `if` whose branches both continue, the false edge's view of a tested local
    /// does not hold.
    fn retract_false_domain(&mut self, condition: ExprId) {
        if let Some(test) = key_test(self.module, &self.naming.resolutions, condition) {
            self.environment.retract(Fact::KeyDomain {
                key: test.key,
                condition,
                negated: true,
            });
        }
    }

    /// Whether `id`'s value is the result of a library call its untyped arguments left
    /// undecided between candidates.
    fn pending_reads(&self) -> usize {
        self.placeholder_reads + self.globals.map_or(0, |globals| globals.pending_reads())
    }

    fn ends_in_undecided_call(&self, id: ExprId) -> bool {
        match self.module.kind(id) {
            ExpressionKind::Call { .. } => self.undecided_calls.contains(id),
            ExpressionKind::Block { statements, .. } => self.module[statements]
                .last()
                .is_some_and(|&last| self.ends_in_undecided_call(last)),
            ExpressionKind::Paren(inner) => self.ends_in_undecided_call(*inner),
            _ => false,
        }
    }

    /// `stop()` never returns, so no value of its result can be misused: it types as `Any`, the
    /// one type that fits every context, which also keeps strict mode from reporting it.
    fn infer_stop(&mut self, arguments: &[Argument]) -> Ty<'db> {
        for argument in arguments {
            if let Some(value) = argument.value {
                self.infer(value);
            }
        }
        any(self.db)
    }

    /// Whether a `break` in `id` leaves the loop enclosing it; an inner loop or function
    /// owns its own `break`s.
    fn breaks_out(&self, id: ExprId) -> bool {
        match self.module.kind(id) {
            ExpressionKind::Break => true,
            ExpressionKind::For { .. }
            | ExpressionKind::While { .. }
            | ExpressionKind::Repeat { .. }
            | ExpressionKind::Function { .. } => false,
            _ => self.module.any_child(id, |child| self.breaks_out(child)),
        }
    }

    fn reads_slot(&self, id: ExprId, slot: BindingId) -> bool {
        match self.module.kind(id) {
            ExpressionKind::NameRef(_) => self.naming.resolutions.get(&id) == Some(&slot),
            ExpressionKind::Function { .. } => false,
            _ => self
                .module
                .any_child(id, |child| self.reads_slot(child, slot)),
        }
    }

    /// A top-level `return` (an R runtime error) still checks its value but joins no frame.
    fn infer_return(&mut self, range: TextRange, arguments: &[Argument]) -> Ty<'db> {
        let value_ty = match arguments.first().and_then(|argument| argument.value) {
            Some(value) => self.infer(value),
            None => null(self.db),
        };
        for argument in arguments.iter().skip(1) {
            if let Some(value) = argument.value {
                self.infer(value);
            }
        }
        let resolved = self.table.resolve(self.db, value_ty);
        let blamed = match arguments.first().and_then(|argument| argument.value) {
            Some(value) => self.blame_range(value),
            None => range,
        };
        if let Some(value) = arguments.first().and_then(|argument| argument.value)
            && matches!(
                self.module.kind(value),
                ExpressionKind::Literal(LiteralKind::Na(NaAtom::Logical))
            )
        {
            self.bare_na_returns.insert(blamed);
        }
        let value = arguments.first().and_then(|argument| argument.value);
        if let Some(frame) = self.return_frames.last_mut() {
            frame.push(EarlyReturn {
                ty: resolved,
                range: blamed,
                value,
            });
        }
        null(self.db)
    }

    fn recognize_guard(&mut self, condition: ExprId) -> Option<GuardRefinement<'db>> {
        if let Some((pair, differ)) = self.null_states_compared(condition) {
            let edge = vec![SlotRefinement::Establish(Fact::NullTogether(
                pair.0, pair.1,
            ))];
            return Some(if differ {
                GuardRefinement {
                    true_edge: Vec::new(),
                    false_edge: edge,
                }
            } else {
                GuardRefinement {
                    true_edge: edge,
                    false_edge: Vec::new(),
                }
            });
        }
        match self.module.kind(condition) {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => {
                let inner = self.recognize_guard(*operand)?;
                // `!missing(x)` has no read-erroring edge to swap into.
                let narrowed_only = |edge: Vec<SlotRefinement<'db>>| {
                    edge.into_iter()
                        .filter(|refinement| !matches!(refinement, SlotRefinement::Missing(..)))
                        .collect()
                };
                Some(GuardRefinement {
                    true_edge: narrowed_only(inner.false_edge),
                    false_edge: narrowed_only(inner.true_edge),
                })
            }
            ExpressionKind::Paren(inner) => self.recognize_guard(*inner),
            // `length(x) != 2` compares a length with a literal, and whichever edge a length of
            // zero cannot reach holds a value that is not `NULL`.
            ExpressionKind::Binary {
                operator:
                    operator @ (BinaryOperator::Less
                    | BinaryOperator::Greater
                    | BinaryOperator::LessEq
                    | BinaryOperator::GreaterEq
                    | BinaryOperator::Equal
                    | BinaryOperator::NotEqual),
                lhs,
                rhs,
                ..
            } => {
                let module = self.module;
                let (operator, (callee, value), bound) =
                    match (length_operand(module, *lhs), length_operand(module, *rhs)) {
                        (Some(call), None) => (*operator, call, numeric_literal(module, *rhs)?),
                        (None, Some(call)) => {
                            (mirrored(*operator), call, numeric_literal(module, *lhs)?)
                        }
                        _ => return None,
                    };
                if self.naming.resolutions.contains_key(&callee) {
                    return None;
                }
                let slot = self.guard_slot(value)?;
                let edge = self.non_null_edge(slot)?;
                let empty_passes = match operator {
                    BinaryOperator::Less => 0.0 < bound,
                    BinaryOperator::Greater => 0.0 > bound,
                    BinaryOperator::LessEq => 0.0 <= bound,
                    BinaryOperator::GreaterEq => 0.0 >= bound,
                    BinaryOperator::Equal => bound == 0.0,
                    _ => bound != 0.0,
                };
                Some(if empty_passes {
                    GuardRefinement {
                        true_edge: Vec::new(),
                        false_edge: edge,
                    }
                } else {
                    GuardRefinement {
                        true_edge: edge,
                        false_edge: Vec::new(),
                    }
                })
            }
            // The right operand runs only on the left's true edge (`&&`) or false edge (`||`),
            // so it is recognized under that edge and the two compose.
            ExpressionKind::Binary {
                operator: operator @ (BinaryOperator::And2 | BinaryOperator::Or2),
                lhs,
                rhs,
                ..
            } => {
                let conjunction = *operator == BinaryOperator::And2;
                let (lhs, rhs) = (*lhs, *rhs);
                let left = self.recognize_guard(lhs).unwrap_or_default();
                let mark = self.environment.mark();
                let left_edge = if conjunction {
                    &left.true_edge
                } else {
                    &left.false_edge
                };
                self.apply_refinements(left_edge);
                let right = self.recognize_guard(rhs).unwrap_or_default();
                self.environment.rollback(mark);
                let (mut combined, right_edge) = if conjunction {
                    (left.true_edge, right.true_edge)
                } else {
                    (left.false_edge, right.false_edge)
                };
                combined.extend(right_edge);
                if combined.is_empty() {
                    return None;
                }
                Some(if conjunction {
                    GuardRefinement {
                        true_edge: combined,
                        false_edge: Vec::new(),
                    }
                } else {
                    GuardRefinement {
                        true_edge: Vec::new(),
                        false_edge: combined,
                    }
                })
            }
            ExpressionKind::Call { callee, arguments } => {
                let ExpressionKind::NameRef(name) = self.module.kind(*callee) else {
                    return None;
                };
                if self.naming.resolutions.contains_key(callee) {
                    return None;
                }
                // `identical(format_name, identity)` holds only when the local is that very
                // function, so the true edge calls it with the function's own type.
                if name == "identical"
                    && let [first, second] = &self.module[arguments][..]
                    && first.name.is_none()
                    && second.name.is_none()
                {
                    let (local, function) = (first.value?, second.value?);
                    // `identical(info$type, "conda")` holds only when `info` has that field, so
                    // the true edge has `info` not `NULL`.
                    if let ExpressionKind::Field {
                        at: false,
                        target,
                        name: Some(_),
                    } = self.module.kind(local)
                    {
                        key_value(self.module, function)?;
                        let slot = self.guard_slot(*target)?;
                        return Some(GuardRefinement {
                            true_edge: self.non_null_edge(slot)?,
                            false_edge: Vec::new(),
                        });
                    }
                    if !matches!(self.module.kind(local), ExpressionKind::NameRef(_)) {
                        return None;
                    }
                    let slot = *self.naming.resolutions.get(&local)?;
                    let ty = self.recorded_type(function);
                    if self.callable(ty) != Callable::Yes {
                        return None;
                    }
                    // A polymorphic function (`identity`) keeps its scheme, so each later call
                    // instantiates it afresh instead of sharing the one instantiation above.
                    let refinement = match self.function_scheme(function) {
                        Some(index) => SlotRefinement::NarrowScheme(slot, index),
                        None => SlotRefinement::Narrow(slot, ty),
                    };
                    return Some(GuardRefinement {
                        true_edge: vec![refinement],
                        false_edge: Vec::new(),
                    });
                }
                if name == "inherits" {
                    return self.inherits_guard(&self.module[arguments]);
                }
                let [argument] = &self.module[arguments][..] else {
                    return None;
                };
                if argument.name.is_some() {
                    return None;
                }
                let value = argument.value?;
                // `is.integer(limit <- check_limit(limit))` tests the value it just stored, so
                // the guard narrows the assigned local.
                let value = match self.module.kind(value) {
                    ExpressionKind::Assign {
                        spelling: AssignSpelling::Local | AssignSpelling::Equals,
                        target,
                        ..
                    } if matches!(self.module.kind(*target), ExpressionKind::NameRef(_)) => *target,
                    _ => value,
                };
                if name == "is.null"
                    && let ExpressionKind::Field {
                        at: false,
                        target,
                        name: Some(field),
                    } = self.module.kind(value)
                {
                    let field = self.module.name(*field);
                    return self.field_null_guard(*target, &field);
                }
                let slot = self.guard_slot(value)?;
                // A value of nonzero length is not `NULL`, so `if (length(x))` guards it.
                if name == "length" {
                    return Some(GuardRefinement {
                        true_edge: self.non_null_edge(slot)?,
                        false_edge: Vec::new(),
                    });
                }
                if name == "missing" {
                    return self
                        .no_default_formals
                        .contains(slot)
                        .then(|| GuardRefinement {
                            true_edge: vec![SlotRefinement::Missing(slot)],
                            false_edge: Vec::new(),
                        });
                }
                let Some(guard) = guard_kind(name) else {
                    return Some(GuardRefinement {
                        true_edge: vec![SlotRefinement::Establish(Fact::Holds(slot, *name))],
                        false_edge: Vec::new(),
                    });
                };
                let EnvEntry::Mono(current) = self.environment.get(slot)? else {
                    return None;
                };
                let resolved = self.table.resolve(self.db, current);
                let mut edges = self.guard_edges(guard, slot, resolved)?;
                if guard == GuardKind::Null {
                    let partners: Vec<BindingId> = self.environment.null_partners(slot).collect();
                    for partner in partners {
                        let Some(EnvEntry::Mono(current)) = self.environment.get(partner) else {
                            continue;
                        };
                        let resolved = self.table.resolve(self.db, current);
                        if let Some(partner_edges) = self.guard_edges(guard, partner, resolved) {
                            edges.true_edge.extend(partner_edges.true_edge);
                            edges.false_edge.extend(partner_edges.false_edge);
                        }
                    }
                }
                Some(edges)
            }
            _ => None,
        }
    }

    /// `inherits(x, "simpleError")` holds only of a value with a class attribute when the class
    /// is not one R gives an unclassed value implicitly, so the true edge drops `x`'s atomic,
    /// `NULL` and function members: after `res <- tryCatch(download(), error = function(e) e)`,
    /// `inherits(res, "error")` leaves the condition.
    fn inherits_guard(&mut self, arguments: &[Argument]) -> Option<GuardRefinement<'db>> {
        let [subject, class] = arguments else {
            return None;
        };
        if subject.name.is_some() || class.name.is_some() {
            return None;
        }
        let ExpressionKind::Literal(LiteralKind::String(text)) = self.module.kind(class.value?)
        else {
            return None;
        };
        if matches!(
            &self.module[text],
            "integer"
                | "numeric"
                | "double"
                | "character"
                | "logical"
                | "complex"
                | "raw"
                | "function"
                | "list"
                | "NULL"
                | "matrix"
                | "array"
                | "environment"
                | "name"
                | "call"
                | "expression"
        ) {
            return None;
        }
        let slot = self.guard_slot(subject.value?)?;
        let EnvEntry::Mono(current) = self.environment.get(slot)? else {
            return None;
        };
        let members = guard_members(self.db, self.table.resolve(self.db, current));
        let decide = |db: &'db dyn Db, member: Ty<'db>| match member.kind(db) {
            TyKind::Scalar(_)
            | TyKind::Vector(_)
            | TyKind::NamedVector(_)
            | TyKind::Null
            | TyKind::Function(_) => Some(false),
            _ => None,
        };
        let refinement = self.filtered_edges(slot, &members, decide);
        Some(GuardRefinement {
            true_edge: refinement.true_edge,
            false_edge: Vec::new(),
        })
    }

    /// A test of whether two locals differ in being `NULL`, written as
    /// `is.null(A) && !is.null(b) || !is.null(A) && is.null(b)`, `xor(is.null(A), is.null(b))` or
    /// `is.null(A) != is.null(b)`. Returns the two slots and whether the test is true when they
    /// differ.
    fn null_states_compared(&self, condition: ExprId) -> Option<((BindingId, BindingId), bool)> {
        let module = self.module;
        let strip = |mut id: ExprId| {
            while let ExpressionKind::Paren(inner) = module.kind(id) {
                id = *inner;
            }
            id
        };
        let null_test = |id: ExprId| -> Option<(BindingId, bool)> {
            let mut id = strip(id);
            let mut holds = true;
            while let ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } = module.kind(id)
            {
                holds = !holds;
                id = strip(*operand);
            }
            let ExpressionKind::Call { callee, arguments } = module.kind(id) else {
                return None;
            };
            if variable_name(module, *callee).as_deref() != Some("is.null")
                || self.naming.resolutions.contains_key(callee)
            {
                return None;
            }
            let [argument] = &module[arguments][..] else {
                return None;
            };
            let value = strip(argument.value.filter(|_| argument.name.is_none())?);
            Some((*self.naming.resolutions.get(&value)?, holds))
        };
        let pair = |first: ExprId, second: ExprId| {
            let (first, first_holds) = null_test(first)?;
            let (second, second_holds) = null_test(second)?;
            (first != second).then_some(((first, second), first_holds != second_holds))
        };
        match module.kind(strip(condition)) {
            ExpressionKind::Binary {
                operator: operator @ (BinaryOperator::Equal | BinaryOperator::NotEqual),
                lhs,
                rhs,
                ..
            } => {
                let (slots, opposite) = pair(*lhs, *rhs)?;
                Some((slots, opposite == (*operator == BinaryOperator::Equal)))
            }
            ExpressionKind::Call { callee, arguments }
                if variable_name(module, *callee).as_deref() == Some("xor")
                    && !self.naming.resolutions.contains_key(callee) =>
            {
                let [first, second] = &module[arguments][..] else {
                    return None;
                };
                if first.name.is_some() || second.name.is_some() {
                    return None;
                }
                let (slots, opposite) = pair(first.value?, second.value?)?;
                Some((slots, !opposite))
            }
            ExpressionKind::Binary {
                operator: BinaryOperator::Or2,
                lhs,
                rhs,
                ..
            } => {
                let conjunct = |id: ExprId| match module.kind(strip(id)) {
                    ExpressionKind::Binary {
                        operator: BinaryOperator::And2,
                        lhs,
                        rhs,
                        ..
                    } => {
                        let (first, first_holds) = null_test(*lhs)?;
                        let (second, second_holds) = null_test(*rhs)?;
                        let (first, first_holds, second, second_holds) = if first <= second {
                            (first, first_holds, second, second_holds)
                        } else {
                            (second, second_holds, first, first_holds)
                        };
                        (first != second && first_holds != second_holds)
                            .then_some(((first, second), first_holds))
                    }
                    _ => None,
                };
                let (left, left_first) = conjunct(*lhs)?;
                let (right, right_first) = conjunct(*rhs)?;
                (left == right && left_first != right_first).then_some((left, true))
            }
            _ => None,
        }
    }

    /// The slot a guard on the bare name `value` narrows.
    fn guard_slot(&mut self, value: ExprId) -> Option<BindingId> {
        if !matches!(self.module.kind(value), ExpressionKind::NameRef(_)) {
            return None;
        }
        match self.naming.resolutions.get(&value) {
            Some(&slot) => Some(slot),
            // A non-local narrows like a local (guard and branches evaluate in order), except
            // inside a closure, which runs later and sees a value the test says nothing about.
            None => {
                if self.naming.deferred_non_locals.contains(&value) {
                    return None;
                }
                let observed = self.infer(value);
                self.non_local_guard_slot(value, observed)
            }
        }
    }

    /// The refinement that drops `NULL` from a slot whose union holds it. A value of no known
    /// shape is left alone, since a length test says nothing else about it.
    fn non_null_edge(&mut self, slot: BindingId) -> Option<Vec<SlotRefinement<'db>>> {
        let EnvEntry::Mono(current) = self.environment.get(slot)? else {
            return None;
        };
        let resolved = self.table.resolve(self.db, current);
        let TyKind::Union(members) = resolved.kind(self.db) else {
            return None;
        };
        if !members
            .iter()
            .any(|member| matches!(member.kind(self.db), TyKind::Null))
        {
            return None;
        }
        let edges = self.guard_edges(GuardKind::Null, slot, resolved)?;
        (!edges.false_edge.is_empty()).then_some(edges.false_edge)
    }

    /// `is.null(x$field)` on a local `x`. A value that passes the test has the field, so the
    /// false edge drops each member of `x` that cannot carry it: `NULL`, an empty `list()`
    /// placeholder, and a record with no field the name could partially match. It also drops
    /// `NULL` from the field itself in each record that names it exactly, so `x$field` reads
    /// non-`NULL` there. A nominal member keeps its identity, so for a nominal the edge records
    /// the narrowed field as a fact of the slot instead. The true edge keeps every member, since
    /// a field may itself hold `NULL`.
    fn field_null_guard(&mut self, target: ExprId, field: &str) -> Option<GuardRefinement<'db>> {
        if !matches!(self.module.kind(target), ExpressionKind::NameRef(_)) {
            return None;
        }
        let &slot = self.naming.resolutions.get(&target)?;
        let EnvEntry::Mono(current) = self.environment.get(slot)? else {
            return None;
        };
        let resolved = self.table.resolve(self.db, current);
        let members = guard_members(self.db, resolved);
        let lacks_field = |member: Ty<'db>| match member.kind(self.db) {
            TyKind::Null => true,
            TyKind::Record(fields) => !fields
                .iter()
                .any(|candidate| candidate.name.text(self.db).starts_with(field)),
            _ => is_empty_list(self.db, member),
        };
        // A nominal member is tested through its representation and kept as itself, so the
        // test tells `Circle` from `Square` without losing either identity.
        let projections: Vec<Ty<'db>> = members
            .iter()
            .map(|&member| match member.kind(self.db) {
                TyKind::Named(..) => self.structural(member),
                _ => member,
            })
            .collect();
        let kept: Vec<Ty<'db>> = members
            .iter()
            .zip(&projections)
            .filter(|&(_, &projection)| !lacks_field(projection))
            .map(|(&member, _)| match member.kind(self.db) {
                TyKind::Named(..) => member,
                _ => self.without_null_field(member, field),
            })
            .collect();
        if kept.is_empty() {
            return None;
        }
        let mut false_edge = Vec::new();
        if let Some(narrowed) = self.nominal_field_without_null(&kept, field) {
            false_edge.push(SlotRefinement::Establish(Fact::FieldNarrowed {
                slot,
                field: Symbol::new(field),
                ty: narrowed,
            }));
        }
        if kept.as_slice() != members.as_slice() {
            false_edge.push(SlotRefinement::Narrow(slot, union_of(self.db, kept)));
        }
        (!false_edge.is_empty()).then_some(GuardRefinement {
            true_edge: Vec::new(),
            false_edge,
        })
    }

    /// The type of field `field` without `NULL` when every member is a nominal whose
    /// representation names the field exactly and lets it hold `NULL`.
    fn nominal_field_without_null(&mut self, members: &[Ty<'db>], field: &str) -> Option<Ty<'db>> {
        let mut present = Vec::new();
        let mut had_null = false;
        for &member in members {
            if !matches!(member.kind(self.db), TyKind::Named(..)) {
                return None;
            }
            let representation = self.structural(member);
            let TyKind::Record(fields) = representation.kind(self.db) else {
                return None;
            };
            let candidate = fields
                .iter()
                .find(|candidate| candidate.name.text(self.db) == field)?;
            let field_ty = self.table.resolve(self.db, candidate.ty);
            for &field_member in guard_members(self.db, field_ty).iter() {
                if matches!(field_member.kind(self.db), TyKind::Null) {
                    had_null = true;
                } else {
                    present.push(field_member);
                }
            }
        }
        (had_null && !present.is_empty()).then(|| union_of(self.db, present))
    }

    /// `record` with `NULL` dropped from its field `field` and the field made present, or
    /// `record` itself when it is no record, lacks that exact field, or the field holds only
    /// `NULL`.
    fn without_null_field(&self, record: Ty<'db>, field: &str) -> Ty<'db> {
        let TyKind::Record(fields) = record.kind(self.db) else {
            return record;
        };
        let Some(index) = fields
            .iter()
            .position(|candidate| candidate.name.text(self.db) == field)
        else {
            return record;
        };
        let field_ty = self.table.resolve(self.db, fields[index].ty);
        let field_members = guard_members(self.db, field_ty);
        let present: Vec<Ty<'db>> = field_members
            .iter()
            .copied()
            .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
            .collect();
        if present.is_empty() || (present.len() == field_members.len() && !fields[index].optional) {
            return record;
        }
        let mut narrowed = fields.to_vec();
        narrowed[index] = RecordField {
            name: fields[index].name,
            ty: union_of(self.db, present),
            optional: false,
        };
        Ty::new(self.db, TyKind::Record(&narrowed))
    }

    /// Seeded with the observed type so the guard has a union to filter.
    fn non_local_guard_slot(&mut self, read: ExprId, observed: Ty<'db>) -> Option<BindingId> {
        let name = *self.naming.non_locals.get(&read)?;
        let slot = self.non_local_slot(name);
        if self.environment.get(slot).is_none() {
            self.environment.set(slot, EnvEntry::Mono(observed));
        }
        Some(slot)
    }

    fn non_local_slot(&mut self, name: Symbol) -> BindingId {
        if let Some(&slot) = self.non_local_slots.get(&name) {
            return slot;
        }
        let slot = BindingId(self.next_non_local_slot);
        self.next_non_local_slot += 1;
        self.non_local_slots.insert(name, slot);
        slot
    }

    fn apply_refinements(&mut self, refinements: &[SlotRefinement<'db>]) {
        for refinement in refinements {
            match *refinement {
                SlotRefinement::Narrow(slot, ty) => self.environment.set(slot, EnvEntry::Mono(ty)),
                SlotRefinement::NarrowScheme(slot, index) => {
                    self.environment.set(slot, EnvEntry::Scheme(index))
                }
                SlotRefinement::Missing(slot) => {
                    if let Some(EnvEntry::Mono(current)) = self.environment.get(slot) {
                        self.environment.set(slot, EnvEntry::MissingFormal(current));
                    }
                }
                SlotRefinement::Establish(fact) => self.environment.establish(fact),
            }
        }
    }

    fn guard_edges(
        &mut self,
        guard: GuardKind,
        slot: BindingId,
        resolved: Ty<'db>,
    ) -> Option<GuardRefinement<'db>> {
        match guard {
            GuardKind::Null => {
                match resolved.kind(self.db) {
                    // The runtime guarantees the true edge is `NULL` even for an untracked type.
                    TyKind::Any | TyKind::Unknown => {
                        return Some(GuardRefinement {
                            true_edge: vec![SlotRefinement::Narrow(slot, null(self.db))],
                            false_edge: Vec::new(),
                        });
                    }
                    // An unconstrained variable is shaped by the test into `T | NULL` (the
                    // unannotated coalesce idiom); a constrained variable or rigid is never reshaped.
                    TyKind::Var(var) => {
                        let Entry::Unbound {
                            constraint: Constraint::Unconstrained,
                            ..
                        } = *self.table.entry(*var)
                        else {
                            return None;
                        };
                        let fresh = self.fresh(Constraint::Unconstrained);
                        let shaped = union_of(self.db, [fresh, null(self.db)]);
                        if self.table.unify(self.db, resolved, shaped).is_err() {
                            return None;
                        }
                        return Some(GuardRefinement {
                            true_edge: vec![SlotRefinement::Narrow(slot, shaped)],
                            false_edge: vec![SlotRefinement::Narrow(slot, fresh)],
                        });
                    }
                    _ => {}
                }
                let members = guard_members(self.db, resolved);
                if !members
                    .iter()
                    .any(|member| matches!(member.kind(self.db), TyKind::Null))
                {
                    return None;
                }
                let decide = |db: &'db dyn Db, member: Ty<'db>| match member.kind(db) {
                    TyKind::Null => Some(true),
                    TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Named(..) => None,
                    _ => Some(false),
                };
                Some(self.filtered_edges(slot, &members, decide))
            }
            GuardKind::Family(family) => {
                // Inventing a concrete shape for `Any`/`Unknown` would false-positive against
                // scalar-claim stub signatures.
                // An untyped value that passes the test is used as that family on the true
                // edge only (`is.numeric(x) && x > 0`), so it reads as unchecked there rather
                // than constraining the parameter every other path still holds.
                let unconstrained = match resolved.kind(self.db) {
                    TyKind::Var(_) => {
                        self.table.open_constraint(self.db, resolved)
                            == Some(Constraint::Unconstrained)
                    }
                    TyKind::Rigid(name) => matches!(
                        self.table.rigid_constraints.get(name),
                        None | Some(Constraint::Unconstrained)
                    ),
                    _ => false,
                };
                if unconstrained {
                    return Some(GuardRefinement {
                        true_edge: vec![SlotRefinement::Narrow(slot, unknown(self.db))],
                        false_edge: Vec::new(),
                    });
                }
                if matches!(
                    resolved.kind(self.db),
                    TyKind::Any | TyKind::Unknown | TyKind::Var(_) | TyKind::Rigid(_)
                ) {
                    return None;
                }
                let members = guard_members(self.db, resolved);
                let decide =
                    move |db: &'db dyn Db, member: Ty<'db>| family_membership(db, member, family);
                Some(self.filtered_edges(slot, &members, decide))
            }
        }
    }

    /// An undecidable member stays on both edges; an edge that removes nothing (or keeps
    /// nothing) refines nothing.
    fn filtered_edges(
        &self,
        slot: BindingId,
        members: &[Ty<'db>],
        decide: impl Fn(&'db dyn Db, Ty<'db>) -> Option<bool>,
    ) -> GuardRefinement<'db> {
        let true_members: Vec<Ty<'db>> = members
            .iter()
            .copied()
            .filter(|&member| decide(self.db, member) != Some(false))
            .collect();
        let false_members: Vec<Ty<'db>> = members
            .iter()
            .copied()
            .filter(|&member| decide(self.db, member) != Some(true))
            .collect();
        let edge = |kept: Vec<Ty<'db>>| -> Vec<SlotRefinement<'db>> {
            if kept.is_empty() || kept.len() == members.len() {
                return Vec::new();
            }
            vec![SlotRefinement::Narrow(slot, union_of(self.db, kept))]
        };
        GuardRefinement {
            true_edge: edge(true_members),
            false_edge: edge(false_members),
        }
    }

    /// The local slot `name` in a `switch(typeof(name), ...)` subject.
    fn typeof_subject_slot(&self, subject: ExprId) -> Option<BindingId> {
        let ExpressionKind::Call { callee, arguments } = self.module.kind(subject) else {
            return None;
        };
        let ExpressionKind::NameRef(name) = self.module.kind(*callee) else {
            return None;
        };
        if *name != "typeof" || self.naming.resolutions.contains_key(callee) {
            return None;
        }
        let [argument] = &self.module[arguments][..] else {
            return None;
        };
        let value = argument.value.filter(|_| argument.name.is_none())?;
        if !matches!(self.module.kind(value), ExpressionKind::NameRef(_)) {
            return None;
        }
        self.naming.resolutions.get(&value).copied()
    }

    /// An arm of `switch(typeof(x), ...)` runs only for values of its labels' types. A
    /// variable gets a fresh variable in the arm, so how the arm uses the value (`logical =
    /// if (x) ...`) does not pin the type the other arms see; a union keeps the members a
    /// label can match.
    fn narrow_to_typeof_labels(&mut self, slot: BindingId, labels: &[Symbol]) {
        let Some(EnvEntry::Mono(current)) = self.environment.get(slot) else {
            return;
        };
        let resolved = self.table.resolve(self.db, current);
        if matches!(resolved.kind(self.db), TyKind::Var(_) | TyKind::Rigid(_)) {
            let fresh = self.fresh(Constraint::Unconstrained);
            self.environment.set(slot, EnvEntry::Mono(fresh));
            return;
        }
        let members = guard_members(self.db, resolved);
        let kept: Vec<Ty<'db>> = members
            .iter()
            .copied()
            .filter(|&member| {
                labels
                    .iter()
                    .any(|label| typeof_label_membership(self.db, member, *label) != Some(false))
            })
            .collect();
        if kept.is_empty() || kept.len() == members.len() {
            return;
        }
        let narrowed = union_of(self.db, kept);
        self.environment.set(slot, EnvEntry::Mono(narrowed));
    }

    fn infer_unary(&mut self, id: ExprId, operator: UnaryOperator, operand: ExprId) -> Ty<'db> {
        match operator {
            UnaryOperator::Minus => self.infer_unary_minus(operand),
            // `!!x` and `!!!x` are rlang's injection and splice operators as often as double
            // negation, and the operand of an injection can be any value.
            UnaryOperator::Not
                if matches!(
                    self.module.kind(operand),
                    ExpressionKind::Unary {
                        operator: UnaryOperator::Not,
                        ..
                    }
                ) =>
            {
                let mut innermost = operand;
                while let ExpressionKind::Unary {
                    operator: UnaryOperator::Not,
                    operand,
                } = self.module.kind(innermost)
                {
                    innermost = *operand;
                }
                let inner = self.infer(innermost);
                let inner = self.structural(inner);
                match inner.kind(self.db) {
                    // rlang injects R code, never a ry value, so `!!done` negates twice.
                    TyKind::Native(NativeScalar::Bool) => inner,
                    TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double) => {
                        scalar(self.db, Atomic::Logical)
                    }
                    TyKind::Vector(element)
                        if matches!(
                            element.kind(self.db),
                            TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
                        ) =>
                    {
                        Ty::new(self.db, TyKind::Vector(scalar(self.db, Atomic::Logical)))
                    }
                    _ => {
                        self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                        unknown(self.db)
                    }
                }
            }
            UnaryOperator::Not => self.infer_unary_not(operand),
            UnaryOperator::Tilde => self.infer_formula(&[operand]),
            UnaryOperator::Plus | UnaryOperator::Help => {
                self.infer(operand);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            }
        }
    }

    /// A formula quotes its operands, which name model variables rather than bindings in scope,
    /// so they infer for the IDE but nothing inside is judged.
    fn infer_formula(&mut self, operands: &[ExprId]) -> Ty<'db> {
        let errors_mark = self.errors.len();
        let origins_mark = self.strict_origins.len();
        for &operand in operands {
            self.infer(operand);
        }
        self.truncate_errors(errors_mark);
        self.strict_origins.truncate(origins_mark);
        Ty::new(self.db, TyKind::Named(Name::new(self.db, "formula"), &[]))
    }

    fn infer_unary_minus(&mut self, operand: ExprId) -> Ty<'db> {
        let operand_range = self.blame_range(operand);
        // `-` written before a suffixed literal is part of it, as in Roc, so
        // `-9223372036854775808i64` is checked as the `I64` it denotes rather than as an
        // out-of-range literal negated.
        let inferred = match self.module.kind(operand) {
            ExpressionKind::Literal(literal @ LiteralKind::Suffixed { number, ty }) => {
                let scalar = suffix_scalar(*ty);
                if NumberLiteral::parse(&self.module[number], true)
                    .is_some_and(|value| !scalar.holds(&value))
                {
                    let expected = native_scalar(self.db, scalar);
                    self.errors.push(TypeError {
                        range: operand_range,
                        kind: TypeErrorKind::LiteralOutOfRange { expected },
                    });
                }
                let literal_ty = self.literal_ty(literal);
                self.record(operand, literal_ty)
            }
            _ => self.infer(operand),
        };
        let resolved = self.structural(inferred);
        if let TyKind::Native(native) = resolved.kind(self.db) {
            if !native.is_number() {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::NumberOperands {
                        operator: "-",
                        left: resolved,
                        right: None,
                    },
                });
                return unknown(self.db);
            }
            return resolved;
        }
        // A DataFrame expression negates into a bigger expression, as `Ops.Expr` does at run time.
        if is_expr_type(self.db, resolved) {
            return resolved;
        }
        if let Some(element) = array_element(self.db, resolved) {
            return self.array_unary(
                operand_range,
                resolved,
                element,
                OperandExpectation::Numeric,
            );
        }
        // R rejects `-NULL`, but a computed value that is only sometimes `NULL` is negated where
        // the code has ruled that out, as in `if (length(k)) res <- res[-k]`. A bare `NULL` and
        // the `NULL` a declaration admits stay errors.
        let resolved = match resolved.kind(self.db) {
            TyKind::Union(members)
                if !self.names_declared_value(operand)
                    && members
                        .iter()
                        .any(|member| matches!(member.kind(self.db), TyKind::Null)) =>
            {
                union_of(
                    self.db,
                    members
                        .iter()
                        .copied()
                        .filter(|member| !matches!(member.kind(self.db), TyKind::Null)),
                )
            }
            _ => resolved,
        };
        match self.classify_numeric_operand(resolved) {
            NumericOperand::Concrete(shape, atomic) => self.shaped(shape, atomic),
            NumericOperand::ConcreteUnion(parts) => {
                let members: Vec<Ty<'db>> = parts
                    .into_iter()
                    .map(|(shape, atomic)| self.shaped(shape, atomic))
                    .collect();
                union_of(self.db, members)
            }
            // R has no unary `-` on a `Date`, which only adds a number, so the operand needs
            // the arithmetic a class has with its own values.
            NumericOperand::Flexible(ty) => {
                self.constrain_operand(operand_range, ty, Constraint::Numeric);
                ty
            }
            NumericOperand::FlexibleVector(element) => {
                if let Some(element) = element {
                    self.constrain_operand(operand_range, element, Constraint::Numeric);
                }
                resolved
            }
            NumericOperand::OpenUnion(open) => {
                for member in open {
                    self.constrain_operand(operand_range, member, Constraint::Numeric);
                }
                unknown(self.db)
            }
            NumericOperand::AnyUnknown => unknown(self.db),
            // R stops on `-day`, so a class negates only when its arithmetic takes two of its
            // values, as `difftime`'s does, the same test the flexible operand gets.
            NumericOperand::Invalid
                if self.declares_arithmetic(resolved)
                    && constraint_rejects(self.db, &self.table, Constraint::Numeric, resolved)
                        .is_none() =>
            {
                resolved
            }
            NumericOperand::Invalid => {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::InvalidOperand {
                        expected: OperandExpectation::Numeric,
                        found: resolved,
                    },
                });
                unknown(self.db)
            }
        }
    }

    /// A class with arithmetic methods (`matrix`, `difftime`) negates to itself: R
    /// dispatches unary `-` through the same `-.class`, `Arith.class` and `Ops.class`. A
    /// frame with known columns carries its class (`data.frame`) like a nominal type does.
    fn declares_arithmetic(&self, ty: Ty<'db>) -> bool {
        let class = match ty.kind(self.db) {
            TyKind::Named(name, _) => name.text(self.db),
            TyKind::Frame(kind, _) => match kind.class() {
                Some(class) => class,
                None => return false,
            },
            _ => return false,
        };
        let Some(globals) = self.globals else {
            return false;
        };
        ["-", "Arith", "Ops"].iter().any(|prefix| {
            let method = method_symbol(prefix, class);
            globals
                .overloads(method, false)
                .is_some_and(|candidates| !candidates.is_empty())
                || globals.scheme(method, false).is_some()
        })
    }

    fn negates_to_itself(&self, ty: Ty<'db>) -> bool {
        let TyKind::Named(name, _) = ty.kind(self.db) else {
            return false;
        };
        let class = name.text(self.db);
        if matches!(class, "matrix" | "array") {
            return true;
        }
        let Some(globals) = self.globals else {
            return false;
        };
        ["!", "Ops"].iter().any(|prefix| {
            let method = method_symbol(prefix, class);
            globals
                .overloads(method, false)
                .is_some_and(|candidates| !candidates.is_empty())
                || globals.scheme(method, false).is_some()
        })
    }

    fn infer_unary_not(&mut self, operand: ExprId) -> Ty<'db> {
        let operand_range = self.blame_range(operand);
        let inferred = self.infer(operand);
        let resolved = self.structural(inferred);
        // A DataFrame expression negates into a bigger expression, as `Ops.Expr` does at run time.
        if is_expr_type(self.db, resolved) {
            return resolved;
        }
        if let Some(element) = array_element(self.db, resolved) {
            return self.array_unary(
                operand_range,
                resolved,
                element,
                OperandExpectation::Logical,
            );
        }
        match resolved.kind(self.db) {
            TyKind::Native(NativeScalar::Bool) => return resolved,
            TyKind::Native(_) => {
                let expected = native_scalar(self.db, NativeScalar::Bool);
                let error = self.type_mismatch(operand_range, expected, resolved, Some(operand));
                self.errors.push(error);
                return expected;
            }
            TyKind::Any | TyKind::Unknown => return unknown(self.db),
            TyKind::Var(_) | TyKind::Rigid(_) => {
                if !self.condition_coerces(resolved)
                    && !self.reads_open_defaulted_formal(operand, resolved)
                {
                    self.unify_or_report(operand_range, scalar(self.db, Atomic::Logical), resolved);
                }
                return scalar(self.db, Atomic::Logical);
            }
            _ => {}
        }
        // `!` keeps a matrix's shape (`!as.matrix(flags)` is a logical matrix) and dispatches
        // to a class's `!` or `Ops` method. `Date` has only `-` and `Arith`, so `!Sys.Date()`
        // stays an error.
        if self.negates_to_itself(resolved) {
            return resolved;
        }
        let members: Vec<Ty<'db>> = match resolved.kind(self.db) {
            TyKind::Union(members) => members.to_vec(),
            _ => vec![resolved],
        };
        // `!` coerces a numeric operand as a condition does (`!0` is TRUE), and `!NULL` is an
        // empty logical vector, so a value that may be `NULL` negates without error.
        let mut vector = false;
        for &member in &members {
            match member.kind(self.db) {
                // An unannotated parameter keeps its open variable beside the shape a test gave it
                // (`write` is `T | NULL` under `if (is.null(read)) read <- !write`), and the
                // variable is no evidence of a wrong operand.
                TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
                | TyKind::Var(_) => {}
                TyKind::Rigid(_) if self.condition_coerces(member) => {}
                TyKind::Null => vector = true,
                TyKind::Vector(element) | TyKind::NamedVector(element)
                    if matches!(
                        element.kind(self.db),
                        TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
                            | TyKind::Var(_)
                            | TyKind::Any
                            | TyKind::Unknown
                    ) =>
                {
                    vector = true;
                }
                TyKind::Any | TyKind::Unknown => return unknown(self.db),
                _ => {
                    self.errors.push(TypeError {
                        range: operand_range,
                        kind: TypeErrorKind::InvalidOperand {
                            expected: OperandExpectation::Logical,
                            found: resolved,
                        },
                    });
                    return unknown(self.db);
                }
            }
        }
        if vector {
            Ty::new(self.db, TyKind::Vector(scalar(self.db, Atomic::Logical)))
        } else {
            scalar(self.db, Atomic::Logical)
        }
    }

    /// `-x` keeps an `integer` or `double` Array's type, and `!x` takes only `Array<logical>`.
    fn array_unary(
        &mut self,
        operand_range: TextRange,
        array: Ty<'db>,
        element: Ty<'db>,
        expected: OperandExpectation,
    ) -> Ty<'db> {
        let element = self.table.resolve(self.db, element);
        let fits = match element.kind(self.db) {
            TyKind::Native(NativeScalar::Bool) => expected == OperandExpectation::Logical,
            TyKind::Native(native) => native.is_number() && expected == OperandExpectation::Numeric,
            TyKind::Var(_) | TyKind::Rigid(_) => match self.ry_number_class(element) {
                Some(_) => expected == OperandExpectation::Numeric,
                None => true,
            },
            TyKind::Any | TyKind::Unknown => true,
            _ => false,
        };
        if !fits {
            let array = self.settle_literals(array);
            self.errors.push(TypeError {
                range: operand_range,
                kind: TypeErrorKind::InvalidOperand {
                    expected,
                    found: array,
                },
            });
            return unknown(self.db);
        }
        array
    }

    fn infer_binary(
        &mut self,
        id: ExprId,
        range: TextRange,
        operator: BinaryOperator,
        special_name: Option<&str>,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Ty<'db> {
        use BinaryOperator::*;
        if let Some(foreach) = foreach_loop(self.module, id) {
            let result = self.infer_foreach(id, &foreach);
            return self.record(id, result);
        }
        if operator == Special
            && let Some(name) = special_name
            && let Some(result) = self.infer_declared_operator(range, name, lhs, rhs)
        {
            return self.record(id, result);
        }
        match operator {
            Add | Subtract | Multiply | Modulo | IntegerDivide | Divide | Power | Less
            | Greater | LessEq | GreaterEq | Equal | NotEqual => {
                // Both operands infer exactly once, before any candidate probe: a probe snapshot
                // does not reverse environment writes, and re-inferring makes nested chains
                // exponential.
                self.literal_reads.insert(lhs);
                self.literal_reads.insert(rhs);
                let left = self.infer(lhs);
                let right = self.infer(rhs);
                self.literal_reads.remove(lhs);
                self.literal_reads.remove(rhs);
                // `f + 1` on a `data.frame{...}` dispatches to `Arith.data.frame`.
                let left = self.frame_as_class(left);
                let right = self.frame_as_class(right);
                if let Some(result) = self.infer_native_operator(operator, lhs, rhs, left, right) {
                    return result;
                }
                // Arithmetic on two undecided literals is itself an undecided literal, so
                // `2 * 3.14159 * r` adapts to `r` as a lone literal would. `/` makes it a
                // fraction; `^` and `%/%` are left out because R gives `double` and `integer` for
                // them whatever the operand types.
                if matches!(operator, Add | Subtract | Multiply | Modulo | Divide)
                    && self.open_literal_var(left)
                    && self.open_literal_var(right)
                {
                    let result = if operator == Divide {
                        self.fresh_literal(Constraint::FracLiteral, false)
                    } else {
                        left
                    };
                    if self.table.unify(self.db, left, right).is_ok()
                        && self.table.unify(self.db, left, result).is_ok()
                    {
                        return result;
                    }
                }
                for operand in [left, right] {
                    if self.open_literal_var(operand) {
                        self.default_literal_var(operand, false);
                    }
                }
                if let Some(result) =
                    self.infer_array_operator(range, operator, (lhs, rhs), left, right)
                {
                    return result;
                }
                self.operator_method_depth += 1;
                let method_result =
                    self.operator_method_result(range, operator, lhs, rhs, left, right);
                self.operator_method_depth -= 1;
                method_result.unwrap_or_else(|| {
                    self.ordinary_operator_result(range, operator, lhs, rhs, left, right)
                })
            }
            Sequence => self.infer_colon(lhs, rhs),
            // The right operand runs only on the left's true edge for `&&` (false for `||`), so
            // `is.numeric(x) && x > 0` narrows like the `if` it abbreviates.
            And2 | Or2 => {
                self.expect_scalar_logical(lhs, ConditionSite::Operand);
                // `!is.null(x) && length(x)`: the right operand runs only on the left's true
                // edge (`&&`) or false edge (`||`).
                let guard = self.recognize_guard(lhs).unwrap_or_default();
                let edge = if operator == And2 {
                    guard.true_edge
                } else {
                    guard.false_edge
                };
                let mark = self.environment.mark();
                self.apply_refinements(&edge);
                // `ok || return()` leaves instead of yielding a value, so the operand is not one.
                if self.diverges(rhs) {
                    self.infer(rhs);
                } else {
                    self.expect_scalar_logical(rhs, ConditionSite::Operand);
                }
                // The right operand may not run, so its own assignments join with the state
                // before it; the refinement itself ends with the operator.
                let writes: Vec<(BindingId, Option<EnvEntry<'db>>)> = self
                    .environment
                    .writes_since(mark)
                    .into_iter()
                    .filter(|&(slot, entry)| {
                        !edge.iter().any(|refinement| match (*refinement, entry) {
                            (
                                SlotRefinement::Narrow(narrowed, ty),
                                Some(EnvEntry::Mono(current)),
                            ) => narrowed == slot && ty == current,
                            (
                                SlotRefinement::NarrowScheme(narrowed, index),
                                Some(EnvEntry::Scheme(current)),
                            ) => narrowed == slot && index == current,
                            (
                                SlotRefinement::Missing(missing),
                                Some(EnvEntry::MissingFormal(_)),
                            ) => missing == slot,
                            _ => false,
                        })
                    })
                    .collect();
                self.environment.rollback(mark);
                self.join_writes(&writes, JoinEdge::Branch);
                // Two `Bool` operands give a `Bool`, so typed code keeps its own type, and so
                // does a literal beside one (`TRUE && done`), which takes the `Bool` type. Any
                // other R logical operand gives R's `logical`, as R's operator does.
                let boolean = native_scalar(self.db, NativeScalar::Bool);
                let [left_bool, right_bool] = [lhs, rhs].map(|operand| {
                    let ty = self.recorded_type(operand);
                    self.structural(ty) == boolean || (operand == rhs && self.diverges(rhs))
                });
                let literal = match (left_bool, right_bool) {
                    (true, true) => return boolean,
                    (true, false) => rhs,
                    (false, true) => lhs,
                    (false, false) => return scalar(self.db, Atomic::Logical),
                };
                if literal_value_scalar(self.module, literal) == Some(NativeScalar::Bool)
                    && self.adapt_literal(literal, boolean) == LiteralFit::Fits
                {
                    boolean
                } else {
                    scalar(self.db, Atomic::Logical)
                }
            }
            // Opaque constructs: operands still infer for the IDE, but their diagnostics are
            // discarded because nothing inside the construct is judged. Elementwise `&` and `|`
            // over logical or numeric operands are typed; any other operand (raw, a classed
            // object with its own method) leaves them opaque.
            Tilde => self.infer_formula(&[lhs, rhs]),
            And | Or | Special | Pipe | Help => {
                let errors_mark = self.errors.len();
                let origins_mark = self.strict_origins.len();
                let left = self.infer(lhs);
                let right = self.infer(rhs);
                if matches!(operator, And | Or) {
                    if let Some(result) =
                        self.infer_native_operator(operator, lhs, rhs, left, right)
                    {
                        return result;
                    }
                    if let Some(result) =
                        self.infer_array_operator(range, operator, (lhs, rhs), left, right)
                    {
                        return result;
                    }
                    // An operand whose type is still a variable is not constrained: it may be a
                    // scalar or a vector, which no single type expresses, so the operator stays
                    // opaque for it.
                    match (
                        self.elementwise_logical_operand(left),
                        self.elementwise_logical_operand(right),
                    ) {
                        (Some(LogicalOperand::Scalar), Some(LogicalOperand::Scalar)) => {
                            return scalar(self.db, Atomic::Logical);
                        }
                        (Some(LogicalOperand::Unknown), Some(_))
                        | (Some(_), Some(LogicalOperand::Unknown)) => {
                            // The operand that could not be typed is already an origin.
                            return unknown(self.db);
                        }
                        (Some(LogicalOperand::Any), Some(_))
                        | (Some(_), Some(LogicalOperand::Any)) => {
                            return any(self.db);
                        }
                        (Some(_), Some(_)) => {
                            let logical = scalar(self.db, Atomic::Logical);
                            return Ty::new(self.db, TyKind::Vector(logical));
                        }
                        _ => {}
                    }
                }
                self.truncate_errors(errors_mark);
                self.strict_origins.truncate(origins_mark);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            }
        }
    }

    /// An operator with a ry scalar operand: both operands have one ry type, with no conversion
    /// between number types, and a literal on the other side takes that type. `None` when
    /// neither operand is a ry scalar, so R's own rules apply.
    fn infer_native_operator(
        &mut self,
        operator: BinaryOperator,
        lhs: ExprId,
        rhs: ExprId,
        left: Ty<'db>,
        right: Ty<'db>,
    ) -> Option<Ty<'db>> {
        use BinaryOperator::*;
        let left = self.structural(left);
        let right = self.structural(right);
        // A DataFrame expression beside another one, or beside a ry value that shares R's
        // representation, builds a bigger expression, as `Ops.Expr` does at run time. `I64` is
        // left to the ordinary rule, which rejects it: an R operator cannot read one.
        for (expression, other) in [(left, right), (right, left)] {
            if is_expr_type(self.db, expression)
                && (is_expr_type(self.db, other)
                    || matches!(
                        other.kind(self.db),
                        TyKind::Native(NativeScalar::F64 | NativeScalar::Bool | NativeScalar::Str)
                    ))
            {
                return Some(expression);
            }
        }
        let (known, known_side, other, other_ty) =
            match (self.ry_number_class(left), self.ry_number_class(right)) {
                (Some(class), _) => (class, left, rhs, right),
                (None, Some(class)) => (class, right, lhs, left),
                (None, None) => return None,
            };
        // A `List` beside a scalar applies the operator to each item, a list operator's rule.
        if array_element(self.db, other_ty).is_some() {
            return None;
        }
        let spelling = match operator {
            And => "&",
            Or => "|",
            _ => operator_spelling(operator)?,
        };
        let range = self.module.range(lhs).cover(self.module.range(rhs));
        let arithmetic = matches!(
            operator,
            Add | Subtract | Multiply | Divide | Modulo | IntegerDivide | Power
        );
        if arithmetic && !known.is_number() {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::NumberOperands {
                    operator: spelling,
                    left: known_side,
                    right: None,
                },
            });
            return Some(unknown(self.db));
        }
        // The other operand takes the known type when it is a literal or an open variable.
        let fits = match other_ty.kind(self.db) {
            TyKind::Native(_) | TyKind::Rigid(_) => {
                self.table.unify(self.db, other_ty, known_side).is_ok()
            }
            TyKind::Any | TyKind::Unknown => true,
            TyKind::Var(_) => self.table.unify(self.db, other_ty, known_side).is_ok(),
            _ => match known {
                RyClass::Scalar(_) => match self.adapt_literal(other, known_side) {
                    LiteralFit::Fits => true,
                    LiteralFit::OutOfRange => return Some(unknown(self.db)),
                    LiteralFit::NotLiteral => false,
                },
                // An undecided number meets a literal by deciding it together.
                RyClass::Open(_) => match self.list_item_literal(other) {
                    Some(literal) => self.table.unify(self.db, literal, known_side).is_ok(),
                    None => false,
                },
            },
        };
        if !fits {
            let left = self.settle_literals(left);
            let right = self.settle_literals(right);
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::NumberOperands {
                    operator: spelling,
                    left,
                    right: Some(right),
                },
            });
            return Some(unknown(self.db));
        }
        let boolean = native_scalar(self.db, NativeScalar::Bool);
        match (operator, known) {
            (Add | Subtract | Multiply | Modulo | IntegerDivide | Power, _) => Some(known_side),
            (Divide, RyClass::Scalar(native)) if native.is_integer() => {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::IntegerDivision { ty: known_side },
                });
                Some(unknown(self.db))
            }
            (Divide, RyClass::Scalar(_) | RyClass::Open(Constraint::Frac)) => Some(known_side),
            // `/` decides an undecided number as a fraction.
            (Divide, RyClass::Open(_)) => match known_side.kind(self.db) {
                TyKind::Var(var)
                    if self
                        .table
                        .constrain(self.db, *var, Constraint::Frac)
                        .is_ok() =>
                {
                    Some(known_side)
                }
                _ => {
                    self.errors.push(TypeError {
                        range,
                        kind: TypeErrorKind::IntegerDivision { ty: known_side },
                    });
                    Some(unknown(self.db))
                }
            },
            (Less | Greater | LessEq | GreaterEq, _)
                if known.is_number() || known == RyClass::Scalar(NativeScalar::Str) =>
            {
                Some(boolean)
            }
            (Equal | NotEqual, _) => Some(boolean),
            (And | Or, RyClass::Scalar(NativeScalar::Bool)) => Some(boolean),
            _ => {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::NumberOperands {
                        operator: spelling,
                        left,
                        right: Some(right),
                    },
                });
                Some(unknown(self.db))
            }
        }
    }

    /// What decides a ry operator: a ry scalar, or a number variable that only a ry number type
    /// can fill (an undecided `List` item, or a type parameter bounded by `number`, `int` or
    /// `frac`).
    fn ry_number_class(&self, ty: Ty<'db>) -> Option<RyClass> {
        let constraint = match ty.kind(self.db) {
            TyKind::Native(native) => return Some(RyClass::Scalar(*native)),
            TyKind::Var(_) => self.table.open_constraint(self.db, ty)?,
            TyKind::Rigid(name) => *self.table.rigid_constraints.get(name)?,
            _ => return None,
        };
        matches!(
            constraint,
            Constraint::Number | Constraint::Int | Constraint::Frac
        )
        .then_some(RyClass::Open(constraint))
    }

    /// Gives a literal value the ry type it is checked against: a number literal takes a ry
    /// number type that holds it, a string literal `Str`, `TRUE` and `FALSE` `Bool`. A `List`,
    /// record or tuple literal passes the expectation to its items. The adapted type is
    /// recorded for the literal, so the editor shows the type the literal took.
    fn adapt_literal(&mut self, value: ExprId, expected: Ty<'db>) -> LiteralFit {
        let module = self.module;
        let mut value = value;
        while let ExpressionKind::Paren(inner) = module.kind(value) {
            value = *inner;
        }
        let expected = self.structural(expected);
        let target = match expected.kind(self.db) {
            TyKind::Native(native) => Some(*native),
            _ => None,
        };
        // `2 * 3.14159 * r`: each literal adapts, and `/` needs a fraction type, as it does
        // between ry numbers.
        if let Some(native) = target.filter(|native| native.is_number())
            && let ExpressionKind::Binary {
                operator, lhs, rhs, ..
            } = *module.kind(value)
            && is_literal_arithmetic(module, value)
            && (operator != BinaryOperator::Divide || native.is_fraction())
        {
            for operand in [lhs, rhs] {
                match self.adapt_literal(operand, expected) {
                    LiteralFit::Fits => {}
                    fit => return fit,
                }
            }
            self.record(value, expected);
            return LiteralFit::Fits;
        }
        // `TRUE && !FALSE` and `7 > 2` are made of literals alone, so they are a `Bool` when one
        // is asked for, each number literal in a comparison taking the ry number it is written as.
        if target == Some(NativeScalar::Bool)
            && !matches!(module.kind(value), ExpressionKind::Literal(_))
            && is_literal_logic(module, value)
        {
            if !literal_leaves_fit(module, value, NativeScalar::Bool) {
                return LiteralFit::NotLiteral;
            }
            let fit = match *module.kind(value) {
                ExpressionKind::Unary { operand, .. } => self.adapt_literal(operand, expected),
                ExpressionKind::Binary {
                    operator: BinaryOperator::And2 | BinaryOperator::Or2,
                    lhs,
                    rhs,
                    ..
                } => match self.adapt_literal(lhs, expected) {
                    LiteralFit::Fits => self.adapt_literal(rhs, expected),
                    fit => fit,
                },
                ExpressionKind::Binary { lhs, rhs, .. } => {
                    let number = if is_fractional_arithmetic(module, lhs)
                        || is_fractional_arithmetic(module, rhs)
                    {
                        NativeScalar::F64
                    } else {
                        NativeScalar::I64
                    };
                    let number = native_scalar(self.db, number);
                    match self.adapt_literal(lhs, number) {
                        LiteralFit::Fits => self.adapt_literal(rhs, number),
                        fit => fit,
                    }
                }
                _ => LiteralFit::NotLiteral,
            };
            if matches!(fit, LiteralFit::Fits) {
                self.record(value, expected);
            }
            return fit;
        }
        // `if (c) 0.1 else 3.0` and `{ 3L }` take the value of a branch or of the statement, so
        // each literal there adapts as it would written alone. The `if` was inferred already,
        // condition included, so only its literal leaves remain to adapt.
        if let Some(native) = target
            && literal_value_scalar(module, value).is_some()
            && literal_leaves_fit(module, value, native)
        {
            let leaves = match *module.kind(value) {
                ExpressionKind::If {
                    then_branch,
                    else_branch: Some(else_branch),
                    ..
                } => [Some(then_branch), Some(else_branch)],
                ExpressionKind::Block { statements } => [module[statements].first().copied(), None],
                _ => [None, None],
            };
            if leaves[0].is_some() {
                for leaf in leaves.into_iter().flatten() {
                    match self.adapt_literal(leaf, expected) {
                        LiteralFit::Fits => {}
                        fit => return fit,
                    }
                }
                self.record(value, expected);
                return LiteralFit::Fits;
            }
        }
        // A literal for an undecided `List` item is a ry value, as it is when written in the
        // list itself, and so is a number literal for a type parameter bounded by `number`,
        // `int` or `frac`.
        let open = match target {
            None => self.table.open_constraint(self.db, expected),
            Some(_) => None,
        };
        let adapts = match open {
            Some(Constraint::ArrayItem) => true,
            Some(constraint @ (Constraint::Number | Constraint::Int | Constraint::Frac)) => {
                number_literal(module, value)
                    .is_some_and(|literal| literal_meets(&literal, constraint))
            }
            _ => false,
        };
        // `Num.to_f64(6 + 7L)`: literal arithmetic, or an `if` of literals, for a type parameter
        // bounded by `number` takes the ry number its literals are written as.
        if let Some(constraint @ (Constraint::Number | Constraint::Int | Constraint::Frac)) = open
            && number_literal(module, value).is_none()
            && let Some(scalar @ (NativeScalar::I64 | NativeScalar::F64)) =
                literal_value_scalar(module, value)
        {
            let scalar = match constraint {
                Constraint::Frac => NativeScalar::F64,
                _ => scalar,
            };
            let number = native_scalar(self.db, scalar);
            if self.table.unify(self.db, number, expected).is_ok() {
                return self.adapt_literal(value, number);
            }
        }
        if adapts
            && let Some(literal) = self.list_item_literal(value)
            && self.table.unify(self.db, literal, expected).is_ok()
        {
            return LiteralFit::Fits;
        }
        if self.ry_callee
            && open == Some(Constraint::Unconstrained)
            && let Some(literal) = number_literal(module, value)
        {
            let constraint = if literal.is_fraction() {
                Constraint::FracLiteral
            } else {
                Constraint::WholeLiteral {
                    integer: literal.integer,
                }
            };
            let ty = self.fresh_literal(constraint, false);
            self.record_literal(value, ty);
            if self.table.unify(self.db, ty, expected).is_ok() {
                return LiteralFit::Fits;
            }
        }
        if let Some(target) = target {
            if let Some(literal) = number_literal(module, value) {
                if !target.is_number() {
                    return LiteralFit::NotLiteral;
                }
                let previous = self.recorded_type(value);
                let open = matches!(
                    self.table.shallow_resolve(self.db, previous).kind(self.db),
                    TyKind::Var(_)
                );
                // An open item variable shared with a fraction item (`[1, 2.5]`) refuses an
                // integer type, which is the fraction's mismatch, not this literal's.
                if !target.holds(&literal)
                    || (open && self.table.unify(self.db, previous, expected).is_err())
                {
                    self.errors.push(TypeError {
                        range: self.blame_range(value),
                        kind: TypeErrorKind::LiteralOutOfRange { expected },
                    });
                    return LiteralFit::OutOfRange;
                }
                self.record_literal(value, expected);
                return LiteralFit::Fits;
            }
            let fits = match module.kind(value) {
                ExpressionKind::Literal(LiteralKind::String(_)) => target == NativeScalar::Str,
                ExpressionKind::Literal(LiteralKind::Logical(_)) => target == NativeScalar::Bool,
                _ => false,
            };
            if fits {
                self.record(value, expected);
                return LiteralFit::Fits;
            }
            return LiteralFit::NotLiteral;
        }
        match module.kind(value) {
            ExpressionKind::Array(items) => {
                let Some(element) = array_element(self.db, expected) else {
                    return LiteralFit::NotLiteral;
                };
                let items = module[items].to_vec();
                self.adapt_components(&items, |_| Some(element), expected, value)
            }
            ExpressionKind::Tuple(items) => {
                let TyKind::NativeTuple(expected_items) = *expected.kind(self.db) else {
                    return LiteralFit::NotLiteral;
                };
                let items = module[items].to_vec();
                if items.len() != expected_items.len() {
                    return LiteralFit::NotLiteral;
                }
                self.adapt_components(
                    &items,
                    |index| expected_items.get(index).copied(),
                    expected,
                    value,
                )
            }
            ExpressionKind::Tag {
                name,
                payload: Some(payload),
            } => {
                let (name, payload) = (*name, *payload);
                let name = module.name(name);
                let single = [expected];
                let members: &[Ty<'db>] = match expected.kind(self.db) {
                    TyKind::Union(members) => members,
                    _ => &single,
                };
                let Some(member) = members.iter().copied().find(|member| {
                    matches!(member.kind(self.db), TyKind::Tag(tag, Some(_)) if tag.text(self.db) == name.as_str())
                }) else {
                    return LiteralFit::NotLiteral;
                };
                let TyKind::Tag(_, Some(member_payload)) = member.kind(self.db) else {
                    return LiteralFit::NotLiteral;
                };
                let fit = self.adapt_literal(payload, *member_payload);
                if fit == LiteralFit::Fits {
                    self.record(value, member);
                }
                fit
            }
            ExpressionKind::Record(fields) => {
                let TyKind::NativeRecord(expected_fields) = *expected.kind(self.db) else {
                    return LiteralFit::NotLiteral;
                };
                let fields = module[fields].to_vec();
                if fields.len() != expected_fields.len() {
                    return LiteralFit::NotLiteral;
                }
                let items: Vec<ExprId> = fields.iter().map(|field| field.value).collect();
                let db = self.db;
                self.adapt_components(
                    &items,
                    |index| {
                        let name = fields.get(index)?.name.as_str();
                        expected_fields
                            .iter()
                            .find(|field| field.name.text(db) == name)
                            .map(|field| field.ty)
                    },
                    expected,
                    value,
                )
            }
            _ => LiteralFit::NotLiteral,
        }
    }

    /// Adapts each component of a collection literal to its expected type. The literal fits
    /// when every component fits, adapted or as inferred.
    fn adapt_components(
        &mut self,
        components: &[ExprId],
        expected_component: impl Fn(usize) -> Option<Ty<'db>>,
        expected: Ty<'db>,
        value: ExprId,
    ) -> LiteralFit {
        let mut adapted = false;
        for (index, &component) in components.iter().enumerate() {
            let Some(component_expected) = expected_component(index) else {
                return LiteralFit::NotLiteral;
            };
            match self.adapt_literal(component, component_expected) {
                LiteralFit::Fits => adapted = true,
                LiteralFit::OutOfRange => return LiteralFit::OutOfRange,
                LiteralFit::NotLiteral => {
                    let found = self.recorded_type(component);
                    if !self.table.compatible(self.db, found, component_expected) {
                        return LiteralFit::NotLiteral;
                    }
                }
            }
        }
        if !adapted {
            return LiteralFit::NotLiteral;
        }
        self.record(value, expected);
        LiteralFit::Fits
    }

    /// Records a number literal's adapted type on the literal and on the `-` folded into it.
    fn record_literal(&mut self, value: ExprId, ty: Ty<'db>) {
        self.record(value, ty);
        if let ExpressionKind::Unary { operand, .. } = self.module.kind(value) {
            self.record(*operand, ty);
        }
    }

    /// Elementwise operators on lists, as R's on vectors. Both operands share one item type, with
    /// no conversion between number types, and a single value of that type may stand on either
    /// side, a literal taking the item type. `None` when neither operand is a `List`, so R's own
    /// rules apply.
    fn infer_array_operator(
        &mut self,
        range: TextRange,
        operator: BinaryOperator,
        (lhs, rhs): (ExprId, ExprId),
        left: Ty<'db>,
        right: Ty<'db>,
    ) -> Option<Ty<'db>> {
        use BinaryOperator::*;
        let left = self.structural(left);
        let right = self.structural(right);
        let left_element = array_element(self.db, left);
        let right_element = array_element(self.db, right);
        let known = left_element.or(right_element)?;
        let spelling = match operator {
            And => "&",
            Or => "|",
            _ => operator_spelling(operator)?,
        };
        let reject = |checker: &mut Self| {
            let left = checker.settle_literals(left);
            let right = checker.settle_literals(right);
            checker.errors.push(TypeError {
                range,
                kind: TypeErrorKind::ArrayOperands {
                    operator: spelling,
                    left,
                    right,
                },
            });
            Some(unknown(checker.db))
        };
        let operand_element =
            |checker: &mut Self, value: ExprId, operand: Ty<'db>, element: Option<Ty<'db>>| {
                if let Some(element) = element {
                    return Some(element);
                }
                match operand.kind(checker.db) {
                    TyKind::Native(_) | TyKind::Var(_) | TyKind::Rigid(_) => Some(operand),
                    TyKind::Any | TyKind::Unknown => Some(known),
                    _ => match checker.adapt_literal(value, known) {
                        LiteralFit::Fits | LiteralFit::OutOfRange => Some(known),
                        LiteralFit::NotLiteral => None,
                    },
                }
            };
        let (Some(left_item), Some(right_item)) = (
            operand_element(self, lhs, left, left_element),
            operand_element(self, rhs, right, right_element),
        ) else {
            return reject(self);
        };
        let snapshot = self.table.snapshot();
        if self.table.unify(self.db, left_item, right_item).is_err() {
            self.table.rollback(snapshot);
            return reject(self);
        }
        let mut item = self.table.resolve(self.db, left_item);
        // `/` has an exact quotient only on fractional numbers, so it decides undecided items,
        // such as the literals of `[1, 2] / 2`, as `F64`, the way Roc gives `/` a `Frac`.
        if operator == Divide && matches!(item.kind(self.db), TyKind::Var(_)) {
            let fraction = native_scalar(self.db, NativeScalar::F64);
            let snapshot = self.table.snapshot();
            if self.table.unify(self.db, item, fraction).is_ok() {
                item = fraction;
            } else {
                self.table.rollback(snapshot);
            }
        }
        let item_kind = match item.kind(self.db) {
            TyKind::Native(native) => Some(*native),
            TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => None,
            _ => return reject(self),
        };
        let boolean = native_scalar(self.db, NativeScalar::Bool);
        let result = match operator {
            Add | Subtract | Multiply | Modulo | IntegerDivide | Power | Divide => {
                if item_kind.is_some_and(|native| !native.is_number()) {
                    return reject(self);
                }
                if operator == Divide && item_kind.is_some_and(NativeScalar::is_integer) {
                    self.errors.push(TypeError {
                        range,
                        kind: TypeErrorKind::IntegerDivision { ty: item },
                    });
                    return Some(unknown(self.db));
                }
                item
            }
            Less | Greater | LessEq | GreaterEq => {
                if item_kind
                    .is_some_and(|native| !native.is_number() && native != NativeScalar::Str)
                {
                    return reject(self);
                }
                boolean
            }
            Equal | NotEqual => boolean,
            And | Or => {
                if item_kind.is_some_and(|native| native != NativeScalar::Bool) {
                    return reject(self);
                }
                boolean
            }
            _ => return None,
        };
        Some(array(self.db, result))
    }

    /// The shape of an operand of elementwise `&` or `|`; `None` when it is not a plain logical
    /// or numeric value, or when its type is still a variable.
    fn elementwise_logical_operand(&self, ty: Ty<'db>) -> Option<LogicalOperand> {
        let element_fits = |ty: Ty<'db>| {
            matches!(
                ty.kind(self.db),
                TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
            )
        };
        let resolved = self.table.resolve(self.db, ty);
        match resolved.kind(self.db) {
            TyKind::Any => Some(LogicalOperand::Any),
            TyKind::Unknown => Some(LogicalOperand::Unknown),
            _ if element_fits(resolved) => Some(LogicalOperand::Scalar),
            TyKind::Vector(element) | TyKind::NamedVector(element)
                if element_fits(*element)
                    || matches!(
                        element.kind(self.db),
                        TyKind::Any | TyKind::Unknown | TyKind::Var(_) | TyKind::Rigid(_)
                    ) =>
            {
                Some(LogicalOperand::Vector)
            }
            _ => None,
        }
    }

    /// Only stub-declared `%op%`s are typed as calls: a project's own may quote its
    /// right operand (magrittr's `%>%`), and checking it as a call would reject correct code.
    /// `foreach(g = groups) %do% body`: the body runs once per element with each iterator
    /// bound to an element of its value, and `.combine` folds the body's values. Under `%do%`
    /// the body is a loop in the calling frame; under a parallel backend its writes vanish.
    fn infer_foreach(&mut self, id: ExprId, foreach: &ForeachLoop) -> Ty<'db> {
        let module = self.module;
        let pending_mark = self.pending_enclosing_writes.len();
        let mark = self.environment.mark();
        let mut iterators: SmallVec<[(BindingId, Ty<'db>); 2]> = SmallVec::new();
        for &stage in &foreach.stages {
            self.infer(stage);
            for (_, value) in foreach_iterators(module, stage) {
                let Some(&slot) = self.naming.extras().for_variables.get(&value) else {
                    continue;
                };
                let sequence = self.recorded_type(value);
                let element = if self.refuse_hidden_representation(value, sequence) {
                    unknown(self.db)
                } else {
                    let resolved = self.structural(sequence);
                    // An iterator object (`icount()`, `iter(m, by = "row")`) is not a
                    // vector, and foreach's own iteration decides its element.
                    self.iteration_element(resolved)
                        .unwrap_or_else(|_| unknown(self.db))
                };
                self.environment.set(slot, EnvEntry::Mono(element));
                iterators.push((slot, element));
            }
        }
        let value = if foreach.caller_frame {
            self.check_loop_body(id, None, foreach.body, false, None);
            // doSEQ leaves each iterator at its last element, and unset when there was none.
            for &(slot, element) in &iterators {
                let last = match self.environment.get(slot) {
                    Some(EnvEntry::Mono(ty)) => ty,
                    _ => element,
                };
                let after = union_of(self.db, [last, null(self.db)]);
                self.environment.set(slot, EnvEntry::Mono(after));
            }
            self.recorded_type(foreach.body)
        } else {
            let value = self.infer(foreach.body);
            self.environment.rollback(mark);
            self.reapply_enclosing_writes(pending_mark);
            value
        };
        let mut result = self.settle_literals(value);
        for &stage in foreach.stages.iter().rev() {
            if let ExpressionKind::Call { arguments, .. } = module.kind(stage)
                && foreach_iterators(module, stage).next().is_some()
            {
                result = self.foreach_combined(&module[*arguments], result);
            }
        }
        result
    }

    /// What one `foreach()` stage gives for body values of type `value`: a list without
    /// `.combine`, a vector under `.combine = c` of scalars or vectors, and `Any` for every
    /// other fold.
    fn foreach_combined(&mut self, arguments: &[Argument], value: Ty<'db>) -> Ty<'db> {
        let module = self.module;
        let option = |name: &str| {
            arguments
                .iter()
                .find(|argument| {
                    module
                        .argument_name(argument)
                        .is_some_and(|found| found == name)
                })
                .and_then(|argument| argument.value)
        };
        if option(".final").is_some() || option(".init").is_some() {
            return any(self.db);
        }
        let Some(combine) = option(".combine") else {
            return Ty::new(self.db, TyKind::List(value));
        };
        let concatenates = match module.kind(combine) {
            ExpressionKind::NameRef(name) => {
                name == "c" && !self.naming.resolutions.contains_key(&combine)
            }
            ExpressionKind::Literal(LiteralKind::String(text)) => &module[*text] == "c",
            _ => false,
        };
        let resolved = self.table.resolve(self.db, value);
        match resolved.kind(self.db) {
            TyKind::Scalar(atomic) if concatenates => {
                Ty::new(self.db, TyKind::Vector(scalar(self.db, *atomic)))
            }
            TyKind::Vector(_) if concatenates => resolved,
            _ => any(self.db),
        }
    }

    fn infer_declared_operator(
        &mut self,
        range: TextRange,
        name: &str,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<Ty<'db>> {
        let globals = self.globals?;
        let symbol = Symbol::new(name);
        let in_project = globals.defined_in_project(symbol, false);
        if !in_project && !self.library?.schemes.contains_key(name) {
            return None;
        }
        let scheme = globals.scheme(symbol, false)?;
        let first_var = self.table.next_var().0;
        let instantiated = self.instantiate(&scheme);
        let instantiated_vars = first_var..self.table.next_var().0;
        let resolved = self.table.shallow_resolve(self.db, instantiated);
        let TyKind::Function(function) = resolved.kind(self.db) else {
            return None;
        };
        // A project operator that captures an operand's code (`substitute(rhs)`, as magrittr's
        // `%>%` does) is non-standard evaluation, so its operands are not ordinary arguments.
        if in_project && !function.quoted.is_empty() {
            return None;
        }
        let left = self.infer(lhs);
        let right = self.infer(rhs);
        let arguments = [
            self.operand_argument(lhs, left),
            self.operand_argument(rhs, right),
        ];
        let findings = self.match_arguments(range, function, &arguments);
        self.errors.extend(findings);
        let candidates =
            self.supplied_parameter_variables(resolved, &arguments, instantiated_vars.clone());
        if candidates.is_empty() {
            return Some(function.ret);
        }
        Some(self.drop_unconstrained_members(function.ret, &candidates, instantiated_vars))
    }

    fn operand_argument(&self, value: ExprId, ty: Ty<'db>) -> CallArgument<'db> {
        CallArgument {
            name: None,
            name_range: None,
            ty: Some(ty),
            range: self.blame_range(value),
            value: Some(value),
            whole_double: self.is_whole_double(value),
            forwards_dots: false,
            named_function: None,
        }
    }

    /// An arithmetic or comparison operator on operands no class method claims.
    fn ordinary_operator_result(
        &mut self,
        range: TextRange,
        operator: BinaryOperator,
        lhs: ExprId,
        rhs: ExprId,
        left: Ty<'db>,
        right: Ty<'db>,
    ) -> Ty<'db> {
        use BinaryOperator::*;
        match operator {
            Less | Greater | LessEq | GreaterEq | Equal | NotEqual => {
                self.infer_compare(lhs, rhs, left, right)
            }
            // `/` and `^` always produce doubles.
            _ => self.infer_binary_numeric(range, operator, lhs, rhs, left, right),
        }
    }

    /// The S3 class vector a nominal stands for: the type itself, then each nominal its
    /// representation names, so `data.table` over `data.frame` is `c("data.table",
    /// "data.frame")`. Resolved fully, so an alias of a class is the class it names; empty for
    /// a type that is not nominal.
    fn class_chain(&self, ty: Ty<'db>) -> SmallVec<[Name<'db>; 2]> {
        let mut chain: SmallVec<[Name<'db>; 2]> = SmallVec::new();
        let mut current = self.table.resolve(self.db, ty);
        while let TyKind::Named(name, arguments) = current.kind(self.db) {
            if chain.contains(name) {
                break;
            }
            chain.push(*name);
            let Some(representation) = self.table.representation(self.db, *name, arguments) else {
                break;
            };
            current = self.table.resolve(self.db, representation);
        }
        chain
    }

    /// Dispatches as R does: `+.Date`, then the group generic (`Arith.Date`), then
    /// `Ops.Date`, left operand's class first. `None` when no nominal operand declares
    /// anything, so the ordinary operator rules apply.
    fn operator_method_result(
        &mut self,
        range: TextRange,
        operator: BinaryOperator,
        lhs: ExprId,
        rhs: ExprId,
        left: Ty<'db>,
        right: Ty<'db>,
    ) -> Option<Ty<'db>> {
        let globals = self.globals?;
        let spelling = operator_spelling(operator)?;
        let group = operator_group(operator)?;
        // R computes `NULL == x` as an empty vector, so `truth == "a"` on a `factor | NULL`
        // dispatches through `==.factor` exactly as it would on the factor alone.
        let mut nullable_union =
            |ty: Ty<'db>| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
                TyKind::Union(members)
                    if members
                        .iter()
                        .any(|member| matches!(member.kind(self.db), TyKind::Null)) =>
                {
                    let view = self.operand_view(ty);
                    match view.kind(self.db) {
                        TyKind::Named(..) => view,
                        _ => ty,
                    }
                }
                _ => ty,
            };
        let left = nullable_union(left);
        let right = nullable_union(right);
        // A union beside a class dispatches per member, as R does at run time, so the
        // accumulator `acc <- 0; for (...) acc <- acc + m %*% m` (`double | matrix`) takes
        // `Arith.matrix` for both members. A member no method claims takes the ordinary rules,
        // so `x >= 0` on `matrix | double` is `logical`, but only when some member is a class
        // (a plain union stays with the ordinary rules) and every member types without error.
        for (index, operand) in [left, right].into_iter().enumerate() {
            let TyKind::Union(members) = self.table.shallow_resolve(self.db, operand).kind(self.db)
            else {
                continue;
            };
            let has_class = members
                .iter()
                .any(|member| matches!(member.kind(self.db), TyKind::Named(..)));
            let member_count = members.len();
            let errors_before = self.errors.len();
            let origins_before = self.strict_origins.len();
            let snapshot = self.table.snapshot();
            let mut results = Vec::with_capacity(members.len());
            for &member in members.iter() {
                let (member_left, member_right) = match index {
                    0 => (member, right),
                    _ => (left, member),
                };
                match self.operator_method_result(
                    range,
                    operator,
                    lhs,
                    rhs,
                    member_left,
                    member_right,
                ) {
                    Some(result) => results.push(result),
                    None if has_class && !matches!(member.kind(self.db), TyKind::Named(..)) => {
                        let result = self
                            .infer_array_operator(
                                range,
                                operator,
                                (lhs, rhs),
                                member_left,
                                member_right,
                            )
                            .unwrap_or_else(|| {
                                self.ordinary_operator_result(
                                    range,
                                    operator,
                                    lhs,
                                    rhs,
                                    member_left,
                                    member_right,
                                )
                            });
                        if self.errors.len() > errors_before {
                            break;
                        }
                        results.push(result);
                    }
                    None => break,
                }
            }
            if results.len() == member_count {
                return Some(union_of(self.db, results));
            }
            self.truncate_errors(errors_before);
            self.strict_origins.truncate(origins_before);
            self.table.rollback(snapshot);
        }
        // A generic bounded `numeric` probes as the `double` it may be.
        let probe_operand =
            |ty: Ty<'db>| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
                TyKind::Rigid(name)
                    if self
                        .table
                        .rigid_satisfies(self.db, *name, Constraint::Arithmetic) =>
                {
                    scalar(self.db, Atomic::Double)
                }
                _ => ty,
            };
        let arguments = [
            self.operand_argument(lhs, probe_operand(left)),
            self.operand_argument(rhs, probe_operand(right)),
        ];
        // Each operand's whole class vector, left first: a `TradeDate` over `Date` takes
        // `+.Date` when it declares no `+` of its own.
        let mut classes: Vec<String> = Vec::new();
        for operand in [left, right] {
            for class in self.class_chain(operand) {
                let class = class.text(self.db);
                if !classes.iter().any(|known| known == class) {
                    classes.push(class.to_owned());
                }
            }
        }
        // An untyped operand (`omega + v %*% w` on a parameter) must not take the first
        // candidate's class, which would reject the plain vector R accepts there. It is
        // only required to be numeric, and the call types when every fit agrees.
        let flexible: Vec<(TextRange, Ty<'db>)> = [(lhs, left), (rhs, right)]
            .into_iter()
            .map(|(operand, ty)| {
                (
                    self.blame_range(operand),
                    self.table.shallow_resolve(self.db, ty),
                )
            })
            .filter(|(_, ty)| matches!(ty.kind(self.db), TyKind::Var(_)))
            .collect();
        // An untyped operand may itself be of another class whose method takes the typed one:
        // `start + days(1)` is a `Date` when `start` is, through `+.Date`, not only the
        // `Period` that `+.Period` gives. So every class with arithmetic methods is a fit.
        let own_classes = classes.len();
        if !flexible.is_empty() && !classes.is_empty() {
            let mut others: Vec<Symbol> = globals
                .arithmetic_classes()
                .iter()
                .filter(|class| !classes.iter().any(|known| known == class))
                .collect();
            others.sort();
            classes.extend(others.into_iter().map(|class| class.as_str().to_owned()));
        }
        let mut fitting_returns: Vec<Ty<'db>> = Vec::new();
        let mut fits_another_class = false;
        let mut declared_any = false;
        let mut declared_own = false;
        for (class_index, class) in classes.iter().enumerate() {
            for method in [
                method_symbol(spelling, class),
                method_symbol(group, class),
                method_symbol("Ops", class),
            ] {
                let Some(candidates) = globals
                    .overloads(method, false)
                    .map(Cow::Borrowed)
                    .or_else(|| {
                        globals
                            .scheme(method, false)
                            .map(|scheme| Cow::Owned(vec![scheme]))
                    })
                    .filter(|candidates| !candidates.is_empty())
                else {
                    continue;
                };
                declared_any = true;
                declared_own |= class_index < own_classes;
                for scheme in candidates.iter() {
                    let snapshot = self.table.snapshot();
                    let instantiated = self.instantiate(scheme);
                    let resolved = self.table.shallow_resolve(self.db, instantiated);
                    let TyKind::Function(function) = resolved.kind(self.db) else {
                        self.table.rollback(snapshot);
                        continue;
                    };
                    // A method of another class that takes anything at all, like
                    // `Arith.data.frame`, says nothing about what the untyped operand is.
                    if class_index >= own_classes
                        && function
                            .positional
                            .iter()
                            .copied()
                            .chain(function.named.iter().map(|parameter| parameter.ty))
                            .any(|ty| {
                                matches!(
                                    self.table.shallow_resolve(self.db, ty).kind(self.db),
                                    TyKind::Any
                                )
                            })
                    {
                        self.table.rollback(snapshot);
                        continue;
                    }
                    if self.match_arguments(range, function, &arguments).is_empty() {
                        if flexible.is_empty() {
                            return Some(function.ret);
                        }
                        let ret = self.table.resolve(self.db, function.ret);
                        self.table.rollback(snapshot);
                        fitting_returns.push(ret);
                        fits_another_class |= class_index >= own_classes;
                        continue;
                    }
                    self.table.rollback(snapshot);
                }
            }
        }
        if let Some(&first) = fitting_returns.first() {
            // An operand that may be of another class is not narrowed to a number.
            if !fits_another_class {
                for (operand_range, ty) in flexible {
                    self.constrain_numeric_flexible(operand_range, ty);
                }
            }
            return Some(if fitting_returns.iter().all(|&ret| ret == first) {
                first
            } else {
                unknown(self.db)
            });
        }
        // A class that declares the operator but fits no candidate is a real contract
        // mismatch (`Date + Date` errors in R too). Only other classes' methods were tried
        // for an untyped operand, and when the project registers a method on a class no
        // type tracks (an S7 `method(`+`, ...)`), the operand may be of that class.
        if declared_any && !declared_own && globals.has_untracked_arithmetic_class() {
            return Some(unknown(self.db));
        }
        if declared_any {
            let left = self.table.resolve(self.db, left);
            let right = self.table.resolve(self.db, right);
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::UnsupportedOperandPair {
                    operator: spelling,
                    left,
                    right,
                },
            });
            return Some(unknown(self.db));
        }
        None
    }

    fn blame_range(&self, mut id: ExprId) -> TextRange {
        while let ExpressionKind::Paren(inner) = self.module.kind(id) {
            id = *inner;
        }
        self.module.range(id)
    }

    fn expect_scalar_logical(&mut self, condition: ExprId, site: ConditionSite) {
        let condition_range = self.blame_range(condition);
        let inferred = self.infer(condition);
        let resolved = self.structural(inferred);
        if (self.condition_coerces(resolved) && !self.is_long_combine(condition))
            || self.reads_open_defaulted_formal(condition, resolved)
            || self.reads_null_placeholder(condition, resolved)
            || self.tests_a_shipped_function(condition, resolved)
        {
            return;
        }
        // `if` reads a string through `as.logical`, so `if (Sys.getenv("FLAG", unset = "TRUE"))`
        // is ordinary R. Only a literal shows its text, and then only R's logical spellings pass.
        // `&&`, `||` and a match guard reject any string, so they skip this.
        if site == ConditionSite::IfOrWhile
            && matches!(resolved.kind(self.db), TyKind::Scalar(Atomic::Character))
            && self.reads_as_logical_text(condition)
        {
            return;
        }
        // A `NULL` member is the "not supplied" path of an optional value, and R code tests such
        // a value once it holds one, so a union is a condition when its other members are. Its
        // open members (a parameter rebound on one path) are bound to `logical` as a lone open
        // value would be.
        if let TyKind::Union(members) = resolved.kind(self.db) {
            let values: Vec<_> = members
                .iter()
                .copied()
                .filter(|&member| !matches!(member.kind(self.db), TyKind::Null))
                .collect();
            // A `character` member is read through `as.logical` as a lone string is.
            let reads_text = |member: Ty<'db>| {
                site == ConditionSite::IfOrWhile
                    && matches!(member.kind(self.db), TyKind::Scalar(Atomic::Character))
            };
            if !values.is_empty()
                && values.iter().all(|&member| {
                    self.condition_coerces(member)
                        || reads_text(member)
                        || self.is_open_parameter(member)
                })
            {
                for member in values {
                    if !self.condition_coerces(member) && !reads_text(member) {
                        self.unify_or_report(
                            condition_range,
                            scalar(self.db, Atomic::Logical),
                            member,
                        );
                    }
                }
                return;
            }
        }
        self.unify_or_report(condition_range, scalar(self.db, Atomic::Logical), resolved);
    }

    /// Whether a string value may read as a logical through `as.logical`: only a literal shows
    /// its text, and then only R's logical spellings do.
    fn reads_as_logical_text(&self, value: ExprId) -> bool {
        let mut value = value;
        while let ExpressionKind::Paren(inner) = self.module.kind(value) {
            value = *inner;
        }
        match self.module.kind(value) {
            ExpressionKind::Literal(LiteralKind::String(text)) => matches!(
                &self.module[text],
                "TRUE" | "true" | "True" | "T" | "FALSE" | "false" | "False" | "F"
            ),
            _ => true,
        }
    }

    /// `c(a, b, ...)` with two or more elements is longer than one whatever its elements are,
    /// which is the one case where a vector condition's length is known.
    fn is_long_combine(&self, id: ExprId) -> bool {
        let id = match self.module.kind(id) {
            ExpressionKind::Paren(inner) => *inner,
            _ => id,
        };
        let ExpressionKind::Call { callee, arguments } = self.module.kind(id) else {
            return false;
        };
        matches!(self.module.kind(*callee), ExpressionKind::NameRef(name) if name == "c")
            && self.module[arguments]
                .iter()
                .filter(|argument| argument.value.is_some())
                .count()
                >= 2
    }

    /// An option flag such as `function(x, which = FALSE) if (which) ...` is called with
    /// `1` or `0` as often as with a logical, and R's condition coerces them, so testing a
    /// defaulted formal leaves its type to its other uses rather than pinning `logical`.
    fn reads_open_defaulted_formal(&self, operand: ExprId, resolved: Ty<'db>) -> bool {
        let mut operand = operand;
        while let ExpressionKind::Paren(inner) = self.module.kind(operand) {
            operand = *inner;
        }
        self.is_open_parameter(resolved)
            && self
                .naming
                .resolutions
                .get(&operand)
                .is_some_and(|slot| self.defaulted_formals.contains(*slot))
    }

    /// An unconstrained variable, or an unconstrained rigid so that a rendered `<T>` scheme
    /// checks the same way when written back as an annotation.
    fn is_open_parameter(&self, resolved: Ty<'db>) -> bool {
        match resolved.kind(self.db) {
            TyKind::Var(var) => matches!(
                self.table.entry(*var),
                Entry::Unbound {
                    constraint: Constraint::Unconstrained,
                    ..
                }
            ),
            TyKind::Rigid(name) => self
                .table
                .rigid_constraints
                .get(name)
                .is_none_or(|constraint| *constraint == Constraint::Unconstrained),
            _ => false,
        }
    }

    /// A numeric condition is ordinary R (`if (length(x))`); character, complex, raw,
    /// and map-like conditions keep their error as in R. A flexible condition is left
    /// to unification, which binds it to `logical`.
    fn condition_coerces(&self, ty: Ty<'db>) -> bool {
        match ty.kind(self.db) {
            TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
            | TyKind::Native(NativeScalar::Bool) => true,
            // A vector type carries no length, and the vectors code tests (`if (x >= 1)` on a
            // `double[]`) are length one at run time far more often than not.
            TyKind::Vector(element) => self.condition_coerces(*element),
            TyKind::Union(members) => members.iter().all(|&member| self.condition_coerces(member)),
            // A value already required to be numeric (`if (n %% 2L)`) coerces like a number.
            TyKind::Var(_) => matches!(
                self.table.open_constraint(self.db, ty),
                Some(Constraint::Numeric | Constraint::Arithmetic | Constraint::ScalarNumeric)
            ),
            TyKind::Rigid(name) => {
                self.table
                    .rigid_satisfies(self.db, *name, Constraint::Arithmetic)
            }
            _ => false,
        }
    }

    fn infer_binary_numeric(
        &mut self,
        range: TextRange,
        operator: BinaryOperator,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: Ty<'db>,
        rhs_ty: Ty<'db>,
    ) -> Ty<'db> {
        // `/` and `^` always produce doubles.
        let always_double = matches!(operator, BinaryOperator::Divide | BinaryOperator::Power);
        let lhs_range = self.blame_range(lhs);
        let rhs_range = self.blame_range(rhs);
        let resolved_left = self.operand_view(lhs_ty);
        let resolved_right = self.operand_view(rhs_ty);
        // A list or environment operand is an error in R unless it carries a class with an
        // operator method (ggplot2's `+.gg` on a ggproto environment), and a chain `s + a + b`
        // passes the method's result on, so either kind of operand leaves the expression to
        // that method.
        let may_dispatch = |ty: Ty<'db>| {
            matches!(
                ty.kind(self.db),
                TyKind::List(_)
                    | TyKind::NamedList(_)
                    | TyKind::Tuple(_)
                    | TyKind::Record(_)
                    | TyKind::Environment(_)
                    | TyKind::Unknown
            )
        };
        if (may_dispatch(resolved_left) || may_dispatch(resolved_right))
            && self
                .globals
                .is_some_and(|globals| globals.has_untracked_arithmetic_class())
        {
            return unknown(self.db);
        }
        // An operand of unknown type may be a classed object whose method takes the other
        // operand as it is, as ggplot2's `+` takes a list of components, so a list beside it
        // is no evidence of an error.
        let opaque = |ty: Ty<'db>| matches!(ty.kind(self.db), TyKind::Unknown | TyKind::Any);
        let list_like = |ty: Ty<'db>| {
            guard_members(self.db, ty).iter().all(|member| {
                matches!(
                    member.kind(self.db),
                    TyKind::List(_)
                        | TyKind::NamedList(_)
                        | TyKind::Tuple(_)
                        | TyKind::Record(_)
                        | TyKind::Null
                )
            })
        };
        if (opaque(resolved_left) && list_like(resolved_right))
            || (opaque(resolved_right) && list_like(resolved_left))
        {
            return unknown(self.db);
        }
        let left = self.classify_numeric_operand(resolved_left);
        let right = self.classify_numeric_operand(resolved_right);

        for (operand, resolved, operand_range) in [
            (&left, resolved_left, lhs_range),
            (&right, resolved_right, rhs_range),
        ] {
            if matches!(operand, NumericOperand::Invalid) {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::InvalidOperand {
                        expected: OperandExpectation::Numeric,
                        found: resolved,
                    },
                });
                return unknown(self.db);
            }
        }
        if matches!(left, NumericOperand::AnyUnknown) || matches!(right, NumericOperand::AnyUnknown)
        {
            return unknown(self.db);
        }
        if matches!(left, NumericOperand::OpenUnion(_))
            || matches!(right, NumericOperand::OpenUnion(_))
        {
            for operand in [&left, &right] {
                match operand {
                    NumericOperand::OpenUnion(open) => {
                        for &member in open {
                            self.constrain_numeric_flexible(range, member);
                        }
                    }
                    NumericOperand::Flexible(ty) => self.constrain_numeric_flexible(range, *ty),
                    NumericOperand::FlexibleVector(Some(element)) => {
                        self.constrain_numeric_flexible(range, *element)
                    }
                    _ => {}
                }
            }
            return unknown(self.db);
        }

        let mut flexible: Option<Ty<'db>> = None;
        let mut tied = false;
        for operand in [&left, &right] {
            if let NumericOperand::Flexible(ty) = operand {
                match flexible {
                    // R recycles the shorter operand, so a scalar bound on one side (`2:r`
                    // makes `r` scalar) says nothing of the other: ks's `x / sigma^r` keeps `x`
                    // and `sigma` open to vectors. Neither side fixes the result's atomic type
                    // (`1L + 2.5` is `double`), so it claims `double`, R's widest numeric.
                    Some(existing)
                        if self.scalar_bound(existing) != self.scalar_bound(*ty)
                            && [existing, *ty].into_iter().all(|side| {
                                matches!(
                                    self.table.variable_constraint(self.db, side),
                                    None | Some(
                                        Constraint::Numeric
                                            | Constraint::Arithmetic
                                            | Constraint::ScalarNumeric
                                    )
                                )
                            }) =>
                    {
                        self.constrain_numeric_flexible(range, existing);
                        self.constrain_numeric_flexible(range, *ty);
                        return scalar(self.db, Atomic::Double);
                    }
                    // Two distinct declared parameters (`a / b` under `<T: numeric, U: numeric>`)
                    // are different types by contract, so neither fixes the other, and the
                    // result claims `double` as above.
                    Some(existing) if self.distinct_rigids(existing, *ty) => {
                        self.constrain_numeric_flexible(range, existing);
                        self.constrain_numeric_flexible(range, *ty);
                        return scalar(self.db, Atomic::Double);
                    }
                    Some(existing) => {
                        if let Err(error) = self.table.unify(self.db, existing, *ty) {
                            self.report_unify(range, error);
                            return unknown(self.db);
                        }
                        tied = true;
                    }
                    None => flexible = Some(*ty),
                }
            }
        }
        if let Some(ty) = flexible {
            // R subtracts two dates or two times (their difference is a `difftime`), so `a - b`
            // asks no more of a class than `a + 1` does, while `a + b` asks `numeric`.
            let tied_difference = tied && operator == BinaryOperator::Subtract;
            let constraint = match tied && !tied_difference {
                true => Constraint::Numeric,
                false => Constraint::Arithmetic,
            };
            self.constrain_operand(range, ty, constraint);
            // The difference is not `T` itself: two dates subtract to a `difftime`. It claims
            // `double`, the representation every arithmetic class shares.
            if tied_difference {
                return scalar(self.db, Atomic::Double);
            }
        }
        // Combined with its existing atomic-element bound, an arithmetic `T[]` element
        // becomes scalar-numeric.
        for operand in [&left, &right] {
            if let NumericOperand::FlexibleVector(Some(element)) = operand {
                self.constrain_numeric_flexible(range, *element);
            }
        }

        // A flexible-element vector fixes the result shape but not the atomic: a `double`
        // (or union) partner promotes to `double[]`; an integer partner promotes into the element.
        let flexible_vector_present = matches!(left, NumericOperand::FlexibleVector(_))
            || matches!(right, NumericOperand::FlexibleVector(_));
        if flexible_vector_present {
            let double_vector = Ty::new(self.db, TyKind::Vector(scalar(self.db, Atomic::Double)));
            if always_double {
                return double_vector;
            }
            let concrete_parts = left.concrete_parts().or_else(|| right.concrete_parts());
            if let Some(parts) = &concrete_parts
                && (parts.len() > 1 || parts.iter().any(|(_, atomic)| *atomic == Atomic::Double))
            {
                return double_vector;
            }
            let element = match (&left, &right) {
                (
                    NumericOperand::FlexibleVector(Some(left_element)),
                    NumericOperand::FlexibleVector(Some(right_element)),
                ) => {
                    if let Err(error) = self.table.unify(self.db, *left_element, *right_element) {
                        self.report_unify(range, error);
                        return unknown(self.db);
                    }
                    Some(*left_element)
                }
                (NumericOperand::FlexibleVector(None), _)
                | (_, NumericOperand::FlexibleVector(None)) => None,
                (NumericOperand::FlexibleVector(Some(element)), _)
                | (_, NumericOperand::FlexibleVector(Some(element))) => Some(*element),
                _ => None,
            };
            return Ty::new(
                self.db,
                TyKind::Vector(element.unwrap_or_else(|| unknown(self.db))),
            );
        }

        match (left.concrete_parts(), right.concrete_parts()) {
            (Some(left_parts), Some(right_parts)) => {
                let members =
                    self.member_wise_numeric_results(&left_parts, &right_parts, always_double);
                union_of(self.db, members)
            }
            (left_parts, right_parts) => {
                let Some(flexible) = flexible else {
                    return unknown(self.db);
                };
                let concrete_parts = left_parts.or(right_parts);
                if let Some(parts) = &concrete_parts
                    && parts.len() > 1
                {
                    // A union cannot promote into a variable member-wise, so it pins to `double`.
                    self.unify_or_report(range, flexible, scalar(self.db, Atomic::Double));
                    let members = self.member_wise_numeric_results(
                        &[(OperandShape::Scalar, Atomic::Double)],
                        parts,
                        always_double,
                    );
                    return union_of(self.db, members);
                }
                let concrete = concrete_parts.and_then(|parts| parts.first().copied());
                let result_shape = match concrete {
                    Some((OperandShape::Vector, _)) => OperandShape::Vector,
                    _ => OperandShape::Scalar,
                };
                if always_double || concrete.map(|(_, atomic)| atomic) == Some(Atomic::Double) {
                    return self.shaped(result_shape, Atomic::Double);
                }
                match result_shape {
                    // `x + 1L` stays polymorphic: integer promotes into whatever `x` resolves to.
                    OperandShape::Scalar => flexible,
                    // A vector result cannot carry an unresolved atomic, so it claims `double`,
                    // R's widest numeric. The flexible operand stays open: it may itself be a
                    // vector, which R recycles against its partner.
                    OperandShape::Vector => self.shaped(OperandShape::Vector, Atomic::Double),
                }
            }
        }
    }

    /// Whole-number endpoints give an integer sequence; a `double` or flexible endpoint
    /// gives `double[]`. Endpoints must be scalar: R only warns about and truncates vectors.
    fn infer_colon(&mut self, lhs: ExprId, rhs: ExprId) -> Ty<'db> {
        let mut result_atomic = Atomic::Integer;
        for operand in [lhs, rhs] {
            let operand_range = self.blame_range(operand);
            let whole_literal = self.is_whole_double(operand);
            let inferred = self.infer(operand);
            let resolved = self.structural(inferred);
            // R takes a logical endpoint as a number and a vector endpoint by its first element; a
            // vector type carries no length, so only a literal `c()` of several elements is known
            // to be too long. A list field that is only sometimes `NULL` is read where the code has
            // ruled that out (mgcv reads `b$off[i]:...` only after testing the flag that fills
            // `off`). A local, an element read straight from a value, and a field of a parameter,
            // whose `NULL` the declaration admits, stay checked.
            let field_read = {
                let mut expression = operand;
                let mut through_field = false;
                loop {
                    match &self.module.expression(expression).kind {
                        ExpressionKind::Field {
                            at: false, target, ..
                        } => {
                            through_field = true;
                            expression = *target;
                        }
                        ExpressionKind::Index { target, .. } => expression = *target,
                        _ => break,
                    }
                }
                through_field && !self.binds_parameter(expression)
            };
            let members: Vec<Ty<'db>> = match resolved.kind(self.db) {
                TyKind::Union(members) if field_read => members
                    .iter()
                    .copied()
                    .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
                    .collect(),
                TyKind::Union(members) => members.to_vec(),
                _ => vec![resolved],
            };
            let atomics: Option<Vec<Atomic>> = members
                .iter()
                .map(|member| match member.kind(self.db) {
                    TyKind::Scalar(atomic) => Some(*atomic),
                    TyKind::Vector(element) => match element.kind(self.db) {
                        TyKind::Scalar(atomic) => Some(*atomic),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            if let Some(atomics) = atomics
                && !atomics.is_empty()
                && !self.is_long_combine(operand)
                && atomics.iter().all(|atomic| {
                    matches!(atomic, Atomic::Logical | Atomic::Integer | Atomic::Double)
                })
            {
                if atomics.contains(&Atomic::Double) && !whole_literal {
                    result_atomic = Atomic::Double;
                }
                continue;
            }
            // A local written on one path over an enclosing parameter, `integer | T`, needs a
            // number from its open member as a lone `T` would.
            if members.len() > 1
                && members.iter().any(|member| member.is_var())
                && members.iter().all(|member| {
                    member.is_var()
                        || numeric_operand_parts(self.db, *member)
                            .is_some_and(|(_, atomic)| atomic != Atomic::Complex)
                })
            {
                for member in &members {
                    if let TyKind::Var(var) = member.kind(self.db)
                        && let Err(error) =
                            self.table
                                .constrain(self.db, *var, Constraint::ScalarNumeric)
                    {
                        self.report_unify(operand_range, error);
                        return unknown(self.db);
                    }
                }
                result_atomic = Atomic::Double;
                continue;
            }
            match resolved.kind(self.db) {
                TyKind::Any | TyKind::Unknown => return unknown(self.db),
                TyKind::Var(var) => {
                    if let Err(error) =
                        self.table
                            .constrain(self.db, *var, Constraint::ScalarNumeric)
                    {
                        self.report_unify(operand_range, error);
                        return unknown(self.db);
                    }
                    result_atomic = Atomic::Double;
                }
                TyKind::Rigid(name)
                    if self
                        .table
                        .rigid_satisfies(self.db, *name, Constraint::ScalarNumeric) =>
                {
                    result_atomic = Atomic::Double;
                }
                _ => {
                    self.errors.push(TypeError {
                        range: operand_range,
                        kind: TypeErrorKind::InvalidOperand {
                            expected: OperandExpectation::ScalarNumeric,
                            found: resolved,
                        },
                    });
                    return unknown(self.db);
                }
            }
        }
        Ty::new(self.db, TyKind::Vector(scalar(self.db, result_atomic)))
    }

    /// An operand as arithmetic and comparison see it. R computes `NULL + 1` and `NULL == 1`
    /// as empty vectors rather than failing, so a `NULL` member drops out. A bare `NULL`, and a
    /// union of several members one of which is still a type variable, are left unchecked.
    fn operand_view(&mut self, ty: Ty<'db>) -> Ty<'db> {
        let resolved = self.structural(ty);
        match resolved.kind(self.db) {
            TyKind::Null => unknown(self.db),
            TyKind::Union(members) => {
                let kept: Vec<Ty<'db>> = members
                    .iter()
                    .copied()
                    .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
                    .collect();
                if let [only] = kept.as_slice() {
                    return *only;
                }
                if kept.is_empty()
                    || kept.iter().any(|member| {
                        matches!(member.kind(self.db), TyKind::Var(_) | TyKind::Rigid(_))
                    })
                {
                    return unknown(self.db);
                }
                union_of(self.db, kept)
            }
            _ => resolved,
        }
    }

    fn infer_compare(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: Ty<'db>,
        rhs_ty: Ty<'db>,
    ) -> Ty<'db> {
        let lhs_range = self.blame_range(lhs);
        let rhs_range = self.blame_range(rhs);
        let resolved_left = self.operand_view(lhs_ty);
        let resolved_right = self.operand_view(rhs_ty);
        if matches!(resolved_left.kind(self.db), TyKind::Any | TyKind::Unknown)
            || matches!(resolved_right.kind(self.db), TyKind::Any | TyKind::Unknown)
        {
            return unknown(self.db);
        }
        // R deparses a language object to a string before comparing it, so ape's
        // `formula(object) == ~1` is a plain test.
        let language = |ty: Ty<'db>| matches!(ty.kind(self.db), TyKind::Named(name, _) if name.text(self.db) == "formula");
        if language(resolved_left) || language(resolved_right) {
            return scalar(self.db, Atomic::Logical);
        }

        let is_list = |ty: Ty<'db>| {
            matches!(
                ty.kind(self.db),
                TyKind::List(_) | TyKind::NamedList(_) | TyKind::Tuple(_) | TyKind::Record(_)
            )
        };
        let both_lists = is_list(resolved_left) && is_list(resolved_right);
        let mut operand_shapes = Vec::with_capacity(2);
        for (resolved, operand_range) in [(resolved_left, lhs_range), (resolved_right, rhs_range)] {
            let shapes = comparison_operand_shapes(self.db, resolved).filter(|_| !both_lists);
            let Some(shapes) = shapes else {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::InvalidOperand {
                        expected: OperandExpectation::Comparable,
                        found: resolved,
                    },
                });
                return unknown(self.db);
            };
            operand_shapes.push(shapes);
        }
        let mut results = Vec::new();
        if let [left_shapes, right_shapes] = operand_shapes.as_slice() {
            for left_shape in left_shapes {
                for right_shape in right_shapes {
                    let result_shape = if *left_shape == OperandShape::Vector
                        || *right_shape == OperandShape::Vector
                    {
                        OperandShape::Vector
                    } else {
                        OperandShape::Scalar
                    };
                    results.push(self.shaped(result_shape, Atomic::Logical));
                }
            }
        }
        union_of(self.db, results)
    }

    fn shaped(&self, shape: OperandShape, atomic: Atomic) -> Ty<'db> {
        let element = scalar(self.db, atomic);
        match shape {
            OperandShape::Scalar => element,
            OperandShape::Vector => Ty::new(self.db, TyKind::Vector(element)),
        }
    }

    fn member_wise_numeric_results(
        &self,
        left_parts: &[(OperandShape, Atomic)],
        right_parts: &[(OperandShape, Atomic)],
        always_double: bool,
    ) -> Vec<Ty<'db>> {
        let mut results = Vec::with_capacity(left_parts.len() * right_parts.len());
        for (left_shape, left_atomic) in left_parts {
            for (right_shape, right_atomic) in right_parts {
                let shape = if *left_shape == OperandShape::Vector
                    || *right_shape == OperandShape::Vector
                {
                    OperandShape::Vector
                } else {
                    OperandShape::Scalar
                };
                let atomic = if *left_atomic == Atomic::Complex || *right_atomic == Atomic::Complex
                {
                    Atomic::Complex
                } else if always_double {
                    Atomic::Double
                } else if *left_atomic == Atomic::Integer && *right_atomic == Atomic::Integer {
                    Atomic::Integer
                } else {
                    Atomic::Double
                };
                results.push(self.shaped(shape, atomic));
            }
        }
        results
    }

    fn constrain_numeric_flexible(&mut self, range: TextRange, ty: Ty<'db>) {
        self.constrain_operand(range, ty, Constraint::Arithmetic);
    }

    /// Two operands of one type (`a + b` on `a: T, b: T`) need arithmetic between two of its
    /// values, which `Date` lacks although `Date + 1` is legal. A rigid satisfies a constraint
    /// only when its declaration promised it.
    fn constrain_operand(&mut self, range: TextRange, ty: Ty<'db>, constraint: Constraint) {
        match ty.kind(self.db) {
            TyKind::Var(var) => {
                if let Err(error) = self.table.constrain(self.db, *var, constraint) {
                    self.report_unify(range, error);
                }
            }
            TyKind::Rigid(name) if !self.table.rigid_satisfies(self.db, *name, constraint) => {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::ConstraintViolation {
                        constraint,
                        found: ty,
                    },
                });
            }
            _ => {}
        }
    }

    fn classify_numeric_operand(&self, resolved: Ty<'db>) -> NumericOperand<'db> {
        if let Some(parts) = numeric_operand_parts(self.db, resolved) {
            return NumericOperand::Concrete(parts.0, parts.1);
        }
        match resolved.kind(self.db) {
            TyKind::Union(members) => {
                let mut parts = Vec::with_capacity(members.len());
                let mut open = Vec::new();
                let mut dispatches = false;
                // R rejects arithmetic on an unclassed list, so beside a numeric member a list
                // member is a path that a correlated test rules out, as in mgcv's
                // `offset <- if (nlp > 1) as.list(...) else 0` read as a number only when
                // `nlp == 1`.
                let numeric_beside = members
                    .iter()
                    .any(|&member| numeric_operand_parts(self.db, member).is_some());
                for &member in members.iter() {
                    if let Some(part) = numeric_operand_parts(self.db, member) {
                        parts.push(part);
                        continue;
                    }
                    if numeric_beside
                        && matches!(
                            member.kind(self.db),
                            TyKind::List(_)
                                | TyKind::NamedList(_)
                                | TyKind::Tuple(_)
                                | TyKind::Record(_)
                        )
                        && !self.declares_arithmetic(member)
                    {
                        continue;
                    }
                    match self.classify_numeric_operand(member) {
                        NumericOperand::Flexible(ty) => open.push(ty),
                        NumericOperand::FlexibleVector(element) => open.extend(element),
                        NumericOperand::AnyUnknown => dispatches = true,
                        _ if self.declares_arithmetic(member) => dispatches = true,
                        _ => return NumericOperand::Invalid,
                    }
                }
                if open.is_empty() && !dispatches {
                    NumericOperand::ConcreteUnion(parts)
                } else {
                    NumericOperand::OpenUnion(open)
                }
            }
            TyKind::Var(_) => NumericOperand::Flexible(resolved),
            // R's arithmetic reads a logical as an integer, so a parameter bounded by
            // `integer | logical` gives the bound's arithmetic, never itself.
            TyKind::Rigid(name)
                if let Some(&bound) = self.table.rigid_bounds.get(name)
                    && admits_logical(self.db, bound) =>
            {
                self.classify_numeric_operand(bound)
            }
            TyKind::Rigid(name) => {
                match self
                    .table
                    .rigid_satisfies(self.db, *name, Constraint::Arithmetic)
                {
                    true => NumericOperand::Flexible(resolved),
                    false => NumericOperand::Invalid,
                }
            }
            TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(self.db) {
                TyKind::Var(_) => NumericOperand::FlexibleVector(Some(*element)),
                // `N[]` under `<N: integer | logical>` is a vector of one of the bound's members.
                TyKind::Rigid(name)
                    if let Some(&bound) = self.table.rigid_bounds.get(name)
                        && admits_logical(self.db, bound) =>
                {
                    let single = [bound];
                    let members: &[Ty<'db>] = match bound.kind(self.db) {
                        TyKind::Union(members) => members,
                        _ => &single,
                    };
                    let vectors = members.iter().map(|&member| match member.kind(self.db) {
                        TyKind::Scalar(_) => Ty::new(self.db, TyKind::Vector(member)),
                        _ => member,
                    });
                    self.classify_numeric_operand(union_of(self.db, vectors))
                }
                TyKind::Rigid(name)
                    if self
                        .table
                        .rigid_satisfies(self.db, *name, Constraint::Arithmetic) =>
                {
                    NumericOperand::FlexibleVector(Some(*element))
                }
                TyKind::Any | TyKind::Unknown => NumericOperand::FlexibleVector(None),
                _ => NumericOperand::Invalid,
            },
            TyKind::Any | TyKind::Unknown => NumericOperand::AnyUnknown,
            _ => NumericOperand::Invalid,
        }
    }

    fn distinct_rigids(&self, left: Ty<'db>, right: Ty<'db>) -> bool {
        let left = self.table.shallow_resolve(self.db, left);
        let right = self.table.shallow_resolve(self.db, right);
        left != right
            && matches!(left.kind(self.db), TyKind::Rigid(_))
            && matches!(right.kind(self.db), TyKind::Rigid(_))
    }

    /// Whether the type is a variable that a scalar use bounded (`1:n`).
    fn scalar_bound(&self, ty: Ty<'db>) -> bool {
        let resolved = self.table.shallow_resolve(self.db, ty);
        matches!(
            self.table.variable_constraint(self.db, resolved),
            Some(Constraint::ScalarNumeric)
        )
    }

    /// An annotation declares parameter types, never the parameter list: R matches
    /// arguments against the `function(...)` formals, so the signature keeps their
    /// names, order, optionality, and `...`. Disagreements report here, never at a call.
    /// An inline signature sits on the formals and cannot restate optionality, so
    /// `inline_parameters` (typed only inline) skip the optionality checks: the formals
    /// alone decide them.
    fn check_declared_function(
        &mut self,
        function_id: ExprId,
        declared: &FunctionType<'db>,
        inline_parameters: &[String],
    ) -> Ty<'db> {
        let module = self.module;
        let expression = function_id;
        let ExpressionKind::Function { parameters, body } = module.kind(expression) else {
            return Ty::new(self.db, TyKind::Function(&declared.clone()));
        };
        let range = module.range(expression);

        let uses = self.formal_uses(&module[parameters], *body);
        // A call may omit a formal the body never forces, but only a default or a
        // `missing()` test makes optionality part of the formal's contract. Laziness only
        // relaxes what nothing declares: a declared required parameter stays required.
        let is_optional =
            |parameter: &Formal| parameter.default.is_some() || uses.makes_optional(parameter.name);
        let is_declared_optional = |parameter: &Formal| {
            parameter.default.is_some() || uses.missing_tested.contains(&parameter.name)
        };

        self.table.level += 1;
        let enclosing_frame_level = mem::replace(&mut self.frame_level, self.table.level);
        // Pairs formals with declared types as R's matcher pairs arguments; any declared
        // entry the walk never consumes is a disagreement.
        let declared_names: Vec<&str> = declared
            .named
            .iter()
            .map(|field| field.name.text(self.db))
            .collect();
        let paired = pair_formals(
            module[parameters]
                .iter()
                .map(|parameter| parameter.name.as_str()),
            &declared_names,
            declared.positional.len(),
        );
        let mut positional_index = 0usize;
        let mut used_named = Bitset::new_in(self.arena, declared.named.len());
        let mut formal_types: Vec<(Ty<'db>, bool)> = Vec::with_capacity(parameters.len());
        let mut named: Vec<Parameter<'db>> = Vec::with_capacity(parameters.len());
        let mut variadic = None;
        for (parameter, paired) in module[parameters].iter().zip(paired) {
            if paired == Paired::Rest {
                variadic = Some(RestParameter {
                    element: declared
                        .variadic
                        .as_ref()
                        .map_or_else(|| any(self.db), |rest| rest.element),
                    preceding_named: named.len(),
                });
                formal_types.push((self.fresh(Constraint::Unconstrained), false));
                continue;
            }
            // A disagreeing declaration is reported below, and the definition wins: a declared
            // `[name]` over a forced formal stays required, a declared required parameter over
            // a defaulted or `missing()`-tested formal stays optional.
            let (declared_ty, optional) = match paired {
                Paired::Named(index) => {
                    used_named.insert(index);
                    let field = &declared.named[index];
                    let optional =
                        is_declared_optional(parameter) || field.optional && is_optional(parameter);
                    (Some(field.ty), optional)
                }
                Paired::Positional(index) => {
                    positional_index = index + 1;
                    (
                        Some(declared.positional[index]),
                        is_declared_optional(parameter),
                    )
                }
                // Partial annotation: an unannotated formal infers from its uses as if unannotated.
                Paired::Rest | Paired::Undeclared => (None, is_optional(parameter)),
            };
            let parameter_ty = declared_ty.unwrap_or_else(|| self.fresh(Constraint::Unconstrained));
            formal_types.push((parameter_ty, declared_ty.is_some()));
            named.push(Parameter {
                name: Name::new(self.db, parameter.name),
                ty: parameter_ty,
                optional,
                // Inferred below, once the formals are bound, as R evaluates a default in the
                // function's frame.
                default: None,
            });
        }

        // A declared optional needs a real default, `...` must sit at the same boundary on
        // both sides, and every declared type needs a formal. The body still checks under
        // the declared types so IDE facts survive.
        let dots_index = module[parameters].iter().position(|p| p.name == "...");
        let declared_preceding = declared.positional.len()
            + declared
                .variadic
                .as_ref()
                .map_or(0, |rest| rest.preceding_named);
        let variadic_mismatch = match (&declared.variadic, dots_index) {
            (Some(_), Some(index)) => index != declared_preceding,
            (None, None) => false,
            _ => true,
        };
        let restates = |name: &str| !inline_parameters.iter().any(|inline| inline == name);
        let optional_mismatch = module[parameters].iter().any(|parameter| {
            !is_optional(parameter)
                && restates(&parameter.name)
                && declared
                    .named
                    .iter()
                    .any(|field| field.optional && field.name.text(self.db) == parameter.name)
        });
        let surplus_positional = positional_index < declared.positional.len();
        if variadic_mismatch || optional_mismatch || surplus_positional {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::Mismatch {
                    expected: Ty::new(self.db, TyKind::Function(&declared.clone())),
                    found: Ty::new(
                        self.db,
                        TyKind::Function(&FunctionType::declared(
                            &[],
                            &named,
                            variadic,
                            declared.ret,
                        )),
                    ),
                },
            });
        }
        // A defaulted formal is optional whatever the annotation says; reporting it once
        // here keeps it from resurfacing at every caller.
        for field in declared
            .named
            .iter()
            .filter(|field| restates(field.name.text(self.db)))
        {
            let name = field.name.text(self.db);
            if !field.optional
                && module[parameters]
                    .iter()
                    .any(|parameter| parameter.name == name && is_declared_optional(parameter))
            {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::AnnotationRequiredButDefaulted {
                        name: name.to_owned(),
                    },
                });
            }
        }
        for (index, field) in declared.named.iter().enumerate() {
            if !used_named.contains(index) {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::AnnotationParameterMismatch {
                        name: field.name.text(self.db).to_owned(),
                    },
                });
            }
        }

        let pending_mark = self.pending_enclosing_writes.len();
        let mark = self.environment.mark();
        let untyped_candidates: Vec<(&Formal, Option<Ty<'db>>)> = module[parameters]
            .iter()
            .zip(&formal_types)
            .filter(|(parameter, _)| parameter.name != "...")
            .map(|(parameter, &(ty, declared))| (parameter, (!declared).then_some(ty)))
            .collect();
        let mut named_index = 0;
        for (parameter, (parameter_ty, declared)) in module[parameters].iter().zip(formal_types) {
            let parameter_index = named_index;
            if parameter.name != "..." {
                named_index += 1;
            }
            if let Some(slot) = self
                .naming
                .bindings
                .iter()
                .find(|(_, info)| info.range == parameter.range)
                .map(|(id, _)| *id)
            {
                self.environment.set(slot, EnvEntry::Mono(parameter_ty));
                if !declared {
                    self.computed_slots.insert(slot);
                }
                if parameter.default.is_none() {
                    self.no_default_formals.insert(slot);
                } else {
                    self.defaulted_formals.insert(slot);
                }
            }
            if let Some(default) = parameter.default {
                // A declared function type is the context for a function literal default, as
                // it is for a function literal passed to a call.
                let hinted = declared && self.hint_from_declared_function(default, parameter_ty);
                let errors_before = self.errors.len();
                let default_ty = self.infer(default);
                if hinted {
                    self.name_contextual_parameters(default, errors_before, LambdaContext::Default);
                }
                if let Some(entry) = named.get_mut(parameter_index) {
                    entry.default = Some(default_ty);
                }
                // An undeclared formal's type comes from its uses, not its default.
                let resolved_default = self.table.resolve(self.db, default_ty);
                if declared {
                    let default_range = self.blame_range(default);
                    // Check against an instantiation, not the rigid binder: omitting the argument
                    // is the one call where the default makes the binder's choice, so
                    // `<T> fn([x]: T)` over `function(x = 1)` is honest.
                    let admissible = self.instantiate_rigids(parameter_ty);
                    // `function(title = NULL)` is R's optional argument, but it puts `NULL` in the
                    // body, breaking a non-nullable declaration; the dedicated finding names the remedy.
                    if matches!(resolved_default.kind(self.db), TyKind::Null)
                        && !self.table.compatible(self.db, resolved_default, admissible)
                    {
                        self.errors.push(TypeError {
                            range: default_range,
                            kind: TypeErrorKind::NullDefaultNotAdmitted {
                                name: parameter.name.to_string(),
                                declared: parameter_ty,
                            },
                        });
                    } else if !matches!(resolved_default.kind(self.db), TyKind::Null) {
                        let whole_double = self.is_whole_double(default);
                        if let Err(error) = self.check_argument(
                            admissible,
                            default_ty,
                            default_range,
                            whole_double,
                            Some(default),
                        ) {
                            self.errors.push(error);
                        }
                    }
                }
            }
        }
        // An annotation says what the formals take, not how the body reads them, so an
        // annotated function that captures a formal's code is non-standard evaluation too.
        let quoted: Vec<Name<'db>> = module[parameters]
            .iter()
            .filter(|parameter| uses.quotes(parameter.name))
            .map(|parameter| Name::new(self.db, parameter.name))
            .collect();
        let mut signature = FunctionType::declared(&[], &named, variadic, declared.ret);
        signature.quoted = &quoted;

        self.return_frames.push(Vec::new());
        let trailing_ty = self.infer_body_with_capture_discovery(*body, pending_mark);
        let early_returns = self
            .return_frames
            .pop()
            .expect("return frames stay balanced around body inference");
        // An `Unknown` declared return (elided `->`) is inferred from the body, so
        // annotating only the parameters does not drop the return.
        if matches!(declared.ret.kind(self.db), TyKind::Unknown) {
            let ret = self.join_early_returns(&early_returns, trailing_ty, *body);
            signature.ret = ret;
            self.record_untyped_parameters(function_id, &untyped_candidates, ret);
        } else {
            self.record_untyped_parameters(function_id, &untyped_candidates, declared.ret);
            // Each `return` is checked at its own site, not as one union blamed on the tail.
            let mut early_return_failed = false;
            for &EarlyReturn {
                ty: returned,
                range: return_range,
                value,
            } in &early_returns
            {
                // `return(4)` in a function declared `-> I64` is the literal `4i64`, as a tail
                // `4` is.
                if value.is_some_and(|value| {
                    self.adapt_literal(value, declared.ret) != LiteralFit::NotLiteral
                }) {
                    continue;
                }
                if !matches!(returned.kind(self.db), TyKind::Unknown)
                    && !self.table.compatible(self.db, returned, declared.ret)
                {
                    early_return_failed = true;
                    let kind = self
                        .type_mismatch(return_range, declared.ret, returned, None)
                        .kind;
                    self.errors.push(TypeError {
                        range: return_range,
                        kind,
                    });
                }
            }
            // Blame the value-producing expression (a block's tail, an `if`/`else` arm).
            let mut leaves = Vec::new();
            self.collect_tail_expressions(*body, &mut leaves);
            let blamed = match leaves.as_slice() {
                [single] => *single,
                _ => *body,
            };
            let body_range = self.blame_range(blamed);
            // The trailing value alone: early returns already reported themselves.
            let resolved_body = self.table.resolve(self.db, trailing_ty);
            let tail_adapts = !leaves.is_empty()
                && leaves.iter().fold(true, |all, &leaf| {
                    let fits = match self.adapt_literal(leaf, declared.ret) {
                        LiteralFit::Fits | LiteralFit::OutOfRange => true,
                        LiteralFit::NotLiteral => {
                            let found = self.recorded_type(leaf);
                            self.table.compatible(self.db, found, declared.ret)
                        }
                    };
                    all && fits
                });
            if !early_return_failed
                && !tail_adapts
                && !self.diverges(*body)
                && !matches!(resolved_body.kind(self.db), TyKind::Unknown)
                && !self.table.compatible(self.db, resolved_body, declared.ret)
            {
                // The whole body's type is the verdict; leaves only decide where to point. With
                // no single leaf at fault (an `else`-less `if`), the tail keeps the finding.
                let culprits: Vec<(ExprId, Ty<'db>)> = leaves
                    .iter()
                    .filter(|&&leaf| leaf != blamed)
                    .filter_map(|&leaf| {
                        let ty = self.table.resolve(
                            self.db,
                            self.recorded
                                .get(&leaf)
                                .copied()
                                .unwrap_or(unknown(self.db)),
                        );
                        (!matches!(ty.kind(self.db), TyKind::Unknown)
                            && !self.table.compatible(self.db, ty, declared.ret))
                        .then_some((leaf, ty))
                    })
                    .collect();
                if culprits.is_empty() {
                    let kind = self
                        .type_mismatch(body_range, declared.ret, resolved_body, None)
                        .kind;
                    self.errors.push(TypeError {
                        range: body_range,
                        kind,
                    });
                } else {
                    for (leaf, found) in culprits {
                        let range = self.blame_range(leaf);
                        let kind = self.type_mismatch(range, declared.ret, found, None).kind;
                        self.errors.push(TypeError { range, kind });
                    }
                }
            }
        }
        self.environment.rollback(mark);
        self.reapply_enclosing_writes(pending_mark);
        self.frame_level = enclosing_frame_level;
        self.table.level -= 1;
        // Not resolved: annotation-sourced types hold no inference variables, and resolving
        // would expand the aliases and nominals the author wrote.
        Ty::new(self.db, TyKind::Function(&signature))
    }

    /// DistributionUtils' `pDist(..., valueOnly = TRUE)` ends in `if (valueOnly) prob else
    /// list(value = prob, error = err)`. A flag the body never reassigns decides the branch
    /// from the call alone, so its two branch types are kept beside the joined return. A
    /// branch may also end in `return(value)`, as pracma's `gcd(..., extended = FALSE)` does,
    /// or be an earlier `if (flag) return(value)` before the tail, as in Matrix's `iam(j, both =
    /// FALSE)`, provided those are the body's only early returns. That guard may be a block
    /// holding every early return, as survival's `is.ratetable(x, verbose = FALSE)` checks
    /// under `if (!verbose) { ...; return(TRUE) }`, and the flag may be negated.
    fn flag_return(
        &self,
        parameters: &[Formal],
        body: ExprId,
        early_returns: &[EarlyReturn<'db>],
    ) -> Option<FlagReturn<'db>> {
        let module = self.module;
        let strip = |mut id: ExprId| {
            while let ExpressionKind::Paren(inner) = module.kind(id) {
                id = *inner;
            }
            id
        };
        let flag = |condition: ExprId| match module.kind(strip(condition)) {
            ExpressionKind::NameRef(name) => Some((*name, false)),
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => match module.kind(strip(*operand)) {
                ExpressionKind::NameRef(name) => Some((*name, true)),
                _ => None,
            },
            _ => None,
        };
        let returned = |branch: ExprId| {
            let mut value = strip(branch);
            if let ExpressionKind::Block { statements } = module.kind(value)
                && let Some(&last) = module[statements].last()
            {
                value = strip(last);
            }
            match module.kind(value) {
                ExpressionKind::Call { callee, arguments } if matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == "return") => {
                    Some(
                        module[arguments]
                            .first()
                            .and_then(|argument| argument.value),
                    )
                }
                _ => None,
            }
        };
        let mut statements: &[ExprId] = &[];
        let mut tail = strip(body);
        while let ExpressionKind::Block { statements: block } = module.kind(tail) {
            statements = &module[block];
            tail = strip(*statements.last()?);
        }
        // Either the tail branches on the flag, or a statement before it returns early on the
        // flag (`if (both) return(list(row.index = j))`) and the tail is the other value.
        let (name, negated, when_set, when_unset) = match module.kind(tail) {
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch: Some(else_branch),
            } if flag(*condition).is_some() => {
                let (name, negated) = flag(*condition)?;
                let mut returns = 0;
                let mut branch_ty = |branch: ExprId| {
                    let value = match returned(branch) {
                        Some(value) => {
                            returns += 1;
                            value?
                        }
                        None => branch,
                    };
                    self.recorded.get(&value).copied()
                };
                let then_ty = branch_ty(*then_branch)?;
                let else_ty = branch_ty(*else_branch)?;
                if returns != early_returns.len() {
                    return None;
                }
                (name, negated, then_ty, else_ty)
            }
            _ => {
                let (condition, guard) = statements[..statements.len().saturating_sub(1)]
                    .iter()
                    .find_map(|&statement| match module.kind(strip(statement)) {
                        ExpressionKind::If {
                            condition,
                            then_branch,
                            else_branch: None,
                        } if flag(*condition).is_some() && returned(*then_branch).is_some() => {
                            Some((*condition, *then_branch))
                        }
                        _ => None,
                    })?;
                let (name, negated) = flag(condition)?;
                // Every early return is the guard's or the tail's own `return(value)`.
                let guard_range = module.range(guard);
                let tail_range = module.range(tail);
                let (guarded, rest): (Vec<EarlyReturn<'db>>, Vec<EarlyReturn<'db>>) = early_returns
                    .iter()
                    .copied()
                    .partition(|early| guard_range.contains_range(early.range));
                if rest
                    .iter()
                    .any(|early| !tail_range.contains_range(early.range))
                {
                    return None;
                }
                let guard_ty = self.join_early_returns(&guarded, unknown(self.db), guard);
                let tail_value = match returned(tail) {
                    Some(value) => value?,
                    None => tail,
                };
                (name, negated, guard_ty, *self.recorded.get(&tail_value)?)
            }
        };
        let formal = parameters.iter().find(|formal| formal.name == name)?;
        let ExpressionKind::Literal(LiteralKind::Logical(default)) =
            module.kind(strip(formal.default?))
        else {
            return None;
        };
        if self.writes_name(body, name) {
            return None;
        }
        let (when_true, when_false) = if negated {
            (when_unset, when_set)
        } else {
            (when_set, when_unset)
        };
        Some(FlagReturn {
            parameter: Name::new(self.db, name),
            default: *default,
            when_true,
            when_false,
        })
    }

    /// The body's tail is `Reduce(f, x, init = init)` over formals it never rebinds, with `f`
    /// a formal or a local `function(a, b) formal(a, b, ...)`.
    fn reduce_forward(&self, parameters: &[Formal], body: ExprId) -> Option<ReduceForward<'db>> {
        let module = self.module;
        let strip = |mut id: ExprId| {
            while let ExpressionKind::Paren(inner) = module.kind(id) {
                id = *inner;
            }
            id
        };
        let mut statements: &[ExprId] = &[];
        let mut tail = strip(body);
        while let ExpressionKind::Block { statements: inner } = module.kind(tail) {
            statements = &module[inner];
            tail = strip(*statements.last()?);
        }
        let ExpressionKind::Call { callee, arguments } = module.kind(tail) else {
            return None;
        };
        if variable_name(module, *callee).as_deref() != Some("Reduce")
            || self.naming.resolutions.contains_key(callee)
        {
            return None;
        }
        let mut slots: [Option<ExprId>; 3] = [None; 3];
        let mut next = 0;
        for argument in &module[arguments] {
            let value = argument.value?;
            let index = match argument.name {
                None => {
                    while slots.get(next).is_some_and(Option::is_some) {
                        next += 1;
                    }
                    next
                }
                Some(name) => match &*module.name(name) {
                    "f" => 0,
                    "x" => 1,
                    "init" => 2,
                    _ => return None,
                },
            };
            let slot = slots.get_mut(index)?;
            if slot.is_some() {
                return None;
            }
            *slot = Some(value);
        }
        let formal = |value: ExprId| -> Option<Symbol> {
            let ExpressionKind::NameRef(name) = module.kind(strip(value)) else {
                return None;
            };
            parameters.iter().find(|formal| formal.name == *name)?;
            (!self.writes_name(body, *name)).then_some(*name)
        };
        let function = slots[0]?;
        let function = formal(function).or_else(|| {
            let ExpressionKind::NameRef(local) = module.kind(strip(function)) else {
                return None;
            };
            let [definition] = statements
                .iter()
                .filter(|&&statement| {
                    matches!(module.kind(statement), ExpressionKind::Assign { target, .. }
                        if matches!(module.kind(*target), ExpressionKind::NameRef(name) if name == local))
                })
                .collect::<Vec<_>>()[..]
            else {
                return None;
            };
            let ExpressionKind::Assign { value, .. } = module.kind(*definition) else {
                return None;
            };
            let ExpressionKind::Function {
                parameters: closure_parameters,
                body: closure_body,
            } = module.kind(strip(*value))
            else {
                return None;
            };
            let [first, second] = &module[closure_parameters][..] else {
                return None;
            };
            let ExpressionKind::Call { callee, arguments } = module.kind(strip(*closure_body))
            else {
                return None;
            };
            let forwarded = &module[arguments];
            let reads = |argument: Option<&Argument>, name: Symbol| {
                argument.is_some_and(|argument| {
                    argument.name.is_none()
                        && argument.value.is_some_and(|value| {
                            matches!(module.kind(strip(value)), ExpressionKind::NameRef(read) if *read == name)
                        })
                })
            };
            let only_dots = forwarded.iter().skip(2).all(|argument| {
                argument.name.is_none()
                    && argument.value.is_some_and(|value| {
                        matches!(module.kind(value), ExpressionKind::NameRef(read) if *read == "...")
                    })
            });
            (reads(forwarded.first(), first.name) && reads(forwarded.get(1), second.name) && only_dots)
                .then(|| formal(*callee))
                .flatten()
        })?;
        let x = formal(slots[1]?)?;
        let init = match slots[2] {
            Some(init) => Some(formal(init)?),
            None => None,
        };
        Some(ReduceForward {
            function: Name::new(self.db, function),
            x: Name::new(self.db, x),
            init: init.map(|init| Name::new(self.db, init)),
        })
    }

    /// `if (!predicate(formal)) return(NULL)` as a statement of the body, when no other way out
    /// of the body returns `NULL`, or a body ending in a chain of `predicate(formal)` tests of
    /// one formal whose last `else` is `NULL`, when no early return may be `NULL`.
    fn null_unless(
        &self,
        parameters: &[Formal],
        body: ExprId,
        early_returns: &[EarlyReturn<'db>],
        trailing: Ty<'db>,
    ) -> Option<NullUnless<'db>> {
        let module = self.module;
        let strip = |mut id: ExprId| {
            while let ExpressionKind::Paren(inner) = module.kind(id) {
                id = *inner;
            }
            id
        };
        // An open or unchecked member may be instantiated as `NULL` at the call, so it counts.
        let may_be_null = |member: Ty<'db>| {
            matches!(
                member.kind(self.db),
                TyKind::Null | TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown
            )
        };
        let has_null = |ty: Ty<'db>| {
            let ty = self.table.resolve(self.db, ty);
            match ty.kind(self.db) {
                TyKind::Union(members) => members.iter().any(|&member| may_be_null(member)),
                _ => may_be_null(ty),
            }
        };
        let is_null = |id: ExprId| {
            matches!(
                module.kind(strip(id)),
                ExpressionKind::Literal(LiteralKind::Null)
            )
        };
        // `predicate(formal)` with a project or library predicate that is not a type test,
        // on a formal the body never writes.
        let guard = |condition: ExprId| -> Option<(Symbol, Symbol)> {
            let ExpressionKind::Call { callee, arguments } = module.kind(strip(condition)) else {
                return None;
            };
            let ExpressionKind::NameRef(predicate) = module.kind(*callee) else {
                return None;
            };
            if self.naming.resolutions.contains_key(callee) || guard_kind(predicate).is_some() {
                return None;
            }
            let [argument] = &module[arguments][..] else {
                return None;
            };
            let value = argument.value.filter(|_| argument.name.is_none())?;
            let ExpressionKind::NameRef(name) = module.kind(strip(value)) else {
                return None;
            };
            parameters.iter().find(|formal| formal.name == *name)?;
            if self.writes_name(body, *name) {
                return None;
            }
            Some((*predicate, *name))
        };
        let null_unless = |(predicate, parameter): (Symbol, Symbol), class_fallback| NullUnless {
            predicate: Name::new(self.db, predicate),
            parameter: Name::new(self.db, parameter),
            class_fallback,
        };
        let statements = match module.kind(strip(body)) {
            ExpressionKind::Block { statements } => &module[statements][..],
            _ => &[][..],
        };
        let early_exit = statements.iter().find_map(|&statement| {
            let ExpressionKind::If {
                condition,
                then_branch,
                else_branch: None,
            } = module.kind(strip(statement))
            else {
                return None;
            };
            let ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } = module.kind(strip(*condition))
            else {
                return None;
            };
            let tested = guard(*operand)?;
            let mut exit = strip(*then_branch);
            if let ExpressionKind::Block { statements } = module.kind(exit) {
                let [only] = &module[statements][..] else {
                    return None;
                };
                exit = strip(*only);
            }
            let ExpressionKind::Call { callee, arguments } = module.kind(exit) else {
                return None;
            };
            if variable_name(module, *callee).as_deref() != Some("return") {
                return None;
            }
            let [returned] = &module[arguments][..] else {
                return None;
            };
            if !returned.value.is_some_and(is_null) {
                return None;
            }
            let exit_range = module.range(exit);
            let others_null = early_returns
                .iter()
                .filter(|early| !exit_range.contains_range(early.range))
                .any(|early| has_null(early.ty))
                || (!self.diverges(body) && has_null(trailing));
            (!others_null).then(|| null_unless(tested, false))
        });
        if early_exit.is_some() {
            return early_exit;
        }
        if early_returns.iter().any(|early| has_null(early.ty)) {
            return None;
        }
        let mut arm = match statements.last() {
            Some(&last) => strip(last),
            None => strip(body),
        };
        let mut first = None;
        loop {
            let ExpressionKind::If {
                condition,
                then_branch,
                else_branch: Some(else_branch),
            } = module.kind(arm)
            else {
                return None;
            };
            let tested = guard(*condition)?;
            if first.is_some_and(|(_, parameter)| parameter != tested.1) || is_null(*then_branch) {
                return None;
            }
            first.get_or_insert(tested);
            arm = strip(*else_branch);
            if is_null(arm) {
                return first.map(|tested| null_unless(tested, true));
            }
        }
    }

    /// A formal the body never forces is optional, but a forced formal's default runs when the
    /// call leaves that formal out, and forces what the default reads.
    fn default_reads(
        &self,
        parameters: &[Formal],
        uses: &FormalUses,
        named: &[Parameter<'db>],
    ) -> Vec<DefaultRead<'db>> {
        let mut reads = Vec::new();
        for reader in parameters {
            let Some(default) = reader.default else {
                continue;
            };
            if !uses.forces_on_every_path(reader.name) {
                continue;
            }
            let default_uses = self.formal_uses(parameters, default);
            for formal in parameters {
                if formal.name == reader.name
                    || formal.default.is_some()
                    || uses.missing_tested.contains(&formal.name)
                    || !default_uses.forces_on_every_path(formal.name)
                    || !named.iter().any(|parameter| {
                        parameter.optional && parameter.name.text(self.db) == formal.name
                    })
                {
                    continue;
                }
                reads.push(DefaultRead {
                    formal: Name::new(self.db, formal.name),
                    default_of: Name::new(self.db, reader.name),
                });
            }
        }
        reads
    }

    fn writes_name(&self, id: ExprId, name: Symbol) -> bool {
        if let ExpressionKind::Assign { target, .. } = self.module.kind(id)
            && matches!(self.module.kind(*target), ExpressionKind::NameRef(written) if *written == name)
        {
            return true;
        }
        self.module
            .any_child(id, |child| self.writes_name(child, name))
    }

    /// The parameters whose every read under `body` is the whole condition of an `if` or a
    /// `while`, and which are read at least once. A nested function's reads count like any
    /// other. One walk serves every parameter, since a walk per parameter is quadratic in
    /// long signatures.
    fn condition_only_names(&self, body: ExprId, parameters: &[Formal]) -> Vec<Symbol> {
        // Per parameter: read as a whole condition, and read some other way.
        let mut reads: Vec<(Symbol, bool, bool)> = parameters
            .iter()
            .map(|parameter| (parameter.name, false, false))
            .collect();
        if reads.is_empty() {
            return Vec::new();
        }
        self.record_condition_reads(body, &mut reads);
        reads
            .into_iter()
            .filter(|&(_, as_condition, otherwise)| as_condition && !otherwise)
            .map(|(name, _, _)| name)
            .collect()
    }

    fn record_condition_reads(&self, id: ExprId, reads: &mut [(Symbol, bool, bool)]) {
        let module = self.module;
        // A condition that is a tracked name, through any parentheses, is a condition read
        // of it, and holds no other read.
        let condition_name = |mut condition: ExprId| {
            while let ExpressionKind::Paren(inner) = module.kind(condition) {
                condition = *inner;
            }
            match module.kind(condition) {
                ExpressionKind::NameRef(name) => reads.iter().position(|read| read.0 == *name),
                _ => None,
            }
        };
        match module.kind(id) {
            ExpressionKind::NameRef(name) => {
                if let Some(read) = reads.iter_mut().find(|read| read.0 == *name) {
                    read.2 = true;
                }
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } if let Some(position) = condition_name(*condition) => {
                let (then_branch, else_branch) = (*then_branch, *else_branch);
                if let Some(read) = reads.get_mut(position) {
                    read.1 = true;
                }
                self.record_condition_reads(then_branch, reads);
                if let Some(branch) = else_branch {
                    self.record_condition_reads(branch, reads);
                }
            }
            ExpressionKind::While { condition, body }
                if let Some(position) = condition_name(*condition) =>
            {
                let body = *body;
                if let Some(read) = reads.get_mut(position) {
                    read.1 = true;
                }
                self.record_condition_reads(body, reads);
            }
            _ => module.for_each_child(id, |child| self.record_condition_reads(child, reads)),
        }
    }

    /// An `else`-less `if` is a leaf itself: its implicit `NULL` belongs to no arm.
    fn collect_tail_expressions(&self, id: ExprId, into: &mut Vec<ExprId>) {
        match self.module.kind(id) {
            ExpressionKind::Block { statements } => match self.module[statements].last() {
                Some(&tail) => self.collect_tail_expressions(tail, into),
                None => into.push(id),
            },
            ExpressionKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => {
                let (then_branch, else_branch) = (*then_branch, *else_branch);
                self.collect_tail_expressions(then_branch, into);
                self.collect_tail_expressions(else_branch, into);
            }
            ExpressionKind::Match { arms, .. } if !self.module[arms].is_empty() => {
                for arm in &self.module[arms] {
                    self.collect_tail_expressions(arm.body, into);
                }
            }
            _ => into.push(id),
        }
    }

    /// Re-runs the body once when it grew a captured-write join a closure already read;
    /// the first run is fully discarded (environment, unification, diagnostics, `<<-` writes).
    fn infer_body_with_capture_discovery(&mut self, body: ExprId, pending_mark: usize) -> Ty<'db> {
        let saved_repass = mem::replace(&mut self.capture_repass_needed, false);
        let saved_seeds = mem::take(&mut self.superassign_seeds);
        let saved_body_range = mem::replace(&mut self.body_range, self.module.range(body));
        let body_mark = self.environment.mark();
        let body_snapshot = self.table.snapshot();
        let errors_mark = self.errors.len();
        let origins_mark = self.strict_origins.len();
        let mut superassigned = Vec::new();
        self.collect_superassigned(body, &mut superassigned);
        self.seed_superassigned_reads(&superassigned);
        let mut trailing_ty = self.infer(body);
        let range = self.body_range;
        let before = self.late_triggers.len();
        self.late_triggers.retain(|read| !range.contains(*read));
        if self.capture_repass_needed || self.late_triggers.len() < before {
            self.environment.rollback(body_mark);
            self.table.rollback(body_snapshot);
            self.truncate_errors(errors_mark);
            self.strict_origins.truncate(origins_mark);
            self.pending_enclosing_writes.truncate(pending_mark);
            if let Some(frame) = self.return_frames.last_mut() {
                frame.clear();
            }
            self.capture_repass_needed = false;
            self.superassign_seeds.clear();
            self.seed_superassigned_reads(&superassigned);
            trailing_ty = self.infer(body);
            self.late_triggers.retain(|read| !range.contains(*read));
        }
        // A write the body never reached leaves its seed open; it must not generalize.
        let seeds = mem::replace(&mut self.superassign_seeds, saved_seeds);
        for seed in seeds {
            if !seed.written
                && self
                    .table
                    .unify(self.db, seed.variable, unknown(self.db))
                    .is_err()
            {
                panic!("an open superassignment seed always unifies with `Unknown`");
            }
        }
        self.capture_repass_needed = saved_repass;
        self.body_range = saved_body_range;
        trailing_ty
    }

    /// A function that writes an enclosing variable with `<<-` may run many times, so on entry
    /// that variable holds whatever an earlier call wrote: `if (is.null(cache)) cache <<- f()`
    /// reads `cache` as `NULL` or the written value. The entry reads a fresh variable beside the
    /// enclosing type, and the body's first write of the slot binds it. Only a variable that
    /// may still be `NULL` is seeded: that is the lazily filled cache, while a counter's
    /// `count <<- count + 1L` must read the concrete count to type the write at all.
    fn seed_superassigned_reads(&mut self, slots: &[BindingId]) {
        for &slot in slots {
            let Some(EnvEntry::Mono(existing)) = self.environment.get(slot) else {
                continue;
            };
            let existing = self.table.resolve(self.db, existing);
            if !guard_members(self.db, existing)
                .iter()
                .any(|member| matches!(member.kind(self.db), TyKind::Null))
            {
                continue;
            }
            let seed = self.fresh(Constraint::Unconstrained);
            self.superassign_seeds.push(SuperassignSeed {
                slot,
                variable: seed,
                written: false,
            });
            let entry = union_of(self.db, [existing, seed]);
            self.environment.set(slot, EnvEntry::Mono(entry));
        }
    }

    /// The slots this body itself writes with `<<-`. A nested function's writes are its own,
    /// since they happen only when it is called.
    fn collect_superassigned(&self, id: ExprId, slots: &mut Vec<BindingId>) {
        if let ExpressionKind::Assign {
            spelling: AssignSpelling::Super,
            target,
            ..
        } = self.module.kind(id)
            && let Some(&slot) = self.naming.resolutions.get(target)
            && !slots.contains(&slot)
        {
            slots.push(slot);
        }
        if matches!(self.module.kind(id), ExpressionKind::Function { .. }) {
            return;
        }
        let module = self.module;
        module.for_each_child(id, |child| self.collect_superassigned(child, slots));
    }

    /// `return(x)` exits the function, so a body that always ends in an exit contributes no
    /// trailing value: `function() return(TRUE)` returns `logical`, not `logical | NULL`.
    fn join_early_returns(
        &self,
        early_returns: &[EarlyReturn<'db>],
        trailing: Ty<'db>,
        body: ExprId,
    ) -> Ty<'db> {
        if early_returns.is_empty() {
            return trailing;
        }
        let mut members: Vec<Ty<'db>> = early_returns
            .iter()
            .filter(|early| !self.bare_na_returns.contains(&early.range))
            .map(|early| early.ty)
            .collect();
        if !self.diverges(body) {
            members.push(self.table.resolve(self.db, trailing));
        }
        // `return(NA)` beside returns of one atomic type is a missing value of that type, as
        // `c(NA, "a")` is a character vector.
        let atom = |ty: Ty<'db>| match ty.kind(self.db) {
            TyKind::Scalar(atom) => Some(*atom),
            TyKind::Vector(element) => match element.kind(self.db) {
                TyKind::Scalar(atom) => Some(*atom),
                _ => None,
            },
            _ => None,
        };
        let shared = members
            .first()
            .and_then(|&first| atom(self.table.resolve(self.db, first)))
            .filter(|&first| {
                members
                    .iter()
                    .all(|&member| atom(self.table.resolve(self.db, member)) == Some(first))
            });
        if shared.is_none() {
            members.extend(
                early_returns
                    .iter()
                    .filter(|early| self.bare_na_returns.contains(&early.range))
                    .map(|early| early.ty),
            );
        }
        union_of(self.db, members)
    }

    fn infer_call_expression(
        &mut self,
        id: ExprId,
        range: TextRange,
        callee: ExprId,
        arguments: &[Argument],
    ) -> Ty<'db> {
        if let ExpressionKind::NameRef(name) = self.module.kind(callee) {
            let builtin = match name.as_str() {
                "c" => Some(self.infer_combine(id, arguments)),
                "list" => Some(self.infer_list(arguments)),
                "switch" => Some(self.infer_switch(range, arguments)),
                "structure" => self.infer_structure(id, arguments),
                "return" => Some(self.infer_return(range, arguments)),
                "stop" => Some(self.infer_stop(arguments)),
                "DataFrame" => self.infer_frame_constructor(callee, arguments),
                family @ ("lapply" | "sapply" | "vapply" | "Reduce") => {
                    self.infer_apply(id, callee, family, arguments)
                }
                _ => None,
            };
            if let Some(ty) = builtin {
                return ty;
            }
        }
        let errors_before = self.errors.len();
        let ty = self.infer_ordinary_call(id, range, callee, arguments);
        if matches!(self.module.kind(callee), ExpressionKind::NameRef(name) if name.as_str() == "vapply")
            && self.calls_base(callee)
        {
            self.name_vapply_callback(errors_before, arguments);
        }
        if let ExpressionKind::NameRef(name) = self.module.kind(callee)
            && let family @ ("sapply" | "mapply" | "Map") = name.as_str()
            && self.calls_base(callee)
        {
            return self.simplified_apply(family, arguments).unwrap_or(ty);
        }
        if let Some(sequence) = self.integer_seq(callee, arguments, ty) {
            return self.record(id, sequence);
        }
        if let Some(verb) = self.dataframe_verb(callee) {
            return self.refine_dataframe_call(verb, arguments).unwrap_or(ty);
        }
        let function = self.table_function(callee);
        self.refine_table_call(id, function, arguments, ty)
    }

    /// Whether a called name reaches the base function of that name: neither a local binding nor
    /// a project definition shadows it.
    fn calls_base(&self, callee: ExprId) -> bool {
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return false;
        };
        let deferred = self.naming.deferred_non_locals.contains(&callee);
        !self.naming.resolutions.contains_key(&callee)
            && !self.globals.is_some_and(|globals| {
                globals.defined_in_project(Symbol::new(name.as_str()), deferred)
            })
    }

    /// Base `seq` without `by` or a length is R's `from:to`, an integer vector whenever
    /// `from` is a whole number, and `seq(x)` alone is `seq_len` or `seq_along`, always
    /// integer. The stub's numeric candidates cannot tell `1` from `1.5`, so they say
    /// `double[]`; this refines that answer from the arguments as written.
    fn integer_seq(&self, callee: ExprId, arguments: &[Argument], ty: Ty<'db>) -> Option<Ty<'db>> {
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return None;
        };
        if name.as_str() != "seq" || !self.calls_base(callee) {
            return None;
        }
        let resolved = self.table.resolve(self.db, ty);
        let numeric_sequence = matches!(
            resolved.kind(self.db),
            TyKind::Vector(element)
                if matches!(element.kind(self.db), TyKind::Scalar(Atomic::Double | Atomic::Integer))
        );
        if !numeric_sequence {
            return None;
        }
        let mut from = None;
        let mut positional = 0usize;
        for argument in arguments {
            match self.module.argument_name(argument).as_deref() {
                None if positional == 0 => {
                    from = argument.value;
                    positional += 1;
                }
                None if positional == 1 => positional += 1,
                Some("from") => from = argument.value,
                Some("to") => {}
                _ => return None,
            }
        }
        let from = from?;
        let whole = self.is_whole_double(from)
            || self.recorded.get(&from).is_some_and(|&from_ty| {
                matches!(
                    self.table.resolve(self.db, from_ty).kind(self.db),
                    TyKind::Scalar(Atomic::Integer)
                )
            });
        let alone = arguments.len() == 1
            && arguments
                .first()
                .is_some_and(|argument| argument.name.is_none());
        (whole || alone).then(|| self.shaped(OperandShape::Vector, Atomic::Integer))
    }

    fn infer_ordinary_call(
        &mut self,
        id: ExprId,
        range: TextRange,
        callee: ExprId,
        arguments: &[Argument],
    ) -> Ty<'db> {
        // The variables typing a named callee creates are its scheme's instantiated parameters.
        let first_var = self.table.next_var().0;
        let looked_up = match self.function_lookup(callee) {
            FunctionLookup::Ordinary => None,
            FunctionLookup::Found(ty) => Some(ty),
            FunctionLookup::Library(name, candidates) => {
                self.callee_targets.insert(callee, CalleeTarget::Library);
                if candidates.len() >= 2 {
                    let callees = self.hinting_callees(candidates, arguments);
                    let call_arguments =
                        self.infer_call_arguments_against(range, arguments, &callees);
                    return self.overloaded_call(
                        id,
                        range,
                        callee,
                        name,
                        candidates,
                        &call_arguments,
                    );
                }
                let ty = match candidates.last() {
                    Some(scheme) => self.instantiate(scheme),
                    None => match operator_value(self.db, &name) {
                        Some(scheme) => self.instantiate(&scheme),
                        // An export list names the function but no stub types it.
                        None => {
                            self.record_strict_origin(
                                callee,
                                StrictOriginKind::UndeterminedReference(name.to_string()),
                            );
                            unknown(self.db)
                        }
                    },
                };
                Some(self.record(callee, ty))
            }
            FunctionLookup::Undetermined => {
                self.infer_call_arguments(range, arguments);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                return unknown(self.db);
            }
        };
        if looked_up.is_none()
            && let Some(ty) = self.try_overloaded_call(id, range, callee, arguments)
        {
            return ty;
        }
        let callee_range = self.blame_range(callee);
        let callee_ty = match looked_up {
            Some(ty) => ty,
            None => self.infer(callee),
        };
        // Reading a base function no stub declares is left unchecked, because intrinsics such
        // as `c` are typed at their calls. A call that reaches this point is not one of them, so
        // its `Unknown` result enters here, as a library function's does.
        if looked_up.is_none()
            && let Some(name) = self.naming.non_locals.get(&callee)
            && matches!(
                self.table.shallow_resolve(self.db, callee_ty).kind(self.db),
                TyKind::Unknown
            )
            && is_base_export(self.db, name)
            && !self.globals.is_some_and(|globals| {
                globals.defined_in_project(*name, self.naming.deferred_non_locals.contains(&callee))
            })
        {
            self.record_strict_origin(
                callee,
                StrictOriginKind::UndeterminedReference(name.to_string()),
            );
        }
        let named_callee = matches!(
            self.module.kind(callee),
            ExpressionKind::NameRef(_) | ExpressionKind::Namespace { .. }
        );
        let instantiated = first_var..self.table.next_var().0;
        if self.is_null_placeholder(self.table.resolve(self.db, callee_ty)) {
            self.infer_call_arguments(range, arguments);
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        // A literal-`Any` callee is the escape hatch for stubs with no expressible
        // signature: the call is uncheckable, so findings inside its arguments are discarded.
        let resolved_callee = self.table.shallow_resolve(self.db, callee_ty);
        // A parameter or variable the author declared `Any` is their own opt-out, so calling
        // it opts out too; its arguments are still ordinary code and are checked.
        if matches!(resolved_callee.kind(self.db), TyKind::Any) && self.names_declared_value(callee)
        {
            self.infer_call_arguments(range, arguments);
            return any(self.db);
        }
        if matches!(resolved_callee.kind(self.db), TyKind::Any) {
            let recorded_errors = self.errors.len();
            let recorded_origins = self.strict_origins.len();
            self.infer_call_arguments(range, arguments);
            self.truncate_errors(recorded_errors);
            self.strict_origins.truncate(recorded_origins);
            // The `Unknown` enters here, so strict mode attributes it to this call.
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        let callee_ty = match self.names_declared_value(callee) {
            true => callee_ty,
            false => self.without_null_beside_functions(callee_ty),
        };
        let mut call_arguments = self.infer_call_arguments_against(range, arguments, &[callee_ty]);
        self.resolve_match_fun_arguments(callee, &mut call_arguments);
        if self.arity_tested_by_formals(callee, callee_ty, &call_arguments) {
            return unknown(self.db);
        }
        let source = match self.is_lookup(callee) {
            true => CalleeSource::Lookup,
            false => CalleeSource::Value,
        };
        let enclosing_call_first_var = mem::replace(&mut self.call_first_var, first_var);
        let errors_before = self.errors.len();
        let result = self.dispatch_call(range, callee_range, callee_ty, source, &call_arguments);
        self.call_first_var = enclosing_call_first_var;
        // A call its callee refuses can leave the callee's result variables unbound, and
        // printing one would show a bare `T`. A result the declaration fixes is kept.
        if self.errors.len() > errors_before {
            let mut variables = FxHashSet::default();
            collect_vars(self.db, self.table.resolve(self.db, result), &mut variables);
            if variables.iter().any(|variable| variable.0 >= first_var) {
                return unknown(self.db);
            }
        }
        if self.project_method_may_run(callee, &call_arguments) {
            return unknown(self.db);
        }
        let valueless_candidates = if named_callee {
            self.supplied_parameter_variables(callee_ty, &call_arguments, instantiated.clone())
        } else {
            Vec::new()
        };
        if valueless_candidates.is_empty() {
            return result;
        }
        self.drop_unconstrained_members(result, &valueless_candidates, instantiated)
    }

    /// The callee's freshly instantiated type parameters (`instantiated`) that the types of the
    /// parameters the call supplies mention, directly or as what a callback returns. Only those
    /// can stand for no value after the call: `T` in `fn(value: T | NULL, fallback: U) -> T | U`
    /// is open when the caller passes `NULL`, as `F` in `Result.and_then`'s
    /// `f: fn(A) -> Result<B, F>` is when the step never fails. A parameter the call leaves out
    /// may stand for a value the callee stored earlier (a getter and setter), and one that
    /// appears only in the result means "any value", so both keep their meaning.
    fn supplied_parameter_variables(
        &self,
        callee_ty: Ty<'db>,
        arguments: &[CallArgument<'db>],
        instantiated: Range<u32>,
    ) -> Vec<InferenceVar> {
        let db = self.db;
        if instantiated.is_empty() {
            return Vec::new();
        }
        let TyKind::Function(function) = self.table.shallow_resolve(db, callee_ty).kind(db) else {
            return Vec::new();
        };
        let arena = self.arena;
        let targets = &*argument_targets(db, arena, function, argument_shapes(arena, arguments));
        let mut mentioned = FxHashSet::default();
        for target in targets.iter().flatten() {
            let ty = match target {
                ArgumentTarget::Positional(slot) => function.positional.get(*slot).copied(),
                ArgumentTarget::Named(formal) => {
                    function.named.get(*formal).map(|parameter| parameter.ty)
                }
                ArgumentTarget::Rest => function.variadic.map(|rest| rest.element),
                _ => None,
            };
            if let Some(ty) = ty {
                collect_vars(db, ty, &mut mentioned);
            }
        }
        mentioned
            .into_iter()
            .filter(|var| instantiated.contains(&var.0))
            .collect()
    }

    /// A type parameter that no argument constrained stands for no value, so as a member of a
    /// union in the result it adds nothing: `Result.and_then`'s `Result<B, E | F>` is `:Err(E)`
    /// when the step never fails, and `or_else(NULL, "x")` is `character`. A union of nothing but
    /// such parameters stays as it is, and a parameter that a variable from outside the
    /// instantiation reaches (an unannotated outer parameter, or an argument's own open value)
    /// stays too, because its value is decided by that code.
    fn drop_unconstrained_members(
        &mut self,
        result: Ty<'db>,
        candidates: &[InferenceVar],
        instantiated: Range<u32>,
    ) -> Ty<'db> {
        let db = self.db;
        let mut open: FxHashSet<InferenceVar> = candidates
            .iter()
            .map(|&var| self.table.find(var))
            .filter(|&root| {
                instantiated.contains(&root.0)
                    && matches!(self.table.entry(root), Entry::Unbound { .. })
            })
            .collect();
        if open.is_empty() {
            return result;
        }
        let resolved = self.table.resolve(db, result);
        let mut in_result = FxHashSet::default();
        collect_vars(db, resolved, &mut in_result);
        open.retain(|var| in_result.contains(var));
        if open.is_empty() {
            return resolved;
        }
        // A variable from before the call, or one an argument created, decides its own value.
        let outside = (0..instantiated.start).chain(instantiated.end..self.table.next_var().0);
        for outer in outside {
            let root = self.table.find(InferenceVar(outer));
            let mut reached = FxHashSet::default();
            match self.table.entry(root) {
                // An open argument's bound names the callee's parameters it fills.
                Entry::Unbound {
                    bound: Some(bound), ..
                } => {
                    reached.insert(root);
                    let bound = self.table.resolve(db, *bound);
                    collect_vars(db, bound, &mut reached);
                }
                Entry::Unbound { .. } | Entry::Redirect(_) => {
                    reached.insert(root);
                }
                Entry::Bound { ty, .. } => {
                    let ty = self.table.resolve(db, *ty);
                    collect_vars(db, ty, &mut reached);
                }
            }
            open.retain(|var| !reached.contains(var));
            if open.is_empty() {
                return resolved;
            }
        }
        let unconstrained = |member: Ty<'db>| match member.kind(db) {
            TyKind::Var(var) => open.contains(var),
            _ => false,
        };
        fn walk<'db>(
            db: &'db dyn Db,
            ty: Ty<'db>,
            unconstrained: &dyn Fn(Ty<'db>) -> bool,
        ) -> Ty<'db> {
            let ty = map_child_types(db, ty, &mut |child| walk(db, child, unconstrained));
            match ty.kind(db) {
                TyKind::Union(members) if members.iter().any(|&member| !unconstrained(member)) => {
                    union_of(
                        db,
                        members
                            .iter()
                            .copied()
                            .filter(|&member| !unconstrained(member)),
                    )
                }
                _ => ty,
            }
        }
        walk(db, resolved, &unconstrained)
    }

    /// A variable whose every visible write is `NULL` (`msg <- NULL # trick R CMD check` before
    /// `msg %<~% ...`, or a top-level `.flag = NULL` that `assign()` fills) or, for a local, a bare
    /// `NA` (filled later with `<<-`) is a placeholder for a write the checker cannot see. Correct
    /// R never uses an always-`NULL` value where `NULL` fails, so such a read is not reported
    /// there. A parameter's `NULL` is what the caller passed, and stays checked.
    fn reads_null_placeholder(&self, value: ExprId, ty: Ty<'db>) -> bool {
        let mut value = value;
        while let ExpressionKind::Paren(inner) = self.module.kind(value) {
            value = *inner;
        }
        if !matches!(self.module.kind(value), ExpressionKind::NameRef(_))
            || self.binds_parameter(value)
        {
            return false;
        }
        if matches!(ty.kind(self.db), TyKind::Scalar(Atomic::Logical)) {
            let ExpressionKind::NameRef(name) = self.module.kind(value) else {
                return false;
            };
            return self
                .super_assigned
                .binary_search_by_key(&name.as_str(), |written| written.as_str())
                .is_ok()
                && self
                    .naming
                    .resolutions
                    .get(&value)
                    .is_some_and(|slot| !self.valued_slots.contains(*slot));
        }
        if !matches!(ty.kind(self.db), TyKind::Null) {
            return false;
        }
        // A local that a test narrowed to `NULL`, or a write replaced with `NULL`, also held a
        // real value, so its `NULL` is a path the code takes. A top-level variable read from
        // inside a function resolves through the project's definitions, whose other writes are
        // out of sight.
        self.naming
            .resolutions
            .get(&value)
            .is_none_or(|slot| !self.valued_slots.contains(*slot))
    }

    /// A function stored into a field at top level (`generator_funs$new <- function(...)`) is
    /// a template that the package copies into another environment, the way R6 rebinds its
    /// generator methods. A condition in it that names a shipped function, such as `if
    /// (class)`, reads a flag that environment binds, since no R code tests a function for
    /// truth. The same read in an ordinary function is a missing formal and stays reported,
    /// as does a name the project defines.
    fn tests_a_shipped_function(&self, condition: ExprId, ty: Ty<'db>) -> bool {
        let stored_in_field = self.module.root.is_some_and(|root| {
            matches!(
                self.module.kind(root),
                ExpressionKind::Assign { target, value, .. }
                    if matches!(self.module.kind(*target), ExpressionKind::Field { .. })
                        && matches!(self.module.kind(*value), ExpressionKind::Function { .. })
            )
        });
        if !stored_in_field {
            return false;
        }
        let mut condition = condition;
        while let ExpressionKind::Paren(inner) = self.module.kind(condition) {
            condition = *inner;
        }
        let Some(name) = self.naming.non_locals.get(&condition) else {
            return false;
        };
        let deferred = self.naming.deferred_non_locals.contains(&condition);
        matches!(ty.kind(self.db), TyKind::Function(_))
            && !self
                .globals
                .is_some_and(|globals| globals.defined_in_project(*name, deferred))
    }

    fn binds_parameter(&self, callee: ExprId) -> bool {
        self.naming
            .resolutions
            .get(&callee)
            .and_then(|slot| self.naming.bindings.get(slot))
            .is_some_and(|binding| binding.kind == BindingKind::Parameter)
    }

    /// A function that the body computes, from a `switch` without a default or a branch that
    /// leaves it `NULL`, is `NULL` only on paths that the code presumably rules out, such as a
    /// `switch` on a value `match.arg()` validated. A call there presumes the function. The
    /// `NULL` that a parameter's default or declared type admits is the caller's to supply,
    /// so a parameter keeps it and the call is reported.
    fn without_null_beside_functions(&self, ty: Ty<'db>) -> Ty<'db> {
        let resolved = self.table.shallow_resolve(self.db, ty);
        let TyKind::Union(members) = resolved.kind(self.db) else {
            return ty;
        };
        let is_function = |member: Ty<'db>| {
            matches!(
                self.table.shallow_resolve(self.db, member).kind(self.db),
                TyKind::Function(_)
            )
        };
        if !members.iter().any(|&member| is_function(member)) {
            return ty;
        }
        let kept: Vec<Ty<'db>> = members
            .iter()
            .copied()
            .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
            .collect();
        union_of(self.db, kept)
    }

    /// The type without its `NULL` members, or `None` when nothing else is left.
    fn without_null(&self, ty: Ty<'db>) -> Option<Ty<'db>> {
        let resolved = self.table.resolve(self.db, ty);
        match resolved.kind(self.db) {
            TyKind::Null => None,
            TyKind::Union(members)
                if members
                    .iter()
                    .any(|member| matches!(member.kind(self.db), TyKind::Null)) =>
            {
                Some(union_of(
                    self.db,
                    members
                        .iter()
                        .copied()
                        .filter(|member| !matches!(member.kind(self.db), TyKind::Null)),
                ))
            }
            _ => Some(ty),
        }
    }

    /// R looks a called name up as a function: a binding that holds no function is skipped,
    /// so the lookup continues through the enclosing slots, the project global and the
    /// library. A call head naming resolved to a slot is inferred here; any other one is
    /// left to the ordinary path unless its project global is plainly no function.
    fn function_lookup(&mut self, callee: ExprId) -> FunctionLookup<'db> {
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return FunctionLookup::Ordinary;
        };
        let name = *name;
        let Some(fallback) = self.naming.callee_fallbacks.get(&callee) else {
            let Some(non_local) = self.naming.non_locals.get(&callee) else {
                return FunctionLookup::Ordinary;
            };
            let deferred = self.naming.deferred_non_locals.contains(&callee);
            let Some(scheme) = self
                .globals
                .and_then(|globals| globals.scheme(*non_local, deferred))
            else {
                return FunctionLookup::Ordinary;
            };
            if self.callable(scheme.body) != Callable::No {
                return FunctionLookup::Ordinary;
            }
            if let Some(enclosing) = self
                .globals
                .and_then(|globals| globals.enclosing_scheme(*non_local))
                && self.callable(enclosing.body) != Callable::No
            {
                self.callee_targets.insert(callee, CalleeTarget::Project);
                let ty = self.instantiate(&enclosing);
                return FunctionLookup::Found(self.record(callee, ty));
            }
            return match self.library_function(name) {
                Some(candidates) => FunctionLookup::Library(name, candidates),
                None => FunctionLookup::Ordinary,
            };
        };
        let Some(&nearest_slot) = self.naming.resolutions.get(&callee) else {
            return FunctionLookup::Ordinary;
        };
        let outer = fallback.outer.clone();
        let deferred = fallback.deferred;
        let nearest = self.infer(callee);
        let nearest_callable = self.callable(nearest);
        if nearest_callable == Callable::Yes {
            return FunctionLookup::Found(nearest);
        }
        // An untyped nearest binding that is only ever called is a callback, so it is the
        // function. One that is also read as a value (`dir <- path; dir(dir)`) is as likely a
        // plain value whose call R resolves past it, so a same-named function leaves the call
        // undetermined.
        let ambiguous = nearest_callable == Callable::Untyped && self.read_as_value(nearest_slot);
        let mut reaches_top_level = self.is_top_level_slot(nearest_slot);
        for slot in outer {
            reaches_top_level |= self.is_top_level_slot(slot);
            let ty = self.read_slot(callee, slot);
            match self.callable(ty) {
                Callable::No => continue,
                _ if ambiguous => return FunctionLookup::Undetermined,
                _ if nearest_callable == Callable::Untyped => {
                    return FunctionLookup::Found(nearest);
                }
                _ => {
                    self.callee_targets.insert(callee, CalleeTarget::Slot(slot));
                    return FunctionLookup::Found(self.record(callee, ty));
                }
            }
        }
        let project = self.globals.and_then(|globals| match reaches_top_level {
            false => globals.scheme(Symbol::new(&name), deferred),
            true => globals.enclosing_scheme(Symbol::new(&name)),
        });
        if let Some(scheme) = project
            && self.callable(scheme.body) != Callable::No
        {
            if ambiguous {
                return FunctionLookup::Undetermined;
            }
            if nearest_callable == Callable::Untyped {
                return FunctionLookup::Found(nearest);
            }
            self.callee_targets.insert(callee, CalleeTarget::Project);
            let ty = self.instantiate(&scheme);
            return FunctionLookup::Found(self.record(callee, ty));
        }
        match self.library_function(name) {
            Some(_) if ambiguous => FunctionLookup::Undetermined,
            Some(_) if nearest_callable == Callable::Untyped => FunctionLookup::Found(nearest),
            Some(candidates) => FunctionLookup::Library(name, candidates),
            // A package ry has no export list for may export the function R finds
            // (`path(path, "R")` under `import(fs)`), past a nearest binding that is plainly
            // no function as much as past an ambiguous one.
            None if (ambiguous || nearest_callable == Callable::No)
                && (imported_by_name(self.db, &name) || imports_every_name(self.db)) =>
            {
                FunctionLookup::Undetermined
            }
            // Nothing else is a function either: R reports the call, and so does the checker.
            None => FunctionLookup::Found(nearest),
        }
    }

    /// A call on a parameter whose function type an earlier call inferred (`handler(src)`), with
    /// another argument count, where the body reads `formals()` of that parameter. Such code
    /// picks the call by the callback's arity (`if (length(formals(handler)) == 2) handler(src,
    /// call)`), so the inferred shape is no arity to check against.
    fn arity_tested_by_formals(
        &self,
        callee: ExprId,
        callee_ty: Ty<'db>,
        arguments: &[CallArgument<'db>],
    ) -> bool {
        let db = self.db;
        let Some(&slot) = self.naming.resolutions.get(&callee) else {
            return false;
        };
        let resolved = self.table.shallow_resolve(db, callee_ty);
        let members = match resolved.kind(db) {
            TyKind::Union(members) => members.to_vec(),
            _ => vec![resolved],
        };
        let differs =
            members.iter().any(
                |&member| match self.table.shallow_resolve(db, member).kind(db) {
                    TyKind::Function(function) => {
                        function.inferred
                            && !function.positional.is_empty()
                            && function.variadic.is_none()
                            && arguments.len() != function.positional.len() + function.named.len()
                    }
                    _ => false,
                },
            );
        if !differs || arguments.iter().any(|argument| argument.forwards_dots) {
            return false;
        }
        let module = self.module;
        module.expression_ids().any(|id| {
            let ExpressionKind::Call {
                callee: inspector,
                arguments: inspected,
            } = module.kind(id)
            else {
                return false;
            };
            matches!(module.kind(*inspector), ExpressionKind::NameRef(name) if matches!(name.as_str(), "formals" | "formalArgs"))
                && module[*inspected].first().and_then(|argument| argument.value).is_some_and(
                    |value| self.naming.resolutions.get(&value) == Some(&slot),
                )
        })
    }

    fn read_as_value(&mut self, slot: BindingId) -> bool {
        let naming = self.naming;
        let arena = self.arena;
        self.value_read_slots
            .get_or_insert_with(|| {
                let mut slots = Bitset::new_in(arena, naming.bindings.len());
                for (read, slot) in naming.resolutions.iter() {
                    if !naming.callee_fallbacks.contains_key(read) {
                        slots.insert(*slot);
                    }
                }
                slots
            })
            .contains(slot)
    }

    fn is_top_level_slot(&self, slot: BindingId) -> bool {
        self.naming
            .bindings
            .get(&slot)
            .is_some_and(|binding| binding.kind == BindingKind::TopLevel)
    }

    /// The library's candidates for `name` when the last one, which a plain read takes, is a
    /// function.
    fn library_function(&mut self, name: Symbol) -> Option<&'db [TypeScheme<'db>]> {
        let candidates = self.globals?.library_candidates(name)?;
        match candidates.last() {
            Some(last) if self.callable(last.body) == Callable::No => None,
            _ => Some(candidates),
        }
    }

    /// The scheme-arena index of the polymorphic function a name reads: a slot holding a
    /// scheme, or a project or library function with a single generic declaration.
    fn function_scheme(&mut self, function: ExprId) -> Option<u32> {
        let ExpressionKind::NameRef(name) = self.module.kind(function) else {
            return None;
        };
        if let Some(&slot) = self.naming.resolutions.get(&function) {
            return match self.environment.get(slot) {
                Some(EnvEntry::Scheme(index)) => Some(index),
                _ => None,
            };
        }
        let deferred = self.naming.deferred_non_locals.contains(&function);
        let scheme = match self
            .globals
            .and_then(|globals| globals.scheme(*name, deferred))
        {
            Some(scheme) => scheme,
            None => match self.library_function(*name)? {
                [single] => single.clone(),
                _ => return None,
            },
        };
        if scheme.binders.is_empty() {
            return None;
        }
        let index = self.scheme_arena.len() as u32;
        self.scheme_arena.push(scheme);
        Some(index)
    }

    /// Whether a value of type `ty` can be called. An unconstrained variable and a type
    /// parameter are `Untyped`, since nothing says whether they hold a function.
    fn callable(&mut self, ty: Ty<'db>) -> Callable {
        let resolved = self.structural(ty);
        match resolved.kind(self.db) {
            TyKind::Function(_) | TyKind::Any | TyKind::Unknown => Callable::Yes,
            // A nominal left after projection is abstract (`matrix`, `Date`, `data.frame`): an R
            // object with a class, never a closure, so R's function lookup skips it.
            TyKind::Named(..) | TyKind::Native(_) => Callable::No,
            TyKind::Rigid(_) => Callable::Untyped,
            TyKind::Var(_) => match self.table.open_constraint(self.db, resolved) {
                Some(Constraint::Unconstrained) => Callable::Untyped,
                _ => Callable::No,
            },
            TyKind::Union(members) => members
                .iter()
                .map(|&member| self.callable(member))
                .max()
                .unwrap_or(Callable::No),
            TyKind::Null
            | TyKind::Scalar(_)
            | TyKind::Vector(_)
            | TyKind::NamedVector(_)
            | TyKind::List(_)
            | TyKind::NamedList(_)
            | TyKind::Tuple(_)
            | TyKind::Record(_)
            | TyKind::OpenRecord(_)
            | TyKind::Environment(_)
            | TyKind::NativeTuple(_)
            | TyKind::NativeRecord(_)
            | TyKind::Tag(..)
            | TyKind::OtherTags
            | TyKind::Frame(..) => Callable::No,
        }
    }

    /// `DataFrame(id = 1:3, price = c(1, 2, 3))` builds `DataFrame{id: integer[], price: double[]}`.
    /// A project's own `DataFrame` shadows the builtin, so it falls through to the ordinary call.
    fn infer_frame_constructor(
        &mut self,
        callee: ExprId,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let deferred = self.naming.deferred_non_locals.contains(&callee);
        if self.naming.resolutions.contains_key(&callee)
            || self.globals.is_some_and(|globals| {
                globals.defined_in_project(Symbol::new("DataFrame"), deferred)
            })
        {
            return None;
        }
        let module = self.module;
        let mut columns = Some(Vec::with_capacity(arguments.len()));
        for argument in arguments {
            let Some(value) = argument.value else {
                columns = None;
                continue;
            };
            let ty = self.infer(value);
            // `...` forwards columns ry cannot name.
            let forwards_dots =
                matches!(module.kind(value), ExpressionKind::NameRef(name) if name == "...");
            let name = module
                .argument_name(argument)
                .or_else(|| variable_name(module, value).filter(|_| !forwards_dots));
            let field = name.map(|name| self.column_field(&name, ty));
            match (field, columns.as_mut()) {
                (Some(field), Some(columns))
                    if !columns
                        .iter()
                        .any(|column: &RecordField<'db>| column.name == field.name) =>
                {
                    columns.push(field);
                }
                _ => columns = None,
            }
        }
        Some(Ty::new(
            self.db,
            TyKind::Frame(FrameKind::DataFrame, columns.as_deref()),
        ))
    }

    /// The data.table function a call names, when the name reaches its stub: a local or
    /// project definition of the same name is an ordinary function.
    fn table_function(&self, callee: ExprId) -> Option<TableFunction> {
        let (name, package) = match self.module.kind(callee) {
            ExpressionKind::NameRef(name) => (*name, None),
            ExpressionKind::Namespace {
                package: Some(package),
                name: Some(name),
                ..
            } => (*name, Some(*package)),
            _ => return None,
        };
        let function = match name.as_str() {
            "data.frame" => TableFunction::Construct(FrameKind::RDataFrame),
            "data.table" => TableFunction::Construct(FrameKind::DataTable),
            "fread" => TableFunction::Fread,
            "setnames" => TableFunction::SetNames,
            "setcolorder" => TableFunction::SetColumnOrder,
            "setDT" => TableFunction::ConvertInPlace(FrameKind::DataTable),
            "setDF" => TableFunction::ConvertInPlace(FrameKind::RDataFrame),
            "set" | "setattr" => TableFunction::SetInPlace,
            "setkey" => TableFunction::SetKey,
            "setorder" => TableFunction::SetOrder,
            "setkeyv" | "setorderv" => TableFunction::SetKeyOrOrderNamed,
            "rbindlist" => TableFunction::RbindList,
            "fifelse" => TableFunction::Fifelse,
            "melt" => TableFunction::Melt,
            _ => return None,
        };
        let home = match function {
            TableFunction::Construct(FrameKind::RDataFrame) => "base",
            _ => "data.table",
        };
        match package {
            // A conditional package's stubs join resolution only when the project declares
            // it; otherwise `pkg::name` is `Unknown`.
            Some(package) => (package.as_str() == home
                && self.recorded.get(&callee).is_some_and(|&callee| {
                    !matches!(callee.kind(self.db), TyKind::Unknown | TyKind::Any)
                }))
            .then_some(function),
            None => self.reaches_library(callee).then_some(function),
        }
    }

    /// Whether a call head names a declared library function: a local or project definition of
    /// the same name is an ordinary function.
    fn reaches_library(&self, callee: ExprId) -> bool {
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return false;
        };
        let deferred = self.naming.deferred_non_locals.contains(&callee);
        self.globals.is_some_and(|globals| {
            !self.naming.resolutions.contains_key(&callee)
                && !globals.defined_in_project(*name, deferred)
                && globals.scheme(*name, deferred).is_some()
        })
    }

    /// The DataFrame verb a call head names, when the name reaches the library.
    fn dataframe_verb(&self, callee: ExprId) -> Option<DataFrameVerb> {
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return None;
        };
        // The conversions between ry's table and data.table keep every column and its type.
        let conversion = match &**name {
            "as.DataFrame" => Some(FrameKind::DataFrame),
            "as.data.table" => Some(FrameKind::DataTable),
            _ => None,
        };
        if let Some(kind) = conversion {
            return self
                .reaches_library(callee)
                .then_some(DataFrameVerb::Convert(kind));
        }
        let (receiver, verb) = name.split_once('.')?;
        let lazy = match receiver {
            "DataFrame" => false,
            "LazyFrame" => true,
            "GroupBy" | "LazyGroupBy" if verb == "agg" => {
                return self.reaches_library(callee).then_some(DataFrameVerb::Agg);
            }
            _ => return None,
        };
        let verb = match verb {
            "select" => DataFrameVerb::Select { lazy },
            "with_columns" => DataFrameVerb::WithColumns { lazy },
            "filter" | "sort" | "head" | "tail" => DataFrameVerb::KeepColumns { lazy },
            "group_by" => DataFrameVerb::GroupBy { lazy },
            "join" => DataFrameVerb::Join { lazy, asof: false },
            "join_asof" => DataFrameVerb::Join { lazy, asof: true },
            "lazy" => DataFrameVerb::Convert(FrameKind::LazyFrame),
            "collect" => DataFrameVerb::Convert(FrameKind::DataFrame),
            "read_csv" if !lazy => DataFrameVerb::ReadCsv(FrameKind::DataFrame),
            "scan_csv" if lazy => DataFrameVerb::ReadCsv(FrameKind::LazyFrame),
            _ => return None,
        };
        self.reaches_library(callee).then_some(verb)
    }

    /// Carries a DataFrame's schema through a verb: the columns each expression argument computes
    /// are read from its syntax, and a string naming a column must name one the frame has. The
    /// ordinary call has already checked every argument against the verb's declaration, so this
    /// only sharpens its result. `None` keeps the declared result, a frame with unknown columns.
    fn refine_dataframe_call(
        &mut self,
        verb: DataFrameVerb,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let db = self.db;
        let module = self.module;
        let subject = arguments.first()?.value?;
        if let DataFrameVerb::ReadCsv(kind) = verb {
            return self.read_csv_type(kind, arguments);
        }
        let subject_ty = *self.recorded.get(&subject)?;
        let rest = &arguments[1..];
        let (subject_kind, columns) = match *self.structural(subject_ty).kind(db) {
            TyKind::Frame(kind, Some(columns)) => (kind, columns),
            TyKind::Frame(_, None) => {
                self.note_unchecked_columns(subject, rest);
                return None;
            }
            _ => return None,
        };
        let frame = variable_name(module, subject);
        let expressions = |checker: &mut Self, keep: &dyn Fn(&str) -> bool| {
            let mut computed = Vec::with_capacity(rest.len());
            let mut known = true;
            for argument in rest {
                let Some(value) = argument.value else {
                    continue;
                };
                if self
                    .module
                    .argument_name(argument)
                    .as_deref()
                    .is_some_and(|name| !keep(name))
                {
                    continue;
                }
                match checker.frame_argument(value, columns, frame) {
                    Some(mut column) => {
                        if let Some(name) = module.argument_name(argument) {
                            column.name = name.to_string();
                        }
                        computed.push(column);
                    }
                    None => known = false,
                }
            }
            known.then_some(computed)
        };
        let fields = |computed: Vec<ComputedColumn<'db>>| -> Vec<(String, Ty<'db>)> {
            computed
                .into_iter()
                .map(|column| (column.name, column.ty))
                .collect()
        };
        let existing = |columns: &[RecordField<'db>]| -> Vec<(String, Ty<'db>)> {
            columns
                .iter()
                .map(|column| (column.name.text(db).to_owned(), column.ty))
                .collect()
        };
        let frame_kind = |lazy: bool| {
            if lazy {
                FrameKind::LazyFrame
            } else {
                FrameKind::DataFrame
            }
        };
        match verb {
            DataFrameVerb::Select { lazy } => {
                let computed = expressions(self, &|_| true)?;
                Some(frame_of(db, frame_kind(lazy), fields(computed)))
            }
            DataFrameVerb::WithColumns { lazy } => {
                let computed = expressions(self, &|_| true)?;
                let mut result = existing(columns);
                for (name, ty) in fields(computed) {
                    match result.iter_mut().find(|(existing, _)| *existing == name) {
                        Some(slot) => slot.1 = ty,
                        None => result.push((name, ty)),
                    }
                }
                Some(frame_of(db, frame_kind(lazy), result))
            }
            DataFrameVerb::KeepColumns { lazy } => {
                expressions(self, &|name| name != "descending" && name != "n");
                Some(Ty::new(db, TyKind::Frame(frame_kind(lazy), Some(columns))))
            }
            DataFrameVerb::GroupBy { lazy } => {
                let keys = fields(expressions(self, &|_| true)?);
                let key_count = u32::try_from(keys.len()).ok()?;
                let mut result = keys;
                for (name, ty) in existing(columns) {
                    if !result.iter().any(|(key, _)| *key == name) {
                        result.push((name, ty));
                    }
                }
                Some(frame_of(
                    db,
                    FrameKind::GroupBy {
                        lazy,
                        keys: key_count,
                    },
                    result,
                ))
            }
            DataFrameVerb::Agg => {
                let FrameKind::GroupBy { lazy, keys } = subject_kind else {
                    return None;
                };
                let computed = expressions(self, &|_| true)?;
                for (argument, column) in rest
                    .iter()
                    .filter_map(|argument| argument.value)
                    .zip(&computed)
                {
                    let kind = match column.shape {
                        Shape::PerRow => TypeErrorKind::NotAnAggregation {
                            column: column.name.clone(),
                        },
                        Shape::NestedHead => TypeErrorKind::HeadInsideExpression {
                            column: column.name.clone(),
                        },
                        Shape::PerGroup | Shape::Head => continue,
                    };
                    self.errors.push(TypeError {
                        range: self.blame_range(argument),
                        kind,
                    });
                }
                let mut result = existing(columns.get(..usize::try_from(keys).ok()?)?);
                result.extend(fields(computed));
                Some(frame_of(db, frame_kind(lazy), result))
            }
            DataFrameVerb::Join { lazy, asof } => self.join_type(lazy, asof, columns, frame, rest),
            DataFrameVerb::Convert(_) if matches!(subject_kind, FrameKind::GroupBy { .. }) => None,
            DataFrameVerb::Convert(kind) => Some(Ty::new(db, TyKind::Frame(kind, Some(columns)))),
            DataFrameVerb::ReadCsv(_) => None,
        }
    }

    /// Strict mode reports a query that names columns of a frame whose columns are unknown,
    /// because nothing checks those names and types. A chain is reported once, at its first
    /// step that names a column.
    fn note_unchecked_columns(&mut self, subject: ExprId, rest: &[Argument]) {
        let module = self.module;
        let Some(column) = first_named_column(module, rest) else {
            return;
        };
        // Steps that name no column, such as `DataFrame.head(5L)` or `DataFrame.lazy()`,
        // reported nothing, so the walk looks through them for an earlier step that did.
        let mut inner = subject;
        loop {
            while let ExpressionKind::Paren(expression) = module.kind(inner) {
                inner = *expression;
            }
            let ExpressionKind::Call { callee, arguments } = module.kind(inner) else {
                break;
            };
            if self.dataframe_verb(*callee).is_none() {
                break;
            }
            let Some((first, inner_rest)) = module[arguments].split_first() else {
                break;
            };
            let Some(inner_subject) = first.value else {
                break;
            };
            let Some(&ty) = self.recorded.get(&inner_subject) else {
                break;
            };
            if !matches!(self.structural(ty).kind(self.db), TyKind::Frame(_, None)) {
                break;
            }
            if first_named_column(module, inner_rest).is_some() {
                return;
            }
            inner = inner_subject;
        }
        let frame = variable_name(module, subject).map(|name| name.to_string());
        self.record_strict_origin(subject, StrictOriginKind::UnknownColumns { frame, column });
    }

    /// `DataFrame.join(left, right, on = "id")` has the left's columns, then the right's columns
    /// other than its keys, with `suffix` added to a name the left already has. A semi or anti
    /// join keeps only the left's columns, and a cross join has no keys.
    /// `DataFrame.join_asof(left, right, on, by)` joins on `by` and then `on`, with a left join's
    /// columns.
    fn join_type(
        &mut self,
        lazy: bool,
        asof: bool,
        left: &[RecordField<'db>],
        frame: Option<Symbol>,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let db = self.db;
        let module = self.module;
        let formals: &[&str] = if asof {
            &["other", "on", "by", "strategy", "suffix"]
        } else {
            &["other", "on", "how", "left_on", "right_on", "suffix"]
        };
        let matched = match_formals(self.module, arguments, formals);
        let other = matched[0]?;
        let other_ty = *self.recorded.get(&other)?;
        let right = match *self.structural(other_ty).kind(db) {
            TyKind::Frame(FrameKind::DataFrame | FrameKind::LazyFrame, Some(columns)) => columns,
            _ => return None,
        };
        let string = |position: usize| -> Option<Option<String>> {
            match matched[position] {
                None => Some(None),
                Some(value) => match module.kind(value) {
                    ExpressionKind::Literal(LiteralKind::String(text)) => {
                        Some(Some(module[text].to_string()))
                    }
                    _ => None,
                },
            }
        };
        let keys = |position: usize| matched[position].map(|value| literal_strings(module, value));
        let (how, suffix, left_keys, right_keys) = if asof {
            let by_null = matched[2].is_some_and(|value| {
                matches!(
                    module.kind(value),
                    ExpressionKind::Literal(LiteralKind::Null)
                )
            });
            let mut keys_of_both = if by_null {
                Vec::new()
            } else {
                keys(2).unwrap_or(Some(Vec::new()))?
            };
            keys_of_both.extend(keys(1)??);
            let suffix = string(4)?.unwrap_or_else(|| "_right".to_owned());
            (
                "left".to_owned(),
                suffix,
                keys_of_both.clone(),
                keys_of_both,
            )
        } else {
            let how = string(2)?.unwrap_or_else(|| "inner".to_owned());
            let suffix = string(5)?.unwrap_or_else(|| "_right".to_owned());
            let (left_keys, right_keys) = match (keys(1), keys(3), keys(4)) {
                (Some(on), None, None) => (on.clone()?, on?),
                (None, Some(left_on), Some(right_on)) => (left_on?, right_on?),
                // A cross join pairs every row with every row, so it has no keys.
                (None, None, None) if how == "cross" => (Vec::new(), Vec::new()),
                _ => return None,
            };
            (how, suffix, left_keys, right_keys)
        };
        let other_name = variable_name(module, other);
        // `on` names each key once for both frames, so a key missing from both is reported once,
        // against the left frame.
        let mut reported: Vec<TextRange> = Vec::new();
        for (keys, columns, name) in [(&left_keys, left, frame), (&right_keys, right, other_name)] {
            for (key, range) in keys {
                if !columns.iter().any(|column| column.name.text(db) == key)
                    && !reported.contains(range)
                {
                    reported.push(*range);
                    self.errors.push(TypeError {
                        range: *range,
                        kind: column_does_not_exist(db, key, name, columns),
                    });
                }
            }
        }
        if !reported.is_empty() {
            return None;
        }
        let mut result: Vec<(String, Ty<'db>)> = left
            .iter()
            .map(|column| (column.name.text(db).to_owned(), column.ty))
            .collect();
        if !matches!(how.as_str(), "semi" | "anti") {
            for column in right.iter() {
                let name = column.name.text(db);
                if right_keys.iter().any(|(key, _)| key == name) {
                    continue;
                }
                let name = if result.iter().any(|(existing, _)| existing == name) {
                    format!("{name}{suffix}")
                } else {
                    name.to_owned()
                };
                result.push((name, column.ty));
            }
        }
        let kind = if lazy {
            FrameKind::LazyFrame
        } else {
            FrameKind::DataFrame
        };
        Some(frame_of(db, kind, result))
    }

    /// The column a verb's argument computes: a string names a column, and anything else is an
    /// expression. `None` when the argument is not an expression ry can read, such as a variable
    /// holding one.
    fn frame_argument(
        &mut self,
        value: ExprId,
        columns: &[RecordField<'db>],
        frame: Option<Symbol>,
    ) -> Option<ComputedColumn<'db>> {
        let expression = value;
        if let ExpressionKind::Literal(LiteralKind::String(text)) = self.module.kind(expression) {
            return Some(self.column_reference(
                &self.module[text],
                self.module.range(expression),
                columns,
                frame,
            ));
        }
        self.frame_expression(value, columns, frame)
    }

    fn column_reference(
        &mut self,
        name: &str,
        range: TextRange,
        columns: &[RecordField<'db>],
        frame: Option<Symbol>,
    ) -> ComputedColumn<'db> {
        let db = self.db;
        let ty = match columns.iter().find(|column| column.name.text(db) == name) {
            Some(column) => column.ty,
            None => {
                self.errors.push(TypeError {
                    range,
                    kind: column_does_not_exist(db, name, frame, columns),
                });
                unknown(db)
            }
        };
        ComputedColumn {
            name: name.to_owned(),
            ty,
            reads_column: true,
            shape: Shape::PerRow,
        }
    }

    /// The column an expression computes, following Polars' naming: an unaliased expression
    /// takes the name of the first column it reads, `len` for a row count, and `literal` for a
    /// constant.
    fn frame_expression(
        &mut self,
        value: ExprId,
        columns: &[RecordField<'db>],
        frame: Option<Symbol>,
    ) -> Option<ComputedColumn<'db>> {
        let db = self.db;
        let module = self.module;
        let vector = |atomic: Atomic| Ty::new(db, TyKind::Vector(scalar(db, atomic)));
        match module.kind(value) {
            ExpressionKind::Paren(inner) => self.frame_expression(*inner, columns, frame),
            ExpressionKind::Call { callee, arguments } => {
                if !self.reaches_library(*callee) {
                    return None;
                }
                let name = variable_name(module, *callee)?;
                let first = module[arguments]
                    .first()
                    .and_then(|argument| argument.value);
                let argument =
                    |checker: &mut Self| checker.frame_expression(first?, columns, frame);
                let aggregate = |column: ComputedColumn<'db>, ty: Ty<'db>| ComputedColumn {
                    ty,
                    shape: Shape::PerGroup,
                    ..column
                };
                match name.as_str() {
                    "Expr.col" => {
                        let first = first?;
                        let ExpressionKind::Literal(LiteralKind::String(text)) = module.kind(first)
                        else {
                            return None;
                        };
                        Some(self.column_reference(
                            &module[text],
                            module.range(first),
                            columns,
                            frame,
                        ))
                    }
                    "Expr.lit" => {
                        let ty = column_type(db, *self.recorded.get(&first?)?);
                        Some(ComputedColumn {
                            name: "literal".to_owned(),
                            ty,
                            reads_column: false,
                            shape: Shape::PerRow,
                        })
                    }
                    "Expr.len" => Some(ComputedColumn {
                        name: "len".to_owned(),
                        ty: vector(Atomic::Integer),
                        reads_column: false,
                        shape: Shape::PerGroup,
                    }),
                    "Expr.alias" => {
                        let column = argument(self)?;
                        let alias = module[arguments].get(1)?.value?;
                        let ExpressionKind::Literal(LiteralKind::String(alias)) =
                            module.kind(alias)
                        else {
                            return None;
                        };
                        Some(ComputedColumn {
                            name: module[alias].to_string(),
                            ..column
                        })
                    }
                    "Expr.sum" => {
                        let column = argument(self)?;
                        self.check_expression_operand(
                            name.as_str(),
                            first?,
                            &column,
                            ExpressionNeeds::Numbers,
                        );
                        let ty = match element_atomic(db, column.ty) {
                            Some(Atomic::Logical | Atomic::Integer) => vector(Atomic::Integer),
                            _ => vector(Atomic::Double),
                        };
                        Some(aggregate(column, ty))
                    }
                    "Expr.mean" | "Expr.median" | "Expr.std" | "Expr.var" => {
                        let column = argument(self)?;
                        self.check_expression_operand(
                            name.as_str(),
                            first?,
                            &column,
                            ExpressionNeeds::Numbers,
                        );
                        Some(aggregate(column, vector(Atomic::Double)))
                    }
                    "Expr.min" | "Expr.max" | "Expr.first" | "Expr.last" => {
                        let column = argument(self)?;
                        // R's `min` and `max` of logicals are integers, and the engine agrees.
                        let ty = match element_atomic(db, column.ty) {
                            Some(Atomic::Logical)
                                if matches!(name.as_str(), "Expr.min" | "Expr.max") =>
                            {
                                vector(Atomic::Integer)
                            }
                            _ => column.ty,
                        };
                        Some(aggregate(column, ty))
                    }
                    "Expr.count" => {
                        let column = argument(self)?;
                        Some(aggregate(column, vector(Atomic::Integer)))
                    }
                    "Expr.head" => {
                        let column = argument(self)?;
                        Some(ComputedColumn {
                            shape: Shape::Head,
                            ..column
                        })
                    }
                    "Expr.over" => {
                        let column = argument(self)?;
                        for key in module[arguments].iter().skip(1) {
                            let key = key.value?;
                            let ExpressionKind::Literal(LiteralKind::String(text)) =
                                module.kind(key)
                            else {
                                return None;
                            };
                            self.column_reference(&module[text], module.range(key), columns, frame);
                        }
                        Some(ComputedColumn {
                            shape: Shape::PerRow,
                            ..column
                        })
                    }
                    "Expr.rank" => {
                        let column = argument(self)?;
                        Some(ComputedColumn {
                            ty: vector(Atomic::Integer),
                            ..column
                        })
                    }
                    "Expr.is_null" => {
                        let column = argument(self)?;
                        Some(ComputedColumn {
                            ty: vector(Atomic::Logical),
                            ..column
                        })
                    }
                    "Expr.cor" => {
                        let column = argument(self)?;
                        let second = module[arguments].get(1)?.value?;
                        let other = self.frame_operand(second, columns, frame)?;
                        for (operand, value) in [(&column, first?), (&other, second)] {
                            self.check_expression_operand(
                                name.as_str(),
                                value,
                                operand,
                                ExpressionNeeds::Numbers,
                            );
                        }
                        Some(aggregate(column, vector(Atomic::Double)))
                    }
                    "Expr.if_else" => {
                        let test = self.frame_operand(first?, columns, frame)?;
                        self.check_expression_operand(
                            name.as_str(),
                            first?,
                            &test,
                            ExpressionNeeds::Logical,
                        );
                        let yes =
                            self.frame_operand(module[arguments].get(1)?.value?, columns, frame)?;
                        let no =
                            self.frame_operand(module[arguments].get(2)?.value?, columns, frame)?;
                        let ty = if yes.ty == no.ty {
                            yes.ty
                        } else {
                            union_of(db, [yes.ty, no.ty])
                        };
                        let name = if yes.reads_column || !no.reads_column {
                            yes.name
                        } else {
                            no.name
                        };
                        Some(ComputedColumn {
                            name,
                            ty,
                            reads_column: yes.reads_column || no.reads_column,
                            shape: yes.shape.combine(no.shape),
                        })
                    }
                    _ => None,
                }
            }
            ExpressionKind::Unary { operator, operand } => {
                let column = self.frame_expression(*operand, columns, frame)?;
                match operator {
                    UnaryOperator::Not => {
                        self.check_expression_operand(
                            "!",
                            *operand,
                            &column,
                            ExpressionNeeds::LogicalOrNumbers,
                        );
                        Some(ComputedColumn {
                            ty: vector(Atomic::Logical),
                            ..column
                        })
                    }
                    UnaryOperator::Minus | UnaryOperator::Plus => {
                        let spelling = if *operator == UnaryOperator::Minus {
                            "-"
                        } else {
                            "+"
                        };
                        // A negated string keeps its text type, so a reported operand ends the walk
                        // rather than being reported again by every enclosing operator.
                        self.check_expression_operand(
                            spelling,
                            *operand,
                            &column,
                            ExpressionNeeds::Numbers,
                        )
                        .then_some(column)
                    }
                    _ => None,
                }
            }
            ExpressionKind::Binary {
                operator, lhs, rhs, ..
            } => {
                let left = self.frame_operand(*lhs, columns, frame)?;
                let right = self.frame_operand(*rhs, columns, frame)?;
                if !left.reads_column
                    && !right.reads_column
                    && !matches!(module.kind(*lhs), ExpressionKind::Call { .. })
                {
                    return None;
                }
                if is_date_column(db, left.ty) || is_date_column(db, right.ty) {
                    let ty =
                        self.date_operation(value, *operator, (*lhs, &left), (*rhs, &right))?;
                    return Some(ComputedColumn {
                        name: if left.reads_column || !right.reads_column {
                            left.name
                        } else {
                            right.name
                        },
                        ty,
                        reads_column: left.reads_column || right.reads_column,
                        shape: left.shape.combine(right.shape),
                    });
                }
                self.check_expression_operator(value, *operator, (*lhs, &left), (*rhs, &right));
                let ty = match operator {
                    BinaryOperator::Add
                    | BinaryOperator::Subtract
                    | BinaryOperator::Multiply
                    | BinaryOperator::Modulo
                    | BinaryOperator::IntegerDivide => {
                        let integer = |ty| {
                            matches!(
                                element_atomic(db, ty),
                                Some(Atomic::Logical | Atomic::Integer)
                            )
                        };
                        if integer(left.ty) && integer(right.ty) {
                            vector(Atomic::Integer)
                        } else {
                            vector(Atomic::Double)
                        }
                    }
                    BinaryOperator::Divide | BinaryOperator::Power => vector(Atomic::Double),
                    BinaryOperator::Less
                    | BinaryOperator::Greater
                    | BinaryOperator::LessEq
                    | BinaryOperator::GreaterEq
                    | BinaryOperator::Equal
                    | BinaryOperator::NotEqual
                    | BinaryOperator::And
                    | BinaryOperator::Or => vector(Atomic::Logical),
                    _ => return None,
                };
                let name = if left.reads_column || !right.reads_column {
                    left.name
                } else {
                    right.name
                };
                Some(ComputedColumn {
                    name,
                    ty,
                    reads_column: left.reads_column || right.reads_column,
                    shape: left.shape.combine(right.shape),
                })
            }
            _ => None,
        }
    }

    /// Types a binary operator with a `Date` operand, which the engine stores as whole days: a
    /// date plus or minus a number of days is a date, one date minus another is the integer
    /// number of days between them, and two dates compare. Anything else on a date is reported,
    /// and gives `None` so that the enclosing expression is not reported again.
    fn date_operation(
        &mut self,
        value: ExprId,
        operator: BinaryOperator,
        (lhs, left): (ExprId, &ComputedColumn<'db>),
        (rhs, right): (ExprId, &ComputedColumn<'db>),
    ) -> Option<Ty<'db>> {
        let db = self.db;
        let is_date = |ty| is_date_column(db, ty);
        let days =
            |ty| matches!(join_class(db, ty), None | Some(JoinClass::Number)) && !is_date(ty);
        let result = match operator {
            BinaryOperator::Add if is_date(left.ty) && days(right.ty) => Some(left.ty),
            BinaryOperator::Add if days(left.ty) && is_date(right.ty) => Some(right.ty),
            BinaryOperator::Subtract if is_date(left.ty) && days(right.ty) => Some(left.ty),
            BinaryOperator::Subtract if is_date(left.ty) && is_date(right.ty) => {
                Some(Ty::new(db, TyKind::Vector(scalar(db, Atomic::Integer))))
            }
            BinaryOperator::Less
            | BinaryOperator::Greater
            | BinaryOperator::LessEq
            | BinaryOperator::GreaterEq
            | BinaryOperator::Equal
            | BinaryOperator::NotEqual
                if is_date(left.ty) && is_date(right.ty) =>
            {
                Some(Ty::new(db, TyKind::Vector(scalar(db, Atomic::Logical))))
            }
            _ => None,
        };
        if result.is_none() {
            let operator = match operator {
                BinaryOperator::And => "&",
                BinaryOperator::Or => "|",
                _ => operator_spelling(operator)?,
            };
            let kind = TypeErrorKind::DateOperation {
                operator,
                left: self.shown_operand_type(lhs, left),
                right: self.shown_operand_type(rhs, right),
            };
            self.errors.push(TypeError {
                range: self.blame_range(value),
                kind,
            });
        }
        result
    }

    /// Checks a binary operator's operands against what the table engine accepts: arithmetic
    /// takes numbers and logicals, `&` and `|` take logicals and numbers, and a comparison never
    /// pairs strings or factors with numbers. The engine compares an integer with a string by
    /// formatting it, as R does, but ry reports that pairing too, because a schema that puts a
    /// number against text is almost always a mistaken column.
    fn check_expression_operator(
        &mut self,
        value: ExprId,
        operator: BinaryOperator,
        (lhs, left): (ExprId, &ComputedColumn<'db>),
        (rhs, right): (ExprId, &ComputedColumn<'db>),
    ) {
        let db = self.db;
        let needs = match operator {
            BinaryOperator::Add
            | BinaryOperator::Subtract
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Power
            | BinaryOperator::Modulo
            | BinaryOperator::IntegerDivide => ExpressionNeeds::Numbers,
            BinaryOperator::And | BinaryOperator::Or => ExpressionNeeds::LogicalOrNumbers,
            BinaryOperator::Less
            | BinaryOperator::Greater
            | BinaryOperator::LessEq
            | BinaryOperator::GreaterEq
            | BinaryOperator::Equal
            | BinaryOperator::NotEqual => {
                let classes = (join_class(db, left.ty), join_class(db, right.ty));
                if let (Some(JoinClass::Text), Some(JoinClass::Number))
                | (Some(JoinClass::Number), Some(JoinClass::Text)) = classes
                    && let Some(spelling) = operator_spelling(operator)
                {
                    let kind = TypeErrorKind::ExpressionComparison {
                        operator: spelling,
                        left: self.shown_operand_type(lhs, left),
                        right: self.shown_operand_type(rhs, right),
                    };
                    self.errors.push(TypeError {
                        range: self.blame_range(value),
                        kind,
                    });
                }
                return;
            }
            _ => return,
        };
        let spelling = match operator {
            BinaryOperator::And => "&",
            BinaryOperator::Or => "|",
            _ => match operator_spelling(operator) {
                Some(spelling) => spelling,
                None => return,
            },
        };
        self.check_expression_operand(spelling, lhs, left, needs);
        self.check_expression_operand(spelling, rhs, right, needs);
    }

    /// Reports an operand whose known column type `operation` does not take, and returns whether
    /// the operand fits. An operand of unknown type stays silent and fits.
    fn check_expression_operand(
        &mut self,
        operation: &str,
        value: ExprId,
        operand: &ComputedColumn<'db>,
        needs: ExpressionNeeds,
    ) -> bool {
        let Some(class) = join_class(self.db, operand.ty) else {
            return true;
        };
        let fits = match needs {
            ExpressionNeeds::Numbers | ExpressionNeeds::LogicalOrNumbers => {
                class != JoinClass::Text
            }
            ExpressionNeeds::Logical => class == JoinClass::Logical,
        };
        if fits {
            return true;
        }
        let kind = TypeErrorKind::ExpressionOperand {
            operation: operation.to_owned(),
            column: column_read(self.module, value),
            needs,
            found: self.shown_operand_type(value, operand),
        };
        self.errors.push(TypeError {
            range: self.blame_range(value),
            kind,
        });
        false
    }

    /// The type a diagnostic shows for an operand: a column's vector type, or a plain R value's
    /// own type, so that `1L` reads as `integer` rather than the column it would broadcast to.
    fn shown_operand_type(&mut self, value: ExprId, operand: &ComputedColumn<'db>) -> Ty<'db> {
        if operand.reads_column {
            return operand.ty;
        }
        match self.recorded.get(&value) {
            Some(&ty) => {
                let ty = self.table.resolve(self.db, ty);
                if is_expr_type(self.db, ty) {
                    operand.ty
                } else {
                    ty
                }
            }
            None => operand.ty,
        }
    }

    /// An operand inside an expression: another expression, or a plain R value such as `2` or
    /// `"eu"`, which Polars treats as a literal that fits any aggregation.
    fn frame_operand(
        &mut self,
        value: ExprId,
        columns: &[RecordField<'db>],
        frame: Option<Symbol>,
    ) -> Option<ComputedColumn<'db>> {
        if let Some(column) = self.frame_expression(value, columns, frame) {
            return Some(column);
        }
        // Resolved first, so an alias of `Expr` reads as the expression it names.
        let ty = self.table.resolve(self.db, *self.recorded.get(&value)?);
        if is_expr_type(self.db, ty) {
            return None;
        }
        Some(ComputedColumn {
            name: "literal".to_owned(),
            ty: column_type(self.db, self.table.resolve(self.db, ty)),
            reads_column: false,
            shape: Shape::PerGroup,
        })
    }

    /// Sharpens the result of a data.table call whose arguments say which columns it has.
    /// The ordinary call has already checked and recorded every argument, so this only reads
    /// their recorded types.
    fn refine_table_call(
        &mut self,
        id: ExprId,
        function: Option<TableFunction>,
        arguments: &[Argument],
        ordinary: Ty<'db>,
    ) -> Ty<'db> {
        let refined = match function {
            None => None,
            Some(TableFunction::Construct(kind)) => Some(self.constructed_frame(kind, arguments)),
            Some(TableFunction::Fread) => Some(self.fread_type(arguments)),
            Some(TableFunction::SetNames) => self.set_names(arguments),
            Some(TableFunction::SetColumnOrder) => self.set_column_order(arguments),
            Some(TableFunction::ConvertInPlace(kind)) => {
                let subject = arguments.first().and_then(|first| first.value);
                let columns = subject.and_then(|subject| self.frame_columns_of(subject));
                Some(Ty::new(self.db, TyKind::Frame(kind, columns.as_deref())))
            }
            // `set(f, j = "z", value = v)` and `setattr(f, "names", v)` may add, rename or
            // retype any column.
            Some(TableFunction::SetInPlace) => {
                let subject = arguments.first().and_then(|first| first.value);
                match subject.and_then(|subject| self.recorded.get(&subject).copied()) {
                    Some(ty) => match *self.structural(ty).kind(self.db) {
                        TyKind::Frame(kind, Some(_)) if kind.class().is_some() => {
                            Some(Ty::new(self.db, TyKind::Frame(kind, None)))
                        }
                        _ => None,
                    },
                    None => None,
                }
            }
            Some(function @ (TableFunction::SetKey | TableFunction::SetOrder)) => {
                self.check_named_columns(function, arguments)
            }
            Some(TableFunction::SetKeyOrOrderNamed) => self.check_column_vector(arguments),
            Some(TableFunction::RbindList) => Some(self.rbind_list_type(arguments)),
            Some(TableFunction::Melt) => self.melt_type(arguments),
            Some(TableFunction::Fifelse) => self.fifelse_type(arguments),
        };
        // `setnames` and `setcolorder` change the subject by reference, which ry models as a
        // new write of its slot. Naming records the write from the callee's name alone, so a
        // subject that is not a known frame, or a call that turns out to be some other
        // `setnames`, writes the subject's own type back and loses nothing.
        if let Some(&slot) = self.naming.extras().column_updates.get(&id) {
            let written = match (refined, arguments.first().and_then(|first| first.value)) {
                (Some(ty), _) => Some(ty),
                (None, Some(subject)) => self.recorded.get(&subject).copied(),
                (None, None) => None,
            };
            if let Some(written) = written {
                self.environment.set(slot, EnvEntry::Mono(written));
                self.note_captured_write(id, slot, written);
            }
        }
        refined.unwrap_or(ordinary)
    }

    /// `data.frame(id = 1:3, price = c(1, 2, 3))` is `data.frame{id: integer[], price: double[]}`,
    /// and `data.table(...)` the `data.table{...}` with those columns. A column takes its
    /// argument's name, or a bare variable's. An argument R spreads into several columns (a
    /// list, a matrix, another frame), `...`, and a name R would rewrite (repeated or not
    /// syntactic) leave the columns unknown, as do the options that add or retype columns.
    fn constructed_frame(&mut self, kind: FrameKind, arguments: &[Argument]) -> Ty<'db> {
        let unknown_columns = Ty::new(self.db, TyKind::Frame(kind, None));
        let module = self.module;
        let options: &[&str] = match kind {
            FrameKind::DataTable => &["keep.rownames", "check.names", "key", "stringsAsFactors"],
            _ => &[
                "row.names",
                "check.rows",
                "check.names",
                "fix.empty.names",
                "stringsAsFactors",
            ],
        };
        let mut columns: Vec<RecordField<'db>> = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let Some(value) = argument.value else {
                return unknown_columns;
            };
            let argument_name = module.argument_name(argument);
            if let Some(option) = argument_name.as_deref()
                && options.contains(&option)
            {
                // `keep.rownames = TRUE` adds an `rn` column, and `stringsAsFactors = TRUE`
                // turns the character columns into factors.
                let adds_or_retypes = matches!(option, "keep.rownames" | "stringsAsFactors")
                    && !matches!(
                        module.kind(value),
                        ExpressionKind::Literal(LiteralKind::Logical(false))
                    );
                if adds_or_retypes {
                    return unknown_columns;
                }
                continue;
            }
            let name = match argument_name {
                Some(name) => name.as_str().to_owned(),
                None => match module.kind(value) {
                    ExpressionKind::NameRef(name) if name != "..." => name.as_str().to_owned(),
                    _ => return unknown_columns,
                },
            };
            let Some(&ty) = self.recorded.get(&value) else {
                return unknown_columns;
            };
            // `data.frame(a = NULL, b = 1)` has the one column `b`.
            if matches!(self.table.resolve(self.db, ty).kind(self.db), TyKind::Null) {
                continue;
            }
            if !is_syntactic_name(&name)
                || columns
                    .iter()
                    .any(|column| column.name.text(self.db) == name)
                || !self.makes_one_column(ty)
            {
                return unknown_columns;
            }
            columns.push(self.column_field(&name, ty));
        }
        Ty::new(self.db, TyKind::Frame(kind, Some(&columns)))
    }

    /// Whether a constructor makes one column of a value. A list, a matrix or another frame
    /// spreads into several, which R names after it, and a value of unknown type may be one.
    fn makes_one_column(&mut self, ty: Ty<'db>) -> bool {
        let resolved = self.table.resolve(self.db, ty);
        match resolved.kind(self.db) {
            TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_) => true,
            TyKind::Union(members) => {
                let members = members.to_vec();
                members.into_iter().all(|member| {
                    !matches!(member.kind(self.db), TyKind::Null) && self.makes_one_column(member)
                })
            }
            TyKind::Named(name, _) => {
                !matches!(
                    name.text(self.db),
                    "matrix" | "array" | "table" | "data.frame" | "data.table"
                ) && !matches!(
                    self.structural(resolved).kind(self.db),
                    TyKind::List(_) | TyKind::NamedList(_) | TyKind::Record(_) | TyKind::Tuple(_)
                )
            }
            _ => false,
        }
    }

    /// The columns of the frame an argument evaluated to, when they are known.
    fn frame_columns_of(&mut self, value: ExprId) -> Option<Vec<RecordField<'db>>> {
        let ty = *self.recorded.get(&value)?;
        match self.structural(ty).kind(self.db) {
            TyKind::Frame(
                FrameKind::DataFrame | FrameKind::DataTable | FrameKind::RDataFrame,
                Some(columns),
            ) => Some(columns.to_vec()),
            _ => None,
        }
    }

    /// The kind of the frame an argument evaluated to: a data.table function that changes its
    /// argument by reference keeps the argument's kind.
    fn frame_kind_of(&mut self, value: ExprId) -> FrameKind {
        let Some(&ty) = self.recorded.get(&value) else {
            return FrameKind::DataTable;
        };
        match self.structural(ty).kind(self.db) {
            TyKind::Frame(kind, _) => *kind,
            _ => FrameKind::DataTable,
        }
    }

    /// `fread("data/prices.csv")` types with the columns the project's file has, as the host
    /// sniffed them; `fread(text = "...")` and an input holding a line break sniff the literal
    /// itself. Arguments that change the columns ry does not model leave them unknown.
    fn fread_type(&mut self, arguments: &[Argument]) -> Ty<'db> {
        let unknown_columns = Ty::new(self.db, TyKind::Frame(FrameKind::DataTable, None));
        let module = self.module;
        let matched = match_formals(self.module, arguments, &["input", "file", "text", "cmd"]);
        let literal = |position: usize| {
            matched[position].and_then(|value| match module.kind(value) {
                ExpressionKind::Literal(LiteralKind::String(text)) => decode_escapes(&module[text]),
                _ => None,
            })
        };
        let layout = if let Some(text) = literal(2) {
            Some(sniff::sniff(text.as_bytes()))
        } else if let Some(input) = literal(0) {
            if input.contains('\n') {
                Some(sniff::sniff(input.as_bytes()))
            } else {
                self.data_file(&input)
            }
        } else if let Some(file) = literal(1) {
            self.data_file(&file)
        } else {
            None
        };
        let Some(layout) = layout else {
            return unknown_columns;
        };
        let mut factors = false;
        let mut selected: Option<Vec<String>> = None;
        let mut renamed: Option<Vec<String>> = None;
        for argument in arguments {
            let name = self.module.argument_name(argument);
            let Some(name) = name.as_deref() else {
                continue;
            };
            let value = argument.value;
            match name {
                "input" | "file" | "text" | "nThread" | "showProgress" | "verbose" | "key"
                | "index" | "data.table" | "encoding" => {}
                "stringsAsFactors" => match value.map(|value| module.kind(value)) {
                    Some(ExpressionKind::Literal(LiteralKind::Logical(flag))) => factors = *flag,
                    _ => return unknown_columns,
                },
                "select" | "col.names" => {
                    let Some(names) = value.and_then(|value| literal_strings(module, value)) else {
                        return unknown_columns;
                    };
                    let names = names.into_iter().map(|(name, _)| name).collect();
                    if name == "select" {
                        selected = Some(names);
                    } else {
                        renamed = Some(names);
                    }
                }
                _ => return unknown_columns,
            }
        }
        let db = self.db;
        let mut columns = sniffed_columns(db, &layout, factors);
        if let Some(selected) = selected {
            let mut picked = Vec::with_capacity(selected.len());
            for name in selected {
                match columns.iter().find(|(column, _)| *column == name) {
                    Some(column) => picked.push(column.clone()),
                    None => return unknown_columns,
                }
            }
            columns = picked;
        }
        if let Some(renamed) = renamed {
            if renamed.len() != columns.len() {
                return unknown_columns;
            }
            for ((column, _), name) in columns.iter_mut().zip(renamed) {
                *column = name;
            }
        }
        frame_of(db, FrameKind::DataTable, columns)
    }

    /// `DataFrame.read_csv("data/sales.csv")` and `LazyFrame.scan_csv(...)` type with the columns
    /// the project's file has, sniffed as for `fread`; `DataFrame.read_csv`'s
    /// `strings_as_factors = TRUE` reads its text columns as factors. A computed path, a file the project lacks, or a flag that is not a
    /// literal keeps the declared result, a frame with unknown columns.
    fn read_csv_type(&mut self, kind: FrameKind, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        // `LazyFrame.scan_csv` takes only a path.
        let formals: &[&str] = match kind {
            FrameKind::LazyFrame => &["path"],
            _ => &["path", "strings_as_factors"],
        };
        let matched = match_formals(module, arguments, formals);
        let path = match module.kind(matched[0]?) {
            ExpressionKind::Literal(LiteralKind::String(text)) => decode_escapes(&module[text])?,
            _ => return None,
        };
        let factors = match matched
            .get(1)
            .copied()
            .flatten()
            .map(|value| module.kind(value))
        {
            None => false,
            Some(ExpressionKind::Literal(LiteralKind::Logical(flag))) => *flag,
            Some(_) => return None,
        };
        let layout = self.data_file(&path)?;
        Some(frame_of(
            self.db,
            kind,
            sniffed_columns(self.db, &layout, factors),
        ))
    }

    fn data_file(&self, path: &str) -> Option<sniff::Layout> {
        let normalized = path.strip_prefix("./").unwrap_or(path);
        data_file_layout(self.db, DataPath::new(self.db, normalized.to_owned())).clone()
    }

    /// `setnames(f, old, new)` renames the named columns and `setnames(f, new)` renames
    /// them all.
    fn set_names(&mut self, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let matched = match_formals(self.module, arguments, &["x", "old", "new", "skip_absent"]);
        let subject = matched[0]?;
        let columns = self.frame_columns_of(subject)?;
        let kind = self.frame_kind_of(subject);
        let old_value = matched[1]?;
        let old = literal_strings(module, old_value)?;
        let frame = variable_name(module, subject);
        let mut names: Vec<(String, Ty<'db>)> = columns
            .iter()
            .map(|column| (column.name.text(db).to_owned(), column.ty))
            .collect();
        match matched[2] {
            None => {
                if old.len() != names.len() {
                    self.errors.push(TypeError {
                        range: module.range(old_value),
                        kind: TypeErrorKind::ColumnNameCount {
                            names: old.len(),
                            columns: names.len(),
                        },
                    });
                    return Some(Ty::new(db, TyKind::Frame(kind, None)));
                }
                for ((column, _), (name, _)) in names.iter_mut().zip(old) {
                    *column = name;
                }
            }
            Some(new_value) => {
                let new = literal_strings(module, new_value)?;
                if new.len() != old.len() {
                    return Some(Ty::new(db, TyKind::Frame(kind, None)));
                }
                let skip_absent = matched[3].is_some_and(|value| {
                    matches!(
                        module.kind(value),
                        ExpressionKind::Literal(LiteralKind::Logical(true))
                    )
                });
                for ((from, range), (to, _)) in old.into_iter().zip(new) {
                    match names.iter_mut().find(|(column, _)| *column == from) {
                        Some((column, _)) => *column = to,
                        None if skip_absent => {}
                        None => self.errors.push(TypeError {
                            range,
                            kind: column_does_not_exist(db, &from, frame, &columns),
                        }),
                    }
                }
            }
        }
        Some(frame_of(db, kind, names))
    }

    /// `setcolorder(f, c("b", "a"))` moves the named columns to the front, in that order.
    fn set_column_order(&mut self, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let matched = match_formals(
            self.module,
            arguments,
            &["x", "neworder", "before", "after"],
        );
        if matched[2].is_some() || matched[3].is_some() {
            return None;
        }
        let subject = matched[0]?;
        let columns = self.frame_columns_of(subject)?;
        let kind = self.frame_kind_of(subject);
        let order = literal_strings(module, matched[1]?)?;
        let frame = variable_name(module, subject);
        let mut moved: Vec<(String, Ty<'db>)> = Vec::with_capacity(columns.len());
        for (name, range) in order {
            match columns.iter().find(|column| column.name.text(db) == name) {
                Some(column) if !moved.iter().any(|(seen, _)| *seen == name) => {
                    moved.push((name, column.ty));
                }
                Some(_) => return None,
                None => self.errors.push(TypeError {
                    range,
                    kind: column_does_not_exist(db, &name, frame, &columns),
                }),
            }
        }
        for column in &columns {
            let name = column.name.text(db);
            if !moved.iter().any(|(seen, _)| seen == name) {
                moved.push((name.to_owned(), column.ty));
            }
        }
        Some(frame_of(db, kind, moved))
    }

    /// `setkey(f, id, date)` and `setorder(f, -price)` name columns as bare names, which
    /// must exist. The frame keeps its columns.
    fn check_named_columns(
        &mut self,
        function: TableFunction,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let subject = arguments
            .first()
            .filter(|first| first.name.is_none())?
            .value?;
        let columns = self.frame_columns_of(subject)?;
        let frame = variable_name(module, subject);
        for argument in &arguments[1..] {
            if argument.name.is_some() {
                continue;
            }
            let Some(mut value) = argument.value else {
                continue;
            };
            // `setorder` sorts descending by a negated column.
            if function == TableFunction::SetOrder
                && let ExpressionKind::Unary {
                    operator: UnaryOperator::Minus | UnaryOperator::Plus,
                    operand,
                } = module.kind(value)
            {
                value = *operand;
            }
            let expression = value;
            let name = match module.kind(expression) {
                ExpressionKind::NameRef(name) => name.to_string(),
                ExpressionKind::Literal(LiteralKind::String(text)) => module[text].to_string(),
                _ => continue,
            };
            if !columns.iter().any(|column| column.name.text(db) == name) {
                self.errors.push(TypeError {
                    range: module.range(expression),
                    kind: column_does_not_exist(db, &name, frame, &columns),
                });
            }
        }
        self.recorded.get(&subject).copied()
    }

    /// `setkeyv(f, c("id", "date"))` and `setorderv(f, "price")` name columns as strings.
    fn check_column_vector(&mut self, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let matched = match_formals(self.module, arguments, &["x", "cols"]);
        let subject = matched[0]?;
        let columns = self.frame_columns_of(subject)?;
        let frame = variable_name(module, subject);
        for (name, range) in literal_strings(module, matched[1]?)? {
            if !columns.iter().any(|column| column.name.text(db) == name) {
                self.errors.push(TypeError {
                    range,
                    kind: column_does_not_exist(db, &name, frame, &columns),
                });
            }
        }
        self.recorded.get(&subject).copied()
    }

    /// `rbindlist(list(a, b))` stacks frames. It binds columns by position unless
    /// `use.names = TRUE` (by name) or `fill = TRUE` (by name, filling absent columns with
    /// `NA`), and each column takes the widest type among its pieces. The result is a frame
    /// with unknown columns when the pieces are not all known frames or do not line up.
    fn rbind_list_type(&mut self, arguments: &[Argument]) -> Ty<'db> {
        let module = self.module;
        let db = self.db;
        let unknown_columns = Ty::new(db, TyKind::Frame(FrameKind::DataTable, None));
        let matched = match_formals(self.module, arguments, &["l", "use.names", "fill", "idcol"]);
        let Some(list) = matched[0] else {
            return unknown_columns;
        };
        let ExpressionKind::Call {
            callee,
            arguments: items,
        } = module.kind(list)
        else {
            return unknown_columns;
        };
        if variable_name(module, *callee).as_deref() != Some("list") || items.is_empty() {
            return unknown_columns;
        }
        let flag = |value: Option<ExprId>| match value.map(|value| module.kind(value)) {
            None => Some(false),
            Some(ExpressionKind::Literal(LiteralKind::Logical(flag))) => Some(*flag),
            _ => None,
        };
        let (Some(by_name), Some(fill)) = (flag(matched[1]), flag(matched[2])) else {
            return unknown_columns;
        };
        let mut pieces = Vec::with_capacity(items.len());
        for item in &module[items] {
            match item.value.and_then(|value| self.frame_columns_of(value)) {
                Some(columns) => pieces.push(columns),
                None => return unknown_columns,
            }
        }
        let mut stacked: Vec<(String, Vec<Ty<'db>>)> = Vec::new();
        for (index, piece) in pieces.iter().enumerate() {
            if !by_name && !fill {
                if index == 0 {
                    stacked = piece
                        .iter()
                        .map(|column| (column.name.text(db).to_owned(), vec![column.ty]))
                        .collect();
                } else if piece.len() != stacked.len() {
                    return unknown_columns;
                } else {
                    for (slot, column) in stacked.iter_mut().zip(piece) {
                        slot.1.push(column.ty);
                    }
                }
                continue;
            }
            for column in piece {
                let name = column.name.text(db);
                match stacked.iter_mut().find(|(seen, _)| seen == name) {
                    Some((_, types)) => types.push(column.ty),
                    None if index == 0 || fill => stacked.push((name.to_owned(), vec![column.ty])),
                    None => return unknown_columns,
                }
            }
            if !fill && piece.len() != stacked.len() {
                return unknown_columns;
            }
        }
        let mut columns = Vec::with_capacity(stacked.len() + 1);
        match matched[3].map(|value| module.kind(value)) {
            None | Some(ExpressionKind::Literal(LiteralKind::Logical(false))) => {}
            Some(ExpressionKind::Literal(LiteralKind::Logical(true))) => {
                columns.push((".id".to_owned(), self.list_id_type(&module[items])));
            }
            Some(ExpressionKind::Literal(LiteralKind::String(name))) => {
                columns.push((module[name].to_string(), self.list_id_type(&module[items])));
            }
            Some(_) => return unknown_columns,
        }
        for (name, types) in stacked {
            match widest_column(db, &types, true) {
                Some(ty) => columns.push((name, ty)),
                None => return unknown_columns,
            }
        }
        frame_of(db, FrameKind::DataTable, columns)
    }

    /// `fifelse(test, yes, no, na)` is a vector as long as `test` whose type `yes`, `no` and
    /// `na` share. data.table widens integers to doubles and lets a bare `NA` stand for any
    /// type, but refuses any other mix.
    fn fifelse_type(&mut self, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let matched = match_formals(self.module, arguments, &["test", "yes", "no", "na"]);
        let mut shared: Option<(Atomic, &'static str, Ty<'db>)> = None;
        for (position, formal) in [(1, "yes"), (2, "no"), (3, "na")] {
            let Some(value) = matched[position] else {
                continue;
            };
            if matches!(
                module.kind(value),
                ExpressionKind::Literal(LiteralKind::Na(NaAtom::Logical))
            ) {
                continue;
            }
            let ty = *self.recorded.get(&value)?;
            let ty = self.structural(ty);
            let TyKind::Scalar(atomic) = column_element(db, ty).kind(db) else {
                return None;
            };
            let atomic = *atomic;
            shared = match shared {
                None => Some((atomic, formal, ty)),
                Some((seen, _, _)) if seen == atomic => shared,
                Some((Atomic::Integer, first, first_ty)) if atomic == Atomic::Double => {
                    Some((Atomic::Double, first, first_ty))
                }
                Some((Atomic::Double, _, _)) if atomic == Atomic::Integer => shared,
                Some((_, first, first_ty)) => {
                    self.errors.push(TypeError {
                        range: module.range(value),
                        kind: TypeErrorKind::BranchTypesDiffer {
                            function: "fifelse",
                            first: first.to_owned(),
                            first_ty,
                            second: formal.to_owned(),
                            second_ty: ty,
                        },
                    });
                    return Some(unknown(db));
                }
            };
        }
        let (atomic, _, _) = shared?;
        Some(Ty::new(db, TyKind::Vector(scalar(db, atomic))))
    }

    /// `idcol` holds each piece's name when the list names them all, and its position
    /// otherwise.
    fn list_id_type(&self, items: &[Argument]) -> Ty<'db> {
        let atomic = if items.iter().all(|item| item.name.is_some()) {
            Atomic::Character
        } else {
            Atomic::Integer
        };
        Ty::new(self.db, TyKind::Vector(scalar(self.db, atomic)))
    }

    /// `melt(f, id.vars = "id", measure.vars = c("a", "b"))` stacks the measure columns into
    /// a `variable` factor of their names and a `value` column of their widest type. Either
    /// list may be left out, and then it is every other column.
    fn melt_type(&mut self, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let matched = match_formals(
            self.module,
            arguments,
            &[
                "data",
                "id.vars",
                "measure.vars",
                "variable.name",
                "value.name",
            ],
        );
        let subject = matched[0]?;
        let columns = self.frame_columns_of(subject)?;
        let frame = variable_name(module, subject);
        let listed = |checker: &mut Self, value: Option<ExprId>| -> Option<Option<Vec<usize>>> {
            let Some(value) = value else {
                return Some(None);
            };
            let mut positions = Vec::new();
            for (name, range) in literal_strings(module, value)? {
                match columns
                    .iter()
                    .position(|column| column.name.text(db) == name)
                {
                    Some(position) => positions.push(position),
                    None => checker.errors.push(TypeError {
                        range,
                        kind: column_does_not_exist(db, &name, frame, &columns),
                    }),
                }
            }
            Some(Some(positions))
        };
        let ids = listed(self, matched[1])?;
        let measures = listed(self, matched[2])?;
        let others = |listed: &[usize]| -> Vec<usize> {
            (0..columns.len())
                .filter(|position| !listed.contains(position))
                .collect()
        };
        let (ids, measures) = match (ids, measures) {
            (Some(ids), Some(measures)) => (ids, measures),
            (Some(ids), None) => {
                let measures = others(&ids);
                (ids, measures)
            }
            (None, Some(measures)) => (others(&measures), measures),
            (None, None) => return None,
        };
        let name_of = |value: Option<ExprId>, default: &str| -> Option<String> {
            match value.map(|value| module.kind(value)) {
                None => Some(default.to_owned()),
                Some(ExpressionKind::Literal(LiteralKind::String(text))) => {
                    Some(module[text].to_string())
                }
                Some(_) => None,
            }
        };
        let variable = name_of(matched[3], "variable")?;
        let value = name_of(matched[4], "value")?;
        let factor_variable = !arguments.iter().any(|argument| {
            self.module.argument_name(argument).as_deref() == Some("variable.factor")
                && argument.value.is_some_and(|value| {
                    matches!(
                        module.kind(value),
                        ExpressionKind::Literal(LiteralKind::Logical(false))
                    )
                })
        });
        let measure_types: Vec<Ty<'db>> = measures
            .iter()
            .filter_map(|&position| columns.get(position))
            .map(|column| column.ty)
            .collect();
        let mut result: Vec<(String, Ty<'db>)> = ids
            .iter()
            .filter_map(|&position| columns.get(position))
            .map(|column| (column.name.text(db).to_owned(), column.ty))
            .collect();
        let variable_ty = if factor_variable {
            factor_type(db)
        } else {
            Ty::new(db, TyKind::Vector(scalar(db, Atomic::Character)))
        };
        result.push((variable, variable_ty));
        result.push((value, widest_column(db, &measure_types, false)?));
        Some(frame_of(db, FrameKind::DataTable, result))
    }

    /// `None` unless every non-`NULL` argument is one nominal whose class declares `c.Class`.
    fn combine_nominal(&mut self, inferred: &[Ty<'db>]) -> Option<Ty<'db>> {
        let mut nominal: Option<Ty<'db>> = None;
        for &ty in inferred {
            if matches!(ty.kind(self.db), TyKind::Null) {
                continue;
            }
            if !matches!(ty.kind(self.db), TyKind::Named(..)) {
                return None;
            }
            match nominal {
                Some(seen) if seen != ty => return None,
                Some(_) => {}
                None => nominal = Some(ty),
            }
        }
        let nominal = nominal?;
        let TyKind::Named(name, _) = nominal.kind(self.db) else {
            return None;
        };
        let method = method_symbol("c", name.text(self.db));
        self.globals?
            .scheme(method, false)
            .map(|_| nominal)
            .or_else(|| {
                self.globals?
                    .overloads(method, false)
                    .filter(|candidates| !candidates.is_empty())
                    .map(|_| nominal)
            })
    }

    fn infer_combine(&mut self, id: ExprId, arguments: &[Argument]) -> Ty<'db> {
        if arguments.is_empty() {
            return null(self.db);
        }
        // `c()` over any list-shaped argument builds a list (`c(list_a, list_b)` appends),
        // which the atomic coercion cannot describe.
        let supplied: Vec<(&Argument, ExprId, Ty<'db>)> = arguments
            .iter()
            .filter_map(|argument| {
                let value = argument.value?;
                let ty = self.infer(value);
                // `c` is an R function, so it decides a variable written with a literal as
                // R's type, as any other read does.
                if self.open_literal_var(ty) {
                    self.default_literal_var(ty, false);
                }
                Some((argument, value, self.structural(ty)))
            })
            .collect();
        let inferred: Vec<Ty<'db>> = supplied.iter().map(|&(_, _, ty)| ty).collect();
        if inferred
            .iter()
            .any(|&ty| self.push_combined_list_elements(ty, &mut Vec::new()))
        {
            // `c()` drops `NULL`, so `c(if (a) list(1), if (a) list(2))` is a list, and an
            // empty list adds no element, so `c(list(), f)` is `list[fn]`.
            let mut elements: Vec<Ty<'db>> = Vec::new();
            for &ty in &inferred {
                if !matches!(ty.kind(self.db), TyKind::Null)
                    && !self.push_combined_list_elements(ty, &mut elements)
                {
                    elements.push(ty);
                }
            }
            if elements.is_empty() {
                return Ty::new(self.db, TyKind::Tuple(&[]));
            }
            return Ty::new(self.db, TyKind::List(union_of(self.db, elements)));
        }
        // A class declaring `c.Class` keeps its class through concatenation (`c(d1, d2)`
        // on two `Date`s is a `Date`).
        if let Some(combined) = self.combine_nominal(&inferred) {
            return combined;
        }
        let mut item_atomic: Option<Atomic> = None;
        let mut all_arguments_are_named = true;
        let mut saw_non_null_argument = false;
        let mut result_indeterminate = false;
        for &(argument, _, resolved) in &supplied {
            if matches!(resolved.kind(self.db), TyKind::Null) {
                continue;
            }
            saw_non_null_argument = true;
            all_arguments_are_named &= argument.name.is_some();
            // An argument whose atomic is not statically known cannot pin the element type:
            // the result is `Unknown`, and a variable stays unconstrained.
            match resolved.kind(self.db) {
                TyKind::Any | TyKind::Unknown => {
                    result_indeterminate = true;
                    continue;
                }
                TyKind::Var(_) | TyKind::Rigid(_) => {
                    self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                    result_indeterminate = true;
                    continue;
                }
                _ => {}
            }
            // An open member (`T | NULL` from a `NULL` guard, `T[]` from `sort(x)`) is as
            // indeterminate as a bare variable.
            let open = |ty: Ty<'db>| {
                matches!(
                    ty.kind(self.db),
                    TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown
                )
            };
            let open_vector = |ty: Ty<'db>| match ty.kind(self.db) {
                TyKind::Vector(element) | TyKind::NamedVector(element) => open(*element),
                _ => open(ty),
            };
            let single = [resolved];
            let members: &[Ty<'db>] = match resolved.kind(self.db) {
                TyKind::Union(members) => members,
                _ => &single,
            };
            if members.iter().any(|&member| open_vector(member)) {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                result_indeterminate = true;
                continue;
            }
            // `NULL` union members contribute nothing (an accumulator seeded with `c()` is
            // `T[] | NULL` at the loop join).
            let argument_atomics: Option<Vec<Atomic>> = match resolved.kind(self.db) {
                TyKind::Union(members) => members
                    .iter()
                    .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
                    .map(|&member| combine_operand_atomic(self.db, member))
                    .collect(),
                _ => combine_operand_atomic(self.db, resolved).map(|atomic| vec![atomic]),
            };
            // R's `c()` accepts every value: an argument whose atomic is not statically known
            // (a variable, a nominal it strips, a function it wraps into a list) leaves the
            // result `Unknown` and pins nothing.
            let Some(argument_atomics) = argument_atomics.filter(|atomics| !atomics.is_empty())
            else {
                if !matches!(resolved.kind(self.db), TyKind::Any | TyKind::Unknown) {
                    self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                }
                result_indeterminate = true;
                continue;
            };
            for current_atomic in argument_atomics {
                item_atomic = Some(match item_atomic {
                    Some(previous_atomic) => {
                        promote_combine_atomic(previous_atomic, current_atomic)
                    }
                    None => current_atomic,
                });
            }
        }
        if !saw_non_null_argument {
            return null(self.db);
        }
        if result_indeterminate {
            return unknown(self.db);
        }
        let element = scalar(self.db, item_atomic.unwrap_or(Atomic::Integer));
        Ty::new(
            self.db,
            if all_arguments_are_named {
                TyKind::NamedVector(element)
            } else {
                TyKind::Vector(element)
            },
        )
    }

    /// Pushes the elements a list-shaped `c()` argument contributes, and says whether it is
    /// list-shaped. `NULL` and an empty list contribute nothing, so `c(if (a) list())` adds no
    /// element.
    fn push_combined_list_elements(&self, ty: Ty<'db>, elements: &mut Vec<Ty<'db>>) -> bool {
        let single = [ty];
        let members: &[Ty<'db>] = match ty.kind(self.db) {
            TyKind::Union(members) => members,
            TyKind::Null => return false,
            _ => &single,
        };
        let start = elements.len();
        let mut list_shaped = false;
        for &member in members {
            if matches!(member.kind(self.db), TyKind::Null) {
                continue;
            }
            if is_empty_list(self.db, member) {
                list_shaped = true;
                continue;
            }
            match self.list_element_type(member) {
                Some(element) => {
                    list_shaped = true;
                    elements.push(element);
                }
                None => {
                    elements.truncate(start);
                    return false;
                }
            }
        }
        list_shaped
    }

    fn list_element_type(&self, ty: Ty<'db>) -> Option<Ty<'db>> {
        match ty.kind(self.db) {
            TyKind::List(element) | TyKind::NamedList(element) => Some(*element),
            TyKind::Tuple(items) => Some(union_of(self.db, items.iter().copied())),
            TyKind::Record(fields) => Some(union_of(self.db, fields.iter().map(|field| field.ty))),
            _ => None,
        }
    }

    /// Attributes are not part of a type, so the call has its first argument's type.
    /// `dim` is the exception: array shape is untracked, so it falls through to the
    /// declaration. `None` means not intercepted.
    fn infer_structure(&mut self, id: ExprId, arguments: &[Argument]) -> Option<Ty<'db>> {
        let shape_changing = arguments.iter().any(|argument| {
            self.module
                .argument_name(argument)
                .is_some_and(|name| matches!(name.as_str(), "dim" | ".Dim"))
        });
        if shape_changing {
            return None;
        }
        // `.Data` is the formal's name, so it may be given either way.
        let value = arguments
            .iter()
            .find(|argument| self.module.argument_name(argument).as_deref() == Some(".Data"))
            .or_else(|| arguments.iter().find(|argument| argument.name.is_none()))?
            .value?;
        let ty = self.infer(value);
        for argument in arguments {
            if let Some(other) = argument.value
                && other != value
            {
                self.infer(other);
            }
        }
        Some(self.record(id, ty))
    }

    fn infer_list(&mut self, arguments: &[Argument]) -> Ty<'db> {
        if arguments.is_empty() {
            return Ty::new(self.db, TyKind::Tuple(&[]));
        }
        // `list(...)` splices arguments whose number and names are unknown, so the list is
        // open: any field may be present, and reading one yields `Unknown`.
        let splices_dots = arguments.iter().any(|argument| {
            argument.value.is_some_and(|value| {
                matches!(self.module.kind(value), ExpressionKind::NameRef(name) if name == "...")
            })
        });
        if splices_dots {
            for argument in arguments {
                if let Some(value) = argument.value {
                    self.infer(value);
                }
            }
            return Ty::new(self.db, TyKind::NamedList(unknown(self.db)));
        }
        let all_named = arguments.iter().all(|argument| argument.name.is_some());
        let all_unnamed = arguments.iter().all(|argument| argument.name.is_none());
        if !(all_named || all_unnamed) {
            // A partially named list (`list(x, n = 1)`) fits neither tuple nor record, so the
            // names drop and the values join into an array-like list.
            let mut items = Vec::with_capacity(arguments.len());
            for argument in arguments {
                if let Some(value) = argument.value {
                    let inferred = self.infer(value);
                    items.push(self.table.resolve(self.db, inferred));
                }
            }
            return Ty::new(self.db, TyKind::List(union_of(self.db, items)));
        }
        let mut fields = Vec::new();
        let mut items = Vec::new();
        for argument in arguments {
            let ty = match argument.value {
                Some(value) => {
                    let inferred = self.infer(value);
                    self.table.resolve(self.db, inferred)
                }
                None => unknown(self.db),
            };
            match self.module.argument_name(argument) {
                Some(name) => fields.push(RecordField {
                    name: Name::new(self.db, name),
                    ty,
                    optional: false,
                }),
                None => items.push(ty),
            }
        }
        if all_named {
            Ty::new(self.db, TyKind::Record(&fields))
        } else {
            Ty::new(self.db, TyKind::Tuple(&items))
        }
    }

    /// Every branch is checked and the type is the union of their values; R returns
    /// invisible `NULL` when nothing matches, so `NULL` joins unless a default exists.
    fn infer_switch(&mut self, range: TextRange, arguments: &[Argument]) -> Ty<'db> {
        let Some((subject, branches)) = arguments.split_first() else {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::ArityMismatch {
                    expected: 1,
                    found: 0,
                },
            });
            return unknown(self.db);
        };
        if let Some(value) = subject.value {
            self.infer(value);
        }
        let type_tested = subject
            .value
            .and_then(|value| self.typeof_subject_slot(value));
        let mut members = Vec::with_capacity(branches.len() + 1);
        let mut has_default = false;
        // Exactly one alternative runs, so branches fork and join like `if` arms; inferring
        // them in sequence would let a later branch's write win outright.
        let mark = self.environment.mark();
        let mut branch_writes = Vec::with_capacity(branches.len());
        // An empty alternative falls through to the next one, so its label selects that arm too.
        let mut labels: Vec<Symbol> = Vec::new();
        for branch in branches {
            match self.module.argument_name(branch) {
                Some(name) => labels.push(name),
                None => has_default = true,
            }
            let Some(value) = branch.value else {
                continue;
            };
            if let Some(slot) = type_tested
                && branch.name.is_some()
            {
                self.narrow_to_typeof_labels(slot, &labels);
            }
            labels.clear();
            // The arm's narrowing is a refinement of the test, not a write the arm makes.
            let arm_mark = self.environment.mark();
            let ty = self.infer(value);
            members.push(self.table.resolve(self.db, ty));
            if !self.diverges(value) {
                branch_writes.push(self.environment.writes_since(arm_mark));
            }
            self.environment.rollback(mark);
        }
        if let Some((adopted, joined)) = branch_writes.split_last() {
            for &(slot, entry) in adopted {
                if let Some(entry) = entry {
                    self.environment.set(slot, entry);
                }
            }
            for writes in joined {
                self.join_writes(writes, JoinEdge::Branch);
            }
        }
        if !has_default {
            members.push(null(self.db));
        }
        union_of(self.db, members)
    }

    /// Only a variable from outside the call is the caller's: one minted by a lambda
    /// among the arguments is that lambda's to infer (`lapply(xs, function(v) v + 1L)`).
    fn caller_open_variables(
        &self,
        arguments: &[CallArgument<'db>],
    ) -> Vec<(InferenceVar, Entry<'db>)> {
        let mut variables = FxHashSet::default();
        for argument in arguments {
            if let Some(ty) = argument.ty {
                self.table.collect_unbound_vars(self.db, ty, &mut variables);
            }
        }
        variables
            .into_iter()
            .map(|var| (var, self.table.entry(var).clone()))
            .filter(|(_, entry)| {
                matches!(entry, Entry::Unbound { level, .. } if *level <= self.table.level)
            })
            .collect()
    }

    /// Overload results that differ only in list shape (`lapply` over an unchecked vector)
    /// stay guessable; any other difference (`integer` against `double`, `factor` against
    /// `T[]`) would mislead.
    fn is_list_result(&self, ty: Ty<'db>) -> bool {
        matches!(ty.kind(self.db), TyKind::List(_) | TyKind::NamedList(_))
    }

    /// The caller variable an overload set hands back unchanged. `Filter`, `head` and
    /// `unique` declare one candidate per shape, each returning the shape it accepted, so
    /// on an open argument the candidates narrow it differently and no single guess is
    /// right. When every specific fit returns exactly what it narrowed the same variable
    /// to, the call returns that variable itself and leaves it open: the caller gets back
    /// whatever it passes, a `data.frame` included. An all-`Any` catch-all is ignored.
    fn preserved_caller_variable(
        &self,
        caller_entries: &[(InferenceVar, Entry<'db>)],
        fits: &[(usize, bool, Vec<Ty<'db>>)],
        fit_results: &[Ty<'db>],
    ) -> Option<InferenceVar> {
        if fit_results.len() != fits.len() {
            return None;
        }
        let specific: Vec<(&Vec<Ty<'db>>, Ty<'db>)> = fits
            .iter()
            .zip(fit_results)
            .filter(|(_, result)| !self.all_any(&[**result]))
            .map(|((_, _, narrowing), result)| (narrowing, *result))
            .collect();
        let (first, _) = specific.first()?;
        if specific.len() < 2 || specific.iter().all(|(narrowing, _)| narrowing == first) {
            return None;
        }
        caller_entries
            .iter()
            .enumerate()
            .find(|(position, (var, _))| {
                specific.iter().all(|(narrowing, result)| {
                    narrowing.get(*position).is_some_and(|narrowed| {
                        narrowed == result
                            && !matches!(narrowed.kind(self.db), TyKind::Var(other) if other == var)
                    })
                })
            })
            .map(|(_, (var, _))| *var)
    }

    /// `function(x) abs(x)` fits `abs`'s `Any` fallback without narrowing `x`, so the free fit
    /// would win and `x` stay unconstrained. When the fallback is the only free fit and every
    /// specific candidate narrows a caller variable to a number atom, the call needs a number
    /// there whatever candidate a caller picks, so the variable takes the `numeric` bound. A
    /// class with an arithmetic method still satisfies it. `max`, whose candidates also take
    /// `character`, imposes nothing, and neither does a call where the variable is one member
    /// of a union argument: in `unique(x)` with `x: T | double[]`, the `double[]` member is
    /// what rules out the `factor` candidate, so a `factor` for `T` fits the fallback.
    fn bound_numeric_caller_variables(
        &mut self,
        caller_entries: &[(InferenceVar, Entry<'db>)],
        fits: &[(usize, bool, Vec<Ty<'db>>)],
        fit_results: &[Ty<'db>],
        call_arguments: &[CallArgument<'db>],
    ) {
        if fit_results.len() != fits.len()
            || !fits.iter().any(|(_, free, _)| *free)
            || fits
                .iter()
                .zip(fit_results)
                .any(|((_, free, _), result)| *free && !self.all_any(&[*result]))
        {
            return;
        }
        let specific: Vec<&Vec<Ty<'db>>> = fits
            .iter()
            .filter(|(_, free, _)| !free)
            .map(|(_, _, narrowing)| narrowing)
            .collect();
        if specific.is_empty() {
            return;
        }
        for (position, (var, _)) in caller_entries.iter().enumerate() {
            let numeric_everywhere = specific.iter().all(|narrowing| {
                // A number atom, not a class that merely has arithmetic: `sweep`'s `matrix`
                // candidate takes a character matrix too.
                narrowing.get(position).is_some_and(|&narrowed| {
                    let members = match narrowed.kind(self.db) {
                        TyKind::Union(members) => &members[..],
                        _ => std::slice::from_ref(&narrowed),
                    };
                    members
                        .iter()
                        .all(|&member| numeric_operand_parts(self.db, member).is_some())
                })
            });
            if !numeric_everywhere || self.shares_a_union(*var, call_arguments) {
                continue;
            }
            let snapshot = self.table.snapshot();
            if self
                .table
                .constrain(self.db, *var, Constraint::Arithmetic)
                .is_err()
            {
                // A variable an earlier use made non-numeric keeps that finding to itself.
                self.table.rollback(snapshot);
            }
        }
    }

    /// Whether `var` is a member of a union argument beside a member that is no variable, so
    /// that member, not `var`, may be what picked the candidates.
    fn shares_a_union(&self, var: InferenceVar, call_arguments: &[CallArgument<'db>]) -> bool {
        call_arguments
            .iter()
            .filter_map(|argument| argument.ty)
            .any(|ty| {
                let TyKind::Union(members) = self.table.shallow_resolve(self.db, ty).kind(self.db)
                else {
                    return false;
                };
                let mut holds_var = false;
                let mut holds_other = false;
                for &member in members.iter() {
                    match self.table.shallow_resolve(self.db, member).kind(self.db) {
                        TyKind::Var(other) if self.table.find(*other) == self.table.find(var) => {
                            holds_var = true;
                        }
                        TyKind::Var(_) | TyKind::Null => {}
                        _ => holds_other = true,
                    }
                }
                holds_var && holds_other
            })
    }

    /// A declared `<T>` passed to a shape-preserving overload set (see
    /// `preserved_caller_variable`) fits none of its candidates, because a rigid parameter
    /// refuses to narrow. Written back as an annotation, `<T> fn(values: T) -> T` for a
    /// `Filter` wrapper must check the way its inferred form did, so the argument is probed
    /// as an open variable, and the call returns `T` when the candidates preserve it.
    fn preserved_rigid_argument(
        &mut self,
        callee: ExprId,
        schemes: &[TypeScheme<'db>],
        call_arguments: &[CallArgument<'db>],
    ) -> Option<Ty<'db>> {
        let position = call_arguments.iter().position(|argument| {
            argument.ty.is_some_and(|ty| {
                matches!(
                    self.table.shallow_resolve(self.db, ty).kind(self.db),
                    TyKind::Rigid(_)
                )
            })
        })?;
        let rigid = call_arguments.get(position)?.ty?;
        let snapshot = self.table.snapshot();
        let open = self.fresh(Constraint::Unconstrained);
        let TyKind::Var(var) = *open.kind(self.db) else {
            self.table.rollback(snapshot);
            return None;
        };
        let mut probed = call_arguments.to_vec();
        if let Some(argument) = probed.get_mut(position) {
            argument.ty = Some(open);
        }
        let caller_entries = vec![(var, self.table.entry(var).clone())];
        let mut fits = Vec::new();
        let mut fit_results = Vec::new();
        for (index, scheme) in schemes.iter().enumerate() {
            let probe = self.table.snapshot();
            if let Ok((_, function)) = self.probe_overload_candidate(callee, scheme, &probed, false)
            {
                fit_results.push(self.table.resolve(self.db, function.ret));
                let narrowed = self.table.resolve(self.db, open);
                fits.push((
                    index,
                    self.caller_variables_untouched(&caller_entries),
                    vec![narrowed],
                ));
            }
            self.table.rollback(probe);
        }
        let preserved = self.preserved_caller_variable(&caller_entries, &fits, &fit_results);
        self.table.rollback(snapshot);
        preserved.map(|_| rigid)
    }

    fn caller_variables_untouched(&self, entries: &[(InferenceVar, Entry<'db>)]) -> bool {
        entries
            .iter()
            .all(|(var, entry)| self.table.find(*var) == *var && self.table.entry(*var) == entry)
    }

    /// Declaration order is only the tiebreak: a candidate that fits without narrowing
    /// the caller's open variables outranks an earlier one that pins one down. The
    /// literal-as-integer courtesy runs only in a second round so a literal never steers selection.
    /// A `NULL` callee is a placeholder filled in later (`.onLoad`, `<<-`, or a field of an
    /// object ry does not model), so a call on it, or on `T | NULL`, is unchecked.
    fn is_null_placeholder(&self, resolved: Ty<'db>) -> bool {
        match resolved.kind(self.db) {
            TyKind::Null => true,
            TyKind::Union(members) => members
                .iter()
                .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
                .all(|member| matches!(member.kind(self.db), TyKind::Var(_))),
            _ => false,
        }
    }

    fn try_overloaded_call(
        &mut self,
        id: ExprId,
        range: TextRange,
        callee: ExprId,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let name = match self.module.kind(callee) {
            ExpressionKind::NameRef(name) => {
                if self.naming.resolutions.contains_key(&callee) {
                    return None;
                }
                *name
            }
            // Namespace access cannot be shadowed by a local binding.
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => {
                let name =
                    validated_namespace_name(self.db, self.library, *internal, package, name)?;
                if !namespace_reaches_project(self.db, package) {
                    let schemes = self.library?.namespace_schemes((*package)?, name)?;
                    if schemes.len() < 2 {
                        return None;
                    }
                    let callees = self.hinting_callees(schemes, arguments);
                    let call_arguments =
                        self.infer_call_arguments_against(range, arguments, &callees);
                    return Some(self.overloaded_call(
                        id,
                        range,
                        callee,
                        name,
                        schemes,
                        &call_arguments,
                    ));
                }
                name
            }
            _ => return None,
        };
        let schemes = self
            .globals?
            .overloads(name, self.naming.deferred_non_locals.contains(&callee))?;
        if schemes.len() < 2 {
            return None;
        }

        // Arguments infer once, before any probe: the probe snapshot does not reverse
        // environment or recorded-type writes.
        let callees = self.hinting_callees(schemes, arguments);
        let mut call_arguments = self.infer_call_arguments_against(range, arguments, &callees);
        self.resolve_match_fun_arguments(callee, &mut call_arguments);
        if let Some(ty) = self.s3_method_call(range, &name, &call_arguments) {
            return Some(ty);
        }
        let result = self.overloaded_call(id, range, callee, name, schemes, &call_arguments);
        if self.project_method_may_run(callee, &call_arguments) {
            return Some(unknown(self.db));
        }
        Some(result)
    }

    /// A stub describes a base generic's default method, but a package that registers its own
    /// method (R.utils' `S3method(str, Verbose)`) runs it for a first argument of that class,
    /// and the stub does not describe what it returns. An atomic first argument, `NULL` or a
    /// function carries no class the package could have given it.
    fn project_method_may_run(&self, callee: ExprId, arguments: &[CallArgument<'db>]) -> bool {
        let db = self.db;
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return false;
        };
        let Some(globals) = self.globals else {
            return false;
        };
        if self.naming.resolutions.contains_key(&callee)
            || globals.defined_in_project(*name, self.naming.deferred_non_locals.contains(&callee))
            || registered_methods(db, QueryName::new(db, Name::new(db, name.as_str()))).is_none()
        {
            return false;
        }
        let Some(first) = arguments.first() else {
            return false;
        };
        // A parameter's type comes from its uses, which never see a class attribute, so an
        // R.oo-style object that is a classed logical `NA` passes as `logical`.
        if first.value.is_some_and(|value| self.binds_parameter(value)) {
            return true;
        }
        let Some(first) = first.ty else {
            return false;
        };
        !matches!(
            self.table.shallow_resolve(db, first).kind(db),
            TyKind::Scalar(_)
                | TyKind::Vector(_)
                | TyKind::NamedVector(_)
                | TyKind::Null
                | TyKind::Function(_)
        )
    }

    /// R dispatches a call to an overloaded generic on its first argument's class before the
    /// default method runs, so a nominal that declares `sum.money` or `Summary.money` gets that
    /// method's contract instead of the stub candidates, which only describe the default.
    /// `None` when the first argument is not a nominal that declares one.
    fn s3_method_call(
        &mut self,
        range: TextRange,
        name: &str,
        call_arguments: &[CallArgument<'db>],
    ) -> Option<Ty<'db>> {
        let globals = self.globals?;
        let first = call_arguments.first()?.ty?;
        let TyKind::Named(class, _) = self.table.shallow_resolve(self.db, first).kind(self.db)
        else {
            return None;
        };
        let class = class.text(self.db);
        let group = SUMMARY_GROUP.contains(&name).then_some("Summary");
        let candidates = [Some(name), group]
            .into_iter()
            .flatten()
            .find_map(|generic| {
                let method = method_symbol(generic, class);
                globals
                    .overloads(method, false)
                    .map(Cow::Borrowed)
                    .or_else(|| {
                        globals
                            .scheme(method, false)
                            .map(|scheme| Cow::Owned(vec![scheme]))
                    })
                    .filter(|candidates| !candidates.is_empty())
            })?;
        for scheme in candidates.iter() {
            let snapshot = self.table.snapshot();
            let instantiated = self.instantiate(scheme);
            let resolved = self.table.shallow_resolve(self.db, instantiated);
            let TyKind::Function(function) = resolved.kind(self.db) else {
                self.table.rollback(snapshot);
                continue;
            };
            if self
                .match_arguments(range, function, call_arguments)
                .is_empty()
            {
                return Some(function.ret);
            }
            self.table.rollback(snapshot);
        }
        // A method whose declared formals the call does not fit still runs in R, where `...`
        // and the generic's own arguments are loosely forwarded, so this stays unchecked.
        Some(unknown(self.db))
    }

    /// The overload of `schemes` that `call_arguments` select, committed, or the finding
    /// that none fits.
    fn overloaded_call(
        &mut self,
        id: ExprId,
        range: TextRange,
        callee: ExprId,
        name: Symbol,
        schemes: &[TypeScheme<'db>],
        call_arguments: &[CallArgument<'db>],
    ) -> Ty<'db> {
        // A fit leaving the caller's open variables untouched beats one that narrows them:
        // binding `function(x) sum(x)`'s `x` to the first-probed candidate would reject
        // calls R accepts. Ties go to declaration order.
        let caller_entries = self.caller_open_variables(call_arguments);

        // An `Unknown` argument fits every candidate, so declaration order alone would
        // pick one return type (`sum(x[[i]])` as `integer`). The call is only typed when
        // all fitting candidates agree on the result.
        let unchecked_argument = caller_entries.is_empty()
            && call_arguments
                .iter()
                .any(|argument| argument.ty.is_some_and(|ty| self.is_unchecked(ty)));

        // `1` is a double at runtime, so the integer-literal courtesy round runs only when
        // the strict one matched nothing (`sum(1, 2)` must stay a double).
        let rounds: &[bool] = if call_arguments.iter().any(|argument| argument.whole_double) {
            &[false, true]
        } else {
            &[false]
        };

        // Strict round only: a candidate the lenient round also rejects was already
        // rejected on the honest reading.
        let mut candidate_errors: Vec<TypeError<'db>> = Vec::new();
        // (candidate, left caller variables untouched, what it narrowed them to)
        let mut fits: Vec<(usize, bool, Vec<Ty<'db>>)> = Vec::new();
        // Each fit's result, parallel to `fits` when the call narrows caller variables.
        let mut fit_results: Vec<Ty<'db>> = Vec::new();
        let mut fitting_courtesy = false;
        for &courtesy in rounds {
            for (index, scheme) in schemes.iter().enumerate() {
                let snapshot = self.table.snapshot();
                match self.probe_overload_candidate(callee, scheme, call_arguments, courtesy) {
                    Ok((resolved, function)) => {
                        if unchecked_argument {
                            let result = self.table.resolve(self.db, function.ret);
                            self.table.rollback(snapshot);
                            fits.push((index, true, vec![result]));
                            fitting_courtesy = courtesy;
                            continue;
                        }
                        if caller_entries.is_empty() {
                            self.recorded.insert(callee, resolved);
                            self.selected_overloads.insert(callee, index);
                            return self.committed_overload_return(id, function.ret);
                        }
                        let free = self.caller_variables_untouched(&caller_entries);
                        fit_results.push(self.table.resolve(self.db, function.ret));
                        // A union parameter bounds the variable rather than binding it, and
                        // that bound is what the candidate narrowed it to.
                        let narrowing = caller_entries
                            .iter()
                            .map(|(var, _)| match self.table.entry(*var) {
                                Entry::Unbound {
                                    bound: Some(bound), ..
                                } => self.table.resolve(self.db, *bound),
                                _ => self
                                    .table
                                    .resolve(self.db, Ty::new(self.db, TyKind::Var(*var))),
                            })
                            .collect();
                        self.table.rollback(snapshot);
                        fits.push((index, free, narrowing));
                        fitting_courtesy = courtesy;
                    }
                    Err(outcome) => {
                        self.table.rollback(snapshot);
                        if let Some(error) = outcome.into_iter().next()
                            && !courtesy
                        {
                            candidate_errors.push(error);
                        }
                    }
                }
            }
            if !fits.is_empty() {
                break;
            }
        }
        if fits.is_empty()
            && let Some(preserved) = self.preserved_rigid_argument(callee, schemes, call_arguments)
        {
            return preserved;
        }

        // An overload set ends in a catch-all returning `Any` for calls it cannot describe, so
        // it fits every unchecked call too. A more specific fit refines it rather than
        // disagreeing with it, but only when the unchecked arguments cannot be what picks the
        // candidate: `rep(0, n)` with `n` from `nrow()` is `double[]`, because `n` lands on
        // `times: Any`, while `rep(unknown, 2)` stays `Unknown`.
        if unchecked_argument
            && fits.iter().any(|(_, _, results)| !self.all_any(results))
            && self.unchecked_arguments_decide_nothing(callee, schemes, &fits, call_arguments)
        {
            fits.retain(|(_, _, results)| !self.all_any(results));
        }
        // Without a free fit, a guess is taken only when every guess narrows the caller's
        // types alike; disagreeing narrowings make the variable `Unknown` (unchecked, not wrong).
        if unchecked_argument
            && let Some((_, _, first)) = fits.first()
            && fits.iter().any(|(_, _, results)| results != first)
            && fits
                .iter()
                .any(|(_, _, results)| results.iter().any(|&result| !self.is_list_result(result)))
        {
            return self.undecided_overload_result(id, call_arguments);
        }
        // Beside open caller variables, an unchecked argument can still be what picks the
        // candidate: `vapply(xs, g, FUN.VALUE = value)` with an unknown `value` fits every
        // candidate whatever `g` narrows to, each with its own result.
        if !unchecked_argument && fit_results.len() > 1 {
            let mut decided = fit_results
                .iter()
                .filter(|result| !matches!(result.kind(self.db), TyKind::Any));
            let first = decided.next();
            if decided.any(|result| Some(result) != first)
                && call_arguments
                    .iter()
                    .any(|argument| argument.ty.is_some_and(|ty| self.is_unchecked(ty)))
                && !self.unchecked_arguments_decide_nothing(
                    callee,
                    schemes,
                    &fits_with_results(&fits, &fit_results),
                    call_arguments,
                )
            {
                // A caller variable the candidates narrow differently stays unchecked, as when
                // no candidate is picked below, and one they agree on takes that narrowing:
                // re-probing the first fit commits it, since an unchecked variable fits it too.
                if !self.unknown_disagreeing_narrowings(&caller_entries, &fits) {
                    return unknown(self.db);
                }
                if let Some(&(first_index, _, _)) = fits.first()
                    && let Some(scheme) = schemes.get(first_index)
                {
                    let snapshot = self.table.snapshot();
                    if self
                        .probe_overload_candidate(callee, scheme, call_arguments, fitting_courtesy)
                        .is_err()
                    {
                        self.table.rollback(snapshot);
                    }
                }
                return self.undecided_overload_result(id, call_arguments);
            }
        }
        if !unchecked_argument {
            self.bound_numeric_caller_variables(
                &caller_entries,
                &fits,
                &fit_results,
                call_arguments,
            );
        }
        if !unchecked_argument
            && let Some(var) = self.preserved_caller_variable(&caller_entries, &fits, &fit_results)
        {
            return Ty::new(self.db, TyKind::Var(var));
        }
        let winner = fits
            .iter()
            .find(|(_, free, _)| *free)
            .or_else(|| {
                let (_, _, first) = fits.first()?;
                fits.iter()
                    .all(|(_, _, narrowing)| narrowing == first)
                    .then(|| fits.first())
                    .flatten()
            })
            .or_else(|| {
                // A scalar narrowing wins over the same atom's vector, which accepts it:
                // `grepl(p, x)` binds `p` to `character`, as its `character[]` candidate also
                // takes that. Wider rules pick list shapes that later reads reject.
                let db = self.db;
                let scalar_of_vector = |actual: Ty<'db>, expected: Ty<'db>| {
                    actual == expected
                        || matches!(
                            (actual.kind(db), expected.kind(db)),
                            (TyKind::Scalar(atom), TyKind::Vector(element))
                                if element.kind(db) == &TyKind::Scalar(*atom)
                        )
                };
                fits.iter().find(|(_, _, narrowing)| {
                    fits.iter().all(|(_, _, other)| {
                        narrowing
                            .iter()
                            .zip(other)
                            .all(|(&actual, &expected)| scalar_of_vector(actual, expected))
                    })
                })
            });
        if winner.is_none() && !fits.is_empty() {
            self.undecided_calls.insert(id);
            if !self.unknown_disagreeing_narrowings(&caller_entries, &fits) {
                return unknown(self.db);
            }
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        // Re-probe the winner to commit: probing rolled back, and matching is a pure
        // function of the table.
        if let Some(&(index, _, _)) = winner
            && let Some(scheme) = schemes.get(index)
        {
            let snapshot = self.table.snapshot();
            match self.probe_overload_candidate(callee, scheme, call_arguments, fitting_courtesy) {
                Ok((resolved, function)) => {
                    self.recorded.insert(callee, resolved);
                    self.selected_overloads.insert(callee, index);
                    return self.committed_overload_return(id, function.ret);
                }
                Err(_) => self.table.rollback(snapshot),
            }
        }

        // Report the overload-set finding unless one answer exists: every candidate failed for the
        // same reason, or the candidates that got furthest (deepest first blamed argument; a
        // whole-call verdict blames none) did. The standard library declares an Array twin before
        // each `List` signature, and when the argument is no Array that twin's failure explains
        // nothing, so it is left out.
        let candidate_count = schemes.len();
        let (candidate_errors, candidate_count) = if candidate_errors.len() == candidate_count {
            let relevant: Vec<TypeError<'db>> = candidate_errors
                .iter()
                .filter(|error| {
                    !matches!(
                        &error.kind,
                        TypeErrorKind::Mismatch { expected, found }
                            if array_element(self.db, *expected).is_some()
                                && array_element(self.db, *found).is_none()
                    )
                })
                .cloned()
                .collect();
            if relevant.is_empty() || relevant.len() == candidate_errors.len() {
                (candidate_errors, candidate_count)
            } else {
                let count = relevant.len();
                (relevant, count)
            }
        } else {
            (candidate_errors, candidate_count)
        };
        let blamed: Vec<Option<usize>> = candidate_errors
            .iter()
            .map(|error| {
                call_arguments
                    .iter()
                    .position(|argument| argument.range == error.range)
            })
            .collect();
        let single_answer = if candidate_errors.len() != candidate_count {
            None
        } else if candidate_errors
            .windows(2)
            .all(|pair| pair.first() == pair.last())
        {
            candidate_errors.first()
        } else if let Some(deepest) = blamed.iter().copied().flatten().max()
            && let mut furthest = blamed
                .iter()
                .zip(&candidate_errors)
                .filter(|(blame, _)| **blame == Some(deepest))
                .map(|(_, error)| error)
            && let Some(first) = furthest.next()
            // Candidates that all want a function where the argument is none (`sapply(xs, 4)`
            // fails `fn(T) -> U` and the fallback's `fn(Any) -> Any`) agree on what is wrong.
            && furthest.all(|error| {
                let not_a_function = |kind: &TypeErrorKind<'db>| {
                    matches!(kind, TypeErrorKind::Mismatch { expected, found }
                        if matches!(expected.kind(self.db), TyKind::Function(_))
                            && !matches!(found.kind(self.db), TyKind::Function(_)))
                };
                error == first
                    || error.range == first.range
                        && not_a_function(&error.kind)
                        && not_a_function(&first.kind)
            })
        {
            Some(first)
        } else {
            None
        };
        let error = match single_answer {
            Some(error) => error.clone(),
            None => TypeError {
                range,
                kind: TypeErrorKind::NoMatchingOverload {
                    name: name.to_string(),
                    candidates: schemes.len(),
                    first: candidate_errors.first().cloned().map(Box::new),
                },
            },
        };
        self.errors.push(error);
        self.recorded.insert(callee, unknown(self.db));
        // A capture-discovery re-pass must not keep a stale commitment from the first pass.
        self.selected_overloads.remove(callee);
        unknown(self.db)
    }

    /// Like `is_unchecked`, but only for `Unknown`: a type inference could not determine.
    fn is_inferred_unknown(&mut self, ty: Ty<'db>) -> bool {
        let element = self.unchecked_candidate(ty);
        matches!(element.kind(self.db), TyKind::Unknown)
    }

    /// `c(x, unknown)` is as unchecked as `unknown` itself: its element type fits every
    /// candidate too.
    fn is_unchecked(&mut self, ty: Ty<'db>) -> bool {
        let element = self.unchecked_candidate(ty);
        matches!(element.kind(self.db), TyKind::Unknown | TyKind::Any)
    }

    /// The type that decides whether `ty` is unchecked: itself, or the element of a vector or
    /// list of it.
    fn unchecked_candidate(&mut self, ty: Ty<'db>) -> Ty<'db> {
        let resolved = self.table.shallow_resolve(self.db, ty);
        match resolved.kind(self.db) {
            TyKind::Vector(element)
            | TyKind::NamedVector(element)
            | TyKind::List(element)
            | TyKind::NamedList(element) => self.table.shallow_resolve(self.db, *element),
            _ => resolved,
        }
    }

    /// Whether every specific fit still fits when each unchecked argument is replaced by a
    /// type only `Any` or an unconstrained type parameter accepts. If so, those arguments
    /// land where any value is fine, and the known arguments alone chose the candidate.
    /// A call its candidates leave undecided: a declared `Any` is the author's opt-out, so the
    /// call it leaves ambiguous opts out too, as in mypy; only an argument whose type inference
    /// lost makes it `Unknown`. A forwarded `...` counts as `Any`, the element type of a `...`
    /// formal.
    fn undecided_overload_result(
        &mut self,
        id: ExprId,
        call_arguments: &[CallArgument<'db>],
    ) -> Ty<'db> {
        if call_arguments.iter().all(|argument| {
            argument.forwards_dots || argument.ty.is_none_or(|ty| !self.is_inferred_unknown(ty))
        }) {
            return any(self.db);
        }
        self.undecided_calls.insert(id);
        unknown(self.db)
    }

    /// Makes `Unknown` each caller variable the fitting candidates narrow differently, so it
    /// stays unchecked. `false` when one can no longer take `Unknown`.
    fn unknown_disagreeing_narrowings(
        &mut self,
        caller_entries: &[(InferenceVar, Entry<'db>)],
        fits: &[(usize, bool, Vec<Ty<'db>>)],
    ) -> bool {
        let Some((_, _, first)) = fits.first() else {
            return true;
        };
        for (position, (var, _)) in caller_entries.iter().enumerate() {
            if fits
                .iter()
                .any(|(_, _, narrowing)| narrowing.get(position) != first.get(position))
            {
                let var = Ty::new(self.db, TyKind::Var(*var));
                if self.table.unify(self.db, var, unknown(self.db)).is_err() {
                    return false;
                }
            }
        }
        true
    }

    fn unchecked_arguments_decide_nothing(
        &mut self,
        callee: ExprId,
        schemes: &[TypeScheme<'db>],
        fits: &[(usize, bool, Vec<Ty<'db>>)],
        call_arguments: &[CallArgument<'db>],
    ) -> bool {
        let witness = Ty::new(
            self.db,
            TyKind::Named(Name::new(self.db, UNCHECKED_WITNESS), &[]),
        );
        let mut witnessed = call_arguments.to_vec();
        for argument in &mut witnessed {
            if argument.ty.is_some_and(|ty| self.is_unchecked(ty)) {
                argument.ty = Some(witness);
            }
        }
        for (index, _, results) in fits {
            if self.all_any(results) {
                continue;
            }
            let Some(scheme) = schemes.get(*index) else {
                return false;
            };
            let snapshot = self.table.snapshot();
            let fits = self
                .probe_overload_candidate(callee, scheme, &witnessed, false)
                .is_ok();
            self.table.rollback(snapshot);
            if !fits {
                return false;
            }
        }
        true
    }

    fn all_any(&self, results: &[Ty<'db>]) -> bool {
        results
            .iter()
            .all(|&result| matches!(result.kind(self.db), TyKind::Any))
    }

    /// An `Any` return is the corpus's fallback for calls it cannot describe and passes
    /// every later check, so strict mode records it as a hole.
    fn committed_overload_return(&mut self, id: ExprId, ret: Ty<'db>) -> Ty<'db> {
        if matches!(
            self.table.shallow_resolve(self.db, ret).kind(self.db),
            TyKind::Any
        ) {
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
        }
        ret
    }

    /// `Ok`: the bindings are live; the caller keeps or rolls them back. `Err` is empty
    /// when the candidate is not a function type.
    fn probe_overload_candidate(
        &mut self,
        callee: ExprId,
        scheme: &TypeScheme<'db>,
        call_arguments: &[CallArgument<'db>],
        courtesy: bool,
    ) -> Result<(Ty<'db>, &'db FunctionType<'db>), Vec<TypeError<'db>>> {
        // Instantiation renames type variables and keeps the parameter list, so a candidate
        // the arguments cannot land on fails the same way uninstantiated. A courtesy probe
        // records expectations as it checks, so it always runs in full.
        if !courtesy && let TyKind::Function(function) = scheme.body.kind(self.db) {
            let arena = self.arena;
            let targets = &*argument_targets(
                self.db,
                arena,
                function,
                argument_shapes(arena, call_arguments),
            );
            if let Some(error) = shape_verdict(
                self.db,
                self.blame_range(callee),
                function,
                call_arguments,
                targets,
            ) {
                return Err(vec![error]);
            }
        }
        let instantiated = self.instantiate(scheme);
        let resolved = self.table.shallow_resolve(self.db, instantiated);
        let TyKind::Function(function) = resolved.kind(self.db) else {
            return Err(Vec::new());
        };
        if !courtesy {
            self.overload_probe_depth += 1;
        }
        let callee_range = self.blame_range(callee);
        let outcome = self.match_arguments(callee_range, function, call_arguments);
        if !courtesy {
            self.overload_probe_depth -= 1;
        }
        if outcome.is_empty() {
            Ok((resolved, *function))
        } else {
            Err(outcome)
        }
    }

    fn infer_call_arguments(
        &mut self,
        range: TextRange,
        arguments: &[Argument],
    ) -> Vec<CallArgument<'db>> {
        self.infer_call_arguments_against(range, arguments, &[])
    }

    /// Infers a call's arguments, typing an unannotated function literal last: its formals take the
    /// parameter types the callee expects there once the other arguments bound the type variables,
    /// so in `List.map(xs, \(x) c(x, x))` the body types over `xs`'s item type, not an open
    /// variable. `callees` are the instantiated candidates; a hint is used only where every
    /// candidate accepting the other arguments agrees on it.
    fn infer_call_arguments_against(
        &mut self,
        range: TextRange,
        arguments: &[Argument],
        callees: &[Ty<'db>],
    ) -> Vec<CallArgument<'db>> {
        let mut deferred = Bitset::new_in(self.arena, arguments.len());
        if !callees.is_empty() {
            for (index, argument) in arguments.iter().enumerate() {
                if argument
                    .value
                    .is_some_and(|value| self.needs_parameter_hints(value))
                {
                    deferred.insert(index);
                }
            }
        }
        let mut call_arguments: Vec<CallArgument<'db>> = arguments
            .iter()
            .map(|argument| self.call_argument(range, argument, None))
            .collect();
        let quoted = self.quoted_arguments(callees, &call_arguments);
        for (index, argument) in arguments.iter().enumerate() {
            if deferred.contains(index) {
                continue;
            }
            call_arguments[index].ty = if quoted.contains(index) {
                Some(unknown(self.db))
            } else if let Some(value) = argument.value
                && let Some(operand) = splice_operand(self.module, value)
            {
                Some(self.infer_splice(value, operand))
            } else {
                argument.value.map(|value| {
                    let ty = self.infer(value);
                    match self.template_call == Some(range)
                        && matches!(
                            self.table.resolve(self.db, ty).kind(self.db),
                            TyKind::Function(_)
                        ) {
                        true => unknown(self.db),
                        false => ty,
                    }
                })
            };
        }
        if deferred.is_empty() {
            return call_arguments;
        }
        let hints = self.parameter_hints_for(callees, &call_arguments, &deferred);
        for (index, argument) in arguments.iter().enumerate() {
            let Some(value) = argument.value.filter(|_| deferred.contains(index)) else {
                continue;
            };
            if let Some(hint) = hints.get(index).cloned().flatten() {
                self.parameter_hints.insert(value, hint);
            }
            let errors_before = self.errors.len();
            call_arguments[index].ty = Some(self.infer(value));
            self.name_contextual_parameters(value, errors_before, LambdaContext::Call);
        }
        call_arguments
    }

    /// The arguments every candidate callee binds to a quoted formal: code the callee captures
    /// with `substitute()`, which reads names from a frame the caller cannot see.
    fn quoted_arguments(
        &self,
        callees: &[Ty<'db>],
        arguments: &[CallArgument<'db>],
    ) -> Bitset<'a, usize> {
        let arena = self.arena;
        let mut quoted = Bitset::new_in(arena, arguments.len());
        if callees.is_empty() {
            return quoted;
        }
        for index in 0..arguments.len() {
            quoted.insert(index);
        }
        for &callee in callees {
            let resolved = self.table.resolve(self.db, callee);
            // Base R rejects `-"value"`, so beside a callee no stub types it is a tidyselect
            // exclusion (`dplyr::select(df, -"value")`) the callee quotes.
            if matches!(resolved.kind(self.db), TyKind::Unknown) {
                for (index, argument) in arguments.iter().enumerate() {
                    let excludes_by_name = argument.value.is_some_and(|value| {
                        matches!(self.module.kind(value), ExpressionKind::Unary {
                            operator: UnaryOperator::Minus,
                            operand,
                        } if matches!(self.module.kind(*operand), ExpressionKind::Literal(LiteralKind::String(_))))
                    });
                    if !excludes_by_name {
                        quoted.remove(index);
                    }
                }
                continue;
            }
            let TyKind::Function(function) = resolved.kind(self.db) else {
                return Bitset::new_in(arena, 0);
            };
            if function.quoted.is_empty() {
                return Bitset::new_in(arena, 0);
            }
            let targets =
                &*argument_targets(self.db, arena, function, argument_shapes(arena, arguments));
            for (index, target) in targets.iter().enumerate() {
                if !matches!(target, Some(ArgumentTarget::Named(formal))
                    if function.named.get(*formal).is_some_and(|parameter| function.quoted.contains(&parameter.name)))
                {
                    quoted.remove(index);
                }
            }
        }
        quoted
    }

    /// rlang's `!!!x` in a call passes each element of `x` as its own argument, so the argument
    /// types as the element every one of them has.
    fn infer_splice(&mut self, id: ExprId, operand: ExprId) -> Ty<'db> {
        let db = self.db;
        let spliced = self.infer(operand);
        let spliced = self.structural(spliced);
        let element = match spliced.kind(db) {
            TyKind::Unknown | TyKind::Any => spliced,
            TyKind::List(element)
            | TyKind::NamedList(element)
            | TyKind::Vector(element)
            | TyKind::NamedVector(element) => *element,
            TyKind::Tuple(items) if !items.is_empty() => union_of(db, items.iter().copied()),
            TyKind::Record(fields) if !fields.is_empty() => {
                union_of(db, fields.iter().map(|field| field.ty))
            }
            TyKind::Tuple(_) | TyKind::Record(_) | TyKind::Null => any(db),
            _ => {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(db)
            }
        };
        self.record(id, element)
    }

    fn call_argument(
        &self,
        range: TextRange,
        argument: &Argument,
        ty: Option<Ty<'db>>,
    ) -> CallArgument<'db> {
        let argument_range = argument
            .value
            .map(|value| self.blame_range(value))
            .unwrap_or(range);
        let whole_double = argument
            .value
            .is_some_and(|value| self.is_whole_double(value));
        // R expands an argument whose value is `...` whatever its tag, so PerformanceAnalytics'
        // `FUN(nx, ... = ...)` forwards the dots like `FUN(nx, ...)`.
        let forwards_dots = self
            .module
            .argument_name(argument)
            .is_none_or(|name| name == "...")
            && argument.value.is_some_and(|value| {
                matches!(
                    &self.module.expression(value).kind,
                    ExpressionKind::NameRef(name) if name == "..."
                ) || splice_operand(self.module, value).is_some()
            });
        CallArgument {
            name: self
                .module
                .argument_name(argument)
                .filter(|_| !forwards_dots),
            name_range: self.module.argument_name_range(argument),
            ty,
            range: argument_range,
            value: argument.value,
            whole_double,
            forwards_dots,
            named_function: None,
        }
    }

    /// Rewrites the findings that a typed function literal's body produced at a bare use of
    /// one of its formals, so they say the call supplied the type.
    fn name_contextual_parameters(
        &mut self,
        function: ExprId,
        errors_before: usize,
        context: LambdaContext,
    ) {
        let ExpressionKind::Function { parameters, body } = &self.module.expression(function).kind
        else {
            return;
        };
        if self.errors.len() == errors_before {
            return;
        }
        let formals: Vec<(BindingId, &str)> = self.module[parameters]
            .iter()
            .filter(|parameter| self.hinted_parameters.contains(&parameter.range))
            .filter_map(|parameter| {
                let binding = self
                    .naming
                    .bindings
                    .iter()
                    .find(|(_, info)| info.range == parameter.range)
                    .map(|(id, _)| *id)?;
                Some((binding, parameter.name.as_str()))
            })
            .collect();
        let mut uses: FxHashMap<TextRange, String> = FxHashMap::default();
        let mut pending = vec![*body];
        while let Some(id) = pending.pop() {
            if let ExpressionKind::NameRef(_) = self.module.kind(id)
                && let Some(binding) = self.naming.resolutions.get(&id)
                && let Some((_, name)) = formals.iter().find(|(formal, _)| formal == binding)
            {
                uses.insert(self.module.range(id), (*name).to_owned());
            }
            self.module.for_each_child(id, |child| pending.push(child));
        }
        for error in &mut self.errors[errors_before..] {
            if let TypeErrorKind::InvalidOperand { expected, found } = error.kind
                && let Some(name) = uses.get(&error.range)
            {
                error.kind = TypeErrorKind::ContextualParameter {
                    name: name.clone(),
                    expected,
                    passed: found,
                    context,
                };
            }
        }
    }

    /// Whether `value` is a literal that checking will adapt to `expected`, a ry scalar or an
    /// undecided `List` item, so a probe that only asks about compatibility does not refuse it.
    fn literal_may_adapt(&self, value: ExprId, expected: Ty<'db>) -> bool {
        let mut value = value;
        while let ExpressionKind::Paren(inner) = self.module.kind(value) {
            value = *inner;
        }
        let expected = self.table.resolve(self.db, expected);
        let target = match expected.kind(self.db) {
            TyKind::Native(native) => Some(*native),
            TyKind::Var(_) => match self.table.open_constraint(self.db, expected) {
                Some(Constraint::ArrayItem) => None,
                Some(constraint @ (Constraint::Number | Constraint::Int | Constraint::Frac)) => {
                    return number_literal(self.module, value)
                        .is_some_and(|literal| literal_meets(&literal, constraint));
                }
                _ => return false,
            },
            _ => return false,
        };
        if let Some(literal) = number_literal(self.module, value) {
            return target.is_none_or(|target| target.holds(&literal));
        }
        match self.module.kind(value) {
            ExpressionKind::Literal(LiteralKind::String(_)) => {
                target.is_none_or(|target| target == NativeScalar::Str)
            }
            ExpressionKind::Literal(LiteralKind::Logical(_)) => {
                target.is_none_or(|target| target == NativeScalar::Bool)
            }
            _ => false,
        }
    }

    /// The hint for a formal, if one survives the probe's rollback: a type with no open
    /// variable, or an undecided ry number that an argument brought (the items of `[1, 2]`),
    /// named by the argument's own variable, which the lambda body may still decide, as
    /// `\(x) x / 2` decides `x` is a fraction.
    fn settled_hint(
        &self,
        formal: Ty<'db>,
        argument_vars: &FxHashSet<InferenceVar>,
    ) -> Option<Ty<'db>> {
        let TyKind::Var(var) = *formal.kind(self.db) else {
            return (!self.table.contains_unbound_var(self.db, formal)).then_some(formal);
        };
        let representative = self.table.find(var);
        if !matches!(
            self.table.entry(representative),
            Entry::Unbound {
                constraint: Constraint::Number | Constraint::Int | Constraint::Frac,
                ..
            }
        ) {
            return None;
        }
        argument_vars
            .iter()
            .copied()
            .filter(|&candidate| self.table.find(candidate) == representative)
            .min_by_key(|candidate| candidate.0)
            .map(|candidate| Ty::new(self.db, TyKind::Var(candidate)))
    }

    /// A function literal with a formal that has neither a type annotation nor a default. A
    /// formal after `...` is left out, because R binds positional arguments to `...` first.
    fn needs_parameter_hints(&self, value: ExprId) -> bool {
        let ExpressionKind::Function { parameters, .. } = &self.module.expression(value).kind
        else {
            return false;
        };
        let parameters = &self.module[parameters];
        let leading = parameters
            .iter()
            .position(|parameter| parameter.name == "...")
            .map_or(parameters, |dots| &parameters[..dots]);
        let after_dots_required = parameters[leading.len()..]
            .iter()
            .any(|parameter| parameter.name != "..." && parameter.default.is_none());
        self.declared_function_annotation(value).is_none()
            && self.inline_parameters(value).is_empty()
            && !after_dots_required
            && leading.iter().any(|parameter| parameter.default.is_none())
    }

    /// Gives the untyped function literal `value` the parameter types of the declared function
    /// type `expected`, as a call gives them to a literal it is passed. Reports whether it did.
    fn hint_from_declared_function(&mut self, value: ExprId, expected: Ty<'db>) -> bool {
        if !self.needs_parameter_hints(value) {
            return false;
        }
        let expected = self.table.resolve(self.db, expected);
        let TyKind::Function(expected) = expected.kind(self.db) else {
            return false;
        };
        let hints: Vec<Option<Ty<'db>>> = expected
            .positional
            .iter()
            .copied()
            .chain(expected.named.iter().map(|parameter| parameter.ty))
            .map(|formal| {
                (!matches!(formal.kind(self.db), TyKind::Any | TyKind::Unknown)
                    && !self.table.contains_unbound_var(self.db, formal))
                .then_some(formal)
            })
            .collect();
        if hints.iter().all(Option::is_none) {
            return false;
        }
        self.parameter_hints.insert(value, hints);
        true
    }

    /// Per argument, the parameter types its deferred function literal takes, found by
    /// probing each candidate with the already-inferred arguments and rolling back.
    fn parameter_hints_for(
        &mut self,
        callees: &[Ty<'db>],
        arguments: &[CallArgument<'db>],
        deferred: &Bitset<'a, usize>,
    ) -> Vec<Option<Vec<Option<Ty<'db>>>>> {
        let mut agreed: Vec<Option<Vec<Option<Ty<'db>>>>> = vec![None; arguments.len()];
        let mut first_candidate = true;
        for &callee in callees {
            let resolved = self.table.shallow_resolve(self.db, callee);
            let resolved = if matches!(resolved.kind(self.db), TyKind::Named(..)) {
                self.structural(resolved)
            } else {
                resolved
            };
            let TyKind::Function(function) = resolved.kind(self.db) else {
                continue;
            };
            let arena = self.arena;
            let targets =
                argument_targets(self.db, arena, function, argument_shapes(arena, arguments));
            let target_ty = |target: &Option<ArgumentTarget>| match target {
                Some(ArgumentTarget::Positional(index)) => function.positional.get(*index).copied(),
                Some(ArgumentTarget::Named(index)) => {
                    function.named.get(*index).map(|parameter| parameter.ty)
                }
                Some(ArgumentTarget::Rest) => function.variadic.as_ref().map(|rest| rest.element),
                _ => None,
            };
            // A formal is an accumulator, as `S` is in `walk(list, state: S, f: fn(S, T) -> S)`,
            // when it shares a type variable with the callback's result and another argument is
            // that bare variable, the initial state. The result widens such a variable, so the
            // type the initial value gives it (an empty `list()`) is no context. A variable fixed
            // by a collection, as `T` is in `update(list: List<T>, i, f: fn(T) -> T)`, is not
            // widened, so its formal keeps the hint. Read before the probe binds the variables.
            let state_vars: FxHashSet<InferenceVar> = targets
                .iter()
                .enumerate()
                .filter(|(index, _)| !deferred.contains(*index))
                .filter_map(|(_, target)| target_ty(target))
                .filter_map(
                    |ty| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
                        TyKind::Var(var) => Some(*var),
                        _ => None,
                    },
                )
                .collect();
            let accumulators: Vec<Bitset<'a, usize>> = targets
                .iter()
                .enumerate()
                .map(|(index, target)| {
                    let mut accumulating = Bitset::new_in(arena, 0);
                    let expected = target_ty(target)
                        .filter(|_| deferred.contains(index))
                        .map(|expected| self.table.shallow_resolve(self.db, expected));
                    let Some(TyKind::Function(expected)) = expected.map(|ty| ty.kind(self.db))
                    else {
                        return accumulating;
                    };
                    let mut result_vars = FxHashSet::default();
                    self.table
                        .collect_unbound_vars(self.db, expected.ret, &mut result_vars);
                    result_vars.retain(|var| state_vars.contains(var));
                    let formals = expected
                        .positional
                        .iter()
                        .copied()
                        .chain(expected.named.iter().map(|parameter| parameter.ty));
                    for (formal_index, formal) in formals.enumerate() {
                        let mut formal_vars = FxHashSet::default();
                        self.table
                            .collect_unbound_vars(self.db, formal, &mut formal_vars);
                        if !formal_vars.is_disjoint(&result_vars) {
                            accumulating.insert(formal_index);
                        }
                    }
                    accumulating
                })
                .collect();
            let mut argument_vars = FxHashSet::default();
            for (index, argument) in arguments.iter().enumerate() {
                if let (Some(ty), false) = (argument.ty, deferred.contains(index)) {
                    self.table
                        .collect_unbound_vars(self.db, ty, &mut argument_vars);
                }
            }
            let snapshot = self.table.snapshot();
            let accepts = arguments.iter().zip(targets.iter()).enumerate().all(
                |(index, (argument, target))| {
                    if deferred.contains(index) {
                        return true;
                    }
                    match (argument.ty, target_ty(target)) {
                        (Some(actual), Some(expected)) => {
                            argument
                                .value
                                .is_some_and(|value| self.literal_may_adapt(value, expected))
                                || self.table.compatible(self.db, actual, expected)
                        }
                        // An argument the candidate has no parameter for rules the candidate out.
                        (Some(_), None) => false,
                        _ => true,
                    }
                },
            );
            let candidate: Vec<Option<Vec<Option<Ty<'db>>>>> = targets
                .iter()
                .enumerate()
                .zip(&accumulators)
                .zip(arguments)
                .map(|(((argument_index, target), accumulators), argument)| {
                    if !deferred.contains(argument_index) || !accepts {
                        return None;
                    }
                    let expected = self.table.shallow_resolve(self.db, target_ty(target)?);
                    let TyKind::Function(expected) = expected.kind(self.db) else {
                        return None;
                    };
                    let formals = expected
                        .positional
                        .iter()
                        .copied()
                        .chain(expected.named.iter().map(|parameter| parameter.ty));
                    // A context that passes `...: Any` gives every formal past its own an `Any`
                    // argument, as the author declared, so `pr_post(router, "/p",
                    // function(req) req$body)` reads `req` as `Any`.
                    let declared_count = expected.positional.len() + expected.named.len();
                    let rest_any = expected.variadic.as_ref().is_some_and(|rest| {
                        matches!(
                            self.table.resolve(self.db, rest.element).kind(self.db),
                            TyKind::Any
                        )
                    });
                    let literal_count = argument.value.map_or(0, |value| {
                        match &self.module.expression(value).kind {
                            ExpressionKind::Function { parameters, .. } => self.module[parameters]
                                .iter()
                                .filter(|parameter| parameter.name != "...")
                                .count(),
                            _ => 0,
                        }
                    });
                    let padding = if rest_any {
                        literal_count.saturating_sub(declared_count)
                    } else {
                        0
                    };
                    Some(
                        formals
                            .chain(std::iter::repeat_n(any(self.db), padding))
                            .enumerate()
                            .map(|(index, formal)| {
                                let formal = self.table.resolve(self.db, formal);
                                // An accumulator whose initial state is a settled ry value keeps
                                // that type, because only a tag union widens among ry types. In
                                // `List.walk(xs, List.repeat(0f64, n), \(sums, x) sums + x)` the
                                // state is `List<F64>` before the body reads `sums + x`. A state
                                // with a tag anywhere in it, such as `[:A]`, stays open.
                                let widening_accumulator = accumulators.contains(index)
                                    && !(is_ry_value_type(self.db, formal)
                                        && !mentions_tag(self.db, formal)
                                        && !self.table.contains_unbound_var(self.db, formal));
                                if widening_accumulator
                                    || (index < declared_count
                                        && matches!(
                                            formal.kind(self.db),
                                            TyKind::Any | TyKind::Unknown
                                        ))
                                {
                                    return None;
                                }
                                self.settled_hint(formal, &argument_vars)
                            })
                            .collect(),
                    )
                })
                .collect();
            self.table.rollback(snapshot);
            if !accepts {
                continue;
            }
            for (slot, hint) in agreed.iter_mut().zip(candidate) {
                *slot = match (first_candidate, slot.take(), hint) {
                    (true, _, hint) => hint,
                    (false, Some(previous), Some(hint)) => Some(
                        previous
                            .iter()
                            .zip(&hint)
                            .map(|(left, right)| if left == right { *left } else { None })
                            .collect(),
                    ),
                    _ => None,
                };
            }
            first_candidate = false;
        }
        agreed
    }

    /// R's functional helpers resolve their function argument with `match.fun`, which also
    /// accepts the function's name as a string (`lapply(xs, "[[", "id")`). Only the base
    /// helpers do this; a closure's own parameter called as a function must hold a function.
    fn resolve_match_fun_arguments(
        &mut self,
        callee: ExprId,
        call_arguments: &mut [CallArgument<'db>],
    ) {
        let name = match self.module.kind(callee) {
            ExpressionKind::NameRef(name) => {
                let deferred = self.naming.deferred_non_locals.contains(&callee);
                if self.naming.resolutions.contains_key(&callee)
                    || self
                        .globals
                        .is_some_and(|globals| globals.defined_in_project(*name, deferred))
                {
                    return;
                }
                name.as_str()
            }
            ExpressionKind::Namespace {
                package: Some(package),
                name: Some(name),
                ..
            } if NATIVE_NAMESPACES.contains(&package.as_str()) => name.as_str(),
            _ => return,
        };
        if !MATCH_FUN_CALLERS.contains(&name) {
            return;
        }
        for argument in call_arguments.iter_mut() {
            let (Some(value), Some(ty)) = (argument.value, argument.ty) else {
                continue;
            };
            let resolved = self.table.resolve(self.db, ty);
            // `match.fun` looks a bare name up with `mode = "function"`, past a binding that
            // holds no function, so sessioninfo's mocking placeholder `getNamespaceVersion <-
            // NULL` leaves `vapply(pkgs, getNamespaceVersion, "")` calling base's function.
            // A local list named like the function (psych's `apply(m, 1, prod)` beside a
            // `prod` list) is passed over the same way; a local string names the function to use.
            // A local holding one of several functions (`F <- if (cond) function(i) ... else
            // function(i) ...`) is a function to R's lookup.
            if let ExpressionKind::NameRef(name) = self.module.kind(value)
                && (!self.naming.resolutions.contains_key(&value)
                    || !is_character(self.db, resolved))
                && match resolved.kind(self.db) {
                    TyKind::Var(_) => false,
                    TyKind::Rigid(_) => true,
                    _ => self.callable(resolved) == Callable::No,
                }
            {
                // An export the stubs list without a type is still a function.
                if let Some(candidates) = self
                    .globals
                    .and_then(|globals| globals.library_candidates(*name))
                {
                    argument.named_function = Some(match candidates.last() {
                        Some(scheme) => self.instantiate(scheme),
                        None => unknown(self.db),
                    });
                }
                continue;
            }
            // `match.fun` takes a function or its name, so an open variable the body also reads
            // as a value (kernlab's `apply(x, 2, var)` beside `if (var < 10^-3)`, diffobj's
            // `lapply(hunk[nm], op, n)` beside `if (op == "tail")`, psych's `prod <- groups`
            // beside `prod[[i]]`) is not pinned to a function here. One used only as a
            // function still infers as one.
            if let Some(&binding) = self.naming.resolutions.get(&value)
                && matches!(resolved.kind(self.db), TyKind::Var(_))
                && self.reads_as_value(binding)
            {
                argument.named_function = Some(unknown(self.db));
                continue;
            }
            if !is_character(self.db, resolved) {
                continue;
            }
            let named = match self.module.kind(value) {
                ExpressionKind::Literal(LiteralKind::String(text)) => self
                    .globals
                    .and_then(|globals| globals.scheme(Symbol::new(&self.module[text]), false))
                    .map(|scheme| self.instantiate(&scheme)),
                _ => None,
            };
            argument.named_function = Some(named.unwrap_or_else(|| unknown(self.db)));
        }
    }

    /// Whether some read of the binding is neither a call's callee nor one of its arguments.
    fn reads_as_value(&self, binding: BindingId) -> bool {
        let module = self.module;
        let reads = |child: ExprId| self.naming.resolutions.get(&child) == Some(&binding);
        module.expression_ids().any(|id| match module.kind(id) {
            ExpressionKind::Call { .. } => false,
            ExpressionKind::Assign { value, .. } => reads(*value),
            _ => module.any_child(id, reads),
        })
    }

    fn is_whole_double(&self, value: ExprId) -> bool {
        match self.module.kind(value) {
            ExpressionKind::Literal(LiteralKind::Double(text)) => {
                is_whole_number_double(&self.module[text])
            }
            _ => false,
        }
    }

    fn dispatch_call(
        &mut self,
        range: TextRange,
        callee_range: TextRange,
        callee: Ty<'db>,
        source: CalleeSource,
        arguments: &[CallArgument<'db>],
    ) -> Ty<'db> {
        let resolved = self.table.shallow_resolve(self.db, callee);
        // Alias- or nominal-typed callees call through their representation; an undeclared
        // name floors to `Unknown` and its diagnostic lives elsewhere.
        let resolved = if matches!(resolved.kind(self.db), TyKind::Named(..)) {
            self.structural(resolved)
        } else {
            resolved
        };
        match resolved.kind(self.db) {
            TyKind::Function(function) => {
                if let Some(folded) =
                    self.forwarded_reduce(range, callee_range, function, arguments)
                {
                    return folded;
                }
                let mut findings = self.match_arguments(callee_range, function, arguments);
                // `api_func(op, options = o)` then `api_func(op)`: what one call passes, the real
                // callee may give a default, so a later call may leave it out. A later call that
                // adds an argument is still reported.
                if function.from_calls {
                    findings.retain(|finding| match finding.kind {
                        TypeErrorKind::MissingArguments { .. } => false,
                        TypeErrorKind::ArityMismatch { expected, found } => found > expected,
                        _ => true,
                    });
                }
                let narrowed = findings.is_empty() && function.inferred;
                self.errors.extend(findings);
                if narrowed {
                    self.flag_selected_return(function, arguments)
                        .or_else(|| self.null_excluded_return(function, arguments))
                        .unwrap_or_else(|| self.narrow_echoed_parameter_unions(function, arguments))
                } else {
                    function.ret
                }
            }
            TyKind::Any | TyKind::Unknown => unknown(self.db),
            TyKind::Var(_) => {
                let ret = self.fresh(Constraint::Unconstrained);
                // A forwarded `...` may carry any number of arguments under any names, so it
                // makes the callee variadic; what the dots carry is checked where the outer
                // function is called.
                let forwards_dots = arguments.iter().any(|argument| argument.forwards_dots);
                let positional: Vec<Ty<'db>> = arguments
                    .iter()
                    .filter(|argument| argument.name.is_none() && !argument.forwards_dots)
                    .map(|argument| argument.ty.unwrap_or_else(|| unknown(self.db)))
                    .collect();
                let expected = Ty::new(
                    self.db,
                    TyKind::Function(&FunctionType {
                        positional: &positional,
                        named: &arguments
                            .iter()
                            .filter_map(|argument| {
                                argument.name.as_ref().map(|name| Parameter {
                                    name: Name::new(self.db, name),
                                    ty: argument.ty.unwrap_or_else(|| unknown(self.db)),
                                    optional: false,
                                    default: None,
                                })
                            })
                            .collect::<Vec<_>>(),
                        variadic: forwards_dots.then(|| RestParameter {
                            element: any(self.db),
                            preceding_named: 0,
                        }),
                        ret,
                        inferred: true,
                        flag_return: None,
                        quoted: &[],
                        default_reads: &[],
                        missing_tested: &[],
                        condition_only: &[],
                        dispatch: None,
                        null_unless: None,
                        reduces: None,
                        from_calls: true,
                    }),
                );
                self.unify_or_report(range, expected, resolved);
                ret
            }
            // Functions plus `NULL` (a default-less `switch`): callable on every other path, so
            // the finding is the nullability and the call still checks against the functions.
            TyKind::Union(members)
                if members
                    .iter()
                    .any(|&member| matches!(member.kind(self.db), TyKind::Null))
                    && members.iter().all(|&member| {
                        matches!(
                            self.table.shallow_resolve(self.db, member).kind(self.db),
                            TyKind::Function(_) | TyKind::Null
                        )
                    }) =>
            {
                self.errors.push(TypeError {
                    range: callee_range,
                    kind: TypeErrorKind::MaybeNullCallee {
                        found: self.table.resolve(self.db, resolved),
                    },
                });
                let callable: Vec<Ty<'db>> = members
                    .iter()
                    .copied()
                    .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
                    .collect();
                let without_null = union_of(self.db, callable);
                self.dispatch_call(range, callee_range, without_null, source, arguments)
            }
            // A function joined with an unbound variable or a generic parameter (`f <- function(source = NULL) {
            // if (is.null(source)) source <- function(x) x; source(1) }`): the variable is
            // whatever callable the caller passes, so only the functions are checked and the
            // result is unchecked.
            TyKind::Union(members)
                if members.iter().any(|&member| {
                    matches!(
                        self.table.shallow_resolve(self.db, member).kind(self.db),
                        TyKind::Function(_)
                    )
                }) && members.iter().any(|&member| {
                    matches!(
                        self.table.shallow_resolve(self.db, member).kind(self.db),
                        TyKind::Var(_) | TyKind::Rigid(_)
                    )
                }) && members.iter().all(|&member| {
                    matches!(
                        self.table.shallow_resolve(self.db, member).kind(self.db),
                        TyKind::Function(_) | TyKind::Var(_) | TyKind::Rigid(_)
                    )
                }) =>
            {
                let functions: Vec<Ty<'db>> = members
                    .iter()
                    .copied()
                    .filter(|&member| {
                        matches!(
                            self.table.shallow_resolve(self.db, member).kind(self.db),
                            TyKind::Function(_)
                        )
                    })
                    .collect();
                let functions = union_of(self.db, functions);
                self.dispatch_call(range, callee_range, functions, source, arguments);
                unknown(self.db)
            }
            // A union of functions (`handlers[[name]](...)`) must accept the call for every
            // member; each probes in an isolated snapshot, so the returns are variable-erased.
            TyKind::Union(members)
                if members.iter().all(|&member| {
                    matches!(
                        self.table.shallow_resolve(self.db, member).kind(self.db),
                        TyKind::Function(_)
                    )
                }) =>
            {
                let functions: Vec<&'db FunctionType<'db>> = members
                    .iter()
                    .filter_map(|&member| {
                        match self.table.shallow_resolve(self.db, member).kind(self.db) {
                            TyKind::Function(function) => Some(*function),
                            _ => None,
                        }
                    })
                    .collect();
                let shape = |function: &FunctionType<'db>| {
                    (
                        function.positional.len(),
                        function
                            .named
                            .iter()
                            .map(|parameter| (parameter.name, parameter.optional))
                            .collect::<Vec<_>>(),
                        function.variadic.is_some(),
                    )
                };
                // A value (not a lookup in a collection) whose members take different
                // parameters comes from code that picks a function by an argument value the
                // checker does not follow, as rlang's `compat("warn")` returns either a one-message
                // shim or `interactive`. Such a call presumes a member that accepts it.
                let selected_by_value = source == CalleeSource::Value
                    && functions.split_first().is_some_and(|(first, rest)| {
                        rest.iter().any(|function| shape(function) != shape(first))
                    });
                if selected_by_value {
                    let mut accepted = 0;
                    let mut first_findings = Vec::new();
                    for function in &functions {
                        let snapshot = self.table.snapshot();
                        let findings = self.match_arguments(callee_range, function, arguments);
                        self.table.rollback(snapshot);
                        if findings.is_empty() {
                            accepted += 1;
                        } else if first_findings.is_empty() {
                            first_findings = findings;
                        }
                    }
                    if accepted == 0 {
                        self.errors.extend(first_findings);
                        return unknown(self.db);
                    }
                    if accepted < functions.len() {
                        return unknown(self.db);
                    }
                }
                let caller_entries = self.caller_open_variables(arguments);
                let mut narrowed = false;
                let mut returns = Vec::with_capacity(functions.len());
                for function in &functions {
                    let snapshot = self.table.snapshot();
                    let findings = self.match_arguments(callee_range, function, arguments);
                    if findings.is_empty() {
                        narrowed |= !self.caller_variables_untouched(&caller_entries);
                        let member_return = self.table.resolve(self.db, function.ret);
                        self.table.rollback(snapshot);
                        returns.push(erase_vars(self.db, member_return));
                    } else {
                        self.table.rollback(snapshot);
                        self.errors.extend(findings);
                        return unknown(self.db);
                    }
                }
                // A caller variable a member accepted only by narrowing would stay open after
                // the rollback; no single signature states what the members demand together, so
                // it becomes `Unknown`.
                if narrowed {
                    for (var, _) in &caller_entries {
                        let var = Ty::new(self.db, TyKind::Var(*var));
                        let unknown = unknown(self.db);
                        if self.table.unify(self.db, var, unknown).is_err() {
                            return unknown;
                        }
                    }
                }
                union_of(self.db, returns)
            }
            // A function joined with plain values comes from code whose result kind depends on
            // an argument the checker does not follow, such as shiny's `NS(namespace)`, which
            // returns a function exactly when `id` is missing. A call presumes the function, so
            // only the functions are checked and the result is unchecked.
            TyKind::Union(members)
                if members.iter().any(|&member| {
                    matches!(
                        self.table.shallow_resolve(self.db, member).kind(self.db),
                        TyKind::Function(_)
                    )
                }) =>
            {
                let functions: Vec<Ty<'db>> = members
                    .iter()
                    .copied()
                    .filter(|&member| {
                        matches!(
                            self.table.shallow_resolve(self.db, member).kind(self.db),
                            TyKind::Function(_)
                        )
                    })
                    .collect();
                let functions = union_of(self.db, functions);
                self.dispatch_call(range, callee_range, functions, source, arguments);
                unknown(self.db)
            }
            _ => {
                self.errors.push(TypeError {
                    range: callee_range,
                    kind: TypeErrorKind::NotAFunction {
                        found: self.table.resolve(self.db, resolved),
                    },
                });
                unknown(self.db)
            }
        }
    }

    /// The branch a call selects when it leaves the flag out, so its default decides, or
    /// passes a literal `TRUE` or `FALSE`.
    fn flag_selected_return(
        &self,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Option<Ty<'db>> {
        let flag = function.flag_return?;
        // A forwarded `...` could carry the flag under its name, but treating every forwarding
        // call as unselected reports DistributionUtils' `pDist(densFn, q, ...) - p`, whose callers
        // never pass `valueOnly` that way. Selecting by the visible arguments trades that rare
        // miss for no false report.
        let formal = function
            .named
            .iter()
            .position(|parameter| parameter.name == flag.parameter)?;
        let arena = self.arena;
        let targets =
            &*argument_targets(self.db, arena, function, argument_shapes(arena, arguments));
        let passed = arguments.iter().zip(targets).find(
            |(_, target)| matches!(target, Some(ArgumentTarget::Named(index)) if *index == formal),
        );
        let value = match passed {
            None => flag.default,
            Some((argument, _)) => {
                let mut value = argument.value?;
                while let ExpressionKind::Paren(inner) = self.module.kind(value) {
                    value = *inner;
                }
                match self.module.kind(value) {
                    ExpressionKind::Literal(LiteralKind::Logical(value)) => *value,
                    _ => return None,
                }
            }
        };
        Some(if value {
            flag.when_true
        } else {
            flag.when_false
        })
    }

    /// A call to a function whose body forwards its formals to `Reduce` folds like `Reduce`
    /// itself. The other arguments are matched as usual, with the callback left to the fold.
    fn forwarded_reduce(
        &mut self,
        range: TextRange,
        callee_range: TextRange,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Option<Ty<'db>> {
        let forward = function.reduces?;
        let db = self.db;
        let arena = self.arena;
        let targets: Vec<Option<ArgumentTarget>> =
            argument_targets(db, arena, function, argument_shapes(arena, arguments)).to_vec();
        // The fold calls the callback with two arguments, so a call that also forwards extra
        // arguments through `...` keeps the wrapper's ordinary signature.
        if targets.contains(&Some(ArgumentTarget::Rest)) {
            return None;
        }
        let passed = |formal: Name<'db>| {
            let index = function
                .named
                .iter()
                .position(|parameter| parameter.name == formal)?;
            arguments.iter().zip(&targets).position(
                |(_, target)| matches!(target, Some(ArgumentTarget::Named(at)) if *at == index),
            )
        };
        let function_at = passed(forward.function)?;
        let x_at = passed(forward.x)?;
        let init_at = forward.init.and_then(passed);
        let callback = arguments.get(function_at)?.value?;
        let x = arguments.get(x_at)?.value?;
        let recorded_errors = self.errors.len();
        let recorded_origins = self.strict_origins.len();
        let first_var = self.table.next_var().0;
        let callback_ty = self.infer(callback);
        let callback_ty = self.table.resolve(db, callback_ty);
        self.truncate_errors(recorded_errors);
        self.strict_origins.truncate(recorded_origins);
        if !matches!(callback_ty.kind(db), TyKind::Function(_))
            || self.mentions_var_before(callback_ty, first_var)
        {
            return None;
        }
        let x_ty = arguments.get(x_at)?.ty?;
        let x_ty = self.structural(x_ty);
        let (elements, _) = element_types(db, x_ty);
        let element = self.join_all(&elements);
        let (start, accumulator) = match init_at {
            Some(at) => (arguments.get(at)?.value?, arguments.get(at)?.ty?),
            None => (x, element),
        };
        let mut rest = arguments.to_vec();
        if let Some(argument) = rest.get_mut(function_at) {
            argument.ty = Some(unknown(db));
        }
        let findings = self.match_arguments(callee_range, function, &rest);
        self.errors.extend(findings);
        Some(self.fold_reduce(
            range,
            callee_range,
            callback,
            callback_ty,
            start,
            x,
            accumulator,
            element,
        ))
    }

    /// The return without `NULL` when the caller already showed the predicate that guards the
    /// callee's `return(NULL)` holds of the argument it passes.
    fn null_excluded_return(
        &self,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Option<Ty<'db>> {
        let guard = function.null_unless?;
        let formal = function
            .named
            .iter()
            .position(|parameter| parameter.name == guard.parameter)?;
        let arena = self.arena;
        let targets =
            &*argument_targets(self.db, arena, function, argument_shapes(arena, arguments));
        let (argument, _) = arguments.iter().zip(targets).find(
            |(_, target)| matches!(target, Some(ArgumentTarget::Named(index)) if *index == formal),
        )?;
        let mut value = argument.value?;
        while let ExpressionKind::Paren(inner) = self.module.kind(value) {
            value = *inner;
        }
        // A class dispatch answers `NULL` for an object of no class it handles, and only a
        // plain atomic vector, `NULL` or a function is known to carry no class.
        let may_carry_class = guard.class_fallback
            && argument.ty.is_some_and(|ty| {
                !matches!(
                    self.table.shallow_resolve(self.db, ty).kind(self.db),
                    TyKind::Scalar(_)
                        | TyKind::Vector(_)
                        | TyKind::NamedVector(_)
                        | TyKind::Null
                        | TyKind::Function(_)
                )
            });
        if !may_carry_class {
            let slot = *self.naming.resolutions.get(&value)?;
            if !self
                .environment
                .holds_of(slot, Symbol::new(guard.predicate.text(self.db)))
            {
                return None;
            }
        }
        let ret = self.table.resolve(self.db, function.ret);
        let TyKind::Union(members) = ret.kind(self.db) else {
            return None;
        };
        Some(union_of(
            self.db,
            members
                .iter()
                .copied()
                .filter(|member| !matches!(member.kind(self.db), TyKind::Null)),
        ))
    }

    /// The return of a call to an inferred signature whose return is a union.
    ///
    /// An inferred parameter typed `list[T] | T[]` takes a list or a vector, and a body that
    /// hands the parameter back returns that same union. A call that passes one of the two,
    /// or a union of members the parameter accepts, returns what it passed:
    /// `normalize(tempfile())` is a `character`, not also a `list[character]`.
    fn narrow_echoed_parameter_unions(
        &mut self,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Ty<'db> {
        let ret = self.table.resolve(self.db, function.ret);
        let TyKind::Union(returned) = ret.kind(self.db) else {
            return function.ret;
        };
        let arena = self.arena;
        let targets =
            &*argument_targets(self.db, arena, function, argument_shapes(arena, arguments));
        // Each argument is measured against the return as declared, so two parameters that
        // echo the same union each contribute what they were passed.
        let mut echoed: Vec<Ty<'db>> = Vec::new();
        let mut passed_members: Vec<Ty<'db>> = Vec::new();
        for (argument, target) in arguments.iter().zip(targets) {
            let expected = match target {
                Some(ArgumentTarget::Positional(slot)) => function.positional.get(*slot).copied(),
                Some(ArgumentTarget::Named(formal)) => {
                    function.named.get(*formal).map(|parameter| parameter.ty)
                }
                _ => None,
            };
            let (Some(expected), Some(passed)) = (expected, argument.ty) else {
                continue;
            };
            let expected = self.table.resolve(self.db, expected);
            let passed = self.table.resolve(self.db, passed);
            let TyKind::Union(accepted) = expected.kind(self.db) else {
                continue;
            };
            if matches!(passed.kind(self.db), TyKind::Var(_))
                || !accepted.iter().all(|member| returned.contains(member))
            {
                continue;
            }
            echoed.extend(accepted.iter().copied());
            passed_members.push(passed);
        }
        let mut members: Vec<Ty<'db>> = returned
            .iter()
            .copied()
            .filter(|member| !echoed.contains(member))
            .collect();
        members.extend(passed_members);
        // A member that is a variable no argument binds stands for anything, so the union does
        // too: the value of a parameter the call left out, as in a getter and setter
        // `function(key, value)` called as a getter, or a variable the callee's scheme leaves
        // free. A variable of the enclosing scope, which a closure returns, keeps its meaning.
        let mut supplied_positional = Bitset::new_in(self.arena, function.positional.len());
        let mut supplied_named = Bitset::new_in(self.arena, function.named.len());
        for target in targets.iter().flatten() {
            match target {
                ArgumentTarget::Positional(slot) => {
                    supplied_positional.insert(*slot);
                }
                ArgumentTarget::Named(formal) => {
                    supplied_named.insert(*formal);
                }
                _ => {}
            }
        }
        let omitted_positional = function
            .positional
            .iter()
            .enumerate()
            .filter(|(slot, _)| !supplied_positional.contains(*slot))
            .map(|(_, ty)| *ty);
        let omitted_named = function
            .named
            .iter()
            .enumerate()
            .filter(|(formal, _)| !supplied_named.contains(*formal))
            .map(|(_, parameter)| parameter.ty);
        let mut omitted = FxHashSet::default();
        for ty in omitted_positional.chain(omitted_named) {
            collect_vars(self.db, self.table.resolve(self.db, ty), &mut omitted);
        }
        let mut bound = FxHashSet::default();
        for argument in arguments {
            if let Some(ty) = argument.ty {
                collect_vars(self.db, self.table.resolve(self.db, ty), &mut bound);
            }
        }
        // A variable only a supplied parameter mentions got its value from that argument, so
        // when the argument binds nothing to it (`NULL` passed to `T | NULL`) it stands for no
        // value, and the call's own result narrowing drops it.
        let mut supplied = FxHashSet::default();
        for ty in function
            .positional
            .iter()
            .enumerate()
            .filter(|(slot, _)| supplied_positional.contains(*slot))
            .map(|(_, ty)| *ty)
            .chain(
                function
                    .named
                    .iter()
                    .enumerate()
                    .filter(|(formal, _)| supplied_named.contains(*formal))
                    .map(|(_, parameter)| parameter.ty),
            )
        {
            collect_vars(self.db, self.table.resolve(self.db, ty), &mut supplied);
        }
        if members.iter().any(|member| {
            matches!(member.kind(self.db), TyKind::Var(var)
                if (omitted.contains(var)
                    || (var.0 >= self.call_first_var && !supplied.contains(var)))
                    && !bound.contains(var))
        }) {
            return unknown(self.db);
        }
        union_of(self.db, members)
    }

    /// Checks every argument (a failed `compatible` leaves the table untouched, so
    /// verdicts are independent); empty means the call matches. A structural failure is
    /// returned alone: per-argument mismatches under a mis-shaped call mislead.
    fn match_arguments(
        &mut self,
        callee_range: TextRange,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Vec<TypeError<'db>> {
        let ry_callee = function
            .positional
            .iter()
            .chain(function.named.iter().map(|parameter| &parameter.ty))
            .any(|&parameter| {
                let parameter = self.table.resolve(self.db, parameter);
                is_ry_value_type(self.db, parameter)
            });
        let outer_ry_callee = std::mem::replace(&mut self.ry_callee, ry_callee);
        let findings = self.match_argument_targets(callee_range, function, arguments);
        self.ry_callee = outer_ry_callee;
        findings
    }

    fn match_argument_targets(
        &mut self,
        callee_range: TextRange,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Vec<TypeError<'db>> {
        let variadic_element = function.variadic.as_ref().map(|rest| rest.element);
        let arena = self.arena;
        let targets =
            &*argument_targets(self.db, arena, function, argument_shapes(arena, arguments));
        self.join_shared_parameters(function, arguments, targets);

        // A function-typed argument may be checked against arguments forwarded to it later
        // (`lapply(x, gsub, pattern = "a")`), so the rest intake must be known up front.
        let forwarded_argument_indexes = BumpVec::from_iter_in(
            targets
                .iter()
                .enumerate()
                .filter(|(_, target)| matches!(target, Some(ArgumentTarget::Rest)))
                .map(|(index, _)| index),
            arena,
        );

        let forwards_dots = arguments.iter().any(|argument| argument.forwards_dots);
        let mut findings = Vec::new();
        // An unknown name throws off positional accounting, so its arity verdict would be noise.
        let mut unknown_name = false;
        for (index, argument) in arguments.iter().enumerate() {
            if let Some(element) = variadic_element
                && let Some(ty) = argument.ty
                && argument
                    .value
                    .is_some_and(|value| splice_operand(self.module, value).is_some())
                && let Err(error) = self.check_argument(element, ty, argument.range, false, None)
            {
                findings.push(error);
            }
            let Some(target) = targets.get(index).and_then(Option::as_ref) else {
                continue;
            };
            match target {
                ArgumentTarget::Positional(slot) => {
                    if let Some(&expected) = function.positional.get(*slot)
                        && let Some(ty) =
                            self.argument_type_for(expected, argument, function.inferred)
                        && let Err(error) = self.check_argument(
                            expected,
                            ty,
                            argument.range,
                            argument.whole_double,
                            argument.value,
                        )
                    {
                        findings.push(error);
                    }
                }
                ArgumentTarget::Named(formal) => {
                    // A formal the body only tests with `if` or `while` reads a string through
                    // `as.logical`, so renv's `not(Sys.getenv("CHECK", unset = "TRUE"))` passes
                    // `not <- function(value) if (value) FALSE else TRUE`. `!` and `&&` reject a
                    // string, so a formal they read gets no such leniency. A literal shows its
                    // text, and only R's logical spellings pass.
                    if let Some(parameter) = function.named.get(*formal)
                        && function.condition_only.contains(&parameter.name)
                        && matches!(
                            self.table
                                .shallow_resolve(self.db, parameter.ty)
                                .kind(self.db),
                            TyKind::Scalar(Atomic::Logical)
                        )
                        && argument.ty.is_some_and(|ty| {
                            matches!(
                                self.table.resolve(self.db, ty).kind(self.db),
                                TyKind::Scalar(Atomic::Character)
                            )
                        })
                        && argument
                            .value
                            .is_none_or(|value| self.reads_as_logical_text(value))
                    {
                        continue;
                    }
                    // An undeclared formal whose default is `NULL` receives the same value
                    // whether the caller omits it or passes `NULL`, and omitting it is always
                    // allowed, so Rsolnp's `gosolnp(eqfun = NULL, eqB = NULL)` checks only the
                    // part of the argument that is not `NULL`.
                    let null_default = function.inferred
                        && function.named.get(*formal).is_some_and(|parameter| {
                            parameter.default.is_some_and(|default| {
                                matches!(
                                    self.table.resolve(self.db, default).kind(self.db),
                                    TyKind::Null
                                )
                            })
                        });
                    let stripped;
                    let argument = match argument.ty {
                        Some(ty) if null_default => match self.without_null(ty) {
                            Some(kept) => {
                                stripped = CallArgument {
                                    ty: Some(kept),
                                    ..argument.clone()
                                };
                                &stripped
                            }
                            None => continue,
                        },
                        _ => argument,
                    };
                    if let Some(expected) = function.named.get(*formal).map(|field| field.ty)
                        && let Some(ty) =
                            self.argument_type_for(expected, argument, function.inferred)
                        && let Err(error) = self.check_argument(
                            expected,
                            ty,
                            argument.range,
                            argument.whole_double,
                            argument.value,
                        )
                        && !self.forwarding_callback_probe(
                            expected,
                            ty,
                            &forwarded_argument_indexes,
                            arguments,
                            argument.range,
                        )
                    {
                        findings.push(error);
                    }
                }
                ArgumentTarget::Rest => {
                    if let Some(element) = variadic_element
                        && let Some(ty) =
                            self.argument_type_for(element, argument, function.inferred)
                        && let Err(error) = self.check_argument(
                            element,
                            ty,
                            argument.range,
                            argument.whole_double,
                            argument.value,
                        )
                    {
                        findings.push(error);
                    }
                }
                ArgumentTarget::UndeclaredName => {}
                ArgumentTarget::UnmatchedName => {
                    if let Some(name) = &argument.name {
                        findings.push(TypeError {
                            range: argument.name_range.unwrap_or(argument.range),
                            kind: self.named_argument_mismatch(function, name),
                        });
                        unknown_name = true;
                    }
                }
                ArgumentTarget::Overflow => {
                    if let Some(error) =
                        shape_verdict(self.db, callee_range, function, arguments, targets)
                    {
                        return vec![error];
                    }
                }
            }
        }

        // An omitted formal holds its default's type on this call; left free it would be
        // `Any` and silence every downstream check. Skipped when `...` is forwarded, since
        // the formal may arrive through the dots.
        if !forwards_dots {
            for (formal, parameter) in function.named.iter().enumerate() {
                let supplied = targets.iter().any(
                    |target| matches!(target, Some(ArgumentTarget::Named(filled)) if *filled == formal),
                );
                if supplied {
                    continue;
                }
                // Best effort: a default that does not fit its own parameter is reported at the
                // definition, never at a call.
                if let Some(default) = parameter.default {
                    let _ = self.table.unify(self.db, parameter.ty, default);
                }
            }
        }

        if unknown_name {
            return findings;
        }
        match shape_verdict(self.db, callee_range, function, arguments, targets) {
            Some(error) => vec![error],
            None => findings,
        }
    }

    /// Binds a type variable that several parameters share (`<T> fn(first: T, second: T)`, or
    /// `list[T]` and `Array<T>` items) to the join of the arguments before any is checked. Checking
    /// in order would bind `T` to the first and reject a wider second, so `Bool.is_eq(:Red, light)`
    /// with `light: :Red | :Green` would fail where the reversed call passes. Joining first is
    /// order-independent and sound:
    /// every argument fits it, and a contravariant use (`f: fn(T) -> U`) is still checked against
    /// it. Tags join to their union and numbers to R's arithmetic promotion; any other pair, such
    /// as `integer` and `character`, is a real mistake left to the argument checks.
    fn join_shared_parameters(
        &mut self,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
        targets: &[Option<ArgumentTarget>],
    ) {
        let mut shared: Vec<(InferenceVar, Vec<Ty<'db>>)> = Vec::new();
        for (argument, target) in arguments.iter().zip(targets) {
            let expected = match target {
                Some(ArgumentTarget::Positional(slot)) => function.positional.get(*slot).copied(),
                Some(ArgumentTarget::Named(formal)) => {
                    function.named.get(*formal).map(|field| field.ty)
                }
                _ => None,
            };
            let (Some(expected), Some(found)) = (expected, argument.ty) else {
                continue;
            };
            let Some((var, found)) = self.shared_occurrence(expected, found) else {
                continue;
            };
            match shared.iter_mut().find(|(existing, _)| *existing == var) {
                Some((_, found_types)) => found_types.push(found),
                None => shared.push((var, vec![found])),
            }
        }
        for (var, found_types) in shared {
            if found_types.len() < 2
                || found_types.iter().all(|&found| found == found_types[0])
                || found_types
                    .iter()
                    .any(|&found| contains_inference_var(self.db, found))
            {
                continue;
            }
            let joined = if found_types.iter().all(|&found| is_tag_type(self.db, found)) {
                union_of(self.db, found_types)
            } else {
                match numeric_join(self.db, found_types) {
                    Some(joined) => joined,
                    None => continue,
                }
            };
            let snapshot = self.table.snapshot();
            let variable = Ty::new(self.db, TyKind::Var(var));
            if self.table.unify(self.db, variable, joined).is_err() {
                // A constraint the join breaks: the argument checks report it one by one.
                self.table.rollback(snapshot);
            }
        }
    }

    /// The unbound variable a parameter type is, or holds as its items, paired with the type
    /// the argument supplies in that place.
    fn shared_occurrence(
        &self,
        expected: Ty<'db>,
        found: Ty<'db>,
    ) -> Option<(InferenceVar, Ty<'db>)> {
        let db = self.db;
        let expected = self.table.shallow_resolve(db, expected);
        let found = self.table.resolve(db, found);
        let (var, found) = match expected.kind(db) {
            TyKind::Var(var) => (*var, found),
            TyKind::List(item) => {
                let found = match found.kind(db) {
                    TyKind::List(found_item) => *found_item,
                    TyKind::Tuple(items) if !items.is_empty() => {
                        union_of(db, items.iter().copied())
                    }
                    _ => return None,
                };
                (self.item_variable(*item)?, found)
            }
            TyKind::Named(name, items) if name.text(db) == "List" => {
                let (TyKind::Named(found_name, found_items), [item]) = (found.kind(db), items)
                else {
                    return None;
                };
                let (true, [found_item]) = (found_name == name, found_items) else {
                    return None;
                };
                (self.item_variable(*item)?, *found_item)
            }
            _ => return None,
        };
        let var = self.table.find(var);
        matches!(self.table.entry(var), Entry::Unbound { .. }).then_some((var, found))
    }

    fn item_variable(&self, item: Ty<'db>) -> Option<InferenceVar> {
        match self.table.shallow_resolve(self.db, item).kind(self.db) {
            TyKind::Var(var) => Some(*var),
            _ => None,
        }
    }

    /// A function name given as a string stands for the function only where a function is
    /// expected; anywhere else it is the string it looks like.
    fn argument_type_for(
        &self,
        expected: Ty<'db>,
        argument: &CallArgument<'db>,
        inferred_signature: bool,
    ) -> Option<Ty<'db>> {
        // `tapply`'s `FUN` is a function or `NULL`, and `"sum"` still names the function.
        let expects_function = |ty: Ty<'db>| match ty.kind(self.db) {
            TyKind::Function(_) => true,
            TyKind::Union(members) => members
                .iter()
                .any(|member| matches!(member.kind(self.db), TyKind::Function(_))),
            _ => false,
        };
        if let Some(named) = argument.named_function
            && expects_function(self.table.shallow_resolve(self.db, expected))
        {
            return Some(named);
        }
        let ty = argument.ty?;
        // An overload probe would take the leniency as a fit and pick `sum`'s `difftime`
        // candidate for `sum(w * mu)`.
        if self.overload_probe_depth == 0
            && self.operator_method_depth == 0
            && argument
                .value
                .is_some_and(|value| self.arithmetic_on_open_operand_fits(expected, value))
        {
            return Some(expected);
        }
        if inferred_signature && self.fits_inferred_scalar(expected, ty) {
            return Some(expected);
        }
        let ty = self.joined_list_union(expected, ty);
        match argument.value {
            Some(value) if self.names_computed_value(value) => {
                let ty = self.without_inferred_null(expected, ty);
                Some(self.without_inferred_length(expected, ty, self.binds_parameter(value)))
            }
            _ => Some(ty),
        }
    }

    /// A list that is `list(1)` on one path and `list("a")` on another is a list of either,
    /// so a generic parameter such as `lapply`'s `X: list[T]` sees `list[double | character]`
    /// rather than two lists that bind `T` differently.
    fn joined_list_union(&self, expected: Ty<'db>, ty: Ty<'db>) -> Ty<'db> {
        let resolved = self.table.resolve(self.db, ty);
        let TyKind::Union(members) = resolved.kind(self.db) else {
            return ty;
        };
        // `list()` on one path joins its element as `NULL`, which no path ever yields.
        let Some(elements) = members
            .iter()
            .filter(|&&member| !is_empty_list(self.db, member))
            .map(|&member| self.list_element_type(member))
            .collect::<Option<Vec<Ty<'db>>>>()
        else {
            return ty;
        };
        if elements.len() < 2 || elements.windows(2).all(|pair| pair[0] == pair[1]) {
            return ty;
        }
        let mut generic = FxHashSet::default();
        collect_vars(self.db, self.table.resolve(self.db, expected), &mut generic);
        if generic.is_empty() {
            return ty;
        }
        Ty::new(self.db, TyKind::List(union_of(self.db, elements)))
    }

    /// Arithmetic on a value whose type is still open types as `double`, but it gives a
    /// `matrix` when that value is one (`Arith.matrix`), so `solve.QP(Dmat = 2 * sigma)`
    /// on a parameter `sigma` passes. Any class with arithmetic methods takes such a value.
    fn arithmetic_on_open_operand_fits(&self, expected: Ty<'db>, value: ExprId) -> bool {
        let mut value = value;
        while let ExpressionKind::Paren(inner) = self.module.kind(value) {
            value = *inner;
        }
        let ExpressionKind::Binary {
            operator, lhs, rhs, ..
        } = self.module.kind(value)
        else {
            return false;
        };
        if !ARITHMETIC_OPERATORS.contains(operator) {
            return false;
        }
        let TyKind::Named(class, _) = self.table.shallow_resolve(self.db, expected).kind(self.db)
        else {
            return false;
        };
        let Some(globals) = self.globals else {
            return false;
        };
        globals.arithmetic_classes().contains(class.text(self.db))
            && [*lhs, *rhs].into_iter().any(|operand| {
                self.recorded.get(&operand).is_some_and(|&ty| {
                    matches!(
                        self.table.shallow_resolve(self.db, ty).kind(self.db),
                        TyKind::Var(_)
                    )
                })
            })
    }

    /// A name bound by a declaration rather than by inference: a parameter, whose `NULL` the
    /// caller supplies, or a local whose last assignment is annotated. Its `NULL` is meant.
    fn names_declared_value(&self, value: ExprId) -> bool {
        self.binds_parameter(value)
            || (matches!(
                self.module.expression(value).kind,
                ExpressionKind::NameRef(_)
            ) && self
                .naming
                .resolutions
                .get(&value)
                .is_some_and(|slot| !self.computed_slots.contains(*slot)))
    }

    /// An element read from a collection, directly or through a variable it was stored in.
    fn is_lookup(&self, expression: ExprId) -> bool {
        match self.module.kind(expression) {
            ExpressionKind::Index { .. } | ExpressionKind::Field { .. } => true,
            ExpressionKind::NameRef(_) => self
                .naming
                .resolutions
                .get(&expression)
                .is_some_and(|slot| self.lookup_slots.contains(*slot)),
            _ => false,
        }
    }

    /// A variable the body computed, or a `switch()` call written in place of one: Matrix's
    /// `new(switch(shape, g = "dgeMatrix", t = "dtrMatrix"))` lists an arm for every shape.
    fn names_computed_value(&self, value: ExprId) -> bool {
        match self.module.kind(value) {
            ExpressionKind::NameRef(_) => self
                .naming
                .resolutions
                .get(&value)
                .is_some_and(|slot| self.computed_slots.contains(*slot)),
            ExpressionKind::Call { callee, .. } => matches!(
                self.module.kind(*callee),
                ExpressionKind::NameRef(name) if name.as_str() == "switch"
            ),
            _ => false,
        }
    }

    /// A variable assigned from a `switch` without a default, an `if` without `else` or a helper
    /// that returns `NULL` on some path is presumably not `NULL` where it is passed on (rlang's
    /// `type <- switch(type, ...)` feeds `sprintf(type, kind)` after every type has an arm). So
    /// such an argument drops its `NULL` when the parameter's type is settled or bounded and admits
    /// none, as does an undeclared formal's `NULL` from a `NULL` default or an `is.null()` test. A
    /// declared parameter's or annotated assignment's `NULL` stays.
    fn without_inferred_null(&self, expected: Ty<'db>, ty: Ty<'db>) -> Ty<'db> {
        let expected = self.table.resolve(self.db, expected);
        let admits_null = |ty: Ty<'db>| match ty.kind(self.db) {
            TyKind::Null => true,
            TyKind::Union(members) => members
                .iter()
                .any(|member| matches!(member.kind(self.db), TyKind::Null)),
            _ => false,
        };
        // A type still open to inference could take the `NULL`, and stripping it there would
        // pick a different overload (`lapply` takes `list[T] | T[] | NULL`). No bound admits
        // `NULL`, though, and neither does a callback such as Rsolnp's `fn(Unknown, ...) -> T`.
        let open = match expected.kind(self.db) {
            TyKind::Function(_) => false,
            TyKind::Var(_) => self.table.variable_constraint(self.db, expected).is_none(),
            _ => contains_inference_var(self.db, expected),
        };
        if admits_null(expected) || open {
            return ty;
        }
        let resolved = self.table.resolve(self.db, ty);
        let TyKind::Union(members) = resolved.kind(self.db) else {
            return ty;
        };
        if !members
            .iter()
            .any(|member| matches!(member.kind(self.db), TyKind::Null))
        {
            return ty;
        }
        let kept: Vec<Ty<'db>> = members
            .iter()
            .copied()
            .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
            .collect();
        union_of(self.db, kept)
    }

    /// A scalar parameter type inferred from an unannotated body records how the body uses the
    /// value, and a vector type carries no length, so zoo's `rlength <- function(x) if (all(!x))
    /// ...` is called with the `logical[]` it was written for. A vector of the same atomic type
    /// therefore passes. So does a number where the body only claimed `logical`, because that
    /// claim comes from a condition or `!`, which coerce numbers as R does.
    fn fits_inferred_scalar(&self, expected: Ty<'db>, ty: Ty<'db>) -> bool {
        // Only a scalar the signature names: a type variable an earlier argument bound to a
        // scalar still widens through unification (`cost(TRUE, 0.5)` binds `T := double`).
        let TyKind::Scalar(atomic) = *expected.kind(self.db) else {
            return false;
        };
        let fits = |member: Ty<'db>| {
            let element = match member.kind(self.db) {
                TyKind::Vector(element) => *element,
                _ => member,
            };
            match *element.kind(self.db) {
                TyKind::Scalar(other) => {
                    other == atomic
                        || (atomic == Atomic::Logical
                            && matches!(other, Atomic::Integer | Atomic::Double))
                        || (atomic == Atomic::Integer && other == Atomic::Double)
                }
                _ => false,
            }
        };
        match self.table.resolve(self.db, ty).kind(self.db) {
            TyKind::Union(members) => members.iter().all(|&member| fits(member)),
            _ => fits(self.table.resolve(self.db, ty)),
        }
    }

    /// A variable the body computes as `character | character[]`, through a helper that returns
    /// a vector on some path, is presumably one string where a parameter takes one: renv's
    /// `pattern <- sprintf("^%s=", name)` feeds `sub(pattern, "", spec)`. So such an argument
    /// passes to a scalar parameter of its atomic type as that scalar.
    fn without_inferred_length(
        &self,
        expected: Ty<'db>,
        ty: Ty<'db>,
        undeclared_formal: bool,
    ) -> Ty<'db> {
        let expected = self.table.resolve(self.db, expected);
        let TyKind::Scalar(atomic) = expected.kind(self.db) else {
            return ty;
        };
        let resolved = self.table.resolve(self.db, ty);
        let same_atomic = |member: Ty<'db>| match member.kind(self.db) {
            TyKind::Scalar(other) => other == atomic,
            TyKind::Vector(element) => {
                matches!(element.kind(self.db), TyKind::Scalar(other) if other == atomic)
            }
            _ => false,
        };
        // Only a value that may be one string or many: a plain vector is known to be a vector.
        // An undeclared formal's vector type comes from a use that takes many, such as
        // `tempfile(tmpdir = tmpdir)`, which says nothing about how many the caller passes.
        let fits = match resolved.kind(self.db) {
            TyKind::Vector(_) if undeclared_formal => same_atomic(resolved),
            TyKind::Union(members) => {
                members.iter().all(|&member| same_atomic(member))
                    && members
                        .iter()
                        .any(|member| matches!(member.kind(self.db), TyKind::Scalar(_)))
            }
            _ => false,
        };
        match fits {
            true => expected,
            false => ty,
        }
    }

    fn named_argument_mismatch(
        &self,
        function: &FunctionType<'db>,
        argument: &str,
    ) -> TypeErrorKind<'db> {
        let parameters: Vec<String> = function
            .named
            .iter()
            .map(|field| field.name.text(self.db).to_owned())
            .collect();
        TypeErrorKind::NamedArgumentMismatch {
            duplicate: parameters.iter().any(|parameter| parameter == argument),
            suggestion: nearest_name(argument, parameters.iter().map(String::as_str)),
            argument: argument.to_owned(),
            expected_parameters: parameters,
        }
    }

    /// R's apply family calls `FUN(element, ...)`, so a callback with extra formals is
    /// correct when the caller forwards the difference (`lapply(x, gsub, pattern = "a")`).
    /// Simulates that invocation as a probe whose bindings commit only on success.
    fn forwarding_callback_probe(
        &mut self,
        expected_parameter: Ty<'db>,
        actual_argument: Ty<'db>,
        forwarded_argument_indexes: &[usize],
        arguments: &[CallArgument<'db>],
        callback_range: TextRange,
    ) -> bool {
        let expected = self.table.resolve(self.db, expected_parameter);
        let actual = self.table.resolve(self.db, actual_argument);
        let TyKind::Function(expected_callback) = expected.kind(self.db) else {
            return false;
        };
        // A local holding one of several functions forwards to whichever one it holds, so
        // each takes the call with a return of its own, and their join is the result.
        let members: Vec<&FunctionType<'db>> = match actual.kind(self.db) {
            TyKind::Function(function) => vec![function],
            TyKind::Union(members) => {
                let functions: Vec<&FunctionType<'db>> = members
                    .iter()
                    .filter_map(|member| match member.kind(self.db) {
                        TyKind::Function(function) => Some(*function),
                        _ => None,
                    })
                    .collect();
                if functions.len() != members.len() {
                    return false;
                }
                functions
            }
            _ => return false,
        };
        let snapshot = self.table.snapshot();
        let verdict = if let [function] = members.as_slice() {
            self.forwarded_call_fits(
                expected_callback,
                function,
                expected_callback.ret,
                forwarded_argument_indexes,
                arguments,
                callback_range,
            )
        } else {
            let mut returns = Vec::with_capacity(members.len());
            let mut fits = true;
            for function in members {
                let ret = self.fresh(Constraint::Unconstrained);
                if !self.forwarded_call_fits(
                    expected_callback,
                    function,
                    ret,
                    forwarded_argument_indexes,
                    arguments,
                    callback_range,
                ) {
                    fits = false;
                    break;
                }
                returns.push(self.table.resolve(self.db, ret));
            }
            fits && {
                let joined = union_of(self.db, returns);
                self.table
                    .compatible(self.db, joined, expected_callback.ret)
            }
        };
        if !verdict {
            self.table.rollback(snapshot);
        }
        verdict
    }

    /// Whether `actual_callback`, called as the apply family calls it with the forwarded
    /// arguments, takes `expected_callback`'s elements and returns what fits `ret`.
    fn forwarded_call_fits(
        &mut self,
        expected_callback: &FunctionType<'db>,
        actual_callback: &FunctionType<'db>,
        ret: Ty<'db>,
        forwarded_argument_indexes: &[usize],
        arguments: &[CallArgument<'db>],
        callback_range: TextRange,
    ) -> bool {
        'probe: {
            let mut open_positionals = actual_callback.positional.to_vec();
            let mut open_named = actual_callback.named.to_vec();
            let mut pre_rest_open = match &actual_callback.variadic {
                Some(rest) => rest.preceding_named,
                None => open_named.len(),
            };
            let actual_rest_element = actual_callback.variadic.as_ref().map(|rest| rest.element);

            // Forwarded names consume same-named formals first, as R matches names before
            // positions.
            let mut forwarded_positionals = Vec::new();
            for &index in forwarded_argument_indexes {
                let argument = &arguments[index];
                let Some(name) = &argument.name else {
                    forwarded_positionals.push(index);
                    continue;
                };
                let target = match open_named
                    .iter()
                    .position(|field| field.name.text(self.db) == name.as_str())
                {
                    Some(position) => {
                        if position < pre_rest_open {
                            pre_rest_open -= 1;
                        }
                        open_named.remove(position).ty
                    }
                    None => match actual_rest_element {
                        Some(element) => element,
                        None => break 'probe false,
                    },
                };
                if let Some(ty) = self.argument_type_for(target, argument, actual_callback.inferred)
                    && self
                        .check_argument(target, ty, argument.range, argument.whole_double, None)
                        .is_err()
                {
                    break 'probe false;
                }
            }

            // The interface's parameter types are the elements the callee passes; they fill
            // the remaining formals before the forwarded positionals.
            let elements = expected_callback
                .positional
                .iter()
                .copied()
                .chain(expected_callback.named.iter().map(|field| field.ty))
                .map(|element| (Some(element), callback_range, false, None));
            let forwarded = forwarded_positionals.iter().map(|&index| {
                let argument = &arguments[index];
                (
                    argument.ty,
                    argument.range,
                    argument.whole_double,
                    Some(argument),
                )
            });
            let inputs: Vec<_> = elements.chain(forwarded).collect();
            for (argument_ty, blame_range, whole_double, argument) in inputs {
                let target = if !open_positionals.is_empty() {
                    open_positionals.remove(0)
                } else if pre_rest_open > 0 {
                    pre_rest_open -= 1;
                    open_named.remove(0).ty
                } else if let Some(element) = actual_rest_element {
                    element
                } else {
                    break 'probe false;
                };
                let argument_ty = match argument {
                    Some(argument) => {
                        self.argument_type_for(target, argument, actual_callback.inferred)
                    }
                    // An element takes the leniency a direct call gives an inferred scalar,
                    // so gmp's `lapply(0:(n - 1L), Eulerian, n = n)` fills `k` as
                    // `Eulerian(n = n, 0L)` would.
                    None if actual_callback.inferred
                        && argument_ty.is_some_and(|ty| self.fits_inferred_scalar(target, ty)) =>
                    {
                        continue;
                    }
                    None => argument_ty,
                };
                if let Some(argument_ty) = argument_ty
                    && self
                        .check_argument(target, argument_ty, blame_range, whole_double, None)
                        .is_err()
                {
                    break 'probe false;
                }
            }

            open_positionals.is_empty()
                && open_named.iter().all(|field| field.optional)
                && self.table.compatible(self.db, actual_callback.ret, ret)
        }
    }

    /// Whether `expected` is an open type variable that only a number fits, or a vector of
    /// one.
    fn numeric_parameter(&self, expected: Ty<'db>) -> bool {
        let numeric = |ty: Ty<'db>| {
            matches!(
                self.table.open_constraint(self.db, ty),
                Some(Constraint::Numeric | Constraint::Arithmetic | Constraint::ScalarNumeric)
            )
        };
        match self.table.resolve(self.db, expected).kind(self.db) {
            TyKind::Vector(element) | TyKind::NamedVector(element) => numeric(*element),
            _ => numeric(expected),
        }
    }

    /// Compatibility, not unification, so parameter-position coercions apply. An
    /// `Unknown` argument is accepted: its cause was already diagnosed.
    fn check_argument(
        &mut self,
        expected: Ty<'db>,
        found: Ty<'db>,
        range: TextRange,
        whole_double: bool,
        value: Option<ExprId>,
    ) -> Result<(), TypeError<'db>> {
        if let Some(value) = value {
            self.record_expectation(value, expected);
        }
        let resolved_found = self.table.resolve(self.db, found);
        if matches!(resolved_found.kind(self.db), TyKind::Unknown)
            || value.is_some_and(|value| self.reads_null_placeholder(value, resolved_found))
        {
            return Ok(());
        }
        // R's arithmetic reads `TRUE` as `1L`, so a `numeric` type parameter, which the callee
        // may do arithmetic on, takes a logical argument as `integer`: `twice(TRUE)` under
        // `<N: numeric> fn(x: N) -> N` returns the integer `2`, never a logical.
        if self.numeric_parameter(expected)
            && let Some(promoted) = promote_logical(self.db, resolved_found)
            && self.table.compatible(self.db, promoted, expected)
        {
            return Ok(());
        }
        // A literal that does not fit reported itself.
        if let Some(value) = value
            && self.adapt_literal(value, expected) != LiteralFit::NotLiteral
        {
            return Ok(());
        }
        if self.table.compatible(self.db, resolved_found, expected) {
            return Ok(());
        }
        // R code writes `seq_len(10)`, not `10L`: a whole-number double literal counts as
        // integer at a parameter, except during a strict overload probe.
        if self.overload_probe_depth == 0
            && matches!(resolved_found.kind(self.db), TyKind::Scalar(Atomic::Double))
            && whole_double
            && self
                .table
                .compatible(self.db, scalar(self.db, Atomic::Integer), expected)
        {
            return Ok(());
        }
        // Report the numeric constraint rather than render a bare inference variable as expected.
        let resolved_expected = self.table.resolve(self.db, expected);
        // A function parameter of base R goes through `match.fun`, which also takes the
        // function's name, so `lapply(xs, "[[", "id")` and `apply(m, 1, "mean")` are
        // idiomatic. The name is not resolved, so the call is accepted unchecked. A function
        // that takes a ry value is ry's own and calls its callback directly.
        if matches!(resolved_expected.kind(self.db), TyKind::Function(_))
            && is_character(self.db, resolved_found)
        {
            if !self.ry_callee {
                return Ok(());
            }
            let name = value.and_then(|value| match self.module.kind(value) {
                ExpressionKind::Literal(LiteralKind::String(text)) => {
                    Some(self.module[text].to_owned())
                }
                _ => None,
            });
            return Err(TypeError {
                range,
                kind: TypeErrorKind::FunctionName {
                    expected: resolved_expected,
                    name,
                },
            });
        }
        if let TyKind::Var(var) = resolved_expected.kind(self.db)
            && let Entry::Unbound {
                constraint,
                bound: None,
                ..
            } = *self.table.entry(*var)
            && constraint != Constraint::Unconstrained
        {
            return Err(TypeError {
                range,
                kind: TypeErrorKind::ConstraintViolation {
                    constraint,
                    found: resolved_found,
                },
            });
        }
        if let (TyKind::Function(found_function), TyKind::Function(expected_function)) = (
            resolved_found.kind(self.db),
            resolved_expected.kind(self.db),
        ) && let Some(mismatch) =
            self.table
                .explain_function_mismatch(self.db, found_function, expected_function)
        {
            return Err(TypeError {
                range,
                kind: TypeErrorKind::CallbackShape {
                    mismatch: Box::new(mismatch),
                },
            });
        }
        Err(self.type_mismatch(range, resolved_expected, resolved_found, value))
    }

    /// Binds type-parameter arguments through compatibility, which is how a generic
    /// nominal infers them from inference-variable fields.
    fn check_new_nominal(
        &mut self,
        name: Name<'db>,
        given_arguments: &[Ty<'db>],
        value: ExprId,
        value_ty: Ty<'db>,
    ) -> TypeScheme<'db> {
        let range = self.blame_range(value);
        let Some(definition) = self
            .table
            .definitions
            .and_then(|table| table.get(self.db, name))
            .cloned()
        else {
            return self.generalize(value_ty);
        };
        if definition.alias {
            return self.generalize(value_ty);
        }
        if let Some(opaque) = definition.opaque.filter(|opaque| opaque.hidden) {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::OpaqueOutsideHome {
                    name,
                    opaque,
                    creating: true,
                },
            });
            let arguments: Vec<Ty<'db>> = definition
                .parameters
                .iter()
                .map(|_| unknown(self.db))
                .collect();
            return self.generalize(Ty::new(self.db, TyKind::Named(name, &arguments)));
        }
        // An unapplied generic infers its arguments through the representation check;
        // argument variables live one level up so leftovers generalize into the scheme.
        self.table.level += 1;
        let arguments: Vec<Ty<'db>> = if given_arguments.len() == definition.parameters.len() {
            given_arguments.to_vec()
        } else {
            definition
                .parameters
                .iter()
                .map(|_| self.fresh(Constraint::Unconstrained))
                .collect()
        };
        let nominal = Ty::new(self.db, TyKind::Named(name, &arguments.clone()));
        if !matches!(definition.body.kind(self.db), TyKind::Unknown) {
            let representation = apply_definition(self.db, &definition, &arguments);
            self.record_expectation(value, representation);
            let resolved_value = self.table.resolve(self.db, value_ty);
            if !self
                .table
                .compatible(self.db, resolved_value, representation)
            {
                let error = self.type_mismatch(range, representation, resolved_value, Some(value));
                self.errors.push(error);
            }
        }
        self.table.level -= 1;
        self.generalize(nominal)
    }

    /// Reports a read or write of an `opaque` type's representation outside its home file, where
    /// the type has none, and says whether it did. A union reports its first such member.
    fn refuse_hidden_representation(&mut self, subject: ExprId, ty: Ty<'db>) -> bool {
        let resolved = self.table.resolve(self.db, ty);
        let members = match resolved.kind(self.db) {
            TyKind::Union(members) => &members[..],
            _ => std::slice::from_ref(&resolved),
        };
        let Some(definitions) = self.table.definitions else {
            return false;
        };
        let Some((name, opaque)) = members.iter().find_map(|member| {
            let TyKind::Named(name, _) = member.kind(self.db) else {
                return None;
            };
            let opaque = definitions.get(self.db, *name)?.opaque?;
            opaque.hidden.then_some((*name, opaque))
        }) else {
            return false;
        };
        self.errors.push(TypeError {
            range: self.blame_range(subject),
            kind: TypeErrorKind::OpaqueOutsideHome {
                name,
                opaque,
                creating: false,
            },
        });
        true
    }

    /// An R frame as its class, for the rules that dispatch on a class (`Arith.data.frame`,
    /// `[.data.frame`); any other type as it is.
    fn frame_as_class(&self, ty: Ty<'db>) -> Ty<'db> {
        match *self.table.shallow_resolve(self.db, ty).kind(self.db) {
            TyKind::Frame(kind, _) if let Some(class) = kind.class() => {
                Ty::new(self.db, TyKind::Named(Name::new(self.db, class), &[]))
            }
            _ => ty,
        }
    }

    /// Projects non-alias nominals to their representation. An undeclared nominal (a
    /// typo reported elsewhere) floors to `Unknown` so operator checks never cascade.
    fn structural(&mut self, ty: Ty<'db>) -> Ty<'db> {
        self.structural_outside(ty, &mut Vec::new())
    }

    /// `expanding` holds the nominal types whose representation is being projected, so a
    /// recursive union such as `type L = list{a: double} | L` keeps its inner `L` as written.
    fn structural_outside(&mut self, ty: Ty<'db>, expanding: &mut Vec<Ty<'db>>) -> Ty<'db> {
        let mut current = self.table.resolve(self.db, ty);
        for _ in 0..16 {
            let TyKind::Named(name, arguments) = current.kind(self.db) else {
                break;
            };
            if self.table.undeclared_nominal(self.db, current) {
                return Ty::new(self.db, TyKind::Unknown);
            }
            match self.table.representation(self.db, *name, arguments) {
                Some(representation) => {
                    expanding.push(current);
                    current = self.table.resolve(self.db, representation);
                }
                None => break,
            }
        }
        // A union of nominal types projects member by member, so `$` on `Circle | Square`, or
        // on a `Shape` defined as that union, is checked against both representations as it is
        // on the structural union.
        if let TyKind::Union(members) = current.kind(self.db)
            && members
                .iter()
                .any(|member| matches!(member.kind(self.db), TyKind::Named(..)))
        {
            let mark = expanding.len();
            let projected: Vec<Ty<'db>> = members
                .iter()
                .map(|&member| {
                    if expanding.contains(&member) {
                        return member;
                    }
                    let projected = self.structural_outside(member, expanding);
                    expanding.truncate(mark);
                    projected
                })
                .collect();
            return union_of(self.db, projected);
        }
        current
    }

    fn infer_index(
        &mut self,
        id: ExprId,
        range: TextRange,
        double: bool,
        target: ExprId,
        arguments: &[Argument],
    ) -> Ty<'db> {
        let target_ty = self.infer(target);
        if self.refuse_hidden_representation(target, target_ty) {
            return unknown(self.db);
        }
        let subject = self.structural(target_ty);
        let is_query = !double
            && matches!(
                subject.kind(self.db),
                TyKind::Frame(FrameKind::DataFrame | FrameKind::DataTable, _)
            );
        let result = if is_query {
            self.query_depth += 1;
            let result = self.infer_frame_query(id, target, subject, arguments);
            self.query_depth -= 1;
            result
        } else {
            self.infer_plain_index(id, range, double, target, target_ty, subject, arguments)
        };
        // Any other subject (a data.table, an unknown) keeps its type through `:=`, and the
        // write still has to land so the statement's binding carries that type forward.
        if let Some(&slot) = self.naming.extras().column_updates.get(&id) {
            let written = if is_query { result } else { target_ty };
            self.environment.set(slot, EnvEntry::Mono(written));
            self.note_captured_write(id, slot, written);
        } else if is_query
            && is_column_update(self.module, arguments)
            && let Some(name) = self.naming.non_locals.get(&target)
        {
            // A frame bound elsewhere binds nothing here, but the update still changes the
            // frame the name refers to, so later reads in this flow see the new columns.
            let slot = self.non_local_slot(*name);
            self.environment.set(slot, EnvEntry::Mono(result));
        }
        result
    }

    /// `m[i, j]` on a `matrix`, whose element type is not tracked, so elements are `Any`. R
    /// drops a dimension indexed by one position, so a scalar index gives elements, while empty
    /// and vector indexes, or `drop = FALSE`, keep the matrix. `None` leaves other forms to the
    /// general rules.
    fn matrix_index(
        &mut self,
        double: bool,
        arguments: &[Argument],
        index_types: &[Ty<'db>],
    ) -> Option<Ty<'db>> {
        let mut values = index_types.iter();
        let mut slots = 0;
        let mut any_scalar = false;
        let mut all_vectors = true;
        let mut keep_dimensions = false;
        for argument in arguments {
            let ty = argument.value.and_then(|_| values.next().copied());
            if let Some(name) = argument.name {
                if self.module.name(name).as_str() != "drop" {
                    return None;
                }
                let value = argument.value?;
                keep_dimensions = matches!(
                    self.module.expression(value).kind,
                    ExpressionKind::Literal(LiteralKind::Logical(false))
                );
                if !keep_dimensions {
                    all_vectors = false;
                }
                continue;
            }
            slots += 1;
            let Some(ty) = ty else {
                continue;
            };
            match self.table.resolve(self.db, ty).kind(self.db) {
                TyKind::Scalar(_) => any_scalar = true,
                TyKind::Vector(_) | TyKind::NamedVector(_) => {}
                TyKind::Named(name, _) if name.text(self.db) == "matrix" => {}
                _ => all_vectors = false,
            }
        }
        if double || slots != 2 {
            return (slots >= 1).then(|| any(self.db));
        }
        let matrix = Ty::new(self.db, TyKind::Named(Name::new(self.db, "matrix"), &[]));
        if keep_dimensions {
            return Some(matrix);
        }
        Some(if !any_scalar && all_vectors {
            matrix
        } else {
            any(self.db)
        })
    }

    /// The return type a stub declares for `[.class` or `[[.class`. The index arguments are
    /// not checked against it: the stubs declare these methods only to say which class the
    /// subset keeps.
    fn index_method_result(&mut self, class: &str, double: bool) -> Option<Ty<'db>> {
        self.subset_method_result(if double { "[[" } else { "[" }, class)
    }

    fn subset_method_result(&mut self, operator: &str, class: &str) -> Option<Ty<'db>> {
        let method = method_symbol(operator, class);
        let scheme = self.globals?.scheme(method, false)?;
        let instantiated = self.instantiate(&scheme);
        match self
            .table
            .shallow_resolve(self.db, instantiated)
            .kind(self.db)
        {
            TyKind::Function(function) => Some(function.ret),
            _ => None,
        }
    }

    /// Every index infers first regardless of shape, so names inside an unsupported
    /// form (`m[i, j]`) still get diagnostics.
    #[allow(clippy::too_many_arguments)]
    fn infer_plain_index(
        &mut self,
        id: ExprId,
        range: TextRange,
        double: bool,
        target: ExprId,
        target_ty: Ty<'db>,
        subject: Ty<'db>,
        arguments: &[Argument],
    ) -> Ty<'db> {
        // A subject that may be a data.table of unknown columns (a declared one, data.table
        // syntax, or a value of no known shape) evaluates its indexes in a frame ry cannot
        // see, where a `NULL` placeholder local names a column.
        // `f[i, j]` on a `data.frame{...}` is base R's `[.data.frame`, which may return a row
        // subset, a column or a value. `f[["x"]]` reads a column, as `f$x` does.
        let (subject, target_ty) = if double {
            (subject, target_ty)
        } else {
            (self.frame_as_class(subject), self.frame_as_class(target_ty))
        };
        // A class that extends `data.table` queries as one.
        let data_table = !double
            && self
                .class_chain(target_ty)
                .iter()
                .any(|name| name.text(self.db) == "data.table");
        let unseen_frame = !double
            && (data_table
                || self.naming.extras().masked_subsets.contains(&id)
                || matches!(
                    self.table.resolve(self.db, subject).kind(self.db),
                    TyKind::Unknown | TyKind::Any | TyKind::Var(_)
                ));
        if unseen_frame {
            self.unseen_frames += 1;
        }
        let mut index_types = Vec::with_capacity(arguments.len());
        for argument in arguments {
            if let Some(value) = argument.value {
                index_types.push(self.infer(value));
            }
        }
        if unseen_frame {
            self.unseen_frames -= 1;
        }
        // A declared `data.table` subject makes `[` a query: index arguments evaluate in
        // the data's frame (masked column reads), and the `j` syntax decides the result class.
        if data_table {
            for argument in arguments {
                if let Some(value) = argument.value {
                    self.mask_column_reads(value);
                }
            }
            return if data_table_keeps_class(self.module, arguments) {
                self.table.resolve(self.db, target_ty)
            } else {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            };
        }
        // data.table syntax without a data.table-typed subject returns a shape no base
        // rule covers: silent `Unknown`.
        if self.naming.extras().masked_subsets.contains(&id) {
            return unknown(self.db);
        }
        // R dispatches `[` and `[[` on the class vector before any shape rule applies, so a
        // stub's `[.factor` or `[[.Date` gives the result type, and an `xts` subsets with
        // `[.xts` before the `[.zoo` of the class it extends.
        let chain = self.class_chain(target_ty);
        let subject_chain = self.class_chain(subject);
        for name in chain.iter().chain(subject_chain.iter()) {
            if name.text(self.db) == "matrix"
                && let Some(ty) = self.matrix_index(double, arguments, &index_types)
            {
                return ty;
            }
            if let Some(ty) = self.index_method_result(name.text(self.db), double) {
                return ty;
            }
        }
        if let Some(element) = array_element(self.db, subject) {
            return self.array_index(range, double, subject, element, arguments, &index_types);
        }
        // An `Unknown`/`Any` subject's gap was already diagnosed, so `m[i, j]` must not
        // cascade an arity error.
        match subject.kind(self.db) {
            TyKind::Unknown => return unknown(self.db),
            TyKind::Any => return any(self.db),
            // A shape the author never wrote down (a sealed nominal, or `m` in
            // `function(m, i, j) m[i, j]`) refuses before the index-arity check: the caller
            // knows the shape, the callee does not.
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                return unknown(self.db);
            }
            _ => {}
        }
        // Empty slots do not count (`m[, i]` has one index) but named arguments do; a single
        // index among several slots is unmodeled matrix selection, refused silently.
        let filled = arguments
            .iter()
            .filter(|argument| argument.value.is_some())
            .count();
        // `x[]` is `x` itself. Several indexes (matrix and data.frame selection) and named
        // arguments such as `drop = FALSE` are unmodeled shapes, refused with `Unknown` rather
        // than reported, because they are ordinary R.
        if filled == 0 && arguments.len() <= 1 && !double {
            return subject;
        }
        if filled != 1 || arguments.len() != 1 || arguments[0].name.is_some() {
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        let result = if double {
            let module = self.module;
            let index = arguments[0].value.map(|value| module.kind(value));
            let key_range = arguments[0]
                .value
                .map_or(range, |value| self.blame_range(value));
            self.extract_result(id, range, key_range, subject, index)
                .map_err(|error| name_frame(error, self.module, target))
        } else {
            self.subset_result(id, range, subject, index_types.first().copied())
        };
        match result {
            Ok(ty) => ty,
            Err(error) => {
                self.errors.push(error);
                unknown(self.db)
            }
        }
    }

    /// An Array has one dimension and no names, so it takes exactly one positional index:
    /// `x[[i]]` reads one item by position, and `x[i]` selects an Array of items by positions or
    /// by a logical mask. `x[]` is `x` itself.
    fn array_index(
        &mut self,
        range: TextRange,
        double: bool,
        subject: Ty<'db>,
        element: Ty<'db>,
        arguments: &[Argument],
        index_types: &[Ty<'db>],
    ) -> Ty<'db> {
        let result = if double { element } else { subject };
        if !double
            && arguments.iter().all(|argument| argument.value.is_none())
            && arguments.len() <= 1
        {
            return subject;
        }
        let index = match (arguments, index_types) {
            ([argument], [index]) if argument.name.is_none() => Some(self.structural(*index)),
            _ => None,
        };
        // An R number stays a valid position, so a literal position (`xs[[1]]`) needs no
        // conversion.
        let position = |db, ty: Ty<'db>, logical: bool| match ty.kind(db) {
            TyKind::Scalar(Atomic::Integer | Atomic::Double) => true,
            TyKind::Native(NativeScalar::Bool) | TyKind::Scalar(Atomic::Logical) => logical,
            TyKind::Native(native) => native.is_integer(),
            TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => true,
            _ => false,
        };
        let fits = index.is_some_and(|index| {
            let index = self.table.resolve(self.db, index);
            match index.kind(self.db) {
                _ if position(self.db, index, false) => true,
                TyKind::Vector(item) if !double => position(self.db, *item, true),
                _ if !double => array_element(self.db, index).is_some_and(|item| {
                    let item = self.table.resolve(self.db, item);
                    position(self.db, item, true)
                }),
                _ => false,
            }
        });
        if !fits {
            let range = match arguments {
                [
                    Argument {
                        value: Some(value),
                        name: None,
                        ..
                    },
                ] => self.blame_range(*value),
                _ => range,
            };
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::ArrayIndex {
                    double,
                    found: index,
                },
            });
        }
        result
    }

    /// `f[i, j, by]` on a frame: data.table's query. `i` filters or orders rows (or joins
    /// with `on =`), `by` groups them, and `j` computes the result, all evaluated with the
    /// frame's columns in scope.
    fn infer_frame_query(
        &mut self,
        id: ExprId,
        target: ExprId,
        subject: Ty<'db>,
        arguments: &[Argument],
    ) -> Ty<'db> {
        let module = self.module;
        let db = self.db;
        let kind = match subject.kind(db) {
            TyKind::Frame(kind, _) => *kind,
            _ => FrameKind::DataFrame,
        };
        let named = |formal: &str| {
            arguments
                .iter()
                .find(|argument| self.module.argument_name(argument).as_deref() == Some(formal))
        };
        let (i, j) = query_index_arguments(self.module, arguments);
        let by = named("by").or_else(|| named("keyby"));
        let on = named("on");
        let frame = variable_name(module, target);
        let bracket = module.range(id);
        let subject_columns = frame_columns(db, subject);
        let mut callees = SmallVec::new();
        for value in arguments.iter().filter_map(|argument| argument.value) {
            collect_callees(module, value, &mut callees);
        }
        callees.sort_unstable();
        let i_value = i.and_then(|argument| argument.value);
        let mut source_columns = subject_columns.map(<[_]>::to_vec);
        let mut join = None;
        let mut layout = None;
        let each_i = by.and_then(|argument| argument.value).is_some_and(
            |value| matches!(module.kind(value), ExpressionKind::NameRef(name) if name == ".EACHI"),
        );
        if let Some(on_value) = on.and_then(|argument| argument.value) {
            // The joined frame evaluates in the caller's scope, not the queried frame's.
            let joined = i_value.map(|value| {
                let ty = self.infer(value);
                self.structural(ty)
            });
            source_columns = match joined.map(|joined| joined.kind(db)) {
                Some(TyKind::Frame(
                    FrameKind::DataFrame | FrameKind::DataTable,
                    joined_columns,
                )) => {
                    // Under `.EACHI`, `i.col` reads one row of `i`, like a bare column of `i`.
                    join = Some(JoinSides {
                        x: subject_columns.map(<[_]>::to_vec),
                        i: joined_columns.as_ref().map(|columns| {
                            columns
                                .iter()
                                .map(|column| RecordField {
                                    ty: if each_i {
                                        column_element(db, column.ty)
                                    } else {
                                        column.ty
                                    },
                                    ..*column
                                })
                                .collect()
                        }),
                    });
                    let joined_name = i_value.and_then(|value| variable_name(module, value));
                    match join_keys(module, on_value) {
                        Some(keys) => {
                            if let Some(roll) = named("roll")
                                && let Some(roll_value) = roll.value
                                && keys.iter().any(|key| key.operator != JoinOperator::Equal)
                                // data.table rolls unless `roll` equals `FALSE`, which `0` does.
                                && !matches!(
                                    module.kind(roll_value),
                                    ExpressionKind::Literal(LiteralKind::Logical(false))
                                ) && !matches!(
                                    module.kind(roll_value),
                                    ExpressionKind::Literal(LiteralKind::Integer(text) | LiteralKind::Double(text))
                                        if module[text].trim_end_matches('L').parse::<f64>() == Ok(0.0)
                                ) && !(matches!(
                                    module.kind(roll_value),
                                    ExpressionKind::NameRef(name) if name == "F"
                                ) && !self.naming.resolutions.contains_key(&roll_value))
                            {
                                self.errors.push(TypeError {
                                    range: module.argument_name_range(roll).map_or_else(
                                        || module.range(roll_value),
                                        |name| name.cover(module.range(roll_value)),
                                    ),
                                    kind: TypeErrorKind::RollingNonEquiJoin,
                                });
                            }
                            layout = self.join_columns(
                                &keys,
                                (frame, subject_columns),
                                (joined_name, joined_columns.as_deref()),
                            );
                            layout
                                .as_ref()
                                .map(|layout| layout.iter().map(|column| column.field).collect())
                        }
                        None => {
                            self.infer(on_value);
                            None
                        }
                    }
                }
                // A key vector or list joins on a shape ry does not model.
                _ => {
                    self.column_scopes.push(ColumnScope {
                        columns: None,
                        frame,
                        bracket,
                        subset_data: Ty::new(db, TyKind::Frame(kind, None)),
                        callees,
                        join: None,
                    });
                    for value in arguments.iter().filter_map(|argument| argument.value) {
                        if Some(value) != i_value && value != on_value {
                            self.infer(value);
                        }
                    }
                    self.column_scopes.pop();
                    return unknown(db);
                }
            };
        }
        // Under `by = .EACHI`, `j` runs once per row of `i`, so every column carrying `i`'s
        // values holds that row's one value.
        let scope_columns = match (&layout, each_i) {
            (Some(layout), true) => Some(
                layout
                    .iter()
                    .map(|column| RecordField {
                        ty: if column.from_i {
                            column_element(db, column.field.ty)
                        } else {
                            column.field.ty
                        },
                        ..column.field
                    })
                    .collect(),
            ),
            (None, true) => None,
            (_, false) => source_columns.clone(),
        };
        self.column_scopes.push(ColumnScope {
            columns: scope_columns,
            frame,
            bracket,
            subset_data: Ty::new(db, TyKind::Frame(kind, None)),
            callees,
            join,
        });
        if on.is_none()
            && let Some(value) = i_value
        {
            let ty = self.infer(value);
            let resolved = self.table.resolve(db, ty);
            if !is_row_index(db, resolved) {
                self.errors.push(TypeError {
                    range: self.blame_range(value),
                    kind: TypeErrorKind::BadRowIndex { found: resolved },
                });
            }
        }
        let groups = match by.and_then(|argument| argument.value) {
            // The groups are the compared columns, in the order of their conditions.
            Some(value) if each_i => {
                self.infer(value);
                layout.as_ref().map(|layout| {
                    let mut compared: Vec<&JoinedColumn<'db>> = layout
                        .iter()
                        .filter(|column| column.condition.is_some())
                        .collect();
                    compared.sort_by_key(|column| column.condition);
                    compared.into_iter().map(|column| column.field).collect()
                })
            }
            Some(value) => self.query_groups(value),
            None => Some(Vec::new()),
        };
        // `.SDcols` names `.SD`'s columns exactly, so a grouping column it names stays in.
        let subset_columns = match (
            named(".SDcols").and_then(|argument| argument.value),
            &source_columns,
        ) {
            (Some(selection), Some(columns)) => self.subset_columns(columns, selection),
            (Some(selection), None) => {
                self.infer(selection);
                None
            }
            (None, Some(columns)) => groups.as_ref().map(|groups| {
                columns
                    .iter()
                    .filter(|column| !groups.iter().any(|group| group.name == column.name))
                    .cloned()
                    .collect()
            }),
            (None, None) => None,
        };
        let subset_data = Ty::new(db, TyKind::Frame(kind, subset_columns.as_deref()));
        if let Some(scope) = self.column_scopes.last_mut() {
            scope.subset_data = subset_data;
        }
        let j_value = j.and_then(|argument| argument.value);
        let update = j_value.and_then(|value| match module.kind(value) {
            ExpressionKind::Call { callee, arguments }
                if variable_name(module, *callee).as_deref() == Some(":=") =>
            {
                Some(arguments)
            }
            _ => None,
        });
        let result = match j_value {
            Some(_) if let Some(update) = update => Ty::new(
                db,
                TyKind::Frame(
                    kind,
                    self.column_update(
                        &module[update],
                        subject_columns,
                        i_value.is_some() || by.is_some(),
                    )
                    .as_deref(),
                ),
            ),
            // `with = FALSE` and a `..name` `j` read `j` as column names or positions, which ry
            // does not model.
            Some(value)
                if named("with").is_some()
                    || matches!(
                        module.kind(value),
                        ExpressionKind::NameRef(name) if dot_dot_variable(name).is_some()
                    ) =>
            {
                self.infer(value);
                Ty::new(db, TyKind::Frame(kind, None))
            }
            Some(value) => self.query_result(
                id,
                kind,
                value,
                source_columns.as_deref(),
                groups,
                by.is_some(),
            ),
            None => Ty::new(db, TyKind::Frame(kind, source_columns.as_deref())),
        };
        for argument in arguments {
            let handled = [i, j, by, on, named(".SDcols")]
                .into_iter()
                .flatten()
                .any(|handled| std::ptr::eq(handled, argument));
            if !handled && let Some(value) = argument.value {
                self.infer(value);
            }
        }
        self.column_scopes.pop();
        result
    }

    /// The joined source in data.table's layout: the queried frame's columns, each compared
    /// column typed by the joined column of its first condition, whose values it takes; then a
    /// column `t.1`, `t.2`, ... for each further condition on the same column `t`; then the
    /// joined frame's columns no condition uses, `i.`-prefixed where the queried frame has the
    /// name. A name that still repeats takes the first free numeric suffix, as `make.unique`
    /// gives it. Each condition's two columns are checked for a type the join can compare.
    fn join_columns(
        &mut self,
        keys: &[JoinKey],
        (x_name, x_columns): (Option<Symbol>, Option<&[RecordField<'db>]>),
        (y_name, y_columns): (Option<Symbol>, Option<&[RecordField<'db>]>),
    ) -> Option<Vec<JoinedColumn<'db>>> {
        let db = self.db;
        let mut complete = true;
        for key in keys {
            for (column, range, frame, columns) in [
                (&key.x, key.x_range, &x_name, x_columns),
                (&key.y, key.y_range, &y_name, y_columns),
            ] {
                let Some(columns) = columns else {
                    continue;
                };
                if !columns
                    .iter()
                    .any(|candidate| candidate.name.text(db) == column)
                {
                    complete = false;
                    self.errors.push(TypeError {
                        range,
                        kind: column_does_not_exist(db, column, *frame, columns),
                    });
                }
            }
        }
        let (Some(x_columns), Some(y_columns), true) = (x_columns, y_columns, complete) else {
            return None;
        };
        let column_of = |columns: &[RecordField<'db>], name: &str| {
            columns
                .iter()
                .find(|column| column.name.text(db) == name)
                .map(|column| column.ty)
        };
        for key in keys {
            let (Some(x_ty), Some(y_ty)) =
                (column_of(x_columns, &key.x), column_of(y_columns, &key.y))
            else {
                continue;
            };
            let (x_class, y_class) = (join_class(db, x_ty), join_class(db, y_ty));
            let text_side = [
                (x_class, &key.x, key.x_range, x_ty),
                (y_class, &key.y, key.y_range, y_ty),
            ]
            .into_iter()
            .find(|(class, ..)| *class == Some(JoinClass::Text));
            if key.operator != JoinOperator::Equal
                && let Some((_, column, range, found)) = text_side
            {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::OrderedJoinOnText {
                        condition: key.text(),
                        column: column.clone(),
                        found,
                    },
                });
            } else if let (Some(x_class), Some(y_class)) = (x_class, y_class)
                && x_class != y_class
            {
                self.errors.push(TypeError {
                    range: key.x_range.cover(key.y_range),
                    kind: TypeErrorKind::IncompatibleJoinColumns {
                        condition: key.text(),
                        x: x_ty,
                        i: y_ty,
                    },
                });
            }
        }
        let mut layout: Vec<JoinedColumn<'db>> =
            Vec::with_capacity(x_columns.len() + y_columns.len());
        for column in x_columns {
            let condition = keys.iter().position(|key| key.x == column.name.text(db));
            let joined = condition
                .and_then(|index| keys.get(index))
                .and_then(|key| column_of(y_columns, &key.y));
            layout.push(JoinedColumn {
                field: RecordField {
                    ty: joined.unwrap_or(column.ty),
                    ..*column
                },
                from_i: condition.is_some(),
                condition,
            });
        }
        for (index, key) in keys.iter().enumerate() {
            let earlier = keys
                .iter()
                .take(index)
                .filter(|earlier| earlier.x == key.x)
                .count();
            if earlier > 0
                && let Some(ty) = column_of(y_columns, &key.y)
            {
                layout.push(JoinedColumn {
                    field: RecordField {
                        name: Name::new(db, format!("{}.{earlier}", key.x)),
                        ty,
                        optional: false,
                    },
                    from_i: true,
                    condition: Some(index),
                });
            }
        }
        for column in y_columns {
            let name = column.name.text(db);
            if keys.iter().any(|key| key.y == name) {
                continue;
            }
            let name = if column_of(x_columns, name).is_some() {
                Name::new(db, format!("i.{name}"))
            } else {
                column.name
            };
            layout.push(JoinedColumn {
                field: RecordField { name, ..*column },
                from_i: true,
                condition: None,
            });
        }
        let mut taken: FxHashSet<String> = layout
            .iter()
            .map(|column| column.field.name.text(db).to_owned())
            .collect();
        let mut seen = FxHashSet::default();
        for column in &mut layout {
            let name = column.field.name.text(db).to_owned();
            if seen.insert(name.clone()) {
                continue;
            }
            if let Some(unique) = (1..)
                .map(|suffix| format!("{name}.{suffix}"))
                .find(|candidate| !taken.contains(candidate))
            {
                taken.insert(unique.clone());
                seen.insert(unique.clone());
                column.field.name = Name::new(db, unique);
            }
        }
        Some(layout)
    }

    /// The group columns `by` names, in order. `None` when they are not all known: a
    /// computed group without a name, `.EACHI`, or a form ry does not model.
    fn query_groups(&mut self, value: ExprId) -> Option<Vec<RecordField<'db>>> {
        let module = self.module;
        match module.kind(value) {
            ExpressionKind::NameRef(name) => {
                let ty = self.infer(value);
                (name != ".EACHI")
                    .then(|| self.column_field(name, ty))
                    .map(|field| vec![field])
            }
            ExpressionKind::Literal(LiteralKind::String(text)) => {
                self.literal_groups(&[(&module[text], module.range(value))])
            }
            ExpressionKind::Call { callee, arguments } => {
                match variable_name(module, *callee).as_deref() {
                    Some("c") if string_literals(module, &module[arguments]).is_some() => {
                        let literals: Vec<(&str, TextRange)> = module[arguments]
                            .iter()
                            .filter_map(|argument| argument.value)
                            .filter_map(|item| match module.kind(item) {
                                ExpressionKind::Literal(LiteralKind::String(text)) => {
                                    Some((&module[text], module.range(item)))
                                }
                                _ => None,
                            })
                            .collect();
                        self.literal_groups(&literals)
                    }
                    Some("." | "list") => {
                        let mut groups = Some(Vec::with_capacity(arguments.len()));
                        for argument in &module[arguments] {
                            let Some(item) = argument.value else {
                                continue;
                            };
                            let ty = self.infer(item);
                            let name = module
                                .argument_name(argument)
                                .or_else(|| variable_name(module, item));
                            match name {
                                Some(name) => {
                                    let field = self.column_field(&name, ty);
                                    if let Some(groups) = groups.as_mut() {
                                        groups.push(field);
                                    }
                                }
                                None => groups = None,
                            }
                        }
                        groups
                    }
                    _ => {
                        self.infer(value);
                        None
                    }
                }
            }
            _ => {
                self.infer(value);
                None
            }
        }
    }

    /// `by = "g,h"` and `by = c("g", "h")` name columns of the query's source directly.
    fn literal_groups(&mut self, literals: &[(&str, TextRange)]) -> Option<Vec<RecordField<'db>>> {
        let db = self.db;
        let scope = self.column_scopes.last()?;
        let columns = scope.columns.clone()?;
        let frame = scope.frame;
        let mut groups = Some(Vec::new());
        for &(text, range) in literals {
            for name in text.split(',').map(str::trim) {
                match columns.iter().find(|column| column.name.text(db) == name) {
                    Some(column) => {
                        if let Some(groups) = groups.as_mut() {
                            groups.push(*column);
                        }
                    }
                    None => {
                        groups = None;
                        self.errors.push(TypeError {
                            range,
                            kind: column_does_not_exist(db, name, frame, &columns),
                        });
                    }
                }
            }
        }
        groups
    }

    /// The columns `.SDcols` selects from the query's source, in the source's order:
    /// `c("a", "b")`, a range `a:b`, a type predicate such as `is.numeric`, or any of them
    /// negated with `!` or `-`. `None` when the selection is not one of these shapes or reads
    /// a column whose type cannot answer the predicate.
    fn subset_columns(
        &mut self,
        columns: &[RecordField<'db>],
        value: ExprId,
    ) -> Option<Vec<RecordField<'db>>> {
        let module = self.module;
        let db = self.db;
        let unparenthesized = |mut id: ExprId| {
            while let ExpressionKind::Paren(inner) = module.kind(id) {
                id = *inner;
            }
            id
        };
        let (negated, selection) = match module.kind(unparenthesized(value)) {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not | UnaryOperator::Minus,
                operand,
            } => (true, unparenthesized(*operand)),
            _ => (false, unparenthesized(value)),
        };
        let frame = self.column_scopes.last().and_then(|scope| scope.frame);
        let position = |checker: &mut Self, item: ExprId| {
            let expression = item;
            let name = match module.kind(expression) {
                ExpressionKind::Literal(LiteralKind::String(text)) => module[text].to_string(),
                ExpressionKind::NameRef(name) => name.to_string(),
                _ => return Err(()),
            };
            let found = columns
                .iter()
                .position(|column| column.name.text(db) == name.as_str());
            if found.is_none() {
                checker.errors.push(TypeError {
                    range: module.range(expression),
                    kind: column_does_not_exist(db, &name, frame, columns),
                });
            }
            Ok(found)
        };
        let selected: Vec<bool> = match module.kind(selection) {
            ExpressionKind::Literal(LiteralKind::String(_)) => {
                let found = position(self, selection).ok()??;
                (0..columns.len()).map(|index| index == found).collect()
            }
            ExpressionKind::Call { callee, arguments }
                if variable_name(module, *callee).as_deref() == Some("c")
                    && string_literals(module, &module[arguments]).is_some() =>
            {
                let mut chosen = vec![false; columns.len()];
                let mut complete = true;
                for item in module[arguments]
                    .iter()
                    .filter_map(|argument| argument.value)
                {
                    match position(self, item) {
                        Ok(Some(index)) => {
                            if let Some(slot) = chosen.get_mut(index) {
                                *slot = true;
                            }
                        }
                        _ => complete = false,
                    }
                }
                if !complete {
                    return None;
                }
                chosen
            }
            // A variable holding column names, as `cols <- c("a", "b")` makes it.
            ExpressionKind::NameRef(_) if let Some(names) = self.column_names(selection) => {
                self.infer(selection);
                let mut chosen = vec![false; columns.len()];
                let mut complete = true;
                for (name, range) in &names {
                    match columns
                        .iter()
                        .position(|column| column.name.text(db) == name.as_str())
                    {
                        Some(index) => {
                            if let Some(slot) = chosen.get_mut(index) {
                                *slot = true;
                            }
                        }
                        None => {
                            complete = false;
                            self.errors.push(TypeError {
                                range: *range,
                                kind: column_does_not_exist(db, name, frame, columns),
                            });
                        }
                    }
                }
                if !complete {
                    return None;
                }
                chosen
            }
            ExpressionKind::Binary {
                operator: BinaryOperator::Sequence,
                lhs,
                rhs,
                ..
            } => {
                let named = |item: ExprId| {
                    matches!(
                        module.kind(item),
                        ExpressionKind::Literal(LiteralKind::String(_))
                            | ExpressionKind::NameRef(_)
                    )
                };
                if !named(*lhs) || !named(*rhs) {
                    self.infer(value);
                    return None;
                }
                let (Ok(from), Ok(to)) = (position(self, *lhs), position(self, *rhs)) else {
                    return None;
                };
                let (from, to) = (from?, to?);
                let (low, high) = (from.min(to), from.max(to));
                (0..columns.len())
                    .map(|index| (low..=high).contains(&index))
                    .collect()
            }
            ExpressionKind::NameRef(name) => {
                let wanted: &[Atomic] = match name.as_str() {
                    "is.numeric" => &[Atomic::Integer, Atomic::Double],
                    "is.double" => &[Atomic::Double],
                    "is.integer" => &[Atomic::Integer],
                    "is.character" => &[Atomic::Character],
                    "is.logical" => &[Atomic::Logical],
                    _ => {
                        self.infer(value);
                        return None;
                    }
                };
                let mut chosen = Vec::with_capacity(columns.len());
                for column in columns {
                    match column_element(db, column.ty).kind(db) {
                        TyKind::Scalar(atomic) => chosen.push(wanted.contains(atomic)),
                        _ => return None,
                    }
                }
                chosen
            }
            _ => {
                self.infer(value);
                return None;
            }
        };
        Some(
            columns
                .iter()
                .zip(selected)
                .filter(|(_, chosen)| *chosen != negated)
                .map(|(column, _)| *column)
                .collect(),
        )
    }

    /// `lapply(.SD, f, ...)` gives one column per column of `.SD`, each `f` applied to that
    /// column. The call is typed once per column with `.SD` read as a one-column list, so `f`
    /// is checked against every column's own type rather than their union. `None` when `j` is
    /// not that call; `Some(None)` when `.SD`'s columns are not known.
    fn subset_apply(&mut self, value: ExprId) -> Option<Option<Vec<RecordField<'db>>>> {
        let module = self.module;
        let db = self.db;
        let ExpressionKind::Call { callee, arguments } = module.kind(value) else {
            return None;
        };
        let bound = bind_formals(module, &module[arguments], &["X", "FUN"]);
        let subset = bound.value(0)?;
        if variable_name(module, *callee).as_deref() != Some("lapply")
            || self.naming.resolutions.contains_key(callee)
            || !matches!(module.kind(subset), ExpressionKind::NameRef(name) if name == ".SD")
        {
            return None;
        }
        let subset_data = self.column_scopes.last()?.subset_data;
        let columns = match subset_data.kind(db) {
            TyKind::Frame(_, Some(columns)) if !columns.is_empty() => *columns,
            _ => {
                self.infer(value);
                return Some(None);
            }
        };
        let mut findings: Vec<Vec<TypeError<'db>>> = Vec::with_capacity(columns.len());
        let mut applied = Some(Vec::with_capacity(columns.len()));
        // An overloaded `f` passed by name is a set of functions, not one function type, so
        // `lapply` cannot pick its result per element; calling it on each column directly can.
        let callback = bound.value(1).and_then(|function| self.callback(function));
        let overloads_used = callback.is_some();
        if let Some(callback) = callback {
            let rows: Vec<Vec<Ty<'db>>> = columns.iter().map(|column| vec![column.ty]).collect();
            let forwarded: Vec<Argument> = module[arguments]
                .iter()
                .filter(|argument| {
                    argument.value != Some(subset) && argument.value != Some(callback.function)
                })
                .cloned()
                .collect();
            let results = self.apply_overloads(value, &callback, &[subset], &forwarded, &rows);
            for (column, (ty, column_findings)) in columns.iter().zip(results) {
                findings.push(column_findings);
                if let Some(applied) = applied.as_mut() {
                    let column_name = column.name.text(db).to_owned();
                    applied.push(self.column_field(&column_name, ty));
                }
            }
        } else {
            for column in columns.iter() {
                if let Some(scope) = self.column_scopes.last_mut() {
                    scope.subset_data = Ty::new(db, TyKind::Record(&[*column]));
                }
                let recorded_errors = self.errors.len();
                let ty = self.infer(value);
                findings.push(self.split_off_errors(recorded_errors));
                let element = match self.table.resolve(db, ty).kind(db) {
                    TyKind::List(element) | TyKind::NamedList(element) => Some(*element),
                    _ => None,
                };
                match (element, applied.as_mut()) {
                    (Some(element), Some(applied)) => {
                        let column_name = column.name.text(db).to_owned();
                        applied.push(self.column_field(&column_name, element));
                    }
                    _ => applied = None,
                }
            }
            if let Some(scope) = self.column_scopes.last_mut() {
                scope.subset_data = subset_data;
            }
        }
        self.record(subset, subset_data);
        if overloads_used {
            self.infer(*callee);
        }
        let applied_ty = match &applied {
            Some(applied) => Ty::new(db, TyKind::Record(&applied.clone())),
            None => unknown(db),
        };
        self.record(value, applied_ty);
        // A finding every column causes is about `f` itself and is reported once; one that
        // only some columns cause names each of them.
        let shared: Vec<TypeError<'db>> = findings
            .first()
            .map(|first| {
                first
                    .iter()
                    .filter(|finding| findings.iter().all(|column| column.contains(finding)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for (column, column_findings) in columns.iter().zip(findings) {
            for finding in column_findings {
                if shared.contains(&finding) {
                    continue;
                }
                self.errors.push(TypeError {
                    range: finding.range,
                    kind: TypeErrorKind::InSubsetColumn {
                        column: column.name.text(db).to_owned(),
                        inner: Box::new(finding.kind),
                    },
                });
            }
        }
        self.errors.extend(shared);
        Some(applied)
    }

    /// The scheme set of an overloaded function passed by name as a callback, such as `sum` in
    /// `lapply(xs, sum)`. `None` for a local function or a name with one signature.
    /// Calls the overloaded `function` once per row of `each`, the way an apply-family
    /// function calls its callback: each row fills the leading arguments, whose findings are
    /// blamed on `leading`, and `forwarded` follows them. Each result comes with the findings
    /// that row caused.
    fn apply_overloads(
        &mut self,
        call: ExprId,
        callback: &Callback<'db>,
        leading: &[ExprId],
        forwarded: &[Argument],
        each: &[Vec<Ty<'db>>],
    ) -> Vec<(Ty<'db>, Vec<TypeError<'db>>)> {
        let module = self.module;
        let range = module.range(call);
        let mut call_arguments: Vec<CallArgument<'db>> = leading
            .iter()
            .map(|&argument| CallArgument {
                name: None,
                name_range: None,
                ty: None,
                range: module.range(argument),
                value: None,
                whole_double: false,
                forwards_dots: false,
                named_function: None,
            })
            .collect();
        call_arguments.extend(self.infer_call_arguments(range, forwarded));
        let mut results = Vec::with_capacity(each.len());
        for row in each {
            for (argument, &ty) in call_arguments.iter_mut().zip(row) {
                argument.ty = Some(ty);
            }
            let recorded_errors = self.errors.len();
            let ty = self.overloaded_call(
                call,
                range,
                callback.function,
                Symbol::new(&callback.name),
                callback.schemes,
                &call_arguments,
            );
            results.push((ty, self.split_off_errors(recorded_errors)));
        }
        results
    }

    /// `lapply`, `sapply`, `vapply` and `Reduce` with an overloaded callback passed by name,
    /// such as `lapply(x, sum)`. A set of signatures has no single function type to hand to the
    /// apply function's own signature, so the callback is called directly on each element type
    /// of `x` instead: each field of a record or frame, or the one element type of a vector or
    /// list. `None` for any other callback, which the apply function's signature types.
    fn infer_apply(
        &mut self,
        id: ExprId,
        callee: ExprId,
        family: &str,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let db = self.db;
        if self.naming.resolutions.contains_key(&callee) {
            return None;
        }
        let (formals, named_only): (&[&str], &[&str]) = match family {
            "lapply" => (&["X", "FUN"], &[]),
            "sapply" => (&["X", "FUN"], &["simplify", "USE.NAMES"]),
            "vapply" => (&["X", "FUN", "FUN.VALUE"], &["USE.NAMES"]),
            "Reduce" => (&["f", "x", "init", "right", "accumulate", "simplify"], &[]),
            _ => return None,
        };
        let bound = bind_formals(self.module, arguments, formals);
        let (x, function) = if family == "Reduce" {
            (bound.value(1)?, bound.value(0)?)
        } else {
            (bound.value(0)?, bound.value(1)?)
        };
        let callback = match self.callback(function) {
            Some(callback) => callback,
            None if family == "Reduce" => return self.infer_reduce_fold(id, callee, &bound),
            None => return None,
        };
        self.infer(callee);
        let x_ty = self.infer(x);
        let x_ty = self.structural(x_ty);
        let (elements, named) = element_types(db, x_ty);

        if family == "Reduce" {
            for &extra in &bound.values[2..] {
                if let Some(extra) = extra {
                    self.infer(extra);
                }
            }
            if bound.value(3).is_some() || bound.value(4).is_some() || bound.value(5).is_some() {
                return Some(unknown(db));
            }
            let element = self.join_all(&elements);
            let start = match bound.value(2) {
                Some(init) => self
                    .recorded
                    .get(&init)
                    .copied()
                    .unwrap_or_else(|| unknown(db)),
                None => element,
            };
            // The accumulator's type can widen after the first step (`integer` plus `double`),
            // so a second step reaches the type every later step keeps.
            let mut accumulator = start;
            for _ in 0..2 {
                let results = self.apply_overloads(
                    id,
                    &callback,
                    &[x, x],
                    &[],
                    &[vec![accumulator, element]],
                );
                let (next, findings) = results.into_iter().next()?;
                self.report_once(findings);
                if next == accumulator {
                    break;
                }
                accumulator = if bound.value(2).is_some() {
                    next
                } else {
                    self.join_types(start, next)
                };
            }
            return Some(accumulator);
        }

        let consumed = [Some(x), Some(function), bound.value(2)];
        let forwarded: Vec<Argument> = arguments
            .iter()
            .filter(|argument| {
                !consumed.contains(&argument.value)
                    && !self
                        .module
                        .argument_name(argument)
                        .is_some_and(|name| named_only.contains(&name.as_str()))
            })
            .cloned()
            .collect();
        for argument in arguments {
            if let Some(value) = argument.value
                && self
                    .module
                    .argument_name(argument)
                    .is_some_and(|name| named_only.contains(&name.as_str()))
            {
                self.infer(value);
            }
        }
        let rows: Vec<Vec<Ty<'db>>> = elements.iter().map(|&element| vec![element]).collect();
        let results = self.apply_overloads(id, &callback, &[x], &forwarded, &rows);
        let mut returned = Vec::with_capacity(results.len());
        for (ty, findings) in results {
            self.report_once(findings);
            returned.push(ty);
        }
        let element = self.join_all(&returned);
        let vector = |element: Ty<'db>| {
            Ty::new(
                db,
                if named {
                    TyKind::NamedVector(element)
                } else {
                    TyKind::Vector(element)
                },
            )
        };
        let list = |element: Ty<'db>| {
            Ty::new(
                db,
                if named {
                    TyKind::NamedList(element)
                } else {
                    TyKind::List(element)
                },
            )
        };
        Some(match family {
            // `FUN.VALUE` is a length-one template such as `integer(1)`, and the result is a
            // vector of its type whatever the callback returns, because `vapply` errors otherwise.
            "vapply" => {
                let template = bound.value(2)?;
                let template = self.infer(template);
                let template = self.structural(template);
                match template.kind(db) {
                    TyKind::Scalar(_) => vector(template),
                    TyKind::Vector(element) | TyKind::NamedVector(element) => vector(*element),
                    _ => unknown(db),
                }
            }
            "sapply" => match (
                simplify_flag(self.module, arguments, "simplify"),
                self.table.resolve(db, element).kind(db),
            ) {
                (Some(false), _) => list(element),
                // `sapply` simplifies results that all have length one into a vector.
                (Some(true), TyKind::Scalar(_)) => vector(element),
                _ => unknown(db),
            },
            _ => list(element),
        })
    }

    /// `sapply`, `mapply` and `Map` with a callback whose signature is known. Their stubs return `Any`,
    /// because the result's shape depends on what the callback returns and on the `simplify`
    /// flag, which no candidate can discriminate. A callback that returns a scalar-like value
    /// simplifies to a vector of it, and `simplify = FALSE` keeps a list.
    fn simplified_apply(&mut self, family: &str, arguments: &[Argument]) -> Option<Ty<'db>> {
        let db = self.db;
        let (function, items, flag) = if family == "sapply" {
            let bound = bind_formals(self.module, arguments, &["X", "FUN"]);
            (bound.value(1)?, bound.value(0), Some("simplify"))
        } else {
            let (formal, flag) = if family == "Map" {
                ("f", None)
            } else {
                ("FUN", Some("SIMPLIFY"))
            };
            let function = bind_formals(self.module, arguments, &[formal]).value(0)?;
            // The first argument `...` takes is the one whose names the result keeps.
            let items = arguments
                .iter()
                .filter(|argument| {
                    !self.module.argument_name(argument).is_some_and(|name| {
                        matches!(name.as_str(), "FUN" | "MoreArgs" | "SIMPLIFY" | "USE.NAMES")
                            || name.as_str() == formal
                    })
                })
                .filter_map(|argument| argument.value)
                .find(|value| *value != function);
            (function, items, flag)
        };
        let function = self.recorded.get(&function).copied()?;
        // The parameters are read as the callback declared them, before the call bound them to
        // the elements.
        let (generic, returned) = match self.table.shallow_resolve(db, function).kind(db) {
            TyKind::Function(function) => (
                function
                    .positional
                    .iter()
                    .copied()
                    .chain(function.named.iter().map(|parameter| parameter.ty))
                    .any(|parameter| {
                        contains_inference_var(db, parameter) || contains_rigid(parameter)
                    }),
                self.table.resolve(db, function.ret),
            ),
            _ => return None,
        };
        // An undetermined return keeps the stub's `Any`, which strict mode does not report, and
        // so does one that depends on the call's own type variables (`identity`'s `T`), which
        // the stub's `Any` never fixes.
        if matches!(returned.kind(db), TyKind::Unknown)
            || contains_inference_var(db, returned)
            || contains_rigid(returned)
        {
            return None;
        }
        let items = items.map(|items| {
            let items = self
                .recorded
                .get(&items)
                .copied()
                .unwrap_or_else(|| unknown(db));
            self.structural(items)
        });
        // A generic callback's return was inferred before its parameter met an element, and
        // generic arithmetic claims a scalar (`function(v) v * 2` returns `double`), which holds
        // only when every element is one: an atomic vector's are, a list's may be vectors.
        let scalar_elements = items.is_some_and(|items| {
            element_types(db, items).0.iter().all(|&element| {
                matches!(self.table.resolve(db, element).kind(db), TyKind::Scalar(_))
            })
        });
        if generic && !scalar_elements {
            return None;
        }
        let use_names = simplify_flag(self.module, arguments, "USE.NAMES") != Some(false);
        let named = items.is_some_and(|items| {
            let character = match items.kind(db) {
                TyKind::Vector(element) => *element,
                _ => items,
            };
            let character_names =
                use_names && matches!(character.kind(db), TyKind::Scalar(Atomic::Character));
            // `sapply` keeps the input's names whatever `USE.NAMES` says, while `mapply` and
            // `Map` name the result only under `USE.NAMES`.
            (element_types(db, items).1 && (family == "sapply" || use_names)) || character_names
        });
        // `Map` is `mapply` with `SIMPLIFY = FALSE`.
        let simplify = match flag {
            Some(flag) => simplify_flag(self.module, arguments, flag),
            None => Some(false),
        };
        let shape = match simplify {
            Some(false) if named => TyKind::NamedList(returned),
            Some(false) => TyKind::List(returned),
            Some(true) if matches!(returned.kind(db), TyKind::Scalar(_)) => {
                if named {
                    TyKind::NamedVector(returned)
                } else {
                    TyKind::Vector(returned)
                }
            }
            _ => return None,
        };
        Some(Ty::new(db, shape))
    }

    /// The generic `vapply` candidate fixes `V` from the callback first, so a template that
    /// disagrees is reported against `V[] | V`; this names the callback and its return instead.
    fn name_vapply_callback(&mut self, errors_before: usize, arguments: &[Argument]) {
        let db = self.db;
        let bound = bind_formals(self.module, arguments, &["X", "FUN", "FUN.VALUE"]);
        let (Some(function), Some(template)) = (bound.value(1), bound.value(2)) else {
            return;
        };
        let template_range = self.module.range(template);
        let callback = match self.module.kind(function) {
            ExpressionKind::NameRef(name) => format!("`{name}`"),
            ExpressionKind::Literal(LiteralKind::String(text)) => {
                format!("`{}`", &self.module[text])
            }
            ExpressionKind::Function { .. } => "the function".to_owned(),
            _ => return,
        };
        for index in errors_before..self.errors.len() {
            let error = &self.errors[index];
            let TypeErrorKind::Mismatch { expected, found } = error.kind else {
                continue;
            };
            if error.range != template_range {
                continue;
            }
            // The expected type is `V[] | V`, and `V` may itself be a union, so the vector
            // member's element is the callback's return.
            let returns = match self.table.resolve(db, expected).kind(db) {
                TyKind::Union(members) => members.iter().find_map(|member| match member.kind(db) {
                    TyKind::Vector(element) => Some(*element),
                    _ => None,
                }),
                _ => None,
            };
            let Some(returns) = returns else {
                continue;
            };
            self.errors[index].kind = TypeErrorKind::TemplateDisagreesWithCallback {
                callback: callback.clone(),
                returns,
                template: found,
            };
        }
    }

    fn join_all(&mut self, types: &[Ty<'db>]) -> Ty<'db> {
        let mut joined: Option<Ty<'db>> = None;
        for &ty in types {
            joined = Some(match joined {
                Some(previous) if previous != ty => self.join_types(previous, ty),
                Some(previous) => previous,
                None => ty,
            });
        }
        joined.unwrap_or_else(|| unknown(self.db))
    }

    fn report_once(&mut self, findings: Vec<TypeError<'db>>) {
        for finding in findings {
            if !self.errors.contains(&finding) {
                self.errors.push(finding);
            }
        }
    }

    /// `Reduce(f, x)` with a non-overloaded function, named or picked by an `if`. R hands `f` the
    /// accumulator and each element in turn, and the accumulator takes whatever `f` returns, so its
    /// type is the join of the start and every return (`function(x, y) if (identical(x, y)) x`
    /// folds to `T | NULL`); the stub's `fn(U, T) -> U` cannot widen, so the fold iterates until
    /// the accumulator stops growing. A failing step is reported as is, otherwise only the last
    /// step's findings are kept. A function closing over the enclosing function's open variables
    /// keeps the stub's typing, since iterating would pin them to every step.
    fn infer_reduce_fold(
        &mut self,
        id: ExprId,
        callee: ExprId,
        bound: &BoundFormals,
    ) -> Option<Ty<'db>> {
        let db = self.db;
        let module = self.module;
        let function = bound.value(0)?;
        let x = bound.value(1)?;
        let named_function = |name: ExprId| matches!(module.kind(name), ExpressionKind::NameRef(_));
        let eligible = named_function(function)
            || matches!(
                module.kind(function),
                ExpressionKind::If { then_branch, else_branch: Some(else_branch), .. }
                    if named_function(*then_branch) && named_function(*else_branch)
            );
        if !eligible || bound.values[3..].iter().any(Option::is_some) {
            return None;
        }
        let recorded_errors = self.errors.len();
        let recorded_origins = self.strict_origins.len();
        let first_var = self.table.next_var().0;
        let callback_ty = self.infer(function);
        let callback_ty = self.table.resolve(db, callback_ty);
        // R runs `match.fun(f)`, so a string naming a function is valid and keeps the stub path.
        let callable = |ty: Ty<'db>| matches!(ty.kind(db), TyKind::Function(_));
        let all_functions = match callback_ty.kind(db) {
            TyKind::Union(members) => members.iter().all(|&member| callable(member)),
            _ => callable(callback_ty),
        };
        if !all_functions || self.mentions_var_before(callback_ty, first_var) {
            self.truncate_errors(recorded_errors);
            self.strict_origins.truncate(recorded_origins);
            return None;
        }
        self.infer(callee);
        let x_ty = self.infer(x);
        let x_ty = self.structural(x_ty);
        let (elements, _) = element_types(db, x_ty);
        let element = self.join_all(&elements);
        let init = bound.value(2);
        let accumulator = match init {
            Some(init) => self.infer(init),
            None => element,
        };
        let range = module.range(id);
        let callee_range = self.blame_range(function);
        Some(self.fold_reduce(
            range,
            callee_range,
            function,
            callback_ty,
            init.unwrap_or(x),
            x,
            accumulator,
            element,
        ))
    }

    /// Calls the callback with the accumulator and an element until the accumulator stops
    /// widening. `function` is re-inferred each round so a generic lambda instantiates afresh.
    #[allow(clippy::too_many_arguments)]
    fn fold_reduce(
        &mut self,
        range: TextRange,
        callee_range: TextRange,
        function: ExprId,
        callback_ty: Ty<'db>,
        start: ExprId,
        x: ExprId,
        mut accumulator: Ty<'db>,
        element: Ty<'db>,
    ) -> Ty<'db> {
        let module = self.module;
        let argument = |value: ExprId, ty: Ty<'db>| CallArgument {
            name: None,
            name_range: None,
            ty: Some(ty),
            range: module.range(value),
            value: Some(value),
            whole_double: false,
            forwards_dots: false,
            named_function: None,
        };
        let mut callback_ty = callback_ty;
        // Each step can only add members to the accumulator, so a few rounds reach the
        // fixpoint of any fold the checker can type; the bound guards a type that keeps growing.
        for round in 0..4 {
            let step_errors = self.errors.len();
            let step_origins = self.strict_origins.len();
            let arguments = [argument(start, accumulator), argument(x, element)];
            let returned = self.dispatch_call(
                range,
                callee_range,
                callback_ty,
                CalleeSource::Value,
                &arguments,
            );
            let widened = self.join_types(accumulator, returned);
            if widened == accumulator || round == 3 || self.errors.len() > step_errors {
                return widened;
            }
            accumulator = widened;
            self.truncate_errors(step_errors);
            self.strict_origins.truncate(step_origins);
            let step_errors = self.errors.len();
            let step_origins = self.strict_origins.len();
            callback_ty = self.infer(function);
            self.truncate_errors(step_errors);
            self.strict_origins.truncate(step_origins);
        }
        accumulator
    }

    /// Whether `ty` mentions an inference variable created before `first`, which is one the
    /// enclosing code owns rather than one an instantiation just made.
    fn mentions_var_before(&self, ty: Ty<'db>, first: u32) -> bool {
        let db = self.db;
        if !contains_inference_var(db, ty) {
            return false;
        }
        if let TyKind::Var(var) = ty.kind(db) {
            return var.0 < first;
        }
        let mut found = false;
        for_each_child_type(db, ty, &mut |child| {
            found = found || self.mentions_var_before(child, first);
        });
        found
    }

    /// The overload set a callback passed by name stands for, unless a binding shadows it.
    fn callback(&self, function: ExprId) -> Option<Callback<'db>> {
        let ExpressionKind::NameRef(name) = self.module.kind(function) else {
            return None;
        };
        if self.naming.resolutions.contains_key(&function) {
            return None;
        }
        let deferred = self.naming.deferred_non_locals.contains(&function);
        let schemes = self.globals?.overloads(*name, deferred)?;
        (schemes.len() >= 2).then(|| Callback {
            function,
            name: name.to_string(),
            schemes,
        })
    }

    fn query_result(
        &mut self,
        id: ExprId,
        kind: FrameKind,
        j: ExprId,
        source_columns: Option<&[RecordField<'db>]>,
        groups: Option<Vec<RecordField<'db>>>,
        grouped: bool,
    ) -> Ty<'db> {
        let db = self.db;
        let module = self.module;
        // data.table keeps a computed column that repeats a group's name as a second column of
        // that name, which a frame type cannot hold, so such a result has unknown columns.
        let frame_of = |columns: Option<Vec<RecordField<'db>>>| {
            let columns = columns.filter(|columns| {
                columns.iter().enumerate().all(|(index, column)| {
                    columns
                        .iter()
                        .skip(index + 1)
                        .all(|other| other.name != column.name)
                })
            });
            Ty::new(db, TyKind::Frame(kind, columns.as_deref()))
        };
        if let Some(applied) = self.subset_apply(j) {
            return frame_of(groups.zip(applied).map(|(mut groups, applied)| {
                groups.extend(applied);
                groups
            }));
        }
        if let ExpressionKind::Call { callee, arguments } = module.kind(j)
            && matches!(
                variable_name(module, *callee).as_deref(),
                Some("." | "list")
            )
        {
            let mut columns = groups;
            for (position, argument) in module[arguments].iter().enumerate() {
                let Some(value) = argument.value else {
                    continue;
                };
                let ty = self.infer(value);
                // data.table names an unnamed computed item by its position, and `.N` as `N`.
                let name = module
                    .argument_name(argument)
                    .or_else(|| variable_name(module, value).map(count_column_name))
                    .unwrap_or_else(|| Symbol::from(format!("V{}", position + 1)));
                let field = self.column_field(&name, ty);
                if let Some(columns) = columns.as_mut() {
                    columns.push(field);
                }
            }
            return frame_of(columns);
        }
        let ty = self.infer(j);
        let resolved = self.table.resolve(db, ty);
        let bare_column = match module.kind(j) {
            ExpressionKind::NameRef(name)
                if self.masked_reads.contains(j)
                    && source_columns.is_some_and(|columns| {
                        columns.iter().any(|column| column.name.text(db) == name)
                    }) =>
            {
                Some(*name)
            }
            _ => None,
        };
        if !grouped {
            return match resolved.kind(db) {
                TyKind::Scalar(_)
                | TyKind::Vector(_)
                | TyKind::NamedVector(_)
                | TyKind::Null
                | TyKind::Frame(..)
                | TyKind::Unknown => resolved,
                _ if bare_column.is_some() => resolved,
                _ => {
                    self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                    unknown(db)
                }
            };
        }
        let Some(mut columns) = groups else {
            return frame_of(None);
        };
        let count = matches!(module.kind(j), ExpressionKind::NameRef(name) if name == ".N");
        match (bare_column, resolved.kind(db)) {
            (Some(name), _) => columns.push(self.column_field(&name, resolved)),
            (None, _) if count => columns.push(self.column_field("N", resolved)),
            (None, TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_)) => {
                columns.push(self.column_field("V1", resolved));
            }
            (None, TyKind::Frame(_, Some(computed))) => columns.extend(computed.iter().cloned()),
            (None, TyKind::Record(fields)) => {
                for field in fields.iter() {
                    let name = field.name.text(db).to_owned();
                    columns.push(self.column_field(&name, field.ty));
                }
            }
            _ => return frame_of(None),
        }
        frame_of(Some(columns))
    }

    /// `j` as `y := v`, `c("a", "b") := list(v, w)` or `` `:=`(a = v, b = w) ``: the frame with
    /// each named column set to its value's vector type, or removed by `NULL`; other left sides
    /// widen to unknown columns. data.table coerces a value into an existing column's type, so the
    /// type stays unless the value replaces the column whole, which it does not with `partial` (an
    /// `i` or `by`) or a literal. Only a literal is certainly one value: `as.character(x)` is
    /// scalar-like too.
    fn column_update(
        &mut self,
        arguments: &[Argument],
        base: Option<&[RecordField<'db>]>,
        partial: bool,
    ) -> Option<Vec<RecordField<'db>>> {
        let module = self.module;
        let db = self.db;
        let mut updates: Option<Vec<(String, Ty<'db>, Option<ExprId>)>> = Some(Vec::new());
        match arguments {
            [left, right] if left.name.is_none() && right.name.is_none() => {
                // The left side names columns: a bare name is the column itself, and `(cols)`
                // evaluates `cols`, whose names are known when it holds literal strings.
                let names: Option<Vec<String>> =
                    match left.value.map(|value| (value, module.kind(value))) {
                        Some((_, ExpressionKind::NameRef(name))) => Some(vec![name.to_string()]),
                        Some((value, _)) => {
                            let names = self
                                .column_names(value)
                                .map(|names| names.into_iter().map(|(name, _)| name).collect());
                            if names.is_none()
                                || matches!(module.kind(value), ExpressionKind::Paren(_))
                            {
                                self.infer(value);
                            }
                            names
                        }
                        None => None,
                    };
                let items: Option<Vec<ExprId>> =
                    right.value.and_then(|value| match module.kind(value) {
                        ExpressionKind::Call { callee, arguments }
                            if matches!(
                                variable_name(module, *callee).as_deref(),
                                Some("." | "list")
                            ) =>
                        {
                            Some(
                                module[arguments]
                                    .iter()
                                    .filter_map(|argument| argument.value)
                                    .collect(),
                            )
                        }
                        _ => None,
                    });
                let applied = right.value.and_then(|value| self.subset_apply(value));
                updates = match (names, items, applied) {
                    // `c("a", "b") := lapply(.SD, f)` pairs the names with `.SD`'s columns.
                    (Some(names), _, Some(Some(applied))) if names.len() == applied.len() => Some(
                        names
                            .into_iter()
                            .zip(applied)
                            .map(|(name, column)| (name, column.ty, None))
                            .collect(),
                    ),
                    (_, _, Some(_)) => None,
                    // data.table reads a `list()` right side as one value per name, even for one name.
                    (Some(names), Some(items), None) if names.len() == items.len() => {
                        let mut paired = Vec::with_capacity(names.len());
                        for (name, item) in names.into_iter().zip(items) {
                            paired.push((name, self.infer(item), Some(item)));
                        }
                        Some(paired)
                    }
                    (Some(names), _, None) if names.len() == 1 => {
                        let ty = match right.value {
                            Some(value) => self.infer(value),
                            None => unknown(db),
                        };
                        Some(
                            names
                                .into_iter()
                                .map(|name| (name, ty, right.value))
                                .collect(),
                        )
                    }
                    _ => {
                        if let Some(value) = right.value {
                            self.infer(value);
                        }
                        None
                    }
                };
            }
            _ => {
                for argument in arguments {
                    let Some(value) = argument.value else {
                        continue;
                    };
                    let ty = self.infer(value);
                    match (self.module.argument_name(argument), updates.as_mut()) {
                        (Some(name), Some(updates)) => {
                            updates.push((name.to_string(), ty, Some(value)));
                        }
                        _ => updates = None,
                    }
                }
            }
        }
        let mut columns = base?.to_vec();
        for (name, ty, value) in updates? {
            let resolved = self.table.resolve(db, ty);
            let single = value.is_some_and(|value| is_literal(module, value));
            let position = columns
                .iter()
                .position(|column| column.name.text(db) == name);
            match (resolved.kind(db), position) {
                (TyKind::Null, Some(position)) => {
                    columns.remove(position);
                }
                (TyKind::Null, None) => {}
                (_, Some(position)) if partial || single => {
                    if let (Some(value), Some(column)) = (value, columns.get(position))
                        && lossy_write(db, module, column.ty, resolved, value)
                    {
                        self.errors.push(TypeError {
                            range: self.blame_range(value),
                            kind: TypeErrorKind::LossyColumnWrite {
                                column: name.clone(),
                                column_ty: column.ty,
                                found: resolved,
                            },
                        });
                    }
                }
                (_, Some(position)) => {
                    if let Some(column) = columns.get_mut(position) {
                        column.ty = column_type(db, resolved);
                    }
                }
                (_, None) => columns.push(RecordField {
                    name: Name::new(db, name),
                    ty: column_type(db, resolved),
                    optional: false,
                }),
            }
        }
        Some(columns)
    }

    fn column_field(&mut self, name: &str, ty: Ty<'db>) -> RecordField<'db> {
        let resolved = self.settle_literals(ty);
        RecordField {
            name: Name::new(self.db, name),
            ty: column_type(self.db, resolved),
            optional: false,
        }
    }

    /// Nested function bodies are included: a closure written inside `j` is created in
    /// the data's frame.
    fn mask_column_reads(&mut self, id: ExprId) {
        let kind = self.module.kind(id);
        if matches!(kind, ExpressionKind::NameRef(_)) {
            self.masked_reads.insert(id);
        }
        let module = self.module;
        module.for_each_child(id, |child| self.mask_column_reads(child));
    }

    fn extract_result(
        &mut self,
        origin: ExprId,
        range: TextRange,
        key_range: TextRange,
        subject: Ty<'db>,
        index: Option<&ExpressionKind>,
    ) -> Result<Ty<'db>, TypeError<'db>> {
        if let TyKind::Union(members) = subject.kind(self.db) {
            let mut results = Vec::with_capacity(members.len());
            // An empty `list()` member is the accumulator on a path that never filled it,
            // where R reads `NULL`.
            for &member in members.iter() {
                if is_empty_list(self.db, member) {
                    results.push(null(self.db));
                    continue;
                }
                let result = self
                    .extract_result(origin, range, key_range, member, index)
                    .map_err(|error| widen_error_container(error, subject))?;
                results.push(result);
            }
            return Ok(union_of(self.db, results));
        }
        let literal_position = index.and_then(|kind| integer_literal_position(self.module, kind));
        let literal_name = match index {
            Some(ExpressionKind::Literal(LiteralKind::String(name))) => {
                Some(self.module[name].to_string())
            }
            _ => None,
        };
        match subject.kind(self.db) {
            TyKind::Unknown => Ok(unknown(self.db)),
            TyKind::Any => Ok(any(self.db)),
            // R reads every element of `NULL` as `NULL`, so `opts$name` on an optional list is the
            // ordinary way to read a setting that may be absent.
            TyKind::Null => Ok(subject),
            TyKind::Scalar(atomic) => Ok(scalar(self.db, *atomic)),
            // The untyped columns of a `data.frame`: a declaration has no site to report, so
            // the read originates the `Unknown`.
            TyKind::List(element) | TyKind::NamedList(element)
                if matches!(element.kind(self.db), TyKind::Unknown) =>
            {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(*element)
            }
            // R stops with "subscript out of bounds" on an absent name in an atomic vector,
            // so a read that returns has the element's type.
            TyKind::Vector(element) | TyKind::NamedVector(element) | TyKind::List(element) => {
                Ok(*element)
            }
            // A literal name may be absent from a list at runtime (`T | NULL`).
            TyKind::NamedList(element) => Ok(if literal_name.is_some() {
                union_of(self.db, [*element, null(self.db)])
            } else {
                *element
            }),
            TyKind::NativeTuple(items) => match literal_position {
                Some(position) => match items.get(position) {
                    Some(&item) => Ok(item),
                    None => Err(TypeError {
                        range: key_range,
                        kind: TypeErrorKind::PositionDoesNotExist {
                            position: position + 1,
                            container: subject,
                        },
                    }),
                },
                None => Ok(union_of(self.db, items.iter().copied())),
            },
            // A position counts fields in their sorted order, as the runtime stores them.
            TyKind::NativeRecord(fields) => match literal_name {
                Some(name) => match fields.iter().find(|field| field.name.text(self.db) == name) {
                    Some(field) => Ok(field.ty),
                    None => Err(TypeError {
                        range: key_range,
                        kind: TypeErrorKind::FieldDoesNotExist {
                            suggestion: nearest_name(
                                &name,
                                fields.iter().map(|field| field.name.text(self.db)),
                            ),
                            field: name,
                            container: subject,
                        },
                    }),
                },
                None => match literal_position {
                    Some(position) => match fields.get(position) {
                        Some(field) => Ok(field.ty),
                        None => Err(TypeError {
                            range: key_range,
                            kind: TypeErrorKind::PositionDoesNotExist {
                                position: position + 1,
                                container: subject,
                            },
                        }),
                    },
                    None => Ok(union_of(self.db, fields.iter().map(|field| field.ty))),
                },
            },
            // `list()` is the accumulator idiom: code fills it after building it, often in
            // another function, so its type knows none of the elements read later.
            TyKind::Tuple([]) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(unknown(self.db))
            }
            TyKind::Tuple(items) => match literal_position {
                Some(position) => match items.get(position) {
                    Some(&item) => Ok(item),
                    None => Err(TypeError {
                        range,
                        kind: TypeErrorKind::PositionDoesNotExist {
                            position: position + 1,
                            container: subject,
                        },
                    }),
                },
                None => Ok(union_of(self.db, items.iter().copied())),
            },
            TyKind::Frame(_, None) => Ok(unknown(self.db)),
            TyKind::Frame(_, Some(columns)) => match (literal_position, literal_name) {
                (Some(position), _) => match columns.get(position) {
                    Some(column) => Ok(column.ty),
                    None => Err(TypeError {
                        range: key_range,
                        kind: TypeErrorKind::PositionDoesNotExist {
                            position: position + 1,
                            container: subject,
                        },
                    }),
                },
                (None, Some(name)) => {
                    match columns
                        .iter()
                        .find(|column| column.name.text(self.db) == name)
                    {
                        Some(column) => Ok(column.ty),
                        None => Err(TypeError {
                            range: key_range,
                            kind: column_does_not_exist(self.db, &name, None, columns),
                        }),
                    }
                }
                (None, None) => Ok(union_of(self.db, columns.iter().map(|column| column.ty))),
            },
            // An open record lists only some of its fields, in no fixed order, so it answers
            // only a read by name.
            TyKind::Environment(fields) if let Some(name) = &literal_name => {
                match fields
                    .iter()
                    .find(|field| field.name.text(self.db) == *name)
                {
                    Some(field) => Ok(field.ty),
                    None => Err(TypeError {
                        range: key_range,
                        kind: TypeErrorKind::FieldDoesNotExist {
                            suggestion: nearest_name(
                                name,
                                fields.iter().map(|field| field.name.text(self.db)),
                            ),
                            field: name.clone(),
                            container: subject,
                        },
                    }),
                }
            }
            TyKind::OpenRecord(_) | TyKind::Environment(_)
                if literal_position.is_some() || literal_name.is_none() =>
            {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(unknown(self.db))
            }
            TyKind::Record(fields) | TyKind::OpenRecord(fields) => {
                // Record fields are declaration-ordered, so a positional `[[` works like a tuple's.
                if let Some(position) = literal_position {
                    return match fields.get(position) {
                        Some(field) => Ok(field.ty),
                        None => Err(TypeError {
                            range: key_range,
                            kind: TypeErrorKind::PositionDoesNotExist {
                                position: position + 1,
                                container: subject,
                            },
                        }),
                    };
                }
                match literal_name {
                    None => Ok(union_of(self.db, fields.iter().map(|field| field.ty))),
                    Some(name) => {
                        match fields.iter().find(|field| field.name.text(self.db) == name) {
                            Some(field) => Ok(field.ty),
                            // `[[` reads an absent name as `Unknown`, like `$` without partial
                            // matching.
                            None => match nearest_name(
                                &name,
                                fields.iter().map(|field| field.name.text(self.db)),
                            ) {
                                Some(suggestion) => Err(TypeError {
                                    range: key_range,
                                    kind: TypeErrorKind::FieldDoesNotExist {
                                        suggestion: Some(suggestion),
                                        field: name,
                                        container: subject,
                                    },
                                }),
                                None => {
                                    self.record_strict_origin(
                                        origin,
                                        StrictOriginKind::UnsupportedConstruct,
                                    );
                                    Ok(unknown(self.db))
                                }
                            },
                        }
                    }
                }
            }
            // Unmodeled element access (`function(x) x[[1L]]` is idiomatic): refuse with
            // `Unknown`, never reject.
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(unknown(self.db))
            }
            _ => Err(TypeError {
                range,
                kind: TypeErrorKind::NotAList { found: subject },
            }),
        }
    }

    fn subset_result(
        &mut self,
        origin: ExprId,
        range: TextRange,
        subject: Ty<'db>,
        index: Option<Ty<'db>>,
    ) -> Result<Ty<'db>, TypeError<'db>> {
        if let TyKind::Union(members) = subject.kind(self.db) {
            let mut results = Vec::with_capacity(members.len());
            for &member in members.iter() {
                let result = self
                    .subset_result(origin, range, member, index)
                    .map_err(|error| widen_error_container(error, subject))?;
                results.push(result);
            }
            return Ok(union_of(self.db, results));
        }
        match subject.kind(self.db) {
            TyKind::Unknown => Ok(unknown(self.db)),
            TyKind::Any => Ok(any(self.db)),
            TyKind::Null => Ok(subject),
            TyKind::Scalar(_) => match self.vector_index_shape(range, index)? {
                VectorIndexShape::ScalarLike => Ok(subject),
                VectorIndexShape::VectorLike => Ok(Ty::new(self.db, TyKind::Vector(subject))),
            },
            TyKind::Vector(element) | TyKind::NamedVector(element) => {
                match self.vector_index_shape(range, index)? {
                    VectorIndexShape::ScalarLike => Ok(*element),
                    VectorIndexShape::VectorLike => Ok(subject),
                }
            }
            TyKind::List(_) | TyKind::NamedList(_) => Ok(subject),
            TyKind::Tuple(items) => Ok(Ty::new(
                self.db,
                TyKind::List(union_of(self.db, items.iter().copied())),
            )),
            TyKind::Record(fields) => Ok(Ty::new(
                self.db,
                TyKind::NamedList(union_of(self.db, fields.iter().map(|field| field.ty))),
            )),
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Frame(..) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(unknown(self.db))
            }
            _ => Err(TypeError {
                range,
                kind: TypeErrorKind::UnsupportedSubset { found: subject },
            }),
        }
    }

    /// A scalar-like numeric or character index is a deliberate scalar claim (see the
    /// typing reference); undetermined shapes claim scalar and stay unconstrained.
    fn vector_index_shape(
        &mut self,
        range: TextRange,
        index: Option<Ty<'db>>,
    ) -> Result<VectorIndexShape, TypeError<'db>> {
        let Some(index) = index else {
            return Ok(VectorIndexShape::VectorLike);
        };
        let resolved = self.table.resolve(self.db, index);
        self.classify_vector_index(range, resolved)
    }

    fn classify_vector_index(
        &mut self,
        range: TextRange,
        index: Ty<'db>,
    ) -> Result<VectorIndexShape, TypeError<'db>> {
        match index.kind(self.db) {
            TyKind::Scalar(Atomic::Integer | Atomic::Double | Atomic::Character) => {
                Ok(VectorIndexShape::ScalarLike)
            }
            TyKind::Scalar(Atomic::Logical) => Ok(VectorIndexShape::VectorLike),
            TyKind::Native(native) if native.is_integer() => Ok(VectorIndexShape::ScalarLike),
            TyKind::Native(NativeScalar::Bool) => Ok(VectorIndexShape::VectorLike),
            TyKind::Native(_) | TyKind::Scalar(Atomic::Complex | Atomic::Raw) => Err(TypeError {
                range,
                kind: TypeErrorKind::BadVectorIndex { index },
            }),
            TyKind::Vector(_) | TyKind::NamedVector(_) | TyKind::Null => {
                Ok(VectorIndexShape::VectorLike)
            }
            TyKind::Unknown
            | TyKind::Any
            | TyKind::Var(_)
            | TyKind::Rigid(_)
            | TyKind::Named(..) => Ok(VectorIndexShape::ScalarLike),
            TyKind::Union(members) => {
                let mut shape = VectorIndexShape::ScalarLike;
                for &member in members.iter() {
                    if self.classify_vector_index(range, member)? == VectorIndexShape::VectorLike {
                        shape = VectorIndexShape::VectorLike;
                    }
                }
                Ok(shape)
            }
            TyKind::List(_)
            | TyKind::NamedList(_)
            | TyKind::Tuple(_)
            | TyKind::Record(_)
            | TyKind::OpenRecord(_)
            | TyKind::Environment(_)
            | TyKind::NativeTuple(_)
            | TyKind::NativeRecord(_)
            | TyKind::Tag(..)
            | TyKind::OtherTags
            | TyKind::Frame(..)
            | TyKind::Function(_) => Err(TypeError {
                range,
                kind: TypeErrorKind::BadVectorIndex { index },
            }),
        }
    }

    fn infer_field(
        &mut self,
        id: ExprId,
        range: TextRange,
        name_range: TextRange,
        at: bool,
        target: ExprId,
        name: Option<Symbol>,
    ) -> Ty<'db> {
        let target_ty = self.infer(target);
        if at {
            // Slots are untyped, but a slot of an `Any` value is as unchecked as the value.
            if matches!(
                self.table.shallow_resolve(self.db, target_ty).kind(self.db),
                TyKind::Any
            ) {
                return any(self.db);
            }
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        let Some(name) = name else {
            return unknown(self.db);
        };
        // A `Class$method` stub types that member of an object whose members are reached with
        // `$`, such as a polars frame's `$select`.
        if let TyKind::Named(class, _) =
            self.table.shallow_resolve(self.db, target_ty).kind(self.db)
            && let Some(scheme) = self.globals.and_then(|globals| {
                globals.scheme(
                    Symbol::new(&format!("{}${}", class.text(self.db), name.as_str())),
                    false,
                )
            })
        {
            return self.instantiate(&scheme);
        }
        // R dispatches `$` on a classed value to its `$.class` method before the representation
        // is ever read, so the method decides the result even for a transparent nominal.
        if let TyKind::Named(class, _) =
            self.table.shallow_resolve(self.db, target_ty).kind(self.db)
            && let Some(ty) = self.subset_method_result("$", class.text(self.db))
        {
            return ty;
        }
        if self.refuse_hidden_representation(target, target_ty) {
            return unknown(self.db);
        }
        let subject = self.structural(target_ty);
        match self.dollar_result(id, range, name_range, subject, &name) {
            Ok(ty) => self.narrowed_field(target, name).unwrap_or(ty),
            Err(error) => {
                self.errors.push(name_frame(error, self.module, target));
                unknown(self.db)
            }
        }
    }

    /// The type a dominating `is.null(x$field)` test showed `x$field` to have.
    fn narrowed_field(&self, target: ExprId, name: Symbol) -> Option<Ty<'db>> {
        if !matches!(self.module.kind(target), ExpressionKind::NameRef(_)) {
            return None;
        }
        let &slot = self.naming.resolutions.get(&target)?;
        // A closure may run after a later write has undone the test.
        if let Some(&horizon) = self.naming.extras().captured_reads.get(&target) {
            let position = self.module.range(target).start();
            let rewritten = self
                .capture_writes
                .get(slot)
                .into_iter()
                .flatten()
                .any(|(at, _)| *at > position && *at <= horizon && !self.body_range.contains(*at));
            if rewritten {
                return None;
            }
        }
        self.environment.facts.iter().find_map(|fact| match *fact {
            Fact::FieldNarrowed {
                slot: narrowed,
                field,
                ty,
            } if narrowed == slot && field == name => Some(ty),
            _ => None,
        })
    }

    fn dollar_result(
        &mut self,
        origin: ExprId,
        range: TextRange,
        name_range: TextRange,
        subject: Ty<'db>,
        name: &str,
    ) -> Result<Ty<'db>, TypeError<'db>> {
        // A member that lacks the field reads it as `Unknown` (see the record arm); a field
        // no member carries is a typo, and `$` on an atomic member is always an error.
        if let TyKind::Union(members) = subject.kind(self.db) {
            let lacks_field = |member: Ty<'db>| match member.kind(self.db) {
                TyKind::Record(fields) => !fields
                    .iter()
                    .any(|field| field.name.text(self.db).starts_with(name)),
                _ => is_empty_list(self.db, member),
            };
            if members.iter().all(|&member| lacks_field(member))
                && let Some(suggestion) = nearest_name(
                    name,
                    union_field_names(self.db, subject)
                        .iter()
                        .map(String::as_str),
                )
            {
                return Err(TypeError {
                    range: name_range,
                    kind: TypeErrorKind::FieldDoesNotExist {
                        suggestion: Some(suggestion),
                        field: name.to_owned(),
                        container: subject,
                    },
                });
            }
            let mut results = Vec::with_capacity(members.len());
            let mut absent: Option<TypeError<'db>> = None;
            let mut atomic: Option<TypeError<'db>> = None;
            let mut has_fields = false;
            let mut unfilled = false;
            for &member in members.iter() {
                // An empty `list()` member is the accumulator on a path that never filled
                // it, where R reads `NULL`.
                if is_empty_list(self.db, member) {
                    unfilled = true;
                    continue;
                }
                match self.dollar_result(origin, range, name_range, member, name) {
                    Ok(result) => {
                        has_fields |= !matches!(member.kind(self.db), TyKind::Null);
                        results.push(result);
                    }
                    Err(error) if matches!(error.kind, TypeErrorKind::FieldDoesNotExist { .. }) => {
                        absent.get_or_insert(error);
                    }
                    Err(error)
                        if matches!(error.kind, TypeErrorKind::DollarOnAtomicVector { .. }) =>
                    {
                        atomic.get_or_insert(error);
                    }
                    Err(error) => return Err(widen_error_container(error, subject)),
                }
            }
            // A function that returns a vector or a list by a flag (`info = "name"`) gives the
            // union, so a vector member leaves the read `Unknown` when a list member has fields,
            // as an absent field does. Beside `NULL` alone, the vector is the value read.
            if let Some(error) = atomic {
                if !has_fields {
                    return Err(widen_error_container(error, subject));
                }
                absent.get_or_insert(error);
            }
            if unfilled && !results.is_empty() {
                results.push(null(self.db));
            }
            if results.is_empty() && absent.is_none() {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                return Ok(unknown(self.db));
            }
            return match (results.is_empty(), absent) {
                (true, Some(mut error)) => {
                    // The failing member may have no fields at all, so suggest from the whole union.
                    if let TypeErrorKind::FieldDoesNotExist { suggestion, .. } = &mut error.kind {
                        *suggestion = nearest_name(
                            name,
                            union_field_names(self.db, subject)
                                .iter()
                                .map(String::as_str),
                        );
                    }
                    Err(widen_error_container(error, subject))
                }
                (_, None) => Ok(union_of(self.db, results)),
                (_, Some(_)) => {
                    self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                    Ok(unknown(self.db))
                }
            };
        }
        match subject.kind(self.db) {
            TyKind::Unknown => Ok(unknown(self.db)),
            TyKind::Any => Ok(any(self.db)),
            // `NULL$name` is `NULL` in R, so an optional list reads its settings directly.
            TyKind::Null => Ok(subject),
            // igraph's `V(graph)$weight` reads a classed integer vector through `$.igraph.vs`.
            TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_)
                if self
                    .globals
                    .is_some_and(|globals| globals.defines_dollar_method()) =>
            {
                Ok(unknown(self.db))
            }
            TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_) => Err(TypeError {
                range,
                kind: TypeErrorKind::DollarOnAtomicVector { found: subject },
            }),
            TyKind::NamedList(element) if matches!(element.kind(self.db), TyKind::Unknown) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(*element)
            }
            TyKind::NamedList(element) => Ok(union_of(self.db, [*element, null(self.db)])),
            // A list whose elements are not known (`as.list(environment())`, `mget(...)`)
            // says nothing about its names either.
            TyKind::List(element)
                if matches!(element.kind(self.db), TyKind::Any | TyKind::Unknown) =>
            {
                Ok(*element)
            }
            // An array-like list can still carry names at run time (`names(x) <- ...`, or a value
            // a stub types loosely), and R reads a missing name as `NULL`.
            TyKind::List(element) => Ok(union_of(self.db, [*element, null(self.db)])),
            TyKind::Frame(_, None) => Ok(unknown(self.db)),
            TyKind::Frame(_, Some(columns)) => {
                match columns
                    .iter()
                    .find(|column| column.name.text(self.db) == name)
                {
                    Some(column) => Ok(column.ty),
                    None => Err(TypeError {
                        range: name_range,
                        kind: column_does_not_exist(self.db, name, None, columns),
                    }),
                }
            }
            // An R list is open: `$` partially matches a unique prefix (`fit$coef` reads
            // `coefficients`), and code routinely adds a field after building the list
            // (`x$total <- ...` in another function), so a name the literal lacks reads as
            // `Unknown`, not as R's `NULL`: the type records only the fields seen at
            // construction. Only a near miss of a known field reads as a typo.
            TyKind::Record(fields) | TyKind::OpenRecord(fields) => {
                if let Some(field) = fields.iter().find(|field| field.name.text(self.db) == name) {
                    return Ok(field.ty);
                }
                let mut prefixed = fields
                    .iter()
                    .filter(|field| field.name.text(self.db).starts_with(name));
                if let (Some(field), None) = (prefixed.next(), prefixed.next()) {
                    return Ok(field.ty);
                }
                match nearest_name(name, fields.iter().map(|field| field.name.text(self.db))) {
                    Some(suggestion) => Err(TypeError {
                        range: name_range,
                        kind: TypeErrorKind::FieldDoesNotExist {
                            suggestion: Some(suggestion),
                            field: name.to_owned(),
                            container: subject,
                        },
                    }),
                    None => {
                        self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                        Ok(unknown(self.db))
                    }
                }
            }
            // An environment's typed bindings are all declared, so an unlisted name is a mistake.
            TyKind::Environment(fields) => {
                match fields.iter().find(|field| field.name.text(self.db) == name) {
                    Some(field) => Ok(field.ty),
                    None => Err(TypeError {
                        range: name_range,
                        kind: TypeErrorKind::FieldDoesNotExist {
                            suggestion: nearest_name(
                                name,
                                fields.iter().map(|field| field.name.text(self.db)),
                            ),
                            field: name.to_owned(),
                            container: subject,
                        },
                    }),
                }
            }
            TyKind::NativeRecord(fields) => {
                match fields.iter().find(|field| field.name.text(self.db) == name) {
                    Some(field) => Ok(field.ty),
                    None => Err(TypeError {
                        range: name_range,
                        kind: TypeErrorKind::FieldDoesNotExist {
                            suggestion: nearest_name(
                                name,
                                fields.iter().map(|field| field.name.text(self.db)),
                            ),
                            field: name.to_owned(),
                            container: subject,
                        },
                    }),
                }
            }
            TyKind::Tuple([]) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(unknown(self.db))
            }
            TyKind::Tuple(_) | TyKind::NativeTuple(_) => Err(TypeError {
                range: name_range,
                kind: TypeErrorKind::FieldDoesNotExist {
                    suggestion: None,
                    field: name.to_owned(),
                    container: subject,
                },
            }),
            // A stub types `$` on its class the way R dispatches it, through a `$.class` method.
            TyKind::Named(class, _)
                if let Some(ty) = self.subset_method_result("$", class.text(self.db)) =>
            {
                Ok(ty)
            }
            // Refuse rather than reject: `df$col` and `function(node) node$value` are idiomatic.
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(unknown(self.db))
            }
            _ => Err(TypeError {
                range,
                kind: TypeErrorKind::NotAList { found: subject },
            }),
        }
    }

    /// A branch whose value is a literal takes the ry type another branch has, as it would
    /// against an annotation, so `if (c) 0 else n` with `n: I64` is an `I64` and
    /// `:Err(_) => :Ok(0)` beside an `:Ok(I64)` arm is `:Ok(I64)`. Any other branch keeps its type.
    fn adapt_literal_branch(
        &mut self,
        branch: ExprId,
        branch_ty: Ty<'db>,
        other: Ty<'db>,
    ) -> Ty<'db> {
        let other = self.table.resolve(self.db, other);
        let resolved = self.table.resolve(self.db, branch_ty);
        self.decide_item_literals(Some(branch), resolved, other);
        // `:Ok(x) => x` with `x` the undecided item of `[1]`, beside a `7i64` arm, is decided
        // by that arm, as a use would decide it.
        if matches!(self.ry_number_class(resolved), Some(RyClass::Open(_)))
            && matches!(other.kind(self.db), TyKind::Native(native) if native.is_number())
            && self.table.unify(self.db, resolved, other).is_ok()
        {
            return other;
        }
        // An open ry number, such as the item of `[4L]` before anything decides it, takes the
        // literal as well.
        let ry_other = holds_ry_scalar(self.db, other)
            || matches!(self.ry_number_class(other), Some(RyClass::Open(_)));
        // A tag's payload literals are decided as it is inferred, so `:Step(0, a)` beside
        // `:Step(q, p)` adapts its `0` here, as a payload written only with literals does.
        let payload = matches!(resolved.kind(self.db), TyKind::Tag(_, Some(_)));
        if !ry_other || (holds_ry_scalar(self.db, resolved) && !payload) {
            return branch_ty;
        }
        match self.adapt_literal(branch, other) {
            LiteralFit::Fits => other,
            LiteralFit::OutOfRange | LiteralFit::NotLiteral => branch_ty,
        }
    }

    /// A tuple item or record field written with a literal, such as the `0` of
    /// `(0, a)` beside a branch giving `(q, p)` with `q: F64`, is decided by the ry type in the
    /// same place of the other branch, as a whole branch written with a literal is, and is R's
    /// type beside an R type. `branch` is the expression written there, when it is one.
    fn decide_item_literals(&mut self, branch: Option<ExprId>, branch_ty: Ty<'db>, other: Ty<'db>) {
        let module = self.module;
        let mut branch = branch;
        while let Some(ExpressionKind::Paren(inner)) = branch.map(|id| module.kind(id)) {
            branch = Some(*inner);
        }
        let branch_kind = branch.map(|id| module.kind(id));
        let mut pairs: SmallVec<[(Option<ExprId>, Ty<'db>, Ty<'db>); 4]> = SmallVec::new();
        match (branch_ty.kind(self.db), other.kind(self.db)) {
            (TyKind::NativeTuple(items), TyKind::NativeTuple(others))
                if items.len() == others.len() =>
            {
                let expressions = match branch_kind {
                    Some(ExpressionKind::Tuple(expressions))
                        if module[*expressions].len() == items.len() =>
                    {
                        &module[*expressions]
                    }
                    _ => &[],
                };
                for (index, (&item, &other_item)) in items.iter().zip(others.iter()).enumerate() {
                    pairs.push((expressions.get(index).copied(), item, other_item));
                }
            }
            (TyKind::NativeRecord(fields), TyKind::NativeRecord(others)) => {
                let expressions = match branch_kind {
                    Some(ExpressionKind::Record(expressions)) => &module[*expressions],
                    _ => &[],
                };
                for field in fields.iter() {
                    let Some(matching) = others.iter().find(|other| other.name == field.name)
                    else {
                        continue;
                    };
                    let expression = expressions
                        .iter()
                        .find(|written| Name::new(self.db, written.name) == field.name)
                        .map(|written| written.value);
                    pairs.push((expression, field.ty, matching.ty));
                }
            }
            _ => return,
        }
        for (expression, item, other_item) in pairs {
            let other_item = self.table.resolve(self.db, other_item);
            if !self.open_literal_var(item) {
                let item = self.table.resolve(self.db, item);
                self.decide_item_literals(expression, item, other_item);
                continue;
            }
            match other_item.kind(self.db) {
                TyKind::Native(_) => {
                    // A literal the type cannot hold, such as `2.5` beside an `I64`, is a
                    // mismatch at the literal, and the item stops there instead of repeating
                    // the mismatch at each use.
                    let fit = match expression {
                        Some(expression) => self.adapt_literal(expression, other_item),
                        None => LiteralFit::NotLiteral,
                    };
                    let decided = match fit {
                        LiteralFit::OutOfRange => unknown(self.db),
                        LiteralFit::Fits | LiteralFit::NotLiteral => other_item,
                    };
                    // A literal of another kind, such as `"a"` beside an `F64`, stays open for a
                    // use to decide.
                    if self.table.unify(self.db, item, decided).is_err() {
                        continue;
                    }
                }
                // Beside an R type, such as a `double` field, the literal is R's own type.
                TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_) => {
                    self.default_literal_var(item, false);
                }
                _ => {}
            }
        }
    }

    /// Differing openness joins by union, never unification: the concrete branch would
    /// pin the other (`function(flag, x) if (flag) x else "s"` would demand
    /// `x: character`). Two branches open in variables of their own join by union too, and
    /// only a callback's two open parameters tie.
    fn join_branch_values(&mut self, left: Ty<'db>, right: Ty<'db>) -> Ty<'db> {
        self.default_join_literals(left, right);
        let left_resolved = self.table.resolve(self.db, left);
        let right_resolved = self.table.resolve(self.db, right);
        // `Any` unifies with everything, so unifying would type the `if` as the other branch
        // and check the `Any` path's value against it; a union with `Any` is `Any`. A variable
        // written on two paths still joins by unification (`join_written_values`), so a later
        // use is checked against what the typed path established.
        if matches!(left_resolved.kind(self.db), TyKind::Any)
            || matches!(right_resolved.kind(self.db), TyKind::Any)
        {
            return any(self.db);
        }
        let left_open = self.table.contains_unbound_var(self.db, left_resolved);
        let right_open = self.table.contains_unbound_var(self.db, right_resolved);
        if left_open != right_open {
            let open = if left_open {
                left_resolved
            } else {
                right_resolved
            };
            // Only an unconstrained variable is protected: pinning an already-restricted one adds
            // no new requirement and lets recursion converge. One nested in a list is protected
            // too: sessioninfo's `if (identical(dp, FALSE)) list(character(), character()) else
            // list(dp, dp)` leaves `dp` open. A bounded one is protected from types outside its
            // bound's atomic scalars and vectors: mgcv's `if (is.null(L)) rho + rho0 else L %*% rho
            // + rho0` must not demand a matrix `rho`.
            let closed = if left_open {
                right_resolved
            } else {
                left_resolved
            };
            let atomic = matches!(closed.kind(self.db), TyKind::Scalar(_) | TyKind::Vector(_));
            let mut vars = FxHashSet::default();
            self.table.collect_unbound_vars(self.db, open, &mut vars);
            if vars.into_iter().any(|var| {
                match self
                    .table
                    .open_constraint(self.db, Ty::new(self.db, TyKind::Var(var)))
                {
                    Some(Constraint::Unconstrained) => true,
                    Some(_) => !atomic,
                    None => false,
                }
            }) {
                return union_of(self.db, [left_resolved, right_resolved]);
            }
        }
        // Two independent open values (`x %||% y`) are two values a caller chooses separately,
        // so tying them would make `default(FALSE)` wrong for a body that writes `x %||% list()`.
        // Only variables this function generalizes are the caller's, and a callback's
        // parameters (`Reduce(function(acc, x) if (x > acc) x else acc, xs)`) are the callee's.
        if left_open
            && right_open
            && left_resolved != right_resolved
            && self.callback_frame_level != Some(self.frame_level)
        {
            let level = self.table.level;
            let unconstrained = |ty: Ty<'db>, vars: &mut FxHashSet<_>| {
                self.table.collect_unbound_vars(self.db, ty, vars);
            };
            let mut left_vars = FxHashSet::default();
            let mut right_vars = FxHashSet::default();
            unconstrained(left_resolved, &mut left_vars);
            unconstrained(right_resolved, &mut right_vars);
            let free = |vars: &FxHashSet<_>, others: &FxHashSet<_>| {
                vars.iter().any(|var| {
                    !others.contains(var)
                        && matches!(
                            self.table.entry(self.table.find(*var)),
                            Entry::Unbound { level: var_level, .. } if *var_level >= level
                        )
                        && matches!(
                            self.table
                                .open_constraint(self.db, Ty::new(self.db, TyKind::Var(*var))),
                            Some(Constraint::Unconstrained)
                        )
                })
            };
            if free(&left_vars, &right_vars) && free(&right_vars, &left_vars) {
                return union_of(self.db, [left_resolved, right_resolved]);
            }
        }
        self.join_types(left, right)
    }

    /// A path that rebinds an open value (`if (is.null(repo)) repo <- getwd()`) must not
    /// pin the value the other path still holds, which is usually a parameter. Neither may
    /// a union keep that variable, since a later use of the rebound value (`repo + 1`)
    /// would then constrain the parameter, so the joined value is unchecked.
    fn join_written_values(&mut self, left: Ty<'db>, right: Ty<'db>) -> Ty<'db> {
        self.default_join_literals(left, right);
        let left_resolved = self.table.resolve(self.db, left);
        let right_resolved = self.table.resolve(self.db, right);
        // A rigid is the same open parameter written back as an annotation, so it joins the
        // same way. A bound such as `numeric` (from `omega + x` before the rebinding) does not
        // make the other path's value the parameter's type: `omega <- omega + m %*% m` on one
        // path must leave a caller free to pass a plain vector.
        let open_parameter = |ty: Ty<'db>| match ty.kind(self.db) {
            TyKind::Var(_) => self.table.open_constraint(self.db, ty).is_some(),
            TyKind::Rigid(_) => true,
            _ => false,
        };
        let (open, other) = match (
            open_parameter(left_resolved),
            open_parameter(right_resolved),
        ) {
            (true, false) => (left_resolved, right_resolved),
            (false, true) => (right_resolved, left_resolved),
            // Two different parameters (`x <- n` in a loop) stay independent.
            (true, true) if left_resolved != right_resolved => return unknown(self.db),
            _ => return self.join_types(left, right),
        };
        // A guard's own narrowing (`T` against `T | NULL`) and a `NULL` path keep the
        // parameter.
        let shared = match open.kind(self.db) {
            TyKind::Var(var) => {
                let mut vars = FxHashSet::default();
                self.table.collect_unbound_vars(self.db, other, &mut vars);
                vars.contains(var)
            }
            _ => match other.kind(self.db) {
                TyKind::Union(members) => members.contains(&open),
                _ => false,
            },
        };
        if shared || matches!(other.kind(self.db), TyKind::Null) {
            return self.join_types(left, right);
        }
        unknown(self.db)
    }

    /// Unifies when possible; a `NULL` side joins by union so it never binds a variable.
    fn join_types(&mut self, left: Ty<'db>, right: Ty<'db>) -> Ty<'db> {
        let left_resolved = self.table.resolve(self.db, left);
        let right_resolved = self.table.resolve(self.db, right);
        if matches!(left_resolved.kind(self.db), TyKind::Null)
            || matches!(right_resolved.kind(self.db), TyKind::Null)
        {
            return union_of(self.db, [left_resolved, right_resolved]);
        }
        // Two values a union bounds keep their own types: `if (k) a else b` over two such
        // parameters returns whichever the caller passed, not one type both must share.
        let bounded = |ty: Ty<'db>| {
            ty.as_var(self.db)
                .is_some_and(|var| self.table.bound_of(var).is_some())
        };
        if left_resolved != right_resolved && (bounded(left_resolved) || bounded(right_resolved)) {
            return union_of(self.db, [left_resolved, right_resolved]);
        }
        let snapshot = self.table.snapshot();
        match self.table.unify(self.db, left, right) {
            Ok(()) => left,
            Err(_) => {
                self.table.rollback(snapshot);
                self.wider_record(left_resolved, right_resolved)
                    .unwrap_or_else(|| union_of(self.db, [left_resolved, right_resolved]))
            }
        }
    }

    /// Of two records with the same field names, the one the other fits, so a branch that
    /// narrowed `cfg$limit` joins back into `cfg`'s own type rather than a union of both.
    fn wider_record(&mut self, left: Ty<'db>, right: Ty<'db>) -> Option<Ty<'db>> {
        let (TyKind::Record(left_fields), TyKind::Record(right_fields)) =
            (left.kind(self.db), right.kind(self.db))
        else {
            return None;
        };
        if left_fields.len() != right_fields.len()
            || left_fields
                .iter()
                .zip(right_fields.iter())
                .any(|(left_field, right_field)| left_field.name != right_field.name)
        {
            return None;
        }
        for (narrow, wide) in [(left, right), (right, left)] {
            let snapshot = self.table.snapshot();
            let fits = self.table.compatible(self.db, narrow, wide);
            self.table.rollback(snapshot);
            if fits {
                return Some(wide);
            }
        }
        None
    }

    /// A scheme instantiates; its fresh variables erase at the export edge like any residual.
    fn entry_monotype(&mut self, entry: EnvEntry<'db>) -> Ty<'db> {
        match entry {
            EnvEntry::Mono(ty) | EnvEntry::MissingFormal(ty) => ty,
            EnvEntry::Scheme(index) => {
                let scheme = self.scheme_arena[index as usize].clone();
                self.instantiate(&scheme)
            }
        }
    }

    /// Whether `slot` is a parameter that still holds a type nothing has constrained.
    fn is_open_formal(&self, slot: BindingId, ty: Ty<'db>) -> bool {
        self.naming
            .bindings
            .get(&slot)
            .is_some_and(|binding| binding.kind == BindingKind::Parameter)
            && matches!(
                self.table.open_constraint(self.db, ty),
                Some(Constraint::Unconstrained)
            )
    }

    /// Returns the slots whose entries changed: the loop fixed point's stability signal.
    fn join_writes(
        &mut self,
        writes: &[(BindingId, Option<EnvEntry<'db>>)],
        edge: JoinEdge,
    ) -> Vec<BindingId> {
        let mut changed = Vec::new();
        for &(slot, branch_entry) in writes {
            // The missing-formal marker is branch-local: once joined it is the supplied type.
            let branch_entry = match branch_entry {
                Some(EnvEntry::MissingFormal(ty)) => Some(EnvEntry::Mono(ty)),
                other => other,
            };
            let mut current = self.environment.get(slot);
            // The path that did not write a top-level slot holds the earlier project binding; a
            // name the project never bound keeps the written type (the read is maybe-undefined).
            if current.is_none() && branch_entry.is_some() {
                current = self.materialize_pre_state(slot, true).map(EnvEntry::Mono);
            }
            // Each loop pass writes fresh variables for its literals, so two writes that
            // decided them alike differ only before resolution.
            let same_resolved = match (current, branch_entry) {
                (Some(EnvEntry::Mono(a)), Some(EnvEntry::Mono(b))) if a != b => {
                    self.table.resolve(self.db, a) == self.table.resolve(self.db, b)
                }
                _ => false,
            };
            let joined = match (current, branch_entry) {
                (Some(entry), Some(_)) if same_resolved => Some(entry),
                (Some(EnvEntry::Mono(a)), Some(EnvEntry::Mono(b))) if a != b => {
                    self.default_join_literals(a, b);
                    let unknown_side = [a, b].into_iter().any(|ty| {
                        matches!(
                            self.table.resolve(self.db, ty).kind(self.db),
                            TyKind::Unknown
                        )
                    });
                    Some(EnvEntry::Mono(
                        if unknown_side && matches!(edge, JoinEdge::Branch) {
                            unknown(self.db)
                        } else {
                            self.join_written_values(a, b)
                        },
                    ))
                }
                (None, Some(entry)) => Some(entry),
                // Keeping identical entries as-is preserves a function slot's polymorphism.
                (Some(a), Some(b)) if a == b => Some(a),
                // `if (missing(f)) f <- function(n) ...` supplies a default: the formal keeps
                // its own open type, as it does under `function(f = function(n) ...)`, so the
                // literal's parameters do not pin to the first call's arguments.
                (Some(EnvEntry::Mono(formal)), Some(EnvEntry::Scheme(_)))
                | (Some(EnvEntry::Scheme(_)), Some(EnvEntry::Mono(formal)))
                    if self.missing_path_functions.remove(slot)
                        && self.is_open_formal(slot, formal) =>
                {
                    Some(EnvEntry::Mono(formal))
                }
                // A conditionally rewritten function slot is not polymorphic: its honest type is
                // the monotype union of what each path holds.
                (Some(a), Some(b)) => {
                    // Union, not `join_types`: two instantiated schemes always unify through their
                    // fresh variables, yielding a signature that describes neither path.
                    let (a, b) = (self.entry_monotype(a), self.entry_monotype(b));
                    let (a, b) = (
                        self.table.resolve(self.db, a),
                        self.table.resolve(self.db, b),
                    );
                    Some(EnvEntry::Mono(union_of(self.db, [a, b])))
                }
                (_, None) => None,
            };
            if let Some(entry) = joined
                && Some(entry) != current
            {
                self.environment.set(slot, entry);
                changed.push(slot);
            } else if self.environment.forget(slot) {
                changed.push(slot);
            }
        }
        changed
    }

    /// Quantifies unbound variables above the current level.
    fn generalize(&mut self, ty: Ty<'db>) -> TypeScheme<'db> {
        self.abstract_scheme(ty).0
    }

    fn abstract_scheme(
        &mut self,
        ty: Ty<'db>,
    ) -> (TypeScheme<'db>, FxHashMap<InferenceVar, Name<'db>>) {
        self.settle_lone_bounds(ty);
        let resolved = self.table.resolve(self.db, ty);
        let mut binders = Vec::new();
        let mut mapping: FxHashMap<InferenceVar, Name<'db>> = FxHashMap::default();
        let body = self.abstract_vars(resolved, &mut binders, &mut mapping);
        let scheme = TypeScheme {
            binders: ThinVec::from(binders),
            body,
        };
        (scheme, mapping)
    }

    /// Generalizes the item's own value and keeps which variable became which binder.
    /// A recorded type as the editor shows it: resolved, with each variable the item's
    /// signature generalized named by its binder. `memo` spans one item's types, which share
    /// subterms.
    fn editor_type(&self, ty: Ty<'db>, memo: &mut FxHashMap<Ty<'db>, Ty<'db>>) -> Ty<'db> {
        let resolved = self.table.resolve(self.db, ty);
        if self.root_binders.is_empty() || !contains_inference_var(self.db, resolved) {
            return resolved;
        }
        self.name_root_binders(resolved, memo)
    }

    fn name_root_binders(&self, ty: Ty<'db>, memo: &mut FxHashMap<Ty<'db>, Ty<'db>>) -> Ty<'db> {
        if !contains_inference_var(self.db, ty) {
            return ty;
        }
        if let Some(&named) = memo.get(&ty) {
            return named;
        }
        let named = match *ty.kind(self.db) {
            TyKind::Var(var) => match self.root_binders.get(&self.table.find(var)) {
                Some(&name) => Ty::new(self.db, TyKind::Rigid(name)),
                None => ty,
            },
            _ => map_child_types(self.db, ty, &mut |child| {
                self.name_root_binders(child, memo)
            }),
        };
        memo.insert(ty, named);
        named
    }

    fn generalize_root(&mut self, ty: Ty<'db>) -> TypeScheme<'db> {
        let (scheme, mapping) = self.abstract_scheme(ty);
        self.root_binders = mapping;
        scheme
    }

    fn abstract_vars(
        &mut self,
        ty: Ty<'db>,
        binders: &mut Vec<Binder<'db>>,
        mapping: &mut FxHashMap<InferenceVar, Name<'db>>,
    ) -> Ty<'db> {
        let TyKind::Var(var) = *ty.kind(self.db) else {
            return map_child_types(self.db, ty, &mut |child| {
                self.abstract_vars(child, binders, mapping)
            });
        };
        let representative = self.table.find(var);
        if let Some(&name) = mapping.get(&representative) {
            return Ty::new(self.db, TyKind::Rigid(name));
        }
        let Entry::Unbound {
            level,
            constraint,
            bound,
        } = *self.table.entry(representative)
        else {
            return ty;
        };
        if level <= self.table.level {
            // Escaped below the binding level: stays monomorphic.
            return ty;
        }
        if let Some((literal, native)) = self.table.literal_of(representative) {
            self.default_literal_var(Ty::new(self.db, TyKind::Var(literal)), native);
            return self.table.resolve(self.db, ty);
        }
        // The bound's own variables are binders first, so `<T, U: list[T] | T[]>` names
        // `T` before the bound reads it.
        let bound = bound.map(|bound| {
            let resolved = self.table.resolve(self.db, bound);
            self.abstract_vars(resolved, binders, mapping)
        });
        // A generated name is no identifier, so a message letters it by position.
        let name = Name::new(self.db, format!("#{}", binders.len()));
        mapping.insert(representative, name);
        binders.push(Binder {
            name,
            bound: match bound {
                Some(bound) => Bound::Type(bound),
                None => Bound::Constraint(constraint),
            },
        });
        Ty::new(self.db, TyKind::Rigid(name))
    }

    /// A bounded variable earns a binder only where it ties two places of a signature together
    /// (a parameter and the return, or two parameters); anywhere else it is its bound, which
    /// reads better than a binder that says nothing more.
    fn settle_lone_bounds(&mut self, ty: Ty<'db>) {
        let resolved = self.table.resolve(self.db, ty);
        let slots: Vec<Ty<'db>> = match resolved.kind(self.db) {
            TyKind::Function(function) => function
                .positional
                .iter()
                .copied()
                .chain(
                    function
                        .named
                        .iter()
                        .flat_map(|parameter| parameter.types()),
                )
                .chain(function.variadic.iter().map(|rest| rest.element))
                .chain([function.ret])
                .collect(),
            _ => vec![resolved],
        };
        let mut seen = FxHashSet::default();
        let mut tied = FxHashSet::default();
        for slot in slots {
            let mut vars = FxHashSet::default();
            self.table.collect_unbound_vars(self.db, slot, &mut vars);
            for var in vars {
                if !seen.insert(var) {
                    tied.insert(var);
                }
            }
        }
        let mut lone: Vec<InferenceVar> =
            seen.into_iter().filter(|var| !tied.contains(var)).collect();
        lone.sort_unstable_by_key(|var| var.0);
        for var in lone {
            if let Entry::Unbound {
                level,
                bound: Some(_),
                ..
            } = *self.table.entry(var)
                && level > self.table.level
            {
                let snapshot = self.table.snapshot();
                if self.table.settle_bound(self.db, var).is_err() {
                    // A bounded variable carries no constraint and no cycle, so this does not
                    // happen; if it did, keeping the binder still gives a correct scheme.
                    self.table.rollback(snapshot);
                }
            }
        }
    }

    /// The candidates instantiated only when an argument can use their parameter types.
    fn hinting_callees(
        &mut self,
        schemes: &[TypeScheme<'db>],
        arguments: &[Argument],
    ) -> Vec<Ty<'db>> {
        let hintable = arguments.iter().any(|argument| {
            argument
                .value
                .is_some_and(|value| self.needs_parameter_hints(value))
        });
        if !hintable {
            return Vec::new();
        }
        schemes
            .iter()
            .map(|scheme| self.instantiate(scheme))
            .collect()
    }

    fn instantiate(&mut self, scheme: &TypeScheme<'db>) -> Ty<'db> {
        let mut substitution = BumpVec::with_capacity_in(scheme.binders.len(), self.arena);
        for binder in &scheme.binders {
            let fresh = self.fresh(binder.constraint());
            substitution.push((binder.name, fresh));
        }
        for (binder, &(_, fresh)) in scheme.binders.iter().zip(substitution.iter()) {
            if let Some(bound) = binder.type_bound()
                && let Some(var) = fresh.as_var(self.db)
            {
                let bound = substitute_rigid(self.db, bound, &substitution);
                self.table.set_bound(var, bound);
            }
        }
        substitute_rigid(self.db, scheme.body, &substitution)
    }
}

/// The S3 method `generic.class`, spelled in a per-thread buffer: operator, subset and
/// dispatch checks probe several of these per expression, and a fresh string for each probe
/// was one of the checker's largest allocation sources.
fn method_symbol(generic: &str, class: &str) -> Symbol {
    thread_local! {
        static SPELLING: RefCell<String> = const { RefCell::new(String::new()) };
    }
    SPELLING.with_borrow_mut(|spelling| {
        spelling.clear();
        spelling.push_str(generic);
        spelling.push('.');
        spelling.push_str(class);
        Symbol::new(spelling)
    })
}

/// The one member of a union `select` accepts; `None` when none or several do.
fn unique_member<'db, T>(
    db: &'db dyn Db,
    members: &[Ty<'db>],
    select: impl Fn(&TyKind<'db>) -> Option<T>,
) -> Option<T> {
    single(
        members
            .iter()
            .filter_map(|member| select(member.kind(db)))
            .collect(),
    )
}

fn single<T>(items: Vec<T>) -> Option<T> {
    let mut items = items.into_iter();
    let first = items.next()?;
    items.next().is_none().then_some(first)
}

/// The type an atomic vector `prior` has after an element or slice write of the atomic `value`.
/// R coerces the whole vector when the written value has a wider type, so
/// `prob <- rep(NA, n); prob[i] <- 0.5` and `x <- 1:3; x[[2]] <- 2.5` both leave a `double[]`.
/// `None` when either is not atomic.
fn atomic_write<'db>(db: &'db dyn Db, prior: Ty<'db>, value: Ty<'db>) -> Option<Ty<'db>> {
    let container = combine_operand_atomic(db, prior)?;
    let written = combine_operand_atomic(db, value)?;
    let element = scalar(db, promote_combine_atomic(container, written));
    // A type carries no length, so a scalar stays a scalar as it does after `x[1] <- 2L`.
    Some(match prior.kind(db) {
        TyKind::Scalar(_) => element,
        TyKind::NamedVector(_) => Ty::new(db, TyKind::NamedVector(element)),
        _ => Ty::new(db, TyKind::Vector(element)),
    })
}

fn combine_operand_atomic<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Atomic> {
    match ty.kind(db) {
        TyKind::Scalar(atomic) => Some(*atomic),
        // `F64`, `Bool` and `Str` are R's `double`, `logical` and `character` at run time.
        TyKind::Native(native) => native.shared_atomic(),
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Scalar(atomic) => Some(*atomic),
            _ => None,
        },
        _ => None,
    }
}

/// R's coercion order for `c()`, lowest first: `c(as.raw(1), 1L)` is an integer vector.
fn promote_combine_atomic(left: Atomic, right: Atomic) -> Atomic {
    let rank = |atomic: Atomic| match atomic {
        Atomic::Raw => 0u8,
        Atomic::Logical => 1,
        Atomic::Integer => 2,
        Atomic::Double => 3,
        Atomic::Complex => 4,
        Atomic::Character => 5,
    };
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

fn union_field_names<'db>(db: &'db dyn Db, subject: Ty<'db>) -> Vec<String> {
    let mut names = Vec::new();
    let mut collect = |ty: Ty<'db>| {
        if let TyKind::Record(fields) = ty.kind(db) {
            names.extend(fields.iter().map(|field| field.name.text(db).to_owned()));
        }
    };
    match subject.kind(db) {
        TyKind::Union(members) => members.iter().for_each(|&member| collect(member)),
        _ => collect(subject),
    }
    names
}

/// A failing union member reports the full union, not the member that failed.
fn widen_error_container<'db>(mut error: TypeError<'db>, union: Ty<'db>) -> TypeError<'db> {
    match &mut error.kind {
        TypeErrorKind::NotAList { found }
        | TypeErrorKind::UnsupportedSubset { found }
        | TypeErrorKind::DollarOnAtomicVector { found } => *found = union,
        TypeErrorKind::PositionDoesNotExist { container, .. }
        | TypeErrorKind::FieldDoesNotExist { container, .. } => *container = union,
        _ => {}
    }
    error
}

/// A column error raised without the frame's variable (by `[[` or `$`) names it here.
fn name_frame<'db>(mut error: TypeError<'db>, module: &Module, target: ExprId) -> TypeError<'db> {
    if let TypeErrorKind::ColumnDoesNotExist { frame, .. } = &mut error.kind {
        *frame = variable_name(module, target);
    }
    error
}

fn column_does_not_exist<'db>(
    db: &'db dyn Db,
    column: &str,
    frame: Option<Symbol>,
    columns: &[RecordField<'db>],
) -> TypeErrorKind<'db> {
    let names: Vec<String> = columns
        .iter()
        .map(|candidate| candidate.name.text(db).to_owned())
        .collect();
    TypeErrorKind::ColumnDoesNotExist {
        suggestion: nearest_name(column, names.iter().map(String::as_str)),
        column: column.to_owned(),
        frame,
        columns: names,
    }
}

/// Whether writing `value` into a column of type `column` keeps the column's type but loses
/// information, as data.table converts it: strings into a logical or numeric column are parsed,
/// and a fractional number written into an integer column is truncated. Only a literal number is
/// known to be fractional.
fn lossy_write<'db>(
    db: &'db dyn Db,
    module: &Module,
    column: Ty<'db>,
    value_ty: Ty<'db>,
    value: ExprId,
) -> bool {
    let atomic = |ty: Ty<'db>| match column_element(db, ty).kind(db) {
        TyKind::Scalar(atomic) => Some(*atomic),
        _ => None,
    };
    let factor_column = matches!(
        column_element(db, column).kind(db),
        TyKind::Named(name, _) if name.text(db) == "factor"
    );
    if factor_column {
        return matches!(
            atomic(value_ty),
            Some(Atomic::Logical | Atomic::Integer | Atomic::Double)
        );
    }
    match (atomic(column), atomic(value_ty)) {
        (Some(Atomic::Logical | Atomic::Integer | Atomic::Double), Some(Atomic::Character)) => true,
        (Some(Atomic::Integer), Some(Atomic::Double)) => {
            let mut literal = value;
            loop {
                match module.kind(literal) {
                    ExpressionKind::Unary {
                        operator: UnaryOperator::Minus,
                        operand,
                    } => literal = *operand,
                    ExpressionKind::Paren(inner) => literal = *inner,
                    _ => break,
                }
            }
            matches!(
                module.kind(literal),
                ExpressionKind::Literal(LiteralKind::Double(text))
                    if module[text].parse::<f64>().is_ok_and(|number| number.fract() != 0.0)
            )
        }
        _ => false,
    }
}

/// The column name data.table gives an unnamed item of `j` that is a bare name: the name
/// itself, except `.N`, which becomes `N`.
fn count_column_name(name: Symbol) -> Symbol {
    if name == ".N" {
        Symbol::from("N".to_owned())
    } else {
        name
    }
}

fn variable_name(module: &Module, id: ExprId) -> Option<Symbol> {
    match module.kind(id) {
        ExpressionKind::NameRef(name) => Some(*name),
        _ => None,
    }
}

/// The column `Expr.col("name")` reads, looking through parentheses.
fn column_read(module: &Module, mut id: ExprId) -> Option<String> {
    while let ExpressionKind::Paren(inner) = module.kind(id) {
        id = *inner;
    }
    let ExpressionKind::Call { callee, arguments } = module.kind(id) else {
        return None;
    };
    if !matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == "Expr.col") {
        return None;
    }
    let first = module[arguments].first()?.value?;
    match module.kind(first) {
        ExpressionKind::Literal(LiteralKind::String(text)) => Some(module[text].to_string()),
        _ => None,
    }
}

/// Whether an expression is a literal, possibly negated: certainly one value.
fn is_literal(module: &Module, id: ExprId) -> bool {
    match module.kind(id) {
        ExpressionKind::Literal(_) => true,
        ExpressionKind::Unary {
            operator: UnaryOperator::Minus,
            operand,
        } => is_literal(module, *operand),
        _ => false,
    }
}

fn frame_columns<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<&'db [RecordField<'db>]> {
    match ty.kind(db) {
        TyKind::Frame(_, columns) => *columns,
        _ => None,
    }
}

/// `None` unless every argument is an unnamed string literal.
/// `lapply`'s `X` and `FUN` arguments: by name, or else the first and second positional ones.
/// The arguments of a call to an R function with these `formals`, bound as R binds them:
/// exact names first, then the unnamed arguments in order.
fn bind_formals(module: &Module, arguments: &[Argument], formals: &[&str]) -> BoundFormals {
    let mut values: Vec<Option<ExprId>> = formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .find(|argument| module.argument_name(argument).as_deref() == Some(*formal))
                .and_then(|argument| argument.value)
        })
        .collect();
    let mut positional = arguments
        .iter()
        .filter(|argument| argument.name.is_none())
        .filter_map(|argument| argument.value);
    for slot in values.iter_mut() {
        if slot.is_none() {
            *slot = positional.next();
        }
    }
    BoundFormals { values }
}

struct BoundFormals {
    values: Vec<Option<ExprId>>,
}

impl BoundFormals {
    fn value(&self, position: usize) -> Option<ExprId> {
        self.values.get(position).copied().flatten()
    }
}

/// An overloaded function passed by name as a callback.
struct Callback<'db> {
    function: ExprId,
    name: String,
    schemes: &'db [TypeScheme<'db>],
}

/// The types an apply-family function hands its callback one at a time, and whether the
/// result keeps `x`'s names.
fn element_types<'db>(db: &'db dyn Db, x: Ty<'db>) -> (Vec<Ty<'db>>, bool) {
    match x.kind(db) {
        TyKind::Vector(element) | TyKind::List(element) => (vec![*element], false),
        TyKind::NamedVector(element) | TyKind::NamedList(element) => (vec![*element], true),
        TyKind::Tuple(items) => (items.to_vec(), false),
        TyKind::Record(fields) | TyKind::Frame(_, Some(fields)) => {
            (fields.iter().map(|field| field.ty).collect(), true)
        }
        TyKind::Scalar(_) => (vec![x], false),
        _ => (vec![unknown(db)], false),
    }
}

/// A logical flag that only a name can pass, as `sapply`'s `simplify` after `...`: `Some(true)`
/// when it is left out or literally `TRUE`, `Some(false)` when literally `FALSE`, and `None` when
/// its value is not known.
fn simplify_flag(module: &Module, arguments: &[Argument], name: &str) -> Option<bool> {
    let Some(argument) = arguments
        .iter()
        .find(|argument| module.argument_name(argument).as_deref() == Some(name))
    else {
        return Some(true);
    };
    match argument.value.map(|value| module.kind(value)) {
        Some(ExpressionKind::Literal(LiteralKind::Logical(value))) => Some(*value),
        _ => None,
    }
}

/// The only write to a binding, when its value is literal strings.
#[derive(Clone, Copy, PartialEq, Eq)]
struct StringWrite {
    target: ExprId,
    value: ExprId,
}

/// The strings of `"a"` or `c("a", "b")`, each with its literal's range.
fn literal_strings(module: &Module, value: ExprId) -> Option<Vec<(String, TextRange)>> {
    let expression = value;
    match module.kind(expression) {
        ExpressionKind::Literal(LiteralKind::String(text)) => {
            Some(vec![(module[text].to_string(), module.range(expression))])
        }
        ExpressionKind::Call { callee, arguments }
            if variable_name(module, *callee).as_deref() == Some("c") =>
        {
            module[arguments]
                .iter()
                .map(|argument| {
                    let item = argument.value.filter(|_| argument.name.is_none())?;
                    let expression = item;
                    match module.kind(expression) {
                        ExpressionKind::Literal(LiteralKind::String(text)) => {
                            Some((module[text].to_string(), module.range(expression)))
                        }
                        _ => None,
                    }
                })
                .collect()
        }
        _ => None,
    }
}

/// The data.table functions whose results the checker sharpens from their arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TableFunction {
    /// `data.frame(...)` or `data.table(...)`.
    Construct(FrameKind),
    /// `setDT(f)` or `setDF(f)`, which change the class by reference and keep the columns.
    ConvertInPlace(FrameKind),
    /// `set(f, ...)` or `setattr(f, ...)`.
    SetInPlace,
    Fread,
    SetNames,
    SetColumnOrder,
    SetKey,
    SetOrder,
    SetKeyOrOrderNamed,
    RbindList,
    Melt,
    Fifelse,
}

/// Each formal's argument, matched as R matches them: exact names first, then the unnamed
/// arguments fill the remaining formals in order.
fn match_formals(module: &Module, arguments: &[Argument], formals: &[&str]) -> Vec<Option<ExprId>> {
    let mut matched: Vec<Option<ExprId>> = formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .find(|argument| module.argument_name(argument).as_deref() == Some(*formal))
                .and_then(|argument| argument.value)
        })
        .collect();
    let named: Vec<bool> = formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .any(|argument| module.argument_name(argument).as_deref() == Some(*formal))
        })
        .collect();
    let mut open = (0..formals.len()).filter(|&position| !named[position]);
    for argument in arguments.iter().filter(|argument| argument.name.is_none()) {
        let Some(position) = open.next() else {
            break;
        };
        if let Some(slot) = matched.get_mut(position) {
            *slot = argument.value;
        }
    }
    matched
}

/// A string literal's value with R's common escapes decoded, since the syntax tree keeps
/// them as written. `None` for an escape this does not decode (`\x`, `\u` and the like).
fn decode_escapes(text: &str) -> Option<String> {
    let mut decoded = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        decoded.push(match characters.next()? {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            escaped @ ('\\' | '"' | '\'') => escaped,
            _ => return None,
        });
    }
    Some(decoded)
}

/// The first column a verb's arguments after the subject name, as a string or a column read.
fn first_named_column(module: &Module, rest: &[Argument]) -> Option<String> {
    for argument in rest {
        let Some(value) = argument.value else {
            continue;
        };
        if module
            .argument_name(argument)
            .as_deref()
            .is_some_and(|name| matches!(name, "how" | "suffix" | "strategy" | "descending" | "n"))
        {
            continue;
        }
        let column = match module.kind(value) {
            ExpressionKind::Literal(LiteralKind::String(text)) => Some(module[text].to_string()),
            _ => first_column_read(module, value),
        };
        if column.is_some() {
            return column;
        }
    }
    None
}

/// The first column an expression reads with `Expr.col("name")`, in source order.
fn first_column_read(module: &Module, value: ExprId) -> Option<String> {
    if let ExpressionKind::Call { callee, arguments } = module.kind(value)
        && matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == "Expr.col")
        && let Some(first) = module[arguments].first().and_then(|first| first.value)
        && let ExpressionKind::Literal(LiteralKind::String(text)) = module.kind(first)
    {
        return Some(module[text].to_string());
    }
    let mut found = None;
    module.for_each_child(value, |child| {
        if found.is_none() {
            found = first_column_read(module, child);
        }
    });
    found
}

/// A sniffed file's columns, each the vector type of its sniffed kind, and text as a factor when
/// `factors` asks for it, as the readers do.
fn sniffed_columns<'db>(
    db: &'db dyn Db,
    layout: &sniff::Layout,
    factors: bool,
) -> Vec<(String, Ty<'db>)> {
    layout
        .columns
        .iter()
        .map(|column| {
            let atomic = match column.kind {
                sniff::ColumnType::Logical => Atomic::Logical,
                sniff::ColumnType::Integer => Atomic::Integer,
                sniff::ColumnType::Double => Atomic::Double,
                sniff::ColumnType::Character => Atomic::Character,
            };
            let ty = if factors && atomic == Atomic::Character {
                factor_type(db)
            } else {
                Ty::new(db, TyKind::Vector(scalar(db, atomic)))
            };
            (column.name.clone(), ty)
        })
        .collect()
}

/// A frame from named columns, or a frame with unknown columns when two share a name.
fn frame_of<'db>(db: &'db dyn Db, kind: FrameKind, columns: Vec<(String, Ty<'db>)>) -> Ty<'db> {
    let mut fields: Vec<RecordField<'db>> = Vec::with_capacity(columns.len());
    for (name, ty) in columns {
        if fields.iter().any(|field| field.name.text(db) == name) {
            return Ty::new(db, TyKind::Frame(kind, None));
        }
        fields.push(RecordField {
            name: Name::new(db, name),
            ty: column_type(db, ty),
            optional: false,
        });
    }
    Ty::new(db, TyKind::Frame(kind, Some(&fields)))
}

/// The element type of a vector type.
fn element_atomic(db: &dyn Db, ty: Ty<'_>) -> Option<Atomic> {
    match ty.kind(db) {
        TyKind::Vector(element) | TyKind::NamedVector(element) => element_atomic(db, *element),
        TyKind::Scalar(atomic) => Some(*atomic),
        _ => None,
    }
}

fn is_date_column(db: &dyn Db, ty: Ty<'_>) -> bool {
    matches!(column_element(db, ty).kind(db), TyKind::Named(name, _) if name.text(db) == "Date")
}

fn is_expr_type(db: &dyn Db, ty: Ty<'_>) -> bool {
    matches!(ty.kind(db), TyKind::Named(name, _) if name.text(db) == "Expr")
}

fn factor_type(db: &dyn Db) -> Ty<'_> {
    Ty::new(db, TyKind::Named(Name::new(db, "factor"), &[]))
}

/// The column type data.table gives values of several column types stacked together: the
/// widest of logical, integer, double and character. A factor stays a factor when the others
/// are factors or strings and `keep_factors` holds (rbindlist unions their levels); otherwise
/// it counts as its labels (melt's value column). `None` for a type outside that ladder.
fn widest_column<'db>(db: &'db dyn Db, types: &[Ty<'db>], keep_factors: bool) -> Option<Ty<'db>> {
    let rank = |atomic: Atomic| match atomic {
        Atomic::Logical => Some(0),
        Atomic::Integer => Some(1),
        Atomic::Double => Some(2),
        Atomic::Character => Some(3),
        Atomic::Complex | Atomic::Raw => None,
    };
    let mut widest: Option<Atomic> = None;
    let mut factors = false;
    for &ty in types {
        let atomic = match column_element(db, ty).kind(db) {
            TyKind::Scalar(atomic) => *atomic,
            TyKind::Named(name, _) if name.text(db) == "factor" => {
                factors = true;
                Atomic::Character
            }
            _ => return None,
        };
        rank(atomic)?;
        if widest.is_none_or(|seen| rank(seen) < rank(atomic)) {
            widest = Some(atomic);
        }
    }
    let widest = widest?;
    if factors && keep_factors {
        return (widest == Atomic::Character).then(|| factor_type(db));
    }
    Some(Ty::new(db, TyKind::Vector(scalar(db, widest))))
}

fn string_literals(module: &Module, arguments: &[Argument]) -> Option<Vec<String>> {
    arguments
        .iter()
        .map(
            |argument| match argument.value.map(|value| module.kind(value)) {
                Some(ExpressionKind::Literal(LiteralKind::String(text)))
                    if argument.name.is_none() =>
                {
                    Some(module[text].to_string())
                }
                _ => None,
            },
        )
        .collect()
}

fn collect_callees(module: &Module, id: ExprId, callees: &mut SmallVec<[ExprId; 8]>) {
    let kind = module.kind(id);
    if let ExpressionKind::Call { callee, .. } = kind
        && matches!(module.kind(*callee), ExpressionKind::NameRef(_))
    {
        callees.push(*callee);
    }
    module.for_each_child(id, |child| collect_callees(module, child, callees));
}

/// A row index filters (logical) or picks positions (numeric). A type ry cannot pin
/// down passes, since only a known wrong type is reported.
fn is_row_index<'db>(db: &'db dyn Db, ty: Ty<'db>) -> bool {
    match ty.kind(db) {
        TyKind::Scalar(atomic) => {
            matches!(atomic, Atomic::Logical | Atomic::Integer | Atomic::Double)
        }
        TyKind::Native(native) => native.is_integer() || *native == NativeScalar::Bool,
        TyKind::Vector(element) | TyKind::NamedVector(element) => is_row_index(db, *element),
        TyKind::Union(members) => members.iter().all(|&member| is_row_index(db, member)),
        TyKind::Null
        | TyKind::Unknown
        | TyKind::Any
        | TyKind::Var(_)
        | TyKind::Rigid(_)
        | TyKind::Named(..) => true,
        TyKind::List(_)
        | TyKind::NamedList(_)
        | TyKind::Tuple(_)
        | TyKind::Record(_)
        | TyKind::OpenRecord(_)
        | TyKind::Environment(_)
        | TyKind::NativeTuple(_)
        | TyKind::NativeRecord(_)
        | TyKind::Tag(..)
        | TyKind::OtherTags
        | TyKind::Frame(..)
        | TyKind::Function(_) => false,
    }
}

/// The `on =` forms data.table documents: a string of comma-separated conditions (`"id"`,
/// `"a==b"`, `"t>=lo,t<hi"`), `c()` of such strings or of `x = "y"` pairs, and `.()` or
/// `list()` of names, `x = y` pairs and comparisons (`t >= lo`). `None` for anything else,
/// such as a variable holding the keys.
fn join_keys(module: &Module, on: ExprId) -> Option<Vec<JoinKey>> {
    let expression = on;
    match module.kind(expression) {
        ExpressionKind::Literal(LiteralKind::String(text)) => {
            split_join_keys(&module[text], module.range(expression))
        }
        ExpressionKind::Call { callee, arguments } => {
            let callee_name = variable_name(module, *callee)?;
            let mut keys = Vec::new();
            for argument in &module[arguments] {
                let value = argument.value?;
                let value_range = module.range(value);
                let x_range = module.argument_name_range(argument).unwrap_or(value_range);
                match (callee_name.as_str(), module.kind(value)) {
                    ("c", ExpressionKind::Literal(LiteralKind::String(text))) => {
                        let text = &module[*text];
                        match module.argument_name(argument) {
                            Some(_) if !is_join_operator_free(text) => return None,
                            Some(x) => keys.push(JoinKey {
                                x: x.to_string(),
                                y: text.trim().to_owned(),
                                operator: JoinOperator::Equal,
                                x_range,
                                y_range: value_range,
                            }),
                            None => keys.extend(split_join_keys(text, value_range)?),
                        }
                    }
                    ("." | "list", ExpressionKind::NameRef(y)) => keys.push(JoinKey {
                        x: module
                            .argument_name(argument)
                            .map_or_else(|| y.to_string(), |name| name.to_string()),
                        y: y.to_string(),
                        operator: JoinOperator::Equal,
                        x_range,
                        y_range: value_range,
                    }),
                    (
                        "." | "list",
                        ExpressionKind::Binary {
                            operator, lhs, rhs, ..
                        },
                    ) if argument.name.is_none() => {
                        let operator = match operator {
                            BinaryOperator::Equal => JoinOperator::Equal,
                            BinaryOperator::Less => JoinOperator::Less,
                            BinaryOperator::LessEq => JoinOperator::LessEqual,
                            BinaryOperator::Greater => JoinOperator::Greater,
                            BinaryOperator::GreaterEq => JoinOperator::GreaterEqual,
                            _ => return None,
                        };
                        keys.push(JoinKey {
                            x: variable_name(module, *lhs)?.to_string(),
                            y: variable_name(module, *rhs)?.to_string(),
                            operator,
                            x_range: module.range(*lhs),
                            y_range: module.range(*rhs),
                        });
                    }
                    _ => return None,
                }
            }
            (!keys.is_empty()).then_some(keys)
        }
        _ => None,
    }
}

/// The conditions of one `on` string. When the literal's source is its text between two quotes,
/// each column gets its own range inside the literal; a raw string, whose source is longer, gives
/// every column the literal's range.
fn split_join_keys(text: &str, range: TextRange) -> Option<Vec<JoinKey>> {
    let verbatim = usize::from(range.len()) == text.len() + 2;
    let range_of = |start: usize, part: &str| {
        let offset = u32::try_from(start)
            .ok()
            .filter(|_| verbatim && !part.is_empty());
        let length = u32::try_from(part.len()).ok();
        match (offset, length) {
            (Some(offset), Some(length)) => TextRange::at(
                range.start() + TextSize::from(1 + offset),
                TextSize::from(length),
            ),
            _ => range,
        }
    };
    let mut keys = Vec::new();
    let mut part_start = 0;
    for part in text.split(',') {
        let operator_at = part.find(['<', '>', '=', '!']);
        let (x_part, operator, y_part, y_start) = match operator_at {
            None => (part, JoinOperator::Equal, part, 0),
            Some(at) => {
                let rest = part.get(at..)?;
                let (operator, width) = [
                    ("==", JoinOperator::Equal),
                    ("<=", JoinOperator::LessEqual),
                    (">=", JoinOperator::GreaterEqual),
                    ("<", JoinOperator::Less),
                    (">", JoinOperator::Greater),
                ]
                .into_iter()
                .find(|(symbol, _)| rest.starts_with(symbol))
                .map(|(symbol, operator)| (operator, symbol.len()))?;
                (
                    part.get(..at)?,
                    operator,
                    part.get(at + width..)?,
                    at + width,
                )
            }
        };
        let (x, y) = (x_part.trim(), y_part.trim());
        if x.is_empty() || y.is_empty() || !is_join_operator_free(x) || !is_join_operator_free(y) {
            return None;
        }
        let x_start = part_start + (x_part.len() - x_part.trim_start().len());
        let y_start = part_start + y_start + (y_part.len() - y_part.trim_start().len());
        keys.push(JoinKey {
            x: x.to_owned(),
            y: y.to_owned(),
            operator,
            x_range: range_of(x_start, x),
            y_range: range_of(y_start, y),
        });
        part_start += part.len() + 1;
    }
    Some(keys)
}

fn is_join_operator_free(text: &str) -> bool {
    !text.contains(['<', '>', '=', '!'])
}

/// The scalar a column holds per row: the element of a vector type.
pub(crate) fn column_element<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    match ty.kind(db) {
        TyKind::Vector(element) | TyKind::NamedVector(element) => *element,
        _ => ty,
    }
}

/// What a join compares a column's values as. Columns of different classes cannot be joined,
/// as data.table and the engine refuse them; a factor is typed `Any`, so it has no class.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JoinClass {
    Logical,
    Number,
    Text,
}

fn join_class<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<JoinClass> {
    match column_element(db, ty).kind(db) {
        TyKind::Scalar(Atomic::Logical) => Some(JoinClass::Logical),
        TyKind::Scalar(Atomic::Integer | Atomic::Double) => Some(JoinClass::Number),
        TyKind::Scalar(Atomic::Character) => Some(JoinClass::Text),
        // data.table joins a factor on its labels, so it pairs with text and nothing else.
        TyKind::Named(name, _) if name.text(db) == "factor" => Some(JoinClass::Text),
        _ => None,
    }
}

/// Decided purely by syntax: the class survives when `j` (second positional or
/// `j =`) is absent, under `by`/`keyby`, or is a `:=`, `.()`, or `list()` call; any
/// other `j` needs column knowledge.
fn data_table_keeps_class(module: &Module, arguments: &[Argument]) -> bool {
    let mut positional = arguments.iter().filter(|argument| argument.name.is_none());
    let j = arguments
        .iter()
        .find(|argument| module.argument_name(argument).as_deref() == Some("j"))
        .or_else(|| {
            let _i = positional.next();
            positional.next()
        });
    let Some(value) = j.and_then(|argument| argument.value) else {
        return true;
    };
    if arguments.iter().any(|argument| {
        matches!(
            module.argument_name(argument).as_deref(),
            Some("by") | Some("keyby")
        )
    }) {
        return true;
    }
    matches!(
        module.kind(value),
        ExpressionKind::Call { callee, .. }
            if matches!(
                module.kind(*callee),
                ExpressionKind::NameRef(name) if matches!(name.as_str(), ":=" | "." | "list")
            )
    )
}

/// An annotation that names an undeclared type or a non-atomic `X[]` element carries no
/// payload, like a refused block: the diagnostics report the mistake once, and a call is never
/// checked against a type the author did not mean.
fn refused_by_vocabulary<'db>(
    db: &'db dyn Db,
    globals: Option<&dyn GlobalEnv<'db>>,
    annotation: &Annotation<'db>,
) -> bool {
    if annotation.nominal_references.is_empty() && annotation.vector_elements.is_empty() {
        return false;
    }
    let Some(globals) = globals else {
        return false;
    };
    let no_nominals = SymbolSet::default();
    let stub_nominals = stubs(db).map_or(&no_nominals, |library| &library.nominals);
    annotation_misuses_vocabulary(db, annotation, globals.type_definitions(), stub_nominals)
}

/// A signature's parameter types in the order a call binds positional arguments.
fn parameter_types<'db>(function: &FunctionType<'db>) -> SmallVec<[Ty<'db>; 4]> {
    function
        .positional
        .iter()
        .copied()
        .chain(function.named.iter().map(|parameter| parameter.ty))
        .collect()
}

/// A DFS gray set: a name is expanding only while its own body is walked, so a
/// diamond is not a cycle. Nominal definitions may legitimately recurse.
pub(crate) fn alias_cycle_walk<'db>(
    db: &'db dyn Db,
    definitions: TypeDefinitions<'db>,
    ty: Ty<'db>,
    expanding: &mut FxHashSet<Name<'db>>,
) -> Option<String> {
    match ty.kind(db) {
        TyKind::Named(name, arguments) => {
            if let Some(found) = arguments
                .iter()
                .find_map(|&inner| alias_cycle_walk(db, definitions, inner, expanding))
            {
                return Some(found);
            }
            let definition = definitions.get(db, *name)?;
            if !definition.alias {
                return None;
            }
            if !expanding.insert(*name) {
                return Some(name.text(db).to_owned());
            }
            let found = alias_cycle_walk(db, definitions, definition.body, expanding);
            expanding.remove(name);
            found
        }
        _ => {
            let mut found = None;
            for_each_child_type(db, ty, &mut |child| {
                if found.is_none() {
                    found = alias_cycle_walk(db, definitions, child, expanding);
                }
            });
            found
        }
    }
}

/// Whether `pkg::name` can reach the project's own bindings: only for the project's package.
fn namespace_reaches_project(db: &dyn Db, package: &Option<Symbol>) -> bool {
    package
        .as_ref()
        .is_none_or(|package| is_own_package(db, package.as_str()))
}

/// The name a qualified read resolves to, or `None` when the package is known not to export
/// it. A package the stub corpus does not know, or no corpus at all, cannot be validated, so
/// every such read resolves.
fn validated_namespace_name(
    db: &dyn Db,
    library: Option<&StubLibrary<'_>>,
    internal: bool,
    package: &Option<Symbol>,
    name: &Option<Symbol>,
) -> Option<Symbol> {
    let name = (*name)?;
    let Some(package) = package else {
        return Some(name);
    };
    // The project's own package resolves through globals and wins over a same-named stub namespace.
    if is_own_package(db, package.as_str()) {
        return (internal
            || own_package_reaches(db, QueryName::new(db, Name::new(db, name.as_str())))
                != Some(false))
        .then_some(name);
    }
    match library.map(|library| library.knows_namespace(package.as_str())) {
        None => Some(name),
        Some(false) => None,
        Some(true) => {
            if internal
                || library.is_some_and(|library| library.exports(package.as_str(), name.as_str()))
            {
                Some(name)
            } else {
                None
            }
        }
    }
}

/// Every top-level alternative of a pattern, looking through `|`.
fn collect_alternatives(module: &Module, pattern: PatId, alternatives: &mut Vec<PatId>) {
    match &module.pattern(pattern).kind {
        PatternKind::Or(inner) => {
            for &alternative in module[inner].iter() {
                collect_alternatives(module, alternative, alternatives);
            }
        }
        _ => alternatives.push(pattern),
    }
}

/// A literal pattern matches a length-one value of its own kind, with numbers matching
/// numbers; `NA` matches a missing value of any atomic type, and `NULL` only `NULL`.
fn literal_overlaps<'db>(
    db: &'db dyn Db,
    literal: &LiteralKind,
    literal_ty: Ty<'db>,
    subject: Ty<'db>,
) -> bool {
    let atomic = match subject.kind(db) {
        TyKind::Scalar(atomic) => *atomic,
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Scalar(atomic) => *atomic,
            _ => return true,
        },
        TyKind::Null => return matches!(literal, LiteralKind::Null),
        _ => return false,
    };
    if matches!(literal, LiteralKind::Na(_)) {
        return true;
    }
    let TyKind::Scalar(literal_atomic) = literal_ty.kind(db) else {
        return false;
    };
    let numeric = |atomic: Atomic| matches!(atomic, Atomic::Integer | Atomic::Double);
    *literal_atomic == atomic || (numeric(*literal_atomic) && numeric(atomic))
}

/// R's `Summary` group generic, which a class's `Summary.<class>` method implements for all
/// of these at once.
const SUMMARY_GROUP: [&str; 7] = ["all", "any", "sum", "prod", "min", "max", "range"];

/// The kind of ry value `ty` is, when every value it describes is one.
fn native_value_kind(db: &dyn Db, ty: Ty<'_>) -> Option<NativeValueKind> {
    match ty.kind(db) {
        TyKind::NativeRecord(_) => Some(NativeValueKind::Record),
        TyKind::NativeTuple(_) => Some(NativeValueKind::Tuple),
        TyKind::Tag(..) | TyKind::OtherTags => Some(NativeValueKind::Tag),
        TyKind::Native(NativeScalar::I64) => Some(NativeValueKind::I64),
        TyKind::Named(..)
            if array_element(db, ty).is_some_and(|element| match element.kind(db) {
                TyKind::Native(native) => native.shared_atomic().is_none(),
                _ => is_ry_value_type(db, element),
            }) =>
        {
            Some(NativeValueKind::List)
        }
        TyKind::Named(name, _) => match name.text(db) {
            "Dict" if is_ry_type(db, "Dict") => Some(NativeValueKind::Dict),
            "Set" if is_ry_type(db, "Set") => Some(NativeValueKind::Set),
            _ => None,
        },
        TyKind::Union(members) => {
            let mut kinds = members.iter().map(|&member| native_value_kind(db, member));
            let first = kinds.next()??;
            kinds.all(|kind| kind == Some(first)).then_some(first)
        }
        _ => None,
    }
}

/// The inference variables `ty` mentions, as the table resolved them when `ty` was read.
fn collect_vars<'db>(db: &'db dyn Db, ty: Ty<'db>, into: &mut FxHashSet<InferenceVar>) {
    if let TyKind::Var(var) = ty.kind(db) {
        into.insert(*var);
    }
    for_each_child_type(db, ty, &mut |child| collect_vars(db, child, into));
}

/// A base operator read as a value, as in `Reduce(`+`, xs)` or `sweep(m, 2, s, `*`)`. Its result
/// depends on R's coercion and dispatch rules for the operand types, which one signature cannot
/// state, so it takes and returns `Any`.
fn is_base_export(db: &dyn Db, name: &str) -> bool {
    stubs(db).is_some_and(|library| {
        library
            .exports_by_namespace
            .get("base")
            .is_some_and(|exports| exports.contains(name))
    })
}

fn operator_value<'db>(db: &'db dyn Db, name: &str) -> Option<TypeScheme<'db>> {
    const OPERATORS: &[&str] = &[
        "+", "-", "*", "/", "^", "%%", "%/%", "==", "!=", "<", ">", "<=", ">=", "&", "|", "!",
        "&&", "||",
    ];
    if !OPERATORS.contains(&name) {
        return None;
    }
    lower_type_text(db, "fn(e1: Any, [e2]: Any) -> Any")
}

/// Whether a library function returns a `List<B>` whose items no list argument supplies, so its
/// result's storage when empty is decided by nothing at run time: `List.map`'s `B` comes from the
/// callback, while `List.keep_if` returns the `List<A>` it was given.
fn builds_fresh_items<'db>(db: &'db dyn Db, scheme: &TypeScheme<'db>) -> bool {
    let TyKind::Function(function) = scheme.body.kind(db) else {
        return false;
    };
    let Some(item) = crate::types::array_element(db, function.ret) else {
        return false;
    };
    matches!(item.kind(db), TyKind::Rigid(_))
        && !function
            .positional
            .iter()
            .chain(function.named.iter().map(|parameter| &parameter.ty))
            .any(|&parameter| crate::types::array_element(db, parameter) == Some(item))
}

/// Whether a library function returns a `Dict` and takes none, so only the checker knows its
/// key and value types (`Dict.empty`, `Dict.with_capacity`).
fn builds_empty_dict<'db>(db: &'db dyn Db, scheme: &TypeScheme<'db>) -> bool {
    let TyKind::Function(function) = scheme.body.kind(db) else {
        return false;
    };
    crate::types::dict_entries(db, function.ret).is_some()
        && !function
            .positional
            .iter()
            .chain(function.named.iter().map(|parameter| &parameter.ty))
            .any(|&parameter| crate::types::dict_entries(db, parameter).is_some())
}

/// The storage of a list of `item`s, or `None` for `Bool`, whose storage is the empty list's
/// default, and for an item type left open.
fn list_items<'db>(db: &'db dyn Db, item: Ty<'db>) -> Option<ListItems> {
    match item.kind(db) {
        TyKind::Native(NativeScalar::I64) => Some(ListItems::I64),
        TyKind::Native(NativeScalar::F64) => Some(ListItems::F64),
        TyKind::Native(NativeScalar::Str) => Some(ListItems::Str),
        TyKind::Native(NativeScalar::Bool) => None,
        _ if crate::types::is_ry_value_type(db, item) => Some(ListItems::RyValues),
        _ => None,
    }
}

/// Whether a tag or a tag union appears anywhere in `ty`, the part of a ry type that widens.
fn mentions_tag<'db>(db: &'db dyn Db, ty: Ty<'db>) -> bool {
    if matches!(
        ty.kind(db),
        TyKind::Tag(..) | TyKind::Union(_) | TyKind::OtherTags
    ) {
        return true;
    }
    let mut found = false;
    for_each_child_type(db, ty, &mut |child| {
        found = found || mentions_tag(db, child)
    });
    found
}
