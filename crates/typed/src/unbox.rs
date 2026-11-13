//! Replaces a record or tag that never leaves its function with the values of its fields.
//! `invoice <- price_line(order)` followed by `invoice$gross`, with `price_line` inlined, would
//! otherwise allocate two boxes per call only to read fields back out of them. So would
//! `match (Str.to_i64(text)) { :Ok(n) => ..., :Err(_) => ... }`, which builds `:Ok(n)` on one
//! branch only to take it apart after the branches join.
//!
//! The IR's locals are variables, so a variable that holds a box becomes a group of variables:
//! one for the box's shape, and one for each field some instruction reads. Building the box
//! assigns them, an assignment between two such variables copies them, and a read of the shape
//! or of a field reads them. No box exists, and because every assignment of the old variable
//! assigns the new ones at the same place, a read sees the same value on every path, including
//! around a loop and after a join of branches that built different shapes.
//!
//! A tag without a payload is one static box per shape, which nothing allocates, but a variable
//! that holds one becomes its shape all the same, so a test of it compares a number instead of
//! loading the shape out of the box.
//!
//! Variables that are copied into each other form a group, which is replaced as a whole or not
//! at all. A group keeps its boxes when any of its variables is assigned by anything but a
//! construction, a copy or a tag without a payload, or is used by anything but a read of its
//! shape or of a field or a copy within the group: as an argument, a list item, a field of
//! another box or the function's result.
//!
//! A field that only one shape of a tag has is read from a variable that the constructions of
//! the other shapes do not assign. Such a read never runs for those shapes, but the reference
//! counts treat the variable as live, which is sound because a local starts out as zero and a
//! zero string, list or box counts as nothing.
//!
//! The list `Str.split_on` builds gets the same treatment when its items are only read at
//! constant positions, as in `parts <- Str.split_on(row, ","); parts[[2]]`
//! ([`replace_split_lists`]). Its variables are the string, the number of pieces, and where
//! each piece that is read starts and ends, so a read cuts its piece out of the string and no
//! list or unread piece is ever allocated.

use {
    crate::{
        ir::{Constant, Function, Inst, Item, Local, Op, Terminator, Type},
        ownership::Bits,
        shapes,
    },
    smallvec::SmallVec,
};

/// How many times [`unbox`] runs: each run frees the boxes that were only fields of the boxes
/// the run before it removed.
const MAX_ROUNDS: usize = 4;

const NONE: u32 = u32::MAX;

/// Removes every box of `function` that only has its fields or its shape read.
pub fn unbox(function: &mut Function) {
    for _ in 0..MAX_ROUNDS {
        if !unbox_once(function) {
            break;
        }
    }
}

/// Removes every instruction whose result nothing reads and that neither fails nor does
/// anything else, and then those whose results only removed instructions read. A record that
/// became its fields leaves such instructions behind: the value of a field no read asks for
/// is still computed, and for a string that is an allocation.
pub fn remove_unread(function: &mut Function) {
    let mut reads = vec![0u32; function.locals.len()];
    for block in &function.blocks {
        for inst in &block.insts {
            for operand in inst.op.operands() {
                reads[operand.0 as usize] += 1;
            }
        }
        match block.terminator {
            Terminator::Branch { condition, .. } => reads[condition.0 as usize] += 1,
            Terminator::Return(value) => reads[value.0 as usize] += 1,
            Terminator::Jump(_) | Terminator::Fail => {}
        }
    }
    // An instruction is read only by instructions after it or around a loop, so a few passes
    // from the back remove a chain of them.
    for _ in 0..MAX_ROUNDS {
        let mut removed = false;
        for block in function.blocks.iter_mut().rev() {
            let mut index = block.insts.len();
            while index > 0 {
                index -= 1;
                let inst = &block.insts[index];
                let assigned = inst.assigned();
                if assigned.is_empty()
                    || inst.op.can_fail()
                    || assigned.iter().any(|local| reads[local.0 as usize] != 0)
                {
                    continue;
                }
                for operand in inst.op.operands() {
                    reads[operand.0 as usize] -= 1;
                }
                block.insts.remove(index);
                removed = true;
            }
        }
        if !removed {
            break;
        }
    }
}

/// The largest position a read of a replaced split list may have. It bounds the stack room one
/// split takes for its offsets, and a set of read positions fits one word.
const MAX_PIECES: i64 = 32;

/// Replaces every list of `Str.split_on` that only has items read at constant positions with
/// the string and the offsets of those pieces in it.
///
/// A group of list variables is replaced when every assignment to one of them is a split or a
/// copy within the group, and every use is a copy within the group or an [`Op::Get`] whose
/// index is a local that one `I64` constant assigns. A read keeps its failure: an
/// [`Op::Piece`] fails where the list would have been too short.
pub fn replace_split_lists(function: &mut Function) {
    let locals = function.locals.len();
    let mut groups = Groups {
        parents: (0..locals as u32).collect(),
    };
    // The constant each local holds, for a local that one constant assigns and nothing else.
    let mut constants: Vec<Option<i64>> = vec![None; locals];
    let mut assigned = Bits::new(locals);
    let mut splits = false;
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        for local in inst.assigned() {
            let index = local.0 as usize;
            constants[index] = match inst.op {
                Op::Const(Constant::I64(value))
                    if !assigned.contains(index) && !function.is_param(local) =>
                {
                    Some(value)
                }
                _ => None,
            };
            assigned.insert(index);
        }
        match (&inst.op, inst.dest) {
            (&Op::Copy(source), Some(dest))
                if holds_strings(function, source) && holds_strings(function, dest) =>
            {
                groups.join(source, dest);
            }
            (Op::Split(..), Some(_)) => splits = true,
            _ => {}
        }
    }
    if !splits {
        return;
    }

    // By the local that names a group: whether its lists stay, whether a split builds one, and
    // the positions its reads have, as one bit each.
    let mut kept = Bits::new(locals);
    let mut built = Bits::new(locals);
    let mut reads: Vec<u32> = vec![0; locals];
    for block in &function.blocks {
        for inst in &block.insts {
            for local in inst.assigned() {
                if !holds_strings(function, local) {
                    continue;
                }
                let group = groups.find(local) as usize;
                match inst.op {
                    Op::Split(..) => built.insert(group),
                    Op::Copy(source) if holds_strings(function, source) => {}
                    _ => kept.insert(group),
                }
            }
            for operand in inst.op.operands() {
                if !holds_strings(function, operand) {
                    continue;
                }
                let group = groups.find(operand) as usize;
                let replaceable = match (&inst.op, inst.dest) {
                    (&Op::Get(_, index), Some(_)) => match constants[index.0 as usize] {
                        Some(position @ 1..=MAX_PIECES) => {
                            reads[group] |= 1 << (position - 1);
                            true
                        }
                        _ => false,
                    },
                    (Op::Copy(_), Some(dest)) => holds_strings(function, dest),
                    _ => false,
                };
                if !replaceable {
                    kept.insert(group);
                }
            }
        }
        if let Terminator::Return(local) = block.terminator
            && holds_strings(function, local)
        {
            kept.insert(groups.find(local) as usize);
        }
    }

    // The variables of a replaced local are fresh locals numbered from `first[local]`: the
    // string, the number of pieces, and the start and the end of each piece its group reads,
    // in the order of their positions.
    let mut first: Vec<u32> = vec![NONE; locals];
    for (index, first) in first.iter_mut().enumerate() {
        let local = Local(index as u32);
        if !holds_strings(function, local) {
            continue;
        }
        let group = groups.find(local) as usize;
        if kept.contains(group) || !built.contains(group) {
            continue;
        }
        *first = function.locals.len() as u32;
        function.locals.push(Type::Str);
        let offsets = 1 + 2 * reads[group].count_ones() as usize;
        let room = function.locals.len() + offsets;
        function.locals.resize(room, Type::I64);
    }

    let replaced = |local: Local| first[local.0 as usize] != NONE;
    // The start of the piece at `position` among the variables of a local whose group reads
    // the positions `reads`; its end is the local after it.
    let start_of = |first: u32, reads: u32, position: u32| {
        let before = (reads & ((1 << (position - 1)) - 1)).count_ones();
        Local(first + 2 + 2 * before)
    };
    for body in &mut function.blocks {
        let old = std::mem::take(&mut body.insts);
        body.insts.reserve(old.len());
        for inst in old {
            match (&inst.op, inst.dest) {
                (&Op::Split(string, separator), Some(dest)) if replaced(dest) => {
                    let first = first[dest.0 as usize];
                    let reads = reads[groups.find(dest) as usize];
                    // A list that is never read needs nothing computed at all.
                    if reads == 0 {
                        continue;
                    }
                    let bounds: SmallVec<[(u32, Local, Local); 4]> = (1..=MAX_PIECES as u32)
                        .filter(|position| reads & (1 << (position - 1)) != 0)
                        .map(|position| {
                            let start = start_of(first, reads, position);
                            (position, start, Local(start.0 + 1))
                        })
                        .collect();
                    let measure = Op::SplitBounds {
                        string,
                        separator,
                        bounds,
                    };
                    body.insts.push(Inst::new(Some(Local(first + 1)), measure));
                    // The string is copied after it is measured, so that a string that dies
                    // here is moved into the variable and not counted twice.
                    body.insts
                        .push(Inst::new(Some(Local(first)), Op::Copy(string)));
                }
                (&Op::Copy(source), Some(dest)) if replaced(dest) => {
                    let (from, to) = (first[source.0 as usize], first[dest.0 as usize]);
                    let reads = reads[groups.find(dest) as usize];
                    let count = if from == to || reads == 0 {
                        0
                    } else {
                        2 + 2 * reads.count_ones()
                    };
                    for offset in 0..count {
                        let local = Local(to + offset);
                        body.insts
                            .push(Inst::new(Some(local), Op::Copy(Local(from + offset))));
                    }
                }
                (&Op::Get(list, index), dest) if replaced(list) => {
                    let first = first[list.0 as usize];
                    let reads = reads[groups.find(list) as usize];
                    let Some(position) = constants[index.0 as usize] else {
                        panic!("a replaced split list is read at a position that is not constant");
                    };
                    let position = position as u32;
                    let start = start_of(first, reads, position);
                    let piece = Op::Piece {
                        string: Local(first),
                        start,
                        end: Local(start.0 + 1),
                        count: Local(first + 1),
                        index: position,
                    };
                    body.insts.push(Inst::new(dest, piece));
                }
                _ => body.insts.push(inst),
            }
        }
    }
}

fn unbox_once(function: &mut Function) -> bool {
    let locals = function.locals.len();
    let mut groups = Groups {
        parents: (0..locals as u32).collect(),
    };
    let mut builds = false;
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        match (&inst.op, inst.dest) {
            (&Op::Copy(source), Some(dest))
                if holds_box(function, source) && holds_box(function, dest) =>
            {
                groups.join(source, dest);
            }
            (Op::Construct(..) | Op::Const(Constant::Object(_)), _) => builds = true,
            _ => {}
        }
    }
    if !builds {
        return false;
    }

    // A group whose boxes stay, and a group some instruction builds a box or a tag for, each
    // marked at the local that names the group. A group nothing builds has nothing to replace,
    // which is every group of a round before this one.
    let mut kept = Bits::new(locals);
    let mut built = Bits::new(locals);
    let mut fields = Fields {
        slots: Vec::new(),
        heads: vec![NONE; locals],
        lens: vec![0; locals],
    };
    for block in &function.blocks {
        for inst in &block.insts {
            for local in inst.assigned() {
                if !holds_box(function, local) {
                    continue;
                }
                let group = groups.find(local) as usize;
                let builds = match inst.op {
                    Op::Construct(..) => true,
                    Op::Const(Constant::Object(pointer)) => {
                        shapes::constant_shape(pointer).is_some()
                    }
                    _ => false,
                };
                let copies = matches!(inst.op, Op::Copy(source) if holds_box(function, source));
                if inst.dest != Some(local) || !(builds || copies) {
                    kept.insert(group);
                } else if builds {
                    built.insert(group);
                }
            }
            for operand in inst.op.operands() {
                if !holds_box(function, operand) {
                    continue;
                }
                let group = groups.find(operand);
                let replaceable = match (&inst.op, inst.dest) {
                    (Op::ShapeOf(_), _) => true,
                    (&Op::Field(_, shape, field), Some(dest)) => {
                        // Every read of one field has the same type in lowered code. A group
                        // whose reads differ keeps its boxes rather than its variables
                        // converting between the two.
                        let read = function.ty(dest);
                        let slot = fields.find_or_add(group, shape, field, read);
                        fields.slots[slot].read == read
                    }
                    (Op::Field(..), None) => true,
                    (Op::Copy(_), Some(dest)) => holds_box(function, dest),
                    _ => false,
                };
                if !replaceable {
                    kept.insert(group as usize);
                }
            }
        }
        if let Terminator::Return(local) = block.terminator
            && holds_box(function, local)
        {
            kept.insert(groups.find(local) as usize);
        }
    }

    // Each field's variable has the type of the values the constructions store in it, or the
    // type the box stores when they differ.
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        let (Op::Construct(shape, values), Some(dest)) = (&inst.op, inst.dest) else {
            continue;
        };
        if !holds_box(function, dest) {
            continue;
        }
        let group = groups.find(dest);
        let mut at = fields.heads[group as usize];
        while let Some(slot) = fields.slots.get_mut(at as usize) {
            at = slot.next;
            if slot.shape != *shape {
                continue;
            }
            let Some(&value) = values.get(slot.field as usize) else {
                kept.insert(group as usize);
                continue;
            };
            let value = function.ty(value);
            match slot.value {
                None => slot.value = Some(value),
                Some(held) if held == value => {}
                Some(held) => {
                    let stored = shapes::shape(*shape)
                        .fields
                        .get(slot.field as usize)
                        .map(|&item| Type::of_item(item));
                    match stored {
                        Some(stored) if fits(stored, held) && fits(stored, value) => {
                            slot.value = Some(stored);
                        }
                        _ => kept.insert(group as usize),
                    }
                }
            }
        }
    }
    for slot in &fields.slots {
        if !fits(slot.read, slot.value.unwrap_or(slot.read)) {
            kept.insert(slot.group as usize);
        }
    }

    // The variables of a replaced local are fresh locals numbered from `first[local]`: its
    // shape, then its group's fields in the order `Fields` numbers them.
    let mut first: Vec<u32> = vec![NONE; locals];
    let mut added: Vec<Type> = Vec::new();
    for (index, first) in first.iter_mut().enumerate() {
        let local = Local(index as u32);
        if !holds_box(function, local) {
            continue;
        }
        let group = groups.find(local) as usize;
        if kept.contains(group) || !built.contains(group) {
            continue;
        }
        *first = (locals + added.len()) as u32;
        added.push(Type::Int);
        let start = added.len();
        added.resize(start + fields.lens[group] as usize, Type::Int);
        let mut at = fields.heads[group];
        while let Some(slot) = fields.slots.get(at as usize) {
            added[start + slot.position as usize] = slot.value.unwrap_or(slot.read);
            at = slot.next;
        }
    }
    if added.is_empty() {
        return false;
    }
    function.locals.extend(added);

    let replaced = |local: Local| first[local.0 as usize] != NONE;
    for body in &mut function.blocks {
        let old = std::mem::take(&mut body.insts);
        body.insts.reserve(old.len());
        for inst in old {
            match (&inst.op, inst.dest) {
                (Op::Construct(shape, values), Some(dest)) if replaced(dest) => {
                    let first = first[dest.0 as usize];
                    let constant = Constant::Int(*shape as i32);
                    body.insts
                        .push(Inst::new(Some(Local(first)), Op::Const(constant)));
                    let mut at = fields.heads[groups.find(dest) as usize];
                    while let Some(slot) = fields.slots.get(at as usize) {
                        at = slot.next;
                        if slot.shape == *shape
                            && let Some(&value) = values.get(slot.field as usize)
                        {
                            let local = Local(first + 1 + slot.position);
                            body.insts.push(Inst::new(Some(local), Op::Copy(value)));
                        }
                    }
                }
                (&Op::Const(Constant::Object(pointer)), Some(dest)) if replaced(dest) => {
                    let shape = shapes::constant_shape(pointer).unwrap_or(NONE);
                    let constant = Constant::Int(shape as i32);
                    let local = Local(first[dest.0 as usize]);
                    body.insts.push(Inst::new(Some(local), Op::Const(constant)));
                }
                (&Op::Copy(source), Some(dest)) if replaced(dest) => {
                    let (from, to) = (first[source.0 as usize], first[dest.0 as usize]);
                    let count = if from == to {
                        0
                    } else {
                        1 + fields.lens[groups.find(dest) as usize]
                    };
                    for offset in 0..count {
                        let local = Local(to + offset);
                        body.insts
                            .push(Inst::new(Some(local), Op::Copy(Local(from + offset))));
                    }
                }
                (&Op::ShapeOf(source), dest) if replaced(source) => {
                    let shape = Local(first[source.0 as usize]);
                    body.insts.push(Inst::new(dest, Op::Copy(shape)));
                }
                (&Op::Field(source, shape, field), dest) if replaced(source) => {
                    let group = groups.find(source);
                    if let (Some(slot), Some(_)) = (fields.find(group, shape, field), dest) {
                        let position = fields.slots[slot].position;
                        let local = Local(first[source.0 as usize] + 1 + position);
                        body.insts.push(Inst::new(dest, Op::Copy(local)));
                    }
                }
                _ => body.insts.push(inst),
            }
        }
    }
    true
}

/// Whether `local` is a variable that holds a record or a tag and that the function owns. A
/// parameter holds its caller's box.
fn holds_box(function: &Function, local: Local) -> bool {
    !function.is_param(local) && matches!(function.ty(local), Type::Record(_) | Type::Tag(_))
}

/// Whether `local` is a variable that holds a list of strings and that the function owns.
fn holds_strings(function: &Function, local: Local) -> bool {
    let strings = Type::Vector {
        item: Item::Str,
        array: true,
    };
    !function.is_param(local) && function.ty(local) == strings
}

/// Whether a field read into a local of type `read` may copy a local of type `value`: the same
/// type, a `Bool` read as the logical it is stored as, or a tag of a known union read as the tag
/// of any union a box stores.
fn fits(read: Type, value: Type) -> bool {
    read == value
        || matches!(
            (read, value),
            (Type::Lgl, Type::Bool) | (Type::Tag(shapes::ANY), Type::Tag(_))
        )
}

/// The locals that are copied into each other, as disjoint sets each named by one of its
/// locals.
struct Groups {
    parents: Vec<u32>,
}

impl Groups {
    fn find(&mut self, local: Local) -> u32 {
        let mut at = local.0;
        loop {
            let parent = self.parents[at as usize];
            if parent == at {
                return at;
            }
            let above = self.parents[parent as usize];
            self.parents[at as usize] = above;
            at = above;
        }
    }

    fn join(&mut self, a: Local, b: Local) {
        let (a, b) = (self.find(a), self.find(b));
        self.parents[a.max(b) as usize] = a.min(b);
    }
}

/// The fields each group has read, as one list per group threaded through `slots`.
struct Fields {
    slots: Vec<Slot>,
    /// By the local that names a group: its first slot, or `NONE`.
    heads: Vec<u32>,
    /// By the local that names a group: how many slots it has.
    lens: Vec<u32>,
}

struct Slot {
    group: u32,
    shape: u32,
    field: u32,
    /// The slot's number within its group.
    position: u32,
    /// The type the field is read as.
    read: Type,
    /// The type of the values the group's constructions store in the field, once one is seen.
    value: Option<Type>,
    next: u32,
}

impl Fields {
    fn find(&self, group: u32, shape: u32, field: u32) -> Option<usize> {
        let mut at = self.heads[group as usize];
        while let Some(slot) = self.slots.get(at as usize) {
            if slot.shape == shape && slot.field == field {
                return Some(at as usize);
            }
            at = slot.next;
        }
        None
    }

    fn find_or_add(&mut self, group: u32, shape: u32, field: u32, read: Type) -> usize {
        if let Some(slot) = self.find(group, shape, field) {
            return slot;
        }
        self.slots.push(Slot {
            group,
            shape,
            field,
            position: self.lens[group as usize],
            read,
            value: None,
            next: self.heads[group as usize],
        });
        self.lens[group as usize] += 1;
        self.heads[group as usize] = self.slots.len() as u32 - 1;
        self.slots.len() - 1
    }
}
