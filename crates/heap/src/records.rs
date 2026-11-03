//! The runtime tables behind ry's records, tuples and tags: the tag-name interner and the layout
//! interner. Both only grow, so an id handed out once stays valid for the life of the heap, and
//! two values have the same shape exactly when their layout ids are equal.
//!
//! The standard library's dictionaries and sets are record objects too, with a fixed layout of
//! their own: a dictionary holds its keys, values and hash index in three slots, and a set its
//! elements and index in two. Sharing the record object keeps one boundary check, one tracer and
//! one value-semantics rule for every ry value.
//!
//! Tag and field names are interned symbols, which the heap roots for its whole life, so the
//! tables hold them without tracing.

use {crate::object::Sexp, rustc_hash::FxHashMap};

/// A record, tuple or tag shape.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Layout {
    /// The tag's index in the tag interner, or `None` for a record or tuple.
    pub tag: Option<u32>,
    pub fields: Fields,
}

/// The slots of a layout: positional (a tuple, or a tag with a positional payload), named, or
/// one of the standard library's collections.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Fields {
    Positional(u32),
    /// Field-name symbols, sorted by the bytes of their names.
    Named(Box<[Sexp]>),
    /// Keys (an atomic vector, or `NULL` when empty), values (a list) and the hash index (an
    /// integer vector).
    Dict,
    /// Elements (an atomic vector, or `NULL` when empty) and the hash index.
    Set,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NativeKind {
    Record,
    Tuple,
    Tag,
    Dict,
    Set,
}

impl NativeKind {
    /// The name `typeof()` reports.
    pub fn type_name(self) -> &'static str {
        match self {
            NativeKind::Record => "record",
            NativeKind::Tuple => "tuple",
            NativeKind::Tag => "tag",
            NativeKind::Dict => "dict",
            NativeKind::Set => "set",
        }
    }

    /// The implicit class `class()`, `inherits()` and S3 dispatch see.
    pub fn class_name(self) -> &'static str {
        match self {
            NativeKind::Record => "ry_record",
            NativeKind::Tuple => "ry_tuple",
            NativeKind::Tag => "ry_tag",
            NativeKind::Dict => "ry_dict",
            NativeKind::Set => "ry_set",
        }
    }
}

impl Layout {
    pub fn kind(&self) -> NativeKind {
        match (&self.tag, &self.fields) {
            (_, Fields::Dict) => NativeKind::Dict,
            (_, Fields::Set) => NativeKind::Set,
            (Some(_), _) => NativeKind::Tag,
            (None, Fields::Named(_)) => NativeKind::Record,
            (None, Fields::Positional(_)) => NativeKind::Tuple,
        }
    }

    pub fn slot_count(&self) -> usize {
        match &self.fields {
            Fields::Positional(count) => *count as usize,
            Fields::Named(names) => names.len(),
            Fields::Dict => 3,
            Fields::Set => 2,
        }
    }

    /// The field names, empty for a positional layout.
    pub fn names(&self) -> &[Sexp] {
        match &self.fields {
            Fields::Named(names) => names,
            Fields::Positional(_) | Fields::Dict | Fields::Set => &[],
        }
    }
}

#[derive(Default)]
pub struct Layouts {
    tags: Vec<Sexp>,
    tag_ids: FxHashMap<Sexp, u32>,
    layouts: Vec<Layout>,
    layout_ids: FxHashMap<Layout, u32>,
    /// For every slot of every layout, the [`slot_kind`] bits of each value a record of the
    /// layout has held there. Native code speculates a field's type from them, and checks it.
    slot_kinds: Vec<u8>,
    /// Where each layout's slots start in `slot_kinds`, by layout id.
    kinds_start: Vec<u32>,
}

/// The kinds of value a record slot may hold, as bits of a layout's slot profile.
pub mod slot_kind {
    pub const DOUBLE: u8 = 1 << 0;
    pub const INTEGER: u8 = 1 << 1;
    pub const LOGICAL: u8 = 1 << 2;
    /// A handle to a plain (unclassed, attribute-free) record, tuple or tag.
    pub const RECORD: u8 = 1 << 3;
    /// Any other handle.
    pub const OTHER: u8 = 1 << 4;
}

impl Layouts {
    /// The id of the tag named by `symbol`, interning it on first use.
    pub fn intern_tag(&mut self, symbol: Sexp) -> u32 {
        if let Some(&id) = self.tag_ids.get(&symbol) {
            return id;
        }
        let id = self.tags.len() as u32;
        self.tags.push(symbol);
        self.tag_ids.insert(symbol, id);
        id
    }

    /// The symbol a tag id names.
    pub fn tag_name(&self, id: u32) -> Option<Sexp> {
        self.tags.get(id as usize).copied()
    }

    /// The id of `layout`, interning it on first use. Named fields must already be sorted.
    pub fn intern(&mut self, layout: Layout) -> u32 {
        if let Some(&id) = self.layout_ids.get(&layout) {
            return id;
        }
        let id = self.layouts.len() as u32;
        self.kinds_start.push(self.slot_kinds.len() as u32);
        self.slot_kinds
            .extend(std::iter::repeat_n(0, layout.slot_count()));
        self.layouts.push(layout.clone());
        self.layout_ids.insert(layout, id);
        id
    }

    pub fn get(&self, id: u32) -> Option<&Layout> {
        self.layouts.get(id as usize)
    }

    /// The [`slot_kind`] bits of every value a record of layout `id` has held in slot `index`:
    /// zero for a slot no record has filled yet.
    pub fn slot_kinds(&self, id: u32, index: usize) -> u8 {
        self.kinds_start
            .get(id as usize)
            .and_then(|&start| self.slot_kinds.get(start as usize + index))
            .copied()
            .unwrap_or(0)
    }

    /// Records that a record of layout `id` holds a value of kind `kind` in slot `index`.
    pub fn observe(&mut self, id: u32, index: usize, kind: u8) {
        if let Some(&start) = self.kinds_start.get(id as usize)
            && let Some(bits) = self.slot_kinds.get_mut(start as usize + index)
        {
            *bits |= kind;
        }
    }
}
