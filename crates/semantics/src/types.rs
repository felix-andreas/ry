use {
    crate::{Db, hir::Symbol},
    hashbrown::HashTable,
    rustc_hash::{FxBuildHasher, FxHashMap},
    std::{
        cell::Cell,
        fmt,
        hash::{BuildHasher, Hash, Hasher},
        iter,
        marker::PhantomData,
        mem,
        sync::{LazyLock, Mutex},
    },
};

/// A name in a type (a type name, a field, a tag, a type parameter). It is a process-wide
/// [`Symbol`], so equality is an index comparison and no database is consulted. The lifetime
/// only keeps the API of a database-scoped handle.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name<'db> {
    symbol: Symbol,
    db: PhantomData<&'db ()>,
}

impl<'db> Name<'db> {
    pub fn new<D: ?Sized>(_db: &'db D, text: impl AsRef<str>) -> Name<'db> {
        Name {
            symbol: Symbol::new(text.as_ref()),
            db: PhantomData,
        }
    }

    pub fn text<D: ?Sized>(self, _db: &D) -> &'static str {
        self.symbol.as_str()
    }
}

/// A type, hash-consed process-wide: structurally equal kinds share one immortal allocation,
/// so equality and hashing compare pointers and `kind` is a plain read. The checker builds
/// and compares types far more often than anything else, and salsa interning put a table
/// lookup, a dependency edge and shared atomics on each of those operations. The lifetime is
/// phantom: no type borrows from a database, which is what lets the table outlive them all.
#[derive(Clone, Copy)]
pub struct Ty<'db> {
    entry: &'static TypeEntry,
    db: PhantomData<&'db ()>,
}

/// A hash-consed type with the facts derived from its structure at construction, while its
/// children's facts are at hand.
struct TypeEntry {
    kind: TyKind<'static>,
    /// Nodes in the type tree, capped at [`TYPE_SIZE_CEILING`].
    size: u64,
    /// Which [`TypeEntry::HAS_VAR`]-style kinds occur anywhere in the tree, so the walks that
    /// only rewrite those kinds return a type without them untouched instead of rebuilding it.
    contents: u8,
}

impl TypeEntry {
    const HAS_VAR: u8 = 1;
    const HAS_RIGID: u8 = 1 << 1;
    const HAS_UNKNOWN: u8 = 1 << 2;
    /// A `Named` application, which resolution may expand as an alias.
    const HAS_NAMED: u8 = 1 << 3;
}

impl<'db> Ty<'db> {
    /// Whether resolving through an inference table can change the type: only a variable or
    /// an alias application inside it can.
    pub(crate) fn may_resolve(self) -> bool {
        self.entry.contents & (TypeEntry::HAS_VAR | TypeEntry::HAS_NAMED) != 0
    }
}

/// A name as the key of a tracked function: salsa keys a one-argument query by one of its
/// own structs, and [`Name`] lives outside every database.
#[salsa::interned(revisions = usize::MAX)]
pub struct QueryName<'db> {
    pub name: Name<'db>,
}

impl<'db> Ty<'db> {
    pub fn new<D: ?Sized>(_db: &'db D, kind: TyKind<'db>) -> Ty<'db> {
        // SAFETY: `'db` appears in `TyKind` only through `Ty` and `Name`, whose lifetimes are
        // phantom, so the value borrows nothing and is valid for any lifetime.
        let kind: TyKind<'static> = unsafe { mem::transmute(kind) };
        Ty {
            entry: intern_type(kind),
            db: PhantomData,
        }
    }

    pub fn kind<D: ?Sized>(self, _db: &D) -> &'db TyKind<'db> {
        &self.entry.kind
    }
}

impl PartialEq for Ty<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.entry, other.entry)
    }
}

impl Eq for Ty<'_> {}

impl Hash for Ty<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.entry, state);
    }
}

impl Hash for Name<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.symbol.index().hash(state);
    }
}

impl fmt::Debug for Ty<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.entry.kind, formatter)
    }
}

impl fmt::Debug for Name<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.symbol.as_str(), formatter)
    }
}

// SAFETY: both point into process-wide tables that never free or move an entry, so a value
// from an older revision stays valid in every later one.
unsafe impl salsa::SalsaValue for Ty<'_> {}
unsafe impl salsa::SalsaValue for Name<'_> {}

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
    /// The signature was inferred from an unannotated body, so a scalar parameter type is a
    /// claim of how the body uses the value, not a declared length.
    pub inferred: bool,
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
    /// What an Array can hold: a `logical`, `integer`, `double` or `character` scalar, or a ry
    /// value (a record, tuple, tag, Array, `Dict` or `Set`).
    ArrayItem,
}

impl Constraint {
    /// Shared by rendering and parsing, so every constraint the checker prints can
    /// be written back in an annotation.
    pub const SPELLINGS: [(&'static str, Constraint); 5] = [
        ("numeric", Constraint::Numeric),
        ("atomic", Constraint::AtomicElement),
        ("key", Constraint::Key),
        ("scalar numeric", Constraint::ScalarNumeric),
        ("item", Constraint::ArrayItem),
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
            (Key, AtomicElement | ArrayItem) | (AtomicElement | ArrayItem, Key) => Key,
            (ArrayItem, AtomicElement) | (AtomicElement, ArrayItem) => Key,
            (ArrayItem, Numeric) | (Numeric, ArrayItem) => ScalarNumeric,
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
pub fn contains_unknown(_db: &dyn Db, ty: Ty<'_>) -> bool {
    ty.entry.contents & TypeEntry::HAS_UNKNOWN != 0
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
/// [`TYPE_SIZE_CEILING`] and stored when the type is built. Hash-consing makes types DAGs, so a
/// self-referential record's tree grows exponentially while its distinct nodes stay few.
pub fn type_size<'db>(_db: &'db dyn Db, ty: Ty<'db>) -> u64 {
    ty.entry.size
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
            inferred: function.inferred,
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
    for_each_child(ty.kind(db), visit);
}

fn for_each_child<'db>(kind: &TyKind<'db>, visit: &mut impl FnMut(Ty<'db>)) {
    match kind {
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
pub fn contains_inference_var<'db>(_db: &'db dyn Db, ty: Ty<'db>) -> bool {
    ty.entry.contents & TypeEntry::HAS_VAR != 0
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
    if ty.entry.contents & TypeEntry::HAS_RIGID == 0 {
        return ty;
    }
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
    if ty.entry.contents & TypeEntry::HAS_VAR == 0 {
        return ty;
    }
    if matches!(ty.kind(db), TyKind::Var(_)) {
        return unknown(db);
    }
    map_child_types(db, ty, &mut |child| erase_vars(db, child))
}

/// The top six bits of a hash pick the shard.
const TYPE_SHARDS: usize = 64;

/// Every type ever built, sharded by hash so parallel checkers rarely contend. Entries are
/// leaked, so a `&'static` to one is the type's identity.
static TYPES: LazyLock<[Mutex<HashTable<&'static TypeEntry>>; TYPE_SHARDS]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(HashTable::new())));

/// Slots in each thread's cache of recently interned types.
const RECENT_TYPES: usize = 1024;

thread_local! {
    /// A direct-mapped cache in front of the shards. A checker interns the same few types
    /// (scalars, `Unknown`, the signature it is applying) over and over, and those all hash to
    /// the same shards, so without it parallel checkers queue on a handful of mutexes.
    static RECENT: [Cell<Option<&'static TypeEntry>>; RECENT_TYPES] =
        const { [const { Cell::new(None) }; RECENT_TYPES] };
}

fn intern_type(kind: TyKind<'static>) -> &'static TypeEntry {
    let hash = FxBuildHasher.hash_one(&kind);
    let slot = (hash as usize) % RECENT_TYPES;
    let recent = RECENT.with(|recent| recent[slot].get());
    if let Some(entry) = recent
        && entry.kind == kind
    {
        return entry;
    }
    let interned = intern_type_shared(kind, hash);
    RECENT.with(|recent| recent[slot].set(Some(interned)));
    interned
}

fn intern_type_shared(kind: TyKind<'static>, hash: u64) -> &'static TypeEntry {
    let rehash = |entry: &&'static TypeEntry| FxBuildHasher.hash_one(&entry.kind);
    // The table picks buckets from the low bits, so the shard comes from the high ones.
    let shard = &TYPES[(hash >> 58) as usize];
    // A poisoned shard means another thread died mid-insert; the table still holds only
    // complete, leaked types, so keep serving it.
    let mut shard = shard
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match shard.find(hash, |entry| entry.kind == kind) {
        Some(&interned) => interned,
        None => {
            let mut size: u64 = 1;
            let mut contents = match kind {
                TyKind::Var(_) => TypeEntry::HAS_VAR,
                TyKind::Rigid(_) => TypeEntry::HAS_RIGID,
                TyKind::Unknown => TypeEntry::HAS_UNKNOWN,
                TyKind::Named(..) => TypeEntry::HAS_NAMED,
                _ => 0,
            };
            for_each_child(&kind, &mut |child| {
                size = size.saturating_add(child.entry.size).min(TYPE_SIZE_CEILING);
                contents |= child.entry.contents;
            });
            let interned: &'static TypeEntry = Box::leak(Box::new(TypeEntry {
                kind,
                size,
                contents,
            }));
            shard.insert_unique(hash, interned, rehash);
            interned
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::RootDatabase};

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
                    name: Name::new(&db, "x"),
                    ty: scalar(&db, Atomic::Integer),
                    optional: true,
                    default: Some(var),
                }],
                variadic: None,
                ret: scalar(&db, Atomic::Integer),
                inferred: false,
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
                inferred: false,
            }),
        );
        let f2 = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: vec![b],
                named: Vec::new(),
                variadic: None,
                ret: scalar(&db, Atomic::Double),
                inferred: false,
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
