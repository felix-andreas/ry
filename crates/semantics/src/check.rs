//! Expression inference walk: Hindley–Milner over one item's HIR. Branches roll
//! back through an undo log instead of cloning the environment.

use crate::annotations::{Annotation, NamedDefinition};
use crate::diagnostics::nearest_name;
use crate::exhaustiveness::{MatchFinding, match_findings};
use crate::hir::{
    Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind, MatchArm,
    Module, NaAtom, Parameter as Formal, PatId, PatternKind, Symbol, UnaryOperator,
    destructure_leaves, integer_literal_position, is_whole_number_double,
};
use crate::infer::{
    Entry, FunctionMismatch, InferenceTable, RecordMismatch, UnifyError, apply_definition,
};
use crate::metadata::is_own_package;
use crate::naming::{
    BindingId, BindingKind, ItemNaming, dot_dot_variable, is_column_update, query_index_arguments,
};
use crate::stubs::{STANDARD_LIBRARY, is_ry_type, namespace_exports, namespace_known, stubs};
use crate::types::{
    Atomic, Constraint, FunctionType, InferenceVar, Name, NativeField, Parameter, RecordField,
    RestParameter, TYPE_SIZE_CEILING, Ty, TyKind, TypeScheme, any, column_type, conflicting_tags,
    contains_inference_var, erase_vars, for_each_child_type, map_child_types, native_record, null,
    scalar, substitute_rigid, type_size, union_of, unknown,
};
use crate::{DataPath, Db, data_file_layout};
use rustc_hash::{FxHashMap, FxHashSet};
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use syntax::native_values::accepts_native;
use syntax::{TextRange, TextSize};

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

    /// Borrowed: per-file facts consulted once per item; an owned copy would make every
    /// item pay for the whole table.
    fn type_definitions(&self) -> &'db FxHashMap<Name<'db>, NamedDefinition<'db>>;

    fn arithmetic_classes(&self) -> &'db FxHashSet<String>;

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
        expression_annotations: expression_annotations.iter().cloned().collect(),
        environment: Environment::default(),
        scheme_arena: Vec::new(),
        recorded: FxHashMap::default(),
        selected_overloads: FxHashMap::default(),
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
        non_local_slots: FxHashMap::default(),
        string_values: FxHashMap::default(),
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
                range: module.expression(root).range,
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
        match (&module.expression(root).kind, declared_fn) {
            // A function literal with its own signature is typed by it, and the binding's
            // declared type is then checked against that like any other value.
            (ExpressionKind::Assign { value, .. }, Some((declared, function)))
                if matches!(
                    module.expression(*value).kind,
                    ExpressionKind::Function { .. }
                ) && !context.expression_annotations.contains_key(value) =>
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
                                    range: module.expression(*value).range,
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
                            if annotation.is_some_and(|a| !a.trusted)
                                && !matches!(declared.body.kind(db), TyKind::Unknown | TyKind::Any)
                            {
                                let expected = declared.body;
                                let range = module.expression(*value).range;
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
    let mut expression_types = vec![None; module.expressions.len()];
    for (&id, &ty) in &context.recorded {
        let slot = expression_types
            .get_mut(id.0 as usize)
            .unwrap_or_else(|| panic!("recorded type for {id:?} outside the item's module"));
        *slot = Some(context.table.resolve(context.db, ty));
    }
    let expression_types = ExpressionTypes(expression_types.into_boxed_slice());
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
    let match_findings = match_findings(db, module, &expression_types);
    ItemCheck {
        match_findings,
        expression_types,
        errors,
        strict_origins: context.strict_origins,
        scheme,
        selected_overloads: context.selected_overloads,
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
            Constraint::AtomicElement | Constraint::Key | Constraint::Unconstrained => unknown(db),
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
    expression_annotations: FxHashMap<ExprId, Annotation<'db>>,
    environment: Environment<'db>,
    scheme_arena: Vec<TypeScheme<'db>>,
    recorded: FxHashMap<ExprId, Ty<'db>>,
    selected_overloads: FxHashMap<ExprId, usize>,
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
    /// Slots for names this item reads but another item binds, so guard refinements live
    /// in the environment like any slot's. Ids are minted above naming's so they never collide.
    non_local_slots: FxHashMap<Symbol, BindingId>,
    next_non_local_slot: u32,
    /// The write of a binding whose only write is literal strings (`"a"` or `c("a", "b")`),
    /// compared by site so a re-pass over the same write keeps it. `None` once a second write
    /// site, or a write of anything else, appears. Queries read it for column names given through a variable
    /// (`.SDcols = cols`, `(cols) := ...`).
    string_values: FxHashMap<BindingId, Option<StringWrite>>,
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
}

enum CalleeLookup<'db> {
    Binding,
    Skipped(TypeScheme<'db>),
    Undetermined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperandShape {
    Scalar,
    Vector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VectorIndexShape {
    ScalarLike,
    VectorLike,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComparisonFamily {
    /// R promotes a logical operand to integer before comparing, so `flags > 0` and
    /// `flag == TRUE` compare freely.
    Numeric,
    Character,
}

enum NumericOperand<'db> {
    Concrete(OperandShape, Atomic),
    ConcreteUnion(Vec<(OperandShape, Atomic)>),
    Flexible(Ty<'db>),
    /// Vector shape known, element not: a generic variable/rigid to constrain, or `None`
    /// for `Any`/`Unknown`.
    FlexibleVector(Option<Ty<'db>>),
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

/// A recognized type-guard condition; a `None` edge carries no refinement.
struct GuardRefinement<'db> {
    /// `missing(name)` on a no-default formal: the true edge marks the slot
    /// read-erroring instead of refining it.
    missing_on_true: bool,
    slot: BindingId,
    true_edge: Option<Ty<'db>>,
    false_edge: Option<Ty<'db>>,
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

enum FlexibleComparisonOperand<'db> {
    Bare(Ty<'db>),
    VectorElement(Option<Ty<'db>>),
}

impl<'db> FlexibleComparisonOperand<'db> {
    fn constrainable(&self) -> Option<Ty<'db>> {
        match self {
            FlexibleComparisonOperand::Bare(ty) => Some(*ty),
            FlexibleComparisonOperand::VectorElement(ty) => *ty,
        }
    }
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

fn comparison_operand_parts_list<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
) -> Option<Vec<(OperandShape, ComparisonFamily)>> {
    match ty.kind(db) {
        TyKind::Union(members) => members
            .iter()
            .map(|&member| comparison_operand_parts(db, member))
            .collect(),
        _ => comparison_operand_parts(db, ty).map(|parts| vec![parts]),
    }
}

fn comparison_operand_parts<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
) -> Option<(OperandShape, ComparisonFamily)> {
    let (shape, atomic) = match ty.kind(db) {
        TyKind::Scalar(atomic) => (OperandShape::Scalar, *atomic),
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Scalar(atomic) => (OperandShape::Vector, *atomic),
            _ => return None,
        },
        _ => return None,
    };
    let family = match atomic {
        Atomic::Logical | Atomic::Integer | Atomic::Double => ComparisonFamily::Numeric,
        Atomic::Character => ComparisonFamily::Character,
        Atomic::Complex | Atomic::Raw => return None,
    };
    Some((shape, family))
}

fn flexible_comparison_operand<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
) -> Option<FlexibleComparisonOperand<'db>> {
    match ty.kind(db) {
        TyKind::Var(_) | TyKind::Rigid(_) => Some(FlexibleComparisonOperand::Bare(ty)),
        TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
            TyKind::Var(_) | TyKind::Rigid(_) => {
                Some(FlexibleComparisonOperand::VectorElement(Some(*element)))
            }
            TyKind::Any | TyKind::Unknown => Some(FlexibleComparisonOperand::VectorElement(None)),
            _ => None,
        },
        _ => None,
    }
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
        let range = self.module.expression(expression).range;
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
        let Some(mismatch) = self.table.explain_record_mismatch(self.db, found, expected) else {
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
        let ExpressionKind::Call { arguments, .. } = &self.module.expression(call).kind else {
            return None;
        };
        arguments
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
        let expression = module.expression(id);
        let range = expression.range;
        let ty = match &expression.kind {
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
                    matches!(
                        self.module.expression(value).kind,
                        ExpressionKind::Function { .. }
                    ) && !self.expression_annotations.contains_key(&value)
                });
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
                            && matches!(
                                self.module.expression(target).kind,
                                ExpressionKind::NameRef(_)
                            );
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
                let ty = self.infer_call_expression(id, range, callee, &arguments);
                self.check_native_arguments(range, callee, &arguments, first_call_error);
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
                self.infer_index(id, range, double, target, &arguments)
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
            // Validated against the stub namespaces (`:::` skips the export check), but the
            // scheme comes from globals so a project override of a stub name keeps working.
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => {
                let validated = validated_namespace_name(self.db, *internal, package, name);
                let name = *name;
                match validated
                    .as_ref()
                    .and_then(|name| self.globals.and_then(|globals| globals.scheme(name, false)))
                {
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
                self.table.level += 1;
                let enclosing_frame_level = mem::replace(&mut self.frame_level, self.table.level);
                let pending_mark = self.pending_enclosing_writes.len();
                let mark = self.environment.mark();
                let omissible = self.omissible_formals(&parameters, *body);
                let mut named = Vec::new();
                let mut variadic = None;
                for parameter in &parameters {
                    if parameter.name == "..." {
                        variadic = Some(RestParameter {
                            element: any(self.db),
                            preceding_named: named.len(),
                        });
                        continue;
                    }
                    let parameter_ty = self.fresh(Constraint::Unconstrained);
                    let entry = named.len();
                    named.push(Parameter {
                        name: Name::new(self.db, parameter.name.as_str()),
                        ty: parameter_ty,
                        optional: parameter.default.is_some()
                            || omissible.contains(&parameter.name),
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
                        if no_default {
                            self.no_default_formals.insert(slot);
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
                let signature: Vec<(&Formal, Option<Ty<'db>>)> = parameters
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
                self.infer_if(condition, then_branch, else_branch)
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
                for statement in statements {
                    last = self.infer(statement);
                }
                last
            }
            ExpressionKind::Paren(inner) => self.infer(*inner),
            ExpressionKind::Break | ExpressionKind::Next => unknown(self.db),
            ExpressionKind::Tuple(items) => {
                let items = items.clone();
                let items = items.iter().map(|&item| self.infer(item)).collect();
                Ty::new(self.db, TyKind::NativeTuple(items))
            }
            ExpressionKind::Record(fields) => {
                let fields = fields.clone();
                let fields = fields
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name.as_str().to_owned()),
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
                    TyKind::Tag(Name::new(self.db, name.as_str().to_owned()), payload),
                )
            }
            ExpressionKind::Match { scrutinee, arms } => {
                let (scrutinee, arms) = (*scrutinee, arms.clone());
                self.infer_match(expression.range, scrutinee, &arms)
            }
        };
        // Assignments applied their annotation before the slot write above.
        let ty = if matches!(expression.kind, ExpressionKind::Assign { .. }) {
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
        let (function, may_be_library) = match &self.module.expression(callee).kind {
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
            let resolved = self.table.resolve(self.db, ty);
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
            let is_argument = self.module.expressions.iter().any(|expression| {
                matches!(
                    &expression.kind,
                    ExpressionKind::Call { arguments, .. } | ExpressionKind::Index { arguments, .. }
                        if arguments.iter().any(|argument| argument.value == Some(function))
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
                && let Some(scheme) = self.globals.and_then(|globals| {
                    globals.scheme(name, naming.deferred_non_locals.contains(&id))
                })
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
            return unknown(self.db);
        };
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
                let range = self.module.expression(id).range;
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
        let ExpressionKind::NameRef(name) = &module.expression(id).kind else {
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
        if let ExpressionKind::NameRef(_) = self.module.expression(target).kind {
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
                match &self.module.expression(root).kind {
                    ExpressionKind::Index { target, .. } | ExpressionKind::Field { target, .. } => {
                        root = *target;
                    }
                    ExpressionKind::Call { arguments, .. } => {
                        match arguments.first().and_then(|argument| argument.value) {
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
        match &module.expression(id).kind {
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
        match &self.module.expression(id).kind {
            ExpressionKind::NameRef(_) => Some(id),
            ExpressionKind::Index { target, .. } | ExpressionKind::Field { target, .. } => {
                self.replacement_base(*target)
            }
            ExpressionKind::Call { arguments, .. } => arguments
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
        match &module.expression(id).kind {
            ExpressionKind::Index {
                target, arguments, ..
            } => {
                self.infer_replacement_spine(*target, base);
                for argument in arguments {
                    if let Some(value) = argument.value {
                        self.infer(value);
                    }
                }
            }
            ExpressionKind::Field { target, .. } => {
                self.infer_replacement_spine(*target, base);
            }
            ExpressionKind::Call { arguments, .. } => {
                let mut argument_iter = arguments.iter();
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

    fn replacement_written_type(
        &mut self,
        lhs: ExprId,
        base: ExprId,
        value_expression: ExprId,
        prior: Ty<'db>,
        value: Ty<'db>,
    ) -> Ty<'db> {
        let module = self.module;
        let kind = &module.expression(lhs).kind;
        let field_name = match kind {
            ExpressionKind::Field {
                at: false,
                target,
                name,
                ..
            } if *target == base => name.map(|name| name.to_string()),
            ExpressionKind::Index {
                double: true,
                target,
                arguments,
            } if *target == base => match &arguments[..] {
                [argument] if argument.name.is_none() => {
                    argument
                        .value
                        .and_then(|value| match &self.module.expression(value).kind {
                            ExpressionKind::Literal(LiteralKind::String(name)) => {
                                Some(name.to_string())
                            }
                            _ => None,
                        })
                }
                _ => None,
            },
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
                TyKind::Frame(Some(mut columns)) => {
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
                    Ty::new(self.db, TyKind::Frame(Some(columns)))
                }
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
                                _ => module.expression(lhs).range,
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
                    self.check_nominal_field_write(prior, &field_name, value_expression, value);
                    prior
                }
                _ => prior,
            };
        }
        let is_computed_extract = matches!(
            &kind,
            ExpressionKind::Index { double: true, target, arguments }
                if *target == base
                    && matches!(&arguments[..], [argument] if argument.name.is_none())
        );
        if !is_computed_extract {
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
    ) -> Ty<'db> {
        self.expect_scalar_logical(condition);
        let guard = self.recognize_guard(condition);

        let mark = self.environment.mark();
        if let Some(guard) = &guard {
            if let Some(true_edge) = guard.true_edge {
                self.environment.set(guard.slot, EnvEntry::Mono(true_edge));
            } else if guard.missing_on_true
                && let Some(EnvEntry::Mono(current)) = self.environment.get(guard.slot)
            {
                self.environment
                    .set(guard.slot, EnvEntry::MissingFormal(current));
            }
        }
        let then_ty = self.infer(then_branch);
        let then_diverges = self.diverges(then_branch);
        let then_writes = self.environment.writes_since(mark);
        self.environment.rollback(mark);

        // The false edge applies to the else branch and, when the then branch diverges,
        // to everything after the `if`.
        let false_mark = self.environment.mark();
        if let Some(guard) = &guard
            && let Some(false_edge) = guard.false_edge
        {
            self.environment.set(guard.slot, EnvEntry::Mono(false_edge));
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
            (false, false) => {
                self.join_writes(&then_writes);
                let joined = self.join_branch_values(then_ty, else_ty);
                let mut range = self.module.expression(then_branch).range;
                if let Some(else_branch) = else_branch {
                    range = range.cover(self.module.expression(else_branch).range);
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
        let subject = self.structural(scrutinee_ty);
        let mut values = Vec::with_capacity(arms.len());
        let mut all_values = Vec::with_capacity(arms.len());
        let mut arm_writes = Vec::with_capacity(arms.len());
        for arm in arms {
            let mark = self.environment.mark();
            let mut bindings = Vec::new();
            self.check_pattern(arm.pattern, subject, &mut bindings);
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
                        TyKind::Tag(Name::new(self.db, name.as_str().to_owned()), payload),
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
                let items = items
                    .iter()
                    .map(|_| self.fresh(Constraint::Unconstrained))
                    .collect();
                Some(Ty::new(self.db, TyKind::NativeTuple(items)))
            }
            PatternKind::Record {
                fields,
                rest: false,
            } => {
                let fields = fields
                    .iter()
                    .map(|field| NativeField {
                        name: Name::new(self.db, field.name.as_str().to_owned()),
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
                let hint = self.pattern_mismatch_hint(pattern, subject);
                self.errors.push(TypeError {
                    range: node.range,
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
            let hint = self.pattern_mismatch_hint(pattern, subject);
            self.errors.push(TypeError {
                range: node.range,
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

    fn pattern_mismatch_hint(&self, pattern: PatId, subject: Ty<'db>) -> Option<String> {
        let PatternKind::Tag { name, payload, .. } = &self.module.pattern(pattern).kind else {
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
            return Some(format!("`:{}` carries no payload.", name.as_str()));
        }
        let Some((_, Some(tag_payload))) = found else {
            let nearest = nearest_name(name.as_str(), tags.iter().map(|(tag, _)| *tag))?;
            return Some(format!(
                "The value has no tag `:{}`. Did you mean `:{nearest}`?",
                name.as_str()
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
        let unknown = fields
            .iter()
            .find(|field| !known.contains(&field.name.as_str()))?;
        let suggestion = nearest_name(unknown.name.as_str(), known.iter().copied())
            .map(|nearest| format!(" Did you mean `{nearest}`?"))
            .unwrap_or_default();
        Some(format!(
            "`:{}` has no field `{}`.{suggestion}",
            name.as_str(),
            unknown.name.as_str()
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
                for &alternative in alternatives.iter() {
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
                for (position, &item) in items.iter().enumerate() {
                    let item_ty =
                        self.component_union(subject, |db, member| match member.kind(db) {
                            TyKind::NativeTuple(types) => types.get(position).copied(),
                            _ => None,
                        });
                    self.check_pattern(item, item_ty, bindings);
                }
            }
            PatternKind::Record { fields, .. } => {
                for field in fields.iter() {
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
                alternatives
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
                        && fields.iter().all(|field| {
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
            TyKind::Named(..) => Ok(any(self.db)),
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

    /// Nested function bodies are excluded: their tests cover their own formals.
    fn collect_missing_tested(&self, id: ExprId, tested: &mut FxHashSet<Symbol>) {
        let kind = &self.module.expression(id).kind;
        if matches!(kind, ExpressionKind::Function { .. }) {
            return;
        }
        if let ExpressionKind::Call { callee, arguments } = kind
            && matches!(
                &self.module.expression(*callee).kind,
                ExpressionKind::NameRef(name) if name == "missing"
            )
            && let [argument] = &arguments[..]
            && argument.name.is_none()
            && let Some(value) = argument.value
            && let ExpressionKind::NameRef(name) = &self.module.expression(value).kind
        {
            tested.insert(*name);
        }
        for child in kind.child_ids() {
            self.collect_missing_tested(child, tested);
        }
    }

    /// The formals without a default that a call may still omit, because R evaluates
    /// arguments lazily: the body tests them with `missing(name)` (R's
    /// optional-without-default idiom), or it dispatches with `UseMethod()`, which forces only
    /// the first formal and hands the rest to a method unevaluated.
    fn omissible_formals(&self, parameters: &[Formal], body: ExprId) -> FxHashSet<Symbol> {
        let mut omissible = FxHashSet::default();
        self.collect_missing_tested(body, &mut omissible);
        if self.dispatches(body) {
            omissible.extend(
                parameters
                    .iter()
                    .filter(|parameter| parameter.name != "...")
                    .skip(1)
                    .map(|parameter| parameter.name),
            );
        }
        omissible
    }

    /// A body that dispatches hands its formals to a method unevaluated.
    fn dispatches(&self, id: ExprId) -> bool {
        let kind = &self.module.expression(id).kind;
        if matches!(kind, ExpressionKind::Function { .. }) {
            return false;
        }
        if let ExpressionKind::Call { callee, .. } = kind
            && matches!(
                &self.module.expression(*callee).kind,
                ExpressionKind::NameRef(name)
                    if matches!(name.as_str(), "UseMethod" | "standardGeneric" | "NextMethod")
            )
        {
            return true;
        }
        kind.child_ids().into_iter().any(|child| self.dispatches(child))
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
        match &self.module.expression(id).kind {
            ExpressionKind::Break | ExpressionKind::Next => true,
            ExpressionKind::Call { callee, .. } => matches!(
                &self.module.expression(*callee).kind,
                ExpressionKind::NameRef(name) if name == "return" || name == "stop"
            ),
            ExpressionKind::Block { statements, .. } => {
                statements.last().is_some_and(|&last| self.diverges(last))
            }
            ExpressionKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => self.diverges(*then_branch) && self.diverges(*else_branch),
            ExpressionKind::Paren(inner) => self.diverges(*inner),
            ExpressionKind::Match { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|arm| self.diverges(arm.body))
            }
            _ => false,
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
        match &self.module.expression(condition).kind {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => {
                let inner = self.recognize_guard(*operand)?;
                // `!missing(x)` has no read-erroring edge to swap into.
                Some(GuardRefinement {
                    missing_on_true: false,
                    slot: inner.slot,
                    true_edge: inner.false_edge,
                    false_edge: inner.true_edge,
                })
            }
            ExpressionKind::Paren(inner) => self.recognize_guard(*inner),
            ExpressionKind::Call { callee, arguments } => {
                let ExpressionKind::NameRef(name) = &self.module.expression(*callee).kind else {
                    return None;
                };
                if self.naming.resolutions.contains_key(callee) {
                    return None;
                }
                let [argument] = &arguments[..] else {
                    return None;
                };
                if argument.name.is_some() {
                    return None;
                }
                let value = argument.value?;
                if !matches!(
                    self.module.expression(value).kind,
                    ExpressionKind::NameRef(_)
                ) {
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
                        .then_some(GuardRefinement {
                            missing_on_true: true,
                            slot,
                            true_edge: None,
                            false_edge: None,
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
                            missing_on_true: false,
                            slot,
                            true_edge: Some(null(self.db)),
                            false_edge: None,
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
                            missing_on_true: false,
                            slot,
                            true_edge: Some(shaped),
                            false_edge: Some(fresh),
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
        let edge = |kept: Vec<Ty<'db>>| -> Option<Ty<'db>> {
            if kept.is_empty() || kept.len() == members.len() {
                return None;
            }
            Some(union_of(self.db, kept))
        };
        GuardRefinement {
            missing_on_true: false,
            slot,
            true_edge: edge(true_members),
            false_edge: edge(false_members),
        }
    }

    fn infer_unary(&mut self, id: ExprId, operator: UnaryOperator, operand: ExprId) -> Ty<'db> {
        match operator {
            UnaryOperator::Minus => self.infer_unary_minus(operand),
            // `!!x` and `!!!x` are rlang's injection and splice operators as often as double
            // negation, and the operand of an injection can be any value.
            UnaryOperator::Not
                if matches!(
                    self.module.expression(operand).kind,
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
                } = self.module.expression(innermost).kind
                {
                    innermost = operand;
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
            NumericOperand::AnyUnknown => unknown(self.db),
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

    fn infer_unary_not(&mut self, operand: ExprId) -> Ty<'db> {
        let operand_range = self.blame_range(operand);
        let inferred = self.infer(operand);
        let resolved = self.structural(inferred);
        match resolved.kind(self.db) {
            // `!` coerces a numeric operand as a condition does (`!0` is TRUE).
            TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double) => {
                scalar(self.db, Atomic::Logical)
            }
            TyKind::Vector(element) | TyKind::NamedVector(element)
                if matches!(
                    element.kind(self.db),
                    TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
                        | TyKind::Var(_)
                        | TyKind::Any
                        | TyKind::Unknown
                ) =>
            {
                Ty::new(self.db, TyKind::Vector(scalar(self.db, Atomic::Logical)))
            }
            TyKind::Any | TyKind::Unknown => unknown(self.db),
            TyKind::Var(_) | TyKind::Rigid(_) => {
                self.unify_or_report(operand_range, scalar(self.db, Atomic::Logical), resolved);
                scalar(self.db, Atomic::Logical)
            }
            _ => {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::InvalidOperand {
                        expected: OperandExpectation::Logical,
                        found: resolved,
                    },
                });
                unknown(self.db)
            }
        }
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
            And2 | Or2 => {
                self.expect_scalar_logical(lhs);
                self.expect_scalar_logical(rhs);
                scalar(self.db, Atomic::Logical)
            }
            // Opaque constructs: operands still infer for the IDE, but their diagnostics are
            // discarded because nothing inside the construct is judged.
            And | Or | Special | Pipe | Tilde | Help => {
                let errors_mark = self.errors.len();
                let origins_mark = self.strict_origins.len();
                self.infer(lhs);
                self.infer(rhs);
                self.errors.truncate(errors_mark);
                self.strict_origins.truncate(origins_mark);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                unknown(self.db)
            }
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
        let arguments = [
            self.operand_argument(lhs, left),
            self.operand_argument(rhs, right),
        ];
        let class_of = |ty: Ty<'db>| match self.table.shallow_resolve(self.db, ty).kind(self.db) {
            TyKind::Named(name, _) => Some(name.text(self.db).to_owned()),
            _ => None,
        };
        let classes: Vec<String> = [class_of(left), class_of(right)]
            .into_iter()
            .flatten()
            .collect();
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
                        return Some(function.ret);
                    }
                    self.table.rollback(snapshot);
                }
            }
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
        while let ExpressionKind::Paren(inner) = &self.module.expression(id).kind {
            id = *inner;
        }
        self.module.expression(id).range
    }

    fn expect_scalar_logical(&mut self, condition: ExprId) {
        let condition_range = self.blame_range(condition);
        let inferred = self.infer(condition);
        let resolved = self.structural(inferred);
        if self.condition_coerces(resolved) && !self.is_long_combine(condition) {
            return;
        }
        // A value already required to be numeric (`if (n %% 2L)`) coerces like a number, so the
        // condition must not pin it to `logical`.
        if matches!(resolved.kind(self.db), TyKind::Var(_))
            && matches!(
                self.table.open_constraint(self.db, resolved),
                Some(Constraint::Numeric | Constraint::ScalarNumeric)
            )
        {
            return;
        }
        self.unify_or_report(condition_range, scalar(self.db, Atomic::Logical), resolved);
    }

    /// `c(a, b, ...)` with two or more elements is longer than one whatever its elements are,
    /// which is the one case where a vector condition's length is known.
    fn is_long_combine(&self, id: ExprId) -> bool {
        let id = match &self.module.expression(id).kind {
            ExpressionKind::Paren(inner) => *inner,
            _ => id,
        };
        let ExpressionKind::Call { callee, arguments } = &self.module.expression(id).kind else {
            return false;
        };
        matches!(&self.module.expression(*callee).kind, ExpressionKind::NameRef(name) if name == "c")
            && arguments.iter().filter(|argument| argument.value.is_some()).count() >= 2
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
        let resolved_left = self.structural(lhs_ty);
        let resolved_right = self.structural(rhs_ty);
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
                    // A vector result cannot carry an unresolved atomic, so default to `double`.
                    OperandShape::Vector => {
                        self.unify_or_report(range, flexible, scalar(self.db, Atomic::Double));
                        self.shaped(OperandShape::Vector, Atomic::Double)
                    }
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
            match resolved.kind(self.db) {
                TyKind::Scalar(Atomic::Integer) => {}
                TyKind::Scalar(Atomic::Double) if whole_literal => {}
                TyKind::Scalar(Atomic::Double) => result_atomic = Atomic::Double,
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

    fn infer_compare(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: Ty<'db>,
        rhs_ty: Ty<'db>,
    ) -> Ty<'db> {
        let lhs_range = self.blame_range(lhs);
        let rhs_range = self.blame_range(rhs);
        let resolved_left = self.structural(lhs_ty);
        let resolved_right = self.structural(rhs_ty);
        if matches!(resolved_left.kind(self.db), TyKind::Any | TyKind::Unknown)
            || matches!(resolved_right.kind(self.db), TyKind::Any | TyKind::Unknown)
        {
            return unknown(self.db);
        }

        let left_parts = comparison_operand_parts_list(self.db, resolved_left);
        let right_parts = comparison_operand_parts_list(self.db, resolved_right);
        let left_flexible = flexible_comparison_operand(self.db, resolved_left);
        let right_flexible = flexible_comparison_operand(self.db, resolved_right);

        for (parts, flexible, resolved, operand_range) in [
            (&left_parts, &left_flexible, resolved_left, lhs_range),
            (&right_parts, &right_flexible, resolved_right, rhs_range),
        ] {
            if parts.is_none() && flexible.is_none() {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::InvalidOperand {
                        expected: OperandExpectation::Comparable,
                        found: resolved,
                    },
                });
                return unknown(self.db);
            }
        }

        if let (Some(left_parts), Some(right_parts)) = (&left_parts, &right_parts)
            && left_parts.iter().any(|(_, left_family)| {
                right_parts
                    .iter()
                    .any(|(_, right_family)| left_family != right_family)
            })
        {
            self.errors.push(TypeError {
                // Blame the right operand: the message takes the left side as the expectation.
                range: rhs_range,
                kind: TypeErrorKind::Mismatch {
                    expected: resolved_left,
                    found: resolved_right,
                },
            });
            return unknown(self.db);
        }

        // Against a non-numeric family a flexible operand stays free: there is no
        // character-or-logical constraint.
        let all_numeric = |parts: &Option<Vec<(OperandShape, ComparisonFamily)>>| {
            parts.as_ref().is_some_and(|parts| {
                parts
                    .iter()
                    .all(|(_, family)| *family == ComparisonFamily::Numeric)
            })
        };
        if let Some(flexible) = &left_flexible
            && all_numeric(&right_parts)
            && let Some(ty) = flexible.constrainable()
        {
            self.constrain_numeric_flexible(lhs_range, ty);
        }
        if let Some(flexible) = &right_flexible
            && all_numeric(&left_parts)
            && let Some(ty) = flexible.constrainable()
        {
            self.constrain_numeric_flexible(rhs_range, ty);
        }

        let shapes = |parts: &Option<Vec<(OperandShape, ComparisonFamily)>>,
                      flexible: &Option<FlexibleComparisonOperand<'db>>|
         -> Vec<OperandShape> {
            match (parts, flexible) {
                (Some(parts), _) => parts.iter().map(|(shape, _)| *shape).collect(),
                (None, Some(FlexibleComparisonOperand::VectorElement(_))) => {
                    vec![OperandShape::Vector]
                }
                _ => vec![OperandShape::Scalar],
            }
        };
        let left_shapes = shapes(&left_parts, &left_flexible);
        let right_shapes = shapes(&right_parts, &right_flexible);
        let mut results = Vec::new();
        for left_shape in &left_shapes {
            for right_shape in &right_shapes {
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
                for &member in members {
                    match numeric_operand_parts(self.db, member) {
                        Some(part) => parts.push(part),
                        None => return NumericOperand::Invalid,
                    }
                }
                NumericOperand::ConcreteUnion(parts)
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
        let expression = module.expression(function_id);
        let ExpressionKind::Function { parameters, body } = &expression.kind else {
            return Ty::new(self.db, TyKind::Function(declared.clone()));
        };
        let range = expression.range;

        let omissible = self.omissible_formals(parameters, *body);
        let is_optional =
            |parameter: &Formal| parameter.default.is_some() || omissible.contains(&parameter.name);

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
        for parameter in parameters {
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
            let declared_ty = if let Some(index) = declared
                .named
                .iter()
                .position(|field| field.name.text(self.db) == parameter.name)
            {
                used_named[index] = true;
                Some(declared.named[index].ty)
            } else if positional_index < declared.positional.len() {
                let ty = declared.positional[positional_index];
                positional_index += 1;
                Some(ty)
            } else {
                // Partial annotation: an unannotated formal infers from its uses as if unannotated.
                None
            };
            let parameter_ty = declared_ty.unwrap_or_else(|| self.fresh(Constraint::Unconstrained));
            formal_types.push((parameter_ty, declared_ty.is_some()));
            signature.named.push(Parameter {
                name: Name::new(self.db, parameter.name.as_str()),
                ty: parameter_ty,
                optional: is_optional(parameter),
                default: parameter.default.map(|default| self.infer(default)),
            });
        }

        // A declared optional needs a real default, `...` must sit at the same boundary on
        // both sides, and every declared type needs a formal. The body still checks under
        // the declared types so IDE facts survive.
        let dots_index = parameters.iter().position(|p| p.name == "...");
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
        let optional_mismatch = parameters.iter().any(|parameter| {
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
                && parameters
                    .iter()
                    .any(|parameter| parameter.name == name && is_optional(parameter))
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
        let untyped_candidates: Vec<(&Formal, Option<Ty<'db>>)> = parameters
            .iter()
            .zip(&formal_types)
            .filter(|(parameter, _)| parameter.name != "...")
            .map(|(parameter, &(ty, declared))| (parameter, (!declared).then_some(ty)))
            .collect();
        for (parameter, (parameter_ty, declared)) in parameters.iter().zip(formal_types) {
            if let Some(slot) = self
                .naming
                .bindings
                .iter()
                .find(|(_, info)| info.range == parameter.range)
                .map(|(id, _)| *id)
            {
                self.environment.set(slot, EnvEntry::Mono(parameter_ty));
                if parameter.default.is_none() {
                    self.no_default_formals.insert(slot);
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
        match &self.module.expression(id).kind {
            ExpressionKind::Block { statements } => match statements.last() {
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

    /// A body that ends in `return(x)` or `stop()` never produces its trailing value, so only
    /// the early returns count: `function(x) return(x)` returns `x`'s type, not `x | NULL`.
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
        if let ExpressionKind::NameRef(name) = &self.module.expression(callee).kind {
            let builtin = match name.as_str() {
                "c" => Some(self.infer_combine(id, arguments)),
                "list" => Some(self.infer_list(arguments)),
                "switch" => Some(self.infer_switch(range, arguments)),
                "structure" => self.infer_structure(id, arguments),
                "return" => Some(self.infer_return(range, arguments)),
                "frame" => self.infer_frame_constructor(callee, arguments),
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
        if let Some(ty) = self.try_overloaded_call(id, range, callee, arguments) {
            return ty;
        }
        let callee_range = self.blame_range(callee);
        let callee_ty = self.infer(callee);
        let callee_ty = match self.callee_lookup(callee, callee_ty) {
            CalleeLookup::Binding => callee_ty,
            CalleeLookup::Skipped(scheme) => self.instantiate(&scheme),
            CalleeLookup::Undetermined => {
                self.infer_call_arguments(range, arguments);
                return unknown(self.db);
            }
        };
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
        let call_arguments = self.infer_call_arguments(range, arguments);
        self.dispatch_call(range, callee_range, callee_ty, &call_arguments)
    }

    /// `frame(id = 1:3, price = c(1, 2, 3))` builds `frame{id: integer[], price: double[]}`.
    /// A call with no arguments is graphics' `frame()` (a new plot page), and a project's
    /// own `frame` shadows the builtin, so both fall through to the ordinary call.
    fn infer_frame_constructor(
        &mut self,
        callee: ExprId,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let deferred = self.naming.deferred_non_locals.contains(&callee);
        if arguments.is_empty()
            || self.naming.resolutions.contains_key(&callee)
            || self
                .globals
                .is_some_and(|globals| globals.defined_in_project("frame", deferred))
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
            let forwards_dots = matches!(&module.expression(value).kind, ExpressionKind::NameRef(name) if name == "...");
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
        Some(Ty::new(self.db, TyKind::Frame(columns)))
    }

    /// The data.table function a call names, when the name reaches its stub: a local or
    /// project definition of the same name is an ordinary function.
    fn table_function(&self, callee: ExprId) -> Option<TableFunction> {
        let ExpressionKind::NameRef(name) = &self.module.expression(callee).kind else {
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
        let deferred = self.naming.deferred_non_locals.contains(&callee);
        let globals = self.globals?;
        (!self.naming.resolutions.contains_key(&callee)
            && !globals.defined_in_project(name, deferred)
            && globals.scheme(name, deferred).is_some())
        .then_some(function)
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
            TyKind::Frame(Some(columns)) => Some(columns.clone()),
            _ => None,
        }
    }

    /// `fread("data/prices.csv")` types with the columns the project's file has, as the host
    /// sniffed them; `fread(text = "...")` and an input holding a line break sniff the literal
    /// itself. Arguments that change the columns ry does not model leave them unknown.
    fn fread_type(&mut self, arguments: &[Argument]) -> Ty<'db> {
        let unknown_columns = Ty::new(self.db, TyKind::Frame(None));
        let module = self.module;
        let matched = match_formals(arguments, &["input", "file", "text", "cmd"]);
        let literal = |position: usize| {
            matched[position].and_then(|value| match &module.expression(value).kind {
                ExpressionKind::Literal(LiteralKind::String(text)) => decode_escapes(text),
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
                "stringsAsFactors" => match value.map(|value| &module.expression(value).kind) {
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
        frame_of(db, columns)
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
                        range: module.expression(old_value).range,
                        kind: TypeErrorKind::ColumnNameCount {
                            names: old.len(),
                            columns: names.len(),
                        },
                    });
                    return Some(Ty::new(db, TyKind::Frame(None)));
                }
                for ((column, _), (name, _)) in names.iter_mut().zip(old) {
                    *column = name;
                }
            }
            Some(new_value) => {
                let new = literal_strings(module, new_value)?;
                if new.len() != old.len() {
                    return Some(Ty::new(db, TyKind::Frame(None)));
                }
                let skip_absent = matched[3].is_some_and(|value| {
                    matches!(
                        module.expression(value).kind,
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
        Some(frame_of(db, names))
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
        Some(frame_of(db, moved))
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
                } = &module.expression(value).kind
            {
                value = *operand;
            }
            let expression = module.expression(value);
            let name = match &expression.kind {
                ExpressionKind::NameRef(name) => name.to_string(),
                ExpressionKind::Literal(LiteralKind::String(text)) => text.to_string(),
                _ => continue,
            };
            if !columns.iter().any(|column| column.name.text(db) == name) {
                self.errors.push(TypeError {
                    range: expression.range,
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
        let unknown_columns = Ty::new(db, TyKind::Frame(None));
        let matched = match_formals(arguments, &["l", "use.names", "fill", "idcol"]);
        let Some(list) = matched[0] else {
            return unknown_columns;
        };
        let ExpressionKind::Call {
            callee,
            arguments: items,
        } = &module.expression(list).kind
        else {
            return unknown_columns;
        };
        if variable_name(module, *callee).as_deref() != Some("list") || items.is_empty() {
            return unknown_columns;
        }
        let flag = |value: Option<ExprId>| match value.map(|value| &module.expression(value).kind) {
            None => Some(false),
            Some(ExpressionKind::Literal(LiteralKind::Logical(flag))) => Some(*flag),
            _ => None,
        };
        let (Some(by_name), Some(fill)) = (flag(matched[1]), flag(matched[2])) else {
            return unknown_columns;
        };
        let mut pieces = Vec::with_capacity(items.len());
        for item in items {
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
        match matched[3].map(|value| &module.expression(value).kind) {
            None | Some(ExpressionKind::Literal(LiteralKind::Logical(false))) => {}
            Some(ExpressionKind::Literal(LiteralKind::Logical(true))) => {
                columns.push((".id".to_owned(), self.list_id_type(items)));
            }
            Some(ExpressionKind::Literal(LiteralKind::String(name))) => {
                columns.push((name.to_string(), self.list_id_type(items)));
            }
            Some(_) => return unknown_columns,
        }
        for (name, types) in stacked {
            match widest_column(db, &types, true) {
                Some(ty) => columns.push((name, ty)),
                None => return unknown_columns,
            }
        }
        frame_of(db, columns)
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
                module.expression(value).kind,
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
                        range: module.expression(value).range,
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
            match value.map(|value| &module.expression(value).kind) {
                None => Some(default.to_owned()),
                Some(ExpressionKind::Literal(LiteralKind::String(text))) => Some(text.to_string()),
                Some(_) => None,
            }
        };
        let variable = name_of(matched[3], "variable")?;
        let value = name_of(matched[4], "value")?;
        let factor_variable = !arguments.iter().any(|argument| {
            argument.name.as_deref() == Some("variable.factor")
                && argument.value.is_some_and(|value| {
                    matches!(
                        module.expression(value).kind,
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
        Some(frame_of(db, result))
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
        let values: Vec<ExprId> = arguments.iter().filter_map(|a| a.value).collect();
        let inferred: Vec<Ty<'db>> = values
            .iter()
            .map(|&value| {
                let ty = self.infer(value);
                self.structural(ty)
            })
            .collect();
        if inferred
            .iter()
            .any(|&ty| self.list_element_type(ty).is_some())
        {
            let elements: Vec<Ty<'db>> = inferred
                .iter()
                .map(|&ty| self.list_element_type(ty).unwrap_or(ty))
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
        for argument in arguments {
            let Some(value) = argument.value else {
                continue;
            };
            let argument_range = self.blame_range(value);
            let inferred = self.infer(value);
            let resolved = self.structural(inferred);
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
            let Some(argument_atomics) = argument_atomics.filter(|atomics| !atomics.is_empty())
            else {
                // R's default `c()` strips a nominal into something the checker cannot name:
                // indeterminate, not an error.
                if matches!(resolved.kind(self.db), TyKind::Named(..)) {
                    self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                    result_indeterminate = true;
                    continue;
                }
                self.errors.push(TypeError {
                    range: argument_range,
                    kind: TypeErrorKind::Mismatch {
                        expected: scalar(self.db, Atomic::Integer),
                        found: resolved,
                    },
                });
                result_indeterminate = true;
                continue;
            };
            for current_atomic in argument_atomics {
                item_atomic = Some(match item_atomic {
                    Some(previous_atomic) => {
                        match promote_combine_atomic(previous_atomic, current_atomic) {
                            Some(promoted) => promoted,
                            None => {
                                self.errors.push(TypeError {
                                    range: argument_range,
                                    kind: TypeErrorKind::Mismatch {
                                        expected: scalar(self.db, previous_atomic),
                                        found: resolved,
                                    },
                                });
                                result_indeterminate = true;
                                previous_atomic
                            }
                        }
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
        let mut members = Vec::with_capacity(branches.len() + 1);
        let mut has_default = false;
        // Exactly one alternative runs, so branches fork and join like `if` arms; inferring
        // them in sequence would let a later branch's write win outright.
        let mark = self.environment.mark();
        let mut branch_writes = Vec::with_capacity(branches.len());
        for branch in branches {
            if branch.name.is_none() {
                has_default = true;
            }
            let Some(value) = branch.value else {
                continue;
            };
            let ty = self.infer(value);
            members.push(self.table.resolve(self.db, ty));
            if !self.diverges(value) {
                branch_writes.push(self.environment.writes_since(mark));
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

    /// Overload results that differ only in container shape (`lapply` over an unchecked
    /// vector) stay guessable; a differing `integer` against `double` would mislead.
    fn is_numeric_result(&self, ty: Ty<'db>) -> bool {
        match ty.kind(self.db) {
            TyKind::Scalar(Atomic::Integer | Atomic::Double) => true,
            TyKind::Vector(element) => {
                matches!(
                    element.kind(self.db),
                    TyKind::Scalar(Atomic::Integer | Atomic::Double)
                )
            }
            _ => false,
        }
    }

    fn caller_variables_untouched(&self, entries: &[(InferenceVar, Entry<'db>)]) -> bool {
        entries
            .iter()
            .all(|(var, entry)| self.table.find(*var) == *var && self.table.entry(*var) == entry)
    }

    /// Declaration order is only the tiebreak: a candidate that fits without narrowing
    /// the caller's open variables outranks an earlier one that pins one down. The
    /// literal-as-integer courtesy runs only in a second round so a literal never steers selection.
    /// R's function lookup skips every binding whose value is not a function, so in
    /// `function(x, log = FALSE) if (log) log(x) else x` the call reaches base `log`, and a
    /// package's `trimws <- NULL` placeholder does not hide base `trimws`. A binding the call
    /// would skip reads as the next declaration out; a binding that may or may not be a
    /// function, such as an unconstrained parameter that shares its name with one, cannot be
    /// decided statically.
    fn callee_lookup(&mut self, callee: ExprId, callee_ty: Ty<'db>) -> CalleeLookup<'db> {
        let ExpressionKind::NameRef(name) = &self.module.expression(callee).kind else {
            return CalleeLookup::Binding;
        };
        let local = self.naming.resolutions.contains_key(&callee);
        let resolved = self.table.resolve(self.db, callee_ty);
        let callable = match resolved.kind(self.db) {
            TyKind::Var(_) if local => None,
            TyKind::Union(members) => {
                if members
                    .iter()
                    .any(|member| !matches!(member.kind(self.db), TyKind::Null))
                {
                    return CalleeLookup::Binding;
                }
                Some(false)
            }
            TyKind::Null => Some(false),
            TyKind::Scalar(_)
            | TyKind::Vector(_)
            | TyKind::NamedVector(_)
            | TyKind::List(_)
            | TyKind::NamedList(_)
            | TyKind::Tuple(_)
            | TyKind::Record(_)
            | TyKind::NativeTuple(_)
            | TyKind::NativeRecord(_)
            | TyKind::Tag(..)
            | TyKind::Frame(_) => Some(false),
            _ => return CalleeLookup::Binding,
        };
        let Some(globals) = self.globals else {
            return CalleeLookup::Binding;
        };
        let outer = if local {
            globals.scheme(name, true)
        } else {
            globals.stub_scheme(name)
        };
        let outer = outer.filter(|scheme| {
            matches!(
                scheme.body.kind(self.db),
                TyKind::Function(_) | TyKind::Any | TyKind::Unknown
            )
        });
        match (callable, outer) {
            (None, Some(_)) => CalleeLookup::Undetermined,
            (None, None) => CalleeLookup::Binding,
            (Some(_), Some(scheme)) => CalleeLookup::Skipped(scheme),
            // A non-local `NULL` with nothing behind it is the placeholder a package fills in
            // from `.onLoad` (`f <- NULL`, then `f <<- function(...)`), so the call is unchecked.
            (Some(_), None) if !local && matches!(resolved.kind(self.db), TyKind::Null) => {
                CalleeLookup::Undetermined
            }
            (Some(_), None) => CalleeLookup::Binding,
        }
    }

    /// The type a call's local callee holds right now, read without recording anything.
    fn local_callee_type(&mut self, callee: ExprId) -> Option<Ty<'db>> {
        let slot = *self.naming.resolutions.get(&callee)?;
        match self.environment.get(slot)? {
            EnvEntry::Mono(ty) => Some(ty),
            EnvEntry::Scheme(index) => Some(self.scheme_arena[index as usize].body),
            EnvEntry::MissingFormal(_) => None,
        }
    }

    fn try_overloaded_call(
        &mut self,
        id: ExprId,
        range: TextRange,
        callee: ExprId,
        arguments: &[Argument],
    ) -> Option<Ty<'db>> {
        let name = match &self.module.expression(callee).kind {
            ExpressionKind::NameRef(name) => {
                if self.naming.resolutions.contains_key(&callee) {
                    let callee_ty = self.local_callee_type(callee)?;
                    if !matches!(
                        self.callee_lookup(callee, callee_ty),
                        CalleeLookup::Skipped(_)
                    ) {
                        return None;
                    }
                }
                name.to_string()
            }
            // Namespace access cannot be shadowed by a local binding.
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => validated_namespace_name(self.db, *internal, package, name)?,
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
        let call_arguments = self.infer_call_arguments(range, arguments);
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
                    matches!(
                        self.table.shallow_resolve(self.db, ty).kind(self.db),
                        TyKind::Unknown | TyKind::Any
                    )
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
                .any(|(_, _, results)| results.iter().any(|&result| self.is_numeric_result(result)))
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
        let blamed: Vec<Option<usize>> = candidate_errors
            .iter()
            .map(|error| {
                call_arguments
                    .iter()
                    .position(|argument| argument.range == error.range)
            })
            .collect();
        let single_answer = if candidate_errors.len() != schemes.len() {
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
        arguments
            .iter()
            .map(|argument| {
                let ty = argument.value.map(|value| self.infer(value));
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
                }
            })
            .collect()
    }

    fn is_whole_double(&self, value: ExprId) -> bool {
        match &self.module.expression(value).kind {
            ExpressionKind::Literal(LiteralKind::Double(text)) => is_whole_number_double(text),
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
                let positional: Vec<Ty<'db>> = arguments
                    .iter()
                    .filter(|argument| argument.name.is_none())
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
                        variadic: None,
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
                        && let Some(ty) = argument.ty
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
                        && let Some(ty) = argument.ty
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
                        && let Some(ty) = argument.ty
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
                        return vec![TypeError {
                            // The first argument with no formal left is the one to remove.
                            range: argument.range,
                            kind: TypeErrorKind::ArityMismatch {
                                expected: total,
                                found: arguments.len(),
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
        let missing_required = function.named.iter().enumerate().any(|(formal, field)| {
            !field.optional
                && !targets
                    .iter()
                    .any(|target| matches!(target, Some(ArgumentTarget::Named(filled)) if *filled == formal))
        });
        if !forwards_dots
            && !unknown_name
            && (positional_filled != function.positional.len() || missing_required)
        {
            return vec![TypeError {
                range: callee_range,
                kind: TypeErrorKind::ArityMismatch {
                    expected: required,
                    found: arguments.len(),
                },
            }];
        }
        findings
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
        // R then matches a name by unique prefix against the formals before `...`, so
        // `rep(x, len = 3)` fills `length.out`.
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
        let is_query = !double && matches!(subject.kind(self.db), TyKind::Frame(_));
        let result = if is_query {
            self.query_depth += 1;
            let result = self.infer_frame_query(id, target, subject, arguments);
            self.query_depth -= 1;
            result
        } else {
            self.infer_plain_index(id, range, double, target, subject, arguments)
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
        // An `Unknown`/`Any` subject's gap was already diagnosed, so `m[i, j]` must not
        // cascade an arity error.
        match subject.kind(self.db) {
            TyKind::Unknown => return unknown(self.db),
            TyKind::Any => return any(self.db),
            // A shape the author never wrote down (a sealed nominal, or `m` in
            // `function(m, i, j) m[i, j]`) refuses before the index-arity check: the caller
            // knows the shape, the callee does not.
            // R dispatches `[` on the class, so a stub's `[.factor` gives the result type.
            TyKind::Named(name, _)
                if let Some(ty) = self.index_method_result(name.text(self.db), double) =>
            {
                return ty;
            }
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
            let index = arguments[0]
                .value
                .map(|value| &module.expression(value).kind);
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
        let named = |formal: &str| {
            arguments
                .iter()
                .find(|argument| argument.name.as_deref() == Some(formal))
        };
        let (i, j) = query_index_arguments(arguments);
        let by = named("by").or_else(|| named("keyby"));
        let on = named("on");
        let frame = variable_name(module, target);
        let bracket = module.expression(id).range;
        let subject_columns = frame_columns(db, subject);
        let mut callees = FxHashSet::default();
        for value in arguments.iter().filter_map(|argument| argument.value) {
            collect_callees(module, value, &mut callees);
        }
        let i_value = i.and_then(|argument| argument.value);
        let mut source_columns = subject_columns.clone();
        let mut join = None;
        let mut layout = None;
        let each_i = by
            .and_then(|argument| argument.value)
            .is_some_and(|value| {
                matches!(&module.expression(value).kind, ExpressionKind::NameRef(name) if name == ".EACHI")
            });
        if let Some(on_value) = on.and_then(|argument| argument.value) {
            // The joined frame evaluates in the caller's scope, not the queried frame's.
            let joined = i_value.map(|value| {
                let ty = self.infer(value);
                self.structural(ty)
            });
            source_columns = match joined.map(|joined| joined.kind(db)) {
                Some(TyKind::Frame(joined_columns)) => {
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
                                    &module.expression(roll_value).kind,
                                    ExpressionKind::Literal(LiteralKind::Logical(false))
                                ) && !matches!(
                                    &module.expression(roll_value).kind,
                                    ExpressionKind::Literal(LiteralKind::Integer(text) | LiteralKind::Double(text))
                                        if text.trim_end_matches('L').parse::<f64>() == Ok(0.0)
                                ) && !(matches!(
                                    &module.expression(roll_value).kind,
                                    ExpressionKind::NameRef(name) if name == "F"
                                ) && !self.naming.resolutions.contains_key(&roll_value))
                            {
                                self.errors.push(TypeError {
                                    range: roll.name_range.map_or_else(
                                        || module.expression(roll_value).range,
                                        |name| name.cover(module.expression(roll_value).range),
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
                        subset_data: Ty::new(db, TyKind::Frame(None)),
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
            subset_data: Ty::new(db, TyKind::Frame(None)),
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
        let subset_data = Ty::new(db, TyKind::Frame(subset_columns));
        if let Some(scope) = self.column_scopes.last_mut() {
            scope.subset_data = subset_data;
        }
        let j_value = j.and_then(|argument| argument.value);
        let update = j_value.and_then(|value| match &module.expression(value).kind {
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
                TyKind::Frame(self.column_update(
                    update,
                    subject_columns,
                    i_value.is_some() || by.is_some(),
                )),
            ),
            // `with = FALSE` and a `..name` `j` read `j` as column names or positions, which ry
            // does not model.
            Some(value)
                if named("with").is_some()
                    || matches!(
                        &module.expression(value).kind,
                        ExpressionKind::NameRef(name) if dot_dot_variable(name).is_some()
                    ) =>
            {
                self.infer(value);
                Ty::new(db, TyKind::Frame(None))
            }
            Some(value) => {
                self.query_result(id, value, source_columns.as_deref(), groups, by.is_some())
            }
            None => Ty::new(db, TyKind::Frame(source_columns)),
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
        match &module.expression(value).kind {
            ExpressionKind::NameRef(name) => {
                let ty = self.infer(value);
                (name != ".EACHI")
                    .then(|| self.column_field(name, ty))
                    .map(|field| vec![field])
            }
            ExpressionKind::Literal(LiteralKind::String(text)) => {
                self.literal_groups(&[(&**text, module.expression(value).range)])
            }
            ExpressionKind::Call { callee, arguments } => {
                match variable_name(module, *callee).as_deref() {
                    Some("c") if string_literals(module, arguments).is_some() => {
                        let literals: Vec<(&str, TextRange)> = arguments
                            .iter()
                            .filter_map(|argument| argument.value)
                            .filter_map(|item| match &module.expression(item).kind {
                                ExpressionKind::Literal(LiteralKind::String(text)) => {
                                    Some((&**text, module.expression(item).range))
                                }
                                _ => None,
                            })
                            .collect();
                        self.literal_groups(&literals)
                    }
                    Some("." | "list") => {
                        let mut groups = Some(Vec::with_capacity(arguments.len()));
                        for argument in arguments {
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
            while let ExpressionKind::Paren(inner) = &module.expression(id).kind {
                id = *inner;
            }
            id
        };
        let (negated, selection) = match &module.expression(unparenthesized(value)).kind {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not | UnaryOperator::Minus,
                operand,
            } => (true, unparenthesized(*operand)),
            _ => (false, unparenthesized(value)),
        };
        let frame = self.column_scopes.last().and_then(|scope| scope.frame);
        let position = |checker: &mut Self, item: ExprId| {
            let expression = module.expression(item);
            let name = match &expression.kind {
                ExpressionKind::Literal(LiteralKind::String(text)) => text.to_string(),
                ExpressionKind::NameRef(name) => name.to_string(),
                _ => return Err(()),
            };
            let found = columns
                .iter()
                .position(|column| column.name.text(db) == name.as_str());
            if found.is_none() {
                checker.errors.push(TypeError {
                    range: expression.range,
                    kind: column_does_not_exist(db, &name, frame, columns),
                });
            }
            Ok(found)
        };
        let selected: Vec<bool> = match &module.expression(selection).kind {
            ExpressionKind::Literal(LiteralKind::String(_)) => {
                let found = position(self, selection).ok()??;
                (0..columns.len()).map(|index| index == found).collect()
            }
            ExpressionKind::Call { callee, arguments }
                if variable_name(module, *callee).as_deref() == Some("c")
                    && string_literals(module, arguments).is_some() =>
            {
                let mut chosen = vec![false; columns.len()];
                let mut complete = true;
                for item in arguments.iter().filter_map(|argument| argument.value) {
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
                        &module.expression(item).kind,
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
        let ExpressionKind::Call { callee, arguments } = &module.expression(value).kind else {
            return None;
        };
        let bound = bind_formals(arguments, &["X", "FUN"]);
        let subset = bound.value(0)?;
        if variable_name(module, *callee).as_deref() != Some("lapply")
            || self.naming.resolutions.contains_key(callee)
            || !matches!(&module.expression(subset).kind, ExpressionKind::NameRef(name) if name == ".SD")
        {
            return None;
        }
        let subset_data = self.column_scopes.last()?.subset_data;
        let columns = match subset_data.kind(db) {
            TyKind::Frame(Some(columns)) if !columns.is_empty() => columns.clone(),
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
            let forwarded: Vec<Argument> = arguments
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
        let range = module.expression(call).range;
        let mut call_arguments: Vec<CallArgument<'db>> = leading
            .iter()
            .map(|&argument| CallArgument {
                name: None,
                name_range: None,
                ty: None,
                range: module.expression(argument).range,
                value: None,
                whole_double: false,
                forwards_dots: false,
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
        let ExpressionKind::NameRef(name) = &self.module.expression(function).kind else {
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
            Ty::new(db, TyKind::Frame(columns))
        };
        if let Some(applied) = self.subset_apply(j) {
            return frame_of(groups.zip(applied).map(|(mut groups, applied)| {
                groups.extend(applied);
                groups
            }));
        }
        if let ExpressionKind::Call { callee, arguments } = &module.expression(j).kind
            && matches!(
                variable_name(module, *callee).as_deref(),
                Some("." | "list")
            )
        {
            let mut columns = groups;
            for (position, argument) in arguments.iter().enumerate() {
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
        let bare_column = match &module.expression(j).kind {
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
                | TyKind::Frame(_)
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
        let count =
            matches!(&module.expression(j).kind, ExpressionKind::NameRef(name) if name == ".N");
        match (bare_column, resolved.kind(db)) {
            (Some(name), _) => columns.push(self.column_field(&name, resolved)),
            (None, _) if count => columns.push(self.column_field("N", resolved)),
            (None, TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_)) => {
                columns.push(self.column_field("V1", resolved));
            }
            (None, TyKind::Frame(Some(computed))) => columns.extend(computed.iter().cloned()),
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
                let names: Option<Vec<String>> = match left
                    .value
                    .map(|value| (value, &module.expression(value).kind))
                {
                    Some((_, ExpressionKind::NameRef(name))) => Some(vec![name.to_string()]),
                    Some((value, _)) => {
                        let names = self
                            .column_names(value)
                            .map(|names| names.into_iter().map(|(name, _)| name).collect());
                        if names.is_none()
                            || matches!(&module.expression(value).kind, ExpressionKind::Paren(_))
                        {
                            self.infer(value);
                        }
                        names
                    }
                    None => None,
                };
                let items: Option<Vec<ExprId>> =
                    right
                        .value
                        .and_then(|value| match &module.expression(value).kind {
                            ExpressionKind::Call { callee, arguments }
                                if matches!(
                                    variable_name(module, *callee).as_deref(),
                                    Some("." | "list")
                                ) =>
                            {
                                Some(
                                    arguments
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
            name: Name::new(self.db, name.to_owned()),
            ty: column_type(self.db, resolved),
            optional: false,
        }
    }

    /// Nested function bodies are included: a closure written inside `j` is created in
    /// the data's frame.
    fn mask_column_reads(&mut self, id: ExprId) {
        let kind = &self.module.expression(id).kind;
        if matches!(kind, ExpressionKind::NameRef(_)) {
            self.masked_reads.insert(id);
        }
        for &child in kind.child_ids().iter() {
            self.mask_column_reads(child);
        }
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
            for &member in members {
                let result = self
                    .extract_result(origin, range, key_range, member, index)
                    .map_err(|error| widen_error_container(error, subject))?;
                results.push(result);
            }
            return Ok(union_of(self.db, results));
        }
        let literal_position = index.and_then(integer_literal_position);
        let literal_name = match index {
            Some(ExpressionKind::Literal(LiteralKind::String(name))) => Some(name.to_string()),
            _ => None,
        };
        match subject.kind(self.db) {
            TyKind::Unknown => Ok(unknown(self.db)),
            TyKind::Any => Ok(any(self.db)),
            // R reads every element of `NULL` as `NULL`, so `opts$name` on an optional list is the
            // ordinary way to read a setting that may be absent.
            TyKind::Null => Ok(subject),
            TyKind::Scalar(atomic) => Ok(scalar(self.db, *atomic)),
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
            TyKind::Frame(None) => Ok(unknown(self.db)),
            TyKind::Frame(Some(columns)) => match (literal_position, literal_name) {
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
                            None => {
                                let suggestion = nearest_name(
                                    &name,
                                    fields.iter().map(|field| field.name.text(self.db)),
                                );
                                Err(TypeError {
                                    range: key_range,
                                    kind: TypeErrorKind::FieldDoesNotExist {
                                        suggestion,
                                        field: name,
                                        container: subject,
                                    },
                                })
                            }
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
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Frame(_) => {
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
            | TyKind::Frame(_)
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
        // R answers `NULL` for a name a list lacks (the accumulator idiom), so a member
        // missing the field makes the read nullable; a field no member carries is a typo,
        // and `$` on an atomic member is always an error.
        if let TyKind::Union(members) = subject.kind(self.db) {
            let mut results = Vec::with_capacity(members.len());
            let mut absent: Option<TypeError<'db>> = None;
            for &member in members {
                match self.dollar_result(origin, range, name_range, member, name) {
                    Ok(result) => results.push(result),
                    Err(error) if matches!(error.kind, TypeErrorKind::FieldDoesNotExist { .. }) => {
                        absent.get_or_insert(error);
                    }
                    Err(error) => return Err(widen_error_container(error, subject)),
                }
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
                (_, absent) => {
                    if absent.is_some() {
                        results.push(null(self.db));
                    }
                    Ok(union_of(self.db, results))
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
            TyKind::NamedList(element) => Ok(union_of(self.db, [*element, null(self.db)])),
            TyKind::Frame(None) => Ok(unknown(self.db)),
            TyKind::Frame(Some(columns)) => {
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
            TyKind::Record(fields) => {
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
            TyKind::Tuple(_) | TyKind::NativeTuple(_) | TyKind::List(_) => Err(TypeError {
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
                    Some(EnvEntry::Mono(self.join_types(a, b)))
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

    fn instantiate(&mut self, scheme: &TypeScheme<'db>) -> Ty<'db> {
        let mut substitution: FxHashMap<Name<'db>, Ty<'db>> = FxHashMap::default();
        for (name, constraint) in &scheme.binders {
            let fresh = self.fresh(*constraint);
            substitution.insert(*name, fresh);
        }
        substitute_rigid(self.db, scheme.body, &substitution)
    }
}

fn is_character<'db>(db: &'db dyn Db, ty: Ty<'db>) -> bool {
    match ty.kind(db) {
        TyKind::Scalar(Atomic::Character) => true,
        TyKind::Vector(element) | TyKind::NamedVector(element) => {
            matches!(element.kind(db), TyKind::Scalar(Atomic::Character))
        }
        _ => false,
    }
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

/// `raw` sits outside R's coercion hierarchy and only combines with itself.
fn promote_combine_atomic(left: Atomic, right: Atomic) -> Option<Atomic> {
    if left == right {
        return Some(left);
    }
    let rank = |atomic: Atomic| match atomic {
        Atomic::Logical => Some(0u8),
        Atomic::Integer => Some(1),
        Atomic::Double => Some(2),
        Atomic::Complex => Some(3),
        Atomic::Character => Some(4),
        Atomic::Raw => None,
    };
    Some(match rank(left)?.max(rank(right)?) {
        0 => Atomic::Logical,
        1 => Atomic::Integer,
        2 => Atomic::Double,
        3 => Atomic::Complex,
        _ => Atomic::Character,
    })
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
                match &module.expression(literal).kind {
                    ExpressionKind::Unary {
                        operator: UnaryOperator::Minus,
                        operand,
                    } => literal = *operand,
                    ExpressionKind::Paren(inner) => literal = *inner,
                    _ => break,
                }
            }
            matches!(
                &module.expression(literal).kind,
                ExpressionKind::Literal(LiteralKind::Double(text))
                    if text.parse::<f64>().is_ok_and(|number| number.fract() != 0.0)
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
    match &module.expression(id).kind {
        ExpressionKind::NameRef(name) => Some(*name),
        _ => None,
    }
}

/// Whether an expression is a literal, possibly negated: certainly one value.
fn is_literal(module: &Module, id: ExprId) -> bool {
    match &module.expression(id).kind {
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
        TyKind::Frame(columns) => columns.clone(),
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
        TyKind::Record(fields) | TyKind::Frame(Some(fields)) => {
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
    let expression = module.expression(value);
    match &expression.kind {
        ExpressionKind::Literal(LiteralKind::String(text)) => {
            Some(vec![(text.to_string(), expression.range)])
        }
        ExpressionKind::Call { callee, arguments }
            if variable_name(module, *callee).as_deref() == Some("c") =>
        {
            arguments
                .iter()
                .map(|argument| {
                    let item = argument.value.filter(|_| argument.name.is_none())?;
                    let expression = module.expression(item);
                    match &expression.kind {
                        ExpressionKind::Literal(LiteralKind::String(text)) => {
                            Some((text.to_string(), expression.range))
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
fn frame_of<'db>(db: &'db dyn Db, columns: Vec<(String, Ty<'db>)>) -> Ty<'db> {
    let mut fields: Vec<RecordField<'db>> = Vec::with_capacity(columns.len());
    for (name, ty) in columns {
        if fields.iter().any(|field| field.name.text(db) == name) {
            return Ty::new(db, TyKind::Frame(None));
        }
        fields.push(RecordField {
            name: Name::new(db, name),
            ty: column_type(db, ty),
            optional: false,
        });
    }
    Ty::new(db, TyKind::Frame(Some(fields)))
}

fn factor_type(db: &dyn Db) -> Ty<'_> {
    Ty::new(
        db,
        TyKind::Named(Name::new(db, "factor".to_owned()), Vec::new()),
    )
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
            |argument| match argument.value.map(|value| &module.expression(value).kind) {
                Some(ExpressionKind::Literal(LiteralKind::String(text)))
                    if argument.name.is_none() =>
                {
                    Some(text.to_string())
                }
                _ => None,
            },
        )
        .collect()
}

fn collect_callees(module: &Module, id: ExprId, callees: &mut FxHashSet<ExprId>) {
    let kind = &module.expression(id).kind;
    if let ExpressionKind::Call { callee, .. } = kind
        && matches!(module.expression(*callee).kind, ExpressionKind::NameRef(_))
    {
        callees.insert(*callee);
    }
    for child in kind.child_ids() {
        collect_callees(module, child, callees);
    }
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
        | TyKind::Frame(_)
        | TyKind::Function(_) => false,
    }
}

/// The `on =` forms data.table documents: a string of comma-separated conditions (`"id"`,
/// `"a==b"`, `"t>=lo,t<hi"`), `c()` of such strings or of `x = "y"` pairs, and `.()` or
/// `list()` of names, `x = y` pairs and comparisons (`t >= lo`). `None` for anything else,
/// such as a variable holding the keys.
fn join_keys(module: &Module, on: ExprId) -> Option<Vec<JoinKey>> {
    let expression = module.expression(on);
    match &expression.kind {
        ExpressionKind::Literal(LiteralKind::String(text)) => {
            split_join_keys(text, expression.range)
        }
        ExpressionKind::Call { callee, arguments } => {
            let callee_name = variable_name(module, *callee)?;
            let mut keys = Vec::new();
            for argument in arguments {
                let value = argument.value?;
                let value_range = module.expression(value).range;
                let x_range = argument.name_range.unwrap_or(value_range);
                match (callee_name.as_str(), &module.expression(value).kind) {
                    ("c", ExpressionKind::Literal(LiteralKind::String(text))) => {
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
                            x_range: module.expression(*lhs).range,
                            y_range: module.expression(*rhs).range,
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
        &module.expression(value).kind,
        ExpressionKind::Call { callee, .. }
            if matches!(
                &module.expression(*callee).kind,
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

/// With no stub corpus installed validation is impossible, so every qualified read resolves.
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
            for &alternative in inner.iter() {
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
    use super::*;
    use crate::RootDatabase;
    use crate::annotations::lower_annotation;
    use crate::hir::lower_item;
    use crate::naming::resolve_item;
    use syntax::SyntaxKind;
    use syntax::ast::is_expression_kind;

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
    fn comparisons_share_a_family_and_shape_elementwise() {
        let db = RootDatabase::default();
        let flexible = check_source(&db, "positive <- function(x) x > 0L");
        assert!(flexible.errors.is_empty(), "{:?}", flexible.errors);
        let scheme = flexible.scheme.clone().expect("scheme");
        assert_eq!(scheme.binders.len(), 1);
        assert_eq!(scheme.binders[0].1, Constraint::Numeric);
        assert_eq!(scheme_ret(&db, &flexible), scalar(&db, Atomic::Logical));
        let vector = check_source(&db, "f <- function() c(1L, 2L) > 1L");
        assert!(vector.errors.is_empty(), "{:?}", vector.errors);
        assert!(matches!(
            scheme_ret(&db, &vector).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Logical)
        ));
        let mixed = check_source(&db, "f <- function() 1L > \"a\"");
        assert!(
            mixed
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::Mismatch { .. })),
            "{:?}",
            mixed.errors
        );
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
                .any(|error| matches!(error.kind, TypeErrorKind::ArityMismatch { .. })),
            "expected a missing-argument arity error, got {:?}",
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
        let missing_field =
            check_source(&db, "f <- function() {\n  p <- list(a = 1L)\n  p$oops\n}");
        assert!(
            missing_field.errors.iter().any(|error| matches!(
                &error.kind,
                TypeErrorKind::FieldDoesNotExist { field, .. } if field == "oops"
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
