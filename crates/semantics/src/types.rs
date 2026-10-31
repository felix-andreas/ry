use rustc_hash::FxHashMap;
use std::iter;

use crate::Db;

#[salsa::interned(debug)]
pub struct Name<'db> {
    #[returns(deref)]
    pub text: String,
}

#[salsa::interned(debug)]
pub struct Ty<'db> {
    #[returns(ref)]
    pub kind: TyKind<'db>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum TyKind<'db> {
    /// Compatible with everything, silently.
    Any,
    /// An absent or unmodeled fact; strict mode reports its origins.
    Unknown,
    Null,
    Scalar(Atomic),
    Vector(Ty<'db>),
    NamedVector(Ty<'db>),
    List(Ty<'db>),
    NamedList(Ty<'db>),
    /// `list{A, B}`.
    Tuple(Vec<Ty<'db>>),
    /// `list{a: A, b: B}`; fields keep declaration order.
    Record(Vec<RecordField<'db>>),
    Function(FunctionType<'db>),
    /// Normalized; build only through `union_of`, never directly.
    Union(Vec<Ty<'db>>),
    Named(Name<'db>, Vec<Ty<'db>>),
    /// Inference-scoped: resolved through the table, never stored in exported schemes.
    Var(InferenceVar),
    /// A skolem from an explicit `<T>` binder: refuses to bind while its scope is
    /// checked, generalizes back out afterwards.
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

/// Unlike a record field, carries the default's type: R evaluates the default
/// when the argument is omitted, so it is what the parameter holds on that path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Parameter<'db> {
    pub name: Name<'db>,
    pub ty: Ty<'db>,
    /// The formal has a default, or the body tests it with `missing()`.
    pub optional: bool,
    /// Also `None` for `missing()`-style optionality and for an annotated `[name]:`
    /// whose definition is not in view.
    pub default: Option<Ty<'db>>,
}

impl<'db> Parameter<'db> {
    /// Walks must go through here: one that skipped the default would make an
    /// occurs check or level adjustment unsound.
    pub fn types(&self) -> impl Iterator<Item = Ty<'db>> + '_ {
        iter::once(self.ty).chain(self.default)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct FunctionType<'db> {
    pub positional: Vec<Ty<'db>>,
    pub named: Vec<Parameter<'db>>,
    /// Parameters after `...` match by name only.
    pub variadic: Option<RestParameter<'db>>,
    pub ret: Ty<'db>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RestParameter<'db> {
    pub element: Ty<'db>,
    pub preceding_named: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, salsa::SalsaValue)]
pub enum Constraint {
    #[default]
    Unconstrained,
    Numeric,
    AtomicElement,
    ScalarNumeric,
}

impl Constraint {
    /// Shared by rendering and parsing, so every constraint the checker prints can
    /// be written back in an annotation.
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

/// Named types are not expanded: their representation is a separate declaration.
pub fn contains_unknown(db: &dyn Db, ty: Ty<'_>) -> bool {
    if matches!(ty.kind(db), TyKind::Unknown) {
        return true;
    }
    let mut found = false;
    for_each_child_type(db, ty, &mut |child| {
        found = found || contains_unknown(db, child);
    });
    found
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

/// Node count of `ty` as a tree (each path counted separately), saturating at
/// [`TYPE_SIZE_CEILING`]. Interning makes types DAGs, so a self-referential
/// record's tree grows exponentially while its distinct nodes stay few.
#[salsa::tracked(returns(clone))]
pub fn type_size<'db>(db: &'db dyn Db, ty: Ty<'db>) -> u64 {
    let mut total: u64 = 1;
    for_each_child_type(db, ty, &mut |child| {
        total = total
            .saturating_add(type_size(db, child))
            .min(TYPE_SIZE_CEILING);
    });
    total
}

/// Far above any type real code produces, far below what a self-referential value reaches.
pub const TYPE_SIZE_CEILING: u64 = 100_000;

/// Rebuild `ty` with `map` applied to each direct child type. Every type rewrite
/// goes through here so no walk can skip a member such as a parameter default.
/// Leaves, variables, and rigid binders come back unchanged.
pub fn map_child_types<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    map: &mut impl FnMut(Ty<'db>) -> Ty<'db>,
) -> Ty<'db> {
    match ty.kind(db) {
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::Var(_)
        | TyKind::Rigid(_) => ty,
        TyKind::Vector(inner) => Ty::new(db, TyKind::Vector(map(*inner))),
        TyKind::NamedVector(inner) => Ty::new(db, TyKind::NamedVector(map(*inner))),
        TyKind::List(inner) => Ty::new(db, TyKind::List(map(*inner))),
        TyKind::NamedList(inner) => Ty::new(db, TyKind::NamedList(map(*inner))),
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
        // Members can collapse once mapped, so re-normalize.
        TyKind::Union(members) => union_of(db, members.iter().map(|&member| map(member))),
        TyKind::Named(name, arguments) => Ty::new(
            db,
            TyKind::Named(
                *name,
                arguments.iter().map(|&argument| map(argument)).collect(),
            ),
        ),
    }
}

/// The reading counterpart of [`map_child_types`].
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

/// Variables index one table, so a type outliving it (an exported scheme, a
/// value cached across a rollback) must contain none.
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

/// Unmapped rigid variables are left intact.
pub fn substitute_rigid<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &FxHashMap<Name<'db>, Ty<'db>>,
) -> Ty<'db> {
    substitute_rigid_memo(db, ty, substitution, &mut FxHashMap::default())
}

/// Memoized because interning makes types DAGs: unmemoized, shared subtrees are
/// re-walked per path, exponential in depth. Per call, since the substitution is fixed.
fn substitute_rigid_memo<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &FxHashMap<Name<'db>, Ty<'db>>,
    memo: &mut FxHashMap<Ty<'db>, Ty<'db>>,
) -> Ty<'db> {
    if let Some(&done) = memo.get(&ty) {
        return done;
    }
    let substituted = match ty.kind(db) {
        TyKind::Rigid(name) => substitution.get(name).copied().unwrap_or(ty),
        _ => map_child_types(db, ty, &mut |child| {
            substitute_rigid_memo(db, child, substitution, memo)
        }),
    };
    memo.insert(ty, substituted);
    substituted
}

/// For values that must survive a table rollback, which reclaims and reuses
/// variable ids.
pub fn erase_vars<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    if matches!(ty.kind(db), TyKind::Var(_)) {
        return unknown(db);
    }
    map_child_types(db, ty, &mut |child| erase_vars(db, child))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootDatabase;

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

    #[test]
    fn size_saturates_at_the_ceiling() {
        let db = RootDatabase::default();
        let mut ty = scalar(&db, Atomic::Integer);
        for _ in 0..64 {
            ty = Ty::new(&db, TyKind::Tuple(vec![ty, ty]));
        }
        assert_eq!(type_size(&db, ty), TYPE_SIZE_CEILING);
    }

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

        let inner = union_of(&db, [null(&db), int]);
        let outer = union_of(&db, [inner, chr, int]);
        let TyKind::Union(members) = outer.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(members, &vec![int, chr, null(&db)]);

        assert_eq!(union_of(&db, [int, any(&db)]), any(&db));
        assert_eq!(union_of(&db, [unknown(&db), int]), unknown(&db));

        assert_eq!(union_of(&db, [int, int]), int);
        assert_eq!(union_of(&db, []), null(&db));
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
