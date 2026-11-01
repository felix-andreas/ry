//! Expression inference walk: Hindley–Milner over one item's HIR. Branches roll
//! back through an undo log instead of cloning the environment.

use {
    crate::{
        DataPath, Db,
        annotations::{Annotation, NamedDefinition},
        data_file_layout,
        diagnostics::nearest_name,
        exhaustiveness::{MatchFinding, match_findings},
        hir::{
            Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind,
            MatchArm, Module, NaAtom, Parameter as Formal, PatId, PatternKind, Symbol,
            UnaryOperator, destructure_leaves, integer_literal_position, is_whole_number_double,
        },
        infer::{
            Entry, FunctionMismatch, InferenceTable, RecordMismatch, UnifyError, apply_definition,
        },
        metadata::{imported_by_name, imports_every_name, is_own_package},
        naming::{
            BindingId, BindingKind, ItemNaming, MATCH_FUN_CALLERS, dot_dot_variable,
            is_column_update, query_index_arguments,
        },
        stubs::{STANDARD_LIBRARY, is_ry_type, namespace_exports, namespace_known, stubs},
        types::{
            Atomic, Constraint, FrameKind, FunctionType, InferenceVar, Name, NativeField,
            Parameter, RecordField, RestParameter, TYPE_SIZE_CEILING, Ty, TyKind, TypeScheme, any,
            array, array_element, column_type, conflicting_tags, contains_inference_var,
            erase_vars, for_each_child_type, is_ry_value_type, is_tag_type, map_child_types,
            native_record, null, scalar, substitute_rigid, type_size, union_of, unknown,
        },
    },
    rustc_hash::{FxHashMap, FxHashSet},
    std::{
        mem,
        ops::Range,
        sync::atomic::{AtomicUsize, Ordering},
    },
    syntax::{TextRange, TextSize, native_values::accepts_native},
};

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct TypeError<'db> {
    pub range: TextRange,
    pub kind: TypeErrorKind<'db>,
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
    /// Separate from `Mismatch` so the differing field is named instead of printing
    /// two near-identical record types.
    RecordShape {
        mismatch: Box<RecordMismatch<'db>>,
    },
    NotAFunction {
        found: Ty<'db>,
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
    AliasCycle {
        name: String,
    },
    ConstraintViolation {
        constraint: Constraint,
        found: Ty<'db>,
    },
    /// An `InvalidOperand` on a use of a function literal's formal whose type came from
    /// the call the literal is passed to, so the finding names the formal and the call.
    ContextualParameter {
        name: String,
        expected: OperandExpectation,
        passed: Ty<'db>,
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
    pub expression_types: ExpressionTypes<'db>,
    /// The type of the value each `match` pattern sees, narrowed to the members it can match,
    /// so a tag or field inside a pattern hovers as the case it names. Indexed by pattern id;
    /// a pattern that cannot match records nothing.
    pub pattern_types: PatternTypes<'db>,
    /// The declared type each native record, tuple or tag literal was checked against, at an
    /// annotated binding, a call argument or a nominal constructor, so completion can offer
    /// the fields a literal still lacks. Nested literals record nothing: their expectation is
    /// the enclosing one's field or payload.
    pub literal_expectations: FxHashMap<ExprId, Ty<'db>>,
    /// The element type each `for` loop binds its variable to, keyed by the loop. The
    /// variable is a declaration, not an expression, so `expression_types` has no slot for it.
    pub loop_variables: FxHashMap<ExprId, Ty<'db>>,
    pub errors: Vec<TypeError<'db>>,
    pub strict_origins: Vec<StrictOrigin>,
    pub scheme: Option<TypeScheme<'db>>,
    /// Index into the declared overload set; absent when no candidate matched.
    pub selected_overloads: FxHashMap<ExprId, usize>,
    /// Reads inside a `data.table` bracket's index arguments: they evaluate in the
    /// data's own frame, so bare names are columns and skip the unresolved-name check.
    pub masked_reads: FxHashSet<ExprId>,
    /// Bare names inside a query on a frame with known columns that name no column and
    /// no lexical binding. Reported only when nothing else (a global, a builtin) defines
    /// the name, because R falls back to the enclosing scope for a non-column.
    pub column_misses: FxHashMap<ExprId, ColumnMiss>,
    /// Settled scheme of every name the top-level frame binds, so a statement item's
    /// conditional write serves cross-item reads; a definition resolves via `scheme`.
    pub top_level_bindings: Vec<(Symbol, TypeScheme<'db>)>,
    /// Coverage of each `match`, kept apart from `errors` because a missing case does not
    /// make the item's exported type untrustworthy.
    pub match_findings: Vec<MatchFinding>,
    /// Call heads whose lookup skipped the binding naming resolved, because that binding
    /// cannot hold a function. Editor features navigate to the binding the call reaches.
    pub callee_targets: FxHashMap<ExprId, CalleeTarget>,
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

/// Settled type of every expression the checker recorded, indexed by expression id.
/// Ids are dense within an item, so a slot per expression costs one word where a hash
/// entry costs several.
#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::SalsaValue)]
pub struct ExpressionTypes<'db>(Box<[Option<Ty<'db>>]>);

impl<'db> ExpressionTypes<'db> {
    pub fn get(&self, id: &ExprId) -> Option<&Ty<'db>> {
        self.0.get(id.0 as usize)?.as_ref()
    }
}

/// The checker's working copy of [`ExpressionTypes`], before the inference table resolves it.
struct RecordedTypes<'db>(Vec<Option<Ty<'db>>>);

impl<'db> RecordedTypes<'db> {
    fn get(&self, id: &ExprId) -> Option<&Ty<'db>> {
        self.0.get(id.0 as usize)?.as_ref()
    }

    fn insert(&mut self, id: ExprId, ty: Ty<'db>) {
        let slot = self
            .0
            .get_mut(id.0 as usize)
            .unwrap_or_else(|| panic!("recorded type for {id:?} outside the item's module"));
        *slot = Some(ty);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::SalsaValue)]
pub struct PatternTypes<'db>(Box<[Option<Ty<'db>>]>);

impl<'db> PatternTypes<'db> {
    pub fn get(&self, id: PatId) -> Option<Ty<'db>> {
        *self.0.get(id.0 as usize)?
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
    /// An unannotated parameter nothing constrains, so it accepts any value unchecked.
    /// `expression` is the function and `range` the parameter's name.
    UntypedParameter(String),
}

/// No method has a default body: adding a fact must be a compile error at every
/// implementor rather than a silent fallback.
pub trait GlobalEnv<'db> {
    /// `deferred`: a read inside a nested function runs after the document frame
    /// settled, so the LAST binding wins; an immediate read sees only earlier ones.
    fn scheme(&self, name: &str, deferred: bool) -> Option<TypeScheme<'db>>;

    /// `None` when the name has at most one candidate or a package/local definition
    /// shadows the stub set.
    fn overloads(&self, name: &str, deferred: bool) -> Option<Vec<TypeScheme<'db>>>;

    /// A project definition has its own attributable site, so a reference propagates
    /// its `Unknown` instead of re-originating one.
    fn defined_in_project(&self, name: &str, deferred: bool) -> bool;

    /// Never a stub, and never the project-wide conditional slot, whose join includes
    /// this item's own write.
    fn earlier_binding(&self, name: &str) -> Option<TypeScheme<'db>>;

    /// Every stub candidate for `name`, ignoring project definitions: what `pkg::name` or a
    /// call that skipped the project's non-function binding reaches. Empty for an export
    /// the stubs list without a type.
    fn library_candidates(&self, name: &str) -> Option<Vec<TypeScheme<'db>>>;

    /// What a call reaches once its lookup has left the file's top-level frame without finding
    /// a function, the library aside. A package file's frame is the namespace itself, so it
    /// reaches nothing; a test or helper file's frame encloses in the helpers' environment and
    /// the namespace, and a script's in the namespace.
    fn enclosing_scheme(&self, name: &str) -> Option<TypeScheme<'db>>;

    /// What the project's own `pkg::name` reads: the package namespace, then the library.
    /// A test file's or a script's own bindings and the helpers' are not in the namespace.
    fn namespace_scheme(&self, name: &str) -> Option<TypeScheme<'db>>;

    /// Borrowed: per-file facts consulted once per item; an owned copy would make every
    /// item pay for the whole table.
    fn type_definitions(&self) -> &'db FxHashMap<Name<'db>, NamedDefinition<'db>>;

    fn arithmetic_classes(&self) -> &'db FxHashSet<String>;

    /// Whether the project defines an arithmetic or `Ops` method for a class that no type
    /// declares, so a plain list operand may carry that class.
    fn has_untracked_arithmetic_class(&self) -> bool;

    /// The shipped declaration alone, which a call reaches when R's function lookup skips a
    /// project binding whose value is not a function.
    fn stub_scheme(&self, name: &str) -> Option<TypeScheme<'db>>;
}

/// Perf-witness instrument; fixpoint re-runs make it exceed the item count.
pub static CHECK_EXECUTIONS: AtomicUsize = AtomicUsize::new(0);

pub fn check_item_with_annotation<'db>(
    db: &'db dyn Db,
    module: &Module,
    naming: &ItemNaming,
    annotation: Option<&Annotation<'db>>,
    expression_annotations: &[(ExprId, Annotation<'db>)],
    globals: Option<&dyn GlobalEnv<'db>>,
) -> ItemCheck<'db> {
    CHECK_EXECUTIONS.fetch_add(1, Ordering::Relaxed);
    let mut table = InferenceTable::default();
    if let Some(globals) = globals {
        table.definitions = Some(globals.type_definitions());
        table.arithmetic_classes = Some(globals.arithmetic_classes());
    }
    let mut context = Checker {
        db,
        module,
        naming,
        globals,
        table,
        frame_level: 0,
        discarded_if: None,
        parameter_hints: FxHashMap::default(),
        hinted_parameters: FxHashSet::default(),
        expression_annotations: expression_annotations.iter().cloned().collect(),
        environment: Environment::default(),
        scheme_arena: Vec::new(),
        recorded: RecordedTypes(vec![None; module.expression_count()]),
        recorded_patterns: FxHashMap::default(),
        recorded_expectations: FxHashMap::default(),
        recorded_loop_variables: FxHashMap::default(),
        selected_overloads: FxHashMap::default(),
        callee_targets: FxHashMap::default(),
        masked_reads: FxHashSet::default(),
        column_scopes: Vec::new(),
        column_misses: FxHashMap::default(),
        query_depth: 0,
        errors: Vec::new(),
        strict_origins: Vec::new(),
        overload_probe_depth: 0,
        return_frames: Vec::new(),
        pending_enclosing_writes: Vec::new(),
        forward_captured: FxHashSet::default(),
        capture_joins: FxHashMap::default(),
        capture_repass_needed: false,
        pre_materialized: FxHashMap::default(),
        no_default_formals: FxHashSet::default(),
        defaulted_formals: FxHashSet::default(),
        non_local_slots: FxHashMap::default(),
        string_values: FxHashMap::default(),
        value_read_slots: None,
        computed_slots: FxHashSet::default(),
        next_non_local_slot: naming
            .bindings
            .keys()
            .map(|slot| slot.0 + 1)
            .max()
            .unwrap_or(0),
    };
    let mut annotation = annotation;
    if let Some(root) = module.root
        && let Some(present) = annotation
    {
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
        if let Some(name) = cycle {
            context.errors.push(TypeError {
                range: module.range(root),
                kind: TypeErrorKind::AliasCycle { name },
            });
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
                TyKind::Function(function) => Some((declared.clone(), function.clone())),
                _ => None,
            }
        });
        match (module.kind(root), declared_fn) {
            // A function literal with its own signature is typed by it, and the binding's
            // declared type is then checked against that like any other value.
            (ExpressionKind::Assign { value, .. }, Some((declared, function)))
                if matches!(module.kind(*value), ExpressionKind::Function { .. })
                    && !context.expression_annotations.contains_key(value) =>
            {
                for (name, constraint) in &declared.binders {
                    context.table.rigid_constraints.insert(*name, *constraint);
                }
                // Export the reconciled signature, not the annotation as written: R matches
                // arguments against the definition's formals, so annotation mistakes report once.
                let declared = TypeScheme {
                    binders: declared.binders,
                    body: context.check_declared_function(
                        *value,
                        &function,
                        annotation.map_or(&[], |annotation| &annotation.inline_parameters),
                    ),
                };
                context.recorded.insert(root, declared.body);
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
                        Some(declared) => {
                            for (name, constraint) in &declared.binders {
                                context.table.rigid_constraints.insert(*name, *constraint);
                            }
                            context.record_expectation(*value, declared.body);
                            if annotation.is_some_and(|a| !a.trusted)
                                && !matches!(declared.body.kind(db), TyKind::Unknown | TyKind::Any)
                            {
                                let expected = declared.body;
                                let range = module.range(*value);
                                let resolved_value = context.table.resolve(db, value_ty);
                                if !context.table.compatible(db, resolved_value, expected) {
                                    let error = context.type_mismatch(
                                        range,
                                        expected,
                                        resolved_value,
                                        Some(*value),
                                    );
                                    context.errors.push(error);
                                }
                            }
                            scheme_is_declared = true;
                            Some(declared)
                        }
                        None => Some(context.generalize(value_ty)),
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
    let recorded = mem::take(&mut context.recorded.0);
    let expression_types = ExpressionTypes(
        recorded
            .into_iter()
            .map(|slot| slot.map(|ty| context.table.resolve(context.db, ty)))
            .collect(),
    );
    let mut pattern_types = vec![None; module.pattern_count()];
    for (&id, &ty) in &context.recorded_patterns {
        if let Some(slot) = pattern_types.get_mut(id.0 as usize) {
            *slot = Some(context.table.resolve(context.db, ty));
        }
    }
    let pattern_types = PatternTypes(pattern_types.into_boxed_slice());
    let literal_expectations = context
        .recorded_expectations
        .iter()
        .map(|(&id, &ty)| (id, context.table.resolve(context.db, ty)))
        .collect();
    let loop_variables = context
        .recorded_loop_variables
        .iter()
        .map(|(&id, &ty)| (id, context.table.resolve(context.db, ty)))
        .collect();
    // A failed item exports `Unknown` so downstream items do not cascade off an
    // untrustworthy shape. Failing expressions record `Unknown`, so later checks inside
    // the item read them as absent facts rather than cascading.
    let mut errors = context.errors;
    let scheme = if errors.is_empty() || scheme_is_declared {
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
        if info.kind != BindingKind::TopLevel {
            continue;
        }
        let Some(entry) = context.environment.get(id) else {
            continue;
        };
        let binding_scheme = if errors.is_empty() {
            let open = match entry {
                EnvEntry::Mono(ty) | EnvEntry::MissingFormal(ty) => TypeScheme::monomorphic(ty),
                EnvEntry::Scheme(index) => context.scheme_arena[index as usize].clone(),
            };
            close_scheme(db, &mut context.table, open)
        } else {
            TypeScheme::monomorphic(unknown(db))
        };
        top_level_bindings.push((info.name, binding_scheme));
    }
    let match_findings = match_findings(db, context.table.definitions, module, &expression_types);
    ItemCheck {
        match_findings,
        expression_types,
        pattern_types,
        literal_expectations,
        loop_variables,
        errors,
        strict_origins: context.strict_origins,
        scheme,
        selected_overloads: context.selected_overloads,
        callee_targets: context.callee_targets,
        masked_reads: context.masked_reads,
        column_misses: context.column_misses,
        top_level_bindings,
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
    binders: Vec<(Name<'db>, Constraint)>,
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
    let Entry::Unbound { constraint, .. } = *table.entry(var) else {
        return unknown(db);
    };
    if constraint == Constraint::Unconstrained {
        return unknown(db);
    }
    if !closer.abstracted.contains(&table.find(var)) {
        return match constraint {
            Constraint::Numeric | Constraint::ScalarNumeric => scalar(db, Atomic::Double),
            Constraint::AtomicElement
            | Constraint::Key
            | Constraint::ArrayItem
            | Constraint::Unconstrained => unknown(db),
        };
    }
    if let Some(&rigid) = closer.generalized.get(&var) {
        return rigid;
    }
    let mut ordinal = closer.binders.len() + 1;
    let name = loop {
        let candidate = Name::new(db, format!("R{ordinal}"));
        if !closer
            .binders
            .iter()
            .any(|(existing, _)| *existing == candidate)
        {
            break candidate;
        }
        ordinal += 1;
    };
    let rigid = Ty::new(db, TyKind::Rigid(name));
    closer.binders.push((name, constraint));
    closer.generalized.insert(var, rigid);
    rigid
}

#[derive(Debug, Default)]
struct Environment<'db> {
    entries: FxHashMap<BindingId, EnvEntry<'db>>,
    undo: Vec<(BindingId, Option<EnvEntry<'db>>)>,
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

impl<'db> Environment<'db> {
    fn get(&self, slot: BindingId) -> Option<EnvEntry<'db>> {
        self.entries.get(&slot).copied()
    }

    fn set(&mut self, slot: BindingId, entry: EnvEntry<'db>) {
        let previous = self.entries.insert(slot, entry);
        self.undo.push((slot, previous));
    }

    fn mark(&self) -> usize {
        self.undo.len()
    }

    fn rollback(&mut self, mark: usize) {
        while self.undo.len() > mark {
            let (slot, previous) = self.undo.pop().expect("undo length checked");
            match previous {
                Some(entry) => {
                    self.entries.insert(slot, entry);
                }
                None => {
                    self.entries.remove(&slot);
                }
            }
        }
    }

    fn writes_since(&self, mark: usize) -> Vec<(BindingId, Option<EnvEntry<'db>>)> {
        let mut seen = FxHashSet::default();
        let mut writes = Vec::new();
        for (slot, _) in &self.undo[mark..] {
            if seen.insert(*slot) {
                writes.push((*slot, self.entries.get(slot).copied()));
            }
        }
        writes
    }
}

struct Checker<'db, 'a> {
    db: &'db dyn Db,
    module: &'a Module,
    naming: &'a ItemNaming,
    globals: Option<&'a dyn GlobalEnv<'db>>,
    table: InferenceTable<'db>,
    /// A binding's right-hand side infers one level deeper so a function generalizes at
    /// its binding; a stored monotype lowers back to this level, or a later
    /// generalization in the frame would quantify a variable the environment still holds.
    frame_level: u32,
    /// The `if` statement about to infer whose value its block discards.
    discarded_if: Option<ExprId>,
    /// Parameter types an unannotated function literal takes from the function type its call
    /// argument position expects, by formal position; `None` leaves a formal to inference.
    parameter_hints: FxHashMap<ExprId, Vec<Option<Ty<'db>>>>,
    /// The ranges of the formals that took their type from a call's expected function type, so
    /// only findings on those say the call supplied the type.
    hinted_parameters: FxHashSet<TextRange>,
    expression_annotations: FxHashMap<ExprId, Annotation<'db>>,
    environment: Environment<'db>,
    scheme_arena: Vec<TypeScheme<'db>>,
    recorded: RecordedTypes<'db>,
    recorded_patterns: FxHashMap<PatId, Ty<'db>>,
    recorded_expectations: FxHashMap<ExprId, Ty<'db>>,
    recorded_loop_variables: FxHashMap<ExprId, Ty<'db>>,
    selected_overloads: FxHashMap<ExprId, usize>,
    callee_targets: FxHashMap<ExprId, CalleeTarget>,
    masked_reads: FxHashSet<ExprId>,
    /// Queries being inferred, innermost last: a bare name inside one reads a column.
    column_scopes: Vec<ColumnScope<'db>>,
    column_misses: FxHashMap<ExprId, ColumnMiss>,
    /// Frame queries being inferred. data.table syntax makes naming resolve a whole
    /// bracket quietly, but inside a frame query ry knows the columns, so a quiet read
    /// that is no column still reads its global.
    query_depth: u32,
    errors: Vec<TypeError<'db>>,
    strict_origins: Vec<StrictOrigin>,
    /// Non-zero while probing an overload candidate: the literal-as-integer courtesy is
    /// off so it cannot decide the winner (exact matches outrank conversions).
    overload_probe_depth: u32,
    /// Per enclosing function, each early `return` value with its range, so a
    /// declared-return mismatch blames the offending `return`.
    return_frames: Vec<Vec<(Ty<'db>, TextRange)>>,
    /// `<<-` writes to enclosing slots, re-applied after each enclosing body's rollback
    /// so the join survives at the definition site.
    pending_enclosing_writes: Vec<(BindingId, Ty<'db>)>,
    /// Slots a closure read before any write reached them (the letrec shape).
    forward_captured: FxHashSet<BindingId>,
    /// Variable-erased join of every write to a captured slot, surviving rollbacks;
    /// forward-capture reads resolve here instead of the empty definition-point entry.
    capture_joins: FxHashMap<BindingId, Ty<'db>>,
    /// Closures were inferred against an incomplete capture join, so the body re-checks once.
    capture_repass_needed: bool,
    /// Top-level slots whose unwritten path read the cross-item binding: a pre-state
    /// that loop passes re-establish after their rollback.
    pre_materialized: FxHashMap<BindingId, Ty<'db>>,
    no_default_formals: FxHashSet<BindingId>,
    /// Unannotated formals with a default; a condition on one does not pin its type.
    defaulted_formals: FxHashSet<BindingId>,
    /// Slots for names this item reads but another item binds, so guard refinements live
    /// in the environment like any slot's. Ids are minted above naming's so they never collide.
    non_local_slots: FxHashMap<Symbol, BindingId>,
    next_non_local_slot: u32,
    /// The write of a binding whose only write is literal strings (`"a"` or `c("a", "b")`),
    /// compared by site so a re-pass over the same write keeps it. `None` once a second write
    /// site, or a write of anything else, appears. Queries read it for column names given through a variable
    /// (`.SDcols = cols`, `(cols) := ...`).
    string_values: FxHashMap<BindingId, Option<StringWrite>>,
    /// Slots read somewhere other than as a call head, built on first need.
    value_read_slots: Option<FxHashSet<BindingId>>,
    /// Slots whose type the code inferred rather than a declaration stated: those an
    /// unannotated assignment in the body has written so far, and undeclared formals, whose
    /// `NULL` is a placeholder default (NlcOptim's `solqp(H = NULL, ...)`) or the shape an
    /// `is.null()` test gave them (Rsolnp's `if (is.null(eqfun)) ...`).
    computed_slots: FxHashSet<BindingId>,
}

/// How a call head's function lookup went; see [`Checker::function_lookup`].
enum FunctionLookup<'db> {
    /// The ordinary path applies: the head is no bare name, or nothing was skipped.
    Ordinary,
    /// The callee's type, already recorded for the head.
    Found(Ty<'db>),
    /// The library's candidates, past every project binding of the name.
    Library(Symbol, Vec<TypeScheme<'db>>),
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

/// The evaluation frame of one query `f[i, j, by]`.
struct ColumnScope<'db> {
    /// `None` when the frame's columns are not known, which makes every bare name silent.
    columns: Option<Vec<RecordField<'db>>>,
    frame: Option<Symbol>,
    /// A binding made inside the bracket (a closure's parameter in `j`) stays lexical.
    bracket: TextRange,
    subset_data: Ty<'db>,
    /// Called names inside the bracket, which never read a column.
    callees: FxHashSet<ExprId>,
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
struct CallArgument<'db> {
    name: Option<String>,
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
enum ReplacementStep {
    Field(String),
    Position(usize),
    Element,
    /// `x[i]`, `x@a`, or a replacement call such as `names(x)`: not modeled.
    Other,
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
    /// `missing(name)` on a no-default formal: the edge marks the slot read-erroring
    /// instead of refining it.
    Missing(BindingId),
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
            | "expression"
    )
}

fn guard_kind(name: &str) -> Option<GuardKind> {
    Some(match name {
        "is.null" => GuardKind::Null,
        "is.character" => GuardKind::Family(GuardFamily::Character),
        "is.logical" => GuardKind::Family(GuardFamily::Logical),
        "is.integer" => GuardKind::Family(GuardFamily::Integer),
        "is.double" => GuardKind::Family(GuardFamily::Double),
        "is.numeric" => GuardKind::Family(GuardFamily::Numeric),
        "is.function" => GuardKind::Family(GuardFamily::Function),
        "is.list" => GuardKind::Family(GuardFamily::List),
        _ => return None,
    })
}

fn guard_members<'db>(db: &'db dyn Db, resolved: Ty<'db>) -> Vec<Ty<'db>> {
    match resolved.kind(db) {
        TyKind::Union(members) => members.clone(),
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
    missing_tested: FxHashSet<Symbol>,
    /// Names read on every path where no write of them dominates the read.
    forced: FxHashSet<Symbol>,
    assigned: FxHashSet<Symbol>,
}

impl FormalUses {
    fn makes_optional(&self, name: Symbol) -> bool {
        self.missing_tested.contains(&name) || !self.forced.contains(&name)
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

/// The names every non-diverging branch assigned. When every branch diverges, the code after
/// the join is unreachable and keeps `before`.
fn join_assigned(
    before: FxHashSet<Symbol>,
    branches: impl IntoIterator<Item = Option<FxHashSet<Symbol>>>,
) -> FxHashSet<Symbol> {
    branches
        .into_iter()
        .flatten()
        .reduce(|joined, branch| joined.intersection(&branch).copied().collect())
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

enum ArgumentTarget {
    Positional(usize),
    /// Index into `named`, whether claimed by name or filled positionally.
    Named(usize),
    Rest,
    /// A name no formal declares on a non-variadic function, or a duplicate of one
    /// already supplied.
    UnmatchedName,
    Overflow,
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

fn comparable_atomic(atomic: Atomic) -> bool {
    !matches!(atomic, Atomic::Complex | Atomic::Raw)
}

impl<'db> Checker<'db, '_> {
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

    fn unify_or_report(&mut self, range: TextRange, expected: Ty<'db>, found: Ty<'db>) {
        if let Err(error) = self.table.unify(self.db, expected, found) {
            self.report_unify(range, error);
        }
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
        let expected = self.table.resolve(self.db, expected);
        let found = self.table.resolve(self.db, found);
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

    /// The field name when `value` is `(name = ...)` and `expected` is a one-field record of
    /// that name, which is the record literal written without its trailing comma.
    fn assignment_for_record(&self, value: ExprId, expected: Ty<'db>) -> Option<String> {
        let TyKind::NativeRecord(fields) = expected.kind(self.db) else {
            return None;
        };
        let [field] = fields.as_slice() else {
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
        let declared = self
            .table
            .definitions
            .and_then(|definitions| definitions.get(name));
        let from_stubs = stubs(self.db).and_then(|library| library.type_definitions.get(name));
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
            TyKind::Union(members) => members.clone(),
            _ => vec![expected],
        };
        let (component, expected, found) = match module.kind(value) {
            ExpressionKind::Record(fields) => {
                let TyKind::NativeRecord(found_fields) = found.kind(self.db).clone() else {
                    return None;
                };
                let expected_fields = unique_member(self.db, &candidates, |kind| match kind {
                    TyKind::NativeRecord(fields) => Some(fields.clone()),
                    _ => None,
                })?;
                if expected_fields.len() != found_fields.len() {
                    return None;
                }
                let mut differing = Vec::new();
                for found_field in &found_fields {
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
                let TyKind::NativeTuple(found_items) = found.kind(self.db).clone() else {
                    return None;
                };
                let expected_items = unique_member(self.db, &candidates, |kind| match kind {
                    TyKind::NativeTuple(items) => Some(items.clone()),
                    _ => None,
                })?;
                if expected_items.len() != found_items.len() || items.len() != found_items.len() {
                    return None;
                }
                let mut differing = Vec::new();
                for ((&item, &expected_ty), &found_ty) in
                    module[items].iter().zip(&expected_items).zip(&found_items)
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
                    TyKind::Tag(tag, Some(payload)) if tag.text(self.db) == name.as_str() => {
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
                argument
                    .name_range
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
            .find(|argument| argument.name.as_deref() == Some(name))
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
            ExpressionKind::Literal(literal) => self.literal_ty(literal),
            ExpressionKind::NameRef(_) => self.infer_read(id),
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                let (spelling, target, value) = (*spelling, *target, *value);
                let declares_function = self.declared_function_annotation(id).filter(|_| {
                    matches!(self.module.kind(value), ExpressionKind::Function { .. })
                        && !self.expression_annotations.contains_key(&value)
                });
                let computed =
                    declares_function.is_none() && !self.expression_annotations.contains_key(&id);
                let value_ty = match declares_function {
                    Some(declared) => {
                        for (name, constraint) in &declared.binders {
                            self.table.rigid_constraints.insert(*name, *constraint);
                        }
                        let body = match declared.body.kind(self.db) {
                            TyKind::Function(function) => {
                                let inline = self.inline_parameters(id);
                                self.check_declared_function(value, function, &inline)
                            }
                            _ => declared.body,
                        };
                        self.record(value, body)
                    }
                    None => {
                        let binds_name = spelling != AssignSpelling::Super
                            && matches!(self.module.kind(target), ExpressionKind::NameRef(_));
                        if binds_name {
                            self.table.level += 1;
                        }
                        let value_ty = self.infer(value);
                        if binds_name {
                            self.table.level -= 1;
                        }
                        self.apply_expression_annotation(id, value, value_ty)
                    }
                };
                if let Some(&slot) = self.naming.resolutions.get(&target) {
                    match computed {
                        true => self.computed_slots.insert(slot),
                        false => self.computed_slots.remove(&slot),
                    };
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
                let (callee, arguments) = (*callee, arguments.clone());
                let first_call_error = self.errors.len();
                let ty = self.infer_call_expression(id, range, callee, &module[arguments]);
                self.check_native_arguments(range, callee, &module[arguments], first_call_error);
                ty
            }
            ExpressionKind::Index {
                double,
                target,
                arguments,
            } => {
                let double = *double;
                let target = *target;
                let arguments = arguments.clone();
                self.infer_index(id, range, double, target, &module[arguments])
            }
            ExpressionKind::Field {
                at,
                target,
                name,
                name_range,
            } => {
                let at = *at;
                let target = *target;
                let name = *name;
                let name_range = name_range.unwrap_or(range);
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
                let validated = validated_namespace_name(self.db, *internal, package, name);
                let reaches_project = namespace_reaches_project(self.db, package);
                let name = *name;
                match validated.as_ref().and_then(|name| {
                    let globals = self.globals?;
                    if reaches_project {
                        globals.namespace_scheme(name)
                    } else {
                        globals.library_candidates(name)?.pop()
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
                    for (name, constraint) in &declared.binders {
                        self.table.rigid_constraints.insert(*name, *constraint);
                    }
                    let TyKind::Function(function) = declared.body.kind(self.db) else {
                        return unknown(self.db);
                    };
                    let inline = self.inline_parameters(id);
                    let signature = self.check_declared_function(id, function, &inline);
                    return self.record(id, signature);
                }
                let parameters = parameters.clone();
                // Hints apply only when they type every formal without a default: a partly
                // typed lambda (`Reduce(function(a, b) a + b, xs)` fixes `b` but not the
                // accumulator `a`) infers better with all formals open, where `a + b` ties
                // the two together.
                let hints: Vec<Option<Ty<'db>>> = self
                    .parameter_hints
                    .remove(&id)
                    .filter(|hints| {
                        module[parameters]
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
                let pending_mark = self.pending_enclosing_writes.len();
                let mark = self.environment.mark();
                let mut uses = FormalUses::default();
                self.collect_formal_uses(*body, &mut uses);
                self.collect_forced_default_uses(&module[parameters], &mut uses);
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
                        name: Name::new(self.db, parameter.name.as_str()),
                        ty: parameter_ty,
                        optional: parameter.default.is_some()
                            || uses.makes_optional(parameter.name),
                        default: None,
                    });
                    let no_default = parameter.default.is_none();
                    if let Some(slot) = self
                        .naming
                        .bindings
                        .iter()
                        .find(|(_, info)| info.range == parameter.range)
                        .map(|(id, _)| *id)
                    {
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
                    }
                }
                self.return_frames.push(Vec::new());
                let trailing_ty = self.infer_body_with_capture_discovery(*body, pending_mark);
                let early_returns = self
                    .return_frames
                    .pop()
                    .expect("return frames stay balanced around body inference");
                let return_ty = self.join_early_returns(&early_returns, trailing_ty, *body);
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
                self.table.level -= 1;
                Ty::new(
                    self.db,
                    TyKind::Function(FunctionType {
                        positional: Vec::new(),
                        named,
                        variadic,
                        ret: return_ty,
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
                variable_range,
                sequence,
                body,
                ..
            } => {
                let (variable_range, sequence, body) = (*variable_range, *sequence, *body);
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
                let statements = statements.clone();
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
            ExpressionKind::Break | ExpressionKind::Next => unknown(self.db),
            ExpressionKind::Tuple(items) => {
                let items = items.clone();
                let items = module[items].iter().map(|&item| self.infer(item)).collect();
                Ty::new(self.db, TyKind::NativeTuple(items))
            }
            ExpressionKind::Array(items) => self.infer_array_literal(&module[items]),
            ExpressionKind::Record(fields) => {
                let fields = fields.clone();
                let fields = module[fields]
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name.as_str()),
                        ty: self.infer(field.value),
                    })
                    .collect();
                native_record(self.db, fields)
            }
            ExpressionKind::Tag { name, payload, .. } => {
                let (name, payload) = (*name, *payload);
                let payload = payload.map(|payload| self.infer(payload));
                Ty::new(
                    self.db,
                    TyKind::Tag(Name::new(self.db, name.as_str()), payload),
                )
            }
            ExpressionKind::Match { scrutinee, arms } => {
                let (scrutinee, arms) = (*scrutinee, arms.clone());
                self.infer_match(range, scrutinee, &module[arms])
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
        // A library function accepts ry values, but only a bare name or `ry::` reaches it:
        // `pkg::List.len` is the package's own function, whatever its name.
        let (function, may_be_library) = match self.module.kind(callee) {
            ExpressionKind::NameRef(name) => {
                let deferred = self.naming.deferred_non_locals.contains(&callee);
                if self.naming.resolutions.contains_key(&callee)
                    || self
                        .globals
                        .is_some_and(|globals| globals.defined_in_project(name, deferred))
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
                package.is_some_and(|package| package.as_str() == STANDARD_LIBRARY),
            ),
            _ => return,
        };
        if accepts_native(function)
            || (may_be_library && namespace_exports(self.db, STANDARD_LIBRARY, function))
        {
            return;
        }
        for argument in arguments {
            let Some(value) = argument.value else {
                continue;
            };
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
                        self.errors.remove(index);
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

    /// `@trust` and `@if-unknown` do not check the value, and `@new` mints a nominal
    /// rather than declaring a signature.
    fn declared_function_annotation(&self, annotated: ExprId) -> Option<TypeScheme<'db>> {
        let annotation = self.expression_annotations.get(&annotated)?;
        if annotation.trusted || annotation.if_unknown || annotation.new_nominal.is_some() {
            return None;
        }
        let declared = annotation.declared.clone()?;
        matches!(declared.body.kind(self.db), TyKind::Function(_)).then_some(declared)
    }

    fn inline_parameters(&self, annotated: ExprId) -> Vec<String> {
        self.expression_annotations
            .get(&annotated)
            .map(|annotation| annotation.inline_parameters.clone())
            .unwrap_or_default()
    }

    fn apply_expression_annotation(
        &mut self,
        annotated: ExprId,
        value: ExprId,
        value_ty: Ty<'db>,
    ) -> Ty<'db> {
        let Some(annotation) = self.expression_annotations.get(&annotated).cloned() else {
            return value_ty;
        };
        if annotation.declared.is_some() || annotation.new_nominal.is_some() {
            self.annotation_covers(value);
        }
        if let Some((name, arguments, _)) = &annotation.new_nominal {
            let scheme = self.check_new_nominal(*name, arguments, value, value_ty);
            return self.instantiate(&scheme);
        }
        let Some(declared) = &annotation.declared else {
            return value_ty;
        };
        if annotation.trusted {
            return declared.body;
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
        for (name, constraint) in &declared.binders {
            self.table.rigid_constraints.insert(*name, *constraint);
        }
        self.record_expectation(value, declared.body);
        let resolved_value = self.table.resolve(self.db, value_ty);
        if !self
            .table
            .compatible(self.db, resolved_value, declared.body)
        {
            let range = self.blame_range(value);
            let error = self.type_mismatch(range, declared.body, resolved_value, Some(value));
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
                        } if *var_level >= level
                    ) && mentioned.get(&representative) == Some(&1)
                }
                _ => false,
            };
            if !untyped {
                continue;
            }
            // A lambda passed as an argument gets its parameters' types from the call, which
            // unifies only after the lambda is inferred, so it is not judged here.
            let is_argument = self.module.kinds().iter().any(|kind| {
                matches!(
                    kind,
                    ExpressionKind::Call { arguments, .. } | ExpressionKind::Index { arguments, .. }
                        if self.module[arguments].iter().any(|argument| argument.value == Some(function))
                )
            });
            if is_argument {
                return;
            }
            let origin = StrictOrigin {
                expression: function,
                range: parameter.range,
                kind: StrictOriginKind::UntypedParameter(parameter.name.to_string()),
            };
            if !self.strict_origins.contains(&origin) {
                self.strict_origins.push(origin);
            }
        }
    }

    /// Annotating is exactly what a strict finding asks for, so it clears the origin.
    fn annotation_covers(&mut self, value: ExprId) {
        self.strict_origins
            .retain(|origin| origin.expression != value);
    }

    fn literal_ty(&mut self, literal: &LiteralKind) -> Ty<'db> {
        match literal {
            LiteralKind::Integer(_) => scalar(self.db, Atomic::Integer),
            LiteralKind::Double(_) => scalar(self.db, Atomic::Double),
            LiteralKind::Complex(_) => scalar(self.db, Atomic::Complex),
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
        self.read_slot(id, slot)
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
        if let Some(name) = naming.non_locals.get(&id)
            && let Some(scheme) = self
                .globals
                .and_then(|globals| globals.scheme(name, naming.deferred_non_locals.contains(&id)))
        {
            let instantiated = self.instantiate(&scheme);
            // A project-defined name owns its `Unknown` at its defining site, so reading it
            // propagates rather than re-originating.
            let deferred = self.naming.deferred_non_locals.contains(&id);
            if matches!(
                self.table.resolve(self.db, instantiated).kind(self.db),
                TyKind::Unknown
            ) && !self
                .globals
                .is_some_and(|globals| globals.defined_in_project(name, deferred))
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
        unknown(self.db)
    }

    /// The value `slot` holds at the read `id`.
    fn read_slot(&mut self, id: ExprId, slot: BindingId) -> Ty<'db> {
        match self.environment.get(slot) {
            Some(EnvEntry::Mono(ty)) => ty,
            Some(EnvEntry::Scheme(index)) => {
                let scheme = self.scheme_arena[index as usize].clone();
                self.instantiate(&scheme)
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
                if self.naming.captured_slots.contains(&slot) {
                    self.forward_captured.insert(slot);
                    if let Some(&join) = self.capture_joins.get(&slot) {
                        return join;
                    }
                } else if let Some(observed) = self.materialize_pre_state(slot, false) {
                    return observed;
                }
                unknown(self.db)
            }
        }
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
            .any(|scope| scope.callees.contains(&id))
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
        if resolved.is_some() {
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
        let deferred = naming.deferred_quiet_reads.contains(&id);
        let scheme = self
            .globals
            .and_then(|globals| globals.scheme(name, deferred));
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
        let binding = self.naming.bindings.get(&slot)?;
        if binding.kind != BindingKind::TopLevel {
            return None;
        }
        let globals = self.globals?;
        let scheme = if earlier {
            globals.earlier_binding(&binding.name)?
        } else {
            globals.scheme(&binding.name, false)?
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

    fn write_target(
        &mut self,
        spelling: AssignSpelling,
        target: ExprId,
        value: ExprId,
        value_ty: Ty<'db>,
    ) {
        if spelling == AssignSpelling::Destructure {
            let unchecked = unknown(self.db);
            for leaf in destructure_leaves(self.module, target).unwrap_or_default() {
                if let Some(&slot) = self.naming.resolutions.get(&leaf) {
                    self.environment.set(slot, EnvEntry::Mono(unchecked));
                    self.note_captured_write(slot, unchecked);
                }
                self.recorded.insert(leaf, unchecked);
            }
            return;
        }
        if let ExpressionKind::NameRef(_) = self.module.kind(target) {
            if let Some(&slot) = self.naming.resolutions.get(&target) {
                let literal = spelling != AssignSpelling::Super
                    && literal_strings(self.module, value).is_some();
                let write = StringWrite { target, value };
                let tracked = match self.string_values.get(&slot) {
                    None if literal => Some(write),
                    Some(Some(existing)) if literal && *existing == write => Some(write),
                    _ => None,
                };
                self.string_values.insert(slot, tracked);
                if spelling == AssignSpelling::Super {
                    let written = self.table.resolve(self.db, value_ty);
                    self.join_enclosing_write(slot, written);
                    self.pending_enclosing_writes.push((slot, written));
                    self.note_captured_write(slot, value_ty);
                } else {
                    // Let-polymorphism: only function values generalize.
                    let resolved = self.table.shallow_resolve(self.db, value_ty);
                    if matches!(resolved.kind(self.db), TyKind::Function(_)) {
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
                self.note_captured_write(slot, value_ty);
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
            self.write_replacement_target(target, value, value_ty);
        }
    }

    /// The column names `id` spells: a string, `c()` of strings, or a variable whose only
    /// write is one of those. Each name carries the range of the literal that spells it.
    fn column_names(&self, id: ExprId) -> Option<Vec<(String, TextRange)>> {
        let module = self.module;
        match module.kind(id) {
            ExpressionKind::NameRef(_) => {
                let slot = self.naming.resolutions.get(&id)?;
                let write = self.string_values.get(slot)?.as_ref()?;
                literal_strings(module, write.value)
            }
            ExpressionKind::Paren(inner) => self.column_names(*inner),
            _ => literal_strings(module, id),
        }
    }

    fn note_captured_write(&mut self, slot: BindingId, value_ty: Ty<'db>) {
        if !self.naming.captured_slots.contains(&slot) {
            return;
        }
        let written = erase_vars(self.db, self.table.resolve(self.db, value_ty));
        let joined = match self.capture_joins.get(&slot) {
            Some(&existing) if existing != written => self.join_types(existing, written),
            Some(&existing) => existing,
            None => written,
        };
        let changed = self.capture_joins.get(&slot) != Some(&joined);
        self.capture_joins.insert(slot, joined);
        if changed && self.forward_captured.contains(&slot) {
            self.capture_repass_needed = true;
        }
    }

    fn join_enclosing_write(&mut self, slot: BindingId, written: Ty<'db>) {
        let entry = match self.environment.get(slot) {
            Some(EnvEntry::Mono(existing)) if existing != written => {
                EnvEntry::Mono(self.join_types(existing, written))
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

    /// Kept recorded so each further enclosing scope re-applies them too (the join is idempotent).
    fn reapply_enclosing_writes(&mut self, mark: usize) {
        let writes: Vec<(BindingId, Ty<'db>)> = self.pending_enclosing_writes[mark..].to_vec();
        for (slot, written) in writes {
            self.join_enclosing_write(slot, written);
        }
    }

    fn write_replacement_target(&mut self, target: ExprId, value: ExprId, value_ty: Ty<'db>) {
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
        if let Some(&slot) = self.naming.resolutions.get(&base) {
            self.environment.set(slot, EnvEntry::Mono(written));
        }
    }

    /// An opaque nominal or a non-record representation admits nothing statically.
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
    /// `Unknown`, and a step it cannot model (`x[i]`, `x@a`) leaves its container as it was.
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
            } => Some((*target, ReplacementStep::Field(name.to_string()))),
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
            ExpressionKind::Field { target, .. } | ExpressionKind::Index { target, .. } => {
                Some((*target, ReplacementStep::Other))
            }
            ExpressionKind::Call { arguments, .. } => self.module[arguments]
                .first()
                .and_then(|argument| argument.value)
                .map(|value| (value, ReplacementStep::Other)),
            _ => None,
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
            (ReplacementStep::Field(name), TyKind::Record(fields)) => fields
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
        let module = self.module;
        let kind = module.kind(lhs);
        let field_name = match &step {
            ReplacementStep::Field(name) => Some(name.clone()),
            _ => None,
        };
        if let Some(field_name) = field_name {
            return match prior.kind(self.db).clone() {
                TyKind::Record(mut fields) => {
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
                    Ty::new(self.db, TyKind::Record(fields))
                }
                // R's `f$x <- NULL` drops the column, as `:=` does.
                TyKind::Frame(
                    kind @ (FrameKind::DataFrame | FrameKind::DataTable),
                    Some(mut columns),
                ) => {
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
                    Ty::new(self.db, TyKind::Frame(kind, Some(columns)))
                }
                // `x <- NULL; x$a <- 1` and `list()` both grow into a one-field list.
                TyKind::Null => Ty::new(
                    self.db,
                    TyKind::Record(vec![RecordField {
                        name: Name::new(self.db, field_name),
                        ty: value,
                        optional: false,
                    }]),
                ),
                TyKind::Tuple(items) if items.is_empty() => Ty::new(
                    self.db,
                    TyKind::Record(vec![RecordField {
                        name: Name::new(self.db, field_name),
                        ty: value,
                        optional: false,
                    }]),
                ),
                // A ry record's fields are fixed, so the write is checked against the field and
                // the record keeps its type.
                TyKind::NativeRecord(fields) => {
                    match fields
                        .iter()
                        .find(|field| field.name.text(self.db) == field_name)
                    {
                        Some(field) => {
                            if !matches!(value.kind(self.db), TyKind::Unknown)
                                && !self.table.compatible(self.db, value, field.ty)
                            {
                                let range = self.blame_range(value_expression);
                                let kind = self.type_mismatch(range, field.ty, value, None).kind;
                                self.errors.push(TypeError { range, kind });
                            }
                        }
                        None => {
                            let range = match kind {
                                ExpressionKind::Field {
                                    name_range: Some(name_range),
                                    ..
                                } => *name_range,
                                _ => module.range(lhs),
                            };
                            self.errors.push(TypeError {
                                range,
                                kind: TypeErrorKind::FieldCannotBeAdded {
                                    field: field_name,
                                    container: prior,
                                },
                            });
                        }
                    }
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
                _ => prior,
            };
        }
        if let (ReplacementStep::Position(position), TyKind::Tuple(items)) =
            (&step, prior.kind(self.db))
            && *position < items.len()
        {
            let mut items = items.clone();
            items[*position] = value;
            return Ty::new(self.db, TyKind::Tuple(items));
        }
        if !matches!(
            step,
            ReplacementStep::Position(_) | ReplacementStep::Element
        ) {
            return prior;
        }
        match prior.kind(self.db) {
            TyKind::Tuple(items) if items.is_empty() => Ty::new(self.db, TyKind::NamedList(value)),
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

    /// A diverging branch contributes neither its value nor its slot state, so the
    /// surviving edge's refinement persists after the `if` (the early-exit guard idiom).
    fn infer_if(
        &mut self,
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
        discarded: bool,
    ) -> Ty<'db> {
        self.expect_scalar_logical(condition);
        let guard = self.recognize_guard(condition);

        let mark = self.environment.mark();
        let pre_guard: Vec<(BindingId, Ty<'db>, Option<EnvEntry<'db>>)> = guard
            .iter()
            .flat_map(|guard| &guard.true_edge)
            .filter_map(|refinement| match *refinement {
                SlotRefinement::Narrow(slot, ty) => Some((slot, ty, self.environment.get(slot))),
                SlotRefinement::Missing(_) => None,
            })
            .collect();
        if let Some(guard) = &guard {
            self.apply_refinements(&guard.true_edge);
        }
        let then_ty = self.infer(then_branch);
        let then_diverges = self.diverges(then_branch);
        let mut then_writes = self.environment.writes_since(mark);
        // A refinement the branch never overwrote is the value it narrowed, not a new one.
        for (slot, entry) in &mut then_writes {
            if let Some(&(_, _, before)) = pre_guard
                .iter()
                .find(|(narrowed, ty, _)| narrowed == slot && *entry == Some(EnvEntry::Mono(*ty)))
            {
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
        let (else_ty, else_diverges) = match else_branch {
            Some(else_branch) => (self.infer(else_branch), self.diverges(else_branch)),
            None => (null(self.db), false),
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
                self.join_writes(&then_writes);
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
                self.join_writes(&then_writes);
                let joined = self.join_branch_values(then_ty, else_ty);
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

    /// Each arm checks from the state before the `match`, and the arms that complete join
    /// like the branches of an `if`. Pattern bindings are ordinary writes to the frame.
    fn infer_match(&mut self, range: TextRange, scrutinee: ExprId, arms: &[MatchArm]) -> Ty<'db> {
        let scrutinee_ty = self.infer(scrutinee);
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
                self.note_captured_write(slot, ty);
            }
            if let Some(guard) = arm.guard {
                self.expect_scalar_logical(guard);
            }
            let value = self.infer(arm.body);
            all_values.push(value);
            if !self.diverges(arm.body) {
                values.push(value);
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
                self.join_writes(&complete);
            }
        }
        let joined_values = if values.is_empty() {
            all_values
        } else {
            values
        };
        let mut joined_values = joined_values.into_iter();
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
                        TyKind::Tag(Name::new(self.db, name.as_str()), payload),
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
                    .collect();
                Some(Ty::new(self.db, TyKind::NativeTuple(items)))
            }
            PatternKind::Record {
                fields,
                rest: false,
            } => {
                let fields = self.module[fields]
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name.as_str()),
                        ty: self.fresh(Constraint::Unconstrained),
                    })
                    .collect();
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
        let PatternKind::Tag {
            name,
            name_range,
            payload,
        } = &self.module.pattern(pattern).kind
        else {
            return None;
        };
        let subject = self.table.resolve(self.db, subject);
        let members = match subject.kind(self.db) {
            TyKind::Union(members) => members.clone(),
            _ => vec![subject],
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
            return Some((format!("`:{}` carries no payload.", name.as_str()), None));
        }
        let Some((_, Some(tag_payload))) = found else {
            let nearest = nearest_name(name.as_str(), tags.iter().map(|(tag, _)| *tag))?;
            let tag_range = name_range
                .start()
                .checked_sub(TextSize::from(1))
                .map(|start| TextRange::new(start, name_range.end()));
            return Some((
                format!(
                    "The value has no tag `:{}`. Did you mean `:{nearest}`?",
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
            .map(|nearest| format!(" Did you mean `{nearest}`?"))
            .unwrap_or_default();
        Some((
            format!(
                "`:{}` has no field `{}`.{suggestion}",
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
            PatternKind::Bind(leaf) => {
                let leaf = *leaf;
                self.recorded.insert(leaf, subject);
                if let Some(&slot) = self.naming.resolutions.get(&leaf) {
                    match bindings.iter_mut().find(|(bound, _)| *bound == slot) {
                        Some((_, ty)) => *ty = union_of(self.db, [*ty, subject]),
                        None => bindings.push((slot, subject)),
                    }
                }
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
        let members = match subject.kind(self.db) {
            TyKind::Union(members) => members.clone(),
            _ => vec![subject],
        };
        let mut found = Vec::with_capacity(members.len());
        for member in members {
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
                .clone()
                .into_iter()
                .any(|member| self.pattern_admits(pattern, member));
        }
        match &module.pattern(pattern).kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Bind(_) => true,
            PatternKind::Or(alternatives) => {
                let alternatives = alternatives.clone();
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
                    subject_name.text(self.db) == name.as_str()
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
                    (*rest || native.len() == fields.len())
                        && module[fields].iter().all(|field| {
                            native
                                .iter()
                                .any(|native| native.name.text(self.db) == field.name.as_str())
                        })
                }
                _ => false,
            },
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
            let found = self.infer(item);
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
            let first = self.table.resolve(self.db, first);
            let found = self.table.resolve(self.db, found);
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
            None => self.fresh(Constraint::Unconstrained),
        };
        array(self.db, element)
    }

    /// An Array holds single `logical`, `integer`, `double` or `character` values unboxed, or
    /// ry values boxed. An R vector or list item would make the Array ragged, and `complex` and
    /// `raw` have no unboxed storage that the JIT reads.
    fn array_item_allowed(&mut self, ty: Ty<'db>) -> bool {
        let structural = self.structural(ty);
        match structural.kind(self.db) {
            TyKind::Scalar(
                Atomic::Logical | Atomic::Integer | Atomic::Double | Atomic::Character,
            )
            | TyKind::Var(_)
            | TyKind::Rigid(_)
            | TyKind::Any
            | TyKind::Unknown => true,
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
        let element = match self.iteration_element(resolved) {
            Ok(element) => element,
            Err(found) => {
                self.errors.push(TypeError {
                    range: sequence_range,
                    kind: TypeErrorKind::NotIterable { found },
                });
                unknown(self.db)
            }
        };
        if variable_range.is_some() {
            self.recorded_loop_variables
                .insert(loop_expression, element);
        }
        let loop_slot = variable_range.and_then(|variable_range| {
            self.naming
                .bindings
                .iter()
                .find(|(_, info)| info.range == variable_range)
                .map(|(id, _)| *id)
        });
        self.check_loop_body(
            loop_expression,
            None,
            body,
            false,
            loop_slot.map(|slot| (slot, element)),
        );
        null(self.db)
    }

    fn iteration_element(&mut self, resolved: Ty<'db>) -> Result<Ty<'db>, Ty<'db>> {
        match resolved.kind(self.db) {
            TyKind::Union(members) => {
                let mut elements = Vec::with_capacity(members.len());
                for &member in members {
                    let element = self.iteration_element(member).map_err(|_| resolved)?;
                    elements.push(element);
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
    /// changing at the cap widens to `Unknown`.
    fn check_loop_body(
        &mut self,
        loop_expression: ExprId,
        condition: Option<ExprId>,
        body: ExprId,
        runs_at_least_once: bool,
        loop_variable: Option<(BindingId, Ty<'db>)>,
    ) {
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
                self.expect_scalar_logical(condition);
            }
            self.infer(body);
            let writes: Vec<(BindingId, Option<EnvEntry<'db>>)> = self
                .environment
                .writes_since(mark)
                .into_iter()
                .filter(|(slot, _)| loop_variable.is_none_or(|(loop_slot, _)| *slot != loop_slot))
                .collect();
            self.environment.rollback(mark);
            // A read this pass discovered the slot's pre-loop state through the cross-item
            // binding; establish it so the body's writes join into it.
            for (&slot, &ty) in &self.pre_materialized {
                if self.environment.get(slot).is_none() {
                    self.environment.set(slot, EnvEntry::Mono(ty));
                }
            }
            let changed = self.join_writes(&writes);
            if changed.is_empty() {
                if runs_at_least_once {
                    for (slot, entry) in writes {
                        if let Some(entry) = entry {
                            self.environment.set(slot, entry);
                        }
                    }
                }
                return;
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
                return;
            }
            // Not yet stable: this pass ran under a stale entry state, so its diagnostics are
            // discarded.
            self.errors.truncate(errors_mark);
            self.strict_origins.truncate(origins_mark);
        }
    }

    /// The names whose runtime type the body inspects (`typeof(x)`, `class(x)`,
    /// `is.character(x)`, `inherits(x, ...)`). Such a value is dynamically typed by
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
    /// own frame, but its reads of an outer formal count: the closure usually runs, and
    /// when it does it forces the promise.
    fn collect_formal_uses(&self, id: ExprId, uses: &mut FormalUses) {
        let kind = self.module.kind(id);
        match kind {
            ExpressionKind::Function { parameters, body } => {
                let mut nested = FormalUses {
                    assigned: self.module[parameters]
                        .iter()
                        .map(|parameter| parameter.name)
                        .collect(),
                    ..FormalUses::default()
                };
                self.collect_formal_uses(*body, &mut nested);
                for name in nested.forced {
                    if !uses.assigned.contains(&name) {
                        uses.forced.insert(name);
                    }
                }
                return;
            }
            ExpressionKind::NameRef(name) => {
                if !uses.assigned.contains(name) {
                    uses.forced.insert(*name);
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
                            uses.assigned.insert(*name);
                        }
                    }
                }
                return;
            }
            ExpressionKind::Assign { target, value, .. }
                if let ExpressionKind::NameRef(name) = self.module.kind(*target) =>
            {
                self.collect_formal_uses(*value, uses);
                uses.assigned.insert(*name);
                return;
            }
            ExpressionKind::Assign { target, value, .. } => {
                self.collect_formal_uses(*value, uses);
                self.collect_formal_uses(*target, uses);
                return;
            }
            // A write assigns a formal only where it dominates the read: on both arms of a
            // branch, never inside a loop body that may not run or an operand that may not
            // be evaluated.
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.collect_formal_uses(*condition, uses);
                let before = uses.assigned.clone();
                let (then_assigned, then_forced) =
                    self.collect_branch_uses(*then_branch, uses, &before);
                let (else_assigned, else_forced) = match else_branch {
                    Some(else_branch) => self.collect_branch_uses(*else_branch, uses, &before),
                    None => (Some(before.clone()), FxHashSet::default()),
                };
                uses.assigned = join_assigned(before, [then_assigned, else_assigned]);
                uses.forced.extend(join_forced([then_forced, else_forced]));
                return;
            }
            ExpressionKind::Binary {
                operator: BinaryOperator::And2 | BinaryOperator::Or2,
                lhs,
                rhs,
                ..
            } => {
                self.collect_formal_uses(*lhs, uses);
                let before = uses.assigned.clone();
                self.collect_branch_uses(*rhs, uses, &before);
                uses.assigned = before;
                return;
            }
            ExpressionKind::While { condition, body } => {
                self.collect_formal_uses(*condition, uses);
                let before = uses.assigned.clone();
                self.collect_branch_uses(*body, uses, &before);
                uses.assigned = before;
                return;
            }
            ExpressionKind::For {
                variable,
                sequence,
                body,
                ..
            } => {
                self.collect_formal_uses(*sequence, uses);
                uses.assigned.extend(*variable);
                let before = uses.assigned.clone();
                self.collect_branch_uses(*body, uses, &before);
                uses.assigned = before;
                return;
            }
            // `local()` evaluates in a new frame, so its writes bind nothing here.
            ExpressionKind::Local { body } => {
                let before = uses.assigned.clone();
                self.collect_formal_uses(*body, uses);
                uses.assigned = before;
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
                    if callee_name == "missing" {
                        if let [argument] = &self.module[arguments][..]
                            && argument.name.is_none()
                            && let Some(value) = argument.value
                            && let ExpressionKind::NameRef(name) = self.module.kind(value)
                        {
                            uses.missing_tested.insert(*name);
                        }
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
                // `f(x = x)` hands the promise on unforced, and a closure the project defines
                // decides for itself whether to read it, so forwarding a formal under its own
                // name does not force it. A bare formal passed to a project parameter the callee
                // may leave unread, as `fast.svd(m, tol)` passes `tol` to a callee that tests
                // `missing(tol)`, is not forced either. A library function such as `list`
                // forces every argument.
                self.collect_formal_uses(*callee, uses);
                let (forwards, optional_targets) = match self.module.kind(*callee) {
                    ExpressionKind::NameRef(name) => {
                        let forwards = self
                            .globals
                            .is_none_or(|globals| globals.library_candidates(name).is_none());
                        let optional_targets = match forwards {
                            true => self.optional_argument_targets(name, &self.module[arguments]),
                            false => Vec::new(),
                        };
                        (forwards, optional_targets)
                    }
                    _ => (false, Vec::new()),
                };
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
                    if let ExpressionKind::NameRef(value_name) = self.module.kind(value)
                        && ((forwards
                            && (argument.name == Some(*value_name)
                                || optional_targets.get(index).copied().unwrap_or(false)))
                            || library_name.zip(argument.name).is_some_and(
                                |(function, parameter)| {
                                    tests_missing_without_default(function, parameter.as_str())
                                },
                            ))
                    {
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

    /// For each argument of a call to the project function `name`, whether it binds a
    /// parameter the callee may leave unread: optional without a default. A missing formal
    /// passed on does not take the callee's default, so a defaulted parameter that the
    /// callee reads still errors. Names match exactly, and the remaining
    /// arguments fill the unclaimed formals before `...` in order. An unknown callee yields no
    /// optional targets, so its arguments stay forced.
    fn optional_argument_targets(&self, name: &str, arguments: &[Argument]) -> Vec<bool> {
        let Some(scheme) = self.globals.and_then(|globals| globals.scheme(name, true)) else {
            return Vec::new();
        };
        let TyKind::Function(function) = scheme.body.kind(self.db) else {
            return Vec::new();
        };
        let leaves_unread =
            |parameter: &Parameter<'db>| parameter.optional && parameter.default.is_none();
        let mut claimed = vec![false; function.named.len()];
        let mut targets = vec![false; arguments.len()];
        for (index, argument) in arguments.iter().enumerate() {
            let Some(argument_name) = argument.name else {
                continue;
            };
            if let Some(formal) = function
                .named
                .iter()
                .position(|parameter| parameter.name.text(self.db) == argument_name.as_str())
            {
                claimed[formal] = true;
                targets[index] = leaves_unread(&function.named[formal]);
            }
        }
        let pre_rest = function
            .variadic
            .as_ref()
            .map_or(function.named.len(), |rest| {
                rest.preceding_named.min(function.named.len())
            });
        let mut unclaimed = (0..pre_rest).filter(|formal| !claimed[*formal]);
        for (index, argument) in arguments.iter().enumerate() {
            if argument.name.is_some() {
                continue;
            }
            let Some(formal) = unclaimed.next() else {
                break;
            };
            targets[index] = leaves_unread(&function.named[formal]);
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
        let outer_forced = mem::take(&mut uses.forced);
        self.collect_formal_uses(id, uses);
        let forced = mem::replace(&mut uses.forced, outer_forced);
        let after = mem::take(&mut uses.assigned);
        ((!self.diverges(id)).then_some(after), forced)
    }

    /// A forced formal that the call omits evaluates its default, so the default's reads
    /// force too (`function(x, y = x) y`). Repeats until no default adds a name, since a
    /// default may read a formal whose own default reads another.
    fn collect_forced_default_uses(&self, parameters: &[Formal], uses: &mut FormalUses) {
        let mut visited = vec![false; parameters.len()];
        loop {
            let mut grew = false;
            for (index, parameter) in parameters.iter().enumerate() {
                if visited[index] || !uses.forced.contains(&parameter.name) {
                    continue;
                }
                visited[index] = true;
                if let Some(default) = parameter.default {
                    let mut from_default = FormalUses::default();
                    self.collect_formal_uses(default, &mut from_default);
                    uses.forced.extend(from_default.forced);
                    grew = true;
                }
            }
            if !grew {
                return;
            }
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
        let substitution = binders
            .into_iter()
            .map(|(name, constraint)| (name, self.fresh(constraint)))
            .collect();
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
            ExpressionKind::Block { statements, .. } => self.module[statements]
                .last()
                .is_some_and(|&last| self.diverges(last)),
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
        if let Some(frame) = self.return_frames.last_mut() {
            frame.push((resolved, blamed));
        }
        null(self.db)
    }

    fn recognize_guard(&mut self, condition: ExprId) -> Option<GuardRefinement<'db>> {
        match self.module.kind(condition) {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => {
                let inner = self.recognize_guard(*operand)?;
                // `!missing(x)` has no read-erroring edge to swap into.
                let narrowed_only = |edge: Vec<SlotRefinement<'db>>| {
                    edge.into_iter()
                        .filter(|refinement| matches!(refinement, SlotRefinement::Narrow(..)))
                        .collect()
                };
                Some(GuardRefinement {
                    true_edge: narrowed_only(inner.false_edge),
                    false_edge: narrowed_only(inner.true_edge),
                })
            }
            ExpressionKind::Paren(inner) => self.recognize_guard(*inner),
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
                let [argument] = &self.module[arguments][..] else {
                    return None;
                };
                if argument.name.is_some() {
                    return None;
                }
                let value = argument.value?;
                if !matches!(self.module.kind(value), ExpressionKind::NameRef(_)) {
                    return None;
                }
                let slot = match self.naming.resolutions.get(&value) {
                    Some(&slot) => slot,
                    // A non-local narrows like a local (guard and branches evaluate in order), except
                    // inside a closure, which runs later and sees a value the test says nothing about.
                    None => {
                        if self.naming.deferred_non_locals.contains(&value) {
                            return None;
                        }
                        let observed = self.infer(value);
                        self.non_local_guard_slot(value, observed)?
                    }
                };
                if name == "missing" {
                    return self
                        .no_default_formals
                        .contains(&slot)
                        .then(|| GuardRefinement {
                            true_edge: vec![SlotRefinement::Missing(slot)],
                            false_edge: Vec::new(),
                        });
                }
                let guard = guard_kind(name)?;
                let EnvEntry::Mono(current) = self.environment.get(slot)? else {
                    return None;
                };
                let resolved = self.table.resolve(self.db, current);
                self.guard_edges(guard, slot, resolved)
            }
            _ => None,
        }
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
                SlotRefinement::Missing(slot) => {
                    if let Some(EnvEntry::Mono(current)) = self.environment.get(slot) {
                        self.environment.set(slot, EnvEntry::MissingFormal(current));
                    }
                }
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
            UnaryOperator::Plus | UnaryOperator::Tilde | UnaryOperator::Help => {
                self.infer(operand);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            }
        }
    }

    fn infer_unary_minus(&mut self, operand: ExprId) -> Ty<'db> {
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
            NumericOperand::Flexible(ty) => {
                self.constrain_numeric_flexible(operand_range, ty);
                ty
            }
            NumericOperand::FlexibleVector(element) => {
                if let Some(element) = element {
                    self.constrain_numeric_flexible(operand_range, element);
                }
                resolved
            }
            NumericOperand::OpenUnion(open) => {
                for member in open {
                    self.constrain_numeric_flexible(operand_range, member);
                }
                unknown(self.db)
            }
            NumericOperand::AnyUnknown => unknown(self.db),
            NumericOperand::Invalid if self.declares_arithmetic(resolved) => resolved,
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
    /// dispatches unary `-` through the same `-.class`, `Arith.class` and `Ops.class`.
    fn declares_arithmetic(&self, ty: Ty<'db>) -> bool {
        let TyKind::Named(name, _) = ty.kind(self.db) else {
            return false;
        };
        let Some(globals) = self.globals else {
            return false;
        };
        let class = name.text(self.db);
        ["-", "Arith", "Ops"].iter().any(|prefix| {
            let method = format!("{prefix}.{class}");
            globals
                .overloads(&method, false)
                .is_some_and(|candidates| !candidates.is_empty())
                || globals.scheme(&method, false).is_some()
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
            let method = format!("{prefix}.{class}");
            globals
                .overloads(&method, false)
                .is_some_and(|candidates| !candidates.is_empty())
                || globals.scheme(&method, false).is_some()
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
            TyKind::Union(members) => members.clone(),
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
            TyKind::Scalar(Atomic::Integer | Atomic::Double) => {
                expected == OperandExpectation::Numeric
            }
            TyKind::Scalar(Atomic::Logical) => expected == OperandExpectation::Logical,
            TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => true,
            _ => false,
        };
        if !fits {
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
                let left = self.infer(lhs);
                let right = self.infer(rhs);
                if let Some(result) = self.infer_array_operator(range, operator, left, right) {
                    return result;
                }
                self.operator_method_result(range, operator, lhs, rhs, left, right)
                    .unwrap_or_else(|| match operator {
                        Less | Greater | LessEq | GreaterEq | Equal | NotEqual => {
                            self.infer_compare(lhs, rhs, left, right)
                        }
                        // `/` and `^` always produce doubles.
                        Divide | Power => {
                            self.infer_binary_numeric(range, lhs, rhs, left, right, true)
                        }
                        _ => self.infer_binary_numeric(range, lhs, rhs, left, right, false),
                    })
            }
            Sequence => self.infer_colon(lhs, rhs),
            // The right operand runs only on the left's true edge for `&&` (false for `||`), so
            // `is.numeric(x) && x > 0` narrows like the `if` it abbreviates.
            And2 | Or2 => {
                self.expect_scalar_logical(lhs);
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
                self.expect_scalar_logical(rhs);
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
                                SlotRefinement::Missing(missing),
                                Some(EnvEntry::MissingFormal(_)),
                            ) => missing == slot,
                            _ => false,
                        })
                    })
                    .collect();
                self.environment.rollback(mark);
                self.join_writes(&writes);
                scalar(self.db, Atomic::Logical)
            }
            // Opaque constructs: operands still infer for the IDE, but their diagnostics are
            // discarded because nothing inside the construct is judged. Elementwise `&` and `|`
            // over logical or numeric operands are typed; any other operand (raw, a classed
            // object with its own method) leaves them opaque.
            And | Or | Special | Pipe | Tilde | Help => {
                let errors_mark = self.errors.len();
                let origins_mark = self.strict_origins.len();
                let left = self.infer(lhs);
                let right = self.infer(rhs);
                if matches!(operator, And | Or) {
                    if let Some(result) = self.infer_array_operator(range, operator, left, right) {
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
                self.errors.truncate(errors_mark);
                self.strict_origins.truncate(origins_mark);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            }
        }
    }

    /// Elementwise operators on Arrays. Both operands share one item type, with no promotion
    /// between `integer` and `double`, and a single value of that type may stand on either side.
    /// `None` when neither operand is an Array, so R's own rules apply.
    fn infer_array_operator(
        &mut self,
        range: TextRange,
        operator: BinaryOperator,
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
        let operand_element = |checker: &mut Self, operand: Ty<'db>, element: Option<Ty<'db>>| {
            if let Some(element) = element {
                return Some(element);
            }
            match operand.kind(checker.db) {
                TyKind::Scalar(_) | TyKind::Var(_) | TyKind::Rigid(_) => Some(operand),
                TyKind::Any | TyKind::Unknown => Some(known),
                _ => None,
            }
        };
        let (Some(left_item), Some(right_item)) = (
            operand_element(self, left, left_element),
            operand_element(self, right, right_element),
        ) else {
            return reject(self);
        };
        let snapshot = self.table.snapshot();
        if self.table.unify(self.db, left_item, right_item).is_err() {
            self.table.rollback(snapshot);
            return reject(self);
        }
        let item = self.table.resolve(self.db, left_item);
        let item_kind = match item.kind(self.db) {
            TyKind::Scalar(atomic) => Some(*atomic),
            TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown => None,
            _ => return reject(self),
        };
        let logical = scalar(self.db, Atomic::Logical);
        let result = match operator {
            Add | Subtract | Multiply | Modulo | IntegerDivide | Divide | Power => {
                if item_kind
                    .is_some_and(|atomic| !matches!(atomic, Atomic::Integer | Atomic::Double))
                {
                    return reject(self);
                }
                if matches!(operator, Divide | Power) {
                    scalar(self.db, Atomic::Double)
                } else {
                    item
                }
            }
            Less | Greater | LessEq | GreaterEq | Equal | NotEqual => logical,
            And | Or => {
                if item_kind.is_some_and(|atomic| atomic != Atomic::Logical) {
                    return reject(self);
                }
                logical
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
    fn infer_declared_operator(
        &mut self,
        range: TextRange,
        name: &str,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<Ty<'db>> {
        let library = stubs(self.db)?;
        if !library.schemes.contains_key(name) {
            return None;
        }
        if self
            .globals
            .is_some_and(|globals| globals.defined_in_project(name, false))
        {
            return None;
        }
        let scheme = self.globals?.scheme(name, false)?;
        let instantiated = self.instantiate(&scheme);
        let resolved = self.table.shallow_resolve(self.db, instantiated);
        let TyKind::Function(function) = resolved.kind(self.db) else {
            return None;
        };
        let left = self.infer(lhs);
        let right = self.infer(rhs);
        let arguments = [
            self.operand_argument(lhs, left),
            self.operand_argument(rhs, right),
        ];
        let findings = self.match_arguments(range, function, &arguments);
        self.errors.extend(findings);
        Some(function.ret)
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
        // A generic bounded `numeric` probes as the `double` it may be.
        let probe_operand =
            |ty: Ty<'db>| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
                TyKind::Rigid(name)
                    if matches!(
                        self.table.rigid_constraints.get(name),
                        Some(Constraint::Numeric | Constraint::ScalarNumeric)
                    ) =>
                {
                    scalar(self.db, Atomic::Double)
                }
                _ => ty,
            };
        let arguments = [
            self.operand_argument(lhs, probe_operand(left)),
            self.operand_argument(rhs, probe_operand(right)),
        ];
        let class_of = |ty: Ty<'db>| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
            TyKind::Named(name, _) => Some(name.text(self.db).to_owned()),
            _ => None,
        };
        let classes: Vec<String> = [class_of(left), class_of(right)]
            .into_iter()
            .flatten()
            .collect();
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
        let mut fitting_returns: Vec<Ty<'db>> = Vec::new();
        let mut declared_any = false;
        for class in &classes {
            for method in [
                format!("{spelling}.{class}"),
                format!("{group}.{class}"),
                format!("Ops.{class}"),
            ] {
                let Some(candidates) = globals
                    .overloads(&method, false)
                    .or_else(|| globals.scheme(&method, false).map(|scheme| vec![scheme]))
                    .filter(|candidates| !candidates.is_empty())
                else {
                    continue;
                };
                declared_any = true;
                for scheme in candidates {
                    let snapshot = self.table.snapshot();
                    let instantiated = self.instantiate(&scheme);
                    let resolved = self.table.shallow_resolve(self.db, instantiated);
                    let TyKind::Function(function) = resolved.kind(self.db) else {
                        self.table.rollback(snapshot);
                        continue;
                    };
                    if self.match_arguments(range, function, &arguments).is_empty() {
                        if flexible.is_empty() {
                            return Some(function.ret);
                        }
                        let ret = self.table.resolve(self.db, function.ret);
                        self.table.rollback(snapshot);
                        fitting_returns.push(ret);
                        continue;
                    }
                    self.table.rollback(snapshot);
                }
            }
        }
        if let Some(&first) = fitting_returns.first() {
            for (operand_range, ty) in flexible {
                self.constrain_numeric_flexible(operand_range, ty);
            }
            return Some(if fitting_returns.iter().all(|&ret| ret == first) {
                first
            } else {
                unknown(self.db)
            });
        }
        // A class that declares the operator but fits no candidate is a real contract
        // mismatch (`Date + Date` errors in R too).
        if declared_any {
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

    fn expect_scalar_logical(&mut self, condition: ExprId) {
        let condition_range = self.blame_range(condition);
        let inferred = self.infer(condition);
        let resolved = self.structural(inferred);
        if (self.condition_coerces(resolved) && !self.is_long_combine(condition))
            || self.reads_open_defaulted_formal(condition, resolved)
        {
            return;
        }
        // A `NULL` member is the "not supplied" path of an optional value, and R code tests such
        // a value once it holds one, so a union is a condition when its other members are. Its
        // open members (a parameter rebound on one path) are bound to `logical` as a lone open
        // value would be.
        if let TyKind::Union(members) = resolved.kind(self.db) {
            let members = members.clone();
            let values: Vec<_> = members
                .iter()
                .copied()
                .filter(|&member| !matches!(member.kind(self.db), TyKind::Null))
                .collect();
            if !values.is_empty()
                && values
                    .iter()
                    .all(|&member| self.condition_coerces(member) || self.is_open_parameter(member))
            {
                for member in values {
                    if !self.condition_coerces(member) {
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
                .is_some_and(|slot| self.defaulted_formals.contains(slot))
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
            TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double) => true,
            // A vector type carries no length, and the vectors code tests (`if (x >= 1)` on a
            // `double[]`) are length one at run time far more often than not.
            TyKind::Vector(element) => self.condition_coerces(*element),
            TyKind::Union(members) => members.iter().all(|&member| self.condition_coerces(member)),
            // A value already required to be numeric (`if (n %% 2L)`) coerces like a number.
            TyKind::Var(_) => matches!(
                self.table.open_constraint(self.db, ty),
                Some(Constraint::Numeric | Constraint::ScalarNumeric)
            ),
            TyKind::Rigid(name) => matches!(
                self.table.rigid_constraints.get(name),
                Some(Constraint::Numeric | Constraint::ScalarNumeric)
            ),
            _ => false,
        }
    }

    fn infer_binary_numeric(
        &mut self,
        range: TextRange,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: Ty<'db>,
        rhs_ty: Ty<'db>,
        always_double: bool,
    ) -> Ty<'db> {
        let lhs_range = self.blame_range(lhs);
        let rhs_range = self.blame_range(rhs);
        let resolved_left = self.operand_view(lhs_ty);
        let resolved_right = self.operand_view(rhs_ty);
        // A list operand is an error in R unless it carries a class with an operator method,
        // and a chain `s + a + b` passes the method's result on, so either kind of operand
        // leaves the expression to that method.
        let may_dispatch = |ty: Ty<'db>| {
            matches!(
                ty.kind(self.db),
                TyKind::List(_)
                    | TyKind::NamedList(_)
                    | TyKind::Tuple(_)
                    | TyKind::Record(_)
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
        for operand in [&left, &right] {
            if let NumericOperand::Flexible(ty) = operand {
                match flexible {
                    Some(existing) => {
                        if let Err(error) = self.table.unify(self.db, existing, *ty) {
                            self.report_unify(range, error);
                            return unknown(self.db);
                        }
                    }
                    None => flexible = Some(*ty),
                }
            }
        }
        if let Some(ty) = flexible {
            self.constrain_numeric_flexible(range, ty);
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
            // R takes a logical endpoint as a number, and a vector endpoint by its first element.
            // A vector type carries no length, so only a literal `c()` of several elements is
            // known to be too long.
            let members: Vec<Ty<'db>> = match resolved.kind(self.db) {
                TyKind::Union(members) => members.clone(),
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
                    if matches!(
                        self.table.rigid_constraints.get(name),
                        Some(Constraint::ScalarNumeric)
                    ) =>
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

    /// A rigid satisfies the numeric constraint only when its declaration promised it.
    fn constrain_numeric_flexible(&mut self, range: TextRange, ty: Ty<'db>) {
        match ty.kind(self.db) {
            TyKind::Var(var) => {
                if let Err(error) = self.table.constrain(self.db, *var, Constraint::Numeric) {
                    self.report_unify(range, error);
                }
            }
            TyKind::Rigid(name)
                if !matches!(
                    self.table.rigid_constraints.get(name),
                    Some(Constraint::Numeric | Constraint::ScalarNumeric)
                ) =>
            {
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::ConstraintViolation {
                        constraint: Constraint::Numeric,
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
                for &member in members {
                    if let Some(part) = numeric_operand_parts(self.db, member) {
                        parts.push(part);
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
            TyKind::Rigid(name) => match self.table.rigid_constraints.get(name) {
                Some(Constraint::Numeric | Constraint::ScalarNumeric) => {
                    NumericOperand::Flexible(resolved)
                }
                _ => NumericOperand::Invalid,
            },
            TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(self.db) {
                TyKind::Var(_) => NumericOperand::FlexibleVector(Some(*element)),
                TyKind::Rigid(name)
                    if matches!(
                        self.table.rigid_constraints.get(name),
                        Some(Constraint::Numeric | Constraint::ScalarNumeric)
                    ) =>
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
            return Ty::new(self.db, TyKind::Function(declared.clone()));
        };
        let range = module.range(expression);

        let mut uses = FormalUses::default();
        self.collect_formal_uses(*body, &mut uses);
        self.collect_forced_default_uses(&module[parameters], &mut uses);
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
        let mut positional_index = 0usize;
        let mut used_named: Vec<bool> = vec![false; declared.named.len()];
        let mut formal_types: Vec<(Ty<'db>, bool)> = Vec::with_capacity(parameters.len());
        let mut signature = FunctionType {
            positional: Vec::new(),
            named: Vec::new(),
            variadic: None,
            ret: declared.ret,
        };
        for parameter in &module[parameters] {
            if parameter.name == "..." {
                signature.variadic = Some(RestParameter {
                    element: declared
                        .variadic
                        .as_ref()
                        .map_or_else(|| any(self.db), |rest| rest.element),
                    preceding_named: signature.named.len(),
                });
                formal_types.push((self.fresh(Constraint::Unconstrained), false));
                continue;
            }
            // A disagreeing declaration is reported below, and the definition wins: a declared
            // `[name]` over a forced formal stays required, a declared required parameter over
            // a defaulted or `missing()`-tested formal stays optional.
            let (declared_ty, optional) = if let Some(index) = declared
                .named
                .iter()
                .position(|field| field.name.text(self.db) == parameter.name)
            {
                used_named[index] = true;
                let field = &declared.named[index];
                let optional =
                    is_declared_optional(parameter) || field.optional && is_optional(parameter);
                (Some(field.ty), optional)
            } else if positional_index < declared.positional.len() {
                let ty = declared.positional[positional_index];
                positional_index += 1;
                (Some(ty), is_declared_optional(parameter))
            } else {
                // Partial annotation: an unannotated formal infers from its uses as if unannotated.
                (None, is_optional(parameter))
            };
            let parameter_ty = declared_ty.unwrap_or_else(|| self.fresh(Constraint::Unconstrained));
            formal_types.push((parameter_ty, declared_ty.is_some()));
            signature.named.push(Parameter {
                name: Name::new(self.db, parameter.name.as_str()),
                ty: parameter_ty,
                optional,
                default: parameter.default.map(|default| self.infer(default)),
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
                    expected: Ty::new(self.db, TyKind::Function(declared.clone())),
                    found: Ty::new(self.db, TyKind::Function(signature.clone())),
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
            if !used_named[index] {
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
        for (parameter, (parameter_ty, declared)) in module[parameters].iter().zip(formal_types) {
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
                let default_ty = self.infer(default);
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

        self.return_frames.push(Vec::new());
        let trailing_ty = self.infer_body_with_capture_discovery(*body, pending_mark);
        let early_returns = self
            .return_frames
            .pop()
            .expect("return frames stay balanced around body inference");
        // An `Unknown` declared return (elided `->`) is inferred from the body, so
        // annotating only the parameters does not drop the return.
        if matches!(declared.ret.kind(self.db), TyKind::Unknown) {
            signature.ret = self.join_early_returns(&early_returns, trailing_ty, *body);
            self.record_untyped_parameters(function_id, &untyped_candidates, signature.ret);
        } else {
            self.record_untyped_parameters(function_id, &untyped_candidates, declared.ret);
            // Each `return` is checked at its own site, not as one union blamed on the tail.
            let mut early_return_failed = false;
            for &(returned, return_range) in &early_returns {
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
            if !early_return_failed
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
        Ty::new(self.db, TyKind::Function(signature))
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
            _ => into.push(id),
        }
    }

    /// Re-runs the body once when it grew a captured-write join a closure already read;
    /// the first run is fully discarded (environment, unification, diagnostics, `<<-` writes).
    fn infer_body_with_capture_discovery(&mut self, body: ExprId, pending_mark: usize) -> Ty<'db> {
        let saved_repass = mem::replace(&mut self.capture_repass_needed, false);
        let body_mark = self.environment.mark();
        let body_snapshot = self.table.snapshot();
        let errors_mark = self.errors.len();
        let origins_mark = self.strict_origins.len();
        let mut trailing_ty = self.infer(body);
        if self.capture_repass_needed {
            self.environment.rollback(body_mark);
            self.table.rollback(body_snapshot);
            self.errors.truncate(errors_mark);
            self.strict_origins.truncate(origins_mark);
            self.pending_enclosing_writes.truncate(pending_mark);
            if let Some(frame) = self.return_frames.last_mut() {
                frame.clear();
            }
            self.capture_repass_needed = false;
            trailing_ty = self.infer(body);
        }
        self.capture_repass_needed = saved_repass;
        trailing_ty
    }

    /// `return(x)` exits the function, so a body that always ends in an exit contributes no
    /// trailing value: `function() return(TRUE)` returns `logical`, not `logical | NULL`.
    fn join_early_returns(
        &self,
        early_returns: &[(Ty<'db>, TextRange)],
        trailing: Ty<'db>,
        body: ExprId,
    ) -> Ty<'db> {
        if early_returns.is_empty() {
            return trailing;
        }
        let mut members: Vec<Ty<'db>> = early_returns.iter().map(|(ty, _)| *ty).collect();
        if !self.diverges(body) {
            members.push(self.table.resolve(self.db, trailing));
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
        let ty = self.infer_ordinary_call(id, range, callee, arguments);
        if let Some(verb) = self.dataframe_verb(callee) {
            return self.refine_dataframe_call(verb, arguments).unwrap_or(ty);
        }
        let function = self.table_function(callee);
        self.refine_table_call(id, function, arguments, ty)
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
                    let callees = self.hinting_callees(&candidates, arguments);
                    let call_arguments =
                        self.infer_call_arguments_against(range, arguments, &callees);
                    return self.overloaded_call(
                        id,
                        range,
                        callee,
                        name.to_string(),
                        &candidates,
                        &call_arguments,
                    );
                }
                let ty = match candidates.last() {
                    Some(scheme) => self.instantiate(scheme),
                    None => unknown(self.db),
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
        let named_callee = matches!(
            self.module.kind(callee),
            ExpressionKind::NameRef(_) | ExpressionKind::Namespace { .. }
        );
        let callback_outputs = if named_callee {
            self.callback_outputs(callee_ty, first_var..self.table.next_var().0)
        } else {
            Vec::new()
        };
        if self.is_null_placeholder(self.table.resolve(self.db, callee_ty)) {
            self.infer_call_arguments(range, arguments);
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        // A literal-`Any` callee is the escape hatch for stubs with no expressible
        // signature: the call is uncheckable, so findings inside its arguments are discarded.
        let resolved_callee = self.table.shallow_resolve(self.db, callee_ty);
        if matches!(resolved_callee.kind(self.db), TyKind::Any) {
            let recorded_errors = self.errors.len();
            let recorded_origins = self.strict_origins.len();
            self.infer_call_arguments(range, arguments);
            self.errors.truncate(recorded_errors);
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
        let result = self.dispatch_call(range, callee_range, callee_ty, &call_arguments);
        if callback_outputs.is_empty() {
            return result;
        }
        self.drop_unconstrained_members(result, &callback_outputs, first_var)
    }

    /// The callee's freshly instantiated type parameters (`instantiated`) that its parameter
    /// types mention only as what a callback returns, such as `F` in `Result.and_then`'s
    /// `f: fn(A) -> Result<B, F>`. Only those can stand for no value after the call: a
    /// parameter an argument fills directly, or one that appears only in the result, means
    /// "any value" when it stays open.
    fn callback_outputs(&self, callee_ty: Ty<'db>, instantiated: Range<u32>) -> Vec<InferenceVar> {
        let db = self.db;
        if instantiated.is_empty() {
            return Vec::new();
        }
        let TyKind::Function(function) = self.table.shallow_resolve(db, callee_ty).kind(db) else {
            return Vec::new();
        };
        let mut direct = FxHashSet::default();
        let mut outputs = FxHashSet::default();
        let parameter_types = function
            .positional
            .iter()
            .copied()
            .chain(function.named.iter().map(|parameter| parameter.ty))
            .chain(function.variadic.iter().map(|rest| rest.element));
        for ty in parameter_types {
            match ty.kind(db) {
                TyKind::Function(callback) => {
                    collect_vars(db, callback.ret, &mut outputs);
                    let inputs = callback
                        .positional
                        .iter()
                        .copied()
                        .chain(callback.named.iter().map(|parameter| parameter.ty))
                        .chain(callback.variadic.iter().map(|rest| rest.element));
                    for input in inputs {
                        collect_vars(db, input, &mut direct);
                    }
                }
                _ => collect_vars(db, ty, &mut direct),
            }
        }
        outputs
            .into_iter()
            .filter(|var| instantiated.contains(&var.0) && !direct.contains(var))
            .collect()
    }

    /// A callback output that no argument constrained stands for no value, so as a member of a
    /// union in the result it adds nothing: `Result.and_then`'s `Result<B, E | F>` is `:Err(E)`
    /// when the step never fails. A union of nothing but such parameters stays as it is, and a
    /// parameter that a variable from before the call reaches (an unannotated outer parameter
    /// passed as the callback) stays too, because its value is decided by that outer code.
    fn drop_unconstrained_members(
        &mut self,
        result: Ty<'db>,
        callback_outputs: &[InferenceVar],
        first_var: u32,
    ) -> Ty<'db> {
        let db = self.db;
        let mut open: FxHashSet<InferenceVar> = callback_outputs
            .iter()
            .map(|&var| self.table.find(var))
            .filter(|&root| matches!(self.table.entry(root), Entry::Unbound { .. }))
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
        for outer in 0..first_var {
            let outer = InferenceVar(outer);
            let mut reached = FxHashSet::default();
            match self.table.entry(outer) {
                Entry::Unbound { .. } => {
                    reached.insert(self.table.find(outer));
                }
                Entry::Bound { ty, .. } => {
                    let ty = self.table.resolve(db, *ty);
                    collect_vars(db, ty, &mut reached);
                }
                Entry::Redirect(_) => {}
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
                .and_then(|globals| globals.scheme(non_local, deferred))
            else {
                return FunctionLookup::Ordinary;
            };
            if self.callable(scheme.body) != Callable::No {
                return FunctionLookup::Ordinary;
            }
            if let Some(enclosing) = self
                .globals
                .and_then(|globals| globals.enclosing_scheme(non_local))
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
            false => globals.scheme(&name, deferred),
            true => globals.enclosing_scheme(&name),
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

    fn read_as_value(&mut self, slot: BindingId) -> bool {
        let naming = self.naming;
        self.value_read_slots
            .get_or_insert_with(|| {
                naming
                    .resolutions
                    .iter()
                    .filter(|(read, _)| !naming.callee_fallbacks.contains_key(read))
                    .map(|(_, slot)| *slot)
                    .collect()
            })
            .contains(&slot)
    }

    fn is_top_level_slot(&self, slot: BindingId) -> bool {
        self.naming
            .bindings
            .get(&slot)
            .is_some_and(|binding| binding.kind == BindingKind::TopLevel)
    }

    /// The library's candidates for `name` when the last one, which a plain read takes, is a
    /// function.
    fn library_function(&mut self, name: Symbol) -> Option<Vec<TypeScheme<'db>>> {
        let candidates = self.globals?.library_candidates(&name)?;
        match candidates.last() {
            Some(last) if self.callable(last.body) == Callable::No => None,
            _ => Some(candidates),
        }
    }

    /// Whether a value of type `ty` can be called. An unconstrained variable and a type
    /// parameter are `Untyped`, since nothing says whether they hold a function.
    fn callable(&mut self, ty: Ty<'db>) -> Callable {
        let resolved = self.structural(ty);
        match resolved.kind(self.db) {
            TyKind::Function(_) | TyKind::Any | TyKind::Unknown => Callable::Yes,
            // A nominal left after projection is opaque (`matrix`, `Date`, `data.frame`): an R
            // object with a class, never a closure, so R's function lookup skips it.
            TyKind::Named(..) => Callable::No,
            TyKind::Rigid(_) => Callable::Untyped,
            TyKind::Var(_) => match self.table.open_constraint(self.db, resolved) {
                Some(Constraint::Unconstrained) => Callable::Untyped,
                _ => Callable::No,
            },
            TyKind::Union(members) => {
                let members = members.clone();
                members
                    .into_iter()
                    .map(|member| self.callable(member))
                    .max()
                    .unwrap_or(Callable::No)
            }
            TyKind::Null
            | TyKind::Scalar(_)
            | TyKind::Vector(_)
            | TyKind::NamedVector(_)
            | TyKind::List(_)
            | TyKind::NamedList(_)
            | TyKind::Tuple(_)
            | TyKind::Record(_)
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
            || self
                .globals
                .is_some_and(|globals| globals.defined_in_project("DataFrame", deferred))
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
            let name = argument
                .name
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
            TyKind::Frame(FrameKind::DataFrame, columns),
        ))
    }

    /// The data.table function a call names, when the name reaches its stub: a local or
    /// project definition of the same name is an ordinary function.
    fn table_function(&self, callee: ExprId) -> Option<TableFunction> {
        let ExpressionKind::NameRef(name) = self.module.kind(callee) else {
            return None;
        };
        let function = match name.as_str() {
            "fread" => TableFunction::Fread,
            "setnames" => TableFunction::SetNames,
            "setcolorder" => TableFunction::SetColumnOrder,
            "setkey" => TableFunction::SetKey,
            "setorder" => TableFunction::SetOrder,
            "setkeyv" | "setorderv" => TableFunction::SetKeyOrOrderNamed,
            "rbindlist" => TableFunction::RbindList,
            "fifelse" => TableFunction::Fifelse,
            "melt" => TableFunction::Melt,
            _ => return None,
        };
        self.reaches_library(callee).then_some(function)
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
                && !globals.defined_in_project(name, deferred)
                && globals.scheme(name, deferred).is_some()
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
            let ty = self.fread_type(arguments);
            return match ty.kind(db) {
                TyKind::Frame(_, Some(columns)) => {
                    Some(Ty::new(db, TyKind::Frame(kind, Some(columns.clone()))))
                }
                _ => None,
            };
        }
        let subject_ty = *self.recorded.get(&subject)?;
        let TyKind::Frame(subject_kind, Some(columns)) =
            self.structural(subject_ty).kind(db).clone()
        else {
            return None;
        };
        let frame = variable_name(module, subject);
        let rest = &arguments[1..];
        let expressions = |checker: &mut Self, keep: &dyn Fn(&str) -> bool| {
            let mut computed = Vec::with_capacity(rest.len());
            let mut known = true;
            for argument in rest {
                let Some(value) = argument.value else {
                    continue;
                };
                if argument.name.as_deref().is_some_and(|name| !keep(name)) {
                    continue;
                }
                match checker.frame_argument(value, &columns, frame) {
                    Some(mut column) => {
                        if let Some(name) = &argument.name {
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
                let mut result = existing(&columns);
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
                for (name, ty) in existing(&columns) {
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
            DataFrameVerb::Join { lazy, asof } => self.join_type(lazy, asof, &columns, frame, rest),
            DataFrameVerb::Convert(_) if matches!(subject_kind, FrameKind::GroupBy { .. }) => None,
            DataFrameVerb::Convert(kind) => Some(Ty::new(db, TyKind::Frame(kind, Some(columns)))),
            DataFrameVerb::ReadCsv(_) => None,
        }
    }

    /// `DataFrame.join(left, right, on = "id")` has the left's columns, then the right's columns
    /// other than its keys, with `suffix` added to a name the left already has. A semi or anti
    /// join keeps only the left's columns. `DataFrame.join_asof(left, right, on, by)` joins on
    /// `by` and then `on`, with a left join's columns.
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
        let matched = match_formals(arguments, formals);
        let other = matched[0]?;
        let other_ty = *self.recorded.get(&other)?;
        let right = match self.structural(other_ty).kind(db).clone() {
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
                _ => return None,
            };
            (how, suffix, left_keys, right_keys)
        };
        let other_name = variable_name(module, other);
        // `on` names each key once for both frames, so a key missing from both is reported once,
        // against the left frame.
        let mut reported: Vec<TextRange> = Vec::new();
        for (keys, columns, name) in [
            (&left_keys, left, frame),
            (&right_keys, right.as_slice(), other_name),
        ] {
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
            for column in &right {
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
                    "col" => {
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
                    "lit" => {
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
                        let ty = match element_atomic(db, column.ty) {
                            Some(Atomic::Logical | Atomic::Integer) => vector(Atomic::Integer),
                            _ => vector(Atomic::Double),
                        };
                        Some(aggregate(column, ty))
                    }
                    "Expr.mean" | "Expr.median" | "Expr.std" | "Expr.var" => {
                        let column = argument(self)?;
                        Some(aggregate(column, vector(Atomic::Double)))
                    }
                    "Expr.min" | "Expr.max" | "Expr.first" | "Expr.last" => {
                        let column = argument(self)?;
                        let ty = column.ty;
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
                    "Expr.is_null" => {
                        let column = argument(self)?;
                        Some(ComputedColumn {
                            ty: vector(Atomic::Logical),
                            ..column
                        })
                    }
                    "Expr.cor" => {
                        let column = argument(self)?;
                        self.frame_operand(module[arguments].get(1)?.value?, columns, frame)?;
                        Some(aggregate(column, vector(Atomic::Double)))
                    }
                    "Expr.if_else" => {
                        self.frame_operand(first?, columns, frame)?;
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
                    UnaryOperator::Not => Some(ComputedColumn {
                        ty: vector(Atomic::Logical),
                        ..column
                    }),
                    UnaryOperator::Minus | UnaryOperator::Plus => Some(column),
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
        let ty = *self.recorded.get(&value)?;
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
            Some(TableFunction::Fread) => Some(self.fread_type(arguments)),
            Some(TableFunction::SetNames) => self.set_names(arguments),
            Some(TableFunction::SetColumnOrder) => self.set_column_order(arguments),
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
        if let Some(&slot) = self.naming.column_updates.get(&id) {
            let written = match (refined, arguments.first().and_then(|first| first.value)) {
                (Some(ty), _) => Some(ty),
                (None, Some(subject)) => self.recorded.get(&subject).copied(),
                (None, None) => None,
            };
            if let Some(written) = written {
                self.environment.set(slot, EnvEntry::Mono(written));
                self.note_captured_write(slot, written);
            }
        }
        refined.unwrap_or(ordinary)
    }

    /// The columns of the frame an argument evaluated to, when they are known.
    fn frame_columns_of(&mut self, value: ExprId) -> Option<Vec<RecordField<'db>>> {
        let ty = *self.recorded.get(&value)?;
        match self.structural(ty).kind(self.db) {
            TyKind::Frame(FrameKind::DataFrame | FrameKind::DataTable, Some(columns)) => {
                Some(columns.clone())
            }
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
        let matched = match_formals(arguments, &["input", "file", "text", "cmd"]);
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
            let Some(name) = argument.name.as_deref() else {
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
        let mut columns: Vec<(String, Ty<'db>)> = layout
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
            .collect();
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

    fn data_file(&self, path: &str) -> Option<sniff::Layout> {
        let normalized = path.strip_prefix("./").unwrap_or(path);
        data_file_layout(self.db, DataPath::new(self.db, normalized.to_owned())).clone()
    }

    /// `setnames(f, old, new)` renames the named columns and `setnames(f, new)` renames
    /// them all.
    fn set_names(&mut self, arguments: &[Argument]) -> Option<Ty<'db>> {
        let module = self.module;
        let db = self.db;
        let matched = match_formals(arguments, &["x", "old", "new", "skip_absent"]);
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
        let matched = match_formals(arguments, &["x", "neworder", "before", "after"]);
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
        let matched = match_formals(arguments, &["x", "cols"]);
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
        let matched = match_formals(arguments, &["l", "use.names", "fill", "idcol"]);
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
        let matched = match_formals(arguments, &["test", "yes", "no", "na"]);
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
            argument.name.as_deref() == Some("variable.factor")
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
        let method = format!("c.{}", name.text(self.db));
        self.globals?
            .scheme(&method, false)
            .map(|_| nominal)
            .or_else(|| {
                self.globals?
                    .overloads(&method, false)
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
                Some((argument, value, self.structural(ty)))
            })
            .collect();
        let inferred: Vec<Ty<'db>> = supplied.iter().map(|&(_, _, ty)| ty).collect();
        if inferred
            .iter()
            .any(|&ty| self.combined_list_element(ty).is_some())
        {
            // `c()` drops `NULL`, so `c(if (a) list(1), if (a) list(2))` is a list.
            let elements: Vec<Ty<'db>> = inferred
                .iter()
                .filter(|ty| !matches!(ty.kind(self.db), TyKind::Null))
                .map(|&ty| self.combined_list_element(ty).unwrap_or(ty))
                .collect();
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
            let members = match resolved.kind(self.db) {
                TyKind::Union(members) => members.clone(),
                _ => vec![resolved],
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

    /// The element a list-shaped `c()` argument contributes; a `NULL` member of a union
    /// contributes nothing, as `c()` drops it.
    fn combined_list_element(&self, ty: Ty<'db>) -> Option<Ty<'db>> {
        let TyKind::Union(members) = ty.kind(self.db) else {
            return self.list_element_type(ty);
        };
        let elements: Option<Vec<Ty<'db>>> = members
            .iter()
            .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
            .map(|&member| self.list_element_type(member))
            .collect();
        elements.map(|elements| union_of(self.db, elements))
    }

    fn list_element_type(&self, ty: Ty<'db>) -> Option<Ty<'db>> {
        match ty.kind(self.db) {
            TyKind::List(element) | TyKind::NamedList(element) => Some(*element),
            TyKind::Tuple(items) => Some(union_of(self.db, items.clone())),
            TyKind::Record(fields) => Some(union_of(self.db, fields.iter().map(|field| field.ty))),
            _ => None,
        }
    }

    /// Attributes are not part of a type, so the call has its first argument's type.
    /// `dim` is the exception: array shape is untracked, so it falls through to the
    /// declaration. `None` means not intercepted.
    fn infer_structure(&mut self, id: ExprId, arguments: &[Argument]) -> Option<Ty<'db>> {
        let shape_changing = arguments.iter().any(|argument| {
            argument
                .name
                .as_deref()
                .is_some_and(|name| matches!(name, "dim" | ".Dim"))
        });
        if shape_changing {
            return None;
        }
        // `.Data` is the formal's name, so it may be given either way.
        let value = arguments
            .iter()
            .find(|argument| argument.name.as_deref() == Some(".Data"))
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
            return Ty::new(self.db, TyKind::Tuple(Vec::new()));
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
            match &argument.name {
                Some(name) => fields.push(RecordField {
                    name: Name::new(self.db, name.as_str()),
                    ty,
                    optional: false,
                }),
                None => items.push(ty),
            }
        }
        if all_named {
            Ty::new(self.db, TyKind::Record(fields))
        } else {
            Ty::new(self.db, TyKind::Tuple(items))
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
            match branch.name {
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
                self.join_writes(writes);
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
                name.to_string()
            }
            // Namespace access cannot be shadowed by a local binding.
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => {
                let name = validated_namespace_name(self.db, *internal, package, name)?;
                if !namespace_reaches_project(self.db, package) {
                    let schemes = self.globals?.library_candidates(&name)?;
                    if schemes.len() < 2 {
                        return None;
                    }
                    let callees = self.hinting_callees(&schemes, arguments);
                    let call_arguments =
                        self.infer_call_arguments_against(range, arguments, &callees);
                    return Some(self.overloaded_call(
                        id,
                        range,
                        callee,
                        name,
                        &schemes,
                        &call_arguments,
                    ));
                }
                name
            }
            _ => return None,
        };
        let schemes = self
            .globals?
            .overloads(&name, self.naming.deferred_non_locals.contains(&callee))?;
        if schemes.len() < 2 {
            return None;
        }

        // Arguments infer once, before any probe: the probe snapshot does not reverse
        // environment or recorded-type writes.
        let callees = self.hinting_callees(&schemes, arguments);
        let mut call_arguments = self.infer_call_arguments_against(range, arguments, &callees);
        self.resolve_match_fun_arguments(callee, &mut call_arguments);
        if let Some(ty) = self.s3_method_call(range, &name, &call_arguments) {
            return Some(ty);
        }
        Some(self.overloaded_call(id, range, callee, name, &schemes, &call_arguments))
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
                let method = format!("{generic}.{class}");
                globals
                    .overloads(&method, false)
                    .or_else(|| globals.scheme(&method, false).map(|scheme| vec![scheme]))
                    .filter(|candidates| !candidates.is_empty())
            })?;
        for scheme in candidates {
            let snapshot = self.table.snapshot();
            let instantiated = self.instantiate(&scheme);
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
        name: String,
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
            && call_arguments.iter().any(|argument| {
                argument.ty.is_some_and(|ty| {
                    // `c(x, unknown)` is as unchecked as `unknown` itself: its element type
                    // fits every candidate too.
                    let resolved = self.table.shallow_resolve(self.db, ty);
                    let ty = match resolved.kind(self.db) {
                        TyKind::Vector(element)
                        | TyKind::NamedVector(element)
                        | TyKind::List(element)
                        | TyKind::NamedList(element) => {
                            self.table.shallow_resolve(self.db, *element)
                        }
                        _ => resolved,
                    };
                    matches!(ty.kind(self.db), TyKind::Unknown | TyKind::Any)
                })
            });

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
                        let narrowing = caller_entries
                            .iter()
                            .map(|(var, _)| {
                                self.table
                                    .resolve(self.db, Ty::new(self.db, TyKind::Var(*var)))
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

        // Without a free fit, a guess is taken only when every guess narrows the caller's
        // types alike; disagreeing narrowings make the variable `Unknown` (unchecked, not wrong).
        if unchecked_argument
            && let Some((_, _, first)) = fits.first()
            && fits.iter().any(|(_, _, results)| results != first)
            && fits
                .iter()
                .any(|(_, _, results)| results.iter().any(|&result| !self.is_list_result(result)))
        {
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        let winner = fits.iter().find(|(_, free, _)| *free).or_else(|| {
            let (_, _, first) = fits.first()?;
            fits.iter()
                .all(|(_, _, narrowing)| narrowing == first)
                .then(|| fits.first())
                .flatten()
        });
        if winner.is_none()
            && let Some((_, _, first)) = fits.first()
        {
            for (position, (var, _)) in caller_entries.iter().enumerate() {
                if fits
                    .iter()
                    .any(|(_, _, narrowing)| narrowing.get(position) != first.get(position))
                {
                    let var = Ty::new(self.db, TyKind::Var(*var));
                    let unknown = unknown(self.db);
                    if self.table.unify(self.db, var, unknown).is_err() {
                        return unknown;
                    }
                }
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

        // Report the overload-set finding unless one answer exists: every candidate failed
        // for the same reason, or exactly one got strictly further into the call (its first
        // blamed argument is the deepest; a whole-call verdict blames none).
        // The standard library declares an Array twin before each `List` signature. When the
        // argument is not an Array at all, that twin's failure explains nothing, so the finding
        // comes from the remaining candidates.
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
            && blamed
                .iter()
                .filter(|blame| **blame == Some(deepest))
                .count()
                == 1
        {
            blamed
                .iter()
                .position(|blame| *blame == Some(deepest))
                .and_then(|index| candidate_errors.get(index))
        } else {
            None
        };
        let error = match single_answer {
            Some(error) => error.clone(),
            None => TypeError {
                range,
                kind: TypeErrorKind::NoMatchingOverload {
                    name,
                    candidates: schemes.len(),
                    first: candidate_errors.first().cloned().map(Box::new),
                },
            },
        };
        self.errors.push(error);
        self.recorded.insert(callee, unknown(self.db));
        // A capture-discovery re-pass must not keep a stale commitment from the first pass.
        self.selected_overloads.remove(&callee);
        unknown(self.db)
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
    ) -> Result<(Ty<'db>, FunctionType<'db>), Vec<TypeError<'db>>> {
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
            Ok((resolved, function.clone()))
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

    /// Infers a call's arguments, typing an unannotated function literal last and in context:
    /// its formals take the parameter types of the function the callee expects at its
    /// position, once the other arguments have bound the callee's type variables. In
    /// `List.map(xs, \(x) c(x, x))`, `x` is the item type of `xs` before the body infers, so
    /// `c(x, x)` types concretely instead of over an open variable. `callees` are the
    /// instantiated candidate signatures; a hint is used only where every candidate that
    /// accepts the other arguments agrees on it.
    fn infer_call_arguments_against(
        &mut self,
        range: TextRange,
        arguments: &[Argument],
        callees: &[Ty<'db>],
    ) -> Vec<CallArgument<'db>> {
        let deferred: Vec<bool> = arguments
            .iter()
            .map(|argument| {
                !callees.is_empty()
                    && argument
                        .value
                        .is_some_and(|value| self.needs_parameter_hints(value))
            })
            .collect();
        let mut call_arguments: Vec<CallArgument<'db>> = arguments
            .iter()
            .zip(&deferred)
            .map(|(argument, &deferred)| {
                let ty = if deferred {
                    None
                } else {
                    argument.value.map(|value| self.infer(value))
                };
                self.call_argument(range, argument, ty)
            })
            .collect();
        if !deferred.contains(&true) {
            return call_arguments;
        }
        let hints = self.parameter_hints_for(callees, &call_arguments, &deferred);
        for (index, argument) in arguments.iter().enumerate() {
            let Some(value) = argument.value.filter(|_| deferred[index]) else {
                continue;
            };
            if let Some(hint) = hints.get(index).cloned().flatten() {
                self.parameter_hints.insert(value, hint);
            }
            let errors_before = self.errors.len();
            call_arguments[index].ty = Some(self.infer(value));
            self.name_contextual_parameters(value, errors_before);
        }
        call_arguments
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
        let forwards_dots = argument.name.is_none()
            && argument.value.is_some_and(|value| {
                matches!(
                    &self.module.expression(value).kind,
                    ExpressionKind::NameRef(name) if name == "..."
                )
            });
        CallArgument {
            name: argument.name.map(|name| name.to_string()),
            name_range: argument.name_range,
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
    fn name_contextual_parameters(&mut self, function: ExprId, errors_before: usize) {
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
                };
            }
        }
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

    /// Per argument, the parameter types its deferred function literal takes, found by
    /// probing each candidate with the already-inferred arguments and rolling back.
    fn parameter_hints_for(
        &mut self,
        callees: &[Ty<'db>],
        arguments: &[CallArgument<'db>],
        deferred: &[bool],
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
            let function = function.clone();
            let targets = self.argument_targets(&function, arguments);
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
                .zip(deferred)
                .filter(|(_, deferred)| !**deferred)
                .filter_map(|(target, _)| target_ty(target))
                .filter_map(
                    |ty| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
                        TyKind::Var(var) => Some(*var),
                        _ => None,
                    },
                )
                .collect();
            let accumulators: Vec<Vec<bool>> = targets
                .iter()
                .zip(deferred)
                .map(|(target, &deferred)| {
                    let expected = target_ty(target)
                        .filter(|_| deferred)
                        .map(|expected| self.table.shallow_resolve(self.db, expected));
                    let Some(TyKind::Function(expected)) = expected.map(|ty| ty.kind(self.db))
                    else {
                        return Vec::new();
                    };
                    let mut result_vars = FxHashSet::default();
                    self.table
                        .collect_unbound_vars(self.db, expected.ret, &mut result_vars);
                    result_vars.retain(|var| state_vars.contains(var));
                    expected
                        .positional
                        .iter()
                        .copied()
                        .chain(expected.named.iter().map(|parameter| parameter.ty))
                        .map(|formal| {
                            let mut formal_vars = FxHashSet::default();
                            self.table
                                .collect_unbound_vars(self.db, formal, &mut formal_vars);
                            !formal_vars.is_disjoint(&result_vars)
                        })
                        .collect()
                })
                .collect();
            let snapshot = self.table.snapshot();
            let accepts = arguments.iter().zip(&targets).zip(deferred).all(
                |((argument, target), &deferred)| {
                    if deferred {
                        return true;
                    }
                    match (argument.ty, target_ty(target)) {
                        (Some(actual), Some(expected)) => {
                            self.table.compatible(self.db, actual, expected)
                        }
                        // An argument the candidate has no parameter for rules the candidate out.
                        (Some(_), None) => false,
                        _ => true,
                    }
                },
            );
            let candidate: Vec<Option<Vec<Option<Ty<'db>>>>> = targets
                .iter()
                .zip(deferred)
                .zip(&accumulators)
                .map(|((target, &deferred), accumulators)| {
                    if !deferred || !accepts {
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
                    Some(
                        formals
                            .enumerate()
                            .map(|(index, formal)| {
                                let formal = self.table.resolve(self.db, formal);
                                (!accumulators.get(index).copied().unwrap_or(false)
                                    && !self.table.contains_unbound_var(self.db, formal)
                                    && !matches!(
                                        formal.kind(self.db),
                                        TyKind::Any | TyKind::Unknown
                                    ))
                                .then_some(formal)
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
                        .is_some_and(|globals| globals.defined_in_project(name, deferred))
                {
                    return;
                }
                name.as_str()
            }
            ExpressionKind::Namespace {
                package: Some(package),
                name: Some(name),
                ..
            } if package.as_str() == STANDARD_LIBRARY => name.as_str(),
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
            if !is_character(self.db, resolved) {
                continue;
            }
            let named = match self.module.kind(value) {
                ExpressionKind::Literal(LiteralKind::String(text)) => self
                    .globals
                    .and_then(|globals| globals.scheme(&self.module[text], false))
                    .map(|scheme| self.instantiate(&scheme)),
                _ => None,
            };
            argument.named_function = Some(named.unwrap_or_else(|| unknown(self.db)));
        }
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
                let function = function.clone();
                let findings = self.match_arguments(callee_range, &function, arguments);
                self.errors.extend(findings);
                function.ret
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
                    TyKind::Function(FunctionType {
                        positional,
                        named: arguments
                            .iter()
                            .filter_map(|argument| {
                                argument.name.as_ref().map(|name| Parameter {
                                    name: Name::new(self.db, name.clone()),
                                    ty: argument.ty.unwrap_or_else(|| unknown(self.db)),
                                    optional: false,
                                    default: None,
                                })
                            })
                            .collect(),
                        variadic: forwards_dots.then(|| RestParameter {
                            element: any(self.db),
                            preceding_named: 0,
                        }),
                        ret,
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
                self.dispatch_call(range, callee_range, without_null, arguments)
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
                self.dispatch_call(range, callee_range, functions, arguments);
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
                let members = members.clone();
                let caller_entries = self.caller_open_variables(arguments);
                let mut narrowed = false;
                let mut returns = Vec::with_capacity(members.len());
                for member in members {
                    let member = self.table.shallow_resolve(self.db, member);
                    let TyKind::Function(function) = member.kind(self.db) else {
                        continue;
                    };
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
                self.dispatch_call(range, callee_range, functions, arguments);
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

    /// Checks every argument (a failed `compatible` leaves the table untouched, so
    /// verdicts are independent); empty means the call matches. A structural failure is
    /// returned alone: per-argument mismatches under a mis-shaped call mislead.
    fn match_arguments(
        &mut self,
        callee_range: TextRange,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Vec<TypeError<'db>> {
        let total = function.positional.len() + function.named.len();
        let required = function.positional.len()
            + function
                .named
                .iter()
                .filter(|field| !field.optional)
                .count();
        let variadic_element = function.variadic.as_ref().map(|rest| rest.element);
        let targets = self.argument_targets(function, arguments);

        // A function-typed argument may be checked against arguments forwarded to it later
        // (`lapply(x, gsub, pattern = "a")`), so the rest intake must be known up front.
        let forwarded_argument_indexes: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(_, target)| matches!(target, Some(ArgumentTarget::Rest)))
            .map(|(index, _)| index)
            .collect();

        let forwards_dots = arguments.iter().any(|argument| argument.forwards_dots);
        let mut findings = Vec::new();
        // An unknown name throws off positional accounting, so its arity verdict would be noise.
        let mut unknown_name = false;
        for (index, argument) in arguments.iter().enumerate() {
            let Some(target) = targets.get(index).and_then(Option::as_ref) else {
                continue;
            };
            match target {
                ArgumentTarget::Positional(slot) => {
                    if let Some(&expected) = function.positional.get(*slot)
                        && let Some(ty) = self.argument_type_for(expected, argument)
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
                    if let Some(expected) = function.named.get(*formal).map(|field| field.ty)
                        && let Some(ty) = self.argument_type_for(expected, argument)
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
                        && let Some(ty) = self.argument_type_for(element, argument)
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
                    if !forwards_dots {
                        let positional_arguments = arguments
                            .iter()
                            .filter(|argument| argument.name.is_none())
                            .count();
                        let claimed_by_name = targets
                            .iter()
                            .zip(arguments)
                            .filter(|(target, argument)| {
                                argument.name.is_some()
                                    && matches!(target, Some(ArgumentTarget::Named(_)))
                            })
                            .count();
                        return vec![TypeError {
                            // The first argument with no formal left is the one to remove.
                            range: argument.range,
                            kind: TypeErrorKind::ArityMismatch {
                                expected: total.saturating_sub(claimed_by_name),
                                found: positional_arguments,
                            },
                        }];
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

        let positional_filled = targets
            .iter()
            .filter(|target| matches!(target, Some(ArgumentTarget::Positional(_))))
            .count();
        if forwards_dots || unknown_name {
            return findings;
        }
        if positional_filled != function.positional.len() {
            return vec![TypeError {
                range: callee_range,
                kind: TypeErrorKind::ArityMismatch {
                    expected: required,
                    found: arguments.len(),
                },
            }];
        }
        let missing: Vec<String> = function
            .named
            .iter()
            .enumerate()
            .filter(|(formal, field)| {
                !field.optional
                    && !targets.iter().any(
                        |target| matches!(target, Some(ArgumentTarget::Named(filled)) if filled == formal),
                    )
            })
            .map(|(_, field)| field.name.text(self.db).to_owned())
            .collect();
        if !missing.is_empty() {
            return vec![TypeError {
                range: callee_range,
                kind: TypeErrorKind::MissingArguments { names: missing },
            }];
        }
        findings
    }

    /// A function name given as a string stands for the function only where a function is
    /// expected; anywhere else it is the string it looks like.
    fn argument_type_for(
        &self,
        expected: Ty<'db>,
        argument: &CallArgument<'db>,
    ) -> Option<Ty<'db>> {
        if let Some(named) = argument.named_function
            && matches!(
                self.table.shallow_resolve(self.db, expected).kind(self.db),
                TyKind::Function(_)
            )
        {
            return Some(named);
        }
        let ty = argument.ty?;
        match argument.value {
            Some(value) if self.names_computed_value(value) => {
                let ty = self.without_inferred_null(expected, ty);
                Some(self.without_inferred_length(expected, ty))
            }
            _ => Some(ty),
        }
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
                .is_some_and(|slot| !self.computed_slots.contains(slot)))
    }

    fn names_computed_value(&self, value: ExprId) -> bool {
        matches!(
            self.module.expression(value).kind,
            ExpressionKind::NameRef(_)
        ) && self
            .naming
            .resolutions
            .get(&value)
            .is_some_and(|slot| self.computed_slots.contains(slot))
    }

    /// A variable the body assigns, from a `switch` without a default, an `if` without `else`
    /// or a helper that returns `NULL` on some path, is `NULL` only where the code presumably
    /// does not pass it on: rlang's `type <- switch(type, ...)` feeds `sprintf(type, kind)`
    /// after every type has an arm. So an argument naming such a variable drops its `NULL`
    /// when the parameter's type is settled, or bounded, and admits no `NULL`. The `NULL` of an
    /// undeclared formal, from a `NULL` default or an `is.null()` test, is the same kind of
    /// placeholder. The `NULL` of a declared parameter, or of an annotated assignment, stays.
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

    /// A variable the body computes as `character | character[]`, through a helper that returns
    /// a vector on some path, is presumably one string where a parameter takes one: renv's
    /// `pattern <- sprintf("^%s=", name)` feeds `sub(pattern, "", spec)`. So such an argument
    /// passes to a scalar parameter of its atomic type as that scalar.
    fn without_inferred_length(&self, expected: Ty<'db>, ty: Ty<'db>) -> Ty<'db> {
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
        let fits = match resolved.kind(self.db) {
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

    /// R matches exact names before placing positionals, so `vapply(xs, character(1),
    /// FUN = f)` sends `character(1)` to `FUN.VALUE`. Positionals then fill fixed
    /// parameters, then unclaimed named formals before `...`. `None` marks a `...` forward.
    fn argument_targets(
        &self,
        function: &FunctionType<'db>,
        arguments: &[CallArgument<'db>],
    ) -> Vec<Option<ArgumentTarget>> {
        let mut targets: Vec<Option<ArgumentTarget>> = arguments.iter().map(|_| None).collect();
        let mut claimed = vec![false; function.named.len()];

        for (index, argument) in arguments.iter().enumerate() {
            if argument.forwards_dots {
                continue;
            }
            let Some(name) = &argument.name else { continue };
            let formal = function
                .named
                .iter()
                .enumerate()
                .position(|(formal, field)| {
                    field.name.text(self.db) == name.as_str() && !claimed[formal]
                });
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
            let Some(name) = &argument.name else { continue };
            let mut candidates = function.named[..pre_rest]
                .iter()
                .enumerate()
                .filter(|(formal, field)| {
                    !claimed[*formal] && field.name.text(self.db).starts_with(name.as_str())
                })
                .map(|(formal, _)| formal);
            if let (Some(formal), None) = (candidates.next(), candidates.next()) {
                claimed[formal] = true;
                targets[index] = Some(ArgumentTarget::Named(formal));
            }
        }

        let mut next_positional = 0usize;
        let mut next_named = 0usize;
        for (index, argument) in arguments.iter().enumerate() {
            if argument.forwards_dots || targets[index].is_some() {
                continue;
            }
            targets[index] = Some(match &argument.name {
                // R collects unmatched keywords into `...`; a duplicate of a supplied formal, or
                // any unmatched name on a non-variadic function, stays an error.
                Some(name) => {
                    let duplicates_declared = function
                        .named
                        .iter()
                        .any(|field| field.name.text(self.db) == name.as_str());
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
        targets
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
        let (TyKind::Function(expected_callback), TyKind::Function(actual_callback)) =
            (expected.kind(self.db), actual.kind(self.db))
        else {
            return false;
        };
        let snapshot = self.table.snapshot();
        let verdict = 'probe: {
            let mut open_positionals = actual_callback.positional.clone();
            let mut open_named = actual_callback.named.clone();
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
                if let Some(ty) = argument.ty
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
                .map(|element| (Some(element), callback_range, false));
            let forwarded = forwarded_positionals.iter().map(|&index| {
                let argument = &arguments[index];
                (argument.ty, argument.range, argument.whole_double)
            });
            for (argument_ty, blame_range, whole_double) in elements.chain(forwarded) {
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
                && self
                    .table
                    .compatible(self.db, actual_callback.ret, expected_callback.ret)
        };
        if !verdict {
            self.table.rollback(snapshot);
        }
        verdict
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
        if matches!(resolved_found.kind(self.db), TyKind::Unknown) {
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
        // idiomatic. The name is not resolved, so the call is accepted unchecked.
        if matches!(resolved_expected.kind(self.db), TyKind::Function(_))
            && is_character(self.db, resolved_found)
        {
            return Ok(());
        }
        if let TyKind::Var(var) = resolved_expected.kind(self.db)
            && let Entry::Unbound {
                constraint: Constraint::Numeric,
                ..
            } = self.table.entry(*var)
        {
            return Err(TypeError {
                range,
                kind: TypeErrorKind::ConstraintViolation {
                    constraint: Constraint::Numeric,
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
            .and_then(|table| table.get(&name))
            .cloned()
        else {
            return self.generalize(value_ty);
        };
        if definition.alias {
            return self.generalize(value_ty);
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
        let nominal = Ty::new(self.db, TyKind::Named(name, arguments.clone()));
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

    /// Projects non-alias nominals to their representation. An undeclared nominal (a
    /// typo reported elsewhere) floors to `Unknown` so operator checks never cascade.
    fn structural(&mut self, ty: Ty<'db>) -> Ty<'db> {
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
                    current = self.table.resolve(self.db, representation);
                }
                None => break,
            }
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
        if let Some(&slot) = self.naming.column_updates.get(&id) {
            let written = if is_query { result } else { target_ty };
            self.environment.set(slot, EnvEntry::Mono(written));
            self.note_captured_write(slot, written);
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

    /// The return type a stub declares for `[.class` or `[[.class`. The index arguments are
    /// not checked against it: the stubs declare these methods only to say which class the
    /// subset keeps.
    fn index_method_result(&mut self, class: &str, double: bool) -> Option<Ty<'db>> {
        let method = format!("{}.{class}", if double { "[[" } else { "[" });
        let scheme = self.globals?.scheme(&method, false)?;
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
        let mut index_types = Vec::with_capacity(arguments.len());
        for argument in arguments {
            if let Some(value) = argument.value {
                index_types.push(self.infer(value));
            }
        }
        // A declared `data.table` subject makes `[` a query: index arguments evaluate in
        // the data's frame (masked column reads), and the `j` syntax decides the result class.
        if !double
            && matches!(subject.kind(self.db), TyKind::Named(name, _) if name.text(self.db) == "data.table")
        {
            for argument in arguments {
                if let Some(value) = argument.value {
                    self.mask_column_reads(value);
                }
            }
            return if data_table_keeps_class(self.module, arguments) {
                subject
            } else {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            };
        }
        // data.table syntax without a data.table-typed subject returns a shape no base
        // rule covers: silent `Unknown`.
        if self.naming.masked_subsets.contains(&id) {
            return unknown(self.db);
        }
        if let Some(element) = array_element(self.db, subject) {
            return self.array_index(range, double, subject, element, arguments, &index_types);
        }
        // R dispatches `[` on the class, so a stub's `[.factor` gives the result type. The
        // class is the declared one, before any representation projects it away.
        if let TyKind::Named(name, _) = self.table.resolve(self.db, target_ty).kind(self.db)
            && let Some(ty) = self.index_method_result(name.text(self.db), double)
        {
            return ty;
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
        let position = |db, ty: Ty<'db>, logical: bool| match ty.kind(db) {
            TyKind::Scalar(Atomic::Integer | Atomic::Double) => true,
            TyKind::Scalar(Atomic::Logical) => logical,
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
                .find(|argument| argument.name.as_deref() == Some(formal))
        };
        let (i, j) = query_index_arguments(arguments);
        let by = named("by").or_else(|| named("keyby"));
        let on = named("on");
        let frame = variable_name(module, target);
        let bracket = module.range(id);
        let subject_columns = frame_columns(db, subject);
        let mut callees = FxHashSet::default();
        for value in arguments.iter().filter_map(|argument| argument.value) {
            collect_callees(module, value, &mut callees);
        }
        let i_value = i.and_then(|argument| argument.value);
        let mut source_columns = subject_columns.clone();
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
                        x: subject_columns.clone(),
                        i: joined_columns.as_ref().map(|columns| {
                            columns
                                .iter()
                                .map(|column| RecordField {
                                    ty: if each_i {
                                        column_element(db, column.ty)
                                    } else {
                                        column.ty
                                    },
                                    ..column.clone()
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
                                    range: roll.name_range.map_or_else(
                                        || module.range(roll_value),
                                        |name| name.cover(module.range(roll_value)),
                                    ),
                                    kind: TypeErrorKind::RollingNonEquiJoin,
                                });
                            }
                            layout = self.join_columns(
                                &keys,
                                (frame, subject_columns.as_deref()),
                                (joined_name, joined_columns.as_deref()),
                            );
                            layout.as_ref().map(|layout| {
                                layout.iter().map(|column| column.field.clone()).collect()
                            })
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
                        ..column.field.clone()
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
                    compared
                        .into_iter()
                        .map(|column| column.field.clone())
                        .collect()
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
        let subset_data = Ty::new(db, TyKind::Frame(kind, subset_columns));
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
                    ),
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
            None => Ty::new(db, TyKind::Frame(kind, source_columns)),
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
                    ..column.clone()
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
                field: RecordField {
                    name,
                    ..column.clone()
                },
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
                            let name = argument.name.or_else(|| variable_name(module, item));
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
                            groups.push(column.clone());
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
                .map(|(column, _)| column.clone())
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
        let bound = bind_formals(&module[arguments], &["X", "FUN"]);
        let subset = bound.value(0)?;
        if variable_name(module, *callee).as_deref() != Some("lapply")
            || self.naming.resolutions.contains_key(callee)
            || !matches!(module.kind(subset), ExpressionKind::NameRef(name) if name == ".SD")
        {
            return None;
        }
        let subset_data = self.column_scopes.last()?.subset_data;
        let columns = match subset_data.kind(db) {
            TyKind::Frame(_, Some(columns)) if !columns.is_empty() => columns.clone(),
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
            for column in &columns {
                if let Some(scope) = self.column_scopes.last_mut() {
                    scope.subset_data = Ty::new(db, TyKind::Record(vec![column.clone()]));
                }
                let recorded_errors = self.errors.len();
                let ty = self.infer(value);
                findings.push(self.errors.split_off(recorded_errors));
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
            Some(applied) => Ty::new(db, TyKind::Record(applied.clone())),
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
                callback.name.clone(),
                &callback.schemes,
                &call_arguments,
            );
            results.push((ty, self.errors.split_off(recorded_errors)));
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
        let bound = bind_formals(arguments, formals);
        let (x, function) = if family == "Reduce" {
            (bound.value(1)?, bound.value(0)?)
        } else {
            (bound.value(0)?, bound.value(1)?)
        };
        let callback = self.callback(function)?;
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
                    && !argument
                        .name
                        .as_deref()
                        .is_some_and(|name| named_only.contains(&name))
            })
            .cloned()
            .collect();
        for argument in arguments {
            if let Some(value) = argument.value
                && argument
                    .name
                    .as_deref()
                    .is_some_and(|name| named_only.contains(&name))
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
            "sapply" => match self.table.resolve(db, element).kind(db) {
                // `sapply` simplifies results that all have length one into a vector.
                TyKind::Scalar(_) => vector(element),
                _ => unknown(db),
            },
            _ => list(element),
        })
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

    /// The overload set a callback passed by name stands for, unless a binding shadows it.
    fn callback(&self, function: ExprId) -> Option<Callback<'db>> {
        let ExpressionKind::NameRef(name) = self.module.kind(function) else {
            return None;
        };
        if self.naming.resolutions.contains_key(&function) {
            return None;
        }
        let deferred = self.naming.deferred_non_locals.contains(&function);
        let schemes = self.globals?.overloads(name, deferred)?;
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
            Ty::new(db, TyKind::Frame(kind, columns))
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
                let name = argument
                    .name
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
                if self.masked_reads.contains(&j)
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
                for field in fields.clone() {
                    let name = field.name.text(db).to_owned();
                    columns.push(self.column_field(&name, field.ty));
                }
            }
            _ => return frame_of(None),
        }
        frame_of(Some(columns))
    }

    /// `j` as `y := v`, `c("a", "b") := list(v, w)`, or `` `:=`(a = v, b = w) ``: the
    /// queried frame with each named column set to its value's vector type, or removed
    /// by `NULL`. A left side naming columns some other way widens to unknown columns.
    /// data.table coerces a value written into an existing column to the column's type, so the
    /// column keeps its type unless the value replaces it whole: with `partial` (an `i` or a
    /// `by` is present) or a literal value, the type stays. Only a literal is certainly one value;
    /// a scalar-like type is not, because `as.character(x)` is scalar-like too.
    fn column_update(
        &mut self,
        arguments: &[Argument],
        base: Option<Vec<RecordField<'db>>>,
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
                    match (argument.name, updates.as_mut()) {
                        (Some(name), Some(updates)) => {
                            updates.push((name.to_string(), ty, Some(value)));
                        }
                        _ => updates = None,
                    }
                }
            }
        }
        let mut columns = base?;
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
        let resolved = self.table.resolve(self.db, ty);
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
            for &member in members {
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
            TyKind::Vector(element) | TyKind::List(element) => Ok(*element),
            // A literal name may be absent at runtime (`T | NULL`).
            TyKind::NamedVector(element) | TyKind::NamedList(element) => {
                Ok(if literal_name.is_some() {
                    union_of(self.db, [*element, null(self.db)])
                } else {
                    *element
                })
            }
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
            TyKind::Tuple(items) if items.is_empty() => {
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
            TyKind::Record(fields) => {
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
            for &member in members {
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
            TyKind::Scalar(Atomic::Complex | Atomic::Raw) => Err(TypeError {
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
                for &member in members {
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
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return unknown(self.db);
        }
        let Some(name) = name else {
            return unknown(self.db);
        };
        let subject = self.structural(target_ty);
        match self.dollar_result(id, range, name_range, subject, &name) {
            Ok(ty) => ty,
            Err(error) => {
                self.errors.push(name_frame(error, self.module, target));
                unknown(self.db)
            }
        }
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
            let mut unfilled = false;
            for &member in members {
                // An empty `list()` member is the accumulator on a path that never filled
                // it, where R reads `NULL`.
                if is_empty_list(self.db, member) {
                    unfilled = true;
                    continue;
                }
                match self.dollar_result(origin, range, name_range, member, name) {
                    Ok(result) => results.push(result),
                    Err(error) if matches!(error.kind, TypeErrorKind::FieldDoesNotExist { .. }) => {
                        absent.get_or_insert(error);
                    }
                    Err(error) => return Err(widen_error_container(error, subject)),
                }
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
            TyKind::Record(fields) => {
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
            TyKind::Tuple(items) if items.is_empty() => {
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

    /// Differing openness joins by union, never unification: the concrete branch would
    /// pin the other (`function(flag, x) if (flag) x else "s"` would demand
    /// `x: character`). Two open branches do tie, which types the coalesce idiom.
    fn join_branch_values(&mut self, left: Ty<'db>, right: Ty<'db>) -> Ty<'db> {
        let left_resolved = self.table.resolve(self.db, left);
        let right_resolved = self.table.resolve(self.db, right);
        let left_open = self.table.contains_unbound_var(self.db, left_resolved);
        let right_open = self.table.contains_unbound_var(self.db, right_resolved);
        if left_open != right_open {
            let open = if left_open {
                left_resolved
            } else {
                right_resolved
            };
            // Only an unconstrained variable is protected: pinning an already-restricted one
            // adds no new requirement and lets recursion converge.
            if self.table.open_constraint(self.db, open) == Some(Constraint::Unconstrained) {
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
        let snapshot = self.table.snapshot();
        match self.table.unify(self.db, left, right) {
            Ok(()) => left,
            Err(_) => {
                self.table.rollback(snapshot);
                union_of(self.db, [left_resolved, right_resolved])
            }
        }
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

    /// Returns the slots whose entries changed: the loop fixed point's stability signal.
    fn join_writes(&mut self, writes: &[(BindingId, Option<EnvEntry<'db>>)]) -> Vec<BindingId> {
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
            let joined = match (current, branch_entry) {
                (Some(EnvEntry::Mono(a)), Some(EnvEntry::Mono(b))) if a != b => {
                    Some(EnvEntry::Mono(self.join_written_values(a, b)))
                }
                (None, Some(entry)) => Some(entry),
                // Keeping identical entries as-is preserves a function slot's polymorphism.
                (Some(a), Some(b)) if a == b => Some(a),
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
            }
        }
        changed
    }

    /// Quantifies unbound variables above the current level.
    fn generalize(&mut self, ty: Ty<'db>) -> TypeScheme<'db> {
        let resolved = self.table.resolve(self.db, ty);
        let mut binders = Vec::new();
        let mut mapping: FxHashMap<InferenceVar, Name<'db>> = FxHashMap::default();
        let body = self.abstract_vars(resolved, &mut binders, &mut mapping);
        TypeScheme { binders, body }
    }

    fn abstract_vars(
        &mut self,
        ty: Ty<'db>,
        binders: &mut Vec<(Name<'db>, Constraint)>,
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
        let Entry::Unbound { level, constraint } = *self.table.entry(representative) else {
            return ty;
        };
        if level <= self.table.level {
            // Escaped below the binding level: stays monomorphic.
            return ty;
        }
        let letter = (b'T' + (binders.len() as u8 % 7)) as char;
        let suffix = binders.len() / 7;
        let text = if suffix == 0 {
            letter.to_string()
        } else {
            format!("{letter}{suffix}")
        };
        let name = Name::new(self.db, text);
        mapping.insert(representative, name);
        binders.push((name, constraint));
        Ty::new(self.db, TyKind::Rigid(name))
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
        let mut substitution: FxHashMap<Name<'db>, Ty<'db>> = FxHashMap::default();
        for (name, constraint) in &scheme.binders {
            let fresh = self.fresh(*constraint);
            substitution.insert(*name, fresh);
        }
        substitute_rigid(self.db, scheme.body, &substitution)
    }
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

fn combine_operand_atomic<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Atomic> {
    match ty.kind(db) {
        TyKind::Scalar(atomic) => Some(*atomic),
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

fn frame_columns<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Vec<RecordField<'db>>> {
    match ty.kind(db) {
        TyKind::Frame(_, columns) => columns.clone(),
        _ => None,
    }
}

/// `None` unless every argument is an unnamed string literal.
/// `lapply`'s `X` and `FUN` arguments: by name, or else the first and second positional ones.
/// The arguments of a call to an R function with these `formals`, bound as R binds them:
/// exact names first, then the unnamed arguments in order.
fn bind_formals(arguments: &[Argument], formals: &[&str]) -> BoundFormals {
    let mut values: Vec<Option<ExprId>> = formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .find(|argument| argument.name.as_deref() == Some(*formal))
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
    schemes: Vec<TypeScheme<'db>>,
}

/// The types an apply-family function hands its callback one at a time, and whether the
/// result keeps `x`'s names.
fn element_types<'db>(db: &'db dyn Db, x: Ty<'db>) -> (Vec<Ty<'db>>, bool) {
    match x.kind(db) {
        TyKind::Vector(element) | TyKind::List(element) => (vec![*element], false),
        TyKind::NamedVector(element) | TyKind::NamedList(element) => (vec![*element], true),
        TyKind::Tuple(items) => (items.clone(), false),
        TyKind::Record(fields) | TyKind::Frame(_, Some(fields)) => {
            (fields.iter().map(|field| field.ty).collect(), true)
        }
        TyKind::Scalar(_) => (vec![x], false),
        _ => (vec![unknown(db)], false),
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
fn match_formals(arguments: &[Argument], formals: &[&str]) -> Vec<Option<ExprId>> {
    let mut matched: Vec<Option<ExprId>> = formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .find(|argument| argument.name.as_deref() == Some(*formal))
                .and_then(|argument| argument.value)
        })
        .collect();
    let named: Vec<bool> = formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .any(|argument| argument.name.as_deref() == Some(*formal))
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
    Ty::new(db, TyKind::Frame(kind, Some(fields)))
}

/// The element type of a vector type.
fn element_atomic(db: &dyn Db, ty: Ty<'_>) -> Option<Atomic> {
    match ty.kind(db) {
        TyKind::Vector(element) | TyKind::NamedVector(element) => element_atomic(db, *element),
        TyKind::Scalar(atomic) => Some(*atomic),
        _ => None,
    }
}

fn is_expr_type(db: &dyn Db, ty: Ty<'_>) -> bool {
    matches!(ty.kind(db), TyKind::Named(name, _) if name.text(db) == "Expr")
}

fn factor_type(db: &dyn Db) -> Ty<'_> {
    Ty::new(db, TyKind::Named(Name::new(db, "factor"), Vec::new()))
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

fn collect_callees(module: &Module, id: ExprId, callees: &mut FxHashSet<ExprId>) {
    let kind = module.kind(id);
    if let ExpressionKind::Call { callee, .. } = kind
        && matches!(module.kind(*callee), ExpressionKind::NameRef(_))
    {
        callees.insert(*callee);
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
                let x_range = argument.name_range.unwrap_or(value_range);
                match (callee_name.as_str(), module.kind(value)) {
                    ("c", ExpressionKind::Literal(LiteralKind::String(text))) => {
                        let text = &module[*text];
                        match &argument.name {
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
                        x: argument
                            .name
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
        .find(|argument| argument.name.as_deref() == Some("j"))
        .or_else(|| {
            let _i = positional.next();
            positional.next()
        });
    let Some(value) = j.and_then(|argument| argument.value) else {
        return true;
    };
    if arguments
        .iter()
        .any(|argument| matches!(argument.name.as_deref(), Some("by") | Some("keyby")))
    {
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

/// A DFS gray set: a name is expanding only while its own body is walked, so a
/// diamond is not a cycle. Nominal definitions may legitimately recurse.
fn alias_cycle_walk<'db>(
    db: &'db dyn Db,
    definitions: &FxHashMap<Name<'db>, NamedDefinition<'db>>,
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
            let definition = definitions.get(name)?;
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
    internal: bool,
    package: &Option<Symbol>,
    name: &Option<Symbol>,
) -> Option<String> {
    let name = name.as_ref()?.to_string();
    let Some(package) = package else {
        return Some(name);
    };
    // The project's own package resolves through globals and wins over a same-named stub namespace.
    if is_own_package(db, package.as_str()) {
        return Some(name);
    }
    match namespace_known(db, package.as_str()) {
        None => Some(name),
        Some(false) => None,
        Some(true) => {
            if internal || namespace_exports(db, package.as_str(), &name) {
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            RootDatabase, annotations::lower_annotation, hir::lower_item, naming::resolve_item,
        },
        syntax::{SyntaxKind, ast::is_expression_kind},
    };

    fn check_source<'db>(db: &'db RootDatabase, source: &str) -> ItemCheck<'db> {
        let parse = syntax::parse(source);
        let root = parse.syntax_node();
        let annotation = root
            .children()
            .find(|child| child.kind() == SyntaxKind::ANNOTATION)
            .map(|node| lower_annotation(db, &node));
        let item = root
            .children()
            .find(|child| is_expression_kind(child.kind()))
            .expect("one top-level item");
        let module = lower_item(&item);
        let naming = resolve_item(&module, &FxHashMap::default());
        check_item_with_annotation(db, &module, &naming, annotation.as_ref(), &[], None)
    }

    #[test]
    fn arithmetic_generalizes_with_numeric_constraint() {
        let db = RootDatabase::default();
        let check = check_source(&db, "add <- function(x) x + 1L");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.expect("definition scheme");
        assert_eq!(scheme.binders.len(), 1);
        assert_eq!(scheme.binders[0].1, Constraint::Numeric);
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!("expected a function scheme");
        };
        assert_eq!(function.named.len(), 1);
        assert_eq!(function.named[0].name.text(&db), "x");
        assert_eq!(function.named[0].ty, function.ret);
    }

    #[test]
    fn call_mismatch_reports() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "g <- function() {\n  f <- function(x) x + 1\n  f(\"txt\")\n}",
        );
        assert!(
            check.errors.iter().any(|error| matches!(
                error.kind,
                TypeErrorKind::ConstraintViolation {
                    constraint: Constraint::Numeric,
                    ..
                }
            )),
            "expected a numeric-constraint violation, got {:?}",
            check.errors
        );
    }

    #[test]
    fn polymorphic_reuse_across_calls() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "use <- function() {\n  id <- function(x) x\n  id(1L)\n  id(\"s\")\n}",
        );
        assert!(
            check.errors.is_empty(),
            "let-polymorphism failed: {:?}",
            check.errors
        );
    }

    #[test]
    fn if_join_unifies_or_unions() {
        let db = RootDatabase::default();
        // A `NULL` branch joins by union, never binding the variable.
        let check = check_source(&db, "pick <- function(f) if (f) 1L else NULL");
        assert!(check.errors.is_empty());
        let scheme = check.scheme.expect("scheme");
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!()
        };
        let TyKind::Union(members) = function.ret.kind(&db) else {
            panic!("expected integer | NULL, got {:?}", function.ret.kind(&db));
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn unknown_named_argument_reports() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "g <- function() {\n  f <- function(x) x\n  f(z = 1)\n}",
        );
        assert!(
            check.errors.iter().any(|error| matches!(
                &error.kind,
                TypeErrorKind::NamedArgumentMismatch { argument, .. } if argument == "z"
            )),
            "expected a named-argument mismatch, got {:?}",
            check.errors
        );
    }

    #[test]
    fn for_loops_bind_the_element_and_reach_a_fixed_point() {
        let db = RootDatabase::default();
        let accumulator = check_source(
            &db,
            "f <- function() {\n  total <- 0L\n  for (i in 1:3) {\n    total <- total + i\n  }\n  total\n}",
        );
        assert!(accumulator.errors.is_empty(), "{:?}", accumulator.errors);
        assert_eq!(scheme_ret(&db, &accumulator), scalar(&db, Atomic::Integer));
        let heterogeneous = check_source(
            &db,
            "f <- function() {\n  out <- 1L\n  for (item in list(2L, \"a\")) {\n    out <- item\n  }\n  out\n}",
        );
        assert!(
            heterogeneous.errors.is_empty(),
            "{:?}",
            heterogeneous.errors
        );
        assert!(matches!(
            scheme_ret(&db, &heterogeneous).kind(&db),
            TyKind::Union(members) if members.len() == 2
        ));
        let bad = check_source(
            &db,
            "f <- function() {\n  g <- function() 1L\n  for (i in g) i\n}",
        );
        assert!(
            bad.errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::NotIterable { .. })),
            "{:?}",
            bad.errors
        );
    }

    #[test]
    fn while_condition_checks_inside_the_fixed_point() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "f <- function() {\n  done <- FALSE\n  while (!done) {\n    done <- TRUE\n  }\n  done\n}",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
    }

    #[test]
    fn repeat_applies_the_exit_state() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "f <- function() {\n  repeat {\n    x <- 1L\n    break\n  }\n  x + 1L\n}",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
    }

    #[test]
    fn replacement_writes_update_the_base_slot() {
        let db = RootDatabase::default();
        let replaced = check_source(
            &db,
            "f <- function() {\n  p <- list(a = 1L)\n  p$a <- \"s\"\n  p$a\n}",
        );
        assert!(replaced.errors.is_empty(), "{:?}", replaced.errors);
        assert_eq!(scheme_ret(&db, &replaced), scalar(&db, Atomic::Character));
        let added = check_source(
            &db,
            "f <- function() {\n  p <- list()\n  p$b <- TRUE\n  p$b\n}",
        );
        assert!(added.errors.is_empty(), "{:?}", added.errors);
        assert_eq!(scheme_ret(&db, &added), scalar(&db, Atomic::Logical));
        let computed = check_source(
            &db,
            "f <- function(key) {\n  m <- list()\n  m[[key]] <- 1L\n  m[[key]]\n}",
        );
        assert!(computed.errors.is_empty(), "{:?}", computed.errors);
        assert_eq!(scheme_ret(&db, &computed), scalar(&db, Atomic::Integer));
        let kept = check_source(
            &db,
            "f <- function() {\n  v <- c(1L, 2L)\n  names(v) <- c(\"a\", \"b\")\n  v + 1L\n}",
        );
        assert!(kept.errors.is_empty(), "{:?}", kept.errors);
    }

    #[test]
    fn early_returns_union_with_the_trailing_value() {
        let db = RootDatabase::default();
        let check = check_source(&db, "f <- function(c) {\n  if (c) return(\"foo\")\n  5\n}");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let TyKind::Union(members) = scheme_ret(&db, &check).kind(&db) else {
            panic!(
                "expected character | double, got {:?}",
                scheme_ret(&db, &check).kind(&db)
            );
        };
        assert!(members.contains(&scalar(&db, Atomic::Character)));
        assert!(members.contains(&scalar(&db, Atomic::Double)));
    }

    #[test]
    fn diverging_branch_contributes_neither_value_nor_state() {
        let db = RootDatabase::default();
        let value = check_source(
            &db,
            "f <- function(c) {\n  x <- if (c) return(NULL) else 5\n  x + 1\n}",
        );
        assert!(value.errors.is_empty(), "{:?}", value.errors);
        let state = check_source(
            &db,
            "f <- function(flag) {\n  x <- 1L\n  if (flag) {\n    x <- \"s\"\n    return(x)\n  }\n  x + 1L\n}",
        );
        assert!(state.errors.is_empty(), "{:?}", state.errors);
    }

    #[test]
    fn null_guard_narrows_the_early_exit_idiom() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "#: fn(x: integer | NULL) -> integer\nf <- function(x) {\n  if (is.null(x)) {\n    return(0L)\n  }\n  x + 1L\n}",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
    }

    #[test]
    fn null_guard_shapes_the_unannotated_coalesce_idiom() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "coalesce <- function(value, fallback) if (is.null(value)) fallback else value",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.clone().expect("scheme");
        assert_eq!(scheme.binders.len(), 1, "{scheme:?}");
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!()
        };
        assert!(matches!(function.ret.kind(&db), TyKind::Rigid(_)));
        assert_eq!(function.named[1].ty, function.ret, "fallback ties to T");
        let TyKind::Union(members) = function.named[0].ty.kind(&db) else {
            panic!("value must be T | NULL, got {:?}", function.named[0].ty);
        };
        assert!(members.contains(&function.ret));
        assert!(members.contains(&null(&db)));
    }

    #[test]
    fn family_guard_filters_union_members() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "f <- function(flag) {\n  y <- if (flag) 1L else \"a\"\n  if (is.character(y)) y else \"z\"\n}",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert_eq!(
            scheme_ret(&db, &check),
            scalar(&db, Atomic::Character),
            "the true edge must narrow y to character"
        );
    }

    #[test]
    fn arithmetic_ties_two_flexible_operands() {
        let db = RootDatabase::default();
        let check = check_source(&db, "add <- function(a, b) a + b");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.expect("scheme");
        assert_eq!(scheme.binders.len(), 1, "{scheme:?}");
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!()
        };
        assert_eq!(function.named[0].ty, function.ret);
        assert_eq!(function.named[1].ty, function.ret);
    }

    #[test]
    fn division_is_always_double_and_keeps_the_operand_generic() {
        let db = RootDatabase::default();
        let check = check_source(&db, "half <- function(x) x / 2");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.clone().expect("scheme");
        assert_eq!(scheme.binders.len(), 1, "x stays generic: {scheme:?}");
        assert_eq!(scheme_ret(&db, &check), scalar(&db, Atomic::Double));
    }

    #[test]
    fn arithmetic_shape_rules_over_vectors() {
        let db = RootDatabase::default();
        let vector = check_source(&db, "f <- function() c(1L, 2L) + 1L");
        assert!(vector.errors.is_empty(), "{:?}", vector.errors);
        assert!(matches!(
            scheme_ret(&db, &vector).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Integer)
        ));
        let divided = check_source(&db, "f <- function() c(1L, 2L) / 2L");
        assert!(divided.errors.is_empty(), "{:?}", divided.errors);
        assert!(matches!(
            scheme_ret(&db, &divided).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Double)
        ));
        let modulo = check_source(&db, "f <- function() 7L %% 2L");
        assert!(modulo.errors.is_empty(), "{:?}", modulo.errors);
        assert_eq!(scheme_ret(&db, &modulo), scalar(&db, Atomic::Integer));
        let bad = check_source(&db, "f <- function() \"a\" + 1L");
        assert!(
            bad.errors.iter().any(|error| matches!(
                error.kind,
                TypeErrorKind::InvalidOperand {
                    expected: OperandExpectation::Numeric,
                    ..
                }
            )),
            "{:?}",
            bad.errors
        );
    }

    #[test]
    fn colon_yields_integer_sequences_with_double_fallback() {
        let db = RootDatabase::default();
        let literal = check_source(&db, "f <- function() 1:10");
        assert!(literal.errors.is_empty(), "{:?}", literal.errors);
        assert!(matches!(
            scheme_ret(&db, &literal).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Integer)
        ));
        let flexible = check_source(&db, "f <- function(n) 1:n");
        assert!(flexible.errors.is_empty(), "{:?}", flexible.errors);
        assert!(matches!(
            scheme_ret(&db, &flexible).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Double)
        ));
    }

    #[test]
    fn comparisons_coerce_across_atomics_and_shape_elementwise() {
        let db = RootDatabase::default();
        let flexible = check_source(&db, "positive <- function(x) x > 0L");
        assert!(flexible.errors.is_empty(), "{:?}", flexible.errors);
        let scheme = flexible.scheme.clone().expect("scheme");
        assert_eq!(scheme.binders.len(), 1);
        assert_eq!(scheme.binders[0].1, Constraint::Unconstrained);
        assert_eq!(scheme_ret(&db, &flexible), scalar(&db, Atomic::Logical));
        let vector = check_source(&db, "f <- function() c(1L, 2L) > 1L");
        assert!(vector.errors.is_empty(), "{:?}", vector.errors);
        assert!(matches!(
            scheme_ret(&db, &vector).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Logical)
        ));
        let mixed = check_source(&db, "f <- function() 1L > \"a\"");
        assert!(mixed.errors.is_empty(), "{:?}", mixed.errors);
    }

    #[test]
    fn conditions_coerce_numerics_and_refuse_everything_else() {
        let db = RootDatabase::default();
        for source in [
            "f <- function() if (1L) 2L",
            "f <- function() 1L && TRUE",
            "f <- function(x) if (length(x)) 1L else 2L",
            "f <- function() if (2.5) 1L else 2L",
        ] {
            let ok = check_source(&db, source);
            assert!(ok.errors.is_empty(), "{source}: {:?}", ok.errors);
        }
        let bad_condition = check_source(&db, "f <- function() if (\"yes\") 2L");
        assert!(
            bad_condition
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::Mismatch { .. })),
            "{:?}",
            bad_condition.errors
        );
        let ok = check_source(&db, "f <- function(a, b) a && b");
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
    }

    #[test]
    fn unary_operators_preserve_shape() {
        let db = RootDatabase::default();
        let negated = check_source(&db, "f <- function() -c(1L, 2L)");
        assert!(negated.errors.is_empty(), "{:?}", negated.errors);
        assert!(matches!(
            scheme_ret(&db, &negated).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Integer)
        ));
        let notted = check_source(&db, "f <- function() !c(TRUE, FALSE)");
        assert!(notted.errors.is_empty(), "{:?}", notted.errors);
        assert!(matches!(
            scheme_ret(&db, &notted).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Logical)
        ));
        let bad = check_source(&db, "f <- function() -\"a\"");
        assert!(
            bad.errors.iter().any(|error| matches!(
                error.kind,
                TypeErrorKind::InvalidOperand {
                    expected: OperandExpectation::Numeric,
                    ..
                }
            )),
            "{:?}",
            bad.errors
        );
    }

    #[test]
    fn arity_mismatch_reports_both_directions() {
        let db = RootDatabase::default();
        let surplus = check_source(&db, "g <- function() {\n  f <- function(x) x\n  f(1, 2)\n}");
        assert!(
            surplus
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::ArityMismatch { .. })),
            "expected a surplus-argument arity error, got {:?}",
            surplus.errors
        );
        let missing = check_source(&db, "g <- function() {\n  f <- function(x) x\n  f()\n}");
        assert!(
            missing
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::MissingArguments { .. })),
            "expected a missing-argument error, got {:?}",
            missing.errors
        );
        let defaulted = check_source(&db, "g <- function() {\n  f <- function(x = 1) x\n  f()\n}");
        assert!(defaulted.errors.is_empty(), "{:?}", defaulted.errors);
    }

    fn scheme_ret<'db>(db: &'db RootDatabase, check: &ItemCheck<'db>) -> Ty<'db> {
        let scheme = check.scheme.clone().expect("scheme");
        let TyKind::Function(function) = scheme.body.kind(db).clone() else {
            panic!("expected a function scheme");
        };
        function.ret
    }

    #[test]
    fn list_builds_fixed_shapes_and_extraction_is_precise() {
        let db = RootDatabase::default();
        let tuple = check_source(
            &db,
            "f <- function() {\n  x <- list(1L, \"a\")\n  x[[1L]]\n}",
        );
        assert!(tuple.errors.is_empty(), "{:?}", tuple.errors);
        assert_eq!(scheme_ret(&db, &tuple), scalar(&db, Atomic::Integer));
        let record = check_source(
            &db,
            "f <- function() {\n  p <- list(a = 1L, b = \"s\")\n  p$b\n}",
        );
        assert!(record.errors.is_empty(), "{:?}", record.errors);
        assert_eq!(scheme_ret(&db, &record), scalar(&db, Atomic::Character));
        let computed = check_source(
            &db,
            "f <- function(i) {\n  x <- list(1L, \"a\")\n  x[[i]]\n}",
        );
        assert!(computed.errors.is_empty(), "{:?}", computed.errors);
        assert!(matches!(
            scheme_ret(&db, &computed).kind(&db),
            TyKind::Union(members) if members.len() == 2
        ));
        let slice = check_source(&db, "f <- function() {\n  x <- list(1L, \"a\")\n  x[1L]\n}");
        assert!(slice.errors.is_empty(), "{:?}", slice.errors);
        assert!(matches!(
            scheme_ret(&db, &slice).kind(&db),
            TyKind::List(element) if matches!(element.kind(&db), TyKind::Union(_))
        ));
    }

    #[test]
    fn index_errors_report_precisely() {
        let db = RootDatabase::default();
        let out_of_range = check_source(
            &db,
            "f <- function() {\n  x <- list(1L, \"a\")\n  x[[3L]]\n}",
        );
        assert!(
            out_of_range.errors.iter().any(|error| matches!(
                error.kind,
                TypeErrorKind::PositionDoesNotExist { position: 3, .. }
            )),
            "{:?}",
            out_of_range.errors
        );
        let missing_field = check_source(
            &db,
            "f <- function() {\n  p <- list(value = 1L)\n  p$valeu\n}",
        );
        assert!(
            missing_field.errors.iter().any(|error| matches!(
                &error.kind,
                TypeErrorKind::FieldDoesNotExist { field, .. } if field == "valeu"
            )),
            "{:?}",
            missing_field.errors
        );
        let dollar_atomic = check_source(&db, "f <- function() {\n  v <- c(a = 1L)\n  v$a\n}");
        assert!(
            dollar_atomic
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::DollarOnAtomicVector { .. })),
            "{:?}",
            dollar_atomic.errors
        );
    }

    #[test]
    fn named_vector_extraction_is_nullable_by_name() {
        let db = RootDatabase::default();
        let check = check_source(&db, "f <- function() {\n  v <- c(a = 1L)\n  v[[\"a\"]]\n}");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let TyKind::Union(members) = scheme_ret(&db, &check).kind(&db) else {
            panic!("expected integer | NULL");
        };
        assert!(members.contains(&scalar(&db, Atomic::Integer)));
        assert!(members.contains(&null(&db)));
    }

    #[test]
    fn combine_promotes_drops_null_and_keeps_names() {
        let db = RootDatabase::default();
        let promoted = check_source(&db, "f <- function() c(1L, 2.5)");
        assert!(promoted.errors.is_empty());
        assert!(matches!(
            scheme_ret(&db, &promoted).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Double)
        ));
        let character = check_source(&db, "f <- function() c(1L, \"a\")");
        assert!(character.errors.is_empty());
        assert!(matches!(
            scheme_ret(&db, &character).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Character)
        ));
        let named = check_source(&db, "f <- function() c(a = 1L, b = 2L)");
        assert!(named.errors.is_empty());
        assert!(matches!(
            scheme_ret(&db, &named).kind(&db),
            TyKind::NamedVector(_)
        ));
        let null_dropped = check_source(&db, "f <- function() c(1L, NULL)");
        assert!(null_dropped.errors.is_empty());
        assert!(matches!(
            scheme_ret(&db, &null_dropped).kind(&db),
            TyKind::Vector(_)
        ));
        let empty = check_source(&db, "f <- function() c()");
        assert!(matches!(scheme_ret(&db, &empty).kind(&db), TyKind::Null));
    }

    #[test]
    fn switch_unions_branches_with_null_unless_defaulted() {
        let db = RootDatabase::default();
        let no_default = check_source(&db, "f <- function(x) switch(x, a = 1L, b = \"s\")");
        assert!(no_default.errors.is_empty(), "{:?}", no_default.errors);
        let TyKind::Union(members) = scheme_ret(&db, &no_default).kind(&db) else {
            panic!("expected a union");
        };
        assert_eq!(members.len(), 3, "integer | character | NULL: {members:?}");
        let with_default = check_source(&db, "f <- function(x) switch(x, a = 1L, 2L)");
        assert!(with_default.errors.is_empty());
        assert_eq!(
            scheme_ret(&db, &with_default),
            scalar(&db, Atomic::Integer),
            "both branches integer, no NULL member"
        );
    }

    #[test]
    fn union_of_functions_calls_every_member() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "f <- function(flag) {\n  h <- if (flag) function(x) 1L else function(x) \"a\"\n  h(2L)\n}",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.expect("scheme");
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!()
        };
        let TyKind::Union(members) = function.ret.kind(&db) else {
            panic!(
                "expected integer | character, got {:?}",
                function.ret.kind(&db)
            );
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn conditional_slot_write_joins_types() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "f <- function(flag) {\n  x <- 1L\n  if (flag) x <- \"two\"\n  x\n}",
        );
        assert!(check.errors.is_empty());
        let scheme = check.scheme.expect("scheme");
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!()
        };
        let TyKind::Union(members) = function.ret.kind(&db) else {
            panic!(
                "expected integer | character return, got {:?}",
                function.ret.kind(&db)
            );
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn annotated_identity_keeps_declared_scheme() {
        let db = RootDatabase::default();
        let check = check_source(&db, "#: <T> fn(x: T) -> T\nid <- function(x) x");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.expect("declared scheme");
        assert_eq!(scheme.binders.len(), 1);
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!()
        };
        assert!(matches!(function.ret.kind(&db), TyKind::Rigid(_)));
    }

    #[test]
    fn annotated_return_mismatch_reports() {
        let db = RootDatabase::default();
        let check = check_source(&db, "#: fn(x: integer) -> character\nf <- function(x) x");
        assert!(
            check
                .errors
                .iter()
                .any(|e| matches!(e.kind, TypeErrorKind::Mismatch { .. })),
            "expected return mismatch, got {:?}",
            check.errors
        );
    }

    #[test]
    fn annotated_parameter_name_mismatch_reports() {
        let db = RootDatabase::default();
        let check = check_source(&db, "#: fn(x: integer) -> integer\nf <- function(y) 1L");
        assert!(
            check
                .errors
                .iter()
                .any(|e| matches!(&e.kind, TypeErrorKind::AnnotationParameterMismatch { .. })),
            "expected parameter mismatch, got {:?}",
            check.errors
        );
    }

    #[test]
    fn rigid_binder_refuses_undeclared_bound() {
        let db = RootDatabase::default();
        let bad = check_source(&db, "#: <T> fn(x: T) -> T\nbad <- function(x) x + 1L");
        assert!(
            !bad.errors.is_empty(),
            "plain <T> must refuse arithmetic, got no errors"
        );
        let ok = check_source(
            &db,
            "#: <T: numeric> fn(x: T) -> T\nok <- function(x) x + 1L",
        );
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
    }

    #[test]
    fn expanded_param_return_form_checks() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "#: @forall T: numeric\n#: @param x {T}\n#: @return {T}\nround2 <- function(x) x + 1L",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.expect("declared scheme");
        assert_eq!(scheme.binders.len(), 1);
        assert_eq!(scheme.binders[0].1, Constraint::Numeric);
    }

    #[test]
    fn trusted_annotation_skips_enforcement() {
        let db = RootDatabase::default();
        let check = check_source(
            &db,
            "#: @trust fn(x: integer) -> character\nf <- function(x) x",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(check.scheme.is_some());
    }
}

/// The inference variables `ty` mentions, as the table resolved them when `ty` was read.
fn collect_vars<'db>(db: &'db dyn Db, ty: Ty<'db>, into: &mut FxHashSet<InferenceVar>) {
    if let TyKind::Var(var) = ty.kind(db) {
        into.insert(*var);
    }
    for_each_child_type(db, ty, &mut |child| collect_vars(db, child, into));
}
