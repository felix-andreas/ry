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
        self.layouts.push(layout.clone());
        self.layout_ids.insert(layout, id);
        id
    }

    pub fn get(&self, id: u32) -> Option<&Layout> {
        self.layouts.get(id as usize)
    }
}
