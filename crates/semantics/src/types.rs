use rustc_hash::FxHashMap;
use std::iter;

use crate::Db;

#[salsa::interned(debug, revisions = usize::MAX)]
pub struct Name<'db> {
    #[returns(deref)]
    pub text: String,
}

#[salsa::interned(debug, revisions = usize::MAX)]
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
    /// `Tuple{A, B}`: ry's native tuple, which is not a list and never fits a list type.
    NativeTuple(Vec<Ty<'db>>),
    /// `Record{a: A, b: B}`: ry's native record. Field order is not part of the type, so the
    /// fields are kept sorted by name and every field is required.
    NativeRecord(Vec<NativeField<'db>>),
    /// `:Name` or `:Name(...)`. The payload is always a `NativeTuple` (positional) or a
    /// `NativeRecord` (named), so a payload types and compares exactly like that literal.
    Tag(Name<'db>, Option<Ty<'db>>),
    /// `..`, the member that makes a tag union open: it stands for every tag the union does
    /// not list. Only `union_of` places it, always last.
    OtherTags,
    /// A table whose type is a record of column vectors: ry's `DataFrame{a: A[], b: B[]}`,
    /// a `LazyFrame{...}` plan, a `GroupBy{...}`, or a `data.table{...}` whose columns are known.
    /// `None` is a table whose columns are not known (`DataFrame`), which every column reference
    /// treats as silent.
    Frame(FrameKind, Option<Vec<RecordField<'db>>>),
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

/// What a table type is. The kinds never fit each other: a `data.table` runs data.table's code,
/// a `LazyFrame` holds a plan rather than rows, and a `GroupBy` only feeds `GroupBy.agg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum FrameKind {
    DataFrame,
    LazyFrame,
    /// `DataFrame.group_by` or `LazyFrame.group_by`. The first `keys` columns are the group keys,
    /// rendered before a `;`, and the rest are the columns `GroupBy.agg` aggregates.
    GroupBy {
        lazy: bool,
        keys: u32,
    },
    /// A data.table whose columns are known, from `fread` and the rest of data.table's readers.
    DataTable,
}

impl FrameKind {
    pub fn head(self) -> &'static str {
        match self {
            FrameKind::DataFrame => "DataFrame",
            FrameKind::LazyFrame => "LazyFrame",
            FrameKind::GroupBy { lazy: false, .. } => "GroupBy",
            FrameKind::GroupBy { lazy: true, .. } => "LazyGroupBy",
            FrameKind::DataTable => "data.table",
        }
    }

    /// The kind a type head names, for the heads written with braces.
    pub fn from_head(head: &str) -> Option<FrameKind> {
        Some(match head {
            "DataFrame" => FrameKind::DataFrame,
            "LazyFrame" => FrameKind::LazyFrame,
            "GroupBy" => FrameKind::GroupBy {
                lazy: false,
                keys: 0,
            },
            "LazyGroupBy" => FrameKind::GroupBy {
                lazy: true,
                keys: 0,
            },
            "data.table" => FrameKind::DataTable,
            _ => return None,
        })
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NativeField<'db> {
    pub name: Name<'db>,
    pub ty: Ty<'db>,
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
    /// A dictionary key or set element: a `logical`, `integer`, `double` or `character` scalar.
    /// `complex` and `raw` are left out because they have no total order or hash that the
    /// runtime's collections use.
    Key,
    ScalarNumeric,
}

impl Constraint {
    /// Shared by rendering and parsing, so every constraint the checker prints can
    /// be written back in an annotation.
    pub const SPELLINGS: [(&'static str, Constraint); 4] = [
        ("numeric", Constraint::Numeric),
        ("atomic", Constraint::AtomicElement),
        ("key", Constraint::Key),
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
            (Numeric, AtomicElement | Key) | (AtomicElement | Key, Numeric) => ScalarNumeric,
            (Key, AtomicElement) | (AtomicElement, Key) => Key,
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

/// `Array<element>`, ry's strict typed vector.
pub fn array<'db>(db: &'db dyn Db, element: Ty<'db>) -> Ty<'db> {
    Ty::new(db, TyKind::Named(Name::new(db, "Array"), vec![element]))
}

/// The item type of the standard library's `Array<T>`, and `None` for any other type, including a
/// project's own type that happens to be called `Array`.
pub fn array_element<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Ty<'db>> {
    match ty.kind(db) {
        TyKind::Named(name, arguments)
            if arguments.len() == 1
                && name.text(db) == "Array"
                && crate::stubs::is_ry_type(db, "Array") =>
        {
            Some(arguments[0])
        }
        _ => None,
    }
}

/// Whether every value of `ty` is a tag, so that two such types join into one tag union.
pub fn is_tag_type(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::Tag(..) | TyKind::OtherTags => true,
        TyKind::Union(members) => members.iter().all(|&member| is_tag_type(db, member)),
        _ => false,
    }
}

/// Whether every value of `ty` is one of ry's own values (a record, tuple, tag, or a standard
/// library collection), which an Array stores boxed in a list rather than unboxed.
pub fn is_ry_value_type(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::NativeRecord(_) | TyKind::NativeTuple(_) | TyKind::Tag(..) | TyKind::OtherTags => {
            true
        }
        TyKind::Named(name, _) => {
            matches!(name.text(db), "Array" | "Dict" | "Set")
                && crate::stubs::is_ry_type(db, name.text(db))
        }
        TyKind::Union(members) => {
            !members.is_empty() && members.iter().all(|&member| is_ry_value_type(db, member))
        }
        _ => false,
    }
}

/// Sorts by name in byte order, the one order a record's fields are stored and printed in.
pub fn native_record<'db>(db: &'db dyn Db, mut fields: Vec<NativeField<'db>>) -> Ty<'db> {
    fields.sort_by(|left, right| left.name.text(db).cmp(right.name.text(db)));
    Ty::new(db, TyKind::NativeRecord(fields))
}

/// The payload's shape: its item count, or its field names. Two tags with one name and one
/// shape are the same member of a union, and two with one name and different shapes conflict.
#[derive(PartialEq, Eq)]
pub enum PayloadShape<'db> {
    Bare,
    Positional(usize),
    Named(Vec<Name<'db>>),
    /// A payload that is not a native tuple or record, which only a broken annotation makes.
    Other(Ty<'db>),
}

pub fn payload_shape<'db>(db: &'db dyn Db, payload: Option<Ty<'db>>) -> PayloadShape<'db> {
    let Some(payload) = payload else {
        return PayloadShape::Bare;
    };
    match payload.kind(db) {
        TyKind::NativeTuple(items) => PayloadShape::Positional(items.len()),
        TyKind::NativeRecord(fields) => {
            PayloadShape::Named(fields.iter().map(|field| field.name).collect())
        }
        _ => PayloadShape::Other(payload),
    }
}

/// Two same-name tags of a union whose payload shapes differ, which `union_of` keeps apart
/// and the checker reports where they meet.
pub fn conflicting_tags<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<(Ty<'db>, Ty<'db>)> {
    let TyKind::Union(members) = ty.kind(db) else {
        return None;
    };
    for (index, &first) in members.iter().enumerate() {
        let TyKind::Tag(name, _) = first.kind(db) else {
            continue;
        };
        for &second in &members[index + 1..] {
            if let TyKind::Tag(other, _) = second.kind(db)
                && other == name
            {
                return Some((first, second));
            }
        }
    }
    None
}

/// A frame column is a vector: a scalar broadens to its vector type, and names drop,
/// because a column's elements are positional rows.
pub fn column_type<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    match ty.kind(db) {
        TyKind::Scalar(_) => Ty::new(db, TyKind::Vector(ty)),
        TyKind::NamedVector(element) => Ty::new(db, TyKind::Vector(*element)),
        // A frame stores an Array column as the plain R vector underneath it.
        _ => match array_element(db, ty) {
            Some(element) if matches!(element.kind(db), TyKind::Scalar(_)) => {
                Ty::new(db, TyKind::Vector(element))
            }
            _ => ty,
        },
    }
}

pub fn union_of<'db>(db: &'db dyn Db, members: impl IntoIterator<Item = Ty<'db>>) -> Ty<'db> {
    let mut flat: Vec<Ty<'db>> = Vec::new();
    let mut saw_null = false;
    let mut saw_other_tags = false;
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
            TyKind::OtherTags => saw_other_tags = true,
            // A tag name has one payload shape per union, so a second tag of that name and
            // shape joins into the first, position by position.
            TyKind::Tag(name, payload) => {
                let shape = payload_shape(db, *payload);
                let same = flat.iter().position(|&existing| match existing.kind(db) {
                    TyKind::Tag(existing_name, existing_payload) => {
                        existing_name == name && payload_shape(db, *existing_payload) == shape
                    }
                    _ => false,
                });
                match same {
                    Some(index) => flat[index] = merge_tags(db, flat[index], member),
                    None => flat.push(member),
                }
            }
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
    if saw_other_tags {
        flat.push(Ty::new(db, TyKind::OtherTags));
    }
    match flat.len() {
        0 => null(db),
        1 => flat[0],
        _ => Ty::new(db, TyKind::Union(flat)),
    }
}

/// Joins two same-name, same-shape tags into one whose payload unions each position.
fn merge_tags<'db>(db: &'db dyn Db, first: Ty<'db>, second: Ty<'db>) -> Ty<'db> {
    if first == second {
        return first;
    }
    let (TyKind::Tag(name, Some(first_payload)), TyKind::Tag(_, Some(second_payload))) =
        (first.kind(db), second.kind(db))
    else {
        return first;
    };
    let payload = match (first_payload.kind(db), second_payload.kind(db)) {
        (TyKind::NativeTuple(left), TyKind::NativeTuple(right)) => Ty::new(
            db,
            TyKind::NativeTuple(
                left.iter()
                    .zip(right)
                    .map(|(&left, &right)| union_of(db, [left, right]))
                    .collect(),
            ),
        ),
        (TyKind::NativeRecord(left), TyKind::NativeRecord(right)) => Ty::new(
            db,
            TyKind::NativeRecord(
                left.iter()
                    .zip(right)
                    .map(|(left, right)| NativeField {
                        name: left.name,
                        ty: union_of(db, [left.ty, right.ty]),
                    })
                    .collect(),
            ),
        ),
        _ => return first,
    };
    Ty::new(db, TyKind::Tag(*name, Some(payload)))
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
    if let TyKind::Union(members) = ty.kind(db) {
        // Members can collapse once mapped, so re-normalize.
        return union_of(db, members.iter().map(|&member| map(member)));
    }
    // Most maps leave most types alone, and re-interning an unchanged type costs a hash of
    // the whole kind and a table lookup.
    let mut changed = false;
    let kind = mapped_kind(db, ty, &mut |child| {
        let mapped = map(child);
        changed |= mapped != child;
        mapped
    });
    match kind {
        Some(kind) if changed => Ty::new(db, kind),
        _ => ty,
    }
}

/// `ty`'s kind with `map` applied to each child, or `None` for a type without children.
fn mapped_kind<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    map: &mut impl FnMut(Ty<'db>) -> Ty<'db>,
) -> Option<TyKind<'db>> {
    match ty.kind(db) {
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::OtherTags
        | TyKind::Var(_)
        | TyKind::Rigid(_) => None,
        TyKind::Vector(inner) => Some(TyKind::Vector(map(*inner))),
        TyKind::NamedVector(inner) => Some(TyKind::NamedVector(map(*inner))),
        TyKind::List(inner) => Some(TyKind::List(map(*inner))),
        TyKind::NamedList(inner) => Some(TyKind::NamedList(map(*inner))),
        TyKind::Tuple(items) => Some(TyKind::Tuple(items.iter().map(|&item| map(item)).collect())),
        TyKind::Record(fields) => Some(TyKind::Record(
            fields
                .iter()
                .map(|field| RecordField {
                    ty: map(field.ty),
                    ..field.clone()
                })
                .collect(),
        )),
        TyKind::NativeTuple(items) => Some(TyKind::NativeTuple(
            items.iter().map(|&item| map(item)).collect(),
        )),
        TyKind::NativeRecord(fields) => Some(TyKind::NativeRecord(
            fields
                .iter()
                .map(|field| NativeField {
                    name: field.name,
                    ty: map(field.ty),
                })
                .collect(),
        )),
        TyKind::Tag(name, payload) => Some(TyKind::Tag(*name, payload.map(&mut *map))),
        TyKind::Frame(_, None) => None,
        TyKind::Frame(kind, Some(columns)) => Some(TyKind::Frame(
            *kind,
            Some(
                columns
                    .iter()
                    .map(|column| RecordField {
                        ty: map(column.ty),
                        ..column.clone()
                    })
                    .collect(),
            ),
        )),
        TyKind::Function(function) => Some(TyKind::Function(FunctionType {
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
        })),
        TyKind::Union(_) => None,
        TyKind::Named(name, arguments) => Some(TyKind::Named(
            *name,
            arguments.iter().map(|&argument| map(argument)).collect(),
        )),
    }
}

/// The reading counterpart of [`map_child_types`].
pub fn for_each_child_type<'db>(db: &'db dyn Db, ty: Ty<'db>, visit: &mut impl FnMut(Ty<'db>)) {
    match ty.kind(db) {
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::OtherTags
        | TyKind::Var(_)
        | TyKind::Rigid(_) => {}
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner) => visit(*inner),
        TyKind::Tuple(items) | TyKind::NativeTuple(items) | TyKind::Union(items) => {
            items.iter().copied().for_each(visit)
        }
        TyKind::NativeRecord(fields) => fields.iter().for_each(|field| visit(field.ty)),
        TyKind::Tag(_, payload) => payload.iter().copied().for_each(visit),
        TyKind::Record(fields) | TyKind::Frame(_, Some(fields)) => {
            fields.iter().for_each(|field| visit(field.ty))
        }
        TyKind::Frame(_, None) => {}
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

    fn tag<'db>(db: &'db RootDatabase, name: &str, items: Option<Vec<Ty<'db>>>) -> Ty<'db> {
        let payload = items.map(|items| Ty::new(db, TyKind::NativeTuple(items)));
        Ty::new(db, TyKind::Tag(Name::new(db, name), payload))
    }

    #[test]
    fn same_shape_tags_merge_their_payloads() {
        let db = RootDatabase::default();
        let int = scalar(&db, Atomic::Integer);
        let chr = scalar(&db, Atomic::Character);
        let merged = union_of(
            &db,
            [
                tag(&db, "Some", Some(vec![int])),
                tag(&db, "Some", Some(vec![chr])),
            ],
        );
        assert_eq!(
            merged,
            tag(&db, "Some", Some(vec![union_of(&db, [int, chr])])),
            "one member per tag name and shape"
        );
        assert_eq!(conflicting_tags(&db, merged), None);
    }

    #[test]
    fn different_shape_tags_stay_apart_and_conflict() {
        let db = RootDatabase::default();
        let int = scalar(&db, Atomic::Integer);
        let one = tag(&db, "Some", Some(vec![int]));
        let two = tag(&db, "Some", Some(vec![int, int]));
        let union = union_of(&db, [one, two]);
        let TyKind::Union(members) = union.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(members, &vec![one, two]);
        assert_eq!(conflicting_tags(&db, union), Some((one, two)));
    }

    #[test]
    fn other_tags_go_last() {
        let db = RootDatabase::default();
        let other = Ty::new(&db, TyKind::OtherTags);
        let none = tag(&db, "None", None);
        let union = union_of(&db, [other, null(&db), none]);
        let TyKind::Union(members) = union.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(members, &vec![none, null(&db), other]);
    }

    #[test]
    fn constraint_lattice_joins() {
        use Constraint::*;
        assert_eq!(Unconstrained.join(Numeric), Numeric);
        assert_eq!(Numeric.join(AtomicElement), ScalarNumeric);
        assert_eq!(ScalarNumeric.join(Numeric), ScalarNumeric);
        assert_eq!(AtomicElement.join(AtomicElement), AtomicElement);
        assert_eq!(Key.join(AtomicElement), Key);
        assert_eq!(Key.join(Numeric), ScalarNumeric);
    }
}
