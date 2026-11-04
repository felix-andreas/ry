//! Spans are item-relative, so an edit elsewhere in the file reproduces an
//! equal `Module` and salsa cuts off before naming or inference rerun.

use {
    rustc_hash::{FxHashMap, FxHasher},
    std::{
        cell::{Cell, RefCell},
        fmt,
        hash::{Hash, Hasher},
        marker::PhantomData,
        mem::MaybeUninit,
        num::NonZeroU32,
        ops, ptr, slice,
        sync::{LazyLock, Mutex},
    },
    syntax::{
        ElementRef, NodeRef, NumberType, SyntaxKind, SyntaxNode, TextRange, TextSize, TokenRef,
        ast::{is_expression_kind, name_text},
        split_number_suffix,
    },
};

/// An expression's index in its [`Module`]. It stores the index plus one, so an
/// `Option<ExprId>` is four bytes like the id itself; that keeps every `ExpressionKind` at
/// sixteen bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct ExprId(NonZeroU32);

/// A pattern's index in its [`Module`], stored like [`ExprId`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::SalsaValue)]
pub struct PatId(NonZeroU32);

/// A name written at a site, such as the `b` of `a$b`, an argument's tag or a `for`
/// variable, stored like [`ExprId`] and resolved with `module[id]`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NameId(NonZeroU32);

macro_rules! dense_id {
    ($id:ident) => {
        impl $id {
            pub fn new(index: usize) -> $id {
                // Spans are `u32` offsets into the item's text, so no module comes near
                // `u32::MAX` entries; past it ids would alias, which is worse than stopping.
                u32::try_from(index)
                    .ok()
                    .and_then(|index| NonZeroU32::MIN.checked_add(index))
                    .map($id)
                    .expect("an item holds fewer than u32::MAX HIR entries")
            }

            pub fn index(self) -> usize {
                (self.0.get() - 1) as usize
            }
        }

        impl fmt::Debug for $id {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", stringify!($id), self.index())
            }
        }
    };
}

dense_id!(ExprId);
dense_id!(PatId);
dense_id!(NameId);

/// The spelling and range of a name written at a site; see [`NameId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameSite {
    pub name: Symbol,
    pub range: TextRange,
}

/// An identifier, interned process-wide: the same spelling is one allocation shared by every
/// item that mentions it, and a symbol is a four-byte index into that table, so an
/// `Option<Symbol>` costs no more than the symbol. The table only grows, which is bounded by the
/// distinct spellings a session has seen.
#[derive(Clone, Copy, salsa::SalsaValue)]
pub struct Symbol(NonZeroU32);

const SYMBOL_SHARDS: usize = 16;

/// Spelling to symbol, sharded by the spelling's hash so parallel lowering rarely contends.
static SYMBOL_IDS: LazyLock<[Mutex<FxHashMap<&'static str, Symbol>>; SYMBOL_SHARDS]> =
    LazyLock::new(|| std::array::from_fn(|_| Mutex::new(FxHashMap::default())));

/// Symbol to spelling. Appending never moves an entry, so a read needs no lock.
static SYMBOL_TEXT: LazyLock<boxcar::Vec<SymbolEntry>> = LazyLock::new(boxcar::Vec::new);

/// What a hot loop asks of a symbol without reading its spelling: the spelling's hash for a
/// map probe, its first bytes for an ordered set's comparison, and its length in characters
/// for the typo scan's length filter.
struct SymbolEntry {
    text: &'static str,
    hash: u64,
    /// The first eight bytes, big-endian and zero-padded, so comparing two prefixes orders
    /// the spellings whenever they differ in those bytes.
    prefix: u64,
    characters: u32,
}

thread_local! {
    /// Spellings this thread has interned. Lowering interns every identifier it meets, and
    /// the shard lock's atomics bounce cache lines between analysis threads, so a repeat
    /// spelling skips the shared table.
    static LOCAL_SYMBOL_IDS: RefCell<FxHashMap<&'static str, Symbol>> =
        RefCell::new(FxHashMap::default());
}

impl Symbol {
    pub fn new(text: &str) -> Symbol {
        if let Some(symbol) = LOCAL_SYMBOL_IDS.with_borrow(|local| local.get(text).copied()) {
            return symbol;
        }
        let symbol = Symbol::new_shared(text);
        LOCAL_SYMBOL_IDS.with_borrow_mut(|local| local.insert(symbol.as_str(), symbol));
        symbol
    }

    /// The symbol for `text` if any item has interned it, without interning it: a spelling
    /// nothing has interned cannot key a map of symbols, and a probe must not grow the
    /// process-wide table.
    pub fn lookup(text: &str) -> Option<Symbol> {
        if let Some(symbol) = LOCAL_SYMBOL_IDS.with_borrow(|local| local.get(text).copied()) {
            return Some(symbol);
        }
        symbol_shard(text)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(text)
            .copied()
    }

    fn new_shared(text: &str) -> Symbol {
        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        let text_hash = hasher.finish();
        let shard = &SYMBOL_IDS[text_hash as usize % SYMBOL_SHARDS];
        // A poisoned shard means another thread died mid-insert; the map is still a valid
        // map of leaked strings, so keep serving it.
        let mut shard = shard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(&symbol) = shard.get(text) {
            return symbol;
        }
        let interned: &'static str = Box::leak(text.into());
        // Pushed under the shard lock, so one spelling can never get two indexes.
        let index = SYMBOL_TEXT.push(SymbolEntry {
            text: interned,
            hash: text_hash,
            prefix: {
                let mut bytes = [0u8; 8];
                let head = interned.len().min(8);
                bytes[..head].copy_from_slice(&interned.as_bytes()[..head]);
                u64::from_be_bytes(bytes)
            },
            characters: u32::try_from(interned.chars().count()).unwrap_or(u32::MAX),
        });
        let symbol = Symbol(
            u32::try_from(index + 1)
                .ok()
                .and_then(NonZeroU32::new)
                .expect("fewer than 2^32 distinct identifiers"),
        );
        shard.insert(interned, symbol);
        symbol
    }

    /// The symbol's position in the table: stable for the process, but it depends on the
    /// order in which threads first met each spelling.
    pub fn index(self) -> u32 {
        self.0.get()
    }

    pub fn as_str(self) -> &'static str {
        self.entry().text
    }

    /// The spelling's length in characters.
    pub fn characters(self) -> usize {
        self.entry().characters as usize
    }

    fn entry(self) -> &'static SymbolEntry {
        SYMBOL_TEXT
            .get(self.0.get() as usize - 1)
            .expect("a symbol indexes the table that minted it")
    }
}

fn symbol_shard(text: &str) -> &'static Mutex<FxHashMap<&'static str, Symbol>> {
    let mut hasher = FxHasher::default();
    text.hash(&mut hasher);
    &SYMBOL_IDS[hasher.finish() as usize % SYMBOL_SHARDS]
}

impl ops::Deref for Symbol {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Symbol) -> bool {
        self.0 == other.0
    }
}

impl Eq for Symbol {}

impl PartialEq<str> for Symbol {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Symbol {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Symbol> for String {
    fn eq(&self, other: &Symbol) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Symbol> for &str {
    fn eq(&self, other: &Symbol) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Symbol> for str {
    fn eq(&self, other: &Symbol) -> bool {
        self == other.as_str()
    }
}

impl Default for Symbol {
    fn default() -> Symbol {
        Symbol::new("")
    }
}

impl PartialEq<String> for Symbol {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}

/// The spelling's hash, computed once when the symbol is minted. Project-wide name maps hash
/// every name on each rebuild, and hashing the text dominated them. Hashing the index would be
/// cheaper still, but it depends on which thread met a spelling first, so map iteration order
/// would differ between runs.
impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.entry().hash);
    }
}

impl PartialOrd for Symbol {
    fn partial_cmp(&self, other: &Symbol) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Orders by spelling, which unlike the index is the same in every run. Equal prefixes fall
/// back to the full text; a zero-padded prefix only ties when the bytes it holds tie.
impl Ord for Symbol {
    fn cmp(&self, other: &Symbol) -> std::cmp::Ordering {
        if self.0 == other.0 {
            return std::cmp::Ordering::Equal;
        }
        let (left, right) = (self.entry(), other.entry());
        left.prefix
            .cmp(&right.prefix)
            .then_with(|| left.text.cmp(right.text))
    }
}

impl AsRef<str> for Symbol {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for Symbol {
    fn from(text: &str) -> Symbol {
        Symbol::new(text)
    }
}

impl From<String> for Symbol {
    fn from(text: String) -> Symbol {
        Symbol::new(&text)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), formatter)
    }
}

/// One item's expressions and patterns as dense arrays. An expression is an `ExprId` index
/// into the kinds and ranges, which are split because inference walks kinds and only
/// diagnostics read ranges. Every variable-length child list lives in one side array per
/// element type and an expression holds a [`Span`] into it, so every kind is `Copy`.
///
/// All the arrays share one allocation, laid out as a header and then one section per array.
/// A project holds one module per item for as long as it runs, so a dozen separate vectors
/// would cost a dozen allocations, headers and size-class remainders per item, which together
/// came to a quarter of the HIR's memory. Ranges are item-relative, and an item shorter than
/// 64 KiB stores each in one word, half of a `TextRange`.
#[derive(Clone)]
pub struct Module {
    words: Box<[MaybeUninit<u32>]>,
    pub root: Option<ExprId>,
}

/// The header's words: the text's length in bytes, then where each section starts and where
/// the last one ends, in words.
const TEXT_LENGTH: usize = 0;
const SECTION_STARTS: usize = 1;
const SECTIONS: usize = 12;
const HEADER_WORDS: usize = SECTION_STARTS + SECTIONS + 1;

const KINDS: usize = 0;
const RANGES: usize = 1;
const PATTERNS: usize = 2;
const ARGUMENTS: usize = 3;
const PARAMETERS: usize = 4;
const EXPRESSION_LISTS: usize = 5;
const RECORD_FIELDS: usize = 6;
const ARMS: usize = 7;
const PATTERN_LISTS: usize = 8;
const PATTERN_FIELDS: usize = 9;
const NAME_SITES: usize = 10;
const TEXT: usize = 11;

/// An element type a [`Module`] section can hold: plain data whose size is whole words and
/// whose alignment a word satisfies, so a section of words reads back as a slice of it.
///
/// # Safety
///
/// `size_of::<Self>()` must be a nonzero multiple of four and `align_of::<Self>()` at most
/// four.
unsafe trait Element: Copy {}

macro_rules! element {
    ($($element:ty),*) => {
        $(
            const _: () = assert!(
                size_of::<$element>() % 4 == 0 && size_of::<$element>() > 0 && align_of::<$element>() <= 4
            );
            // SAFETY: the assertion above checks the size and alignment.
            unsafe impl Element for $element {}
        )*
    };
}

element!(
    ExpressionKind,
    TextRange,
    u32,
    Pattern,
    Argument,
    Parameter,
    ExprId,
    RecordField,
    MatchArm,
    PatId,
    PatternField,
    NameSite
);

/// The arrays lowering appends to, sealed into a [`Module`] once the item is done.
#[derive(Default)]
struct Parts {
    kinds: Vec<ExpressionKind>,
    ranges: Vec<TextRange>,
    patterns: Vec<Pattern>,
    arguments: Vec<Argument>,
    parameters: Vec<Parameter>,
    expression_lists: Vec<ExprId>,
    record_fields: Vec<RecordField>,
    arms: Vec<MatchArm>,
    pattern_lists: Vec<PatId>,
    pattern_fields: Vec<PatternField>,
    name_sites: Vec<NameSite>,
    text: String,
}

impl Parts {
    fn seal(&self, root: ExprId) -> Module {
        let narrow = self
            .ranges
            .iter()
            .all(|range| u32::from(range.end()) <= u32::from(u16::MAX));
        let narrow_ranges: Vec<u32> = match narrow {
            true => self
                .ranges
                .iter()
                .map(|range| u32::from(range.start()) | u32::from(range.len()) << 16)
                .collect(),
            false => Vec::new(),
        };
        let text = text_bytes(&self.text);
        let sections: [&[MaybeUninit<u8>]; SECTIONS] = [
            element_bytes(&self.kinds),
            match narrow {
                true => element_bytes(&narrow_ranges),
                false => element_bytes(&self.ranges),
            },
            element_bytes(&self.patterns),
            element_bytes(&self.arguments),
            element_bytes(&self.parameters),
            element_bytes(&self.expression_lists),
            element_bytes(&self.record_fields),
            element_bytes(&self.arms),
            element_bytes(&self.pattern_lists),
            element_bytes(&self.pattern_fields),
            element_bytes(&self.name_sites),
            text,
        ];
        // Sized exactly before the first push: mimalloc keeps a block that a vector shrinks
        // into when the new size is at least half the old one, so growing and then boxing
        // would retain up to twice the memory.
        let total = HEADER_WORDS
            + sections
                .iter()
                .map(|bytes| bytes.len().div_ceil(4))
                .sum::<usize>();
        let mut words = Vec::with_capacity(total);
        words.resize(HEADER_WORDS, MaybeUninit::new(0));
        for (section, bytes) in sections.into_iter().enumerate() {
            words[SECTION_STARTS + section] = MaybeUninit::new(to_u32(words.len()));
            let start = words.len();
            words.resize(start + bytes.len().div_ceil(4), MaybeUninit::new(0));
            // SAFETY: the destination was just resized to hold `bytes`, and the two never
            // overlap because `bytes` borrows another buffer.
            unsafe {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    words[start..].as_mut_ptr().cast::<MaybeUninit<u8>>(),
                    bytes.len(),
                );
            }
        }
        words[SECTION_STARTS + SECTIONS] = MaybeUninit::new(to_u32(words.len()));
        words[TEXT_LENGTH] = MaybeUninit::new(to_u32(self.text.len()));
        Module {
            words: words.into_boxed_slice(),
            root: Some(root),
        }
    }

    fn clear(&mut self) {
        self.kinds.clear();
        self.ranges.clear();
        self.patterns.clear();
        self.arguments.clear();
        self.parameters.clear();
        self.expression_lists.clear();
        self.record_fields.clear();
        self.arms.clear();
        self.pattern_lists.clear();
        self.pattern_fields.clear();
        self.name_sites.clear();
        self.text.clear();
    }
}

fn to_u32(count: usize) -> u32 {
    u32::try_from(count).expect("an item's HIR is shorter than u32::MAX words")
}

/// An element slice's bytes, padding included, which is why they stay `MaybeUninit`.
fn element_bytes<T: Element>(elements: &[T]) -> &[MaybeUninit<u8>] {
    // SAFETY: any byte of a live value may be read as a `MaybeUninit<u8>`, and the length is
    // the slice's size in bytes.
    unsafe {
        slice::from_raw_parts(
            elements.as_ptr().cast::<MaybeUninit<u8>>(),
            size_of_val(elements),
        )
    }
}

fn text_bytes(text: &str) -> &[MaybeUninit<u8>] {
    // SAFETY: initialized bytes are valid `MaybeUninit<u8>`s.
    unsafe { slice::from_raw_parts(text.as_ptr().cast::<MaybeUninit<u8>>(), text.len()) }
}

/// The arrays a sealed [`Module`] and the lowering's growing [`Parts`] both read through, for
/// the walks lowering needs over what it has built so far.
trait Arrays {
    fn kind_array(&self) -> &[ExpressionKind];
    fn pattern_array(&self) -> &[Pattern];
    fn argument_array(&self) -> &[Argument];
    fn pattern_list_array(&self) -> &[PatId];
    fn pattern_field_array(&self) -> &[PatternField];
}

impl Arrays for Parts {
    fn kind_array(&self) -> &[ExpressionKind] {
        &self.kinds
    }

    fn pattern_array(&self) -> &[Pattern] {
        &self.patterns
    }

    fn argument_array(&self) -> &[Argument] {
        &self.arguments
    }

    fn pattern_list_array(&self) -> &[PatId] {
        &self.pattern_lists
    }

    fn pattern_field_array(&self) -> &[PatternField] {
        &self.pattern_fields
    }
}

impl Arrays for Module {
    fn kind_array(&self) -> &[ExpressionKind] {
        self.section(KINDS)
    }

    fn pattern_array(&self) -> &[Pattern] {
        self.section(PATTERNS)
    }

    fn argument_array(&self) -> &[Argument] {
        self.section(ARGUMENTS)
    }

    fn pattern_list_array(&self) -> &[PatId] {
        self.section(PATTERN_LISTS)
    }

    fn pattern_field_array(&self) -> &[PatternField] {
        self.section(PATTERN_FIELDS)
    }
}

impl Default for Module {
    fn default() -> Module {
        let mut words = vec![MaybeUninit::new(0); HEADER_WORDS];
        for start in &mut words[SECTION_STARTS..] {
            *start = MaybeUninit::new(HEADER_WORDS as u32);
        }
        Module {
            words: words.into_boxed_slice(),
            root: None,
        }
    }
}

impl PartialEq for Module {
    fn eq(&self, other: &Module) -> bool {
        self.root == other.root
            && self.kinds() == other.kinds()
            && self.ranges().eq(other.ranges())
            && self.patterns() == other.patterns()
            && self.section::<Argument>(ARGUMENTS) == other.section::<Argument>(ARGUMENTS)
            && self.section::<Parameter>(PARAMETERS) == other.section::<Parameter>(PARAMETERS)
            && self.section::<ExprId>(EXPRESSION_LISTS) == other.section::<ExprId>(EXPRESSION_LISTS)
            && self.section::<RecordField>(RECORD_FIELDS)
                == other.section::<RecordField>(RECORD_FIELDS)
            && self.section::<MatchArm>(ARMS) == other.section::<MatchArm>(ARMS)
            && self.section::<PatId>(PATTERN_LISTS) == other.section::<PatId>(PATTERN_LISTS)
            && self.section::<PatternField>(PATTERN_FIELDS)
                == other.section::<PatternField>(PATTERN_FIELDS)
            && self.section::<NameSite>(NAME_SITES) == other.section::<NameSite>(NAME_SITES)
            && self.text() == other.text()
    }
}

impl Eq for Module {}

impl fmt::Debug for Module {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Module")
            .field("kinds", &self.kinds())
            .field("ranges", &self.ranges().collect::<Vec<_>>())
            .field("patterns", &self.patterns())
            .field("arguments", &self.section::<Argument>(ARGUMENTS))
            .field("parameters", &self.section::<Parameter>(PARAMETERS))
            .field(
                "expression_lists",
                &self.section::<ExprId>(EXPRESSION_LISTS),
            )
            .field("record_fields", &self.section::<RecordField>(RECORD_FIELDS))
            .field("arms", &self.section::<MatchArm>(ARMS))
            .field("pattern_lists", &self.section::<PatId>(PATTERN_LISTS))
            .field(
                "pattern_fields",
                &self.section::<PatternField>(PATTERN_FIELDS),
            )
            .field("name_sites", &self.section::<NameSite>(NAME_SITES))
            .field("text", &self.text())
            .field("root", &self.root)
            .finish()
    }
}

/// One expression of a [`Module`], as a scan over [`Module::expressions`] yields it.
#[derive(Debug, Clone, Copy)]
pub struct Expression<'a> {
    pub kind: &'a ExpressionKind,
    pub range: TextRange,
}

/// A run of a [`Module`] side array: `module[span]` is the slice.
pub struct Span<T> {
    start: u32,
    len: u32,
    element: PhantomData<fn() -> T>,
}

impl<T> Span<T> {
    pub fn len(self) -> usize {
        self.len as usize
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    fn range(self) -> ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

impl<T> Clone for Span<T> {
    fn clone(&self) -> Span<T> {
        *self
    }
}

impl<T> Copy for Span<T> {}

impl<T> PartialEq for Span<T> {
    fn eq(&self, other: &Span<T>) -> bool {
        (self.start, self.len) == (other.start, other.len)
    }
}

impl<T> Eq for Span<T> {}

impl<T> Hash for Span<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.start, self.len).hash(state);
    }
}

impl<T> Default for Span<T> {
    fn default() -> Span<T> {
        Span {
            start: 0,
            len: 0,
            element: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Span<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}..{}", self.start, self.start + self.len)
    }
}

/// Literal source text in the module's text buffer: `module[text]` is the string. Unlike a
/// [`Symbol`], which is interned for the life of the process, it is freed with the module, so
/// editing a string literal does not keep every spelling it passed through.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Text {
    start: u32,
    len: u32,
}

impl Text {
    fn range(self) -> ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "text {}..{}", self.start, self.start + self.len)
    }
}

impl ops::Index<NameId> for Module {
    type Output = NameSite;

    fn index(&self, id: NameId) -> &NameSite {
        &self.section::<NameSite>(NAME_SITES)[id.index()]
    }
}

impl ops::Index<&NameId> for Module {
    type Output = NameSite;

    fn index(&self, id: &NameId) -> &NameSite {
        &self[*id]
    }
}

impl ops::Index<Text> for Module {
    type Output = str;

    fn index(&self, text: Text) -> &str {
        &self.text()[text.range()]
    }
}

impl ops::Index<&Text> for Module {
    type Output = str;

    fn index(&self, text: &Text) -> &str {
        &self[*text]
    }
}

macro_rules! side_array {
    ($element:ty, $section:ident) => {
        impl ops::Index<Span<$element>> for Module {
            type Output = [$element];

            fn index(&self, span: Span<$element>) -> &[$element] {
                &self.section::<$element>($section)[span.range()]
            }
        }

        impl ops::Index<&Span<$element>> for Module {
            type Output = [$element];

            fn index(&self, span: &Span<$element>) -> &[$element] {
                &self[*span]
            }
        }
    };
}

side_array!(Argument, ARGUMENTS);
side_array!(Parameter, PARAMETERS);
side_array!(ExprId, EXPRESSION_LISTS);
side_array!(RecordField, RECORD_FIELDS);
side_array!(MatchArm, ARMS);
side_array!(PatId, PATTERN_LISTS);
side_array!(PatternField, PATTERN_FIELDS);

impl Module {
    pub fn kind(&self, id: ExprId) -> &ExpressionKind {
        &self.kinds()[id.index()]
    }

    pub fn expression(&self, id: ExprId) -> Expression<'_> {
        Expression {
            kind: self.kind(id),
            range: self.range(id),
        }
    }

    pub fn range(&self, id: ExprId) -> TextRange {
        let index = id.index();
        match self.narrow_ranges() {
            Some(ranges) => narrow_range(ranges[index]),
            None => self.section::<TextRange>(RANGES)[index],
        }
    }

    /// Every expression's kind, indexed by `ExprId`.
    pub fn kinds(&self) -> &[ExpressionKind] {
        self.section(KINDS)
    }

    /// Every expression's significant source range, in `ExprId` order.
    pub fn ranges(&self) -> impl ExactSizeIterator<Item = TextRange> + Clone + '_ {
        let (narrow, wide) = match self.narrow_ranges() {
            Some(ranges) => (ranges, &[][..]),
            None => (&[][..], self.section::<TextRange>(RANGES)),
        };
        Ranges { narrow, wide }
    }

    /// Every expression with its id order, for a scan that reads both kind and range.
    pub fn expressions(&self) -> impl ExactSizeIterator<Item = Expression<'_>> + Clone {
        self.kinds()
            .iter()
            .zip(self.ranges())
            .map(|(kind, range)| Expression { kind, range })
    }

    pub fn name(&self, id: NameId) -> Symbol {
        self[id].name
    }

    pub fn name_range(&self, id: NameId) -> TextRange {
        self[id].range
    }

    pub fn argument_name(&self, argument: &Argument) -> Option<Symbol> {
        argument.name.map(|name| self[name].name)
    }

    pub fn argument_name_range(&self, argument: &Argument) -> Option<TextRange> {
        argument.name.map(|name| self[name].range)
    }

    pub fn expression_count(&self) -> usize {
        self.kinds().len()
    }

    pub fn expression_ids(&self) -> impl Iterator<Item = ExprId> + use<> {
        (0..self.kinds().len()).map(ExprId::new)
    }

    /// Every pattern, indexed by `PatId`.
    pub fn patterns(&self) -> &[Pattern] {
        self.section(PATTERNS)
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns().len()
    }

    pub fn pattern(&self, id: PatId) -> &Pattern {
        &self.patterns()[id.index()]
    }

    fn text(&self) -> &str {
        let words = self.section_words(TEXT);
        let length = (self.header(TEXT_LENGTH) as usize).min(size_of_val(words));
        // SAFETY: sealing copied a whole `str` to the start of this section and recorded its
        // length, so these bytes are initialized UTF-8.
        unsafe {
            std::str::from_utf8_unchecked(slice::from_raw_parts(
                words.as_ptr().cast::<u8>(),
                length,
            ))
        }
    }

    /// The ranges when the item is short enough to store each in one word.
    fn narrow_ranges(&self) -> Option<&[u32]> {
        let ranges = self.section_words(RANGES);
        (ranges.len() == self.kinds().len()).then(|| {
            // SAFETY: sealing wrote one initialized word per narrow range.
            unsafe { slice::from_raw_parts(ranges.as_ptr().cast::<u32>(), ranges.len()) }
        })
    }

    fn section<T: Element>(&self, section: usize) -> &[T] {
        let words = self.section_words(section);
        // SAFETY: sealing copied a slice of `T` to the start of this section, and `Element`
        // guarantees `T` is whole words with an alignment a word satisfies, so the section's
        // words are exactly that slice. The wide and narrow range encodings are told apart
        // before a range section is read as either.
        unsafe {
            slice::from_raw_parts(
                words.as_ptr().cast::<T>(),
                size_of_val(words) / size_of::<T>(),
            )
        }
    }

    fn section_words(&self, section: usize) -> &[MaybeUninit<u32>] {
        let start = self.header(SECTION_STARTS + section) as usize;
        let end = self.header(SECTION_STARTS + section + 1) as usize;
        &self.words[start..end]
    }

    fn header(&self, index: usize) -> u32 {
        // SAFETY: both `seal` and `default` initialize every header word.
        unsafe { self.words[index].assume_init() }
    }
}

/// A narrow range: the start in the low half-word, the length in the high one.
fn narrow_range(word: u32) -> TextRange {
    TextRange::at(TextSize::from(word & 0xFFFF), TextSize::from(word >> 16))
}

#[derive(Clone)]
struct Ranges<'a> {
    narrow: &'a [u32],
    wide: &'a [TextRange],
}

impl Iterator for Ranges<'_> {
    type Item = TextRange;

    fn next(&mut self) -> Option<TextRange> {
        if let Some((&first, rest)) = self.narrow.split_first() {
            self.narrow = rest;
            return Some(narrow_range(first));
        }
        let (&first, rest) = self.wide.split_first()?;
        self.wide = rest;
        Some(first)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = self.narrow.len() + self.wide.len();
        (length, Some(length))
    }
}

impl ExactSizeIterator for Ranges<'_> {}

impl Module {
    /// The direct child expressions of `id`, in source order.
    pub fn for_each_child(&self, id: ExprId, mut visit: impl FnMut(ExprId)) {
        match *self.kind(id) {
            ExpressionKind::Missing
            | ExpressionKind::Literal(_)
            | ExpressionKind::NameRef(_)
            | ExpressionKind::Namespace { .. }
            | ExpressionKind::Break
            | ExpressionKind::Next => {}
            ExpressionKind::Assign { target, value, .. } => {
                visit(target);
                visit(value);
            }
            ExpressionKind::Unary { operand, .. } => visit(operand),
            ExpressionKind::Binary { lhs, rhs, .. } => {
                visit(lhs);
                visit(rhs);
            }
            ExpressionKind::Call {
                callee: target,
                arguments,
            }
            | ExpressionKind::Index {
                target, arguments, ..
            } => {
                visit(target);
                self[arguments]
                    .iter()
                    .filter_map(|argument| argument.value)
                    .for_each(visit);
            }
            ExpressionKind::Field { target, .. } => visit(target),
            ExpressionKind::Function { parameters, body } => {
                self[parameters]
                    .iter()
                    .filter_map(|parameter| parameter.default)
                    .for_each(&mut visit);
                visit(body);
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                visit(condition);
                visit(then_branch);
                else_branch.into_iter().for_each(visit);
            }
            ExpressionKind::For { sequence, body, .. } => {
                visit(sequence);
                visit(body);
            }
            ExpressionKind::While { condition, body } => {
                visit(condition);
                visit(body);
            }
            ExpressionKind::Repeat { body } | ExpressionKind::Local { body } => visit(body),
            ExpressionKind::Block { statements: items }
            | ExpressionKind::Tuple(items)
            | ExpressionKind::Array(items) => self[items].iter().copied().for_each(visit),
            ExpressionKind::Paren(inner) => visit(inner),
            ExpressionKind::Record(fields) => {
                self[fields].iter().map(|field| field.value).for_each(visit)
            }
            ExpressionKind::RecordUpdate { base, fields } => {
                visit(base);
                self[fields].iter().map(|field| field.value).for_each(visit)
            }
            ExpressionKind::FormatString(pieces) => self[pieces].iter().copied().for_each(visit),
            ExpressionKind::Try(operand) => visit(operand),
            ExpressionKind::Tag { payload, .. } => payload.into_iter().for_each(visit),
            ExpressionKind::Match { scrutinee, arms } => {
                visit(scrutinee);
                for arm in &self[arms] {
                    self[arm.bindings].iter().copied().for_each(&mut visit);
                    arm.guard.into_iter().for_each(&mut visit);
                    visit(arm.body);
                }
            }
        }
    }

    /// Whether any direct child of `id` satisfies `test`, stopping at the first that does.
    pub fn any_child(&self, id: ExprId, mut test: impl FnMut(ExprId) -> bool) -> bool {
        let mut found = false;
        self.for_each_child(id, |child| {
            if !found {
                found = test(child);
            }
        });
        found
    }

    /// Whether a tag payload pattern is a bare `..`, as in `:Tag(..)`, which matches the tag
    /// with any payload or none.
    pub fn is_any_payload(&self, payload: PatId) -> bool {
        matches!(self.pattern(payload).kind, PatternKind::Record { fields, rest: Some(Rest { binding: None }) } if fields.is_empty())
    }

    /// The names a pattern binds, as `NameRef` expressions, in source order.
    pub fn pattern_bindings(&self, pattern: PatId) -> Vec<ExprId> {
        pattern_bindings(self, pattern)
    }
}

fn pattern_bindings(arrays: &impl Arrays, pattern: PatId) -> Vec<ExprId> {
    fn collect(arrays: &impl Arrays, pattern: PatId, bindings: &mut Vec<ExprId>) {
        match arrays.pattern_array()[pattern.index()].kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Literal { .. } => {}
            PatternKind::Bind(name) => bindings.push(name),
            PatternKind::Tag { payload, .. } => {
                if let Some(payload) = payload {
                    collect(arrays, payload, bindings);
                }
            }
            // Every alternative binds the same names (naming reports it otherwise), and each
            // alternative's leaf is its own write site, so all of them are listed.
            PatternKind::Tuple(items) | PatternKind::Or(items) => {
                for &item in &arrays.pattern_list_array()[items.range()] {
                    collect(arrays, item, bindings);
                }
            }
            PatternKind::Record { fields, rest } => {
                for field in &arrays.pattern_field_array()[fields.range()] {
                    collect(arrays, field.pattern, bindings);
                }
                bindings.extend(rest.and_then(|rest| rest.binding));
            }
            PatternKind::List { items, rest } => {
                let items = &arrays.pattern_list_array()[items.range()];
                let position = rest.map_or(items.len(), |rest| rest.position as usize);
                let (before, after) = items.split_at(position.min(items.len()));
                for &item in before {
                    collect(arrays, item, bindings);
                }
                bindings.extend(rest.and_then(|rest| rest.rest.binding));
                for &item in after {
                    collect(arrays, item, bindings);
                }
            }
            PatternKind::As { pattern, binding } => {
                collect(arrays, pattern, bindings);
                bindings.push(binding);
            }
        }
    }
    let mut bindings = Vec::new();
    collect(arrays, pattern, &mut bindings);
    bindings
}

/// The names a zeallot pattern binds, in order; `None` unless the pattern is `c()` or `list()`
/// of names and nested patterns (a computed element makes it an ordinary operator call).
pub fn destructure_leaves(module: &Module, pattern: ExprId) -> Option<Vec<ExprId>> {
    leaves_of(module, pattern)
}

fn leaves_of(arrays: &impl Arrays, pattern: ExprId) -> Option<Vec<ExprId>> {
    fn collect(arrays: &impl Arrays, pattern: ExprId, leaves: &mut Vec<ExprId>) -> Option<()> {
        let kinds = arrays.kind_array();
        let ExpressionKind::Call { callee, arguments } = kinds[pattern.index()] else {
            return None;
        };
        let ExpressionKind::NameRef(name) = kinds[callee.index()] else {
            return None;
        };
        if name != "c" && name != "list" {
            return None;
        }
        for argument in &arrays.argument_array()[arguments.range()] {
            let value = argument.value?;
            match kinds[value.index()] {
                ExpressionKind::NameRef(_) => leaves.push(value),
                ExpressionKind::Call { .. } => collect(arrays, value, leaves)?,
                _ => return None,
            }
        }
        Some(())
    }
    let mut leaves = Vec::new();
    collect(arrays, pattern, &mut leaves)?;
    Some(leaves)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignSpelling {
    /// `<-` and `->`: right assignment is normalized into left form.
    Local,
    Equals,
    Super,
    /// zeallot's `c(a, b) %<-% value` (and `value %->% c(a, b)`): binds every name in the
    /// `c()` or `list()` pattern, so the target is the pattern call, not a single name.
    Destructure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOperator {
    Minus,
    Plus,
    Not,
    Tilde,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Modulo,
    IntegerDivide,
    /// Any `%…%` other than `%%` and `%/%`, including user-defined ones.
    Special,
    Sequence,
    Less,
    Greater,
    LessEq,
    GreaterEq,
    Equal,
    NotEqual,
    And,
    And2,
    Or,
    Or2,
    Pipe,
    Tilde,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralKind {
    /// Source text is kept for literal index positions and the literal-as-integer courtesy.
    Integer(Text),
    Double(Text),
    Complex(Text),
    /// A ry number with a type suffix, as in `5i64`. The text is the number without its
    /// suffix, so an `i128` or `u128` value stays exact.
    Suffixed {
        number: Text,
        ty: NumberType,
    },
    /// The value without its quotes; escapes are not decoded.
    String(Text),
    Logical(bool),
    Null,
    Na(NaAtom),
    Inf,
    NaN,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NaAtom {
    Logical,
    Integer,
    Double,
    Complex,
    Character,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argument {
    pub name: Option<NameId>,
    /// `None` is a positional hole (`f(, x)`) or an empty tagged value.
    pub value: Option<ExprId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub name: Symbol,
    pub default: Option<ExprId>,
    /// The name's range, excluding the default.
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpressionKind {
    /// A broken region: types as Unknown and reports nothing of its own.
    Missing,
    Literal(LiteralKind),
    NameRef(Symbol),
    Assign {
        spelling: AssignSpelling,
        target: ExprId,
        value: ExprId,
    },
    Unary {
        operator: UnaryOperator,
        operand: ExprId,
    },
    Binary {
        operator: BinaryOperator,
        /// Set only for `Special`: R evaluates `a %op% b` as a call to `%op%`,
        /// so naming needs the name to keep a user-defined operator alive.
        special_name: Option<Symbol>,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: ExprId,
        arguments: Span<Argument>,
    },
    Index {
        double: bool,
        target: ExprId,
        arguments: Span<Argument>,
    },
    Field {
        at: bool,
        target: ExprId,
        name: Option<NameId>,
    },
    Namespace {
        internal: bool,
        package: Option<Symbol>,
        name: Option<Symbol>,
    },
    Function {
        parameters: Span<Parameter>,
        body: ExprId,
    },
    If {
        condition: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    For {
        variable: Option<NameId>,
        sequence: ExprId,
        body: ExprId,
    },
    While {
        condition: ExprId,
        body: ExprId,
    },
    Repeat {
        body: ExprId,
    },
    /// Recognized by the bare callee name, like `return` and `stop`; rebinding
    /// `local` is not modeled.
    Local {
        body: ExprId,
    },
    Block {
        statements: Span<ExprId>,
    },
    Paren(ExprId),
    Break,
    Next,
    /// `(a, b)` and `(a,)`: a native tuple.
    Tuple(Span<ExprId>),
    /// `(x = 1, y = 2)`: a native record, fields in source order.
    Record(Span<RecordField>),
    /// `(..base, x = 3)`: a copy of the record `base` with the listed fields replaced.
    RecordUpdate {
        base: ExprId,
        fields: Span<RecordField>,
    },
    /// `[a, b]`: an Array literal.
    Array(Span<ExprId>),
    /// `:Name` or `:Name(...)`. The payload is a `Tuple` or `Record` expression spanning the
    /// parenthesized list, so a payload types exactly like the matching literal.
    Tag {
        name: NameId,
        payload: Option<ExprId>,
    },
    Match {
        scrutinee: ExprId,
        arms: Span<MatchArm>,
    },
    /// `f"..."`: its pieces in source order. A text run is a `String` literal holding the run
    /// as written, so a doubled brace stays doubled; an interpolation is its expression.
    FormatString(Span<ExprId>),
    /// `try expr`: the operand's `Ok` value, or an early return of its `Err`.
    Try(ExprId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordField {
    pub name: Symbol,
    pub name_range: TextRange,
    pub value: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: PatId,
    /// The pattern's `Bind` leaves in source order, so a walk over expressions reaches them
    /// without reading patterns. Lowering fills both at once and neither changes afterwards.
    pub bindings: Span<ExprId>,
    pub guard: Option<ExprId>,
    pub body: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// A broken pattern: matches anything and reports nothing of its own.
    Missing,
    Wildcard,
    /// The name is a `NameRef` expression, a write site exactly like an assignment target.
    Bind(ExprId),
    Literal {
        literal: LiteralKind,
        negative: bool,
    },
    /// The payload is a `Tuple` or `Record` pattern spanning the parenthesized list.
    Tag {
        name: NameId,
        payload: Option<PatId>,
    },
    Tuple(Span<PatId>),
    /// Without `rest`, the pattern must list every field of the record it matches.
    Record {
        fields: Span<PatternField>,
        rest: Option<Rest>,
    },
    /// `[a, b]`, or with a rest, `[first, ..rest, last]`.
    List {
        items: Span<PatId>,
        rest: Option<ListRest>,
    },
    /// `pattern as name`: binds the whole matched value as well as the pattern's names.
    As {
        pattern: PatId,
        binding: ExprId,
    },
    Or(Span<PatId>),
}

/// The `..` of a pattern, which matches what the listed items leave over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rest {
    /// The `NameRef` that `..rest` binds; a bare `..` binds nothing.
    pub binding: Option<ExprId>,
}

/// The `..` of a list pattern: the items before `position` match the list's start, and the
/// rest match its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListRest {
    pub position: u32,
    pub rest: Rest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternField {
    pub name: Symbol,
    pub name_range: TextRange,
    pub pattern: PatId,
}

pub fn lower_item(root: &SyntaxNode) -> Module {
    let mut lowering = LOWERING.take();
    let root_id = lowering.lower_expression(root.node_ref());
    // The scratch arrays keep their capacity for the next item this thread lowers, which saves
    // the reallocation churn of growing fresh vectors per item.
    let module = lowering.parts.seal(root_id);
    lowering.parts.clear();
    LOWERING.set(lowering);
    module
}

thread_local! {
    static LOWERING: Cell<Lowering> = Cell::default();
}

/// Lowering reads borrowed nodes. A list's elements are lowered
/// before the list is complete, and lowering them appends their own lists first, so each list
/// gathers on a stack and moves into its side array once its last element is done. A nested
/// list is always finished and popped before the enclosing one takes its next element, which
/// keeps every list contiguous at the top of its stack.
#[derive(Default)]
struct Lowering {
    parts: Parts,
    expression_stack: Vec<ExprId>,
    argument_stack: Vec<Argument>,
    parameter_stack: Vec<Parameter>,
    field_stack: Vec<RecordField>,
    arm_stack: Vec<MatchArm>,
    pattern_stack: Vec<PatId>,
    pattern_field_stack: Vec<PatternField>,
}

/// Moves `stack[mark..]` to the end of `store`.
fn seal<T>(stack: &mut Vec<T>, mark: usize, store: &mut Vec<T>) -> Span<T> {
    let start = store.len() as u32;
    store.extend(stack.drain(mark..));
    Span {
        start,
        len: store.len() as u32 - start,
        element: PhantomData,
    }
}

impl Lowering {
    fn allocate(&mut self, kind: ExpressionKind, range: TextRange) -> ExprId {
        let id = ExprId::new(self.parts.kinds.len());
        self.parts.kinds.push(kind);
        self.parts.ranges.push(range);
        id
    }

    fn name_site(&mut self, name: Symbol, range: TextRange) -> NameId {
        let id = NameId::new(self.parts.name_sites.len());
        self.parts.name_sites.push(NameSite { name, range });
        id
    }

    fn missing(&mut self, range: TextRange) -> ExprId {
        self.allocate(ExpressionKind::Missing, range)
    }

    fn lower_optional(&mut self, node: Option<NodeRef<'_>>, fallback: TextRange) -> ExprId {
        match node {
            Some(node) => self.lower_expression(node),
            None => self.missing(fallback),
        }
    }

    fn lower_expression(&mut self, node: NodeRef<'_>) -> ExprId {
        let range = significant_range_of(node);
        match node.kind() {
            SyntaxKind::NAME => {
                let name = name_symbol(node).unwrap_or_default();
                self.allocate(ExpressionKind::NameRef(name), range)
            }
            SyntaxKind::LITERAL => {
                let kind = literal_kind(node, &mut self.parts.text);
                self.allocate(ExpressionKind::Literal(kind), range)
            }
            // `@Name(value)` is its value in parentheses once the nominal head is erased;
            // the head is an annotation the checker applies to this expression.
            SyntaxKind::PAREN_EXPR | SyntaxKind::NEW_EXPR => {
                let inner = expression_children(node).next();
                let inner = self.lower_optional(inner, range);
                self.allocate(ExpressionKind::Paren(inner), range)
            }
            SyntaxKind::BRACE_EXPR => {
                let mark = self.expression_stack.len();
                for child in expression_children(node) {
                    let statement = self.lower_expression(child);
                    self.expression_stack.push(statement);
                }
                let statements = seal(
                    &mut self.expression_stack,
                    mark,
                    &mut self.parts.expression_lists,
                );
                // A trailing `;` adds no expression in R's grammar, so
                // `{ 1L; }` is `1L` and needs no flag here.
                self.allocate(ExpressionKind::Block { statements }, range)
            }
            SyntaxKind::UNARY_EXPR => {
                let operator = node.child_tokens().find_map(|token| match token.kind() {
                    SyntaxKind::MINUS => Some(UnaryOperator::Minus),
                    SyntaxKind::PLUS => Some(UnaryOperator::Plus),
                    SyntaxKind::BANG => Some(UnaryOperator::Not),
                    SyntaxKind::TILDE => Some(UnaryOperator::Tilde),
                    SyntaxKind::QUESTION => Some(UnaryOperator::Help),
                    _ => None,
                });
                let operand = expression_children(node).next();
                let operand = self.lower_optional(operand, range);
                match operator {
                    Some(operator) => {
                        self.allocate(ExpressionKind::Unary { operator, operand }, range)
                    }
                    None => self.missing(range),
                }
            }
            SyntaxKind::BINARY_EXPR => self.lower_binary(node, range),
            SyntaxKind::CALL_EXPR => {
                let callee_node = expression_children(node).next();
                let is_local = callee_node.is_some_and(|callee| {
                    callee.kind() == SyntaxKind::NAME
                        && name_symbol(callee).is_some_and(|name| name == "local")
                });
                let mark = self.argument_stack.len();
                self.lower_arguments(node, None);
                if is_local
                    && let [
                        Argument {
                            name: None,
                            value: Some(body),
                            ..
                        },
                    ] = self.argument_stack[mark..]
                {
                    self.argument_stack.truncate(mark);
                    return self.allocate(ExpressionKind::Local { body }, range);
                }
                let arguments = seal(&mut self.argument_stack, mark, &mut self.parts.arguments);
                let callee = self.lower_optional(callee_node, range);
                // R evaluates a string call head by looking up the function of that name, so
                // `"%in%.integer64"(x, y)` calls exactly what the bare name would.
                if let Some(callee_kind) = self.parts.kinds.get_mut(callee.index())
                    && let ExpressionKind::Literal(LiteralKind::String(name)) = *callee_kind
                {
                    *callee_kind =
                        ExpressionKind::NameRef(Symbol::new(&self.parts.text[name.range()]));
                }
                self.allocate(ExpressionKind::Call { callee, arguments }, range)
            }
            SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR => {
                let double = node.kind() == SyntaxKind::SUBSET2_EXPR;
                let target = expression_children(node).next();
                let target = self.lower_optional(target, range);
                let mark = self.argument_stack.len();
                self.lower_arguments(node, None);
                let arguments = seal(&mut self.argument_stack, mark, &mut self.parts.arguments);
                self.allocate(
                    ExpressionKind::Index {
                        double,
                        target,
                        arguments,
                    },
                    range,
                )
            }
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => {
                let at = node.kind() == SyntaxKind::AT_EXPR;
                let mut children = expression_children(node);
                let target = children.next();
                let target = self.lower_optional(target, range);
                let name = children.next().and_then(|name_node| {
                    let name = field_name_text(name_node)?;
                    Some(self.name_site(name, name_node.range()))
                });
                self.allocate(ExpressionKind::Field { at, target, name }, range)
            }
            SyntaxKind::NAMESPACE_EXPR => {
                let internal = node
                    .child_tokens()
                    .any(|token| token.kind() == SyntaxKind::COLON3);
                let mut children = expression_children(node);
                let package = children.next().and_then(field_name_text);
                let name = children.next().and_then(field_name_text);
                self.allocate(
                    ExpressionKind::Namespace {
                        internal,
                        package,
                        name,
                    },
                    range,
                )
            }
            SyntaxKind::FUNCTION_DEF => {
                let mark = self.parameter_stack.len();
                if let Some(list) = child_of_kind(node, SyntaxKind::PARAMETER_LIST) {
                    self.lower_parameters(list);
                }
                let parameters = seal(&mut self.parameter_stack, mark, &mut self.parts.parameters);
                let body = expression_children(node).next();
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::Function { parameters, body }, range)
            }
            SyntaxKind::IF_EXPR => {
                let mut children = expression_children(node);
                let condition = children.next();
                let then_branch = children.next();
                let else_branch = children.next();
                let condition = self.lower_optional(condition, range);
                let then_branch = self.lower_optional(then_branch, range);
                let else_branch = else_branch.map(|node| self.lower_expression(node));
                self.allocate(
                    ExpressionKind::If {
                        condition,
                        then_branch,
                        else_branch,
                    },
                    range,
                )
            }
            SyntaxKind::FOR_EXPR => {
                let mut children = expression_children(node);
                let variable = match children.next() {
                    Some(variable) if variable.kind() == SyntaxKind::NAME => {
                        name_symbol(variable).map(|name| self.name_site(name, variable.range()))
                    }
                    _ => None,
                };
                let sequence = children.next();
                let body = children.next();
                let sequence = self.lower_optional(sequence, range);
                let body = self.lower_optional(body, range);
                self.allocate(
                    ExpressionKind::For {
                        variable,
                        sequence,
                        body,
                    },
                    range,
                )
            }
            SyntaxKind::WHILE_EXPR => {
                let mut children = expression_children(node);
                let condition = children.next();
                let body = children.next();
                let condition = self.lower_optional(condition, range);
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::While { condition, body }, range)
            }
            SyntaxKind::REPEAT_EXPR => {
                let body = expression_children(node).next();
                let body = self.lower_optional(body, range);
                self.allocate(ExpressionKind::Repeat { body }, range)
            }
            SyntaxKind::BREAK_EXPR => self.allocate(ExpressionKind::Break, range),
            SyntaxKind::NEXT_EXPR => self.allocate(ExpressionKind::Next, range),
            SyntaxKind::TUPLE_EXPR | SyntaxKind::RECORD_EXPR => {
                match child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                    Some(list) => self.lower_items(list, range),
                    None => self.missing(range),
                }
            }
            SyntaxKind::ARRAY_EXPR => {
                let items = match child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
                    Some(list) => {
                        let mark = self.argument_stack.len();
                        self.lower_arguments_of(list, None);
                        let item_mark = self.expression_stack.len();
                        for argument in self.argument_stack.drain(mark..) {
                            self.expression_stack.extend(argument.value);
                        }
                        seal(
                            &mut self.expression_stack,
                            item_mark,
                            &mut self.parts.expression_lists,
                        )
                    }
                    None => Span::default(),
                };
                self.allocate(ExpressionKind::Array(items), range)
            }
            SyntaxKind::TAG_EXPR => {
                let Some(name_node) = child_of_kind(node, SyntaxKind::NAME) else {
                    return self.missing(range);
                };
                let name = field_name_text(name_node).unwrap_or_default();
                let name = self.name_site(name, name_node.range());
                let payload = child_of_kind(node, SyntaxKind::ARGUMENT_LIST)
                    .map(|list| self.lower_items(list, list.range()));
                self.allocate(ExpressionKind::Tag { name, payload }, range)
            }
            SyntaxKind::FSTRING => {
                let mark = self.expression_stack.len();
                for child in node.children() {
                    let piece = match child {
                        ElementRef::Token(token) if token.kind() == SyntaxKind::FSTRING_TEXT => {
                            let start = self.parts.text.len() as u32;
                            self.parts.text.push_str(token.text());
                            let text = Text {
                                start,
                                len: token.text().len() as u32,
                            };
                            self.allocate(
                                ExpressionKind::Literal(LiteralKind::String(text)),
                                token.range(),
                            )
                        }
                        ElementRef::Node(interpolation)
                            if interpolation.kind() == SyntaxKind::FSTRING_INTERPOLATION =>
                        {
                            let inner = expression_children(interpolation).next();
                            self.lower_optional(inner, significant_range_of(interpolation))
                        }
                        _ => continue,
                    };
                    self.expression_stack.push(piece);
                }
                let pieces = seal(
                    &mut self.expression_stack,
                    mark,
                    &mut self.parts.expression_lists,
                );
                self.allocate(ExpressionKind::FormatString(pieces), range)
            }
            SyntaxKind::TRY_EXPR => {
                let operand = expression_children(node).next();
                let operand = self.lower_optional(operand, range);
                self.allocate(ExpressionKind::Try(operand), range)
            }
            SyntaxKind::MATCH_EXPR => {
                let scrutinee = expression_children(node).next();
                let scrutinee = self.lower_optional(scrutinee, range);
                let mark = self.arm_stack.len();
                for arm in node
                    .child_nodes()
                    .filter(|child| child.kind() == SyntaxKind::MATCH_ARM)
                {
                    let arm = self.lower_arm(arm);
                    self.arm_stack.push(arm);
                }
                let arms = seal(&mut self.arm_stack, mark, &mut self.parts.arms);
                self.allocate(ExpressionKind::Match { scrutinee, arms }, range)
            }
            _ => self.missing(range),
        }
    }

    /// The items of a native tuple or record literal. The parser already rejects a list that
    /// mixes named and unnamed items, so the first item decides which one this is.
    /// A leading `..base` makes the list a record update, whose other items the parser
    /// requires to be named.
    fn lower_items(&mut self, list: NodeRef<'_>, range: TextRange) -> ExprId {
        let base = child_of_kind(list, SyntaxKind::RECORD_SPREAD).map(|spread| {
            let spread_range = significant_range_of(spread);
            let name = spread
                .first_token()
                .map(|token| Symbol::new(token.text().trim_start_matches('.')))
                .unwrap_or_default();
            let name_range = TextRange::new(
                (spread_range.start() + TextSize::from(2)).min(spread_range.end()),
                spread_range.end(),
            );
            self.allocate(ExpressionKind::NameRef(name), name_range)
        });
        let mark = self.argument_stack.len();
        self.lower_arguments_of(list, None);
        let is_record = base.is_some()
            || self
                .argument_stack
                .get(mark)
                .is_some_and(|argument| argument.name.is_some());
        if is_record {
            let field_mark = self.field_stack.len();
            for index in mark..self.argument_stack.len() {
                let argument = self.argument_stack[index];
                let Some(name) = argument.name else {
                    continue;
                };
                let NameSite {
                    name,
                    range: name_range,
                } = self.parts.name_sites[name.index()];
                let value = match argument.value {
                    Some(value) => value,
                    None => self.missing(name_range),
                };
                self.field_stack.push(RecordField {
                    name,
                    name_range,
                    value,
                });
            }
            self.argument_stack.truncate(mark);
            let fields = seal(
                &mut self.field_stack,
                field_mark,
                &mut self.parts.record_fields,
            );
            let kind = match base {
                Some(base) => ExpressionKind::RecordUpdate { base, fields },
                None => ExpressionKind::Record(fields),
            };
            return self.allocate(kind, range);
        }
        let item_mark = self.expression_stack.len();
        for argument in self.argument_stack.drain(mark..) {
            self.expression_stack.extend(argument.value);
        }
        let items = seal(
            &mut self.expression_stack,
            item_mark,
            &mut self.parts.expression_lists,
        );
        self.allocate(ExpressionKind::Tuple(items), range)
    }

    fn lower_arm(&mut self, arm: NodeRef<'_>) -> MatchArm {
        let range = significant_range_of(arm);
        let pattern = match arm
            .child_nodes()
            .find(|child| is_pattern_kind(child.kind()))
        {
            Some(node) => self.lower_pattern(node),
            None => self.allocate_pattern(PatternKind::Missing, range),
        };
        let bindings = pattern_bindings(&self.parts, pattern);
        let start = self.parts.expression_lists.len() as u32;
        self.parts.expression_lists.extend(bindings);
        let bindings = Span {
            start,
            len: self.parts.expression_lists.len() as u32 - start,
            element: PhantomData,
        };
        let guard = child_of_kind(arm, SyntaxKind::MATCH_GUARD).map(|guard| {
            let condition = expression_children(guard).next();
            self.lower_optional(condition, significant_range_of(guard))
        });
        let body = expression_children(arm).next();
        let body = self.lower_optional(body, range);
        MatchArm {
            pattern,
            bindings,
            guard,
            body,
        }
    }

    fn allocate_pattern(&mut self, kind: PatternKind, range: TextRange) -> PatId {
        let id = PatId::new(self.parts.patterns.len());
        self.parts.patterns.push(Pattern { kind, range });
        id
    }

    fn lower_pattern(&mut self, node: NodeRef<'_>) -> PatId {
        let range = significant_range_of(node);
        let kind = match node.kind() {
            SyntaxKind::PATTERN_WILDCARD => PatternKind::Wildcard,
            SyntaxKind::PATTERN_BIND => {
                let name = child_of_kind(node, SyntaxKind::NAME)
                    .and_then(field_name_text)
                    .unwrap_or_default();
                PatternKind::Bind(self.allocate(ExpressionKind::NameRef(name), range))
            }
            SyntaxKind::PATTERN_LITERAL => {
                let negative = node
                    .child_tokens()
                    .any(|token| token.kind() == SyntaxKind::MINUS);
                match child_of_kind(node, SyntaxKind::LITERAL) {
                    Some(literal) => PatternKind::Literal {
                        literal: literal_kind(literal, &mut self.parts.text),
                        negative,
                    },
                    None => PatternKind::Missing,
                }
            }
            SyntaxKind::PATTERN_TAG => match child_of_kind(node, SyntaxKind::NAME) {
                Some(name_node) => {
                    let payload = child_of_kind(node, SyntaxKind::PATTERN_LIST)
                        .map(|list| self.lower_pattern(list));
                    let name = field_name_text(name_node).unwrap_or_default();
                    PatternKind::Tag {
                        name: self.name_site(name, name_node.range()),
                        payload,
                    }
                }
                None => PatternKind::Missing,
            },
            SyntaxKind::PATTERN_LIST => {
                let is_record = node.child_nodes().any(|child| {
                    matches!(
                        child.kind(),
                        SyntaxKind::PATTERN_FIELD | SyntaxKind::PATTERN_REST
                    )
                });
                if is_record {
                    let rest = child_of_kind(node, SyntaxKind::PATTERN_REST)
                        .map(|rest| self.lower_rest(rest));
                    let mark = self.pattern_field_stack.len();
                    for field in node
                        .child_nodes()
                        .filter(|child| child.kind() == SyntaxKind::PATTERN_FIELD)
                    {
                        let Some(name_node) = child_of_kind(field, SyntaxKind::NAME) else {
                            continue;
                        };
                        let Some(name) = field_name_text(name_node) else {
                            continue;
                        };
                        // A punned field, `(x =)`, binds the field's own name.
                        let pattern = match field
                            .child_nodes()
                            .find(|child| is_pattern_kind(child.kind()))
                        {
                            Some(pattern) => self.lower_pattern(pattern),
                            None if field
                                .child_tokens()
                                .any(|token| token.kind() == SyntaxKind::EQ)
                                && name_node.kind() == SyntaxKind::NAME =>
                            {
                                let name_range = name_node.range();
                                let binding =
                                    self.allocate(ExpressionKind::NameRef(name), name_range);
                                self.allocate_pattern(PatternKind::Bind(binding), name_range)
                            }
                            None => self.allocate_pattern(
                                PatternKind::Missing,
                                significant_range_of(field),
                            ),
                        };
                        self.pattern_field_stack.push(PatternField {
                            name,
                            name_range: name_node.range(),
                            pattern,
                        });
                    }
                    let fields = seal(
                        &mut self.pattern_field_stack,
                        mark,
                        &mut self.parts.pattern_fields,
                    );
                    PatternKind::Record { fields, rest }
                } else {
                    PatternKind::Tuple(self.lower_pattern_list(node))
                }
            }
            SyntaxKind::PATTERN_ARRAY => {
                let mark = self.pattern_stack.len();
                let mut rest = None;
                for item in node.child_nodes() {
                    if item.kind() == SyntaxKind::PATTERN_REST {
                        // The parser reports a second `..`; the first one stands.
                        if rest.is_none() {
                            rest = Some(ListRest {
                                position: (self.pattern_stack.len() - mark) as u32,
                                rest: self.lower_rest(item),
                            });
                        }
                    } else if is_pattern_kind(item.kind()) {
                        let item = self.lower_pattern(item);
                        self.pattern_stack.push(item);
                    }
                }
                let items = seal(&mut self.pattern_stack, mark, &mut self.parts.pattern_lists);
                PatternKind::List { items, rest }
            }
            SyntaxKind::PATTERN_AS => {
                let pattern = match node
                    .child_nodes()
                    .find(|child| is_pattern_kind(child.kind()))
                {
                    Some(inner) => self.lower_pattern(inner),
                    None => self.allocate_pattern(PatternKind::Missing, range),
                };
                match child_of_kind(node, SyntaxKind::NAME) {
                    Some(name_node) => {
                        let name = name_symbol(name_node).unwrap_or_default();
                        let binding = self.allocate(
                            ExpressionKind::NameRef(name),
                            significant_range_of(name_node),
                        );
                        PatternKind::As { pattern, binding }
                    }
                    None => return pattern,
                }
            }
            SyntaxKind::PATTERN_OR => PatternKind::Or(self.lower_pattern_list(node)),
            SyntaxKind::PATTERN_PAREN => {
                return match node
                    .child_nodes()
                    .find(|child| is_pattern_kind(child.kind()))
                {
                    Some(inner) => self.lower_pattern(inner),
                    None => self.allocate_pattern(PatternKind::Missing, range),
                };
            }
            _ => PatternKind::Missing,
        };
        self.allocate_pattern(kind, range)
    }

    fn lower_rest(&mut self, rest: NodeRef<'_>) -> Rest {
        let range = significant_range_of(rest);
        let binding = rest
            .first_token()
            .map(|token| token.text().trim_start_matches('.'))
            .filter(|name| !name.is_empty())
            .map(|name| {
                let name_range = TextRange::new(
                    (range.start() + TextSize::from(2)).min(range.end()),
                    range.end(),
                );
                self.allocate(ExpressionKind::NameRef(Symbol::new(name)), name_range)
            });
        Rest { binding }
    }

    fn lower_pattern_list(&mut self, node: NodeRef<'_>) -> Span<PatId> {
        let mark = self.pattern_stack.len();
        for item in node
            .child_nodes()
            .filter(|child| is_pattern_kind(child.kind()))
        {
            let item = self.lower_pattern(item);
            self.pattern_stack.push(item);
        }
        seal(&mut self.pattern_stack, mark, &mut self.parts.pattern_lists)
    }

    fn lower_binary(&mut self, node: NodeRef<'_>, range: TextRange) -> ExprId {
        let Some(operator_token) = node.child_tokens().find(|token| !token.kind().is_trivia())
        else {
            return self.missing(range);
        };
        let mut operands = expression_children(node);
        let (lhs, rhs) = (operands.next(), operands.next());
        let operator = match operator_token.kind() {
            kind @ (SyntaxKind::LESS_MINUS
            | SyntaxKind::EQ
            | SyntaxKind::LESS2_MINUS
            | SyntaxKind::MINUS_GREATER
            | SyntaxKind::MINUS_GREATER2) => {
                let spelling = match kind {
                    SyntaxKind::LESS_MINUS | SyntaxKind::MINUS_GREATER => AssignSpelling::Local,
                    SyntaxKind::EQ => AssignSpelling::Equals,
                    _ => AssignSpelling::Super,
                };
                let lhs = self.lower_optional(lhs, range);
                let rhs = self.lower_optional(rhs, range);
                let (target, value) =
                    if matches!(kind, SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2) {
                        (rhs, lhs)
                    } else {
                        (lhs, rhs)
                    };
                return self.allocate(
                    ExpressionKind::Assign {
                        spelling,
                        target,
                        value,
                    },
                    range,
                );
            }
            // `:=` binds nothing in R: it is a call to a function named `:=` (data.table,
            // rlang). The callee sits on the operator token so an unresolved `:=` reports there.
            SyntaxKind::COLON_EQ => {
                let callee = self.allocate(
                    ExpressionKind::NameRef(Symbol::new(":=")),
                    operator_token.range(),
                );
                let mark = self.argument_stack.len();
                for operand in [lhs, rhs] {
                    let value = self.lower_optional(operand, range);
                    self.argument_stack.push(Argument {
                        name: None,
                        value: Some(value),
                    });
                }
                let arguments = seal(&mut self.argument_stack, mark, &mut self.parts.arguments);
                return self.allocate(ExpressionKind::Call { callee, arguments }, range);
            }
            SyntaxKind::PIPE_GREATER => {
                if let Some(call) = self.lower_pipe(lhs, rhs, range) {
                    return call;
                }
                BinaryOperator::Pipe
            }
            SyntaxKind::SPECIAL => match operator_token.text() {
                "%%" => BinaryOperator::Modulo,
                "%/%" => BinaryOperator::IntegerDivide,
                _ => BinaryOperator::Special,
            },
            SyntaxKind::PLUS => BinaryOperator::Add,
            SyntaxKind::MINUS => BinaryOperator::Subtract,
            SyntaxKind::STAR => BinaryOperator::Multiply,
            SyntaxKind::SLASH => BinaryOperator::Divide,
            SyntaxKind::CARET => BinaryOperator::Power,
            SyntaxKind::COLON => BinaryOperator::Sequence,
            SyntaxKind::LESS => BinaryOperator::Less,
            SyntaxKind::GREATER => BinaryOperator::Greater,
            SyntaxKind::LESS_EQ => BinaryOperator::LessEq,
            SyntaxKind::GREATER_EQ => BinaryOperator::GreaterEq,
            SyntaxKind::EQ2 => BinaryOperator::Equal,
            SyntaxKind::BANG_EQ => BinaryOperator::NotEqual,
            SyntaxKind::AMP => BinaryOperator::And,
            SyntaxKind::AMP2 => BinaryOperator::And2,
            SyntaxKind::PIPE => BinaryOperator::Or,
            SyntaxKind::PIPE2 => BinaryOperator::Or2,
            SyntaxKind::TILDE => BinaryOperator::Tilde,
            SyntaxKind::QUESTION => BinaryOperator::Help,
            _ => return self.missing(range),
        };
        let lhs = self.lower_optional(lhs, range);
        let rhs = self.lower_optional(rhs, range);
        if operator == BinaryOperator::Special {
            let (pattern, value) = match operator_token.text() {
                "%<-%" => (lhs, rhs),
                "%->%" => (rhs, lhs),
                _ => (lhs, lhs),
            };
            if pattern != value && leaves_of(&self.parts, pattern).is_some() {
                return self.allocate(
                    ExpressionKind::Assign {
                        spelling: AssignSpelling::Destructure,
                        target: pattern,
                        value,
                    },
                    range,
                );
            }
        }
        let special_name =
            (operator == BinaryOperator::Special).then(|| Symbol::new(operator_token.text()));
        self.allocate(
            ExpressionKind::Binary {
                operator,
                special_name,
                lhs,
                rhs,
            },
            range,
        )
    }

    /// R's parser rewrites `x |> f(y)` to `f(x, y)`, so the pipe lowers as that call.
    /// Shapes R rejects return `None` and keep the opaque-operator lowering.
    fn lower_pipe(
        &mut self,
        lhs: Option<NodeRef<'_>>,
        rhs: Option<NodeRef<'_>>,
        range: TextRange,
    ) -> Option<ExprId> {
        let call = rhs.filter(|node| node.kind() == SyntaxKind::CALL_EXPR)?;
        let shape = pipe_shape(call)?;
        let piped = self.lower_optional(lhs, range);
        let callee = call
            .child_nodes()
            .find(|child| child.kind() != SyntaxKind::ARGUMENT_LIST);
        let callee = self.lower_optional(callee, range);
        let mark = self.argument_stack.len();
        let substitute = match shape {
            PipeShape::FirstArgument => {
                self.argument_stack.push(Argument {
                    name: None,
                    value: Some(piped),
                });
                None
            }
            PipeShape::NamedPlaceholder(index) => Some((index, piped)),
        };
        self.lower_arguments(call, substitute);
        let arguments = seal(&mut self.argument_stack, mark, &mut self.parts.arguments);
        Some(self.allocate(ExpressionKind::Call { callee, arguments }, range))
    }

    /// Pushes the arguments of `node`'s argument list onto the argument stack. The `_`
    /// placeholder never becomes an expression, so nothing dangles in the module.
    fn lower_arguments(&mut self, node: NodeRef<'_>, substitute: Option<(usize, ExprId)>) {
        if let Some(list) = child_of_kind(node, SyntaxKind::ARGUMENT_LIST) {
            self.lower_arguments_of(list, substitute);
        }
    }

    fn lower_arguments_of(&mut self, list: NodeRef<'_>, substitute: Option<(usize, ExprId)>) {
        for (index, argument) in list
            .child_nodes()
            .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
            .enumerate()
        {
            let tag = argument
                .child_nodes()
                .find(|child| {
                    child.kind() == SyntaxKind::NAME || child.kind() == SyntaxKind::LITERAL
                })
                .filter(|_| {
                    argument
                        .child_tokens()
                        .any(|token| token.kind() == SyntaxKind::EQ)
                });
            // R's grammar also takes `NULL` as an argument tag: `switch(x, NULL = 1)`
            // labels the case for the string "NULL".
            let name = tag.and_then(|tag| {
                let null_tag = tag.kind() == SyntaxKind::LITERAL
                    && tag
                        .first_token()
                        .is_some_and(|token| token.kind() == SyntaxKind::NULL_KW);
                let name = if null_tag {
                    Symbol::new("NULL")
                } else {
                    field_name_text(tag)?
                };
                Some(self.name_site(name, tag.range()))
            });
            let value = match substitute {
                Some((placeholder, piped)) if placeholder == index => Some(piped),
                _ => {
                    let mut expressions = expression_children(argument);
                    if name.is_some() {
                        // The tag itself is the first expression child.
                        expressions.next();
                    }
                    expressions.next().map(|node| self.lower_expression(node))
                }
            };
            self.argument_stack.push(Argument { name, value });
        }
    }

    fn lower_parameters(&mut self, list: NodeRef<'_>) {
        for parameter in list
            .child_nodes()
            .filter(|child| child.kind() == SyntaxKind::PARAMETER)
        {
            let first = parameter.first_token();
            let name = first
                .map(|token| {
                    let text = token.text();
                    Symbol::new(
                        text.strip_prefix('`')
                            .and_then(|t| t.strip_suffix('`'))
                            .unwrap_or(text),
                    )
                })
                .unwrap_or_default();
            let default = expression_children(parameter)
                .next()
                .map(|node| self.lower_expression(node));
            let range = first.map_or_else(|| parameter.range(), |token| token.range());
            self.parameter_stack.push(Parameter {
                name,
                default,
                range,
            });
        }
    }
}

fn is_pattern_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::PATTERN_WILDCARD
            | SyntaxKind::PATTERN_BIND
            | SyntaxKind::PATTERN_LITERAL
            | SyntaxKind::PATTERN_TAG
            | SyntaxKind::PATTERN_LIST
            | SyntaxKind::PATTERN_OR
            | SyntaxKind::PATTERN_PAREN
            | SyntaxKind::PATTERN_ARRAY
            | SyntaxKind::PATTERN_AS
    )
}

fn expression_children<'a>(node: NodeRef<'a>) -> impl Iterator<Item = NodeRef<'a>> {
    node.child_nodes()
        .filter(|child| is_expression_kind(child.kind()))
}

fn child_of_kind(node: NodeRef<'_>, kind: SyntaxKind) -> Option<NodeRef<'_>> {
    node.child_nodes().find(|child| child.kind() == kind)
}

/// Excludes the trailing trivia (including `#:` annotations) the parser attaches
/// inside expression nodes, so ranges cover code only.
pub fn significant_range(node: &SyntaxNode) -> TextRange {
    significant_range_of(node.node_ref())
}

fn significant_range_of(node: NodeRef<'_>) -> TextRange {
    let start = node.range().start();
    TextRange::new(start, significant_end(node).unwrap_or(start))
}

/// Where the last token that is neither trivia nor inside an `ANNOTATION` ends.
fn significant_end(node: NodeRef<'_>) -> Option<TextSize> {
    // Scanned back from the last element in preorder: the first token that qualifies is the
    // answer, and only it pays for the walk up to `node`.
    let in_annotation = |token: TokenRef<'_>| {
        let mut current = token.parent();
        while let Some(parent) = current
            && parent != node
        {
            if parent.kind() == SyntaxKind::ANNOTATION {
                return true;
            }
            current = parent.parent();
        }
        false
    };
    node.descendants_with_tokens()
        .rev()
        .find_map(|element| match element {
            ElementRef::Token(token) if !token.kind().is_trivia() && !in_annotation(token) => {
                Some(token.range().end())
            }
            _ => None,
        })
}

/// The name a `NAME` node spells, read from its first token.
fn name_symbol(node: NodeRef<'_>) -> Option<Symbol> {
    Some(Symbol::new(&name_text(node.first_token()?.text())))
}

fn literal_kind(node: NodeRef<'_>, buffer: &mut String) -> LiteralKind {
    let mut text = |value: &str| {
        let start = buffer.len() as u32;
        buffer.push_str(value);
        Text {
            start,
            len: value.len() as u32,
        }
    };
    let Some(token) = node.first_token() else {
        return LiteralKind::Null;
    };
    match token.kind() {
        SyntaxKind::INTEGER => LiteralKind::Integer(text(token.text())),
        SyntaxKind::DOUBLE => LiteralKind::Double(text(token.text())),
        SyntaxKind::COMPLEX => LiteralKind::Complex(text(token.text())),
        SyntaxKind::SUFFIXED_NUMBER => match split_number_suffix(token.text()) {
            Some((number, ty)) => LiteralKind::Suffixed {
                number: text(number),
                ty,
            },
            None => LiteralKind::Double(text(token.text())),
        },
        SyntaxKind::STRING | SyntaxKind::RAW_STRING => {
            LiteralKind::String(text(string_value(token.text())))
        }
        SyntaxKind::TRUE_KW => LiteralKind::Logical(true),
        SyntaxKind::FALSE_KW => LiteralKind::Logical(false),
        SyntaxKind::NULL_KW => LiteralKind::Null,
        SyntaxKind::INF_KW => LiteralKind::Inf,
        SyntaxKind::NAN_KW => LiteralKind::NaN,
        SyntaxKind::NA_INTEGER_KW => LiteralKind::Na(NaAtom::Integer),
        SyntaxKind::NA_REAL_KW => LiteralKind::Na(NaAtom::Double),
        SyntaxKind::NA_COMPLEX_KW => LiteralKind::Na(NaAtom::Complex),
        SyntaxKind::NA_CHARACTER_KW => LiteralKind::Na(NaAtom::Character),
        _ => LiteralKind::Na(NaAtom::Logical),
    }
}

/// A double literal R would treat like an integer: whole, and inside R's integer range, so
/// `1e10` stays a double (`1e10:(1e10 + 5)` is a double vector in R).
pub fn is_whole_number_double(text: &str) -> bool {
    text.parse::<f64>()
        .is_ok_and(|value| value.fract() == 0.0 && value.abs() <= f64::from(i32::MAX))
}

/// 0-based. Whole-number doubles count: R indexes `x[[2]]` exactly like `x[[2L]]`.
pub fn integer_literal_position(module: &Module, kind: &ExpressionKind) -> Option<usize> {
    let one_based_index = match kind {
        ExpressionKind::Literal(LiteralKind::Integer(text)) => {
            module[text].trim_end_matches('L').parse::<usize>().ok()?
        }
        ExpressionKind::Literal(LiteralKind::Double(text)) => {
            let value = module[text].parse::<f64>().ok()?;
            if value.fract() != 0.0 || value < 1.0 || !value.is_finite() {
                return None;
            }
            value as usize
        }
        _ => return None,
    };
    one_based_index.checked_sub(1)
}

/// Escapes are not decoded; raw strings strip their full delimiters.
fn string_value(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix(['r', 'R']) {
        let Some(quote) = rest.chars().next() else {
            return text;
        };
        let inner = &rest[quote.len_utf8()..];
        let dashes = inner.chars().take_while(|&c| c == '-').count();
        let Some(open) = inner[dashes..].chars().next() else {
            return text;
        };
        let body = &inner[dashes + open.len_utf8()..];
        let close = match open {
            '(' => ')',
            '[' => ']',
            _ => '}',
        };
        // The closer is the bracket, the same dashes, then the quote.
        return body
            .strip_suffix(quote)
            .and_then(|body| body.strip_suffix(&inner[..dashes]))
            .and_then(|body| body.strip_suffix(close))
            .unwrap_or(body);
    }
    text.trim_matches(['"', '\''])
}

fn field_name_text(node: NodeRef<'_>) -> Option<Symbol> {
    match node.kind() {
        SyntaxKind::NAME => name_symbol(node),
        SyntaxKind::LITERAL => {
            let token = node.first_token()?;
            (token.kind() == SyntaxKind::STRING).then(|| Symbol::new(string_value(token.text())))
        }
        _ => None,
    }
}

enum PipeShape {
    FirstArgument,
    NamedPlaceholder(usize),
}

/// R's placeholder rules: `_` may appear exactly once, as the whole value of a
/// named argument of this call. Any other use is an R error (`None`).
fn pipe_shape(call: NodeRef<'_>) -> Option<PipeShape> {
    match count_placeholders(call) {
        0 => return Some(PipeShape::FirstArgument),
        1 => {}
        _ => return None,
    }
    let list = child_of_kind(call, SyntaxKind::ARGUMENT_LIST)?;
    list.child_nodes()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .position(|argument| {
            let named = argument
                .child_tokens()
                .any(|token| token.kind() == SyntaxKind::EQ);
            // With a tag present, the value is the second expression child.
            named
                && expression_children(argument).nth(1).is_some_and(|value| {
                    value.kind() == SyntaxKind::NAME
                        && value
                            .child_tokens()
                            .any(|token| token.kind() == SyntaxKind::UNDERSCORE)
                })
        })
        .map(PipeShape::NamedPlaceholder)
}

fn count_placeholders(node: NodeRef<'_>) -> usize {
    node.children()
        .map(|child| match child {
            ElementRef::Token(token) => usize::from(token.kind() == SyntaxKind::UNDERSCORE),
            ElementRef::Node(child) => count_placeholders(child),
        })
        .sum()
}
