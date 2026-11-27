use {
    crate::{
        Db,
        hir::Symbol,
        intern::{CHUNK_LENGTH, Column, FirstChunk, IdTable},
    },
    bumpalo::Bump,
    rustc_hash::{FxBuildHasher, FxHashMap},
    smallvec::SmallVec,
    std::{
        fmt,
        hash::{BuildHasher, Hash, Hasher},
        iter,
        marker::PhantomData,
        mem,
        num::NonZeroU32,
    },
    thin_vec::ThinVec,
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
    pub fn new<D: ?Sized>(_db: &'db D, text: impl IntoSymbol) -> Name<'db> {
        Name {
            symbol: text.into_symbol(),
            db: PhantomData,
        }
    }

    pub fn symbol(self) -> Symbol {
        self.symbol
    }

    pub fn text<D: ?Sized>(self, _db: &D) -> &'static str {
        self.symbol.as_str()
    }
}

/// A spelling, or a symbol that already interns one: a symbol passes through without the
/// hash and table probe that interning its text again would cost.
pub trait IntoSymbol {
    fn into_symbol(self) -> Symbol;
}

impl IntoSymbol for Symbol {
    fn into_symbol(self) -> Symbol {
        self
    }
}

impl IntoSymbol for &Symbol {
    fn into_symbol(self) -> Symbol {
        *self
    }
}

impl IntoSymbol for &str {
    fn into_symbol(self) -> Symbol {
        Symbol::new(self)
    }
}

impl IntoSymbol for &&str {
    fn into_symbol(self) -> Symbol {
        Symbol::new(self)
    }
}

impl IntoSymbol for String {
    fn into_symbol(self) -> Symbol {
        Symbol::new(&self)
    }
}

impl IntoSymbol for &String {
    fn into_symbol(self) -> Symbol {
        Symbol::new(self)
    }
}

/// A type: a four-byte id into the process-wide type store. Structurally equal kinds share
/// one id, so equality and hashing compare the id, and `kind` is an index into a column that
/// never moves or frees an entry. The checker builds and compares types far more often than
/// anything else, which is why none of those operations consults a database or takes a lock.
///
/// The id's low 27 bits are the store index. The bits above say what the type is and what
/// occurs anywhere in its tree, so asking whether a type is an inference variable, and the
/// walks that only rewrite one kind, are a bit test that reads no memory. The lifetime is
/// phantom: no type borrows from a database, which is what lets the store outlive them all.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ty<'db> {
    bits: NonZeroU32,
    db: PhantomData<&'db ()>,
}

/// An inference variable occurs in the type.
const HAS_VAR: u32 = 1 << 31;
/// A `Named` application occurs in the type, which resolution may expand as an alias.
const HAS_NAMED: u32 = 1 << 30;
const HAS_RIGID: u32 = 1 << 29;
const HAS_UNKNOWN: u32 = 1 << 28;
/// The type is an inference variable itself, the question unification asks of every type
/// it touches.
const IS_VAR: u32 = 1 << 27;
const CONTENT_BITS: u32 = HAS_VAR | HAS_NAMED | HAS_RIGID | HAS_UNKNOWN;
const INDEX_BITS: u32 = IS_VAR - 1;

/// The kinds without a payload that needs the store. Their ids are constants, so building
/// one costs nothing, and they are the store's first entries from the start. Index 0 is
/// never an id, which keeps an `Option<Ty>` the size of a `Ty`.
const LEAF_KINDS: [TyKind<'static>; LEAVES] = [
    TyKind::Any,
    TyKind::Any,
    TyKind::Unknown,
    TyKind::Null,
    TyKind::OtherTags,
    TyKind::Scalar(Atomic::Logical),
    TyKind::Scalar(Atomic::Integer),
    TyKind::Scalar(Atomic::Double),
    TyKind::Scalar(Atomic::Complex),
    TyKind::Scalar(Atomic::Character),
    TyKind::Scalar(Atomic::Raw),
    TyKind::Native(NativeScalar::I64),
    TyKind::Native(NativeScalar::F64),
    TyKind::Native(NativeScalar::Bool),
    TyKind::Native(NativeScalar::Str),
];
const LEAVES: usize = 15;
/// The inference variables whose ids are constants too: they fill the rest of the store's
/// first chunk, right after the leaves. An item numbers its variables from zero and rarely
/// reaches this many, so making a variable's type and reading the variable back out of it
/// are arithmetic on the id.
const CONSTANT_VARS: usize = CHUNK_LENGTH - LEAVES;
const ANY: u32 = 1;
const UNKNOWN: u32 = 2 | HAS_UNKNOWN;
const NULL: u32 = 3;
const OTHER_TAGS: u32 = 4;
const FIRST_SCALAR: u32 = 5;
const FIRST_NATIVE: u32 = 11;

impl<'db> Ty<'db> {
    pub fn new<D: ?Sized>(_db: &'db D, kind: TyKind<'_>) -> Ty<'db> {
        let bits = match kind {
            TyKind::Any => ANY,
            TyKind::Unknown => UNKNOWN,
            TyKind::Null => NULL,
            TyKind::OtherTags => OTHER_TAGS,
            TyKind::Scalar(atomic) => FIRST_SCALAR + atomic as u32,
            TyKind::Native(native) => FIRST_NATIVE + native as u32,
            TyKind::Var(InferenceVar(var)) if (var as usize) < CONSTANT_VARS => {
                (LEAVES as u32 + var) | HAS_VAR | IS_VAR
            }
            _ => return intern_type(kind),
        };
        Ty::from_bits(bits)
    }

    pub fn kind<D: ?Sized>(self, _db: &D) -> &'db TyKind<'db> {
        // SAFETY: an id is a leaf, which the column holds from the start, or was made by
        // `insert_type` after it wrote this entry, and reached this thread through the type
        // table or another synchronized hand-off.
        unsafe { KINDS.get_unchecked((self.bits.get() & INDEX_BITS) as usize) }
    }

    /// The variable the type is, if it is one. Read off the id, except for a variable
    /// numbered past the constant ones.
    pub fn as_var<D: ?Sized>(self, db: &D) -> Option<InferenceVar> {
        let bits = self.bits.get();
        if bits & IS_VAR == 0 {
            return None;
        }
        let index = (bits & INDEX_BITS) as usize;
        if index < CHUNK_LENGTH {
            return Some(InferenceVar((index - LEAVES) as u32));
        }
        match self.kind(db) {
            TyKind::Var(var) => Some(*var),
            _ => None,
        }
    }

    pub fn is_var(self) -> bool {
        self.bits.get() & IS_VAR != 0
    }

    /// Whether resolving through an inference table can change the type: only a variable or
    /// an alias application inside it can.
    pub(crate) fn may_resolve(self) -> bool {
        self.bits.get() & (HAS_VAR | HAS_NAMED) != 0
    }

    /// Every name a `Named` application in the type applies, each once. Empty for a type that
    /// applies none.
    pub(crate) fn applied_names(self) -> &'db [Name<'db>] {
        if self.bits.get() & HAS_NAMED == 0 {
            return &[];
        }
        self.facts().applied_names
    }

    fn facts(self) -> &'static TypeFacts {
        // SAFETY: as in `kind`; `insert_type` writes both columns before it makes the id.
        unsafe { FACTS.get_unchecked((self.bits.get() & INDEX_BITS) as usize) }
    }

    fn from_bits(bits: u32) -> Ty<'db> {
        Ty {
            // The index is never 0: the leaves start at 1 and the store's ids after them.
            bits: NonZeroU32::new(bits).unwrap_or(NonZeroU32::MIN),
            db: PhantomData,
        }
    }
}

/// A name as the key of a tracked function: salsa keys a one-argument query by one of its
/// own structs, and [`Name`] lives outside every database.
#[salsa::interned(revisions = usize::MAX)]
pub struct QueryName<'db> {
    pub name: Name<'db>,
}

impl Hash for Name<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.symbol.index().hash(state);
    }
}

impl fmt::Debug for Ty<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.kind(&()), formatter)
    }
}

impl fmt::Debug for Name<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.symbol.as_str(), formatter)
    }
}

// SAFETY: both index process-wide tables that never free or move an entry, so a value from
// an older revision stays valid in every later one.
unsafe impl salsa::SalsaValue for Ty<'_> {}
unsafe impl salsa::SalsaValue for Name<'_> {}

/// What a type is, read from the store. A kind with several children borrows them from the
/// store, so reading a type copies nothing and building one that already exists allocates
/// nothing: [`Ty::new`] takes the children borrowed from wherever the caller has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TyKind<'db> {
    /// Compatible with everything, silently.
    Any,
    /// An absent or unmodeled fact; strict mode reports its origins.
    Unknown,
    Null,
    Scalar(Atomic),
    /// One of ry's own scalars: a number type, `Bool` or `Str`. None holds `NA`, and none fits
    /// another; only `F64`, `Bool` and `Str` fit an R type, the one whose representation they share.
    Native(NativeScalar),
    Vector(Ty<'db>),
    NamedVector(Ty<'db>),
    List(Ty<'db>),
    NamedList(Ty<'db>),
    /// `list{A, B}`.
    Tuple(&'db [Ty<'db>]),
    /// `list{a: A, b: B}`; fields keep declaration order.
    Record(&'db [RecordField<'db>]),
    /// `list{a: A, ..}`: a record-like list with at least these fields. A record with more
    /// fits it, and only the listed fields can be read through it.
    OpenRecord(&'db [RecordField<'db>]),
    /// `environment{count: integer}`: an environment whose listed bindings keep their types.
    /// It is mutable, so a write is checked against the field and never changes the type.
    Environment(&'db [RecordField<'db>]),
    /// `Tuple{A, B}`: ry's native tuple, which is not a list and never fits a list type.
    NativeTuple(&'db [Ty<'db>]),
    /// `Record{a: A, b: B}`: ry's native record. Field order is not part of the type, so the
    /// fields are kept sorted by name and every field is required. A row, as in
    /// `Record{a: A, ..R}`, stands for the fields the type does not list; it is a type variable
    /// or parameter, never a record, because `native_record_with_row` merges a resolved one.
    NativeRecord(&'db [NativeField<'db>], Option<Ty<'db>>),
    /// `:Name` or `:Name(...)`. The payload is always a `NativeTuple` (positional) or a
    /// `NativeRecord` (named), so a payload types and compares exactly like that literal.
    Tag(Name<'db>, Option<Ty<'db>>),
    /// `..`, the member that makes a tag union open: it stands for every tag the union does
    /// not list. Only `union_of` places it, always last.
    OtherTags,
    /// A table whose type is a record of column vectors: ry's `DataFrame{a: A[], b: B[]}`,
    /// a `LazyFrame{...}` plan, a `GroupBy{...}`, or a `data.table{...}` whose columns are known.
    /// `None` is a table whose columns are not known (`DataFrame`), which every column reference
    /// treats as silent. A row, as in `DataFrame{a: A[], ..R}`, stands for the columns the type
    /// does not list; only a frame with known columns has one, and it is a type variable or
    /// parameter, never a frame, because `frame_with_row` merges a resolved one.
    Frame(FrameKind, Option<&'db [RecordField<'db>]>, Option<Ty<'db>>),
    Function(&'db FunctionType<'db>),
    /// Normalized; build only through `union_of`, never directly.
    Union(&'db [Ty<'db>]),
    Named(Name<'db>, &'db [Ty<'db>]),
    /// Inference-scoped: resolved through the table, never stored in exported schemes.
    Var(InferenceVar),
    /// A skolem from an explicit `<T>` binder: refuses to bind while its scope is
    /// checked, generalizes back out afterwards.
    Rigid(Name<'db>),
    /// The result of an operator whose operand is still open: `Promoted<T>` is `T` for a ry
    /// number and `double` for an R one, and `Compared<T>` is `Bool` for a ry value and
    /// `logical` for an R one. The operand is a type variable or parameter, or a union holding
    /// one, because `family_result` decides the outcome as soon as the operand is known.
    Family(FamilyResult, Ty<'db>),
}

/// Which outcome a [`TyKind::Family`] picks for each side of the R and ry divide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum FamilyResult {
    /// `x * 2.5`, `x / y`, `x ^ 2`: ry's numbers keep their type, R's give `double`.
    Arithmetic,
    /// `x > 0`: ry's values give `Bool`, R's give `logical`.
    Comparison,
}

impl FamilyResult {
    pub fn head(self) -> &'static str {
        match self {
            FamilyResult::Arithmetic => "Promoted",
            FamilyResult::Comparison => "Compared",
        }
    }

    pub fn from_head(head: &str) -> Option<FamilyResult> {
        match head {
            "Promoted" => Some(FamilyResult::Arithmetic),
            "Compared" => Some(FamilyResult::Comparison),
            _ => None,
        }
    }

    /// The outcome for an operand on ry's side.
    pub fn ry_outcome<'db>(self, db: &'db dyn Db, operand: Ty<'db>) -> Ty<'db> {
        match self {
            FamilyResult::Arithmetic => operand,
            FamilyResult::Comparison => Ty::new(db, TyKind::Native(NativeScalar::Bool)),
        }
    }

    /// The outcome for an operand on R's side.
    pub fn r_outcome<'db>(self, db: &'db dyn Db) -> Ty<'db> {
        Ty::new(
            db,
            TyKind::Scalar(match self {
                FamilyResult::Arithmetic => Atomic::Double,
                FamilyResult::Comparison => Atomic::Logical,
            }),
        )
    }
}

/// The result of `result` over `operand`, decided as far as the operand allows: a ry
/// scalar gives the ry outcome, a known R type the R one, and an open operand keeps the
/// family. Inference decides an open operand by its constraint (`InferenceTable::family`).
pub fn family_result<'db>(db: &'db dyn Db, result: FamilyResult, operand: Ty<'db>) -> Ty<'db> {
    match operand.kind(db) {
        TyKind::Native(_) => result.ry_outcome(db, operand),
        TyKind::Family(_, inner) => family_result(db, result, *inner),
        // A nominal type may stand for a ry number, which only the definitions in scope tell.
        TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Named(..) => {
            Ty::new(db, TyKind::Family(result, operand))
        }
        TyKind::Unknown | TyKind::Any => operand,
        // R's arithmetic and comparisons keep a vector's shape.
        TyKind::Vector(_) | TyKind::NamedVector(_) => {
            Ty::new(db, TyKind::Vector(result.r_outcome(db)))
        }
        TyKind::Union(members)
            if members
                .iter()
                .any(|member| matches!(member.kind(db), TyKind::Var(_) | TyKind::Rigid(_))) =>
        {
            Ty::new(db, TyKind::Family(result, operand))
        }
        TyKind::Union(members)
            if members
                .iter()
                .all(|member| matches!(member.kind(db), TyKind::Native(_))) =>
        {
            match result {
                FamilyResult::Arithmetic => operand,
                FamilyResult::Comparison => result.ry_outcome(db, operand),
            }
        }
        // `==` compares two ry records or lists as ry does.
        _ if is_ry_value_type(db, operand) => result.ry_outcome(db, operand),
        _ => result.r_outcome(db),
    }
}

/// What a table type is. The kinds never fit each other: a `data.table` runs data.table's code,
/// a `data.frame` runs base R's, a `LazyFrame` holds a plan rather than rows, and a `GroupBy`
/// only feeds `GroupBy.agg`.
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
    /// An R `data.frame` whose columns are known, from `data.frame(...)`.
    RDataFrame,
}

impl FrameKind {
    pub fn head(self) -> &'static str {
        match self {
            FrameKind::DataFrame => "DataFrame",
            FrameKind::LazyFrame => "LazyFrame",
            FrameKind::GroupBy { lazy: false, .. } => "GroupBy",
            FrameKind::GroupBy { lazy: true, .. } => "LazyGroupBy",
            FrameKind::DataTable => "data.table",
            FrameKind::RDataFrame => "data.frame",
        }
    }

    /// The R class of a kind whose values are R objects: such a frame is its class with the
    /// columns known, and passes wherever the class does.
    pub fn class(self) -> Option<&'static str> {
        match self {
            FrameKind::DataTable => Some("data.table"),
            FrameKind::RDataFrame => Some("data.frame"),
            _ => None,
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
            "data.frame" => FrameKind::RDataFrame,
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

/// ry's own scalar types, in the order the standard library documents them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum NativeScalar {
    I64,
    F64,
    Bool,
    Str,
}

impl NativeScalar {
    pub const ALL: [NativeScalar; 4] = [
        NativeScalar::I64,
        NativeScalar::F64,
        NativeScalar::Bool,
        NativeScalar::Str,
    ];

    pub fn name(self) -> &'static str {
        match self {
            NativeScalar::I64 => "I64",
            NativeScalar::F64 => "F64",
            NativeScalar::Bool => "Bool",
            NativeScalar::Str => "Str",
        }
    }

    pub fn from_name(text: &str) -> Option<NativeScalar> {
        NativeScalar::ALL
            .into_iter()
            .find(|scalar| scalar.name() == text)
    }

    pub fn is_number(self) -> bool {
        matches!(self, NativeScalar::I64 | NativeScalar::F64)
    }

    pub fn is_integer(self) -> bool {
        self == NativeScalar::I64
    }

    pub fn is_fraction(self) -> bool {
        self == NativeScalar::F64
    }

    /// The R type whose representation this type shares, which it therefore fits.
    pub fn shared_atomic(self) -> Option<Atomic> {
        match self {
            NativeScalar::F64 => Some(Atomic::Double),
            NativeScalar::Bool => Some(Atomic::Logical),
            NativeScalar::Str => Some(Atomic::Character),
            NativeScalar::I64 => None,
        }
    }

    /// Whether this number type holds `literal` exactly: `I64` holds a whole number in its range
    /// and `F64` any finite value.
    pub fn holds(self, literal: &NumberLiteral) -> bool {
        match self {
            NativeScalar::I64 => match literal.whole {
                Some(magnitude) if literal.negative => magnitude <= i64::MIN.unsigned_abs().into(),
                Some(magnitude) => magnitude <= i64::MAX as u128,
                None => false,
            },
            NativeScalar::F64 => literal.value.is_finite(),
            NativeScalar::Bool | NativeScalar::Str => false,
        }
    }
}

/// A number literal's value as written, with a `-` written directly before it folded in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberLiteral {
    pub negative: bool,
    /// The exact magnitude when the literal is a whole number, written as digits, hex or with an
    /// exponent that leaves no fraction.
    pub whole: Option<u128>,
    pub value: f64,
    /// The digits after the decimal point, `None` when the literal has an exponent.
    pub decimal_places: Option<u32>,
    /// Written with R's `L` suffix, so its R type is `integer`.
    pub integer: bool,
    /// Written with a decimal point or an exponent (`2.0`, `1e3`) and no `L`, which makes `F64`
    /// the ry number type it takes when nothing decides one, as in Roc. `1e3L` is R's `integer`.
    pub fractional_form: bool,
}

impl NumberLiteral {
    /// Parses an unsuffixed R number literal (`5`, `5L`, `0x1F`, `1.5`, `1e3`).
    pub fn parse(text: &str, negative: bool) -> Option<NumberLiteral> {
        let integer = text.ends_with('L');
        let digits = text.trim_end_matches('L');
        let (whole, value, decimal_places) = if let Some(hex) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            let whole = u128::from_str_radix(hex, 16).ok()?;
            (Some(whole), whole as f64, Some(0))
        } else {
            let value: f64 = digits.parse().ok()?;
            let has_exponent = digits.contains(['e', 'E']);
            let decimal_places = match digits.split_once('.') {
                _ if has_exponent => None,
                Some((_, fraction)) => Some(fraction.trim_end_matches('0').len() as u32),
                None => Some(0),
            };
            let whole = if !has_exponent && decimal_places == Some(0) {
                digits
                    .split('.')
                    .next()
                    .and_then(|integral| integral.parse::<u128>().ok())
            } else if value.fract() == 0.0 && value.abs() < 2f64.powi(53) {
                Some(value.abs() as u128)
            } else {
                None
            };
            (whole, value, decimal_places)
        };
        let fractional_form = !integer
            && !digits.starts_with("0x")
            && !digits.starts_with("0X")
            && digits.contains(['.', 'e', 'E']);
        Some(NumberLiteral {
            negative,
            whole,
            value: if negative { -value } else { value },
            decimal_places,
            integer,
            fractional_form,
        })
    }

    pub fn is_fraction(&self) -> bool {
        self.whole.is_none()
    }

    /// The ry number the literal takes when nothing decides one: `F64` for a fraction, a
    /// fractional spelling, or a whole number past `I64`'s range, and `I64` otherwise.
    pub fn default_native(&self) -> NativeScalar {
        if self.is_fraction() || self.fractional_form || !NativeScalar::I64.holds(self) {
            NativeScalar::F64
        } else {
            NativeScalar::I64
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, salsa::SalsaValue)]
pub struct InferenceVar(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RecordField<'db> {
    pub name: Name<'db>,
    pub ty: Ty<'db>,
    pub optional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NativeField<'db> {
    pub name: Name<'db>,
    pub ty: Ty<'db>,
}

/// Unlike a record field, carries the default's type: R evaluates the default
/// when the argument is omitted, so it is what the parameter holds on that path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
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

/// A function's signature. The lists borrow, from the store when the signature is read from
/// a type and from the caller when it is being built, so changing one is a copy of the struct
/// with that list pointed at the new items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionType<'db> {
    pub positional: &'db [Ty<'db>],
    pub named: &'db [Parameter<'db>],
    /// Parameters after `...` match by name only.
    pub variadic: Option<RestParameter<'db>>,
    pub ret: Ty<'db>,
    /// The signature was inferred from an unannotated body, so a scalar parameter type is a
    /// claim of how the body uses the value, not a declared length.
    pub inferred: bool,
    /// The body returns one of two values chosen by a logical flag with a literal default,
    /// so a call that leaves the flag out or passes a literal returns that branch alone.
    pub flag_return: Option<FlagReturn<'db>>,
    /// Formals the body reads only through `substitute()`, so a call's argument there is code
    /// the callee captures rather than a value the caller computes. Inferred only.
    pub quoted: &'db [Name<'db>],
    /// Optional formals that a forced formal's default reads: `function(x, y = x) y` may be
    /// called as `g(y = 1)` but not as `g()`. Inferred only.
    pub default_reads: &'db [DefaultRead<'db>],
    /// Formals the body tests with `missing()`, so a missing formal the caller forwards there
    /// may stay unread even when the formal has a default. Inferred only.
    pub missing_tested: &'db [Name<'db>],
    /// Formals the body reads only as the whole condition of an `if` or `while`, which takes a
    /// string through `as.logical`. Inferred only.
    pub condition_only: &'db [Name<'db>],
    /// Formals whose argument's shape the scalar result takes, because every value the body
    /// returns is vectorised math over that formal (`function(x) sqrt(x) * 2`). Inferred only.
    pub elementwise: &'db [Name<'db>],
    /// The formal an S3 generic's `UseMethod` dispatches on. Dispatch forces a supplied
    /// argument, so forwarding a missing formal there fails even though a call may leave the
    /// formal out. Inferred only.
    pub dispatch: Option<Name<'db>>,
    /// The body returns `NULL` only through `if (!predicate(parameter)) return(NULL)`, so a
    /// call where the caller already showed `predicate` holds of its argument returns the
    /// other members alone, or only from the last `else` of a chain of tests of `parameter`'s
    /// class, which an argument that may carry a class passes. Inferred only.
    pub null_unless: Option<NullUnless<'db>>,
    /// The body ends in `Reduce(f, x, init = init)` over its own formals, with `f` a formal
    /// or a closure that only forwards to one, so a call folds like a direct `Reduce` and its
    /// accumulator may widen past the start. Inferred only.
    pub reduces: Option<ReduceForward<'db>>,
    /// Two operand types the body compares that stay separate variables, because R compares
    /// across its types (`function(x, lo) x < lo` takes a `Date` and a string). A ry value
    /// compares only with its own type, so a call where either side is one must make both
    /// that type. Inferred only, and part of the type's identity, so two signatures that print
    /// alike can differ in it.
    pub compared: &'db [ComparedPair<'db>],
    /// Built from a call on a value of unknown type, so its parameters are the arguments that
    /// call passed rather than formals: another call may pass a different set of names, which
    /// the real callee's defaults or `...` may accept.
    pub from_calls: bool,
}

impl<'db> FunctionType<'db> {
    /// A declared signature with these parameters and nothing an inferred body adds.
    pub fn declared(
        positional: &'db [Ty<'db>],
        named: &'db [Parameter<'db>],
        variadic: Option<RestParameter<'db>>,
        ret: Ty<'db>,
    ) -> FunctionType<'db> {
        FunctionType {
            positional,
            named,
            variadic,
            ret,
            inferred: false,
            flag_return: None,
            quoted: &[],
            default_reads: &[],
            missing_tested: &[],
            condition_only: &[],
            elementwise: &[],
            dispatch: None,
            null_unless: None,
            reduces: None,
            compared: &[],
            from_calls: false,
        }
    }
}

/// purrr's `reduce(.x, .f, ..., .init)` calling `Reduce(function(x, y) .f(x, y, ...), .x,
/// init = .init)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ReduceForward<'db> {
    pub function: Name<'db>,
    pub x: Name<'db>,
    pub init: Option<Name<'db>>,
}

/// `ns_env <- function(package) { if (!is_loaded(package)) return(NULL); asNamespace(package) }`,
/// or with `class_fallback` the class dispatch `if (is.mcmc(x)) 1 else if (is.mcmc.list(x))
/// length(x) else NULL`, whose `NULL` is the answer for an object of no class it handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NullUnless<'db> {
    pub predicate: Name<'db>,
    pub parameter: Name<'db>,
    pub class_fallback: bool,
}

/// `formal` is required by a call that leaves out `default_of`, whose default reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct DefaultRead<'db> {
    pub formal: Name<'db>,
    pub default_of: Name<'db>,
}

/// The two operand types of a comparison between different open values
/// ([`FunctionType::compared`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ComparedPair<'db> {
    pub left: Ty<'db>,
    pub right: Ty<'db>,
}

/// `function(q, valueOnly = TRUE) if (valueOnly) prob else list(value = prob)`: `ret` is the
/// union of both branches, and these are the branches themselves. Inferred only, since an
/// annotation has no syntax for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct FlagReturn<'db> {
    pub parameter: Name<'db>,
    pub default: bool,
    pub when_true: Ty<'db>,
    pub when_false: Ty<'db>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct RestParameter<'db> {
    pub element: Ty<'db>,
    pub preceding_named: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, salsa::SalsaValue)]
pub enum Constraint {
    #[default]
    Unconstrained,
    /// A number, or a class whose arithmetic takes two of it (`Money + Money`), so `a + b` on
    /// two values of the type is legal R.
    Numeric,
    /// A number of either family, or a class whose arithmetic takes two of it, where arithmetic
    /// keeps the type: what `a + b` asks of two values of one type when the sum has that type.
    /// It admits what `Numeric` does and the ry numbers `I64` and `F64`, as Roc's `Num a` does,
    /// so `function(a, b) a * b` multiplies two `F64`s as well as two `double`s.
    Num,
    /// A number, or a class that declares any arithmetic method: `Date + 1` is legal R, while
    /// `Date + Date` is not. Two values of it combine only once it is `Numeric`.
    Arithmetic,
    /// What `arithmetic` admits and the ry numbers: what `x + 1` asks of `x`, because R adds
    /// the literal to a number or a `Date` and a ry number takes it as its own type.
    ArithmeticOrNumber,
    /// What `arithmetic` admits and `F64`: what `x * 0.5` and `x / 2` ask of `x`, because an
    /// `I64` holds no fraction and divides only with `%/%`.
    ArithmeticOrF64,
    /// What `numeric` admits and `F64`: what `a / b` asks of two values of one type.
    NumericOrF64,
    AtomicElement,
    /// A dictionary key or set element: a `logical`, `integer`, `double` or `character` scalar.
    /// `complex` and `raw` are left out because they have no total order or hash that the
    /// runtime's collections use.
    Key,
    ScalarNumeric,
    /// What a `List` can hold: a ry value (a ry scalar, record, tuple, tag, `List`, `Dict` or
    /// `Set`).
    ArrayItem,
    /// A ry number: `I64` or `F64`.
    Number,
    /// A number literal known to be whole and used as a ry number, which only `I64` admits. It
    /// is never generalized, so no annotation spells it.
    Int,
    /// A number literal known to be fractional and used as a ry number, which only `F64` admits.
    /// Like `Int`, it is never generalized.
    Frac,
    /// A string literal compared with an open value, which only `Str` admits on ry's side,
    /// because ry compares a `Str` only with a `Str`. Like `Int`, it is never generalized.
    Str,
    /// Every ry type but a function, the values `==` compares structurally.
    Eq,
    /// An R value a comparison takes: an atomic value other than `complex` or `raw` in any
    /// vector shape, a list (compared element by element) or a class, which R's `Ops` methods
    /// compare.
    Comparable,
    /// An R value `==` takes: what `comparable` admits, and `complex` and `raw`, which R tests
    /// for equality but does not order.
    Equatable,
    /// A ry value with an order: `I64`, `F64` or `Str`.
    Ordered,
    /// What `comparable` admits and the ry numbers: what `x > 0` asks of `x`, because R compares
    /// any atomic value with a number while ry compares only a number with one.
    ComparableOrNumber,
    /// What `comparable` admits and what `ordered` does: what `a < b` asks of two values of one
    /// type.
    ComparableOrOrdered,
    /// What `comparable` admits and `Str`: what `x < "a"` asks of `x`.
    ComparableOrStr,
    /// What `equatable` admits and the ry numbers: what `x == 0` asks of `x`.
    EquatableOrNumber,
    /// What `equatable` admits and `Str`: what `x == "a"` asks of `x`.
    EquatableOrStr,
    /// The type of a variable written with a whole-number literal (`total <- 0`), decided by its
    /// first use: a ry integer or fraction type, or R's `double` (`integer` for `0L`).
    WholeLiteral {
        integer: bool,
    },
    /// Like `WholeLiteral`, for a literal with a fraction (`1.5`): a ry fraction type or `double`.
    FracLiteral,
    /// The type of a variable written with a string literal (`name <- "ry"`): `Str` or R's
    /// `character`, decided by its first use like `WholeLiteral`.
    StrLiteral,
    /// The type of a variable written with `TRUE` or `FALSE`: `Bool` or R's `logical`.
    BoolLiteral,
}

impl Constraint {
    /// Shared by rendering and parsing, so every constraint the checker prints can
    /// be written back in an annotation.
    pub const SPELLINGS: [(&'static str, Constraint); 20] = [
        ("num", Constraint::Num),
        ("numeric", Constraint::Numeric),
        ("arithmetic", Constraint::Arithmetic),
        ("arithmetic or number", Constraint::ArithmeticOrNumber),
        ("arithmetic or F64", Constraint::ArithmeticOrF64),
        ("numeric or F64", Constraint::NumericOrF64),
        ("atomic", Constraint::AtomicElement),
        ("key", Constraint::Key),
        ("scalar numeric", Constraint::ScalarNumeric),
        ("item", Constraint::ArrayItem),
        ("number", Constraint::Number),
        ("eq", Constraint::Eq),
        ("comparable", Constraint::Comparable),
        ("ordered", Constraint::Ordered),
        ("comparable or number", Constraint::ComparableOrNumber),
        ("comparable or ordered", Constraint::ComparableOrOrdered),
        ("comparable or Str", Constraint::ComparableOrStr),
        ("equatable", Constraint::Equatable),
        ("equatable or number", Constraint::EquatableOrNumber),
        ("equatable or Str", Constraint::EquatableOrStr),
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

    /// The two halves of a constraint that admits numbers of both families: what it admits of
    /// R's types and what of ry's. `num` is `numeric` and `number`.
    pub fn families(self) -> Option<(Constraint, Constraint)> {
        use Constraint::*;
        Some(match self {
            Num => (Numeric, Number),
            ArithmeticOrNumber => (Arithmetic, Number),
            ArithmeticOrF64 => (Arithmetic, Frac),
            NumericOrF64 => (Numeric, Frac),
            ComparableOrNumber => (Comparable, Number),
            ComparableOrOrdered => (Comparable, Ordered),
            ComparableOrStr => (Comparable, Str),
            EquatableOrNumber => (Equatable, Number),
            EquatableOrStr => (Equatable, Str),
            _ => return None,
        })
    }

    /// The constraint that admits what `r` admits of R's types and what `ry` admits of ry's.
    fn of_families(r: Option<Constraint>, ry: Option<Constraint>) -> Option<Constraint> {
        use Constraint::*;
        match (r, ry) {
            (Some(Numeric), Some(Number)) => Some(Num),
            (Some(Arithmetic), Some(Number)) => Some(ArithmeticOrNumber),
            (Some(Arithmetic), Some(Frac)) => Some(ArithmeticOrF64),
            (Some(Numeric), Some(Frac)) => Some(NumericOrF64),
            (Some(Comparable), Some(Number)) => Some(ComparableOrNumber),
            (Some(Comparable), Some(Ordered)) => Some(ComparableOrOrdered),
            (Some(Comparable), Some(Str)) => Some(ComparableOrStr),
            (Some(Equatable), Some(Number)) => Some(EquatableOrNumber),
            (Some(Equatable), Some(Str)) => Some(EquatableOrStr),
            (Some(r), _) => Some(r),
            (None, ry) => ry,
        }
    }

    /// The part of the constraint R's types can meet: `arithmetic` for `arithmetic or number`.
    pub fn r_side(self) -> Constraint {
        self.families().map_or(self, |(r, _)| r)
    }

    /// The constraint a type must meet to satisfy both, or `None` when no type can: an R number
    /// is never a ry number, and an integer type is never a fraction type.
    pub fn join(self, other: Constraint) -> Option<Constraint> {
        use Constraint::*;
        // A constraint of both families meets another half by half, since no type is in both.
        match (self.families(), other.families()) {
            (Some((r, ry)), Some((other_r, other_ry))) => {
                return Constraint::of_families(r.join(other_r), ry.join(other_ry));
            }
            (Some((r, ry)), None) | (None, Some((r, ry))) => {
                let (both, single) = if self.families().is_some() {
                    (self, other)
                } else {
                    (other, self)
                };
                return match single {
                    Unconstrained | Eq => Some(both),
                    Number | Int | Frac | Str | ArrayItem | Ordered => ry.join(single),
                    // A string half admits no number, so a number literal is an R number.
                    WholeLiteral { .. } | FracLiteral if ry == Str => r.join(single),
                    // Every type a number literal admits is a number, so the literal stays open,
                    // unless only fractions are wanted.
                    WholeLiteral { .. } | FracLiteral if matches!(ry, Number | Ordered) => {
                        Some(single)
                    }
                    WholeLiteral { .. } | FracLiteral => Some(FracLiteral),
                    // `Key` has no exact meet (a key of either family that the half admits), so it
                    // takes the R side, where a key is an atomic scalar.
                    Key => r.join(AtomicElement),
                    // A value used both ways is an R number.
                    _ => r.join(single),
                };
            }
            (None, None) => {}
        }
        // `arithmetic` meets everything as `numeric` does, and only `numeric` itself or a
        // stronger constraint makes the result closed.
        let loosen = |joined: Option<Constraint>| match joined {
            Some(Numeric)
                if ![self, other]
                    .iter()
                    .any(|side| matches!(side, Numeric | Num)) =>
            {
                Some(Arithmetic)
            }
            joined => joined,
        };
        let tighten = |constraint: Constraint| match constraint {
            Arithmetic => Numeric,
            constraint => constraint,
        };
        if self == Arithmetic || other == Arithmetic {
            return match (self, other) {
                (Unconstrained | Eq | Arithmetic, Arithmetic)
                | (Arithmetic, Unconstrained | Eq) => Some(Arithmetic),
                _ => loosen(tighten(self).join(tighten(other))),
            };
        }
        Some(match (self, other) {
            (Unconstrained, c) | (c, Unconstrained) => c,
            (a, b) if a == b => a,
            (Eq, c) | (c, Eq) => c,
            // Every R type another R constraint admits is comparable, and every literal's R type
            // is, so the literal stays open and keeps its default.
            (
                Comparable,
                c @ (Numeric
                | ScalarNumeric
                | AtomicElement
                | WholeLiteral { .. }
                | FracLiteral
                | StrLiteral
                | BoolLiteral),
            )
            | (
                c @ (Numeric
                | ScalarNumeric
                | AtomicElement
                | WholeLiteral { .. }
                | FracLiteral
                | StrLiteral
                | BoolLiteral),
                Comparable,
            ) => c,
            (Comparable, Equatable) | (Equatable, Comparable) => Comparable,
            // A key also admits ry values, which `comparable` does not.
            (Comparable, Key) | (Key, Comparable) => AtomicElement,
            (Comparable, _) | (_, Comparable) => return None,
            // `equatable` admits every R value `comparable` does, and every R type the other R
            // constraints admit.
            (
                Equatable,
                c @ (Numeric
                | ScalarNumeric
                | AtomicElement
                | WholeLiteral { .. }
                | FracLiteral
                | StrLiteral
                | BoolLiteral),
            )
            | (
                c @ (Numeric
                | ScalarNumeric
                | AtomicElement
                | WholeLiteral { .. }
                | FracLiteral
                | StrLiteral
                | BoolLiteral),
                Equatable,
            ) => c,
            (Equatable, Key) | (Key, Equatable) => AtomicElement,
            (Equatable, _) | (_, Equatable) => return None,
            (Str, Ordered | ArrayItem | Key) | (Ordered | ArrayItem | Key, Str) => Str,
            (Str, StrLiteral) | (StrLiteral, Str) => Str,
            (Str, _) | (_, Str) => return None,
            (Ordered, c @ (Number | Int | Frac)) | (c @ (Number | Int | Frac), Ordered) => c,
            (Ordered, ArrayItem | Key) | (ArrayItem | Key, Ordered) => Ordered,
            (Ordered, WholeLiteral { .. }) | (WholeLiteral { .. }, Ordered) => Number,
            (Ordered, FracLiteral) | (FracLiteral, Ordered) => Frac,
            (Ordered, StrLiteral) | (StrLiteral, Ordered) => StrLiteral,
            (Ordered, _) | (_, Ordered) => return None,
            // An R integer literal and an R double literal have different R types, so they never
            // become one literal: `1L` beside `2.5` would make a `double` that holds `1L`.
            (WholeLiteral { integer: left }, WholeLiteral { integer: right }) if left != right => {
                return None;
            }
            (WholeLiteral { integer: true }, FracLiteral)
            | (FracLiteral, WholeLiteral { integer: true }) => return None,
            (WholeLiteral { .. }, FracLiteral) | (FracLiteral, WholeLiteral { .. }) => FracLiteral,
            (WholeLiteral { .. } | Number | ArrayItem | Key, Int)
            | (Int, WholeLiteral { .. } | Number | ArrayItem | Key) => Int,
            (FracLiteral | WholeLiteral { .. } | Number | ArrayItem | Key, Frac)
            | (Frac, FracLiteral | WholeLiteral { .. } | Number | ArrayItem | Key) => Frac,
            (WholeLiteral { .. } | FracLiteral | ArrayItem | Key, Number)
            | (Number, WholeLiteral { .. } | FracLiteral | ArrayItem | Key) => Number,
            (FracLiteral, Int) | (Int, FracLiteral) | (Int, Frac) | (Frac, Int) => return None,
            // Every literal is a key, so a key keeps the literal open; any other R constraint
            // makes it an R value. A text or logical literal is never a number, and the two never meet.
            (StrLiteral, Key) | (Key, StrLiteral) => StrLiteral,
            (literal @ (WholeLiteral { .. } | FracLiteral), Key)
            | (Key, literal @ (WholeLiteral { .. } | FracLiteral)) => literal,
            (BoolLiteral, Key) | (Key, BoolLiteral) => BoolLiteral,
            (StrLiteral, AtomicElement) | (AtomicElement, StrLiteral) => AtomicElement,
            (BoolLiteral, c @ (AtomicElement | Numeric | ScalarNumeric))
            | (c @ (AtomicElement | Numeric | ScalarNumeric), BoolLiteral) => c,
            (StrLiteral | BoolLiteral, _) | (_, StrLiteral | BoolLiteral) => return None,
            // A literal used where R's number constraints apply is an R number.
            (WholeLiteral { .. } | FracLiteral, c) | (c, WholeLiteral { .. } | FracLiteral) => c,
            (Number | Int | Frac, _) | (_, Number | Int | Frac) => return None,
            // `scalar numeric` admits no ry value.
            (ScalarNumeric, ArrayItem) | (ArrayItem, ScalarNumeric) => return None,
            (ScalarNumeric, _) | (_, ScalarNumeric) => ScalarNumeric,
            (Numeric, AtomicElement | Key) | (AtomicElement | Key, Numeric) => ScalarNumeric,
            (Key, AtomicElement) | (AtomicElement, Key) => Key,
            (Key, ArrayItem) | (ArrayItem, Key) => ArrayItem,
            (ArrayItem, AtomicElement | Numeric) | (AtomicElement | Numeric, ArrayItem) => {
                return None;
            }
            _ => unreachable!("all constraint pairs are covered"),
        })
    }

    /// The R type a literal constraint defaults to when nothing decided it.
    pub fn literal_default(self) -> Option<Atomic> {
        match self {
            Constraint::WholeLiteral { integer: true } => Some(Atomic::Integer),
            Constraint::WholeLiteral { integer: false } | Constraint::FracLiteral => {
                Some(Atomic::Double)
            }
            Constraint::StrLiteral => Some(Atomic::Character),
            Constraint::BoolLiteral => Some(Atomic::Logical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct TypeScheme<'db> {
    pub binders: ThinVec<Binder<'db>>,
    pub body: Ty<'db>,
}

/// A type parameter of a scheme: `T`, `T: numeric` or `T: list[U] | U[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Binder<'db> {
    pub name: Name<'db>,
    pub bound: Bound<'db>,
}

/// What a type parameter ranges over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum Bound<'db> {
    Constraint(Constraint),
    /// The types that fit this type, usually a union. `function(path) { vapply(path, f, ...);
    /// path }` takes any vector or list and returns the argument's own type, which the union
    /// alone cannot say: as the parameter's type it would hand every caller back the whole union.
    Type(Ty<'db>),
}

impl<'db> Binder<'db> {
    pub fn constrained(name: Name<'db>, constraint: Constraint) -> Binder<'db> {
        Binder {
            name,
            bound: Bound::Constraint(constraint),
        }
    }

    /// `Unconstrained` for a type bound, which says what the binder admits on its own.
    pub fn constraint(self) -> Constraint {
        match self.bound {
            Bound::Constraint(constraint) => constraint,
            Bound::Type(_) => Constraint::Unconstrained,
        }
    }

    pub fn type_bound(self) -> Option<Ty<'db>> {
        match self.bound {
            Bound::Type(bound) => Some(bound),
            Bound::Constraint(_) => None,
        }
    }
}

impl<'db> TypeScheme<'db> {
    pub fn monomorphic(body: Ty<'db>) -> TypeScheme<'db> {
        TypeScheme {
            binders: ThinVec::new(),
            body,
        }
    }
}

/// Named types are not expanded: their representation is a separate declaration.
pub fn contains_unknown(_db: &dyn Db, ty: Ty<'_>) -> bool {
    ty.bits.get() & HAS_UNKNOWN != 0
}

pub fn any(_db: &dyn Db) -> Ty<'_> {
    Ty::from_bits(ANY)
}

pub fn unknown(_db: &dyn Db) -> Ty<'_> {
    Ty::from_bits(UNKNOWN)
}

pub fn null(_db: &dyn Db) -> Ty<'_> {
    Ty::from_bits(NULL)
}

pub fn scalar(_db: &dyn Db, atomic: Atomic) -> Ty<'_> {
    Ty::from_bits(FIRST_SCALAR + atomic as u32)
}

pub fn native_scalar(_db: &dyn Db, native: NativeScalar) -> Ty<'_> {
    Ty::from_bits(FIRST_NATIVE + native as u32)
}

/// `List<element>`, ry's list.
pub fn array<'db>(db: &'db dyn Db, element: Ty<'db>) -> Ty<'db> {
    Ty::new(db, TyKind::Named(Name::new(db, "List"), &[element]))
}

/// The item type of the standard library's `List<T>`, and `None` for any other type, including a
/// project's own type that happens to be called `List`.
pub fn array_element<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Ty<'db>> {
    match ty.kind(db) {
        TyKind::Named(name, arguments)
            if arguments.len() == 1
                && name.text(db) == "List"
                && crate::stubs::is_ry_type(db, "List") =>
        {
            Some(arguments[0])
        }
        _ => None,
    }
}

/// The element type of a ry `Set<T>`.
pub fn set_element<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<Ty<'db>> {
    match ty.kind(db) {
        TyKind::Named(name, arguments)
            if arguments.len() == 1
                && name.text(db) == "Set"
                && crate::stubs::is_ry_type(db, "Set") =>
        {
            Some(arguments[0])
        }
        _ => None,
    }
}

/// The key and value types of a ry `Dict<K, V>`.
pub fn dict_entries<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<(Ty<'db>, Ty<'db>)> {
    match ty.kind(db) {
        TyKind::Named(name, arguments)
            if arguments.len() == 2
                && name.text(db) == "Dict"
                && crate::stubs::is_ry_type(db, "Dict") =>
        {
            Some((arguments[0], arguments[1]))
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

/// Appends to `out` the spelling of `ty` that a signature gives it, for a value typed code can
/// hold in a dictionary: `I64`, `F64`, `Str`, `Bool`, a `List`, a `Record{..}` or `Tuple{..}` of
/// such values, or a tag, spelled `:` alone because a box's tag field holds any tag. An R double
/// or string spells as the `F64` or `Str` it converts to. `view` gives what a type stands for at
/// its top (a variable's binding, a nominal's representation), so the checker and the native
/// check spell alike. `None` for any other type, for a nominal that holds itself, whose spelling
/// never ends, and for a field name the R string literal the spelling goes into could not hold
/// as it is.
pub fn stored_spelling<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    view: &dyn Fn(Ty<'db>) -> Ty<'db>,
    out: &mut String,
    depth: usize,
) -> Option<()> {
    const DEPTH_LIMIT: usize = 16;
    if depth > DEPTH_LIMIT {
        return None;
    }
    let ty = view(ty);
    if is_tag_type(db, ty) {
        out.push(':');
        return Some(());
    }
    if let Some(item) = array_element(db, ty) {
        out.push_str("List<");
        stored_spelling(db, item, view, out, depth + 1)?;
        out.push('>');
        return Some(());
    }
    match ty.kind(db) {
        TyKind::Native(scalar) => out.push_str(match scalar {
            NativeScalar::I64 => "I64",
            NativeScalar::F64 => "F64",
            NativeScalar::Str => "Str",
            NativeScalar::Bool => "Bool",
        }),
        TyKind::Scalar(Atomic::Double) => out.push_str("F64"),
        TyKind::Scalar(Atomic::Character) => out.push_str("Str"),
        TyKind::NativeRecord(fields, None) if !fields.is_empty() => {
            out.push_str("Record{");
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let name = field.name.text(db);
                if !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
                {
                    return None;
                }
                out.push_str(name);
                out.push_str(": ");
                stored_spelling(db, field.ty, view, out, depth + 1)?;
            }
            out.push('}');
        }
        TyKind::NativeTuple(items) if !items.is_empty() => {
            out.push_str("Tuple{");
            for (index, &item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                stored_spelling(db, item, view, out, depth + 1)?;
            }
            out.push('}');
        }
        _ => return None,
    }
    Some(())
}

/// Whether every value of `ty` is one of ry's own values (a record, tuple, tag, or a standard
/// library collection, or one of its scalars), the values a `List` holds.
pub fn is_ry_value_type(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::NativeRecord(_, _)
        | TyKind::NativeTuple(_)
        | TyKind::Tag(..)
        | TyKind::OtherTags
        | TyKind::Native(_) => true,
        TyKind::Named(name, _) => {
            matches!(name.text(db), "List" | "Dict" | "Set")
                && crate::stubs::is_ry_type(db, name.text(db))
        }
        TyKind::Union(members) => {
            !members.is_empty() && members.iter().all(|&member| is_ry_value_type(db, member))
        }
        _ => false,
    }
}

/// A ry value, or a union with one among its members: a value that compares only with its
/// own type (`I64 | NULL` beside a `character` is an error, as `m == "s"` is).
pub fn holds_ry_value(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::Union(members) => members.iter().any(|&member| is_ry_value_type(db, member)),
        _ => is_ry_value_type(db, ty),
    }
}

/// Sorts by name in byte order, the one order a record's fields are stored and printed in.
pub fn native_record<'db>(
    db: &'db dyn Db,
    fields: impl IntoIterator<Item = NativeField<'db>>,
) -> Ty<'db> {
    native_record_with_row(db, fields, None)
}

/// A record whose `row` stands for the fields it does not list. A row that resolved to a record
/// merges into it, so a row is only ever a type variable or parameter: a listed field wins over
/// a row's field of the same name, and the row's own row carries on. A row that resolved to
/// anything else, such as `Unknown`, leaves the fields unknown, so the record is `Unknown`.
pub fn native_record_with_row<'db>(
    db: &'db dyn Db,
    fields: impl IntoIterator<Item = NativeField<'db>>,
    row: Option<Ty<'db>>,
) -> Ty<'db> {
    let mut fields: SmallVec<[NativeField<'db>; 8]> = fields.into_iter().collect();
    let mut row = row;
    while let Some(tail) = row {
        match tail.kind(db) {
            TyKind::Var(_) | TyKind::Rigid(_) => break,
            TyKind::NativeRecord(more, rest) => {
                let listed = fields.len();
                fields.extend(
                    more.iter()
                        .filter(|field| !fields[..listed].iter().any(|own| own.name == field.name))
                        .copied()
                        .collect::<SmallVec<[NativeField<'db>; 8]>>(),
                );
                row = *rest;
            }
            _ => return Ty::new(db, TyKind::Unknown),
        }
    }
    fields.sort_by(|left, right| left.name.text(db).cmp(right.name.text(db)));
    Ty::new(db, TyKind::NativeRecord(&fields, row))
}

/// A frame whose `row` stands for the columns it does not list. A row that resolved to a table
/// merges into it, so a row is only ever a type variable or parameter: the row's columns follow
/// the listed ones, a listed column wins over a row's column of the same name, and the row's own
/// row carries on. A row that resolved to anything else, such as `Unknown`, leaves the columns
/// unknown.
pub fn frame_with_row<'db>(
    db: &'db dyn Db,
    kind: FrameKind,
    columns: &[RecordField<'db>],
    row: Option<Ty<'db>>,
) -> Ty<'db> {
    let mut merged: SmallVec<[RecordField<'db>; 8]> = SmallVec::new();
    let mut row = row;
    while let Some(tail) = row {
        match tail.kind(db) {
            TyKind::Var(_) | TyKind::Rigid(_) => break,
            TyKind::Frame(_, Some(more), rest) => {
                if merged.is_empty() {
                    merged.extend(columns.iter().copied());
                }
                let listed = merged.len();
                let extra: SmallVec<[RecordField<'db>; 8]> = more
                    .iter()
                    .filter(|column| !merged[..listed].iter().any(|own| own.name == column.name))
                    .copied()
                    .collect();
                merged.extend(extra);
                row = *rest;
            }
            _ => return Ty::new(db, TyKind::Frame(kind, None, None)),
        }
    }
    let columns = if merged.is_empty() { columns } else { &merged };
    Ty::new(db, TyKind::Frame(kind, Some(columns), row))
}

/// The payload's shape: its item count, or its field names. Two tags with one name and one
/// shape are the same member of a union, and two with one name and different shapes conflict.
#[derive(Clone, Copy)]
enum PayloadShape<'db> {
    Bare,
    Positional(usize),
    Named(&'db [NativeField<'db>]),
    /// A payload that is not a native tuple or record, which only a broken annotation makes.
    Other(Ty<'db>),
}

impl PartialEq for PayloadShape<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PayloadShape::Bare, PayloadShape::Bare) => true,
            (PayloadShape::Positional(left), PayloadShape::Positional(right)) => left == right,
            (PayloadShape::Named(left), PayloadShape::Named(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right.iter())
                        .all(|(left, right)| left.name == right.name)
            }
            (PayloadShape::Other(left), PayloadShape::Other(right)) => left == right,
            _ => false,
        }
    }
}

fn payload_shape<'db>(db: &'db dyn Db, payload: Option<Ty<'db>>) -> PayloadShape<'db> {
    let Some(payload) = payload else {
        return PayloadShape::Bare;
    };
    match payload.kind(db) {
        TyKind::NativeTuple(items) => PayloadShape::Positional(items.len()),
        TyKind::NativeRecord(fields, _) => PayloadShape::Named(fields),
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
        // A frame stores a `List` column of `F64`, `Bool` or `Str` as the R vector that shares
        // its representation, and one of `I64` as an integer column, as `[1L, 2L]` reads in R.
        _ => match array_element(db, ty) {
            Some(element) => match element.kind(db) {
                TyKind::Native(native) => {
                    let atomic = native.shared_atomic().unwrap_or(Atomic::Integer);
                    Ty::new(db, TyKind::Vector(scalar(db, atomic)))
                }
                // A `List` of tags without a payload is a column of those tags, which a table
                // stores as the character column of their names.
                _ if is_bare_tags(db, element) => element,
                _ => ty,
            },
            None => ty,
        },
    }
}

/// The type R code reads from a column of type `ty`: a column of tags holds their names, so R
/// reads a character vector.
pub fn r_column_type<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    if is_bare_tags(db, ty) {
        Ty::new(db, TyKind::Vector(scalar(db, Atomic::Character)))
    } else {
        ty
    }
}

/// Whether `ty` is a closed union of tags without a payload, such as `:Bond | :Cash`: the type of
/// a table column that holds tags.
pub fn is_bare_tags<'db>(db: &'db dyn Db, ty: Ty<'db>) -> bool {
    match ty.kind(db) {
        TyKind::Tag(_, None) => true,
        TyKind::Union(members) => members
            .iter()
            .all(|&member| matches!(member.kind(db), TyKind::Tag(_, None))),
        _ => false,
    }
}

/// The children a type is built from, kept on the stack for the usual handful.
pub type TypeList<'db> = SmallVec<[Ty<'db>; 8]>;

pub fn union_of<'db>(db: &'db dyn Db, members: impl IntoIterator<Item = Ty<'db>>) -> Ty<'db> {
    let mut flat = TypeList::new();
    let mut saw_null = false;
    let mut saw_other_tags = false;
    let mut stack: TypeList<'db> = members.into_iter().collect();
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
        flat.push(Ty::from_bits(OTHER_TAGS));
    }
    match flat.len() {
        0 => null(db),
        1 => flat[0],
        _ => Ty::new(db, TyKind::Union(&flat)),
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
        (TyKind::NativeTuple(left), TyKind::NativeTuple(right)) => {
            let items: TypeList<'db> = left
                .iter()
                .zip(right.iter())
                .map(|(&left, &right)| union_of(db, [left, right]))
                .collect();
            Ty::new(db, TyKind::NativeTuple(&items))
        }
        (TyKind::NativeRecord(left, _), TyKind::NativeRecord(right, _)) => {
            let fields: SmallVec<[NativeField<'db>; 8]> = left
                .iter()
                .zip(right.iter())
                .map(|(left, right)| NativeField {
                    name: left.name,
                    ty: union_of(db, [left.ty, right.ty]),
                })
                .collect();
            Ty::new(db, TyKind::NativeRecord(&fields, None))
        }
        _ => return first,
    };
    Ty::new(db, TyKind::Tag(*name, Some(payload)))
}

/// Node count of `ty` as a tree (each path counted separately), saturating at
/// [`TYPE_SIZE_CEILING`] and stored when the type is built. Types share subtrees, so a
/// self-referential record's tree grows exponentially while its distinct nodes stay few.
pub fn type_size<'db>(_db: &'db dyn Db, ty: Ty<'db>) -> u64 {
    u64::from(stored_size(ty))
}

/// Far above any type real code produces, far below what a self-referential value reaches.
pub const TYPE_SIZE_CEILING: u64 = 100_000;

fn stored_size(ty: Ty<'_>) -> u32 {
    ty.facts().size
}

/// Rebuild `ty` with `map` applied to each direct child type. Every type rewrite
/// goes through here so no walk can skip a member such as a parameter default.
/// Leaves, variables, and rigid binders come back unchanged.
pub fn map_child_types<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    map: &mut impl FnMut(Ty<'db>) -> Ty<'db>,
) -> Ty<'db> {
    let kind = ty.kind(db);
    if let TyKind::Union(members) = kind {
        // Members can collapse once mapped, so re-normalize.
        return union_of(db, members.iter().map(|&member| map(member)));
    }
    // Most maps leave most types alone, so the children are mapped first and the type is
    // rebuilt only once one of them changed. `mapped` stays empty until the first change,
    // when it takes the unchanged prefix.
    let mut mapped = TypeList::new();
    let mut unchanged = 0;
    for_each_child(kind, &mut |child| {
        let result = map(child);
        if mapped.is_empty() {
            if result == child {
                unchanged += 1;
                return;
            }
            let mut prefix = 0;
            for_each_child(kind, &mut |original| {
                if prefix < unchanged {
                    mapped.push(original);
                    prefix += 1;
                }
            });
        }
        mapped.push(result);
    });
    if mapped.is_empty() {
        return ty;
    }
    let mut children = mapped.into_iter();
    let rebuilt = rebuilt_type(db, kind, &mut |child| children.next().unwrap_or(child));
    // `for_each_child` and `rebuilt_type` must visit the same children in the same order.
    debug_assert!(
        children.next().is_none(),
        "rebuilt_type visited fewer children than for_each_child"
    );
    rebuilt
}

/// The type of `kind` with `map` applied to each child, in `for_each_child`'s order.
fn rebuilt_type<'db>(
    db: &'db dyn Db,
    kind: &TyKind<'db>,
    map: &mut impl FnMut(Ty<'db>) -> Ty<'db>,
) -> Ty<'db> {
    let mapped_fields = |fields: &[RecordField<'db>], map: &mut dyn FnMut(Ty<'db>) -> Ty<'db>| {
        fields
            .iter()
            .map(|field| RecordField {
                ty: map(field.ty),
                ..*field
            })
            .collect::<SmallVec<[RecordField<'db>; 8]>>()
    };
    match kind {
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::Native(_)
        | TyKind::OtherTags
        | TyKind::Var(_)
        | TyKind::Rigid(_)
        | TyKind::Frame(_, None, _)
        | TyKind::Union(_) => Ty::new(db, *kind),
        TyKind::Vector(inner) => Ty::new(db, TyKind::Vector(map(*inner))),
        TyKind::NamedVector(inner) => Ty::new(db, TyKind::NamedVector(map(*inner))),
        TyKind::List(inner) => Ty::new(db, TyKind::List(map(*inner))),
        TyKind::NamedList(inner) => Ty::new(db, TyKind::NamedList(map(*inner))),
        TyKind::Tuple(items) => {
            let items: TypeList<'db> = items.iter().map(|&item| map(item)).collect();
            Ty::new(db, TyKind::Tuple(&items))
        }
        TyKind::Record(fields) => Ty::new(db, TyKind::Record(&mapped_fields(fields, map))),
        TyKind::OpenRecord(fields) => Ty::new(db, TyKind::OpenRecord(&mapped_fields(fields, map))),
        TyKind::Environment(fields) => {
            Ty::new(db, TyKind::Environment(&mapped_fields(fields, map)))
        }
        TyKind::NativeTuple(items) => {
            let items: TypeList<'db> = items.iter().map(|&item| map(item)).collect();
            Ty::new(db, TyKind::NativeTuple(&items))
        }
        TyKind::NativeRecord(fields, row) => {
            let fields: SmallVec<[NativeField<'db>; 8]> = fields
                .iter()
                .map(|field| NativeField {
                    name: field.name,
                    ty: map(field.ty),
                })
                .collect();
            native_record_with_row(db, fields, row.map(&mut *map))
        }
        TyKind::Tag(name, payload) => Ty::new(db, TyKind::Tag(*name, payload.map(&mut *map))),
        TyKind::Family(result, operand) => family_result(db, *result, map(*operand)),
        TyKind::Frame(frame, Some(columns), row) => {
            let columns = mapped_fields(columns, map);
            frame_with_row(db, *frame, &columns, row.map(&mut *map))
        }
        TyKind::Function(function) => {
            let positional: TypeList<'db> = function.positional.iter().map(|&ty| map(ty)).collect();
            let named: SmallVec<[Parameter<'db>; 8]> = function
                .named
                .iter()
                .map(|parameter| Parameter {
                    ty: map(parameter.ty),
                    default: parameter.default.map(&mut *map),
                    ..*parameter
                })
                .collect();
            let variadic = function.variadic.map(|rest| RestParameter {
                element: map(rest.element),
                ..rest
            });
            let ret = map(function.ret);
            let flag_return = function.flag_return.map(|flag| FlagReturn {
                when_true: map(flag.when_true),
                when_false: map(flag.when_false),
                ..flag
            });
            let compared: SmallVec<[ComparedPair<'db>; 2]> = function
                .compared
                .iter()
                .map(|pair| ComparedPair {
                    left: map(pair.left),
                    right: map(pair.right),
                })
                .collect();
            Ty::new(
                db,
                TyKind::Function(&FunctionType {
                    positional: &positional,
                    named: &named,
                    variadic,
                    ret,
                    flag_return,
                    compared: &compared,
                    ..**function
                }),
            )
        }
        TyKind::Named(name, arguments) => {
            let arguments: TypeList<'db> =
                arguments.iter().map(|&argument| map(argument)).collect();
            Ty::new(db, TyKind::Named(*name, &arguments))
        }
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
        | TyKind::Native(_)
        | TyKind::OtherTags
        | TyKind::Var(_)
        | TyKind::Rigid(_) => {}
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner)
        | TyKind::Family(_, inner) => visit(*inner),
        TyKind::Tuple(items) | TyKind::NativeTuple(items) | TyKind::Union(items) => {
            items.iter().copied().for_each(visit)
        }
        TyKind::NativeRecord(fields, row) => {
            fields.iter().for_each(|field| visit(field.ty));
            row.iter().copied().for_each(visit);
        }
        TyKind::Tag(_, payload) => payload.iter().copied().for_each(visit),
        TyKind::Record(fields) | TyKind::OpenRecord(fields) | TyKind::Environment(fields) => {
            fields.iter().for_each(|field| visit(field.ty))
        }
        TyKind::Frame(_, Some(fields), row) => {
            fields.iter().for_each(|field| visit(field.ty));
            row.iter().copied().for_each(visit);
        }
        TyKind::Frame(_, None, _) => {}
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
            if let Some(flag) = function.flag_return {
                visit(flag.when_true);
                visit(flag.when_false);
            }
            for pair in function.compared {
                visit(pair.left);
                visit(pair.right);
            }
        }
        TyKind::Named(_, arguments) => arguments.iter().copied().for_each(visit),
    }
}

/// Variables index one table, so a type outliving it (an exported scheme, a
/// value cached across a rollback) must contain none.
pub fn contains_inference_var<'db>(_db: &'db dyn Db, ty: Ty<'db>) -> bool {
    ty.bits.get() & HAS_VAR != 0
}

/// Whether `ty` names a type parameter of an enclosing signature.
pub fn contains_rigid(ty: Ty<'_>) -> bool {
    ty.bits.get() & HAS_RIGID != 0
}

/// Unmapped rigid variables are left intact. A name bound twice takes its last binding.
pub fn substitute_rigid<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &[(Name<'db>, Ty<'db>)],
) -> Ty<'db> {
    // Types share subtrees, so an unmemoized walk re-walks a shared subtree once per path,
    // exponential in depth. The tree size bounds that re-walk, and below this size it costs
    // less than building a memo table.
    if stored_size(ty) <= UNMEMOIZED_SUBSTITUTION_SIZE {
        return substitute_rigid_memo(db, ty, substitution, None);
    }
    substitute_rigid_memo(db, ty, substitution, Some(&mut FxHashMap::default()))
}

const UNMEMOIZED_SUBSTITUTION_SIZE: u32 = 64;

fn substitute_rigid_memo<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    substitution: &[(Name<'db>, Ty<'db>)],
    mut memo: Option<&mut FxHashMap<Ty<'db>, Ty<'db>>>,
) -> Ty<'db> {
    if ty.bits.get() & HAS_RIGID == 0 {
        return ty;
    }
    if let Some(&done) = memo.as_deref().and_then(|memo| memo.get(&ty)) {
        return done;
    }
    let substituted = match ty.kind(db) {
        TyKind::Rigid(name) => substitution
            .iter()
            .rev()
            .find(|(bound, _)| bound == name)
            .map_or(ty, |(_, replacement)| *replacement),
        _ => map_child_types(db, ty, &mut |child| {
            substitute_rigid_memo(db, child, substitution, memo.as_deref_mut())
        }),
    };
    if let Some(memo) = memo {
        memo.insert(ty, substituted);
    }
    substituted
}

/// For values that must survive a table rollback, which reclaims and reuses
/// variable ids.
pub fn erase_vars<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
    if ty.bits.get() & HAS_VAR == 0 {
        return ty;
    }
    if ty.is_var() {
        return unknown(db);
    }
    map_child_types(db, ty, &mut |child| erase_vars(db, child))
}

/// The kind of every type, indexed by the low bits of its id.
static KINDS: Column<TyKind<'static>, TYPE_CHUNKS> = Column::starting_with(&CONSTANT_KINDS);
static CONSTANT_KINDS: FirstChunk<TyKind<'static>> = FirstChunk::new({
    let mut kinds = [TyKind::Any; CHUNK_LENGTH];
    let mut index = 0;
    while index < CHUNK_LENGTH {
        kinds[index] = if index < LEAVES {
            LEAF_KINDS[index]
        } else {
            TyKind::Var(InferenceVar((index - LEAVES) as u32))
        };
        index += 1;
    }
    kinds
});

/// What the store derives from a type's structure when it is built, while its children's
/// facts are at hand. A column of its own, parallel to [`KINDS`], because a check reads a
/// type's kind far more often than these.
static FACTS: Column<TypeFacts, TYPE_CHUNKS> = Column::starting_with(&CONSTANT_FACTS);
static CONSTANT_FACTS: FirstChunk<TypeFacts> = FirstChunk::new(
    [TypeFacts {
        size: 1,
        applied_names: &[],
    }; CHUNK_LENGTH],
);

/// The store holds at most 2^24 distinct types. The id has room for more, but each chunk
/// address costs the binary eight bytes, and a process that built this many types would
/// have run out of memory for them first.
const TYPE_CHUNKS: usize = (1 << 24) / CHUNK_LENGTH;

#[derive(Clone, Copy)]
struct TypeFacts {
    /// Nodes in the type tree, capped at [`TYPE_SIZE_CEILING`].
    size: u32,
    applied_names: &'static [Name<'static>],
}

/// Kind to id, and behind its lock the arena that holds every stored type's children. The
/// arena is never reset, which is what makes a slice into it valid for the life of the
/// process. A type is built once and found ever after, so the lock is taken a few thousand
/// times in a whole project check.
static TYPES: IdTable<Option<Bump>> = IdTable::new(None);

fn intern_type<'db>(kind: TyKind<'_>) -> Ty<'db> {
    let hash = FxBuildHasher.hash_one(kind);
    let holds = |bits: NonZeroU32| *Ty::<'static>::from_bits(bits.get()).kind(&()) == kind;
    match TYPES.find(hash, holds) {
        Some(bits) => Ty::from_bits(bits.get()),
        None => insert_type(kind, hash),
    }
}

#[cold]
fn insert_type<'db>(kind: TyKind<'_>, hash: u64) -> Ty<'db> {
    let mut writer = TYPES.write();
    let holds = |bits: NonZeroU32| *Ty::<'static>::from_bits(bits.get()).kind(&()) == kind;
    if let Some(bits) = writer.find(hash, holds) {
        return Ty::from_bits(bits.get());
    }
    let index = writer.count() + CHUNK_LENGTH;
    assert!(
        index < Column::<TyKind<'static>, TYPE_CHUNKS>::CAPACITY,
        "the type store is full: it holds at most 2^24 distinct types"
    );
    let arena: &Bump = writer.state().get_or_insert_with(Bump::new);

    let mut size: u32 = 1;
    let mut contents = match kind {
        TyKind::Var(_) => HAS_VAR | IS_VAR,
        TyKind::Rigid(_) => HAS_RIGID,
        TyKind::Named(..) => HAS_NAMED,
        _ => 0,
    };
    let mut applied: SmallVec<[Name<'static>; 8]> = SmallVec::new();
    if let TyKind::Named(name, _) = kind {
        applied.push(Name {
            symbol: name.symbol,
            db: PhantomData,
        });
    }
    // A type that applies the same names as one of its children shares that child's list.
    let mut widest: &'static [Name<'static>] = &[];
    for_each_child(&kind, &mut |child| {
        size = size
            .saturating_add(stored_size(child))
            .min(TYPE_SIZE_CEILING as u32);
        contents |= child.bits.get() & CONTENT_BITS;
        let names = Ty::<'static>::from_bits(child.bits.get()).applied_names();
        for &name in names {
            if !applied.contains(&name) {
                applied.push(name);
            }
        }
        if names.len() > widest.len() {
            widest = names;
        }
    });
    let applied_names = if applied.len() == widest.len() {
        widest
    } else {
        let stored: &[Name<'static>] = arena.alloc_slice_copy(&applied);
        // SAFETY: the arena lives in a static and is never reset, so the slice is immortal.
        unsafe { mem::transmute::<&[Name<'static>], &'static [Name<'static>]>(stored) }
    };
    let stored = stored_kind(arena, kind);
    // SAFETY: the kind's slices now point into the arena, which is immortal as above, and
    // the lifetimes of `Ty` and `Name` are phantom.
    let stored = unsafe { mem::transmute::<TyKind<'_>, TyKind<'static>>(stored) };

    // SAFETY: `writer` is the lock that serializes the store's writers, `index` is the next
    // one, and no reader learns it before the table publishes it below.
    unsafe {
        KINDS.write(index, stored);
        FACTS.write(
            index,
            TypeFacts {
                size,
                applied_names,
            },
        );
    }
    let Some(bits) = NonZeroU32::new(index as u32 | contents) else {
        unreachable!("a stored type's index starts past the leaves")
    };
    writer.insert(hash, bits, |held| {
        FxBuildHasher.hash_one(*Ty::<'static>::from_bits(held.get()).kind(&()))
    });
    Ty::from_bits(bits.get())
}

/// `kind` with every list it borrows copied into `arena`.
fn stored_kind<'a>(arena: &'a Bump, kind: TyKind<'a>) -> TyKind<'a> {
    match kind {
        TyKind::Tuple(items) => TyKind::Tuple(arena.alloc_slice_copy(items)),
        TyKind::Record(fields) => TyKind::Record(arena.alloc_slice_copy(fields)),
        TyKind::OpenRecord(fields) => TyKind::OpenRecord(arena.alloc_slice_copy(fields)),
        TyKind::Environment(fields) => TyKind::Environment(arena.alloc_slice_copy(fields)),
        TyKind::NativeTuple(items) => TyKind::NativeTuple(arena.alloc_slice_copy(items)),
        TyKind::NativeRecord(fields, row) => {
            TyKind::NativeRecord(arena.alloc_slice_copy(fields), row)
        }
        TyKind::Frame(frame, Some(columns), row) => {
            TyKind::Frame(frame, Some(arena.alloc_slice_copy(columns)), row)
        }
        TyKind::Function(function) => TyKind::Function(arena.alloc(FunctionType {
            positional: arena.alloc_slice_copy(function.positional),
            named: arena.alloc_slice_copy(function.named),
            quoted: arena.alloc_slice_copy(function.quoted),
            default_reads: arena.alloc_slice_copy(function.default_reads),
            missing_tested: arena.alloc_slice_copy(function.missing_tested),
            condition_only: arena.alloc_slice_copy(function.condition_only),
            elementwise: arena.alloc_slice_copy(function.elementwise),
            compared: arena.alloc_slice_copy(function.compared),
            ..*function
        })),
        TyKind::Union(members) => TyKind::Union(arena.alloc_slice_copy(members)),
        TyKind::Named(name, arguments) => TyKind::Named(name, arena.alloc_slice_copy(arguments)),
        TyKind::Any
        | TyKind::Unknown
        | TyKind::Null
        | TyKind::Scalar(_)
        | TyKind::Native(_)
        | TyKind::Vector(_)
        | TyKind::NamedVector(_)
        | TyKind::List(_)
        | TyKind::NamedList(_)
        | TyKind::Tag(..)
        | TyKind::OtherTags
        | TyKind::Frame(_, None, _)
        | TyKind::Family(..)
        | TyKind::Var(_)
        | TyKind::Rigid(_) => kind,
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::RootDatabase, std::thread};

    #[test]
    fn size_counts_the_tree_not_the_shared_graph_and_saturates() {
        let db = RootDatabase::default();
        let leaf = scalar(&db, Atomic::Integer);
        let pair = Ty::new(&db, TyKind::Tuple(&[leaf, leaf]));
        let nested = Ty::new(&db, TyKind::Tuple(&[pair, pair]));

        assert_eq!(type_size(&db, leaf), 1);
        assert_eq!(type_size(&db, pair), 3, "one node plus two leaves");
        assert_eq!(
            type_size(&db, nested),
            7,
            "the shared pair is counted once per path, not once overall"
        );

        let mut deep = leaf;
        for _ in 0..64 {
            deep = Ty::new(&db, TyKind::Tuple(&[deep, deep]));
        }
        assert_eq!(type_size(&db, deep), TYPE_SIZE_CEILING);
    }

    #[test]
    fn a_parameter_default_is_part_of_the_type() {
        let db = RootDatabase::default();
        let var = Ty::new(&db, TyKind::Var(InferenceVar(7)));
        let integer = scalar(&db, Atomic::Integer);
        let named = [Parameter {
            name: Name::new(&db, "x"),
            ty: integer,
            optional: true,
            default: Some(var),
        }];
        let signature = Ty::new(
            &db,
            TyKind::Function(&FunctionType::declared(&[], &named, None, integer)),
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
    fn a_type_knows_what_occurs_in_it() {
        let db = RootDatabase::default();
        let integer = scalar(&db, Atomic::Integer);
        let var = Ty::new(&db, TyKind::Var(InferenceVar(3)));
        let option = Name::new(&db, "Option");
        let result = Name::new(&db, "Result");
        let applied = Ty::new(&db, TyKind::Named(option, &[var]));
        let nested = Ty::new(
            &db,
            TyKind::Named(result, &[applied, Ty::new(&db, TyKind::Vector(applied))]),
        );
        let list = Ty::new(&db, TyKind::List(nested));

        assert!(!integer.may_resolve());
        assert!(integer.applied_names().is_empty());
        assert!(var.may_resolve() && var.applied_names().is_empty());
        assert_eq!(applied.applied_names(), &[option]);
        assert_eq!(nested.applied_names(), &[result, option]);
        assert_eq!(
            list.applied_names(),
            nested.applied_names(),
            "a type that applies no name of its own lists its children's"
        );
        assert!(contains_inference_var(&db, list));
        assert!(!contains_unknown(&db, list));
        assert!(contains_unknown(
            &db,
            Ty::new(&db, TyKind::Tuple(&[integer, unknown(&db)]))
        ));
        let erased = erase_vars(&db, list);
        assert!(!contains_inference_var(&db, erased) && contains_unknown(&db, erased));
        assert_eq!(erased.applied_names(), &[result, option]);
    }

    #[test]
    fn substitution_replaces_the_last_binding_of_a_name() {
        let db = RootDatabase::default();
        let integer = scalar(&db, Atomic::Integer);
        let double = scalar(&db, Atomic::Double);
        let name = Name::new(&db, "T");
        let rigid = Ty::new(&db, TyKind::Rigid(name));
        let pair = Ty::new(&db, TyKind::Tuple(&[rigid, integer]));
        assert_eq!(
            substitute_rigid(&db, pair, &[(name, integer), (name, double)]),
            Ty::new(&db, TyKind::Tuple(&[double, integer]))
        );
        assert_eq!(substitute_rigid(&db, pair, &[]), pair);
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
        assert_eq!(*members, &[int, chr, null(&db)]);

        assert_eq!(union_of(&db, [int, any(&db)]), any(&db));
        assert_eq!(union_of(&db, [unknown(&db), int]), unknown(&db));

        assert_eq!(union_of(&db, [int, int]), int);
        assert_eq!(union_of(&db, []), null(&db));
        let optional = union_of(&db, [null(&db), chr]);
        let TyKind::Union(members) = optional.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(*members, &[chr, null(&db)]);

        let other = Ty::new(&db, TyKind::OtherTags);
        let none = Ty::new(&db, TyKind::Tag(Name::new(&db, "None"), None));
        let open = union_of(&db, [other, null(&db), none]);
        let TyKind::Union(members) = open.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(*members, &[none, null(&db), other], "other tags go last");
    }

    /// Threads that build the same types at the same time, each in its own order, must agree
    /// on every id and read back the kind they stored. The count is far past the table's
    /// first size, so lookups run while the table is being replaced.
    #[test]
    fn threads_interning_the_same_types_agree() {
        const THREADS: usize = 8;
        const TYPES: u32 = 20_000;
        // Variable ids no check reaches, so every one of these types is new to the store.
        const FIRST_VARIABLE: u32 = 3_000_000_000;

        fn build(step: u32) -> (Ty<'static>, Ty<'static>, Ty<'static>) {
            let var = Ty::new(&(), TyKind::Var(InferenceVar(FIRST_VARIABLE + step)));
            let integer = Ty::from_bits(FIRST_SCALAR + Atomic::Integer as u32);
            let pair = Ty::new(&(), TyKind::Tuple(&[var, integer]));
            let function = Ty::new(
                &(),
                TyKind::Function(&FunctionType::declared(&[pair, var], &[], None, pair)),
            );
            (var, pair, function)
        }

        let per_thread: Vec<Vec<(Ty<'static>, Ty<'static>, Ty<'static>)>> =
            thread::scope(|scope| {
                let workers: Vec<_> = (0..THREADS)
                    .map(|worker| {
                        scope.spawn(move || {
                            let mut built = vec![None; TYPES as usize];
                            // A stride coprime to the count visits every step once, in an
                            // order that differs per thread.
                            let stride = [1, 3, 7, 9, 11, 13, 17, 19][worker];
                            let mut step = worker as u32 * 97 % TYPES;
                            for _ in 0..TYPES {
                                built[step as usize] = Some(build(step));
                                step = (step + stride) % TYPES;
                            }
                            built.into_iter().flatten().collect::<Vec<_>>()
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("an interning thread panicked"))
                    .collect()
            });

        let first = &per_thread[0];
        assert_eq!(first.len(), TYPES as usize);
        for other in &per_thread[1..] {
            assert_eq!(first, other, "every thread got the same ids");
        }
        for (step, &(var, pair, function)) in first.iter().enumerate() {
            assert_eq!(
                *var.kind(&()),
                TyKind::Var(InferenceVar(FIRST_VARIABLE + step as u32))
            );
            let TyKind::Tuple(items) = pair.kind(&()) else {
                panic!("the pair reads back as a tuple")
            };
            assert_eq!(items[0], var);
            let TyKind::Function(signature) = function.kind(&()) else {
                panic!("the function reads back as a function")
            };
            assert_eq!(signature.positional, &[pair, var]);
            assert_eq!(signature.ret, pair);
            assert!(contains_inference_var_bits(function));
            assert_eq!(stored_size(function), 8);
        }
    }

    fn contains_inference_var_bits(ty: Ty<'_>) -> bool {
        ty.bits.get() & HAS_VAR != 0
    }

    #[test]
    fn constraint_lattice_joins() {
        use Constraint::*;
        assert_eq!(Unconstrained.join(Numeric), Some(Numeric));
        assert_eq!(Numeric.join(AtomicElement), Some(ScalarNumeric));
        assert_eq!(ScalarNumeric.join(Numeric), Some(ScalarNumeric));
        assert_eq!(AtomicElement.join(AtomicElement), Some(AtomicElement));
        assert_eq!(Key.join(AtomicElement), Some(Key));
        assert_eq!(Key.join(Numeric), Some(ScalarNumeric));
        assert_eq!(Number.join(Int), Some(Int));
        assert_eq!(Int.join(Frac), None);
        assert_eq!(Number.join(Numeric), None);
        assert_eq!(WholeLiteral { integer: false }.join(Frac), Some(Frac));
        assert_eq!(FracLiteral.join(Int), None);
        assert_eq!(FracLiteral.join(Numeric), Some(Numeric));
    }
}
