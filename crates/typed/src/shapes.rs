//! The shapes of the records, tuples and tags the typed core holds in boxes, and the unions of
//! shapes a tag type ranges over.
//!
//! A shape is a heap layout (the tag, if any, and the field names, `heap::records::Layout`)
//! together with the item type of every field, in the layout's slot order. Two values of one
//! layout whose fields hold different types have different shapes, so compiled code that knows a
//! box's shape knows where each field sits and what it holds without a test.
//!
//! A tag field is any tag ([`ANY`]) in the shape, whatever union it came from. That keeps a
//! recursive type such as an expression tree finite: a node's shape does not depend on the
//! shapes below it, so the shapes of a whole tree are the few its nodes take.
//!
//! A tag of any shape is matched against the shapes the session has *confirmed*: those some
//! compiled code constructs, or that crossed the boundary from R. Code that relies on that set
//! records the [`epoch`] it saw, and compiles again once a new shape is confirmed.

use {
    crate::{ir::Item, values},
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::cell::RefCell,
};

/// The union of every shape, in a tag type ([`crate::ir::Type::Tag`]).
pub const ANY: u32 = u32::MAX;

/// What compiled code needs to know about one shape.
#[derive(Clone)]
pub struct Shape {
    /// The heap layout values of this shape have in R.
    pub layout: u32,
    /// Each field's item type, in the layout's slot order.
    pub fields: SmallVec<[Item; 4]>,
    /// Each field's first word in the box.
    pub offsets: SmallVec<[u32; 4]>,
    pub words: u32,
    /// The words holding counted pointers, one bit each.
    pub objects: u32,
    /// The first words of strings, one bit each.
    pub strings: u32,
}

/// Interns the shape of `layout` with fields of these items, in slot order. `None` when its
/// fields take more words than a box holds.
pub fn intern(layout: u32, fields: &[Item]) -> Option<u32> {
    let fields: SmallVec<[Item; 4]> = fields.iter().map(|&item| normal(item)).collect();
    SHAPES.with_borrow_mut(|shapes| {
        if let Some(&id) = shapes.ids.get(&(layout, fields.clone())) {
            return Some(id);
        }
        let mut offsets = SmallVec::new();
        let (mut words, mut objects, mut strings) = (0u32, 0u32, 0u32);
        for &item in &fields {
            offsets.push(words);
            match item {
                Item::Str => {
                    strings |= 1 << words.min(31);
                    words += 2;
                }
                Item::Record(_) | Item::Tag(_) => {
                    objects |= 1 << words.min(31);
                    words += 1;
                }
                Item::Int | Item::Dbl | Item::Lgl | Item::I64 => words += 1,
            }
            if words > values::MAX_BOX_WORDS {
                return None;
            }
        }
        let id = shapes.shapes.len() as u32;
        shapes.shapes.push(Shape {
            layout,
            fields: fields.clone(),
            offsets,
            words,
            objects,
            strings,
        });
        shapes.constants.push(0);
        shapes.ids.insert((layout, fields), id);
        Some(id)
    })
}

/// A copy of shape `id`'s description.
pub fn shape(id: u32) -> Shape {
    SHAPES.with_borrow(|shapes| match shapes.shapes.get(id as usize) {
        Some(shape) => shape.clone(),
        None => panic!("shape {id} was never interned"),
    })
}

/// The one box of a shape without fields, which is static, so every value of that shape is the
/// same pointer.
pub fn constant(id: u32) -> u64 {
    SHAPES.with_borrow_mut(|shapes| {
        let Some(slot) = shapes.constants.get_mut(id as usize) else {
            panic!("shape {id} was never interned");
        };
        if *slot == 0 {
            *slot = values::static_box(id) as u64;
        }
        *slot
    })
}

/// The union holding exactly these shapes.
pub fn union_of(members: &[u32]) -> u32 {
    let mut members: SmallVec<[u32; 4]> = SmallVec::from_slice(members);
    members.sort_unstable();
    members.dedup();
    SHAPES.with_borrow_mut(|shapes| {
        if let Some(&id) = shapes.union_ids.get(&members) {
            return id;
        }
        let id = shapes.unions.len() as u32;
        shapes.unions.push(members.clone());
        shapes.union_ids.insert(members, id);
        id
    })
}

/// The shapes of union `id`, or `None` for [`ANY`].
pub fn members(id: u32) -> Option<SmallVec<[u32; 4]>> {
    if id == ANY {
        return None;
    }
    SHAPES.with_borrow(|shapes| match shapes.unions.get(id as usize) {
        Some(members) => Some(members.clone()),
        None => panic!("union {id} was never interned"),
    })
}

/// The union of two unions.
pub fn join(a: u32, b: u32) -> u32 {
    match (members(a), members(b)) {
        (Some(a), Some(b)) => {
            let both: SmallVec<[u32; 8]> = a.iter().chain(&b).copied().collect();
            union_of(&both)
        }
        _ => ANY,
    }
}

/// Whether every shape of union `a` is one of union `b`'s.
pub fn within(a: u32, b: u32) -> bool {
    if b == ANY || a == b {
        return true;
    }
    match (members(a), members(b)) {
        (Some(a), Some(b)) => a.iter().all(|member| b.contains(member)),
        _ => false,
    }
}

/// Records that values of shape `id` exist, so a match over any tag must consider it.
pub fn confirm(id: u32) {
    SHAPES.with_borrow_mut(|shapes| {
        let (word, bit) = (id as usize / 64, 1u64 << (id % 64));
        if shapes.confirmed.len() <= word {
            shapes.confirmed.resize(word + 1, 0);
        }
        if shapes.confirmed[word] & bit == 0 {
            shapes.confirmed[word] |= bit;
            shapes.epoch += 1;
        }
    })
}

/// How many times the set of confirmed shapes has grown.
pub fn epoch() -> u64 {
    SHAPES.with_borrow(|shapes| shapes.epoch)
}

/// The confirmed shapes, in the order they were interned.
pub fn confirmed() -> SmallVec<[u32; 8]> {
    SHAPES.with_borrow(|shapes| {
        let mut found = SmallVec::new();
        for (word, &bits) in shapes.confirmed.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                found.push((word * 64 + bits.trailing_zeros() as usize) as u32);
                bits &= bits - 1;
            }
        }
        found
    })
}

#[derive(Default)]
struct Shapes {
    shapes: Vec<Shape>,
    ids: FxHashMap<(u32, SmallVec<[Item; 4]>), u32>,
    /// Each shape's static box once made, or 0.
    constants: Vec<u64>,
    unions: Vec<SmallVec<[u32; 4]>>,
    union_ids: FxHashMap<SmallVec<[u32; 4]>, u32>,
    confirmed: Vec<u64>,
    epoch: u64,
}

thread_local! {
    static SHAPES: RefCell<Shapes> = RefCell::new(Shapes::default());
}

/// A field's item as a shape records it: any tag, whatever union it came from.
fn normal(item: Item) -> Item {
    match item {
        Item::Tag(_) => Item::Tag(ANY),
        item => item,
    }
}
