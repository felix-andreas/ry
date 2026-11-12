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
    SHAPES.with_borrow_mut(|shapes| shapes.intern(layout, fields))
}

/// Runs `f` with the shape table borrowed, so that a conversion visiting many values looks up
/// all their shapes under one borrow. `f` must not call the other functions of this module.
pub fn with<T>(f: impl FnOnce(&mut Shapes) -> T) -> T {
    SHAPES.with_borrow_mut(f)
}

/// What [`Shapes::converted`] tells a conversion about the shape of the value it boxes.
pub struct Converted {
    pub id: u32,
    pub words: u32,
    pub objects: u32,
    pub strings: u32,
    /// The shape's static box when it has no words, or 0.
    pub constant: u64,
}

/// The layout of shape `id` and the item of each of its fields, copied out so that the caller
/// can allocate without holding the shape table.
pub fn fields(id: u32) -> (u32, SmallVec<[Item; 4]>) {
    SHAPES.with_borrow(|shapes| {
        let Some(shape) = shapes.shapes.get(id as usize) else {
            panic!("shape {id} was never interned");
        };
        (shape.layout, SmallVec::from_slice(&shape.fields))
    })
}

/// The words a field of `item` takes in a box: two for a string, one otherwise.
pub fn item_words(item: Item) -> u32 {
    if item == Item::Str { 2 } else { 1 }
}

/// The id of a ry list of `item`s, as [`Item::Array`] holds it.
pub fn array_of(item: Item) -> u32 {
    SHAPES.with_borrow_mut(|shapes| shapes.array_of(item))
}

/// The items of the ry lists [`Item::Array`]`(id)` stands for.
pub fn array_item(id: u32) -> Item {
    SHAPES.with_borrow(|shapes| match shapes.arrays.get(id as usize) {
        Some(&item) => item,
        None => panic!("list item type {id} was never interned"),
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
    SHAPES.with_borrow_mut(|shapes| shapes.constant(id))
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
    SHAPES.with_borrow_mut(|shapes| shapes.confirm(id))
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

/// The session's shapes, unions and list item types.
#[derive(Default)]
pub struct Shapes {
    shapes: Vec<Shape>,
    /// The shapes of each layout, indexed by layout id: layout ids are dense and a layout
    /// rarely has more than one shape, so a lookup is one index and a short scan.
    by_layout: Vec<SmallVec<[u32; 2]>>,
    /// Each shape's static box once made, or 0.
    constants: Vec<u64>,
    unions: Vec<SmallVec<[u32; 4]>>,
    union_ids: FxHashMap<SmallVec<[u32; 4]>, u32>,
    confirmed: Vec<u64>,
    epoch: u64,
    /// The item types of the ry lists [`Item::Array`] ids stand for.
    arrays: Vec<Item>,
    array_ids: FxHashMap<Item, u32>,
}

impl Shapes {
    /// The shape a conversion from R gives a value of `layout` with fields of these items,
    /// confirmed when `confirmed`. `None` when the fields take more words than a box holds.
    pub fn converted(
        &mut self,
        layout: u32,
        fields: &[Item],
        confirmed: bool,
    ) -> Option<Converted> {
        let id = self.intern(layout, fields)?;
        if confirmed {
            self.confirm(id);
        }
        let shape = &self.shapes[id as usize];
        let (words, objects, strings) = (shape.words, shape.objects, shape.strings);
        let constant = if words == 0 { self.constant(id) } else { 0 };
        Some(Converted {
            id,
            words,
            objects,
            strings,
            constant,
        })
    }

    /// See [`array_of`].
    pub fn array_of(&mut self, item: Item) -> u32 {
        let item = normal(item);
        if let Some(&id) = self.array_ids.get(&item) {
            return id;
        }
        let id = self.arrays.len() as u32;
        self.arrays.push(item);
        self.array_ids.insert(item, id);
        id
    }

    fn intern(&mut self, layout: u32, fields: &[Item]) -> Option<u32> {
        let known = self.by_layout.get(layout as usize);
        if let Some(&id) = known.into_iter().flatten().find(|&&id| {
            let shape = &self.shapes[id as usize];
            shape.fields.len() == fields.len()
                && shape
                    .fields
                    .iter()
                    .zip(fields)
                    .all(|(&have, &want)| have == normal(want))
        }) {
            return Some(id);
        }
        let fields: SmallVec<[Item; 4]> = fields.iter().map(|&item| normal(item)).collect();
        let mut offsets = SmallVec::new();
        let (mut words, mut objects, mut strings) = (0u32, 0u32, 0u32);
        for &item in &fields {
            offsets.push(words);
            match item {
                Item::Str => strings |= 1 << words.min(31),
                Item::Record(_) | Item::Tag(_) | Item::Array(_) => objects |= 1 << words.min(31),
                Item::Int | Item::Dbl | Item::Lgl | Item::I64 => {}
            }
            words += item_words(item);
            if words > values::MAX_BOX_WORDS {
                return None;
            }
        }
        let id = self.shapes.len() as u32;
        self.shapes.push(Shape {
            layout,
            fields,
            offsets,
            words,
            objects,
            strings,
        });
        self.constants.push(0);
        if self.by_layout.len() <= layout as usize {
            self.by_layout
                .resize_with(layout as usize + 1, SmallVec::new);
        }
        self.by_layout[layout as usize].push(id);
        Some(id)
    }

    fn confirm(&mut self, id: u32) {
        let (word, bit) = (id as usize / 64, 1u64 << (id % 64));
        if self.confirmed.len() <= word {
            self.confirmed.resize(word + 1, 0);
        }
        if self.confirmed[word] & bit == 0 {
            self.confirmed[word] |= bit;
            self.epoch += 1;
        }
    }

    fn constant(&mut self, id: u32) -> u64 {
        let Some(slot) = self.constants.get_mut(id as usize) else {
            panic!("shape {id} was never interned");
        };
        if *slot == 0 {
            *slot = values::static_box(id) as u64;
        }
        *slot
    }
}

thread_local! {
    static SHAPES: RefCell<Shapes> = RefCell::new(Shapes::default());
}

/// A field's item as a shape records it: any tag, whatever union it came from, also as the item
/// of a list. Items of an interned list are already normal.
fn normal(item: Item) -> Item {
    match item {
        Item::Tag(_) => Item::Tag(ANY),
        item => item,
    }
}
