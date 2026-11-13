use {
    crate::{Db, hir::Symbol},
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
        ptr,
        sync::{
            Mutex,
            atomic::{AtomicPtr, AtomicU64, Ordering},
        },
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
/// The id's low 28 bits are the store index. The top four bits say what occurs anywhere in
/// the type's tree, so the walks that only rewrite one kind return a type without it by a
/// bit test, without reading the store. The lifetime is phantom: no type borrows from a
/// database, which is what lets the store outlive them all.
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
const CONTENT_BITS: u32 = HAS_VAR | HAS_NAMED | HAS_RIGID | HAS_UNKNOWN;
const INDEX_BITS: u32 = !CONTENT_BITS;

/// The kinds without a payload that needs the store. Their ids are constants, so building
/// one costs nothing and reading one is an index into this table. Index 0 is never an id,
/// which keeps an `Option<Ty>` the size of a `Ty`.
static LEAF_KINDS: [TyKind<'static>; LEAVES] = [
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
            _ => return intern_type(kind),
        };
        Ty::from_bits(bits)
    }

    pub fn kind<D: ?Sized>(self, _db: &D) -> &'db TyKind<'db> {
        let index = (self.bits.get() & INDEX_BITS) as usize;
        if index < LEAVES {
            return &LEAF_KINDS[index];
        }
        // SAFETY: an id past the leaves is only ever made by `insert_type`, after it pushed
        // the entry at this index, and the column never removes an entry.
        unsafe { KINDS.get_unchecked(index - LEAVES) }
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
        let index = (self.bits.get() & INDEX_BITS) as usize;
        debug_assert!(index >= LEAVES, "a leaf has no stored facts");
        // SAFETY: as in `kind`; `insert_type` pushes both columns before it makes the id.
        unsafe { FACTS.get_unchecked(index - LEAVES) }
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
    /// `Tuple{A, B}`: ry's native tuple, which is not a list and never fits a list type.
    NativeTuple(&'db [Ty<'db>]),
    /// `Record{a: A, b: B}`: ry's native record. Field order is not part of the type, so the
    /// fields are kept sorted by name and every field is required.
    NativeRecord(&'db [NativeField<'db>]),
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
    Frame(FrameKind, Option<&'db [RecordField<'db>]>),
    Function(&'db FunctionType<'db>),
    /// Normalized; build only through `union_of`, never directly.
    Union(&'db [Ty<'db>]),
    Named(Name<'db>, &'db [Ty<'db>]),
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
    /// The body returns `NULL` only through `if (!predicate(parameter)) return(NULL)`, so a
    /// call where the caller already showed `predicate` holds of its argument returns the
    /// other members alone. Inferred only.
    pub null_unless: Option<NullUnless<'db>>,
    /// The body ends in `Reduce(f, x, init = init)` over its own formals, with `f` a formal
    /// or a closure that only forwards to one, so a call folds like a direct `Reduce` and its
    /// accumulator may widen past the start. Inferred only.
    pub reduces: Option<ReduceForward<'db>>,
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
            null_unless: None,
            reduces: None,
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

/// `ns_env <- function(package) { if (!is_loaded(package)) return(NULL); asNamespace(package) }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct NullUnless<'db> {
    pub predicate: Name<'db>,
    pub parameter: Name<'db>,
}

/// `formal` is required by a call that leaves out `default_of`, whose default reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct DefaultRead<'db> {
    pub formal: Name<'db>,
    pub default_of: Name<'db>,
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
    Numeric,
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
    /// Every ry type but a function, the values `==` compares structurally.
    Eq,
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
    pub const SPELLINGS: [(&'static str, Constraint); 7] = [
        ("numeric", Constraint::Numeric),
        ("atomic", Constraint::AtomicElement),
        ("key", Constraint::Key),
        ("scalar numeric", Constraint::ScalarNumeric),
        ("item", Constraint::ArrayItem),
        ("number", Constraint::Number),
        ("eq", Constraint::Eq),
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

    /// The constraint a type must meet to satisfy both, or `None` when no type can: an R number
    /// is never a ry number, and an integer type is never a fraction type.
    pub fn join(self, other: Constraint) -> Option<Constraint> {
        use Constraint::*;
        Some(match (self, other) {
            (Unconstrained, c) | (c, Unconstrained) => c,
            (a, b) if a == b => a,
            (Eq, c) | (c, Eq) => c,
            (WholeLiteral { integer: left }, WholeLiteral { integer: right }) => WholeLiteral {
                integer: left && right,
            },
            (WholeLiteral { .. }, FracLiteral) | (FracLiteral, WholeLiteral { .. }) => FracLiteral,
            (WholeLiteral { .. } | Number | ArrayItem | Key, Int)
            | (Int, WholeLiteral { .. } | Number | ArrayItem | Key) => Int,
            (FracLiteral | WholeLiteral { .. } | Number | ArrayItem | Key, Frac)
            | (Frac, FracLiteral | WholeLiteral { .. } | Number | ArrayItem | Key) => Frac,
            (WholeLiteral { .. } | FracLiteral | ArrayItem | Key, Number)
            | (Number, WholeLiteral { .. } | FracLiteral | ArrayItem | Key) => Number,
            (FracLiteral, Int) | (Int, FracLiteral) | (Int, Frac) | (Frac, Int) => return None,
            // Both members of a literal constraint are keys; any other R constraint makes it an
            // R value. A text or logical literal is never a number, and the two never meet.
            (StrLiteral, Key) | (Key, StrLiteral) => StrLiteral,
            (BoolLiteral, Key) | (Key, BoolLiteral) => BoolLiteral,
            (StrLiteral, AtomicElement) | (AtomicElement, StrLiteral) => AtomicElement,
            (BoolLiteral, c @ (AtomicElement | Numeric | ScalarNumeric))
            | (c @ (AtomicElement | Numeric | ScalarNumeric), BoolLiteral) => c,
            (StrLiteral | BoolLiteral, _) | (_, StrLiteral | BoolLiteral) => return None,
            // A literal used where R's number constraints apply is an R number.
            (WholeLiteral { .. } | FracLiteral, c) | (c, WholeLiteral { .. } | FracLiteral) => c,
            (Number | Int | Frac, _) | (_, Number | Int | Frac) => return None,
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
    pub binders: ThinVec<(Name<'db>, Constraint)>,
    pub body: Ty<'db>,
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

/// Whether every value of `ty` is one of ry's own values (a record, tuple, tag, or a standard
/// library collection, or one of its scalars), the values a `List` holds.
pub fn is_ry_value_type(db: &dyn Db, ty: Ty<'_>) -> bool {
    match ty.kind(db) {
        TyKind::NativeRecord(_)
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

/// Sorts by name in byte order, the one order a record's fields are stored and printed in.
pub fn native_record<'db>(
    db: &'db dyn Db,
    fields: impl IntoIterator<Item = NativeField<'db>>,
) -> Ty<'db> {
    let mut fields: SmallVec<[NativeField<'db>; 8]> = fields.into_iter().collect();
    fields.sort_by(|left, right| left.name.text(db).cmp(right.name.text(db)));
    Ty::new(db, TyKind::NativeRecord(&fields))
}

/// The payload's shape: its item count, or its field names. Two tags with one name and one
/// shape are the same member of a union, and two with one name and different shapes conflict.
#[derive(Clone, Copy)]
pub enum PayloadShape<'db> {
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

impl<'db> PayloadShape<'db> {
    /// The field names of a named payload, in the record's order.
    pub fn names(self) -> impl Iterator<Item = Name<'db>> {
        match self {
            PayloadShape::Named(fields) => fields,
            _ => &[],
        }
        .iter()
        .map(|field| field.name)
    }
}

pub fn payload_shape<'db>(db: &'db dyn Db, payload: Option<Ty<'db>>) -> PayloadShape<'db> {
    let Some(payload) = payload else {
        return PayloadShape::Bare;
    };
    match payload.kind(db) {
        TyKind::NativeTuple(items) => PayloadShape::Positional(items.len()),
        TyKind::NativeRecord(fields) => PayloadShape::Named(fields),
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
        // its representation.
        _ => match array_element(db, ty).map(|element| element.kind(db)) {
            Some(TyKind::Native(native)) => match native.shared_atomic() {
                Some(atomic) => Ty::new(db, TyKind::Vector(scalar(db, atomic))),
                None => ty,
            },
            _ => ty,
        },
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
        (TyKind::NativeRecord(left), TyKind::NativeRecord(right)) => {
            let fields: SmallVec<[NativeField<'db>; 8]> = left
                .iter()
                .zip(right.iter())
                .map(|(left, right)| NativeField {
                    name: left.name,
                    ty: union_of(db, [left.ty, right.ty]),
                })
                .collect();
            Ty::new(db, TyKind::NativeRecord(&fields))
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
    if (ty.bits.get() & INDEX_BITS) < LEAVES as u32 {
        return 1;
    }
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
        | TyKind::Frame(_, None)
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
        TyKind::NativeTuple(items) => {
            let items: TypeList<'db> = items.iter().map(|&item| map(item)).collect();
            Ty::new(db, TyKind::NativeTuple(&items))
        }
        TyKind::NativeRecord(fields) => {
            let fields: SmallVec<[NativeField<'db>; 8]> = fields
                .iter()
                .map(|field| NativeField {
                    name: field.name,
                    ty: map(field.ty),
                })
                .collect();
            Ty::new(db, TyKind::NativeRecord(&fields))
        }
        TyKind::Tag(name, payload) => Ty::new(db, TyKind::Tag(*name, payload.map(&mut *map))),
        TyKind::Frame(frame, Some(columns)) => Ty::new(
            db,
            TyKind::Frame(*frame, Some(&mapped_fields(columns, map))),
        ),
        TyKind::Function(function) => {
            let positional: TypeList<'db> =
                function.positional.iter().map(|&ty| map(ty)).collect();
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
            Ty::new(
                db,
                TyKind::Function(&FunctionType {
                    positional: &positional,
                    named: &named,
                    variadic,
                    ret,
                    flag_return,
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
            if let Some(flag) = function.flag_return {
                visit(flag.when_true);
                visit(flag.when_false);
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
    if matches!(ty.kind(db), TyKind::Var(_)) {
        return unknown(db);
    }
    map_child_types(db, ty, &mut |child| erase_vars(db, child))
}

/// The kind of every type past the leaves, indexed by the type's id less [`LEAVES`]. The
/// column appends without moving an entry, so a read takes no lock.
static KINDS: boxcar::Vec<TyKind<'static>> = boxcar::Vec::new();

/// What the store derives from a type's structure when it is built, while its children's
/// facts are at hand. A column of its own, parallel to [`KINDS`], because a check reads a
/// type's kind far more often than these.
static FACTS: boxcar::Vec<TypeFacts> = boxcar::Vec::new();

struct TypeFacts {
    /// Nodes in the type tree, capped at [`TYPE_SIZE_CEILING`].
    size: u32,
    applied_names: &'static [Name<'static>],
}

/// Kind to id. A lookup reads it without a lock, and the table in use is replaced, never
/// changed in place beyond filling an empty slot, when it grows.
static TYPE_TABLE: AtomicPtr<TypeTable> = AtomicPtr::new(ptr::null_mut());

/// The one writer of the store. A type is built once and found ever after, so the lock is
/// taken a few thousand times in a whole project check.
static TYPE_WRITER: Mutex<Option<TypeWriter>> = Mutex::new(None);

struct TypeWriter {
    /// Holds every stored type's children. Never reset, which is what makes a slice into it
    /// valid for the life of the process.
    arena: Bump,
    count: usize,
}

/// An open-addressing table. A slot is empty (0) or holds the upper half of a kind's hash
/// above the type's id, so a probe compares kinds only where the hashes agree. At most half
/// the slots are ever filled, so every probe ends at an empty one.
struct TypeTable {
    slots: Box<[AtomicU64]>,
}

impl TypeTable {
    fn find(&self, hash: u64, kind: &TyKind<'_>) -> Option<Ty<'static>> {
        let mask = self.slots.len() - 1;
        let mut position = hash as usize & mask;
        loop {
            let slot = self.slots[position].load(Ordering::Acquire);
            if slot == 0 {
                return None;
            }
            if slot >> 32 == hash >> 32 {
                let candidate = Ty::from_bits(slot as u32);
                if candidate.kind(&()) == kind {
                    return Some(candidate);
                }
            }
            position = (position + 1) & mask;
        }
    }

    fn place(&self, hash: u64, ty: Ty<'_>) {
        let mask = self.slots.len() - 1;
        let mut position = hash as usize & mask;
        while self.slots[position].load(Ordering::Relaxed) != 0 {
            position = (position + 1) & mask;
        }
        // A reader that sees the slot may read the columns at this id, which the writer
        // filled before this store.
        self.slots[position].store(
            (hash >> 32) << 32 | u64::from(ty.bits.get()),
            Ordering::Release,
        );
    }
}

const INITIAL_TYPE_SLOTS: usize = 1 << 12;

fn intern_type<'db>(kind: TyKind<'_>) -> Ty<'db> {
    let hash = FxBuildHasher.hash_one(kind);
    // SAFETY: a published table is leaked, so the pointer is null or valid forever.
    let table = unsafe { TYPE_TABLE.load(Ordering::Acquire).as_ref() };
    match table.and_then(|table| table.find(hash, &kind)) {
        Some(found) => found,
        None => insert_type(kind, hash),
    }
}

#[cold]
fn insert_type<'db>(kind: TyKind<'_>, hash: u64) -> Ty<'db> {
    // A poisoned lock means another thread died inside this function. Every step that
    // can panic comes before the columns and the table change, so they are still coherent.
    let mut writer = TYPE_WRITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let writer = writer.get_or_insert_with(|| TypeWriter {
        arena: Bump::new(),
        count: 0,
    });
    // SAFETY: as in `intern_type`.
    let mut table = unsafe { TYPE_TABLE.load(Ordering::Acquire).as_ref() };
    // Another thread may have stored the kind between this thread's lookup and the lock.
    if let Some(found) = table.and_then(|table| table.find(hash, &kind)) {
        return found;
    }
    let index = writer.count + LEAVES;
    assert!(
        index <= INDEX_BITS as usize,
        "the type store is full: it holds at most 2^28 distinct types"
    );

    let mut size: u32 = 1;
    let mut contents = match kind {
        TyKind::Var(_) => HAS_VAR,
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
        let stored: &[Name<'static>] = writer.arena.alloc_slice_copy(&applied);
        // SAFETY: the arena lives in a static and is never reset, so the slice is immortal.
        unsafe { mem::transmute::<&[Name<'static>], &'static [Name<'static>]>(stored) }
    };
    let stored = stored_kind(&writer.arena, kind);
    // SAFETY: the kind's slices now point into the arena, which is immortal as above, and
    // the lifetimes of `Ty` and `Name` are phantom.
    let stored = unsafe { mem::transmute::<TyKind<'_>, TyKind<'static>>(stored) };

    if (writer.count + 1) * 2 > table.map_or(0, |table| table.slots.len()) {
        let slots = table.map_or(INITIAL_TYPE_SLOTS, |table| table.slots.len() * 2);
        // The table it replaces stays allocated, because a lookup on another thread may
        // still be probing it. The retired tables together are smaller than the live one.
        let grown: &'static TypeTable = Box::leak(Box::new(TypeTable {
            slots: (0..slots).map(|_| AtomicU64::new(0)).collect(),
        }));
        for slot in table.iter().flat_map(|table| table.slots.iter()) {
            let slot = slot.load(Ordering::Relaxed);
            if slot != 0 {
                let ty = Ty::<'static>::from_bits(slot as u32);
                grown.place(FxBuildHasher.hash_one(*ty.kind(&())), ty);
            }
        }
        TYPE_TABLE.store(ptr::from_ref(grown).cast_mut(), Ordering::Release);
        table = Some(grown);
    }

    let pushed = KINDS.push(stored);
    let pushed_facts = FACTS.push(TypeFacts {
        size,
        applied_names,
    });
    debug_assert!(
        pushed == writer.count && pushed_facts == writer.count,
        "the store's columns are pushed together under the writer lock"
    );
    writer.count += 1;
    let ty = Ty::from_bits(index as u32 | contents);
    if let Some(table) = table {
        table.place(hash, ty);
    }
    ty
}

/// `kind` with every list it borrows copied into `arena`.
fn stored_kind<'a>(arena: &'a Bump, kind: TyKind<'a>) -> TyKind<'a> {
    match kind {
        TyKind::Tuple(items) => TyKind::Tuple(arena.alloc_slice_copy(items)),
        TyKind::Record(fields) => TyKind::Record(arena.alloc_slice_copy(fields)),
        TyKind::NativeTuple(items) => TyKind::NativeTuple(arena.alloc_slice_copy(items)),
        TyKind::NativeRecord(fields) => TyKind::NativeRecord(arena.alloc_slice_copy(fields)),
        TyKind::Frame(frame, Some(columns)) => {
            TyKind::Frame(frame, Some(arena.alloc_slice_copy(columns)))
        }
        TyKind::Function(function) => TyKind::Function(arena.alloc(FunctionType {
            positional: arena.alloc_slice_copy(function.positional),
            named: arena.alloc_slice_copy(function.named),
            quoted: arena.alloc_slice_copy(function.quoted),
            default_reads: arena.alloc_slice_copy(function.default_reads),
            missing_tested: arena.alloc_slice_copy(function.missing_tested),
            condition_only: arena.alloc_slice_copy(function.condition_only),
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
        | TyKind::Frame(_, None)
        | TyKind::Var(_)
        | TyKind::Rigid(_) => kind,
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::RootDatabase, std::thread};

    #[test]
    fn size_counts_the_tree_not_the_shared_graph() {
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
    }

    #[test]
    fn size_saturates_at_the_ceiling() {
        let db = RootDatabase::default();
        let mut ty = scalar(&db, Atomic::Integer);
        for _ in 0..64 {
            ty = Ty::new(&db, TyKind::Tuple(&[ty, ty]));
        }
        assert_eq!(type_size(&db, ty), TYPE_SIZE_CEILING);
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
    fn interning_gives_id_equality() {
        let db = RootDatabase::default();
        let integer = scalar(&db, Atomic::Integer);
        let double = scalar(&db, Atomic::Double);
        assert_eq!(integer, scalar(&db, Atomic::Integer));
        assert_eq!(integer, Ty::new(&db, TyKind::Scalar(Atomic::Integer)));
        let first = Ty::new(
            &db,
            TyKind::Function(&FunctionType::declared(&[integer], &[], None, double)),
        );
        let positional = vec![integer];
        let second = Ty::new(
            &db,
            TyKind::Function(&FunctionType::declared(&positional, &[], None, double)),
        );
        assert_eq!(first, second);
        assert_ne!(
            first,
            Ty::new(
                &db,
                TyKind::Function(&FunctionType::declared(&[double], &[], None, double)),
            )
        );
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
    }

    fn tag<'db>(db: &'db RootDatabase, name: &str, items: Option<&[Ty<'db>]>) -> Ty<'db> {
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
                tag(&db, "Some", Some(&[int])),
                tag(&db, "Some", Some(&[chr])),
            ],
        );
        assert_eq!(
            merged,
            tag(&db, "Some", Some(&[union_of(&db, [int, chr])])),
            "one member per tag name and shape"
        );
        assert_eq!(conflicting_tags(&db, merged), None);
    }

    #[test]
    fn different_shape_tags_stay_apart_and_conflict() {
        let db = RootDatabase::default();
        let int = scalar(&db, Atomic::Integer);
        let one = tag(&db, "Some", Some(&[int]));
        let two = tag(&db, "Some", Some(&[int, int]));
        let union = union_of(&db, [one, two]);
        let TyKind::Union(members) = union.kind(&db) else {
            panic!("expected a union")
        };
        assert_eq!(*members, &[one, two]);
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
        assert_eq!(*members, &[none, null(&db), other]);
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

        let per_thread: Vec<Vec<(Ty<'static>, Ty<'static>, Ty<'static>)>> = thread::scope(|scope| {
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
            assert_eq!(stored_size(function), 9);
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
