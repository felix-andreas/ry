//! The expression inference walk: Hindley–Milner over one item's HIR.
//!
//! The environment is keyed by naming's variable slots and mirrors the mutable
//! slot model: every write funnels through an undo log so branches roll back
//! without cloning, and merge points join entries (equal keeps; different
//! joins as a monotype union). Function-valued assignments generalize at the
//! binding (level-gated so escaped variables stay monomorphic); reads
//! instantiate schemes with fresh variables. Arithmetic rides the numeric
//! constraint; comparisons and logic produce logicals; `if` joins branches by
//! unify-else-union exactly like the legacy contract.
//!
//! This is the foundation walk: parameter-position coercions, overload sets,
//! `#:` annotation enforcement, strict origins, and cross-item schemes layer
//! on next — each a separate slice over this structure.

use crate::Db;
use crate::hir::{
    Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind, Module,
    UnaryOperator,
};
use crate::infer::{Entry, FunctionMismatch, InferenceTable, RecordMismatch, UnifyError};
use crate::naming::{BindingId, ItemNaming};
use crate::types::{
    Atomic, Constraint, FunctionType, InferenceVar, Name, Parameter, RestParameter, Ty, TyKind,
    TypeScheme, any, scalar, union_of, unknown,
};
use rustc_hash::{FxHashMap, FxHashSet};
use syntax::TextRange;

/// A structured type finding; rendering to wording happens at the diagnostic
/// edge.
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
    /// Two record types that differ in one field. Its own variant rather than a
    /// mismatch between the two whole types, because those print as long
    /// near-identical strings the reader has to diff by eye — and a nested
    /// difference is never attributed to a path at all.
    RecordShape {
        mismatch: Box<RecordMismatch<'db>>,
    },
    NotAFunction {
        found: Ty<'db>,
    },
    /// A read of a no-default formal on the branch where `missing(name)`
    /// held — it would fail at run time.
    MissingFormalRead {
        name: String,
    },
    /// The call's argument count cannot fill the function's formals (too many
    /// positionals, or a required formal left unfilled).
    ArityMismatch {
        expected: usize,
        found: usize,
    },
    /// A named argument matching no declared formal (or duplicating one
    /// already given). Blamed on the offending name's own token and carrying
    /// it, so the reader is never left diffing two comma-separated lists to
    /// find which one the call got wrong.
    NamedArgumentMismatch {
        argument: String,
        duplicate: bool,
        suggestion: Option<String>,
        expected_parameters: Vec<String>,
    },
    /// A callee that may be `NULL` — callable on every other path, so the
    /// finding is the nullability rather than "not a function".
    MaybeNullCallee {
        found: Ty<'db>,
    },
    /// `#: @if-unknown` on a value whose type the checker already knows.
    KnownTypeUnderIfUnknown {
        found: Ty<'db>,
    },
    /// An annotation declares a parameter the definition has no formal for.
    /// An annotation declares a parameter required while the function gives it
    /// a default. R decides optionality, so the annotation is the thing that is
    /// wrong — and reporting it here rather than at every caller is the
    /// difference between "my annotation is wrong" and "this tool is broken".
    AnnotationRequiredButDefaulted {
        name: String,
    },
    AnnotationParameterMismatch {
        name: String,
    },
    /// A `NULL` default on a parameter whose declared type does not admit
    /// `NULL`. Its own variant rather than a plain mismatch because it is the
    /// usual R spelling of an optional argument and the remedy is specific.
    NullDefaultNotAdmitted {
        name: String,
        declared: Ty<'db>,
    },
    /// An annotation mentions a type alias whose expansion re-enters itself.
    AliasCycle {
        name: String,
    },
    /// A constraint (numeric, atomic) rejected the value.
    ConstraintViolation {
        constraint: Constraint,
        found: Ty<'db>,
    },
    /// A function value does not fit an expected function type, and the reason
    /// is one position in its signature. Its own variant rather than a mismatch
    /// between the two whole signatures, because those say nothing about which
    /// position failed — and a constraint does not survive into the rendered
    /// type, so `fn(s: U) -> U` prints the same whether `U` accepts anything or
    /// only numbers, which made the plain mismatch read as a call that should
    /// have fit.
    CallbackShape {
        mismatch: Box<FunctionMismatch<'db>>,
    },
    /// No candidate of an overloaded stub name accepted the arguments; carries
    /// the first candidate's failure for a concrete lead.
    NoMatchingOverload {
        name: String,
        candidates: usize,
        first: Option<Box<TypeError<'db>>>,
    },
    /// `[[` on a value that supports no element extraction.
    NotAList {
        found: Ty<'db>,
    },
    /// `[` on a value the slice rules do not cover.
    BadVectorIndex {
        index: Ty<'db>,
    },
    UnsupportedSubset {
        found: Ty<'db>,
    },
    /// Multi-index, empty, or named-index forms (`m[i, j]`) are not modeled.
    UnsupportedIndexShape {
        index_count: usize,
    },
    /// A literal `[[` position outside a fixed-shape container (1-based).
    PositionDoesNotExist {
        position: usize,
        container: Ty<'db>,
    },
    /// A literal field name absent from a fixed-shape container. The
    /// candidate set is small and closed, so a near-miss is a certain hint.
    FieldDoesNotExist {
        suggestion: Option<String>,
        field: String,
        container: Ty<'db>,
    },
    /// R rejects `$` on every atomic vector, named ones included.
    DollarOnAtomicVector {
        found: Ty<'db>,
    },
    /// An operator operand outside the operator's accepted family.
    InvalidOperand {
        expected: OperandExpectation,
        found: Ty<'db>,
    },
    /// An operator applied to a class that declares that operator, but to no
    /// combination of operand types the class accepts (`Date + Date`).
    UnsupportedOperandPair {
        operator: &'static str,
        left: Ty<'db>,
        right: Ty<'db>,
    },
    /// A `for` sequence that is neither a vector nor a list shape.
    NotIterable {
        found: Ty<'db>,
    },
    /// The occurs check refused a binding: the variable appears inside the
    /// type it would be bound to.
    InfiniteType {
        variable: Ty<'db>,
        container: Ty<'db>,
    },
}

/// What an operator position accepts, for error wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum OperandExpectation {
    Numeric,
    ScalarNumeric,
    Logical,
    Comparable,
}

/// The result of checking one item.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemCheck<'db> {
    /// Per-expression resolved types (post-substitution).
    pub expression_types: FxHashMap<ExprId, Ty<'db>>,
    pub errors: Vec<TypeError<'db>>,
    /// Places the checker genuinely could not determine a type — the strict
    /// check's input. Inference is untouched; these only become diagnostics
    /// under `[check] strict` or the per-file directive.
    pub strict_origins: Vec<StrictOrigin>,
    /// The generalized scheme of the item's top-level binding value, when the
    /// item is a definition.
    pub scheme: Option<TypeScheme<'db>>,
    /// The committed candidate of each call whose callee resolved through a
    /// stub overload set, keyed by the callee expression, as an index into
    /// the declared set. Absent when no candidate matched. Signature help
    /// lists the whole declared set with the committed candidate active.
    pub selected_overloads: FxHashMap<ExprId, usize>,
    /// Reads inside the index arguments of a bracket whose subject is a
    /// declared `data.table`: the bracket evaluates them inside the data's
    /// own frame, where a bare name is a column reference no lexical scope
    /// can see — the unresolved-name warning skips these expressions.
    pub masked_reads: rustc_hash::FxHashSet<ExprId>,
    /// The settled scheme of every name the item's TOP-LEVEL frame binds
    /// (variable-erased like the export). For a statement item this is how a
    /// conditional write (`for (i in 1:3) total <- i`) serves cross-item
    /// reads of the document slot it creates; a definition item's own name
    /// keeps resolving through `scheme`. A failed item exports `Unknown`
    /// here too.
    pub top_level_bindings: Vec<(String, TypeScheme<'db>)>,
}

/// One `Unknown` origin: where an undetermined type was first introduced
/// (propagation is never re-reported).
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct StrictOrigin {
    /// The originating expression (for assignment-value phrasing) and its
    /// item-relative range.
    pub expression: ExprId,
    pub range: TextRange,
    pub kind: StrictOriginKind,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum StrictOriginKind {
    /// A syntactically valid construct the checker does not model.
    UnsupportedConstruct,
    /// A reference that resolved to a binding with no known type.
    UndeterminedReference(String),
    /// A variable widened to `Unknown` at the loop fixed-point cap.
    LoopWidened(String),
    /// A recursive definition whose exported scheme still carries `Unknown`
    /// although its body checked clean: the reference cycle itself is the
    /// only possible source, so the binding is attributed as a whole.
    RecursiveUnknown(String),
}

/// Resolver for names that are not item-local: package globals and the stdlib
/// stub corpus.
///
/// **No method has a default body, deliberately.** Every implementor answers
/// every question, so adding a fact here is a compile error at each one rather
/// than a silent fallback. A defaulted `arithmetic_classes` was exactly that
/// silent fallback: the cyclic group's view inherited an empty set, so a
/// recursive function doing `Date + 1` failed the numeric constraint and
/// exported `Unknown` while its non-recursive twin typed fine.
pub trait GlobalEnv<'db> {
    /// The scheme a read of `name` sees. `deferred` marks a read from inside
    /// a nested function: the closure runs after its document frame settled,
    /// so in sequential documents the LAST binding of the name wins there,
    /// while an immediate read sees only bindings earlier than itself.
    fn scheme(&self, name: &str, deferred: bool) -> Option<TypeScheme<'db>>;

    /// The full ordered overload-candidate set of a name, `None` when the name
    /// has at most one candidate or a package/local definition wins over the
    /// stub set. `deferred` as in [`GlobalEnv::scheme`].
    fn overloads(&self, name: &str, deferred: bool) -> Option<Vec<TypeScheme<'db>>>;

    /// Whether the name is defined by an item of THIS project (a script
    /// statement, a package definition) rather than by a stub or an import.
    /// A project definition has its own attributable site, so a reference to
    /// it propagates an `Unknown` instead of re-originating one.
    fn defined_in_project(&self, name: &str, deferred: bool) -> bool;

    /// The `@type` / `@alias` definitions visible to the item being checked.
    ///
    /// **Borrowed, and both of these are per-FILE facts.** A check consults them
    /// once per item, so handing back an owned copy made every item in a project
    /// pay for the whole project's table: at 2,000 items, typecheck rose from
    /// 18 ms to 100 ms purely as the declaration count went 0 to 2,400. Neither
    /// is ever mutated by a caller — both are pure lookups.
    fn type_definitions(
        &self,
    ) -> &'db FxHashMap<Name<'db>, crate::annotations::NamedDefinition<'db>>;

    /// Classes reachable here that declare an arithmetic operator method,
    /// standard library and project sources alike — the same scope operator
    /// dispatch resolves a method through.
    fn arithmetic_classes(&self) -> &'db rustc_hash::FxHashSet<String>;
}

pub fn check_item<'db>(db: &'db dyn Db, module: &Module, naming: &ItemNaming) -> ItemCheck<'db> {
    check_item_with_annotation(db, module, naming, None, &[], None)
}

/// Full-check executions since process start — a plain instrument for perf
/// witnesses (fixpoint re-runs make executions exceed item counts).
pub static CHECK_EXECUTIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub fn check_item_with_annotation<'db>(
    db: &'db dyn Db,
    module: &Module,
    naming: &ItemNaming,
    annotation: Option<&crate::annotations::Annotation<'db>>,
    expression_annotations: &[(ExprId, crate::annotations::Annotation<'db>)],
    globals: Option<&dyn GlobalEnv<'db>>,
) -> ItemCheck<'db> {
    CHECK_EXECUTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        expression_annotations: expression_annotations.iter().cloned().collect(),
        environment: Environment::default(),
        scheme_arena: Vec::new(),
        recorded: FxHashMap::default(),
        selected_overloads: FxHashMap::default(),
        masked_reads: rustc_hash::FxHashSet::default(),
        errors: Vec::new(),
        strict_origins: Vec::new(),
        overload_probe_depth: 0,
        return_frames: Vec::new(),
        pending_enclosing_writes: Vec::new(),
        forward_captured: rustc_hash::FxHashSet::default(),
        capture_joins: FxHashMap::default(),
        capture_repass_needed: false,
        pre_materialized: FxHashMap::default(),
        no_default_formals: rustc_hash::FxHashSet::default(),
        non_local_slots: FxHashMap::default(),
        next_non_local_slot: naming
            .bindings
            .keys()
            .map(|slot| slot.0 + 1)
            .max()
            .unwrap_or(0),
    };
    // An annotation whose type mentions a cyclically expanding alias is
    // unusable: report once at the annotated statement (where legacy blames
    // it) and fall back to plain inference.
    let mut annotation = annotation;
    if let Some(root) = module.root
        && let Some(present) = annotation
    {
        let mentioned = present.declared.iter().map(|declared| declared.body).chain(
            present
                .new_nominal
                .iter()
                .flat_map(|(_, arguments, _)| arguments.iter().copied()),
        );
        let mut cycle = None;
        for ty in mentioned {
            cycle = find_alias_cycle(db, context.table.definitions, ty);
            if cycle.is_some() {
                break;
            }
        }
        if let Some(name) = cycle {
            context.errors.push(TypeError {
                range: module.expression(root).range,
                kind: TypeErrorKind::AliasCycle { name },
            });
            annotation = None;
        }
    }
    let mut scheme = None;
    // Whether `scheme` is the user's DECLARED contract rather than an inferred
    // one. A declaration stays trustworthy even when the body fails to honour
    // it — it is what the author said the function is — so it survives the
    // failure cut below and every call site keeps being checked.
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
            // A declared function annotation on a function definition: check
            // the body under the declared parameter types (rigid — they
            // refuse to bind), and check the result against the declared
            // return. The declared scheme wins as the export.
            (ExpressionKind::Assign { value, .. }, Some((declared, function)))
                if matches!(
                    module.expression(*value).kind,
                    ExpressionKind::Function { .. }
                ) =>
            {
                for (name, constraint) in &declared.binders {
                    context.table.rigid_constraints.insert(*name, *constraint);
                }
                // The exported scheme is the reconciled signature, not the
                // annotation as written: R matches arguments against the
                // definition's formals, so a shape the annotation got wrong is
                // reported once at the definition instead of turning every
                // correct call into a finding.
                let declared = TypeScheme {
                    binders: declared.binders,
                    body: context.check_declared_function(*value, &function),
                };
                context.recorded.insert(root, declared.body);
                scheme = Some(declared);
                scheme_is_declared = true;
            }
            (ExpressionKind::Assign { value, .. }, _) => {
                let root_ty = context.infer(root);
                let value_ty = context.recorded.get(value).copied().unwrap_or(root_ty);
                scheme = if let Some((new_name, new_arguments, _)) =
                    annotation.and_then(|a| a.new_nominal.clone())
                {
                    Some(context.check_new_nominal(new_name, &new_arguments, *value, value_ty))
                } else {
                    match annotation.and_then(|a| a.declared.clone()) {
                        // A declared non-function type (or a trusted one):
                        // the declaration is the contract, and the value must
                        // satisfy it under the directional compatibility
                        // relation — the value flows into the declared type,
                        // exactly like an argument into a parameter, so
                        // member-into-union, scalar-into-vector, and
                        // nominal-representation projection all apply. A
                        // declared NOMINAL is deliberately still strict the
                        // other way: `#: Point` on a structural value errors —
                        // `@new` is the only nominal introduction. Unknown and
                        // Any declarations are tolerance floors with nothing
                        // to check.
                        // An unknown-only coercion applies exactly where the
                        // checker has nothing and reports where it has
                        // something: see the expression-level path for why the
                        // refusal is the point.
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
                        Some(declared) => {
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
                // A checked, trusted, or `@new` annotation on a bare
                // expression statement applies through the same
                // expression-level path as nested statements; the item still
                // exports nothing.
                if let Some(present) = annotation {
                    context.expression_annotations.insert(root, present.clone());
                }
                context.infer(root);
            }
        }
    }
    let expression_types = context
        .recorded
        .iter()
        .map(|(&id, &ty)| (id, context.table.resolve(context.db, ty)))
        .collect();
    // A failed item exports `Unknown`: a type error means the inferred shape
    // is not trustworthy, so downstream items must not check against it (they
    // would cascade). Inference still runs to completion internally —
    // expression types stay available for IDE surfaces — only the export is
    // cut. Every error *inside* the item is still reported: a failing
    // expression records `Unknown`, which is compatible with everything, so
    // later checks read a poisoned value as an absent fact rather than
    // cascading off it.
    let mut errors = context.errors;
    let scheme = if errors.is_empty() || scheme_is_declared {
        // Inference variables are table-scoped: a scheme crossing the item
        // boundary must never carry one (a foreign table cannot resolve it).
        // At the export edge a residual variable CARRYING A CONSTRAINT
        // generalizes into a binder — `mixed_apply <- invoke(mirror)` keeps
        // its `<T: numeric>` so cross-item calls still check — while an
        // unconstrained one (no information) erases to `Unknown`.
        scheme.map(|scheme| close_scheme(db, &mut context.table, scheme))
    } else {
        scheme.map(|_| TypeScheme::monomorphic(unknown(db)))
    };
    // Speculative paths (overload probing, guard edges) can record the same
    // failure twice; the report is one finding per site and kind.
    errors.sort_by_key(|error| (error.range.start(), error.range.end()));
    errors.dedup();
    let mut top_level_bindings = Vec::new();
    for info in naming.bindings.values() {
        if info.kind != crate::naming::BindingKind::TopLevel {
            continue;
        }
        let Some(entry) = context.environment.get(info.id) else {
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
        top_level_bindings.push((info.name.clone(), binding_scheme));
    }
    ItemCheck {
        expression_types,
        errors,
        strict_origins: context.strict_origins,
        scheme,
        selected_overloads: context.selected_overloads,
        masked_reads: context.masked_reads,
        top_level_bindings,
    }
}

/// Closes a scheme at the export edge: bound variables substitute, unbound
/// CONSTRAINED variables generalize into fresh binders (the constraint is
/// real information a reader must honor), and unbound unconstrained ones
/// erase to `Unknown` via [`erase_residual_vars`]. The synthetic binder
/// names never display — the renderer canonicalizes rigid names to
/// `T`/`U`/`V` by first occurrence.
///
/// This is the only edge a scheme crosses on its way out of the item, so it is
/// where the table-scoped-variable invariant is asserted: an id that reaches a
/// reader's table either dangles (an index past the end) or names an unrelated
/// variable of the reader's own, and the second failure is silent.
fn close_scheme<'db>(
    db: &'db dyn Db,
    table: &mut InferenceTable<'db>,
    scheme: TypeScheme<'db>,
) -> TypeScheme<'db> {
    let mut closer = ResidualCloser {
        binders: scheme.binders,
        generalized: FxHashMap::default(),
    };
    let body = erase_residual_vars_at(db, table, scheme.body, 0, Some(&mut closer));
    debug_assert!(
        !crate::types::contains_inference_var(db, body),
        "a closed scheme must carry no inference variable"
    );
    TypeScheme {
        binders: closer.binders,
        body,
    }
}

/// Accumulates export-edge generalization state for [`close_scheme`].
struct ResidualCloser<'db> {
    binders: Vec<(Name<'db>, Constraint)>,
    generalized: FxHashMap<crate::types::InferenceVar, Ty<'db>>,
}

/// Substitutes every bound inference variable and replaces every still-unbound
/// one with `Unknown` (or, with a closer, generalizes constrained ones — see
/// [`close_scheme`]). Follows variable bindings only — named types stay
/// unexpanded (an exported `UserId` must display as `UserId`, not its alias
/// body). The depth cap guards against variable-linked structures nesting
/// past reason; a closed scheme is required, so past it the type erases.
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
        return crate::types::map_child_types(db, resolved, &mut |child| {
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

/// The slot environment: an undo-logged map from binding slots to types.
#[derive(Debug, Default)]
struct Environment<'db> {
    entries: FxHashMap<BindingId, EnvEntry<'db>>,
    undo: Vec<(BindingId, Option<EnvEntry<'db>>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvEntry<'db> {
    /// A monotype slot value.
    Mono(Ty<'db>),
    /// A generalized (let-bound function) scheme, stored by index to keep the
    /// entry `Copy`; schemes live in the checker's arena.
    Scheme(u32),
    /// A no-default formal on the branch where `missing(name)` held: reading
    /// it would fail at run time ("argument is missing, with no default"), so
    /// a read errors. Carries the supplied-state type; any write returns the
    /// slot to an ordinary entry.
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

    /// The entries written since `mark`, deduplicated to their latest value.
    fn writes_since(&self, mark: usize) -> Vec<(BindingId, Option<EnvEntry<'db>>)> {
        let mut seen = std::collections::BTreeSet::new();
        let mut writes = Vec::new();
        for (slot, _) in self.undo[mark..].iter() {
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
    /// Statement-level annotations below the item root, by annotated
    /// expression (plus the item annotation itself for a non-assignment
    /// root): applied where the expression infers.
    expression_annotations: FxHashMap<ExprId, crate::annotations::Annotation<'db>>,
    environment: Environment<'db>,
    scheme_arena: Vec<TypeScheme<'db>>,
    recorded: FxHashMap<ExprId, Ty<'db>>,
    selected_overloads: FxHashMap<ExprId, usize>,
    masked_reads: rustc_hash::FxHashSet<ExprId>,
    errors: Vec<TypeError<'db>>,
    strict_origins: Vec<StrictOrigin>,
    /// Non-zero while a strict overload-selection round probes a candidate:
    /// the literal-as-integer courtesy is off, so it cannot decide which
    /// candidate wins (exact matches outrank conversions).
    overload_probe_depth: u32,
    /// Early-`return` value types per enclosing function frame; a function's
    /// return type is their union with the body's trailing value.
    /// Per enclosing function: each early `return` value with the range to
    /// blame for it, so a declared-return mismatch reports at the `return`
    /// that is wrong rather than at whichever expression happens to be last.
    return_frames: Vec<Vec<(Ty<'db>, TextRange)>>,
    /// Super-assignment (`<<-`) writes to enclosing slots: re-applied after
    /// each enclosing body's environment rollback so the join survives at the
    /// definition site.
    pending_enclosing_writes: Vec<(BindingId, Ty<'db>)>,
    /// Slots a closure body read before any write reached them (the letrec /
    /// forward-capture shape: `helper <- function() other(); other <- ...`).
    forward_captured: rustc_hash::FxHashSet<BindingId>,
    /// The running join of every write to a captured slot, variable-erased so
    /// it survives rollbacks. Forward-capture reads resolve here — sound for
    /// call-later semantics — instead of the empty definition-point entry.
    capture_joins: FxHashMap<BindingId, Ty<'db>>,
    /// The current body wrote a forward-captured slot: its closures were
    /// inferred against an incomplete join, so the body re-checks once.
    capture_repass_needed: bool,
    /// Top-level slots whose unwritten path resolved to the name's
    /// cross-item binding: the observed type is the slot's PRE-state, not a
    /// body write, so loop passes re-establish it after their rollback (a
    /// loop's first iteration keeps reading the earlier binding).
    pre_materialized: FxHashMap<BindingId, Ty<'db>>,
    /// Formal-parameter slots with no default: a `missing(name)` guard on
    /// one marks its true edge read-erroring.
    no_default_formals: rustc_hash::FxHashSet<BindingId>,
    /// Slots standing for names this item reads but does not bind — a
    /// top-level variable another statement assigned, which naming records as
    /// a non-local because scopes are per-item. A guard needs somewhere to put
    /// its refinement, and flow state for a name belongs in the environment
    /// like any other slot's, so the name gets a slot here rather than a
    /// second place to look. Identity only: the type itself lives in the
    /// environment, and these ids are minted above every id naming issued so
    /// they cannot collide with a real binding.
    non_local_slots: FxHashMap<String, BindingId>,
    next_non_local_slot: u32,
}

/// One call argument, inferred exactly once before any signature matching, so
/// an overload probe can re-match without re-running expression inference.
struct CallArgument<'db> {
    name: Option<String>,
    /// The name token's own range, so a finding about the NAME points at the
    /// name rather than at the value attached to it.
    name_range: Option<TextRange>,
    /// `None` is a positional hole (`f(, x)`).
    ty: Option<Ty<'db>>,
    range: TextRange,
    /// The argument's value expression. A type carries no source ranges, so a
    /// finding about one field of a record has to re-walk the expression that
    /// produced the record to find that field's own range.
    value: Option<ExprId>,
    /// The argument is a whole-number double literal (`1`, `2.0`) — eligible
    /// for the literal-as-integer courtesy.
    whole_double: bool,
    /// The argument is the enclosing function's bare `...`: it forwards an
    /// unknown number of arguments (possibly zero), so the call cannot be
    /// arity-checked and the argument matches no parameter itself.
    forwards_dots: bool,
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
    /// `logical`, `integer` and `double`: R promotes a logical operand to
    /// `integer` before comparing, exactly as it does for arithmetic, so all
    /// three compare freely (`flags > 0`, `flag == TRUE`).
    Numeric,
    Character,
}

/// How an operand of an arithmetic operator classifies: a concrete numeric
/// shape, a union whose members all are (accepted member-wise), a
/// still-flexible variable or declared-numeric rigid, a vector whose element
/// is not yet concrete, an `Any`/`Unknown` short-circuit, or a hard error.
enum NumericOperand<'db> {
    Concrete(OperandShape, Atomic),
    ConcreteUnion(Vec<(OperandShape, Atomic)>),
    Flexible(Ty<'db>),
    /// A vector whose element is a generic variable/rigid (carried, to
    /// constrain) or statically untracked (`Any`/`Unknown`, carrying `None`).
    /// The shape is known — vector — even though the atomic is not.
    FlexibleVector(Option<Ty<'db>>),
    AnyUnknown,
    Invalid,
}

impl NumericOperand<'_> {
    /// The operand's member shapes: one for a concrete operand, all of them
    /// for a union.
    fn concrete_parts(&self) -> Option<Vec<(OperandShape, Atomic)>> {
        match self {
            NumericOperand::Concrete(shape, atomic) => Some(vec![(*shape, *atomic)]),
            NumericOperand::ConcreteUnion(parts) => Some(parts.clone()),
            _ => None,
        }
    }
}

/// One recognized type-guard condition: the tested slot and the refined type
/// on each `if` edge (`None` = no refinement on that edge).
struct GuardRefinement<'db> {
    /// The true edge marks the slot as a read-erroring missing formal
    /// (`missing(name)` on a no-default parameter) instead of refining its
    /// type.
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

/// A non-union type narrows as its own one-member union.
fn guard_members<'db>(db: &'db dyn Db, resolved: Ty<'db>) -> Vec<Ty<'db>> {
    match resolved.kind(db) {
        TyKind::Union(members) => members.clone(),
        _ => vec![resolved],
    }
}

/// Whether a member belongs to a guard family: `Some(bool)` when statically
/// decidable, `None` (kept on both edges) otherwise. A family membership test
/// covers the scalar and the vector of the atomic; `is.list` covers every
/// list shape; `is.function` covers function types.
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

/// Where R's argument matcher sends one supplied argument.
enum ArgumentTarget {
    /// A fixed positional parameter, by index into `positional`.
    Positional(usize),
    /// A named formal, by index into `named` — whether it was claimed by name
    /// or filled positionally.
    Named(usize),
    /// Absorbed by the rest parameter.
    Rest,
    /// A name no parameter declares on a non-variadic function, or one that
    /// duplicates a formal already supplied.
    UnmatchedName,
    /// A positional argument with nowhere left to go.
    Overflow,
}

/// A comparison operand whose family is not yet known: a bare variable or
/// rigid, or a vector whose element is still generic or untracked.
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

/// An arithmetic operand's shape and the atomic it computes in. R promotes a
/// logical operand to `integer` before arithmetic (`TRUE + TRUE` is `2L`), so
/// a logical operand reports `integer` and the result rules below need no
/// logical case of their own.
/// An operator's R spelling, used to build its S3 method name (`+.Date`).
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

/// The operators R groups as `Arith`. **One list, two consumers**: this decides
/// what [`operator_group`] calls arithmetic, and the method-name prefixes the
/// numeric constraint admits are built from it by
/// [`arithmetic_method_prefixes`]. Restating the set instead let `^`, `%%` and
/// `%/%` be dispatched here while the constraint refused the very class that
/// declared them.
const ARITHMETIC_OPERATORS: [BinaryOperator; 7] = [
    BinaryOperator::Add,
    BinaryOperator::Subtract,
    BinaryOperator::Multiply,
    BinaryOperator::Divide,
    BinaryOperator::Power,
    BinaryOperator::Modulo,
    BinaryOperator::IntegerDivide,
];

/// The method names that make a class arithmetic, in the order
/// [`Checker::operator_method_result`] tries them: each operator's own method,
/// then the group generic, then `Ops`.
pub fn arithmetic_method_prefixes() -> impl Iterator<Item = String> {
    ARITHMETIC_OPERATORS
        .iter()
        .filter_map(|&operator| operator_spelling(operator))
        .map(|spelling| format!("{spelling}."))
        .chain(["Arith.".to_owned(), "Ops.".to_owned()])
}

/// R's S3 operator group for an operator: a class may declare one method for
/// the whole group instead of one per operator.
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
        Atomic::Logical => Some(Atomic::Integer),
        Atomic::Integer | Atomic::Double => Some(*atomic),
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

/// A comparison operand's member shapes: one for a concrete operand, all of
/// them for a union (member-wise acceptance); `None` when any member is not
/// comparable.
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
        // A self-referential value builds a type that grows by a factor of its
        // field count per level, and every consumer downstream pays the tree it
        // denotes rather than the graph it shares, so one such expression can
        // stall the whole check. Past the ceiling the expression takes
        // `Unknown` — sound by refusal, and the same move a loop makes for a
        // variable whose type keeps growing structurally. Only composites are
        // measured; a scalar cannot be oversized and must not pay for the ask.
        let ty = if matches!(
            ty.kind(self.db),
            TyKind::Record(_) | TyKind::Function(_) | TyKind::Union(_) | TyKind::Tuple(_)
        ) && crate::types::type_size(self.db, ty) >= crate::types::TYPE_SIZE_CEILING
        {
            self.unknown()
        } else {
            ty
        };
        self.recorded.insert(id, ty);
        ty
    }

    fn unknown(&self) -> Ty<'db> {
        crate::types::unknown(self.db)
    }

    /// Record where an undetermined type was introduced (idempotent: loop
    /// re-walks and re-passes must not duplicate an origin).
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

    /// A whole-type mismatch, narrowed to the one field when both sides are
    /// records — in the message *and* in the range. Every site that reports two
    /// types side by side goes through here, so the narrowing cannot be present
    /// at one and missing at another.
    ///
    /// `value` is the expression the `found` type came from, when the caller
    /// has it. A type carries no source ranges, so pointing at the field means
    /// re-walking that expression alongside the field path.
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

    /// Where the field a record mismatch names was written. A record type is
    /// produced by a `list(...)` call and its fields are that call's tagged
    /// arguments, so the path is walked against the expression that produced
    /// the type.
    ///
    /// `None` whenever the expression is not that shape — a variable holding a
    /// record has no field to point at — and the whole value stays the blame.
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
            // Nothing at the path to point at, so the innermost list that
            // should have carried the field is the finding.
            RecordMismatch::Missing { .. } => Some(self.blame_range(current)),
            RecordMismatch::Field { .. } => {
                Some(self.blame_range(self.tagged_argument(current, last)?.value?))
            }
            // This message is about the NAME being one the type does not
            // declare, so the name is what it underlines.
            RecordMismatch::Unexpected { .. } => {
                let argument = self.tagged_argument(current, last)?;
                argument
                    .name_range
                    .or_else(|| argument.value.map(|value| self.blame_range(value)))
            }
        }
    }

    /// The tagged argument of a call with this name — a record's field as the
    /// `list(...)` that built it wrote it.
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
        let expression = self.module.expression(id).clone();
        let range = expression.range;
        let ty = match &expression.kind {
            ExpressionKind::Missing => self.unknown(),
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
                    )
                });
                let value_ty = match declares_function {
                    // A declared function annotation on a function definition
                    // checks the body under the declared parameter types,
                    // exactly as at the item root.
                    Some(declared) => {
                        for (name, constraint) in &declared.binders {
                            self.table.rigid_constraints.insert(*name, *constraint);
                        }
                        let body = match declared.body.kind(self.db).clone() {
                            TyKind::Function(function) => {
                                self.check_declared_function(value, &function)
                            }
                            _ => declared.body,
                        };
                        self.record(value, body)
                    }
                    None => {
                        let value_ty = self.infer(value);
                        // A statement-level annotation on the assignment applies
                        // before the write so the binding takes the annotated type.
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
                let special_name = special_name.clone();
                self.infer_binary(id, range, operator, special_name.as_deref(), lhs, rhs)
            }
            ExpressionKind::Call { callee, arguments } => {
                let arguments = arguments.clone();
                self.infer_call_expression(id, range, *callee, &arguments)
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
                let name = name.clone();
                let name_range = name_range.unwrap_or(range);
                self.infer_field(id, range, name_range, at, target, name)
            }
            // `pkg::name` resolves only through a namespace the stub corpus
            // knows (and, for `::`, actually exports the name); the scheme
            // itself still comes from the global environment, so a project
            // override winning a stub name's type keeps working. `:::`
            // reaches unexported names, so it skips the export check.
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => {
                let validated = validated_namespace_name(self.db, *internal, package, name);
                let name = name.clone();
                match validated
                    .as_ref()
                    .and_then(|name| self.globals.and_then(|globals| globals.scheme(name, false)))
                {
                    Some(namespace_scheme) => self.instantiate(&namespace_scheme),
                    None => {
                        // An unvalidated qualified read: Unknown, and a
                        // strict origin.
                        if let Some(name) = name {
                            self.record_strict_origin(
                                id,
                                StrictOriginKind::UndeterminedReference(name),
                            );
                        }
                        self.unknown()
                    }
                }
            }
            // R parameters are always matchable by name and by position, so
            // inferred function types carry every formal as a named parameter
            // (optional when it defaults); a `...` formal becomes a rest
            // parameter with element `Any` at its formal position. Defaults
            // are inferred but do not pin an unannotated parameter's type —
            // that comes from the parameter's uses.
            ExpressionKind::Function { parameters, body } => {
                // A statement-level `#:` declaring a function type checks this
                // definition the way the item root does: parameter types push
                // INTO the body (rigid, so they refuse to bind) and the result
                // checks against the declared return. Inferring the body freely
                // and comparing afterwards would report a shape mismatch —
                // `expected fn(x: character) -> integer, found fn(x: T) -> T` —
                // while the real error inside the body went unreported, which
                // leaves every closure-factory body unchecked.
                if let Some(declared) = self.declared_function_annotation(id) {
                    for (name, constraint) in &declared.binders {
                        self.table.rigid_constraints.insert(*name, *constraint);
                    }
                    let TyKind::Function(function) = declared.body.kind(self.db).clone() else {
                        return self.unknown();
                    };
                    let signature = self.check_declared_function(id, &function);
                    return self.record(id, signature);
                }
                let parameters = parameters.clone();
                self.table.level += 1;
                let pending_mark = self.pending_enclosing_writes.len();
                let mark = self.environment.mark();
                // A formal the body tests with `missing(name)` is optional at
                // call sites — R's optional-without-default idiom.
                let mut missing_tested = rustc_hash::FxHashSet::default();
                self.collect_missing_tested(*body, &mut missing_tested);
                let mut named = Vec::new();
                let mut variadic = None;
                for parameter in &parameters {
                    if parameter.name == "..." {
                        variadic = Some(crate::types::RestParameter {
                            element: crate::types::any(self.db),
                            preceding_named: named.len(),
                        });
                        continue;
                    }
                    let parameter_ty = self.fresh(Constraint::Unconstrained);
                    let entry = named.len();
                    named.push(crate::types::Parameter {
                        name: Name::new(self.db, parameter.name.clone()),
                        ty: parameter_ty,
                        optional: parameter.default.is_some()
                            || missing_tested.contains(&parameter.name),
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
                    // The default is inferred here rather than above because
                    // it may read this parameter's own slot and the ones
                    // before it, which are in the environment only now.
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
                let return_ty = self.join_early_returns(&early_returns, trailing_ty);
                self.environment.rollback(mark);
                self.reapply_enclosing_writes(pending_mark);
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
            // The condition re-evaluates before every iteration, so its reads
            // also see the loop's joined state — it checks inside the fixed
            // point.
            ExpressionKind::While { condition, body } => {
                let (condition, body) = (*condition, *body);
                self.check_loop_body(id, Some(condition), body, false, None);
                crate::types::null(self.db)
            }
            // `repeat` runs at least once, so the body's exit state applies
            // after the loop (back edges still join inside it).
            ExpressionKind::Repeat { body } => {
                let body = *body;
                self.check_loop_body(id, None, body, true, None);
                crate::types::null(self.db)
            }
            // `local(expr)` evaluates its body in a fresh environment: the
            // bindings vanish afterwards (only super-assignment joins
            // survive), and the value is the body's value.
            ExpressionKind::Local { body } => {
                let body = *body;
                let pending_mark = self.pending_enclosing_writes.len();
                let mark = self.environment.mark();
                let value = self.infer(body);
                self.environment.rollback(mark);
                self.reapply_enclosing_writes(pending_mark);
                value
            }
            ExpressionKind::Block {
                statements,
                trailing_semicolon,
            } => {
                let statements = statements.clone();
                let trailing_semicolon = *trailing_semicolon;
                let mut last = crate::types::null(self.db);
                for statement in statements {
                    last = self.infer(statement);
                }
                // A `;`-terminated final expression discards the value.
                if trailing_semicolon {
                    crate::types::null(self.db)
                } else {
                    last
                }
            }
            ExpressionKind::Paren(inner) => self.infer(*inner),
            ExpressionKind::Break | ExpressionKind::Next => self.unknown(),
        };
        // Assignments applied their annotation before the slot write above.
        let ty = if matches!(expression.kind, ExpressionKind::Assign { .. }) {
            ty
        } else {
            self.apply_expression_annotation(id, id, ty)
        };
        self.record(id, ty)
    }

    /// The function type a statement-level annotation declares for this
    /// expression, when it declares one to check against. `@trust` and
    /// `@if-unknown` are deliberately excluded: neither checks the value, and
    /// `@new` mints a nominal instead of declaring a signature.
    fn declared_function_annotation(&self, annotated: ExprId) -> Option<TypeScheme<'db>> {
        let annotation = self.expression_annotations.get(&annotated)?;
        if annotation.trusted || annotation.if_unknown || annotation.new_nominal.is_some() {
            return None;
        }
        let declared = annotation.declared.clone()?;
        matches!(declared.body.kind(self.db), TyKind::Function(_)).then_some(declared)
    }

    /// Applies a statement-level annotation to the annotated expression's
    /// value: `@new` checks the representation and mints the nominal,
    /// `@trust` overrides unchecked, and a checked declared type enforces
    /// directional compatibility — the same contract as at the item root,
    /// minus the export.
    fn apply_expression_annotation(
        &mut self,
        annotated: ExprId,
        value: ExprId,
        value_ty: Ty<'db>,
    ) -> Ty<'db> {
        let Some(annotation) = self.expression_annotations.get(&annotated).cloned() else {
            return value_ty;
        };
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
        // An unknown-only coercion fills an inference gap without overriding
        // knowledge: it applies exactly where the checker has nothing, and
        // says so when it has something. That refusal is the whole reason to
        // reach for it over `@trust` — an annotation that silently stayed in
        // place as the inferred type changed underneath it would be
        // indistinguishable from a stale one.
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

    fn literal_ty(&mut self, literal: &LiteralKind) -> Ty<'db> {
        match literal {
            LiteralKind::Integer(_) => scalar(self.db, Atomic::Integer),
            LiteralKind::Double(_) => scalar(self.db, Atomic::Double),
            LiteralKind::Complex => scalar(self.db, Atomic::Complex),
            LiteralKind::String(_) => scalar(self.db, Atomic::Character),
            LiteralKind::Logical(_) => scalar(self.db, Atomic::Logical),
            LiteralKind::Null => crate::types::null(self.db),
            LiteralKind::Na(atom) => scalar(
                self.db,
                match atom {
                    crate::hir::NaAtom::Logical => Atomic::Logical,
                    crate::hir::NaAtom::Integer => Atomic::Integer,
                    crate::hir::NaAtom::Double => Atomic::Double,
                    crate::hir::NaAtom::Complex => Atomic::Complex,
                    crate::hir::NaAtom::Character => Atomic::Character,
                },
            ),
            LiteralKind::Inf | LiteralKind::NaN => scalar(self.db, Atomic::Double),
        }
    }

    fn infer_read(&mut self, id: ExprId) -> Ty<'db> {
        let Some(&slot) = self.naming.resolutions.get(&id) else {
            // A non-local read resolves through the package interface and the
            // stub corpus. Unresolved reads stay silent Unknown (naming owns
            // the unresolved diagnostic); a read that RESOLVES to a binding
            // with no known type is a strict origin.
            // A guard narrowed this name earlier in the same expression, so the
            // refinement is the observed type here — reading through to the
            // binding again would discard it.
            if let Some(name) = self.naming.non_locals.get(&id)
                && let Some(&slot) = self.non_local_slots.get(name)
                && let Some(EnvEntry::Mono(refined)) = self.environment.get(slot)
            {
                return refined;
            }
            if let Some(name) = self.naming.non_locals.get(&id).cloned()
                && let Some(scheme) = self.globals.and_then(|globals| {
                    globals.scheme(&name, self.naming.deferred_non_locals.contains(&id))
                })
            {
                let instantiated = self.instantiate(&scheme);
                // Origins, not propagation: a name this project defines owns
                // its `Unknown` at its own defining site, so reading it here
                // propagates. Re-originating turned one untyped binding into
                // one diagnostic per line that touched it.
                let deferred = self.naming.deferred_non_locals.contains(&id);
                if matches!(
                    self.table.resolve(self.db, instantiated).kind(self.db),
                    TyKind::Unknown
                ) && !self
                    .globals
                    .is_some_and(|globals| globals.defined_in_project(&name, deferred))
                {
                    self.record_strict_origin(id, StrictOriginKind::UndeterminedReference(name));
                }
                return instantiated;
            }
            return self.unknown();
        };
        match self.environment.get(slot) {
            Some(EnvEntry::Mono(ty)) => ty,
            Some(EnvEntry::Scheme(index)) => {
                let scheme = self.schemes()[index as usize].clone();
                self.instantiate(&scheme)
            }
            Some(EnvEntry::MissingFormal(_)) => {
                let name = self
                    .naming
                    .bindings
                    .get(&slot)
                    .map(|info| info.name.clone())
                    .unwrap_or_default();
                let range = self.module.expression(id).range;
                self.errors.push(TypeError {
                    range,
                    kind: TypeErrorKind::MissingFormalRead { name },
                });
                self.unknown()
            }
            // A read before any write reached the slot. A captured slot
            // resolves to the running join of the frame's writes — the
            // closure runs later, when they have happened (the letrec shape);
            // the enclosing body re-checks once when the join completes
            // after this read. A top-level slot's unwritten path reaches the
            // enclosing frame instead: the read observes the name's
            // cross-item binding (a loop's first iteration, a rebinding
            // statement's right-hand side), same as the unused check's
            // cross-item-read rule. The observed type is materialized as the
            // slot's entry so a loop join keeps it as the pre-loop state.
            // Everything else tolerates as Unknown.
            None => {
                if self.naming.captured_slots.contains(&slot) {
                    self.forward_captured.insert(slot);
                    if let Some(&join) = self.capture_joins.get(&slot) {
                        return join;
                    }
                } else if let Some(binding) = self.naming.bindings.get(&slot)
                    && binding.kind == crate::naming::BindingKind::TopLevel
                {
                    let name = binding.name.clone();
                    if let Some(scheme) = self
                        .globals
                        .and_then(|globals| globals.scheme(&name, false))
                    {
                        let instantiated = self.instantiate(&scheme);
                        // An Unknown cross-item binding (or a self-cycle's
                        // recovery value) adds nothing over the tolerant
                        // read, and materializing it would absorb the real
                        // body writes at the loop join.
                        if !matches!(
                            self.table.resolve(self.db, instantiated).kind(self.db),
                            TyKind::Unknown
                        ) {
                            self.environment.set(slot, EnvEntry::Mono(instantiated));
                            self.pre_materialized.insert(slot, instantiated);
                            return instantiated;
                        }
                    }
                }
                self.unknown()
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
        let target_expression = self.module.expression(target).clone();
        if let ExpressionKind::NameRef(_) = target_expression.kind {
            if let Some(&slot) = self.naming.resolutions.get(&target) {
                if spelling == AssignSpelling::Super {
                    // `<<-` mutates an *enclosing* slot: the write joins into
                    // the entry as a monotype and is recorded so it survives
                    // the enclosing body's environment rollback.
                    let written = self.table.resolve(self.db, value_ty);
                    self.join_enclosing_write(slot, written);
                    self.pending_enclosing_writes.push((slot, written));
                    self.note_captured_write(slot, value_ty);
                } else {
                    // Function values generalize at the binding
                    // (let-polymorphism); everything else stays a monotype
                    // slot write.
                    let resolved = self.table.shallow_resolve(self.db, value_ty);
                    if matches!(resolved.kind(self.db), TyKind::Function(_)) {
                        let scheme = self.generalize(value_ty);
                        let index = self.push_scheme(scheme);
                        self.environment.set(slot, EnvEntry::Scheme(index));
                    } else {
                        self.environment.set(slot, EnvEntry::Mono(value_ty));
                    }
                }
                self.note_captured_write(slot, value_ty);
            }
            self.recorded.insert(target, value_ty);
        } else {
            self.write_replacement_target(target, value, value_ty);
        }
    }

    /// Maintain the call-later join for a captured slot's writes (erased, so
    /// the value survives rollbacks) and trigger the enclosing body's
    /// capture re-pass when the write completes a join some closure already
    /// read.
    fn note_captured_write(&mut self, slot: BindingId, value_ty: Ty<'db>) {
        if !self.naming.captured_slots.contains(&slot) {
            return;
        }
        let written = crate::types::erase_vars(self.db, self.table.resolve(self.db, value_ty));
        let joined = match self.capture_joins.get(&slot) {
            Some(&existing) if existing != written => self.join_types(existing, written),
            Some(&existing) => existing,
            None => written,
        };
        // A value that refers to itself grows this join by a factor of its own
        // field count every pass instead of settling — measured climbing 877,
        // 8823, 104655, 1046623 on a record whose fields all return that
        // record. Nothing downstream can afford a type that size, since every
        // walk over it pays the tree rather than the shared graph, so past the
        // ceiling the join widens to `Unknown`: the same sound-by-refusal a
        // loop makes for a variable whose type keeps growing structurally.
        let changed = self.capture_joins.get(&slot) != Some(&joined);
        self.capture_joins.insert(slot, joined);
        if changed && self.forward_captured.contains(&slot) {
            self.capture_repass_needed = true;
        }
    }

    /// Join a super-assignment's written type into an environment entry as a
    /// monotype (an absent entry takes the written type directly; a
    /// scheme-holding entry contributes its instantiated body).
    fn join_enclosing_write(&mut self, slot: BindingId, written: Ty<'db>) {
        let entry = match self.environment.get(slot) {
            Some(EnvEntry::Mono(existing)) if existing != written => {
                EnvEntry::Mono(self.join_types(existing, written))
            }
            Some(entry @ EnvEntry::Mono(_)) => entry,
            Some(EnvEntry::Scheme(index)) => {
                let scheme = self.schemes()[index as usize].clone();
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

    /// Super-assignments in a body mutate *enclosing* slots, so their joins
    /// survive the body's environment rollback: re-apply them at the
    /// definition site. They stay recorded so each further enclosing scope
    /// re-applies them too (the join is idempotent).
    fn reapply_enclosing_writes(&mut self, mark: usize) {
        let writes: Vec<(BindingId, Ty<'db>)> = self.pending_enclosing_writes[mark..].to_vec();
        for (slot, written) in writes {
            self.join_enclosing_write(slot, written);
        }
    }

    /// A replacement-form assignment (`x$field <- v`, `x[["name"]] <- v`,
    /// `x[[key]] <- v`, `names(x) <- v`) reads the base variable, applies the
    /// write, and writes the result back to the base's slot.
    fn write_replacement_target(&mut self, target: ExprId, value: ExprId, value_ty: Ty<'db>) {
        let base = self.replacement_base(target);
        self.infer_replacement_spine(target, base);
        let Some(base) = base else {
            // The accessor spine has no variable at its root (`f(x)$a <- v`);
            // R rejects this shape at run time, so refuse rather than guess.
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

    /// Reports a field write that a nominal's representation does not admit:
    /// a value the declared field type refuses, or a field the representation
    /// does not carry. An opaque nominal (no representation) and a
    /// non-record representation admit nothing statically, so both stay quiet.
    fn check_nominal_field_write(
        &mut self,
        nominal: Ty<'db>,
        field_name: &str,
        value_expression: ExprId,
        value: Ty<'db>,
    ) {
        let representation = self.structural(nominal);
        let TyKind::Record(fields) = representation.kind(self.db).clone() else {
            return;
        };
        let range = self.blame_range(value_expression);
        match fields
            .iter()
            .find(|field| field.name.text(self.db) == field_name)
        {
            // An `Unknown` value is an absent fact, not a wrong one — the same
            // tolerance every other check applies.
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
                    suggestion: crate::diagnostics::nearest_field_name(
                        field_name,
                        fields.iter().map(|field| field.name.text(self.db)),
                    ),
                    field: field_name.to_owned(),
                    container: nominal,
                },
            }),
        }
    }

    /// The root variable of a replacement target's accessor spine: through
    /// index and field accessors and through a replacement call's first
    /// argument (`names(x) <- v` calls `names<-` on `x`).
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

    /// The pieces of a replacement target that are ordinary reads: index and
    /// surplus-argument expressions, and — when the spine has no variable
    /// root — the base position itself. The callee of a replacement call is
    /// skipped (`names(x) <- v` calls `names<-`, not `names`), and the base
    /// variable is skipped (its read supplies the prior type separately).
    fn infer_replacement_spine(&mut self, id: ExprId, base: Option<ExprId>) {
        if Some(id) == base {
            return;
        }
        match self.module.expression(id).kind.clone() {
            ExpressionKind::Index {
                target, arguments, ..
            } => {
                self.infer_replacement_spine(target, base);
                for argument in &arguments {
                    if let Some(value) = argument.value {
                        self.infer(value);
                    }
                }
            }
            ExpressionKind::Field { target, .. } => {
                self.infer_replacement_spine(target, base);
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

    /// The base slot's type after a replacement write: a known-field write
    /// (`$field` / `[["literal"]]`) on a record-like base sets that field
    /// (adding it if absent; an empty `list()` starts a record); a
    /// computed-key `[[<-` refines the reachable list shapes' element type;
    /// everything else (a `[<-`, an `@slot<-`, a multi-index form, or a
    /// replacement-function call) keeps the prior type.
    fn replacement_written_type(
        &mut self,
        lhs: ExprId,
        base: ExprId,
        value_expression: ExprId,
        prior: Ty<'db>,
        value: Ty<'db>,
    ) -> Ty<'db> {
        let kind = self.module.expression(lhs).kind.clone();
        let field_name = match &kind {
            ExpressionKind::Field {
                at: false,
                target,
                name,
                ..
            } if *target == base => name.clone(),
            ExpressionKind::Index {
                double: true,
                target,
                arguments,
            } if *target == base => match arguments.as_slice() {
                [argument] if argument.name.is_none() => {
                    argument
                        .value
                        .and_then(|value| match &self.module.expression(value).kind {
                            ExpressionKind::Literal(LiteralKind::String(name)) => {
                                Some(name.clone())
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
                        None => fields.push(crate::types::RecordField {
                            name: Name::new(self.db, field_name),
                            ty: value,
                            optional: false,
                        }),
                    }
                    Ty::new(self.db, TyKind::Record(fields))
                }
                TyKind::Tuple(items) if items.is_empty() => Ty::new(
                    self.db,
                    TyKind::Record(vec![crate::types::RecordField {
                        name: Name::new(self.db, field_name),
                        ty: value,
                        optional: false,
                    }]),
                ),
                // A nominal's representation is fixed — that is what makes
                // `@type` an invariant instead of a label — so this write is
                // checked against it rather than applied to it, and the value
                // keeps its nominal type either way. Retyping the field would
                // leave a value still claiming the nominal while no longer
                // matching its representation; leaving it alone silently (what
                // the fall-through below used to do) left the checker believing
                // the old field type and answering from it in both directions,
                // accepting a call that must fail and rejecting one that cannot.
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
                    && matches!(arguments.as_slice(), [argument] if argument.name.is_none())
        );
        if !is_computed_extract {
            return prior;
        }
        match prior.kind(self.db).clone() {
            TyKind::Tuple(items) if items.is_empty() => Ty::new(self.db, TyKind::NamedList(value)),
            TyKind::NamedList(element) => Ty::new(
                self.db,
                TyKind::NamedList(union_of(self.db, [element, value])),
            ),
            TyKind::List(element) => {
                Ty::new(self.db, TyKind::List(union_of(self.db, [element, value])))
            }
            _ => prior,
        }
    }

    /// `if`/`else` with guard narrowing and diverging-branch flow: a
    /// type-guard condition refines the tested slot along each edge, and a
    /// branch that never falls through (ends in `return`/`stop`/`break`/
    /// `next`) contributes neither its value nor its slot state — which also
    /// makes the surviving edge's refinement persist after the `if` (the
    /// idiomatic early-exit guard).
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

        // The false edge applies to the else branch and — when the then
        // branch diverges — to everything after the `if`.
        let false_mark = self.environment.mark();
        if let Some(guard) = &guard
            && let Some(false_edge) = guard.false_edge
        {
            self.environment.set(guard.slot, EnvEntry::Mono(false_edge));
        }
        let (else_ty, else_diverges) = match else_branch {
            Some(else_branch) => (self.infer(else_branch), self.diverges(else_branch)),
            None => (crate::types::null(self.db), false),
        };

        match (then_diverges, else_diverges) {
            // Only the else edge survives: its state (including the false-edge
            // refinement) flows on; the diverging branch contributes nothing.
            (true, false) => else_ty,
            // Only the then edge survives: discard the else state and re-apply
            // the then writes (including the true-edge refinement) as the
            // ongoing state.
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
                self.join_writes(then_writes);
                self.join_branch_values(then_ty, else_ty)
            }
            // Nothing falls through; the state after the `if` is unreachable,
            // so keep the pre-`if` state.
            (true, true) => {
                self.environment.rollback(false_mark);
                self.join_branch_values(then_ty, else_ty)
            }
        }
    }

    /// `for (name in value) body`: the source is evaluated once and must be
    /// iterable; the loop variable holds the element type inside the body
    /// (re-initialized every iteration, invisible after the loop).
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
                self.unknown()
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
        crate::types::null(self.db)
    }

    /// What one iteration binds, per source shape; `Err` carries the
    /// non-iterable type for the error.
    fn iteration_element(&mut self, resolved: Ty<'db>) -> Result<Ty<'db>, Ty<'db>> {
        match resolved.kind(self.db).clone() {
            // A union of iterables iterates member-wise; a failing member
            // reports the full union.
            TyKind::Union(members) => {
                let mut elements = Vec::with_capacity(members.len());
                for member in members {
                    let element = self.iteration_element(member).map_err(|_| resolved)?;
                    elements.push(element);
                }
                Ok(union_of(self.db, elements))
            }
            TyKind::Scalar(atomic) => Ok(scalar(self.db, atomic)),
            TyKind::Vector(element)
            | TyKind::NamedVector(element)
            | TyKind::List(element)
            | TyKind::NamedList(element) => Ok(element),
            TyKind::Tuple(items) => Ok(union_of(self.db, items)),
            TyKind::Record(fields) => Ok(union_of(self.db, fields.iter().map(|field| field.ty))),
            // `NULL` iterates zero times (legal R).
            TyKind::Null => Ok(crate::types::null(self.db)),
            TyKind::Any => Ok(crate::types::any(self.db)),
            // An already-failed source does not produce a second error.
            TyKind::Unknown => Ok(self.unknown()),
            // An opaque nominal's element shape is not visible.
            TyKind::Named(..) => Ok(crate::types::any(self.db)),
            // Iteration commits neither vector nor list for the caller, so an
            // unresolved variable stays unconstrained and the element
            // degrades to Unknown.
            TyKind::Var(_) | TyKind::Rigid(_) => Ok(self.unknown()),
            _ => Err(resolved),
        }
    }

    /// Check a loop body to a control-flow fixed point: each pass runs from
    /// the join of the pre-loop state and the previous pass's exit writes;
    /// diagnostics count only on the final (stable) pass, and a slot still
    /// changing at the pass cap widens to `Unknown`. `repeat` bodies apply
    /// their exit state after the loop instead of the zero-iterations join.
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
            // A read this pass resolved through the name's cross-item
            // binding discovered the slot's PRE-loop state (the first
            // iteration's view); establish it before joining so the body's
            // writes join into it instead of replacing an absent entry.
            for (&slot, &ty) in &self.pre_materialized {
                if self.environment.get(slot).is_none() {
                    self.environment.set(slot, EnvEntry::Mono(ty));
                }
            }
            let changed = self.join_writes_reporting(&writes);
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
                    self.environment.set(slot, EnvEntry::Mono(self.unknown()));
                    if let Some(name) = self
                        .naming
                        .bindings
                        .get(&slot)
                        .map(|info| info.name.clone())
                    {
                        self.record_strict_origin(
                            loop_expression,
                            StrictOriginKind::LoopWidened(name),
                        );
                    }
                }
                return;
            }
            // Not yet stable: this pass ran under a stale entry state, so its
            // diagnostics are discarded and the body re-checks.
            self.errors.truncate(errors_mark);
            self.strict_origins.truncate(origins_mark);
        }
    }

    /// The formal names this body tests with `missing(name)`. Nested function
    /// bodies are excluded — their tests cover their own formals.
    fn collect_missing_tested(&self, id: ExprId, tested: &mut rustc_hash::FxHashSet<String>) {
        let kind = &self.module.expression(id).kind;
        if matches!(kind, ExpressionKind::Function { .. }) {
            return;
        }
        if let ExpressionKind::Call { callee, arguments } = kind
            && matches!(
                &self.module.expression(*callee).kind,
                ExpressionKind::NameRef(name) if name == "missing"
            )
            && let [argument] = arguments.as_slice()
            && argument.name.is_none()
            && let Some(value) = argument.value
            && let ExpressionKind::NameRef(name) = &self.module.expression(value).kind
        {
            tested.insert(name.clone());
        }
        for child in kind.child_ids() {
            self.collect_missing_tested(child, tested);
        }
    }

    /// Whether an expression never falls through to the code after it: it is
    /// (or is a block ending in) `return(...)`, `stop(...)`, `break`, or
    /// `next`, or an `if ... else` both of whose branches diverge.
    /// `return`/`stop` are recognized by their bare names (rebinding them is
    /// not modeled).
    /// A copy of `ty` with every in-scope rigid binder replaced by a fresh
    /// variable carrying that binder's constraint — one arbitrary instantiation
    /// of the scheme, for asking whether a value could inhabit the declared
    /// type at *some* choice of its parameters rather than at all of them.
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
        crate::types::substitute_rigid(self.db, ty, &substitution)
    }

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
            _ => false,
        }
    }

    /// `return(x)` exits the enclosing function with `x` (`return()` with
    /// `NULL`): the value joins the frame's return union, and the expression
    /// itself yields no observable value where it stands. A top-level
    /// `return` (an R runtime error) still checks its value but joins no
    /// frame.
    fn infer_return(&mut self, range: TextRange, arguments: &[Argument]) -> Ty<'db> {
        let value_ty = match arguments.first().and_then(|argument| argument.value) {
            Some(value) => self.infer(value),
            None => crate::types::null(self.db),
        };
        for argument in arguments.iter().skip(1) {
            if let Some(value) = argument.value {
                self.infer(value);
            }
        }
        let resolved = self.table.resolve(self.db, value_ty);
        let blamed = match arguments.first().and_then(|argument| argument.value) {
            Some(value) => self.blame_range(value),
            // A bare `return()` has no value expression; blame the call.
            None => range,
        };
        if let Some(frame) = self.return_frames.last_mut() {
            frame.push((resolved, blamed));
        }
        crate::types::null(self.db)
    }

    /// A condition that is a type-guard predicate applied to a plain local
    /// variable; `!cond` swaps the edges. `None` when the condition is not a
    /// recognized guard or the guard cannot refine anything.
    fn recognize_guard(&mut self, condition: ExprId) -> Option<GuardRefinement<'db>> {
        match &self.module.expression(condition).kind {
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            } => {
                let inner = self.recognize_guard(*operand)?;
                // `!missing(x)` has no read-erroring edge to swap into: the
                // false edge stays the ordinary supplied state.
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
                let [argument] = arguments.as_slice() else {
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
                    // A name this item reads but does not bind: a top-level
                    // variable an earlier statement assigned. Narrowing it is
                    // sound for the same reason it is sound for a local — the
                    // guard and the branches are one expression, evaluated in
                    // order — but a read from inside a closure is excluded,
                    // because that body runs later and the test proves nothing
                    // about the value it will see then.
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

    /// The slot standing for a non-local name in this item, seeded with the
    /// type the read observed so the guard has a union to filter. `None` when
    /// the read is not a non-local at all (an unresolved name, which narrows
    /// nothing).
    fn non_local_guard_slot(&mut self, read: ExprId, observed: Ty<'db>) -> Option<BindingId> {
        let name = self.naming.non_locals.get(&read)?.clone();
        let slot = match self.non_local_slots.get(&name) {
            Some(&slot) => slot,
            None => {
                let slot = BindingId(self.next_non_local_slot);
                self.next_non_local_slot += 1;
                self.non_local_slots.insert(name, slot);
                slot
            }
        };
        if self.environment.get(slot).is_none() {
            self.environment.set(slot, EnvEntry::Mono(observed));
        }
        Some(slot)
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
                    // The runtime guarantees the true edge is NULL even when
                    // the static type is untracked.
                    TyKind::Any | TyKind::Unknown => {
                        return Some(GuardRefinement {
                            missing_on_true: false,
                            slot,
                            true_edge: Some(crate::types::null(self.db)),
                            false_edge: None,
                        });
                    }
                    // A completely unconstrained variable is SHAPED by the
                    // test: `NULL` is asserted possible, so it becomes
                    // `T | NULL` for a fresh `T` and the edges narrow as an
                    // ordinary union — the unannotated coalesce idiom. Never
                    // on a constrained variable (it cannot hold NULL) or a
                    // rigid (an annotation's contract is not reshaped).
                    TyKind::Var(var) => {
                        let Entry::Unbound {
                            constraint: Constraint::Unconstrained,
                            ..
                        } = *self.table.entry(*var)
                        else {
                            return None;
                        };
                        let fresh = self.fresh(Constraint::Unconstrained);
                        let shaped = union_of(self.db, [fresh, crate::types::null(self.db)]);
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
                // The guard cannot fire without a NULL member; dead branches
                // are not typed specially.
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
                // Family guards do not refine `Any`/`Unknown` (inventing a
                // concrete shape there would false-positive against
                // scalar-claim stub signatures) and never touch an
                // unresolved variable or rigid.
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

    /// Narrowing filters union members; an undecidable member is
    /// conservatively kept on both edges, and an edge that removes nothing
    /// (or keeps nothing) refines nothing.
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
            UnaryOperator::Not => self.infer_unary_not(operand),
            // Unary `+`, `~` formulas, and `?` help are unsupported
            // constructs: sound-by-refusal Unknown.
            UnaryOperator::Plus | UnaryOperator::Tilde | UnaryOperator::Help => {
                self.infer(operand);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                self.unknown()
            }
        }
    }

    /// Negation is elementwise and type-preserving.
    fn infer_unary_minus(&mut self, operand: ExprId) -> Ty<'db> {
        let operand_range = self.blame_range(operand);
        let inferred = self.infer(operand);
        let resolved = self.structural(inferred);
        match self.classify_numeric_operand(resolved) {
            NumericOperand::Concrete(shape, atomic) => self.shaped(shape, atomic),
            // Member-wise over a union operand: negation preserves each
            // member's shape and atomic, so the result is the same union.
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
            // A generic-element vector keeps its element (constrained
            // numeric); an untracked element stays untracked.
            NumericOperand::FlexibleVector(element) => {
                if let Some(element) = element {
                    self.constrain_numeric_flexible(operand_range, element);
                }
                resolved
            }
            NumericOperand::AnyUnknown => self.unknown(),
            NumericOperand::Invalid => {
                self.errors.push(TypeError {
                    range: operand_range,
                    kind: TypeErrorKind::InvalidOperand {
                        expected: OperandExpectation::Numeric,
                        found: resolved,
                    },
                });
                self.unknown()
            }
        }
    }

    fn infer_unary_not(&mut self, operand: ExprId) -> Ty<'db> {
        let operand_range = self.blame_range(operand);
        let inferred = self.infer(operand);
        let resolved = self.structural(inferred);
        match resolved.kind(self.db) {
            // `!` coerces a numeric operand exactly as a condition does
            // (`!0` is TRUE, `!5` is FALSE), and always yields a logical.
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
            TyKind::Any | TyKind::Unknown => self.unknown(),
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
                self.unknown()
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
                // Both operands infer exactly once, here, before any candidate
                // probe: inference writes environment and recorded-type state
                // that a probe snapshot does not reverse.
                //
                // Inferring them again in the fallback is what made a nested
                // chain cost 2^operators — each level re-walked both subtrees,
                // so one machine-written symbolic derivative (they reach 248
                // operators in a single statement) never finished, and every
                // finding inside such a chain was reported twice over.
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
            // Elementwise `&`/`|`, `%op%` specials, the `|>` pipe, `~`
            // formulas, and `?` help are unsupported constructs:
            // sound-by-refusal Unknown. The operands still infer (their types
            // stay recorded for the IDE) but their diagnostics are discarded —
            // the construct is opaque, so nothing inside it is judged.
            And | Or | Special | Pipe | Tilde | Help => {
                let errors_mark = self.errors.len();
                let origins_mark = self.strict_origins.len();
                self.infer(lhs);
                self.infer(rhs);
                self.errors.truncate(errors_mark);
                self.strict_origins.truncate(origins_mark);
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                self.unknown()
            }
        }
    }

    /// A `%…%` operator the STUB CORPUS declares is the call R makes: `a %in% b`
    /// is `` `%in%`(a, b) ``, so the declaration checks and the result is typed.
    /// Deliberately corpus-only — a project's own `%op%` may be a
    /// non-standard-evaluation wrapper whose right operand is quoted rather than
    /// evaluated (magrittr's `%>%` is the canonical one), and checking that as an
    /// ordinary call would reject correct code. Those stay opaque.
    fn infer_declared_operator(
        &mut self,
        range: TextRange,
        name: &str,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<Ty<'db>> {
        let library = crate::stubs::stubs(self.db)?;
        if !library.schemes.contains_key(name) {
            return None;
        }
        // A project definition of the same operator wins, as everywhere else.
        if self
            .globals
            .is_some_and(|globals| globals.defined_in_project(name, false))
        {
            return None;
        }
        let scheme = self.globals?.scheme(name, false)?;
        let instantiated = self.instantiate(&scheme);
        let resolved = self.table.shallow_resolve(self.db, instantiated);
        let TyKind::Function(function) = resolved.kind(self.db).clone() else {
            return None;
        };
        let left = self.infer(lhs);
        let right = self.infer(rhs);
        let arguments = [
            CallArgument {
                name: None,
                name_range: None,
                ty: Some(left),
                range: self.blame_range(lhs),
                value: Some(lhs),
                whole_double: self.is_whole_double(lhs),
                forwards_dots: false,
            },
            CallArgument {
                name: None,
                name_range: None,
                ty: Some(right),
                range: self.blame_range(rhs),
                value: Some(rhs),
                whole_double: self.is_whole_double(rhs),
                forwards_dots: false,
            },
        ];
        let findings = self.match_arguments(range, &function, &arguments);
        self.errors.extend(findings);
        Some(function.ret)
    }

    /// An operator applied to a nominal operand dispatches to that class's
    /// declared operator method, the way R dispatches `d + 30L` on `Date`
    /// through `+.Date`. Without this every class that defines arithmetic —
    /// `Date`, `POSIXct`, `difftime`, and every `+`-based DSL — is a type
    /// error on its most ordinary use, and there is no way to say otherwise.
    ///
    /// Lookup mirrors R's own order: the operator-specific method
    /// (`+.Date`), then the group generic for the operator's group
    /// (`Arith.Date` / `Compare.Date`), then `Ops.Date`. Either operand's
    /// class can supply the method, left first, so `30L + d` works like
    /// `d + 30L`. `None` means no nominal operand declared anything and the
    /// ordinary numeric/comparison rules apply unchanged.
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
            CallArgument {
                name: None,
                name_range: None,
                ty: Some(left),
                range: self.blame_range(lhs),
                value: Some(lhs),
                whole_double: self.is_whole_double(lhs),
                forwards_dots: false,
            },
            CallArgument {
                name: None,
                name_range: None,
                ty: Some(right),
                range: self.blame_range(rhs),
                value: Some(rhs),
                whole_double: self.is_whole_double(rhs),
                forwards_dots: false,
            },
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
                    let TyKind::Function(function) = resolved.kind(self.db).clone() else {
                        self.table.rollback(snapshot);
                        continue;
                    };
                    if self
                        .match_arguments(range, &function, &arguments)
                        .is_empty()
                    {
                        return Some(function.ret);
                    }
                    self.table.rollback(snapshot);
                }
            }
        }
        // A class that declares the operator but fits no candidate is a real
        // mismatch of that operator's contract (`Date + Date` is an error in R
        // too), reported against the class rather than against "not numeric".
        if declared_any {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::UnsupportedOperandPair {
                    operator: spelling,
                    left,
                    right,
                },
            });
            return Some(self.unknown());
        }
        None
    }

    /// The condition of `if`/`while` and the operands of `&&`/`||` must be
    /// scalar logicals; a still-flexible operand binds to `logical`.
    /// The range to blame for the value an expression produces:
    /// parentheses only group, so the innermost non-paren expression
    /// carries the precise range (the oracle blames the same way).
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
        if self.condition_coerces(resolved) {
            return;
        }
        self.unify_or_report(condition_range, scalar(self.db, Atomic::Logical), resolved);
    }

    /// Whether R would coerce this condition rather than refuse it. A numeric
    /// condition is ordinary R — zero is false, anything else true — so
    /// `if (length(x))` and `while (n)` are idiom, not mistakes. Everything
    /// else keeps its error: `character` because R accepts only the spellings
    /// of `TRUE`/`FALSE` there and raises at run time on any other string,
    /// `complex` and `raw` because R refuses them outright, and a vector
    /// because a condition of length other than one is an error in R too.
    /// A still-flexible condition is left to unification, which binds it to
    /// `logical` — the useful default for an unannotated predicate.
    fn condition_coerces(&self, ty: Ty<'db>) -> bool {
        match ty.kind(self.db) {
            TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double) => true,
            TyKind::Union(members) => members.iter().all(|&member| self.condition_coerces(member)),
            _ => false,
        }
    }

    /// Binary arithmetic over the classified operand shapes: member-wise over
    /// unions (a vector member makes the pair a vector, both-`integer` pairs
    /// stay `integer`, any `double` — or an always-`double` operator like `/`
    /// — promotes the pair), with flexible operands collapsed onto one
    /// representative so `x + y` ties the two together.
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
                return self.unknown();
            }
        }
        if matches!(left, NumericOperand::AnyUnknown) || matches!(right, NumericOperand::AnyUnknown)
        {
            return self.unknown();
        }

        // Collapse flexible operands (variables and numeric rigids) onto one
        // representative so `x + y` ties the two operands together; a rigid
        // pair that cannot unify is a genuine bound violation.
        let mut flexible: Option<Ty<'db>> = None;
        for operand in [&left, &right] {
            if let NumericOperand::Flexible(ty) = operand {
                match flexible {
                    Some(existing) => {
                        if let Err(error) = self.table.unify(self.db, existing, *ty) {
                            self.report_unify(range, error);
                            return self.unknown();
                        }
                    }
                    None => flexible = Some(*ty),
                }
            }
        }
        if let Some(ty) = flexible {
            self.constrain_numeric_flexible(range, ty);
        }
        // A generic vector element (`T[]`) used arithmetically must be
        // numeric; joined with the atomic-element bound it already carries,
        // the element becomes scalar-numeric.
        for operand in [&left, &right] {
            if let NumericOperand::FlexibleVector(Some(element)) = operand {
                self.constrain_numeric_flexible(range, *element);
            }
        }

        // A flexible-element vector operand fixes the result shape (vector)
        // without fixing the atomic: an always-double operation or a concrete
        // `double` (or union) partner promotes to `double[]`; an integer
        // partner promotes *into* the element, so the result keeps the
        // element; two generic elements unify; an untracked element stays
        // untracked.
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
                        return self.unknown();
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
                TyKind::Vector(element.unwrap_or_else(|| self.unknown())),
            );
        }

        match (left.concrete_parts(), right.concrete_parts()) {
            // Member-wise; a single concrete operand is the one-member case,
            // so this arm also carries the ordinary concrete/concrete path.
            (Some(left_parts), Some(right_parts)) => {
                let members =
                    self.member_wise_numeric_results(&left_parts, &right_parts, always_double);
                union_of(self.db, members)
            }
            (left_parts, right_parts) => {
                let Some(flexible) = flexible else {
                    return self.unknown();
                };
                let concrete_parts = left_parts.or(right_parts);
                if let Some(parts) = &concrete_parts
                    && parts.len() > 1
                {
                    // A union operand cannot promote into a variable
                    // member-wise, so the flexible side pins to the default
                    // numeric scalar (`double`) and the operation continues
                    // member-wise.
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
                    // `x + 1L` (and `x + y`) stay polymorphic over the
                    // numeric operand: integer promotes to whatever it
                    // resolves to, so the scalar result is the operand
                    // itself.
                    OperandShape::Scalar => flexible,
                    // A vector result cannot carry an unresolved atomic, so
                    // a flexible operand defaults to `double` here.
                    OperandShape::Vector => {
                        self.unify_or_report(range, flexible, scalar(self.db, Atomic::Double));
                        self.shaped(OperandShape::Vector, Atomic::Double)
                    }
                }
            }
        }
    }

    /// R's `:` yields an integer sequence for whole-number endpoints
    /// (`1:10` counts, via the literal rule); a `double` endpoint — or a
    /// flexible one, which may resolve to `double` — makes it `double[]`.
    /// Endpoints must be scalar numbers: the plain numeric bound would admit
    /// vectors, which R only warns about and truncates.
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
                TyKind::Any | TyKind::Unknown => return self.unknown(),
                TyKind::Var(var) => {
                    if let Err(error) =
                        self.table
                            .constrain(self.db, *var, Constraint::ScalarNumeric)
                    {
                        self.report_unify(operand_range, error);
                        return self.unknown();
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
                    return self.unknown();
                }
            }
        }
        Ty::new(self.db, TyKind::Vector(scalar(self.db, result_atomic)))
    }

    /// Comparisons: both sides must share a comparison family (numeric,
    /// character, logical — member-wise over unions), a flexible operand
    /// compared against a concrete numeric partner is constrained numeric,
    /// and the result is `logical` shaped element-wise (a vector member
    /// compares to `logical[]`).
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
            return self.unknown();
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
                return self.unknown();
            }
        }

        // Two concrete operands must belong to the same comparison family,
        // member-wise: every shape the left union can take must be comparable
        // with every shape of the right.
        if let (Some(left_parts), Some(right_parts)) = (&left_parts, &right_parts)
            && left_parts.iter().any(|(_, left_family)| {
                right_parts
                    .iter()
                    .any(|(_, right_family)| left_family != right_family)
            })
        {
            self.errors.push(TypeError {
                // The right operand, not the whole expression: the message
                // takes the left side as the expectation, so underlining both
                // leaves the reader unable to tell which type is which. This is
                // where arithmetic already points.
                range: rhs_range,
                kind: TypeErrorKind::Mismatch {
                    expected: resolved_left,
                    found: resolved_right,
                },
            });
            return self.unknown();
        }

        // A flexible operand compared against a concrete numeric operand is
        // constrained numeric; comparison against a non-numeric family leaves
        // it free (the system has no character-or-logical constraint).
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
                let atomic = if always_double {
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

    /// Constrain a flexible operand (variable or rigid) to be numeric: a
    /// variable's constraint joins through the lattice; a rigid satisfies it
    /// only when its declaration promised numeric-ness.
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

    /// How an arithmetic operand classifies once resolved.
    fn classify_numeric_operand(&self, resolved: Ty<'db>) -> NumericOperand<'db> {
        if let Some(parts) = numeric_operand_parts(self.db, resolved) {
            return NumericOperand::Concrete(parts.0, parts.1);
        }
        match resolved.kind(self.db) {
            // A union operand is numeric when every member is; any
            // non-numeric member makes the whole operand invalid (the error
            // then shows the full union type).
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

    /// Check a function definition against its declared annotation type and
    /// return the signature its callers see.
    ///
    /// An annotation declares the *types* of a definition's parameters, never
    /// the parameter list: R matches a call's arguments against the formals in
    /// the `function(...)` header, so the returned signature always has the
    /// definition's parameters, in the definition's order, with the
    /// definition's optionality and `...` position. Declared types fill them
    /// name-aware (named declarations match by name, positional declarations
    /// fill the rest in order; rigid binder types refuse to bind), the body
    /// infers under them, and the result checks against the declared return.
    ///
    /// Every way the two sides can disagree is therefore reported once, here,
    /// and never again at a call site — a call must not be blamed for an
    /// annotation's mistake.
    fn check_declared_function(
        &mut self,
        function_id: ExprId,
        declared: &FunctionType<'db>,
    ) -> Ty<'db> {
        let expression = self.module.expression(function_id).clone();
        let ExpressionKind::Function { parameters, body } = &expression.kind else {
            return Ty::new(self.db, TyKind::Function(declared.clone()));
        };
        let range = expression.range;

        // A formal the body tests with `missing(name)` is optional at call
        // sites just as one with a default is — R's optional-without-default
        // idiom — so it counts as optional on the definition's side of every
        // comparison below.
        let mut missing_tested = rustc_hash::FxHashSet::default();
        self.collect_missing_tested(*body, &mut missing_tested);
        let is_optional = |parameter: &crate::hir::Parameter| {
            parameter.default.is_some() || missing_tested.contains(&parameter.name)
        };

        self.table.level += 1;
        // One walk pairs formals with declared types the way R's matcher pairs
        // a call's arguments with them, and builds the exported signature as
        // it goes. What the annotation declares and this walk never consumes
        // is the disagreement.
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
                // Partial annotation is a supported form, so a formal the
                // annotation leaves alone infers from its uses exactly as it
                // would with no annotation at all: the export edge either
                // generalizes the variable or erases it to `Unknown`.
                None
            };
            let parameter_ty = declared_ty.unwrap_or_else(|| self.fresh(Constraint::Unconstrained));
            formal_types.push((parameter_ty, declared_ty.is_some()));
            signature.named.push(Parameter {
                name: Name::new(self.db, parameter.name.clone()),
                ty: parameter_ty,
                optional: is_optional(parameter),
                default: parameter.default.map(|default| self.infer(default)),
            });
        }

        // The declared shape must be one R's argument matcher can honor for
        // this definition: a declared optional `[name]` needs an actual
        // default (callers may omit it), the rest parameter must sit at the
        // same boundary on both sides — including not existing on exactly one
        // side — and every declared type must have a formal to land on. A
        // violation reports the two shapes whole; the body still checks under
        // the declared parameter types so hover and navigation keep their
        // facts.
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
        let optional_mismatch = parameters.iter().any(|parameter| {
            !is_optional(parameter)
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
        // A formal with a default is optional in R whatever the annotation
        // says, so the signature above already took optionality from the code;
        // saying so once here is what keeps it from resurfacing as a missing
        // argument at every caller.
        for field in &declared.named {
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
        // Declared named parameters the definition never declares.
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
                // A `NULL` default is R's "no value" sentinel for optional
                // parameters, always allowed regardless of the declared type;
                // any other default must fit the declared type. An undeclared
                // formal's type comes from its uses, not its default.
                let resolved_default = self.table.resolve(self.db, default_ty);
                if declared {
                    let default_range = self.blame_range(default);
                    // The default is checked against an *instantiation* of the
                    // declared type, not against the rigid binder itself. A
                    // binder is the caller's choice, and omitting the argument
                    // is the one call where the default makes that choice — so
                    // `<T> fn([x]: T)` over `function(x = 1)` is honest, and
                    // was rejected while the comparison used the rigid `T`,
                    // which is how the checker came to refuse a scheme it had
                    // inferred itself. A concrete declared type is unaffected:
                    // `fn(title: character)` still refuses a `NULL` default.
                    let admissible = self.instantiate_rigids(parameter_ty);
                    // `function(title = NULL)` is how R spells an optional
                    // argument, but the caller omitting it puts `NULL` in the
                    // body, where a declared `character` is a promise the
                    // function does not keep. Exempting it — which this used
                    // to do — let the annotation lie: `if (title == "draft")`
                    // then passed the checker and failed at run time with
                    // `argument is of length zero`. The remedy is a nullable
                    // declared type plus a guard, so the message names it
                    // instead of reporting a bare mismatch.
                    if matches!(resolved_default.kind(self.db), TyKind::Null)
                        && !self.table.compatible(self.db, resolved_default, admissible)
                    {
                        self.errors.push(TypeError {
                            range: default_range,
                            kind: TypeErrorKind::NullDefaultNotAdmitted {
                                name: parameter.name.clone(),
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
        // The body's value only needs to be *compatible* with the declared
        // return (covariant, like an argument against a parameter), so a body
        // returning `integer` satisfies a declared `integer | NULL` — and an
        // alias-typed declaration checks through its expansion. An Unknown
        // declared return (elided `->`) constrains nothing — it is inferred
        // from the body instead, exactly as an unannotated definition's is, so
        // annotating only the parameters does not silently drop the return.
        if matches!(declared.ret.kind(self.db), TyKind::Unknown) {
            signature.ret = self.join_early_returns(&early_returns, trailing_ty);
        } else {
            // Each `return` is checked at its OWN site: reporting the union of
            // every return against the declaration would blame whichever
            // expression came last and leave the reader to work out which
            // return is actually wrong.
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
            // Blame the expression that produces the value, not the whole
            // body: a block's tail, and through an `if`/`else` tail, the arm
            // that is actually wrong. A finding on the whole construct makes
            // the reader work out which branch it means, and the offending
            // line is not even rendered when the range is clamped to the
            // construct's first line.
            let mut leaves = Vec::new();
            self.collect_tail_expressions(*body, &mut leaves);
            let blamed = match leaves.as_slice() {
                [single] => *single,
                _ => *body,
            };
            let body_range = self.blame_range(blamed);
            // The trailing value alone: the early returns already reported
            // themselves, so including them here would double-report and
            // widen the found type past what this expression produces.
            let resolved_body = self.table.resolve(self.db, trailing_ty);
            if !early_return_failed
                && !matches!(resolved_body.kind(self.db), TyKind::Unknown)
                && !self.table.compatible(self.db, resolved_body, declared.ret)
            {
                // The whole body's type is the verdict; the leaves only decide
                // where to point, and each one that fails on its own reports at
                // its own site, exactly as each `return` does. When no single
                // leaf is at fault — an `if` with no `else` contributes an
                // implicit `NULL` that belongs to no expression — the tail
                // keeps the one finding.
                let culprits: Vec<(ExprId, Ty<'db>)> = leaves
                    .iter()
                    .filter(|&&leaf| leaf != blamed)
                    .filter_map(|&leaf| {
                        let ty = self.table.resolve(self.db, self.recorded_ty(leaf));
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
        self.table.level -= 1;
        // Not resolved: every type in here came from the annotation, so it
        // holds no inference variables — and resolving would expand the
        // aliases and nominals the author wrote, which are the whole point of
        // declaring the signature.
        Ty::new(self.db, TyKind::Function(signature))
    }

    /// The type already inferred for an expression, `Unknown` when the walk
    /// never recorded one.
    fn recorded_ty(&self, id: ExprId) -> Ty<'db> {
        self.recorded.get(&id).copied().unwrap_or(unknown(self.db))
    }

    /// Every expression whose value can become the enclosing function's result,
    /// following a block to its tail and an `if`/`else` into both arms. An `if`
    /// with no `else` also yields `NULL` on the untaken path, which belongs to
    /// no expression, so it contributes its branch and nothing else.
    fn collect_tail_expressions(&self, id: ExprId, into: &mut Vec<ExprId>) {
        match &self.module.expression(id).kind {
            ExpressionKind::Block {
                statements,
                trailing_semicolon: false,
            } => match statements.last() {
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

    /// Check a function body, re-running it once when the walk grew a
    /// captured-write join some closure had already read (the letrec /
    /// forward-capture shape): the first run exists to complete the joins and
    /// is fully discarded — environment, unification, diagnostics, and
    /// pending super-assign writes all roll back — so the re-run resolves
    /// forward captures against the completed joins with no stale effects.
    /// Bodies that never grow such a join (the overwhelming majority) pay
    /// only the snapshot markers.
    fn infer_body_with_capture_discovery(&mut self, body: ExprId, pending_mark: usize) -> Ty<'db> {
        let saved_repass = std::mem::replace(&mut self.capture_repass_needed, false);
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

    /// A function's return type is the union of every early `return` value
    /// with the body's trailing value.
    fn join_early_returns(
        &self,
        early_returns: &[(Ty<'db>, TextRange)],
        trailing: Ty<'db>,
    ) -> Ty<'db> {
        if early_returns.is_empty() {
            return trailing;
        }
        let mut members: Vec<Ty<'db>> = early_returns.iter().map(|(ty, _)| *ty).collect();
        members.push(self.table.resolve(self.db, trailing));
        union_of(self.db, members)
    }

    /// The call entry point: shape-constructing builtins (`c`, `list`,
    /// `switch`) intercept first, an overloaded stub callee resolves per call
    /// site (each candidate probed in declaration order), and everything else
    /// infers the callee and dispatches on its type.
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
                _ => None,
            };
            if let Some(ty) = builtin {
                return ty;
            }
        }
        if let Some(ty) = self.try_overloaded_call(id, range, callee, arguments) {
            return ty;
        }
        let callee_range = self.blame_range(callee);
        let callee_ty = self.infer(callee);
        // A callee typed as literal `Any` is the sanctioned escape hatch
        // (`stop`, `warning`, `seq` — stubs whose signature is not
        // expressible yet): the call is uncheckable, so diagnostics from its
        // argument expressions are noise in a context the checker has
        // already given up on. Inference still runs — expression types stay
        // for the IDE — but findings inside the arguments are discarded.
        let resolved_callee = self.table.shallow_resolve(self.db, callee_ty);
        if matches!(resolved_callee.kind(self.db), TyKind::Any) {
            let recorded_errors = self.errors.len();
            let recorded_origins = self.strict_origins.len();
            self.infer_call_arguments(range, arguments);
            self.errors.truncate(recorded_errors);
            self.strict_origins.truncate(recorded_origins);
            // This call IS where the `Unknown` enters the program: the callee
            // has no expressible signature yet, so strict mode attributes it
            // here rather than at whatever later line first reads the result.
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return self.unknown();
        }
        let call_arguments = self.infer_call_arguments(range, arguments);
        self.dispatch_call(range, callee_range, callee_ty, &call_arguments)
    }

    /// `c(...)` follows R's atomic coercion hierarchy (logical < integer <
    /// double < complex < character; `raw` only combines with itself), drops
    /// `NULL` arguments (`c(x, NULL)` is `c(x)`, `c()` is `NULL`), and keeps
    /// names: an all-named call builds a map-like vector.
    /// `c()` over arguments that are all the same nominal, when that class
    /// declares a `c.Class` method: the result is that nominal. `None` when the
    /// arguments are not uniform nominals or the class declares no such method.
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
            return crate::types::null(self.db);
        }
        // `c()` over any list-shaped argument concatenates into a LIST, not an
        // atomic vector — `c(list_a, list_b)` is the standard way to append to
        // a list in R. The atomic coercion below cannot describe that, so the
        // list case takes its own path.
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
        // R dispatches `c()` too, and a class whose `c.Class` method is
        // declared keeps its class through concatenation — `c(d1, d2)` on two
        // `Date`s is a `Date` vector, not the integers underneath. Uniform
        // nominal arguments therefore resolve through that declaration; a
        // nominal with no such method falls through to the atomic rules below,
        // where it becomes indeterminate rather than an error, because the
        // checker does not know what R's default `c()` makes of it.
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
            // A non-concrete argument whose element atomic is not statically
            // known — `Any`, `Unknown` (which must never cascade), or an
            // unresolved variable (`function(x) c(x, 1L)`) — cannot pin the
            // combined element type: the result is `Unknown` rather than a
            // rejection or an unsound concrete claim. The variable stays
            // unconstrained, mirroring `$`/`[[`/`[` on the same subject.
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
            // A union argument combines member-wise; its `NULL` members
            // contribute nothing (the idiomatic accumulator seeded with
            // `c()` has type `T[] | NULL` at the loop join and is no error).
            let argument_atomics: Option<Vec<Atomic>> = match resolved.kind(self.db).clone() {
                TyKind::Union(members) => members
                    .iter()
                    .filter(|member| !matches!(member.kind(self.db), TyKind::Null))
                    .map(|&member| combine_operand_atomic(self.db, member))
                    .collect(),
                _ => combine_operand_atomic(self.db, resolved).map(|atomic| vec![atomic]),
            };
            let Some(argument_atomics) = argument_atomics.filter(|atomics| !atomics.is_empty())
            else {
                // A nominal with no declared `c.Class` method is not an error:
                // R's default `c()` strips attributes and returns something the
                // checker cannot name, so the result is indeterminate. Claiming
                // a mismatch here reported `expected integer, found gg` on
                // correct code, an expectation nothing in the call asked for.
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
            return crate::types::null(self.db);
        }
        if result_indeterminate {
            return self.unknown();
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

    /// The element type of a list-shaped type: the declared element for
    /// `list[T]`/`list[named: T]`, the join of the items or fields for a
    /// tuple or record shape. `None` for anything that is not a list.
    fn list_element_type(&self, ty: Ty<'db>) -> Option<Ty<'db>> {
        match ty.kind(self.db) {
            TyKind::List(element) | TyKind::NamedList(element) => Some(*element),
            TyKind::Tuple(items) => Some(union_of(self.db, items.clone())),
            TyKind::Record(fields) => Some(union_of(self.db, fields.iter().map(|field| field.ty))),
            _ => None,
        }
    }

    /// `list(...)` builds the fixed shapes: all-unnamed → tuple-like,
    /// all-named → record-like, partially named → an array-like list.
    /// `structure(value, ...)` returns `value` with attributes attached, and
    /// attributes are not part of a type here — a `class` attribute is data,
    /// which is why S3 dispatch is not modelled. So the call has the type of
    /// its first argument, and `structure(list(name = "a"), class = "dog")`
    /// stays the record it is built from, with its fields checkable.
    ///
    /// `dim` is the exception: it turns a vector into an array, and array
    /// *shape* is untracked, so claiming the vector type would be a claim the
    /// checker cannot stand behind. Those fall through to the declaration
    /// (`Any`) and stay a strict origin. `None` means "not intercepted".
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
            // A partially named list is ordinary R — `do.call(f, list(x, n = 1))`
            // is the standard spelling. Neither the tuple nor the record shape
            // can express it, so the names are dropped and the value types join
            // into an array-like list: less precise than either shape, never a
            // false rejection of legal code.
            let mut items = Vec::with_capacity(arguments.len());
            for argument in arguments {
                if let Some(value) = argument.value {
                    let inferred = self.infer(value);
                    items.push(self.table.resolve(self.db, inferred));
                }
            }
            return Ty::new(self.db, TyKind::List(union_of(self.db, items)));
        }
        if all_named {
            let mut fields = Vec::with_capacity(arguments.len());
            for argument in arguments {
                let ty = match argument.value {
                    Some(value) => {
                        let inferred = self.infer(value);
                        self.table.resolve(self.db, inferred)
                    }
                    None => self.unknown(),
                };
                let name = argument.name.clone().expect("all_named was checked");
                fields.push(crate::types::RecordField {
                    name: Name::new(self.db, name),
                    ty,
                    optional: false,
                });
            }
            Ty::new(self.db, TyKind::Record(fields))
        } else {
            let mut items = Vec::with_capacity(arguments.len());
            for argument in arguments {
                let ty = match argument.value {
                    Some(value) => {
                        let inferred = self.infer(value);
                        self.table.resolve(self.db, inferred)
                    }
                    None => self.unknown(),
                };
                items.push(ty);
            }
            Ty::new(self.db, TyKind::Tuple(items))
        }
    }

    /// `switch(subject, a = ..., b = ..., default)` selects one branch by the
    /// subject's runtime value. Selection cannot be modeled statically, but
    /// every branch IS checked, and the call's type is the union of the
    /// branch values. R returns invisible `NULL` when nothing matches, so
    /// `NULL` joins the union unless a default (unnamed) branch exists; a
    /// named branch with no value falls through and contributes no type.
    fn infer_switch(&mut self, range: TextRange, arguments: &[Argument]) -> Ty<'db> {
        let Some((subject, branches)) = arguments.split_first() else {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::ArityMismatch {
                    expected: 1,
                    found: 0,
                },
            });
            return self.unknown();
        };
        if let Some(value) = subject.value {
            self.infer(value);
        }
        let mut members = Vec::with_capacity(branches.len() + 1);
        let mut has_default = false;
        // Exactly one alternative runs, so the branches fork and join like the
        // arms of an `if` — the same shape generalized from two paths to many.
        // Inferring them in sequence instead let a later branch's write win
        // outright, so `switch(k, a = { r <- 1L }, { r <- "d" })` left `r` a
        // plain `character` where the `if` spelling of it correctly joins to
        // `integer | character`.
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
            // A branch that cannot fall through contributes no state, exactly
            // as a diverging `if` arm does not.
            if !self.diverges(value) {
                branch_writes.push(self.environment.writes_since(mark));
            }
            self.environment.rollback(mark);
        }
        // One branch's writes become the ongoing state and the rest join into
        // it, which is what `infer_if` does with the else arm's state and the
        // then arm's writes.
        if let Some((adopted, joined)) = branch_writes.split_last() {
            for &(slot, entry) in adopted {
                if let Some(entry) = entry {
                    self.environment.set(slot, entry);
                }
            }
            for writes in joined {
                self.join_writes(writes.clone());
            }
        }
        if !has_default {
            members.push(crate::types::null(self.db));
        }
        union_of(self.db, members)
    }

    /// Ordered overload probing. Declaration order is the tiebreak, not the
    /// whole rule: a candidate that accepts the arguments without narrowing
    /// any of the caller's still-open inference variables is a fact rather
    /// than a guess, and outranks an earlier-declared candidate that fits
    /// only by pinning one down. The whole-number-literal courtesy is off in
    /// the first round and a second round runs only if nothing matched
    /// strictly, so a literal never steers selection away from a candidate
    /// the argument's true type would have chosen. Only a plain name resolves
    /// through an overload set, and a local binding shadowing the name
    /// disables it (the local wins, as everywhere). `None` means "not an
    /// overloaded call" — fall through to normal dispatch.
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
                    return None;
                }
                name.clone()
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

        // Arguments are inferred exactly once, before any probe: expression
        // inference writes state the probe snapshot does not reverse (the
        // environment, recorded expression types), so running it inside a
        // probe would leak bindings that reference rolled-back variable ids.
        let call_arguments = self.infer_call_arguments(range, arguments);

        // A fit is a FACT when the concrete arguments alone chose the
        // candidate, and a GUESS when the candidate only fits because
        // unification narrowed one of the caller's own flexible types —
        // binding a wrapper's parameter (`function(x) sum(x)`) to whichever
        // candidate was probed first would reject calls R accepts. So the
        // caller's open variables are recorded up front, every candidate is
        // probed, and a fit that left them all untouched beats one that did
        // not. Among fits of the same kind declaration order decides, which is
        // the plain first-match rule: the corpus orders every set
        // most-specific-first and ends it with a general fallback, and a
        // fallback taking `Any` accepts without binding, so it is a fact and
        // already outranks any guess above it — `function(x) sum(x)` keeps its
        // parameter open without needing a last-wins tiebreak.
        let mut caller_variables = FxHashSet::default();
        for argument in &call_arguments {
            if let Some(ty) = argument.ty {
                self.table
                    .collect_unbound_vars(self.db, ty, &mut caller_variables);
            }
        }
        let caller_entries: Vec<(InferenceVar, Entry<'db>)> = caller_variables
            .into_iter()
            .map(|var| (var, self.table.entry(var).clone()))
            .collect();

        // Selection runs strict first, then (only if nothing matched and a
        // whole-number double literal is present) once more with the
        // literal-as-integer courtesy: `1` is genuinely a double at runtime,
        // so letting it match an integer candidate in the strict round would
        // pick a signature whose return misstates what R computes
        // (`sum(1, 2)` is a double). The courtesy round keeps a name whose
        // only fitting candidate wants `integer` callable as `foo(1)`.
        let rounds: &[bool] = if call_arguments.iter().any(|argument| argument.whole_double) {
            &[false, true]
        } else {
            &[false]
        };

        // Every candidate's own first failure, in declaration order, from the
        // strict round only: the courtesy round is a leniency, so a candidate
        // it also rejects was already rejected on the honest reading.
        let mut candidate_errors: Vec<TypeError<'db>> = Vec::new();
        let mut fits: Vec<(usize, bool)> = Vec::new();
        let mut fitting_courtesy = false;
        for &courtesy in rounds {
            for (index, scheme) in schemes.iter().enumerate() {
                let snapshot = self.table.snapshot();
                match self.probe_overload_candidate(callee, scheme, &call_arguments, courtesy) {
                    Ok((resolved, function)) => {
                        // Nothing flexible to protect: the first fit is the
                        // answer and the bindings it made stand.
                        if caller_entries.is_empty() {
                            self.recorded.insert(callee, resolved);
                            self.selected_overloads.insert(callee, index);
                            return Some(self.committed_overload_return(id, function.ret));
                        }
                        let free = caller_entries.iter().all(|(var, entry)| {
                            self.table.find(*var) == *var && self.table.entry(*var) == entry
                        });
                        self.table.rollback(snapshot);
                        fits.push((index, free));
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

        // With one fit the two arms agree, so a forced choice is never treated
        // as a guess. The winner is re-probed because probing rolls back: the
        // table is in its pre-probe state again and matching is a pure
        // function of it, so the fit repeats — and this time it commits.
        if let Some(&(index, _)) = fits.iter().find(|(_, free)| *free).or(fits.first())
            && let Some(scheme) = schemes.get(index)
        {
            let snapshot = self.table.snapshot();
            match self.probe_overload_candidate(callee, scheme, &call_arguments, fitting_courtesy) {
                Ok((resolved, function)) => {
                    self.recorded.insert(callee, resolved);
                    self.selected_overloads.insert(callee, index);
                    return Some(self.committed_overload_return(id, function.ret));
                }
                Err(_) => self.table.rollback(snapshot),
            }
        }

        // Naming the set ("no overload matches — I tried all N signatures") is
        // what the reader needs when the call could plausibly have meant any of
        // several different shapes and those shapes disagree about what is
        // wrong: `pick("word")` against `fn(integer)` and `fn(double)` is
        // rejected at the same argument by both, for different reasons, and
        // neither reason is *the* answer.
        //
        // Two shapes of failure have one answer, which the wrapper would bury —
        // along with the argument's own range, since the wrapper blames the
        // whole call:
        //   - every candidate rejected the call for the very same reason;
        //   - one candidate got strictly further into the call than any other,
        //     which makes it the signature the caller meant. A two-parameter
        //     callback passed to `lapply` is rejected at the callback by the
        //     candidate that accepted the sequence, and at the sequence by the
        //     candidate that wanted a named list; the first one is the finding.
        //
        // How far a candidate got is the index of the first argument it blamed;
        // a whole-call verdict (wrong arity, a name no formal declares) blames
        // no argument and got nowhere.
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
        self.recorded.insert(callee, self.unknown());
        // A capture-discovery re-pass overwrites recorded state in place; a
        // call that committed on the first pass but fails on the re-pass
        // must not keep the stale commitment.
        self.selected_overloads.remove(&callee);
        Some(self.unknown())
    }

    /// The selected candidate's return type, with a strict-mode origin recorded
    /// when it is `Any`. The corpus ends an overload set with an `Any` fallback
    /// for the calls it cannot describe (`min` over a classed value), and `Any`
    /// satisfies every later check — so the call is a genuine hole, and a user
    /// who turned strict on to find holes should be told about this one. Strict
    /// otherwise reports only `Unknown`, which is why these were invisible.
    fn committed_overload_return(&mut self, id: ExprId, ret: Ty<'db>) -> Ty<'db> {
        if matches!(
            self.table.shallow_resolve(self.db, ret).kind(self.db),
            TyKind::Any
        ) {
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
        }
        ret
    }

    /// One overload probe. `Ok` means the candidate fits and the bindings it
    /// made are live in the table — the caller keeps or rolls them back. `Err`
    /// carries the findings that rejected it, empty when the candidate is not
    /// a function type at all (nothing about the call is wrong then).
    fn probe_overload_candidate(
        &mut self,
        callee: ExprId,
        scheme: &TypeScheme<'db>,
        call_arguments: &[CallArgument<'db>],
        courtesy: bool,
    ) -> Result<(Ty<'db>, FunctionType<'db>), Vec<TypeError<'db>>> {
        let instantiated = self.instantiate(scheme);
        let resolved = self.table.shallow_resolve(self.db, instantiated);
        let TyKind::Function(function) = resolved.kind(self.db).clone() else {
            return Err(Vec::new());
        };
        if !courtesy {
            self.overload_probe_depth += 1;
        }
        let callee_range = self.blame_range(callee);
        let outcome = self.match_arguments(callee_range, &function, call_arguments);
        if !courtesy {
            self.overload_probe_depth -= 1;
        }
        if outcome.is_empty() {
            Ok((resolved, function))
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
                    name: argument.name.clone(),
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
            ExpressionKind::Literal(LiteralKind::Double(text)) => {
                crate::hir::is_whole_number_double(text)
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
        // An alias- or nominal-typed callee calls through its representation:
        // `#: Formatter` (an alias of a function type) names the same
        // callable its expansion does. Undeclared names floor to `Unknown`
        // inside `structural`, so a typo'd callee stays quiet here (the
        // unknown-type diagnostic owns it).
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
            TyKind::Any | TyKind::Unknown => self.unknown(),
            TyKind::Var(_) => {
                // An unresolved callee: constrain it to a function of the
                // observed shape.
                let ret = self.fresh(Constraint::Unconstrained);
                let positional: Vec<Ty<'db>> = arguments
                    .iter()
                    .filter(|argument| argument.name.is_none())
                    .map(|argument| argument.ty.unwrap_or_else(|| self.unknown()))
                    .collect();
                let expected = Ty::new(
                    self.db,
                    TyKind::Function(FunctionType {
                        positional,
                        named: arguments
                            .iter()
                            .filter_map(|argument| {
                                argument.name.as_ref().map(|name| crate::types::Parameter {
                                    name: Name::new(self.db, name.clone()),
                                    ty: argument.ty.unwrap_or_else(|| self.unknown()),
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
            // A call through a union of functions — the dispatch-table idiom,
            // `handlers[[name]](...)` — must be valid for every member, since
            // the value could be any of them. Each member's signature is
            // probed against the arguments in an isolated snapshot and the
            // call's type is the union of the member returns; returns are
            // variable-erased because the probe bindings that produced them
            // roll back.
            // A callee that is functions plus `NULL` — what `switch` without a
            // default produces, since R returns invisible `NULL` when nothing
            // matches. The value IS callable on every non-`NULL` path, so
            // "not a function" would be the wrong complaint: the finding is the
            // nullability, and the arguments and result still check against the
            // function members so nothing downstream cascades.
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
            TyKind::Union(members)
                if members.iter().all(|&member| {
                    matches!(
                        self.table.shallow_resolve(self.db, member).kind(self.db),
                        TyKind::Function(_)
                    )
                }) =>
            {
                let members = members.clone();
                let mut returns = Vec::with_capacity(members.len());
                for member in members {
                    let member = self.table.shallow_resolve(self.db, member);
                    let TyKind::Function(function) = member.kind(self.db).clone() else {
                        continue;
                    };
                    let snapshot = self.table.snapshot();
                    let findings = self.match_arguments(callee_range, &function, arguments);
                    if findings.is_empty() {
                        let member_return = self.table.resolve(self.db, function.ret);
                        self.table.rollback(snapshot);
                        returns.push(crate::types::erase_vars(self.db, member_return));
                    } else {
                        self.table.rollback(snapshot);
                        self.errors.extend(findings);
                        return self.unknown();
                    }
                }
                union_of(self.db, returns)
            }
            _ => {
                // The defect is the callee, not the whole call: blame
                // exactly the expression that is not a function.
                self.errors.push(TypeError {
                    range: callee_range,
                    kind: TypeErrorKind::NotAFunction {
                        found: self.table.resolve(self.db, resolved),
                    },
                });
                self.unknown()
            }
        }
    }

    /// R's argument matcher over already-inferred argument types. Every
    /// argument is checked, so a call with three wrong arguments reports three
    /// findings rather than forcing a fix-one-recheck loop; a failed
    /// `compatible` leaves the table untouched, so each argument's verdict is
    /// independent. Returns the findings in argument order — empty means the
    /// call matches, which is what an overload probe tests inside a snapshot.
    /// A *structural* failure (wrong arity, a name no parameter declares)
    /// describes the call as a whole and is returned alone: per-argument
    /// mismatches under a mis-shaped call are misleading.
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

        // A function-typed argument earlier in the call may be checked against
        // the arguments forwarded to it later in the call
        // (`lapply(x, gsub, pattern = "a")`), so the rest parameter's intake
        // has to be known before any argument is checked.
        let forwarded_argument_indexes: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(_, target)| matches!(target, Some(ArgumentTarget::Rest)))
            .map(|(index, _)| index)
            .collect();

        let forwards_dots = arguments.iter().any(|argument| argument.forwards_dots);
        let mut findings = Vec::new();
        // A name no parameter declares throws off the positional accounting,
        // so its arity verdict would be noise on top of a finding that already
        // says what is wrong.
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
                            // The first argument with no formal left to take
                            // it: that is the one the reader has to remove,
                            // while the callee is the part of the call that is
                            // right. A missing argument still blames the callee
                            // — there is no argument to point at.
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

        // A formal the call leaves out is filled by its default, evaluated in
        // the function's own frame — so the parameter holds the default's type
        // on this call, not whatever the caller might have passed. Without
        // this the parameter stays a free variable and `f()` for
        // `f <- function(x = 1) x` yields `Any`, which is compatible with
        // everything and silences every check downstream of it.
        //
        // Skipped when the call forwards `...`: an argument may be arriving
        // through the dots, so the formal is not known to be omitted.
        if !forwards_dots {
            for (formal, parameter) in function.named.iter().enumerate() {
                let supplied = targets.iter().any(
                    |target| matches!(target, Some(ArgumentTarget::Named(filled)) if *filled == formal),
                );
                if supplied {
                    continue;
                }
                // Best effort: a default that cannot fit its own parameter is
                // the definition's mistake and is reported there, never at a
                // call that did nothing wrong.
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

    /// R matches arguments in two passes, and the order matters: an exact name
    /// claims its formal **before** any positional argument is placed, so
    /// `vapply(xs, character(1), FUN = f)` sends `character(1)` to `FUN.VALUE`
    /// rather than colliding with the named `FUN`. Positional arguments then
    /// fill the fixed parameters, then the unclaimed named formals declared
    /// before the rest parameter (all of them when the function is not
    /// variadic), in declaration order; the rest parameter absorbs whatever is
    /// left. `None` marks an argument that forwards `...`, which every caller
    /// skips.
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
        let mut next_positional = 0usize;
        let mut next_named = 0usize;
        for (index, argument) in arguments.iter().enumerate() {
            if argument.forwards_dots || targets[index].is_some() {
                continue;
            }
            targets[index] = Some(match &argument.name {
                // A named argument matching no unclaimed formal is absorbed by
                // the rest parameter (R collects unmatched keywords into
                // `...`); a name that *duplicates* a formal already supplied
                // stays an error, as does an unmatched name on a non-variadic
                // function.
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
            suggestion: crate::diagnostics::nearest_field_name(
                argument,
                parameters.iter().map(String::as_str),
            ),
            argument: argument.to_owned(),
            expected_parameters: parameters,
        }
    }

    /// The forwarding retry for a callback argument of a variadic callee.
    /// R's apply family invokes `FUN(element, ...)`, so a callback with more
    /// formals than the declared interface is still correct when the caller
    /// forwards the difference — `lapply(x, gsub, pattern = "a",
    /// replacement = "o")` calls `gsub(x[[i]], pattern = "a",
    /// replacement = "o")`, and formals the forwarding leaves unfilled may
    /// default. When the plain interface check fails, this simulates that
    /// invocation against the callback's real signature: forwarded named
    /// arguments consume same-named formals, the interface's parameter types
    /// then fill the remaining formals in order together with forwarded
    /// positionals, leftovers must be optional, and the callback's return
    /// must satisfy the interface's. Runs as a probe: bindings commit only on
    /// success, and a failed probe reports the original interface mismatch.
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
            (expected.kind(self.db).clone(), actual.kind(self.db).clone())
        else {
            return false;
        };
        let snapshot = self.table.snapshot();
        let verdict = self.forwarding_callback_probe_inner(
            &expected_callback,
            &actual_callback,
            forwarded_argument_indexes,
            arguments,
            callback_range,
        );
        if !verdict {
            self.table.rollback(snapshot);
        }
        verdict
    }

    fn forwarding_callback_probe_inner(
        &mut self,
        expected_callback: &FunctionType<'db>,
        actual_callback: &FunctionType<'db>,
        forwarded_argument_indexes: &[usize],
        arguments: &[CallArgument<'db>],
        callback_range: TextRange,
    ) -> bool {
        let mut open_positionals = actual_callback.positional.clone();
        let mut open_named = actual_callback.named.clone();
        let mut pre_rest_open = match &actual_callback.variadic {
            Some(rest) => rest.preceding_named,
            None => open_named.len(),
        };
        let actual_rest_element = actual_callback.variadic.as_ref().map(|rest| rest.element);

        // Forwarded named arguments consume the callback's same-named formals
        // first, as R matches names before positions.
        let mut forwarded_positionals = Vec::new();
        for &index in forwarded_argument_indexes {
            let argument = &arguments[index];
            match &argument.name {
                Some(name) => {
                    match open_named
                        .iter()
                        .position(|field| field.name.text(self.db) == name.as_str())
                    {
                        Some(position) => {
                            let field = open_named.remove(position);
                            if position < pre_rest_open {
                                pre_rest_open -= 1;
                            }
                            if let Some(ty) = argument.ty
                                && self
                                    .check_argument(
                                        field.ty,
                                        ty,
                                        argument.range,
                                        argument.whole_double,
                                        None,
                                    )
                                    .is_err()
                            {
                                return false;
                            }
                        }
                        None => match actual_rest_element {
                            Some(element) => {
                                if let Some(ty) = argument.ty
                                    && self
                                        .check_argument(
                                            element,
                                            ty,
                                            argument.range,
                                            argument.whole_double,
                                            None,
                                        )
                                        .is_err()
                                {
                                    return false;
                                }
                            }
                            None => return false,
                        },
                    }
                }
                None => forwarded_positionals.push(index),
            }
        }

        // The interface's parameter types are the elements the callee will
        // pass; they fill the callback's remaining formals in order, before
        // the forwarded positionals.
        enum Filled<'db> {
            Element(Ty<'db>),
            Forwarded(usize),
        }
        let sequence = expected_callback
            .positional
            .iter()
            .copied()
            .map(Filled::Element)
            .chain(
                expected_callback
                    .named
                    .iter()
                    .map(|field| Filled::Element(field.ty)),
            )
            .chain(forwarded_positionals.into_iter().map(Filled::Forwarded))
            .collect::<Vec<_>>();
        for filled in sequence {
            let (argument_ty, blame_range, whole_double) = match filled {
                Filled::Element(element) => (Some(element), callback_range, false),
                Filled::Forwarded(index) => (
                    arguments[index].ty,
                    arguments[index].range,
                    arguments[index].whole_double,
                ),
            };
            let formal = if !open_positionals.is_empty() {
                Some(open_positionals.remove(0))
            } else if pre_rest_open > 0 {
                pre_rest_open -= 1;
                Some(open_named.remove(0).ty)
            } else {
                None
            };
            let target = match formal {
                Some(formal) => formal,
                None => match actual_rest_element {
                    Some(element) => element,
                    None => return false,
                },
            };
            if let Some(argument_ty) = argument_ty
                && self
                    .check_argument(target, argument_ty, blame_range, whole_double, None)
                    .is_err()
            {
                return false;
            }
        }

        // Every formal the invocation leaves unfilled must have a default.
        if !open_positionals.is_empty() || open_named.iter().any(|field| !field.optional) {
            return false;
        }

        // The callback's result flows out through the interface's return type
        // (covariant).
        self.table
            .compatible(self.db, actual_callback.ret, expected_callback.ret)
    }

    /// One argument against one parameter type: compatibility, not
    /// unification, so parameter-position coercions (scalar-to-vector, `T`
    /// into `T | NULL`, integer widening) apply. An `Unknown` argument is
    /// accepted to avoid cascading a second error after the cause was already
    /// diagnosed where the value became `Unknown`.
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
        // R programmers write `seq_len(10)`, not `seq_len(10L)`: a
        // whole-number double literal counts as an integer at a parameter
        // position. The retry goes through full compatibility, so
        // integer-expecting unions and vector parameters admit the literal
        // too. Off during a strict overload probe — the courtesy must not
        // decide which candidate wins.
        if self.overload_probe_depth == 0
            && matches!(resolved_found.kind(self.db), TyKind::Scalar(Atomic::Double))
            && whole_double
            && self
                .table
                .compatible(self.db, scalar(self.db, Atomic::Integer), expected)
        {
            return Ok(());
        }
        // A numeric-constrained parameter rejected the argument because it is
        // not numeric; report that directly rather than rendering the bare
        // inference variable as the expected type.
        let resolved_expected = self.table.resolve(self.db, expected);
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
        // A function value rejected by an expected function type: name the
        // parameter that failed. The two whole signatures leave the reader to
        // diff them, and the residue a failed unification leaves behind can
        // describe a call that should have fit — `fn(s: U) -> U` against
        // `fn(character) -> T` looks satisfiable, because the constraint that
        // actually refuses `character` is not part of the rendered type.
        if let (TyKind::Function(found_function), TyKind::Function(expected_function)) = (
            resolved_found.kind(self.db).clone(),
            resolved_expected.kind(self.db).clone(),
        ) && let Some(mismatch) =
            self.table
                .explain_function_mismatch(self.db, &found_function, &expected_function)
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

    /// `@new Name` — nominal introduction: the value's structural type checks
    /// against the nominal's representation (binding any type-parameter
    /// arguments through compatibility, which is how a generic nominal infers
    /// its arguments from inference-variable fields), and the binding takes
    /// the nominal type.
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
        // A fully applied `@new Box<integer>` uses the given arguments; an
        // unapplied generic falls back to inferring them through the
        // representation check. Argument variables live one level up so
        // leftover (undetermined) parameters generalize into the scheme.
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
            let representation = crate::infer::apply_definition(self.db, &definition, &arguments);
            let resolved_value = self.table.resolve(self.db, value_ty);
            if !self
                .table
                .compatible(self.db, resolved_value, representation)
            {
                // The value is checked against the representation, so the
                // expected side names the shape the value must have — the
                // nominal name alone would just restate the `@new` line.
                let error = self.type_mismatch(range, representation, resolved_value, Some(value));
                self.errors.push(error);
            }
        }
        self.table.level -= 1;
        self.generalize(nominal)
    }

    /// Resolve, then project non-alias nominals to their representation —
    /// operators and indexing need a structural shape, and a nominal value is
    /// compatible with its representation. Opaque nominals (no
    /// representation) stay `Named`, but an UNDECLARED nominal — a typo the
    /// unknown-type diagnostic already reports — floors to `Unknown` so the
    /// operator checks never cascade against it. The loop bound guards
    /// recursive representations.
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

    /// `x[[i]]` / `x[i]`: the subject and every index infer first regardless
    /// of shape, so names inside an unsupported form (`m[i, j]`) still
    /// resolve and get their own diagnostics.
    fn infer_index(
        &mut self,
        id: ExprId,
        range: TextRange,
        double: bool,
        target: ExprId,
        arguments: &[Argument],
    ) -> Ty<'db> {
        let target_ty = self.infer(target);
        let mut index_types = Vec::with_capacity(arguments.len());
        for argument in arguments {
            if let Some(value) = argument.value {
                index_types.push(self.infer(value));
            }
        }
        let subject = self.structural(target_ty);
        // A declared `data.table` subject makes a single bracket
        // `[.data.table`: a query whose index arguments evaluate inside the
        // data's own frame (their reads are column references — recorded so
        // the unresolved warning skips them) and whose result CLASS the `j`
        // argument's syntax decides even with the columns unknown. Shapes the
        // classifier cannot name keep the sound-refusal Unknown.
        if !double && is_data_table(self.db, subject) {
            for argument in arguments {
                if let Some(value) = argument.value {
                    self.mask_column_reads(value);
                }
            }
            return if data_table_keeps_class(self.module, arguments) {
                subject
            } else {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                self.unknown()
            };
        }
        // A bracket the naming walk recognized as data.table syntax without
        // a data.table-typed subject evaluates its indexes in the data's
        // frame and returns a shape no base indexing rule covers — silent
        // Unknown, like the masked column reads inside it.
        if self.naming.masked_subsets.contains(&id) {
            return self.unknown();
        }
        // An Unknown/Any subject stays Unknown/Any even under an unsupported
        // index shape — the subject's own gap was already diagnosed, so
        // `m[i, j]` must not cascade an arity error. A sealed nominal
        // supports value-dependent indexing of any shape at runtime
        // (`df[rows, cols]`), none of it modeled — Unknown before the
        // index-arity check, so idiomatic two-index subsetting is no error.
        match subject.kind(self.db) {
            TyKind::Unknown => return self.unknown(),
            TyKind::Any => return crate::types::any(self.db),
            // A subject whose shape the author never wrote down — a sealed
            // nominal (`df[rows, cols]`), or a parameter still undetermined
            // (`function(m, i, j) m[i, j]`) — supports value-dependent
            // indexing of any shape at run time, none of it modelled. It is
            // sound-by-refusal before the index-arity check below, so
            // idiomatic two-index subsetting on it is not an error: the whole
            // point of `function(m, i, j) m[i, j]` is that the caller knows
            // the shape and the callee does not.
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
                return self.unknown();
            }
            _ => {}
        }
        // Empty index slots do not count (`m[, i]` and `m[k, ]` each have ONE
        // index); named arguments do (`m[k, , drop = FALSE]` indexes with 2).
        // A single index among several slots is matrix-style selection —
        // unmodeled, so it refuses silently (a strict origin) rather than
        // erroring on one of R's most idiomatic forms.
        let filled = arguments
            .iter()
            .filter(|argument| argument.value.is_some())
            .count();
        if filled != 1 {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::UnsupportedIndexShape {
                    index_count: filled,
                },
            });
            return self.unknown();
        }
        if arguments.len() != 1 {
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return self.unknown();
        }
        if arguments[0].name.is_some() {
            self.errors.push(TypeError {
                range,
                kind: TypeErrorKind::UnsupportedIndexShape { index_count: 1 },
            });
            return self.unknown();
        }
        let result = if double {
            let index = arguments[0]
                .value
                .map(|value| self.module.expression(value).kind.clone());
            // A finding about the KEY points at the key, not at the whole
            // access chain, which contains the subject too.
            let key_range = arguments[0]
                .value
                .map_or(range, |value| self.blame_range(value));
            self.extract_result(id, range, key_range, subject, index.as_ref())
        } else {
            self.subset_result(id, range, subject, index_types.first().copied())
        };
        match result {
            Ok(ty) => ty,
            Err(error) => {
                self.errors.push(error);
                self.unknown()
            }
        }
    }

    /// Records every read under a data.table bracket's index argument as a
    /// column reference. Nested function bodies are included: a closure
    /// written inside `j` is created in the data's frame, so its free names
    /// also fall back to columns.
    fn mask_column_reads(&mut self, id: ExprId) {
        let kind = &self.module.expression(id).kind;
        if matches!(kind, ExpressionKind::NameRef(_)) {
            self.masked_reads.insert(id);
        }
        for &child in kind.child_ids().iter() {
            self.mask_column_reads(child);
        }
    }

    /// `[[` — single-element extraction.
    fn extract_result(
        &mut self,
        origin: ExprId,
        range: TextRange,
        key_range: TextRange,
        subject: Ty<'db>,
        index: Option<&ExpressionKind>,
    ) -> Result<Ty<'db>, TypeError<'db>> {
        // Member-wise over a union subject: `[[` must be valid on every shape
        // the subject can take, and the element's type is the join of the
        // per-member results; a failing member reports the full union.
        if let TyKind::Union(members) = subject.kind(self.db).clone() {
            let mut results = Vec::with_capacity(members.len());
            for member in members {
                let result = self
                    .extract_result(origin, range, key_range, member, index)
                    .map_err(|error| widen_error_container(error, subject))?;
                results.push(result);
            }
            return Ok(union_of(self.db, results));
        }
        let literal_position = index.and_then(crate::hir::integer_literal_position);
        let literal_name = match index {
            Some(ExpressionKind::Literal(LiteralKind::String(name))) => Some(name.clone()),
            _ => None,
        };
        match subject.kind(self.db).clone() {
            TyKind::Unknown => Ok(self.unknown()),
            TyKind::Any => Ok(crate::types::any(self.db)),
            TyKind::Scalar(atomic) => Ok(scalar(self.db, atomic)),
            TyKind::Vector(element) | TyKind::List(element) => Ok(element),
            // A literal name may be absent at runtime (`T | NULL`), while
            // positional and computed access extract an element like R does
            // on any vector or list.
            TyKind::NamedVector(element) | TyKind::NamedList(element) => {
                Ok(if literal_name.is_some() {
                    union_of(self.db, [element, crate::types::null(self.db)])
                } else {
                    element
                })
            }
            // A computed position could reach any item, so the extraction is
            // the union of the item types; only a *literal* position is
            // precise (and still errors when out of range).
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
                None => Ok(union_of(self.db, items)),
            },
            TyKind::Record(fields) => {
                // Record fields are declaration-ordered, so a positional `[[`
                // extracts the field at that position exactly like a tuple
                // item.
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
                    // A computed name could reach any field — the
                    // dispatch-table idiom, `handlers[[name]](...)`.
                    None => Ok(union_of(self.db, fields.iter().map(|field| field.ty))),
                    Some(name) => {
                        match fields.iter().find(|field| field.name.text(self.db) == name) {
                            Some(field) => Ok(field.ty),
                            None => {
                                let suggestion = crate::diagnostics::nearest_field_name(
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
            // A sealed nominal and an unresolved inference variable both
            // support element access the system cannot model — sound-by-
            // refusal Unknown, never a rejection (idiomatic R walks generic
            // data this way: `function(x) x[[1L]]`). The variable stays
            // unconstrained; the refusal is a strict origin.
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(self.unknown())
            }
            _ => Err(TypeError {
                range,
                kind: TypeErrorKind::NotAList { found: subject },
            }),
        }
    }

    /// `[` — the list slice and the vector subset.
    fn subset_result(
        &mut self,
        origin: ExprId,
        range: TextRange,
        subject: Ty<'db>,
        index: Option<Ty<'db>>,
    ) -> Result<Ty<'db>, TypeError<'db>> {
        // Member-wise over a union subject, like `[[`.
        if let TyKind::Union(members) = subject.kind(self.db).clone() {
            let mut results = Vec::with_capacity(members.len());
            for member in members {
                let result = self
                    .subset_result(origin, range, member, index)
                    .map_err(|error| widen_error_container(error, subject))?;
                results.push(result);
            }
            return Ok(union_of(self.db, results));
        }
        match subject.kind(self.db).clone() {
            TyKind::Unknown => Ok(self.unknown()),
            TyKind::Any => Ok(crate::types::any(self.db)),
            // Vector subjects: the index's shape decides between one
            // position (the scalar-claim element) and a sub-vector; `[`
            // keeps map-likeness, unlike arithmetic.
            TyKind::Scalar(_) => match self.vector_index_shape(range, index)? {
                VectorIndexShape::ScalarLike => Ok(subject),
                VectorIndexShape::VectorLike => Ok(Ty::new(self.db, TyKind::Vector(subject))),
            },
            TyKind::Vector(element) => match self.vector_index_shape(range, index)? {
                VectorIndexShape::ScalarLike => Ok(element),
                VectorIndexShape::VectorLike => Ok(subject),
            },
            TyKind::NamedVector(element) => match self.vector_index_shape(range, index)? {
                VectorIndexShape::ScalarLike => Ok(element),
                VectorIndexShape::VectorLike => Ok(subject),
            },
            TyKind::List(_) | TyKind::NamedList(_) => Ok(subject),
            // A `[` slice of a fixed-shape list is a sub-list that can
            // contain any of the item types, so the element type is their
            // union (collapsing back for a homogeneous list; slicing the
            // empty list yields `list[NULL]`).
            TyKind::Tuple(items) => Ok(Ty::new(self.db, TyKind::List(union_of(self.db, items)))),
            TyKind::Record(fields) => Ok(Ty::new(
                self.db,
                TyKind::NamedList(union_of(self.db, fields.iter().map(|field| field.ty))),
            )),
            // Sound-by-refusal, as for `[[`.
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(self.unknown())
            }
            _ => Err(TypeError {
                range,
                kind: TypeErrorKind::UnsupportedSubset { found: subject },
            }),
        }
    }

    /// How a `[` index selects from a vector: one position (a scalar-like
    /// numeric or character index — a deliberate scalar claim, see the
    /// typing reference) or many (vector-like and logical-mask indexes,
    /// `NULL`). Undetermined shapes (inference variables, opaque nominals,
    /// `Unknown`, `Any`) claim scalar and stay unconstrained; non-vector
    /// indexes are type errors.
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
        match index.kind(self.db).clone() {
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
                for member in members {
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
            | TyKind::Function(_) => Err(TypeError {
                range,
                kind: TypeErrorKind::BadVectorIndex { index },
            }),
        }
    }

    /// `x$name` behaves as `[["name"]]` on lists and records — but not on
    /// atomic vectors, which R rejects outright. `x@name` (S4 slot access) is
    /// not modeled: sound-by-refusal Unknown.
    fn infer_field(
        &mut self,
        id: ExprId,
        range: TextRange,
        name_range: TextRange,
        at: bool,
        target: ExprId,
        name: Option<String>,
    ) -> Ty<'db> {
        let target_ty = self.infer(target);
        if at {
            self.record_strict_origin(id, StrictOriginKind::UnsupportedConstruct);
            return self.unknown();
        }
        let Some(name) = name else {
            return self.unknown();
        };
        let subject = self.structural(target_ty);
        match self.dollar_result(id, range, name_range, subject, &name) {
            Ok(ty) => ty,
            Err(error) => {
                self.errors.push(error);
                self.unknown()
            }
        }
    }

    fn dollar_result(
        &mut self,
        origin: ExprId,
        range: TextRange,
        // The field name's own range: a finding about the field points there,
        // not at the whole access chain, which contains the subject too.
        name_range: TextRange,
        subject: Ty<'db>,
        name: &str,
    ) -> Result<Ty<'db>, TypeError<'db>> {
        // Member-wise over a union subject. A shape that lacks the field is
        // not an error on its own: R answers `NULL` for a name a list does
        // not carry, which is exactly what the accumulator idiom relies on
        // (`args <- list(); if (flag) args$escape <- TRUE; args$escape`). So a
        // missing field contributes `NULL` and the read is nullable — while a
        // field no shape carries at all stays an error, because that is a
        // typo rather than an absence the program is prepared for. A
        // *structural* refusal (`$` on an atomic vector) is a hard error from
        // any member: no shape makes it legal.
        if let TyKind::Union(members) = subject.kind(self.db).clone() {
            let mut results = Vec::with_capacity(members.len());
            let mut absent: Option<TypeError<'db>> = None;
            for member in members {
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
                    // The failing member may be the one with no fields at all,
                    // so a suggestion drawn from it alone would be empty.
                    // Draw it from every field the union can carry instead.
                    if let TypeErrorKind::FieldDoesNotExist { suggestion, .. } = &mut error.kind {
                        *suggestion = crate::diagnostics::nearest_field_name(
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
                        results.push(crate::types::null(self.db));
                    }
                    Ok(union_of(self.db, results))
                }
            };
        }
        match subject.kind(self.db).clone() {
            TyKind::Unknown => Ok(self.unknown()),
            TyKind::Any => Ok(crate::types::any(self.db)),
            // R rejects `$` on every atomic vector ("$ operator is invalid
            // for atomic vectors"), named ones included — element extraction
            // is `[[`'s job.
            TyKind::Scalar(_) | TyKind::Vector(_) | TyKind::NamedVector(_) => Err(TypeError {
                range,
                kind: TypeErrorKind::DollarOnAtomicVector { found: subject },
            }),
            TyKind::NamedList(element) => {
                Ok(union_of(self.db, [element, crate::types::null(self.db)]))
            }
            TyKind::Record(fields) => {
                match fields.iter().find(|field| field.name.text(self.db) == name) {
                    Some(field) => Ok(field.ty),
                    None => Err(TypeError {
                        range: name_range,
                        kind: TypeErrorKind::FieldDoesNotExist {
                            suggestion: crate::diagnostics::nearest_field_name(
                                name,
                                fields.iter().map(|field| field.name.text(self.db)),
                            ),
                            field: name.to_owned(),
                            container: subject,
                        },
                    }),
                }
            }
            TyKind::Tuple(_) | TyKind::List(_) => Err(TypeError {
                range: name_range,
                kind: TypeErrorKind::FieldDoesNotExist {
                    suggestion: None,
                    field: name.to_owned(),
                    container: subject,
                },
            }),
            // Sound-by-refusal for sealed nominals (`df$col` is the most
            // idiomatic R there is) and unresolved variables
            // (`function(node) node$value`).
            TyKind::Named(..) | TyKind::Var(_) | TyKind::Rigid(_) => {
                self.record_strict_origin(origin, StrictOriginKind::UnsupportedConstruct);
                Ok(self.unknown())
            }
            _ => Err(TypeError {
                range,
                kind: TypeErrorKind::NotAList { found: subject },
            }),
        }
    }

    /// Branch-merge join: unify when possible (keeps the chooser idiom linking
    /// two inference variables), otherwise the union of the branch types; a
    /// NULL branch joins by pure union so it never binds a variable to NULL.
    /// The value of an `if`/`else` whose branches both fall through. Branches
    /// with genuinely different types produce their UNION — never a
    /// unification — whenever either side still carries an unresolved
    /// inference variable. Unifying there would let the concrete branch pin
    /// the other: `function(flag, x) if (flag) x else "s"` would silently
    /// become `fn(flag, x: character)`, so `f(TRUE, 1)` failed and the error
    /// blamed the *caller* for a line that is not wrong. It is also what the
    /// guard rule requires — an unannotated parameter is not pinned by the
    /// guard that tests it, which is the whole point of
    /// `if (is.character(x)) x else "other"`.
    ///
    /// Two branches that are BOTH still open do tie to each other, because
    /// neither pins the other and the tie is what makes the coalesce idiom
    /// `if (is.null(value)) fallback else value` infer
    /// `<T> fn(value: T | NULL, fallback: T) -> T`. Two concrete branches go
    /// through `join_types`, where identical types collapse instead of forming
    /// a one-member union.
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
            // Only an UNCONSTRAINED variable is protected. One the body has
            // already restricted — `n * fact(n - 1L)` demands numeric — may
            // unify with the other branch, because that pin adds nothing the
            // program did not already require and it is what lets recursion
            // converge to a precise type. An unconstrained variable is a
            // parameter the body only passes through, so pinning it would
            // invent a requirement the code never expressed.
            if self.table.open_constraint(self.db, open) == Some(Constraint::Unconstrained) {
                return union_of(self.db, [left_resolved, right_resolved]);
            }
        }
        self.join_types(left, right)
    }

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

    /// Merge branch writes with the pre-branch state: an entry written in one
    /// branch joins with its other-path value (or stays, optimistically, when
    /// the other path had none).
    fn join_writes(&mut self, writes: Vec<(BindingId, Option<EnvEntry<'db>>)>) {
        self.join_writes_reporting(&writes);
    }

    /// One slot entry as a monotype, for joining paths that disagree about
    /// what the slot holds. A scheme instantiates: its quantified variables
    /// become fresh ones, which the export edge erases like any other residual.
    fn entry_monotype(&mut self, entry: EnvEntry<'db>) -> Ty<'db> {
        match entry {
            EnvEntry::Mono(ty) | EnvEntry::MissingFormal(ty) => ty,
            EnvEntry::Scheme(index) => {
                let scheme = self.scheme_arena[index as usize].clone();
                self.instantiate(&scheme)
            }
        }
    }

    /// Like `join_writes`, reporting the slots whose entries actually changed
    /// (the loop fixed point's stability signal).
    fn join_writes_reporting(
        &mut self,
        writes: &[(BindingId, Option<EnvEntry<'db>>)],
    ) -> Vec<BindingId> {
        let mut changed = Vec::new();
        for &(slot, branch_entry) in writes {
            // The missing-formal marker is branch-local: joined back into
            // the fall-through state it means only "possibly missing", which
            // reads as the ordinary supplied type.
            let branch_entry = match branch_entry {
                Some(EnvEntry::MissingFormal(ty)) => Some(EnvEntry::Mono(ty)),
                other => other,
            };
            let current = self.environment.get(slot);
            let joined = match (current, branch_entry) {
                (Some(EnvEntry::Mono(a)), Some(EnvEntry::Mono(b))) if a != b => {
                    Some(EnvEntry::Mono(self.join_types(a, b)))
                }
                (None, Some(entry)) => Some(entry),
                // Identical entries on both paths: nothing joins, and keeping
                // the entry as-is preserves a function slot's polymorphism.
                (Some(a), Some(b)) if a == b => Some(a),
                // A function slot conditionally rewritten. The scheme has to
                // collapse to a monotype union of what each path holds: a slot
                // that may hold either of two different functions is not
                // polymorphic, and its honest type is the union. Taking one
                // side — which this did in both directions, a branch scheme
                // replacing whatever preceded it and a monotype replacing a
                // scheme — invented a type belonging to neither path, so the
                // slot claimed a contract the other path does not satisfy and
                // reads answered from it in both directions.
                (Some(a), Some(b)) => {
                    // Union rather than `join_types`, which unifies first and
                    // only unions when unification fails. Two instantiated
                    // schemes are always unifiable through their fresh
                    // variables — `fn(x: T) -> T` unifies with
                    // `fn(x: U) -> character` by binding `T := character` — and
                    // the merged signature describes neither path while linking
                    // variables that instantiation made independent precisely
                    // so they would stay separate.
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

    // ---- schemes ----

    fn schemes(&self) -> &Vec<TypeScheme<'db>> {
        &self.scheme_arena
    }

    fn push_scheme(&mut self, scheme: TypeScheme<'db>) -> u32 {
        self.scheme_arena.push(scheme);
        (self.scheme_arena.len() - 1) as u32
    }

    /// Generalize: quantify unbound variables ABOVE the current level.
    fn generalize(&mut self, ty: Ty<'db>) -> TypeScheme<'db> {
        let resolved = self.table.resolve(self.db, ty);
        let mut binders = Vec::new();
        let mut mapping: FxHashMap<crate::types::InferenceVar, Name<'db>> = FxHashMap::default();
        let body = self.abstract_vars(resolved, &mut binders, &mut mapping);
        TypeScheme { binders, body }
    }

    fn abstract_vars(
        &mut self,
        ty: Ty<'db>,
        binders: &mut Vec<(Name<'db>, Constraint)>,
        mapping: &mut FxHashMap<crate::types::InferenceVar, Name<'db>>,
    ) -> Ty<'db> {
        let TyKind::Var(var) = *ty.kind(self.db) else {
            return crate::types::map_child_types(self.db, ty, &mut |child| {
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

    /// Instantiate a scheme with fresh variables carrying its constraints.
    fn instantiate(&mut self, scheme: &TypeScheme<'db>) -> Ty<'db> {
        let mut substitution: FxHashMap<Name<'db>, Ty<'db>> = FxHashMap::default();
        for (name, constraint) in &scheme.binders {
            let fresh = self.fresh(*constraint);
            substitution.insert(*name, fresh);
        }
        crate::types::substitute_rigid(self.db, scheme.body, &substitution)
    }
}

/// The statically known element atomic of a `c(...)` operand: a scalar or a
/// vector with a concrete scalar element; anything else cannot combine.
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

/// R's atomic coercion hierarchy for `c(...)`: logical < integer < double <
/// complex < character (`c(1L, NA)` is `integer`, `c(1L, "a")` is
/// `character`); `raw` does not participate and only combines with itself.
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

/// A failing union member reports the full union — the subject's actual type —
/// not the single member that failed.
/// Every field name any member of a union subject carries, for the
/// "did you mean" of a field no member carries.
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

/// Whether an index subject resolved to the `data.table` nominal, wherever
/// it was declared (the shipped conditional stub or a project `@type`).
fn is_data_table(db: &dyn Db, subject: Ty<'_>) -> bool {
    matches!(subject.kind(db), TyKind::Named(name, _) if name.text(db) == "data.table")
}

/// Whether a `[.data.table` query's result keeps the subject's class, decided
/// purely by the bracket's argument syntax. `j` is the second positional slot
/// (or a `j =` named argument); the class survives when `j` is absent or an
/// empty slot (row filtering, joins), when a `by =`/`keyby =` grouping is
/// present (grouped results always assemble into a table), or when `j` is a
/// `:=` column assignment (the subject returned invisibly) or a
/// `.()`/`list()` select. Every other `j` — a bare column, a computed value,
/// `with =` forms — has a result shape only column knowledge could name.
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

/// The first alias whose expansion re-enters itself while lowering `ty`, if
/// any. A proper DFS gray set: a name is "expanding" only while its own body
/// is walked, so a diamond (two sibling mentions of one alias) is not a
/// cycle. Nominal (`@type`) definitions may be legitimately recursive and are
/// not expanded.
fn find_alias_cycle<'db>(
    db: &'db dyn Db,
    definitions: Option<&FxHashMap<Name<'db>, crate::annotations::NamedDefinition<'db>>>,
    ty: Ty<'db>,
) -> Option<String> {
    // No declarations, no alias to cycle through.
    let definitions = definitions?;
    let mut expanding = rustc_hash::FxHashSet::default();
    alias_cycle_walk(db, definitions, ty, &mut expanding)
}

fn alias_cycle_walk<'db>(
    db: &'db dyn Db,
    definitions: &FxHashMap<Name<'db>, crate::annotations::NamedDefinition<'db>>,
    ty: Ty<'db>,
    expanding: &mut rustc_hash::FxHashSet<Name<'db>>,
) -> Option<String> {
    let walk_all = |types: &[Ty<'db>], expanding: &mut rustc_hash::FxHashSet<Name<'db>>| {
        types
            .iter()
            .find_map(|&inner| alias_cycle_walk(db, definitions, inner, expanding))
    };
    match ty.kind(db) {
        TyKind::Named(name, arguments) => {
            if let Some(found) = walk_all(arguments, expanding) {
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
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner) => alias_cycle_walk(db, definitions, *inner, expanding),
        TyKind::Tuple(members) | TyKind::Union(members) => walk_all(members, expanding),
        TyKind::Record(fields) => fields
            .iter()
            .find_map(|field| alias_cycle_walk(db, definitions, field.ty, expanding)),
        TyKind::Function(function) => {
            if let Some(found) = walk_all(&function.positional, expanding) {
                return Some(found);
            }
            if let Some(found) = function
                .named
                .iter()
                .find_map(|field| alias_cycle_walk(db, definitions, field.ty, expanding))
            {
                return Some(found);
            }
            if let Some(rest) = &function.variadic
                && let Some(found) = alias_cycle_walk(db, definitions, rest.element, expanding)
            {
                return Some(found);
            }
            alias_cycle_walk(db, definitions, function.ret, expanding)
        }
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::Var(_)
        | TyKind::Rigid(_) => None,
    }
}

/// The name a `pkg::name` / `pkg:::name` read may resolve under: the stub
/// corpus must know the namespace, and `::` additionally requires the name to
/// be declared there (`:::` reaches unexported names). With no stub corpus
/// installed validation is impossible, so every qualified read resolves.
fn validated_namespace_name(
    db: &dyn Db,
    internal: bool,
    package: &Option<String>,
    name: &Option<String>,
) -> Option<String> {
    let name = name.clone()?;
    let Some(package) = package else {
        return Some(name);
    };
    // The project's own package resolves through the global environment, which
    // already holds its definitions — and wins over a stub namespace of the
    // same name, exactly as a package binding shadows a stub name.
    if crate::metadata::is_own_package(db, package) {
        return Some(name);
    }
    match crate::stubs::namespace_known(db, package) {
        None => Some(name),
        Some(false) => None,
        Some(true) => {
            if internal || crate::stubs::namespace_exports(db, package, &name) {
                Some(name)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootDatabase;
    use crate::hir::lower_item;
    use crate::naming::resolve_item;

    fn check_source<'db>(db: &'db RootDatabase, source: &str) -> ItemCheck<'db> {
        let parse = syntax::parse(source);
        let root = parse.syntax_node();
        let item = root.children().next().expect("one top-level item");
        let module = lower_item(&item);
        let naming = resolve_item(&module);
        check_item(db, &module, &naming)
    }

    #[test]
    fn arithmetic_generalizes_with_numeric_constraint() {
        let db = RootDatabase::default();
        let check = check_source(&db, "add <- function(x) x + 1L");
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let scheme = check.scheme.expect("definition scheme");
        // `<T: numeric> fn(x: T) -> T`.
        assert_eq!(scheme.binders.len(), 1);
        assert_eq!(scheme.binders[0].1, Constraint::Numeric);
        let TyKind::Function(function) = scheme.body.kind(&db) else {
            panic!("expected a function scheme");
        };
        // Formals carry their names (R matches by name and position).
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
        // NULL branch joins by union, never binding the variable.
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
        // The idiomatic accumulator is clean and stays integer.
        let accumulator = check_source(
            &db,
            "f <- function() {\n  total <- 0L\n  for (i in 1:3) {\n    total <- total + i\n  }\n  total\n}",
        );
        assert!(accumulator.errors.is_empty(), "{:?}", accumulator.errors);
        assert_eq!(scheme_ret(&db, &accumulator), scalar(&db, Atomic::Integer));
        // A heterogeneous fixed-shape list binds the union of item types.
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
        // A function is not iterable.
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
        // A known-field write replaces the field's type.
        let replaced = check_source(
            &db,
            "f <- function() {\n  p <- list(a = 1L)\n  p$a <- \"s\"\n  p$a\n}",
        );
        assert!(replaced.errors.is_empty(), "{:?}", replaced.errors);
        assert_eq!(scheme_ret(&db, &replaced), scalar(&db, Atomic::Character));
        // A fresh field is added; an empty list() starts a record.
        let added = check_source(
            &db,
            "f <- function() {\n  p <- list()\n  p$b <- TRUE\n  p$b\n}",
        );
        assert!(added.errors.is_empty(), "{:?}", added.errors);
        assert_eq!(scheme_ret(&db, &added), scalar(&db, Atomic::Logical));
        // A computed-key write turns an empty list map-like.
        let computed = check_source(
            &db,
            "f <- function(key) {\n  m <- list()\n  m[[key]] <- 1L\n  m[[key]]\n}",
        );
        assert!(computed.errors.is_empty(), "{:?}", computed.errors);
        assert_eq!(scheme_ret(&db, &computed), scalar(&db, Atomic::Integer));
        // A replacement-function call keeps the base's type.
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
        // The value: `if (c) return(NULL) else 5` is `double`, not
        // `NULL | double`.
        let value = check_source(
            &db,
            "f <- function(c) {\n  x <- if (c) return(NULL) else 5\n  x + 1\n}",
        );
        assert!(value.errors.is_empty(), "{:?}", value.errors);
        // The state: a write inside a diverging branch does not join, so the
        // arithmetic below stays clean.
        let state = check_source(
            &db,
            "f <- function(flag) {\n  x <- 1L\n  if (flag) {\n    x <- \"s\"\n    return(x)\n  }\n  x + 1L\n}",
        );
        assert!(state.errors.is_empty(), "{:?}", state.errors);
    }

    #[test]
    fn null_guard_narrows_the_early_exit_idiom() {
        let db = RootDatabase::default();
        let check = check_annotated(
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
        // `<T> fn(value: T | NULL, fallback: T) -> T`.
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
        assert!(members.contains(&crate::types::null(&db)));
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
        // `<T: numeric> fn(a: T, b: T) -> T`: one binder shared by all three.
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
        // integer[] + integer stays integer[].
        let vector = check_source(&db, "f <- function() c(1L, 2L) + 1L");
        assert!(vector.errors.is_empty(), "{:?}", vector.errors);
        assert!(matches!(
            scheme_ret(&db, &vector).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Integer)
        ));
        // integer[] / integer promotes to double[].
        let divided = check_source(&db, "f <- function() c(1L, 2L) / 2L");
        assert!(divided.errors.is_empty(), "{:?}", divided.errors);
        assert!(matches!(
            scheme_ret(&db, &divided).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Double)
        ));
        // `%%` is arithmetic, not an opaque special.
        let modulo = check_source(&db, "f <- function() 7L %% 2L");
        assert!(modulo.errors.is_empty(), "{:?}", modulo.errors);
        assert_eq!(scheme_ret(&db, &modulo), scalar(&db, Atomic::Integer));
        // A non-numeric operand reports the operand, not the whole range.
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
        // A flexible endpoint may resolve to double, so the claim is double[].
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
        // A flexible operand against a numeric partner is constrained.
        let flexible = check_source(&db, "positive <- function(x) x > 0L");
        assert!(flexible.errors.is_empty(), "{:?}", flexible.errors);
        let scheme = flexible.scheme.clone().expect("scheme");
        assert_eq!(scheme.binders.len(), 1);
        assert_eq!(scheme.binders[0].1, Constraint::Numeric);
        assert_eq!(scheme_ret(&db, &flexible), scalar(&db, Atomic::Logical));
        // A vector member compares element-wise.
        let vector = check_source(&db, "f <- function() c(1L, 2L) > 1L");
        assert!(vector.errors.is_empty(), "{:?}", vector.errors);
        assert!(matches!(
            scheme_ret(&db, &vector).kind(&db),
            TyKind::Vector(element) if *element == scalar(&db, Atomic::Logical)
        ));
        // Cross-family comparison is a mismatch.
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
        // R coerces a numeric condition (zero false, anything else true), so
        // the `if (length(x))` idiom is not a mistake.
        for source in [
            "f <- function() if (1L) 2L",
            "f <- function() 1L && TRUE",
            "f <- function(x) if (length(x)) 1L else 2L",
            "f <- function() if (2.5) 1L else 2L",
        ] {
            let ok = check_source(&db, source);
            assert!(ok.errors.is_empty(), "{source}: {:?}", ok.errors);
        }
        // A character condition is a runtime error in R for every string but
        // the spellings of TRUE and FALSE, so it stays a type error.
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
        // A defaulted formal may stay unfilled.
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
        // Literal position on a tuple-like list.
        let tuple = check_source(
            &db,
            "f <- function() {\n  x <- list(1L, \"a\")\n  x[[1L]]\n}",
        );
        assert!(tuple.errors.is_empty(), "{:?}", tuple.errors);
        assert_eq!(scheme_ret(&db, &tuple), scalar(&db, Atomic::Integer));
        // Literal field via `$` on a record-like list.
        let record = check_source(
            &db,
            "f <- function() {\n  p <- list(a = 1L, b = \"s\")\n  p$b\n}",
        );
        assert!(record.errors.is_empty(), "{:?}", record.errors);
        assert_eq!(scheme_ret(&db, &record), scalar(&db, Atomic::Character));
        // A computed index is the union of the item types.
        let computed = check_source(
            &db,
            "f <- function(i) {\n  x <- list(1L, \"a\")\n  x[[i]]\n}",
        );
        assert!(computed.errors.is_empty(), "{:?}", computed.errors);
        assert!(matches!(
            scheme_ret(&db, &computed).kind(&db),
            TyKind::Union(members) if members.len() == 2
        ));
        // `[` slices into a sub-list of the union.
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
        // R rejects `$` on atomic vectors, named ones included.
        let dollar_atomic = check_source(&db, "f <- function() {\n  v <- c(a = 1L)\n  v$a\n}");
        assert!(
            dollar_atomic
                .errors
                .iter()
                .any(|error| matches!(error.kind, TypeErrorKind::DollarOnAtomicVector { .. })),
            "{:?}",
            dollar_atomic.errors
        );
        // Multi-index forms are not modeled.
        let matrix = check_source(&db, "f <- function() {\n  x <- list(1L)\n  x[1L, 2L]\n}");
        assert!(
            matrix.errors.iter().any(|error| matches!(
                error.kind,
                TypeErrorKind::UnsupportedIndexShape { index_count: 2 }
            )),
            "{:?}",
            matrix.errors
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
        assert!(members.contains(&crate::types::null(&db)));
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
        // The dispatch-table idiom: the value could be either function, so
        // the call must be valid for both and types as the union of returns.
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
        // The beta-semantics soundness case: a branch write must be visible
        // after the construct as a union.
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

    fn check_annotated<'db>(db: &'db RootDatabase, source: &str) -> ItemCheck<'db> {
        let parse = syntax::parse(source);
        let root = parse.syntax_node();
        let annotation = root
            .children()
            .find(|child| child.kind() == syntax::SyntaxKind::ANNOTATION)
            .map(|node| crate::annotations::lower_annotation(db, &node));
        let item = root
            .children()
            .find(|child| syntax::ast::is_expression_kind(child.kind()))
            .expect("one top-level item");
        let module = lower_item(&item);
        let naming = resolve_item(&module);
        check_item_with_annotation(db, &module, &naming, annotation.as_ref(), &[], None)
    }

    #[test]
    fn annotated_identity_keeps_declared_scheme() {
        let db = RootDatabase::default();
        let check = check_annotated(&db, "#: <T> fn(x: T) -> T\nid <- function(x) x");
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
        let check = check_annotated(&db, "#: fn(x: integer) -> character\nf <- function(x) x");
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
        let check = check_annotated(&db, "#: fn(x: integer) -> integer\nf <- function(y) 1L");
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
        // The body adds an arithmetic bound the annotation never declared.
        let bad = check_annotated(&db, "#: <T> fn(x: T) -> T\nbad <- function(x) x + 1L");
        assert!(
            !bad.errors.is_empty(),
            "plain <T> must refuse arithmetic, got no errors"
        );
        // With the declared numeric constraint the same body is fine.
        let ok = check_annotated(
            &db,
            "#: <T: numeric> fn(x: T) -> T\nok <- function(x) x + 1L",
        );
        assert!(ok.errors.is_empty(), "{:?}", ok.errors);
    }

    #[test]
    fn expanded_param_return_form_checks() {
        let db = RootDatabase::default();
        let check = check_annotated(
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
        let check = check_annotated(
            &db,
            "#: @trust fn(x: integer) -> character\nf <- function(x) x",
        );
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        assert!(check.scheme.is_some());
    }
}
