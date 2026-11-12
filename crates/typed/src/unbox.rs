//! Replaces a record or tag that never leaves its function with the values of its fields.
//! `invoice <- price_line(order)` followed by `invoice$gross`, with `price_line` inlined, would
//! otherwise allocate two boxes per call only to read fields back out of them; unboxed, each
//! field read is a copy of the value the box was built from, and no box exists.
//!
//! A box qualifies when it is built by one instruction and every use of it, through the copies
//! an assignment or an inlined call makes, reads a field or its shape. Any other use (an
//! argument, a list item, a field of another box, the function's result, a variable that some
//! other instruction also assigns) keeps the box.
//!
//! The IR's locals are variables, not SSA values, so a field read may only be replaced where it
//! is certain to see the box of the latest construction. That holds when each instruction on the
//! way assigns its local nowhere else and dominates the instructions that read that local: on
//! every path from one run of the construction to the read, nothing in between can then assign
//! any local on the chain again. The fields themselves are copied into fresh locals at the
//! construction, so a later assignment to a variable the box was built from changes nothing.

use {
    crate::{
        ir::{Constant, Function, Inst, Local, Op, Terminator, Type},
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

fn unbox_once(function: &mut Function) -> bool {
    let locals = function.locals.len();
    // Where each local is assigned, for a local one instruction assigns: its block and index.
    let mut sites: Vec<(u32, u32)> = vec![(NONE, NONE); locals];
    let mut reassigned = Bits::new(locals);
    let mut constructs = false;
    for (block, body) in function.blocks.iter().enumerate() {
        for (index, inst) in body.insts.iter().enumerate() {
            let Some(dest) = inst.dest else { continue };
            let slot = dest.0 as usize;
            if sites[slot].0 != NONE || function.is_param(dest) {
                reassigned.insert(slot);
            }
            sites[slot] = (block as u32, index as u32);
            constructs |= matches!(&inst.op, Op::Construct(shape, _) if has_box(*shape));
        }
    }
    if !constructs {
        return false;
    }
    let only_assignment = |local: Local| {
        let (block, index) = sites[local.0 as usize];
        (block != NONE && !reassigned.contains(local.0 as usize))
            .then(|| &function.blocks[block as usize].insts[index as usize].op)
    };

    // The box each local is known to hold, named by the local its construction assigns: that
    // local itself, and every local assigned once as a copy of a local that holds the box.
    let mut boxes: Vec<u32> = vec![NONE; locals];
    for (slot, held) in boxes.iter_mut().enumerate() {
        let mut local = Local(slot as u32);
        // A chain of copies is at most as long as the locals are many. A longer walk is a cycle
        // of copies, which holds no box.
        for _ in 0..locals {
            match only_assignment(local) {
                Some(Op::Construct(shape, _)) if has_box(*shape) => {
                    *held = local.0;
                    break;
                }
                Some(&Op::Copy(source)) => local = source,
                _ => break,
            }
        }
    }

    let dominators = Dominators::new(function);
    let mut kept = Bits::new(locals);
    for (block, body) in function.blocks.iter().enumerate() {
        for (index, inst) in body.insts.iter().enumerate() {
            for operand in inst.op.operands() {
                let held = boxes[operand.0 as usize];
                if held == NONE {
                    continue;
                }
                let (at_block, at_index) = sites[operand.0 as usize];
                let dominated = if at_block == block as u32 {
                    (at_index as usize) < index
                } else {
                    dominators.dominates(at_block, block as u32)
                };
                let reads = match (&inst.op, inst.dest) {
                    (Op::Copy(_), Some(dest)) => boxes[dest.0 as usize] == held,
                    (Op::ShapeOf(_), _) => true,
                    (&Op::Field(_, shape, field), Some(dest)) => {
                        match only_assignment(Local(held)) {
                            Some(Op::Construct(built, values)) => {
                                *built == shape
                                    && values.get(field as usize).is_some_and(|&value| {
                                        fits(function.ty(dest), function.ty(value))
                                    })
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if !dominated || !reads {
                    kept.insert(held as usize);
                }
            }
        }
        if let Terminator::Branch {
            condition: local, ..
        }
        | Terminator::Return(local) = body.terminator
            && boxes[local.0 as usize] != NONE
        {
            kept.insert(boxes[local.0 as usize] as usize);
        }
    }

    // A removed box's fields live in fresh locals, numbered from `first_field` of the box's
    // local in the order of the shape's fields, and `built` is its shape.
    let mut first_field: Vec<u32> = vec![NONE; locals];
    let mut built: Vec<u32> = vec![NONE; locals];
    let mut field_types: Vec<Type> = Vec::new();
    for body in &function.blocks {
        for inst in &body.insts {
            if let (Op::Construct(shape, values), Some(dest)) = (&inst.op, inst.dest)
                && boxes[dest.0 as usize] == dest.0
                && !kept.contains(dest.0 as usize)
            {
                first_field[dest.0 as usize] = (locals + field_types.len()) as u32;
                built[dest.0 as usize] = *shape;
                field_types.extend(values.iter().map(|&value| function.ty(value)));
            }
        }
    }
    if field_types.is_empty() {
        return false;
    }
    function.locals.extend(field_types);
    let removed = |local: Local| {
        let held = boxes[local.0 as usize];
        (held != NONE && first_field[held as usize] != NONE).then_some(held as usize)
    };
    for body in &mut function.blocks {
        let old = std::mem::take(&mut body.insts);
        body.insts.reserve(old.len());
        for inst in old {
            match (&inst.op, inst.dest) {
                (Op::Construct(_, values), Some(dest)) if removed(dest).is_some() => {
                    let first = first_field[dest.0 as usize];
                    for (field, &value) in values.iter().enumerate() {
                        let local = Local(first + field as u32);
                        body.insts.push(Inst::new(Some(local), Op::Copy(value)));
                    }
                }
                // Every read of the copy is replaced too, so nothing reads it.
                (&Op::Copy(source), _) if removed(source).is_some() => {}
                (&Op::ShapeOf(source), dest) if removed(source).is_some() => {
                    let shape = removed(source).map_or(NONE, |held| built[held]);
                    body.insts
                        .push(Inst::new(dest, Op::Const(Constant::Int(shape as i32))));
                }
                (&Op::Field(source, _, field), dest) if removed(source).is_some() => {
                    let first = removed(source).map_or(NONE, |held| first_field[held]);
                    body.insts
                        .push(Inst::new(dest, Op::Copy(Local(first + field))));
                }
                _ => body.insts.push(inst),
            }
        }
    }
    true
}

/// Whether values of the shape are boxes on the heap. A shape without fields is one static box.
fn has_box(shape: u32) -> bool {
    shapes::shape(shape).words != 0
}

/// Whether a field read into a local of type `read` may copy the local of type `value` the box
/// was built from: the same type, a `Bool` read as the logical it is stored as, or a tag of a
/// known union read as the tag of any union a box stores.
fn fits(read: Type, value: Type) -> bool {
    read == value
        || matches!(
            (read, value),
            (Type::Lgl, Type::Bool) | (Type::Tag(shapes::ANY), Type::Tag(_))
        )
}

/// The immediate dominator of each block the entry reaches (Cooper, Harvey and Kennedy's
/// algorithm over the blocks in reverse postorder).
struct Dominators {
    /// Each block's immediate dominator, the entry's being itself, or `NONE` for a block the
    /// entry does not reach.
    immediate: Vec<u32>,
}

impl Dominators {
    fn new(function: &Function) -> Dominators {
        let blocks = function.blocks.len();
        // Each reached block's position in a postorder walk from the entry.
        let mut position: Vec<u32> = vec![NONE; blocks];
        let mut postorder: Vec<u32> = Vec::with_capacity(blocks);
        let mut predecessors: Vec<SmallVec<[u32; 4]>> = vec![SmallVec::new(); blocks];
        let mut visited = Bits::new(blocks);
        let mut stack: Vec<(u32, usize)> = Vec::new();
        if blocks > 0 {
            visited.insert(0);
            stack.push((0, 0));
        }
        while let Some(&mut (block, ref mut next)) = stack.last_mut() {
            let successors = function.blocks[block as usize].terminator.successors();
            match successors.get(*next) {
                Some(successor) => {
                    *next += 1;
                    predecessors[successor.0 as usize].push(block);
                    if !visited.contains(successor.0 as usize) {
                        visited.insert(successor.0 as usize);
                        stack.push((successor.0, 0));
                    }
                }
                None => {
                    position[block as usize] = postorder.len() as u32;
                    postorder.push(block);
                    stack.pop();
                }
            }
        }
        let mut immediate: Vec<u32> = vec![NONE; blocks];
        if blocks > 0 {
            immediate[0] = 0;
        }
        let mut changed = true;
        while changed {
            changed = false;
            for &block in postorder.iter().rev().skip(1) {
                let mut dominator = NONE;
                for &predecessor in &predecessors[block as usize] {
                    if immediate[predecessor as usize] == NONE {
                        continue;
                    }
                    dominator = if dominator == NONE {
                        predecessor
                    } else {
                        let (mut a, mut b) = (predecessor, dominator);
                        while a != b {
                            while position[a as usize] < position[b as usize] {
                                a = immediate[a as usize];
                            }
                            while position[b as usize] < position[a as usize] {
                                b = immediate[b as usize];
                            }
                        }
                        a
                    };
                }
                if immediate[block as usize] != dominator {
                    immediate[block as usize] = dominator;
                    changed = true;
                }
            }
        }
        Dominators { immediate }
    }

    /// Whether every path from the entry to `block` passes through `dominator`, which holds for
    /// every block when the entry does not reach `block` at all.
    fn dominates(&self, dominator: u32, block: u32) -> bool {
        let reached = |block: u32| {
            self.immediate
                .get(block as usize)
                .is_some_and(|&up| up != NONE)
        };
        if !reached(block) {
            return true;
        }
        if !reached(dominator) {
            return false;
        }
        let mut at = block;
        loop {
            if at == dominator {
                return true;
            }
            let up = self.immediate[at as usize];
            if up == at {
                return false;
            }
            at = up;
        }
    }
}
