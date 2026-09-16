//! The interned type representation.
//!
//! Types are salsa-interned: a `Ty` is a copyable id, equality is id
//! comparison, and no deep clones exist anywhere in inference — the structural
//! churn that capped the legacy checker is designed out from the start.
//! Inference variables are ordinary interned types (`TyKind::Var`), so there is
//! exactly one type representation end to end.
//!
//! Unions are normalized in exactly one place, `union_of`: flatten, structural
//! dedupe keeping first occurrence, `NULL` ordered last, `Any` then `Unknown`
//! absorb the union, a singleton unwraps, and empty collapses to `NULL`. Never
//! construct `TyKind::Union` directly.

use crate::Db;

/// An interned identifier (variable names, parameter names, type names).
#[salsa::interned(debug)]
pub struct Name<'db> {
    #[returns(deref)]
    pub text: String,
}

/// An interned type: a cheap copyable id; equality is id equality.
#[salsa::interned(debug)]
pub struct Ty<'db> {
    #[returns(ref)]
    pub kind: TyKind<'db>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum TyKind<'db> {
    /// The sanctioned escape hatch: compatible with everything, silently.
    Any,
    /// An absent or unmodeled fact; strict mode reports its origins.
    Unknown,
    Null,
    Scalar(Atomic),
    /// `T[]` — an atomic vector with element type `T`.
    Vector(Ty<'db>),
    /// `T[named]`.
    NamedVector(Ty<'db>),
    /// `list[T]`.
    List(Ty<'db>),
    /// `list[named: T]`.
    NamedList(Ty<'db>),
    /// `list{A, B}`.
    Tuple(Vec<Ty<'db>>),
    /// `list{a: A, b: B}` — fields keep declaration order.
    Record(Vec<RecordField<'db>>),
    Function(FunctionType<'db>),
    /// A normalized union; built only through `union_of`.
    Union(Vec<Ty<'db>>),
    /// A named (nominal or alias-expanded) type with its arguments.
    Named(Name<'db>, Vec<Ty<'db>>),
    /// A unification variable (inference-scoped; resolved through the
    /// inference table, never stored in exported schemes).
    Var(InferenceVar),
    /// A rigid (skolem) variable from an explicit `<T>` binder: refuses to
    /// bind while its scope is checked, generalizes back out afterwards.
    Rigid(Name<'db>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum Atomic {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, salsa::SalsaValue)]
pub struct InferenceVar(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RecordField<'db> {
    pub name: Name<'db>,
    pub ty: Ty<'db>,
    pub optional: bool,
}

/// One named parameter of a function type.
///
/// Separate from [`RecordField`] because a parameter carries something a record
/// field cannot: the type of its **default**. R evaluates the default in the
/// function's own frame when the caller omits the argument, so that type is
/// what the parameter holds on the omitted path — without it a call that leaves
/// the argument out has nothing to instantiate the parameter with and the
/// result degrades to `Any`, silencing everything downstream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Parameter<'db> {
    pub name: Name<'db>,
    pub ty: Ty<'db>,
    /// Callers may omit the argument: the formal has a default, or the body
    /// tests it with `missing()`.
    pub optional: bool,
    /// The default's type, when the formal has one that could be inferred.
    /// `None` for a required parameter, for `missing()`-style optionality with
    /// no default, and for an annotation that declares `[name]:` without the
    /// definition being in view.
    pub default: Option<Ty<'db>>,
}

impl<'db> Parameter<'db> {
    /// Every type this parameter carries. Traversals go through here so a
    /// variable hiding in a default cannot be missed by one walk and found by
    /// another — an occurs check or a level adjustment that skipped it would be
    /// unsound, not merely imprecise.
    pub fn types(&self) -> impl Iterator<Item = Ty<'db>> + '_ {
        std::iter::once(self.ty).chain(self.default)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct FunctionType<'db> {
    /// Positional parameters, in order.
    pub positional: Vec<Ty<'db>>,
    /// Named parameters (declaration order; `optional` marks `[name]:`).
    pub named: Vec<Parameter<'db>>,
    /// The `...` rest parameter's element type, with the count of named
    /// parameters written before it (parameters after `...` match by name
    /// only).
    pub variadic: Option<RestParameter<'db>>,
    pub ret: Ty<'db>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RestParameter<'db> {
    pub element: Ty<'db>,
    pub preceding_named: usize,
}

/// The constraint lattice on inference variables and binders. `Numeric` and
/// `AtomicElement` join to `ScalarNumeric` (a scalar integer/double).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, salsa::SalsaValue)]
pub enum Constraint {
    #[default]
    Unconstrained,
    Numeric,
    AtomicElement,
    ScalarNumeric,
}

impl Constraint {
    /// How this bound is written in a `#:` binder list, and read back out of
    /// one. Rendering and parsing share the table so a constraint the checker
    /// prints is always a constraint a user can write — a finding's type is
    /// meant to be copyable straight into an annotation.
    pub const SPELLINGS: [(&'static str, Constraint); 3] = [
        ("numeric", Constraint::Numeric),
        ("atomic", Constraint::AtomicElement),
        ("scalar numeric", Constraint::ScalarNumeric),
    ];

    pub fn spelling(self) -> Option<&'static str> {
        Constraint::SPELLINGS
            .iter()
            .find(|(_, constraint)| *constraint == self)
            .map(|(spelling, _)| *spelling)
    }

    pub fn from_spelling(text: &str) -> Option<Constraint> {
        Constraint::SPELLINGS
            .iter()
            .find(|(spelling, _)| *spelling == text)
            .map(|(_, constraint)| *constraint)
    }

    pub fn join(self, other: Constraint) -> Constraint {
        use Constraint::*;
        match (self, other) {
            (Unconstrained, c) | (c, Unconstrained) => c,
            (a, b) if a == b => a,
            (ScalarNumeric, _) | (_, ScalarNumeric) => ScalarNumeric,
            (Numeric, AtomicElement) | (AtomicElement, Numeric) => ScalarNumeric,
            _ => unreachable!("all constraint pairs are covered"),
        }
    }
}

/// A generalized type: binders with their constraints over a body.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct TypeScheme<'db> {
    pub binders: Vec<(Name<'db>, Constraint)>,
    pub body: Ty<'db>,
}

impl<'db> TypeScheme<'db> {
    pub fn monomorphic(body: Ty<'db>) -> TypeScheme<'db> {
        TypeScheme {
            binders: Vec::new(),
            body,
        }
    }
}

/// Whether a type contains `Unknown` anywhere in its structure. Named types
/// are opaque here — their representation is a separate declaration, not part
/// of this value's structure.
pub fn contains_unknown(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::Unknown => true,
        TyKind::Any | TyKind::Null | TyKind::Scalar(_) | TyKind::Var(_) | TyKind::Rigid(_) => false,
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner) => contains_unknown(db, *inner),
        TyKind::Tuple(items) => items.iter().any(|&item| contains_unknown(db, item)),
        TyKind::Record(fields) => fields.iter().any(|field| contains_unknown(db, field.ty)),
        TyKind::Function(function) => {
            function
                .positional
                .iter()
                .any(|&parameter| contains_unknown(db, parameter))
                || function
                    .named
                    .iter()
                    .flat_map(|parameter| parameter.types())
                    .any(|ty| contains_unknown(db, ty))
                || function
                    .variadic
                    .as_ref()
                    .is_some_and(|rest| contains_unknown(db, rest.element))
                || contains_unknown(db, function.ret)
        }
        TyKind::Union(members) => members.iter().any(|&member| contains_unknown(db, member)),
        TyKind::Named(_, arguments) => arguments
            .iter()
            .any(|&argument| contains_unknown(db, argument)),
    }
}

pub fn any(db: &dyn Db) -> Ty<'_> {
    Ty::new(db, TyKind::Any)
}

pub fn unknown(db: &dyn Db) -> Ty<'_> {
    Ty::new(db, TyKind::Unknown)
}

pub fn null(db: &dyn Db) -> Ty<'_> {
    Ty::new(db, TyKind::Null)
}

pub fn scalar(db: &dyn Db, atomic: Atomic) -> Ty<'_> {
    Ty::new(db, TyKind::Scalar(atomic))
}

/// THE union constructor: every builder, resolver, importer, and instantiation
/// path goes through here, because members can collapse after substitution.
pub fn union_of<'db>(db: &'db dyn Db, members: impl IntoIterator<Item = Ty<'db>>) -> Ty<'db> {
    let mut flat: Vec<Ty<'db>> = Vec::new();
    let mut saw_null = false;
    let mut stack: Vec<Ty<'db>> = members.into_iter().collect();
    stack.reverse();
    while let Some(member) = stack.pop() {
        match member.kind(db) {
            TyKind::Union(inner) => {
                for &inner_member in inner.iter().rev() {
                    stack.push(inner_member);
                }
            }
            TyKind::Any => return any(db),
            TyKind::Unknown => return unknown(db),
            TyKind::Null => saw_null = true,
            _ => {
                if !flat.contains(&member) {
                    flat.push(member);
                }
            }
        }
    }
    if saw_null {
        flat.push(null(db));
    }
    match flat.len() {
        0 => null(db),
        1 => flat[0],
        _ => Ty::new(db, TyKind::Union(flat)),
    }
}

/// Replace rigid variables per the substitution, leaving unmapped ones
/// intact — how schemes instantiate and how type-definition parameters apply.
/// How many nodes a type has **as a tree** — counting each path separately —
/// saturating at [`TYPE_SIZE_CEILING`].
///
/// Tree size, not distinct-node count, is the number that matters: interning
/// makes a type a DAG, so a self-referential record shares its subtrees and its
/// distinct-node count stays small while the tree it denotes grows by a factor
/// of the field count per level. Anything walking that type without a memo pays
/// the tree size, which is why one R file could grow a captured binding through
/// 877, 8823, 104655 and 1046623 nodes and never finish.
///
/// Tracked, so the sum runs once per distinct type; computing it is O(the DAG)
/// even though the answer describes the tree.
#[salsa::tracked(returns(clone))]
pub fn type_size<'db>(db: &'db dyn Db, ty: Ty<'db>) -> u64 {
    fn sum<'db>(db: &'db dyn Db, types: impl Iterator<Item = Ty<'db>>) -> u64 {
        types.fold(1, |total, ty| {
            total
                .saturating_add(type_size(db, ty))
                .min(TYPE_SIZE_CEILING)
        })
    }
    match ty.kind(db) {
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner) => sum(db, std::iter::once(*inner)),
        TyKind::Tuple(items) | TyKind::Union(items) => sum(db, items.iter().copied()),
        TyKind::Record(fields) => sum(db, fields.iter().map(|field| field.ty)),
        TyKind::Named(_, arguments) => sum(db, arguments.iter().copied()),
        TyKind::Function(function) => sum(
            db,
            function
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
                .chain(std::iter::once(function.ret)),
        ),
        _ => 1,
    }
}

/// The point past which [`type_size`] stops counting. Well above any type real
/// code produces and well below the sizes a self-referential value reaches.
pub const TYPE_SIZE_CEILING: u64 = 100_000;

/// Rebuild `ty` with `map` applied to every type it directly contains — the one
/// place that knows which types a composite is made of, and the mapping
/// counterpart of [`Parameter::types`].
///
/// Every transformation that rewrites types (resolution, substitution,
/// generalization, variable erasure) goes through here, so a member a
/// transformation must not skip cannot be skipped by one walk and honored by
/// another. A parameter's DEFAULT is exactly such a member: five hand-written
/// walks each re-listed the composites, two of them forgot the default, and an
/// inference variable riding inside one crossed the item boundary into a table
/// that could not resolve it.
///
/// Leaves — and the variables and rigid binders a caller substitutes — come back
/// unchanged, so a caller handles the kind it cares about and delegates the rest
/// to this function.
pub fn map_child_types<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    map: &mut impl FnMut(Ty<'db>) -> Ty<'db>,
) -> Ty<'db> {
    match ty.kind(db).clone() {
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::Var(_)
        | TyKind::Rigid(_) => ty,
        TyKind::Vector(inner) => Ty::new(db, TyKind::Vector(map(inner))),
        TyKind::NamedVector(inner) => Ty::new(db, TyKind::NamedVector(map(inner))),
        TyKind::List(inner) => Ty::new(db, TyKind::List(map(inner))),
        TyKind::NamedList(inner) => Ty::new(db, TyKind::NamedList(map(inner))),
        TyKind::Tuple(items) => Ty::new(
            db,
            TyKind::Tuple(items.iter().map(|&item| map(item)).collect()),
        ),
        TyKind::Record(fields) => Ty::new(
            db,
            TyKind::Record(
                fields
                    .iter()
                    .map(|field| RecordField {
                        ty: map(field.ty),
                        ..field.clone()
                    })
                    .collect(),
            ),
        ),
        TyKind::Function(function) => Ty::new(
            db,
            TyKind::Function(FunctionType {
                positional: function.positional.iter().map(|&ty| map(ty)).collect(),
                named: function
                    .named
                    .iter()
                    .map(|parameter| Parameter {
                        ty: map(parameter.ty),
                        default: parameter.default.map(&mut *map),
                        ..parameter.clone()
                    })
                    .collect(),
                variadic: function.variadic.as_ref().map(|rest| RestParameter {
                    element: map(rest.element),
                    ..rest.clone()
                }),
                ret: map(function.ret),
            }),
        ),
        // Members can collapse into one another once mapped, so the result
        // re-normalizes instead of keeping a union of equal members.
        TyKind::Union(members) => union_of(db, members.iter().map(|&member| map(member))),
        TyKind::Named(name, arguments) => Ty::new(
            db,
            TyKind::Named(
                name,
                arguments.iter().map(|&argument| map(argument)).collect(),
            ),
        ),
    }
}

/// Visit every type `ty` directly contains — the reading counterpart of
/// [`map_child_types`], for walks that inspect a type without rebuilding it.
pub fn for_each_child_type<'db>(db: &'db dyn Db, ty: Ty<'db>, visit: &mut impl FnMut(Ty<'db>)) {
    match ty.kind(db) {
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::Var(_)
        | TyKind::Rigid(_) => {}
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner) => visit(*inner),
        TyKind::Tuple(items) | TyKind::Union(items) => items.iter().copied().for_each(visit),
        TyKind::Record(fields) => fields.iter().for_each(|field| visit(field.ty)),
        TyKind::Function(function) => {
            function.positional.iter().copied().for_each(&mut *visit);
            function
                .named
                .iter()
                .flat_map(|parameter| parameter.types())
                .for_each(&mut *visit);
            function
                .variadic
                .iter()
                .for_each(|rest| visit(rest.element));
            visit(function.ret);
        }
        TyKind::Named(_, arguments) => arguments.iter().copied().for_each(visit),
    }
}

/// Whether an inference variable appears anywhere in `ty`. Variables are
/// indices into ONE table, so a type that outlives the table that minted them —
/// an exported scheme, a value cached across a table rollback — must carry
/// none: read in a foreign table the index either dangles or, worse, names an
/// unrelated variable.
pub fn contains_inference_var<'db>(db: &'db dyn Db, ty: Ty<'db>) -> bool {
    if matches!(ty.kind(db), TyKind::Var(_)) {
        return true;
    }
    let mut found = false;
    for_each_child_type(db, ty, &mut |child| {
        found = found || contains_inference_var(db, child);
    });
    found
}

pub fn substitute_rigid<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &rustc_hash::FxHashMap<Name<'db>, Ty<'db>>,
) -> Ty<'db> {
    substitute_rigid_memo(db, ty, substitution, &mut rustc_hash::FxHashMap::default())
}

/// A type is a DAG, not a tree: interning means one subtree is reached by every
/// path that mentions it, and a record whose fields all return that record is
/// reached once per field per level. Substituting without a memo re-walks each
/// shared subtree once per path, which is exponential in depth — measured at 278
/// million calls in 40 seconds on one R file, where the same walk memoised
/// finishes. The memo is per top-level call because the substitution is fixed
/// for its duration, so a node's answer cannot depend on how it was reached.
fn substitute_rigid_memo<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &rustc_hash::FxHashMap<Name<'db>, Ty<'db>>,
    memo: &mut rustc_hash::FxHashMap<Ty<'db>, Ty<'db>>,
) -> Ty<'db> {
    if let Some(&done) = memo.get(&ty) {
        return done;
    }
    let substituted = substitute_rigid_uncached(db, ty, substitution, memo);
    memo.insert(ty, substituted);
    substituted
}

fn substitute_rigid_uncached<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &rustc_hash::FxHashMap<Name<'db>, Ty<'db>>,
    memo: &mut rustc_hash::FxHashMap<Ty<'db>, Ty<'db>>,
) -> Ty<'db> {
    if let TyKind::Rigid(name) = ty.kind(db) {
        return substitution.get(name).copied().unwrap_or(ty);
    }
    map_child_types(db, ty, &mut |child| {
        substitute_rigid_memo(db, child, substitution, memo)
    })
}

/// Replace every inference variable in an (already-resolved) type with
/// `Unknown`, for values that must survive a later table rollback: a stored
/// variable id would dangle once the rollback reclaims (and later reuses) the
/// id.
pub fn erase_vars<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    if matches!(ty.kind(db), TyKind::Var(_)) {
        return unknown(db);
    }
    map_child_types(db, ty, &mut |child| erase_vars(db, child))
}

#[cfg(test)]
mod size_tests {
    use super::*;
    use crate::RootDatabase;

    /// The ceiling exists to stop a *shared* type whose tree is enormous, so the
    /// count has to follow paths rather than distinct nodes — a record holding
    /// one subtree twice costs twice. Counting the graph instead would report a
    /// self-referential value as small and never fire.
    #[test]
    fn size_counts_the_tree_not_the_shared_graph() {
        let db = RootDatabase::default();
        let leaf = scalar(&db, Atomic::Integer);
        let pair = Ty::new(&db, TyKind::Tuple(vec![leaf, leaf]));
        let nested = Ty::new(&db, TyKind::Tuple(vec![pair, pair]));

        assert_eq!(type_size(&db, leaf), 1);
        assert_eq!(type_size(&db, pair), 3, "one node plus two leaves");
        assert_eq!(
            type_size(&db, nested),
            7,
            "the shared pair is counted once per path, not once overall"
        );
    }

    /// Doubling depth doubles the tree, so a value that refers to itself reaches
    /// the ceiling quickly and saturates there instead of overflowing.
    #[test]
    fn size_saturates_at_the_ceiling() {
        let db = RootDatabase::default();
        let mut ty = scalar(&db, Atomic::Integer);
        for _ in 0..64 {
            ty = Ty::new(&db, TyKind::Tuple(vec![ty, ty]));
        }
        assert_eq!(type_size(&db, ty), TYPE_SIZE_CEILING);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootDatabase;

    /// A parameter's default is a type like any other, and the walks that
    /// rewrite or inspect types have to reach it: a variable left there
    /// travelled out of the item whose table minted it and was read against a
    /// table that never had it.
    #[test]
    fn a_parameter_default_is_part_of_the_type() {
        let db = RootDatabase::default();
        let var = Ty::new(&db, TyKind::Var(InferenceVar(7)));
        let signature = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: Vec::new(),
                named: vec![Parameter {
                    name: Name::new(&db, "x".to_owned()),
                    ty: scalar(&db, Atomic::Integer),
                    optional: true,
                    default: Some(var),
                }],
                variadic: None,
                ret: scalar(&db, Atomic::Integer),
            }),
        );

        assert!(contains_inference_var(&db, signature));
        let erased = erase_vars(&db, signature);
        assert!(!contains_inference_var(&db, erased));
        let TyKind::Function(function) = erased.kind(&db) else {
            panic!("erasure keeps the shape")
        };
        assert_eq!(function.named[0].default, Some(unknown(&db)));
    }

    #[test]
    fn interning_gives_id_equality() {
        let db = RootDatabase::default();
        let a = scalar(&db, Atomic::Integer);
        let b = scalar(&db, Atomic::Integer);
        assert_eq!(a, b);
        let f1 = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: vec![a],
                named: Vec::new(),
                variadic: None,
                ret: scalar(&db, Atomic::Double),
            }),
        );
        let f2 = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: vec![b],
                named: Vec::new(),
                variadic: None,
                ret: scalar(&db, Atomic::Double),
            }),
        );
        assert_eq!(f1, f2);
    }

    #[test]
    fn union_normalization() {
        let db = RootDatabase::default();
        let int = scalar(&db, Atomic::Integer);
        let chr = scalar(&db, Atomic::Character);

        // Flatten + dedupe keeping first occurrence + NULL last.
        let inner = union_of(&db, [null(&db), int]);
        let outer = union_of(&db, [inner, chr, int]);
        let TyKind::Union(members) = outer.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(members, &vec![int, chr, null(&db)]);

        // Any and Unknown absorb.
        assert_eq!(union_of(&db, [int, any(&db)]), any(&db));
        assert_eq!(union_of(&db, [unknown(&db), int]), unknown(&db));

        // Singleton unwraps; empty collapses to NULL.
        assert_eq!(union_of(&db, [int, int]), int);
        assert_eq!(union_of(&db, []), null(&db));
        // `T | NULL` stays a union with NULL last.
        let optional = union_of(&db, [null(&db), chr]);
        let TyKind::Union(members) = optional.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(members, &vec![chr, null(&db)]);
    }

    #[test]
    fn constraint_lattice_joins() {
        use Constraint::*;
        assert_eq!(Unconstrained.join(Numeric), Numeric);
        assert_eq!(Numeric.join(AtomicElement), ScalarNumeric);
        assert_eq!(ScalarNumeric.join(Numeric), ScalarNumeric);
        assert_eq!(AtomicElement.join(AtomicElement), AtomicElement);
    }
}
