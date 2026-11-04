//! Reference counting for the typed IR, in the style of Perceus: every counted local owns its
//! reference, an instruction takes ownership of an operand that dies at it when it can use one
//! (to reuse a unique buffer in place, or to move it), and a local that dies anywhere else is
//! dropped right there.
//!
//! Liveness decides all of it. An operand dies at an instruction when no later read sees its
//! current value. A branch whose successors need different locals gets an edge block that drops
//! the ones the taken side no longer reads. A failing instruction drops every counted local live
//! across it, so a call that gives up leaves nothing allocated behind.
//!
//! Parameters borrow their caller's references: they are never dropped and never moved, and an
//! instruction that would consume one copies it instead.

use {
    crate::ir::{Block, BlockId, Function, Inst, Local, Op, Terminator},
    smallvec::SmallVec,
};

/// Inserts every count operation `function` needs and fills in each instruction's moves and
/// failure drops.
pub fn insert(function: &mut Function) {
    retarget_self_updates(function);
    let locals = function.locals.len();
    let counted = {
        let mut counted = Bits::new(locals);
        for (index, ty) in function.locals.iter().enumerate() {
            if ty.is_counted() && index >= function.params {
                counted.insert(index);
            }
        }
        counted
    };
    let live_in = liveness(function, &counted);
    let blocks = function.blocks.len();
    for index in 0..blocks {
        let mut live = live_out(function, &live_in, index, &counted);
        let block = &mut function.blocks[index];
        let old = std::mem::take(&mut block.insts);
        let mut rewritten: Vec<Inst> = Vec::with_capacity(old.len() + 4);
        for mut inst in old.into_iter().rev() {
            let operands = inst.op.operands();
            let mut drops: SmallVec<[Local; 4]> = SmallVec::new();
            if let Some(dest) = inst.dest
                && counted.contains(dest.0 as usize)
                && !live.contains(dest.0 as usize)
            {
                drops.push(dest);
            }
            let mut fail_drops: SmallVec<[Local; 4]> = SmallVec::new();
            if inst.op.can_fail() {
                for local in live.iter() {
                    if Some(Local(local as u32)) != inst.dest {
                        fail_drops.push(Local(local as u32));
                    }
                }
            }
            for (position, &operand) in operands.iter().enumerate() {
                let index = operand.0 as usize;
                if !counted.contains(index) || operands[..position].contains(&operand) {
                    continue;
                }
                let dies = !live.contains(index) || inst.dest == Some(operand);
                if !dies {
                    continue;
                }
                let once = operands.iter().filter(|&&other| other == operand).count() == 1;
                if once && inst.op.can_consume(position) {
                    inst.moves |= 1 << position;
                    // An inline instruction that fails after taking an operand drops it on its
                    // failure path; a runtime function consumes it either way.
                    if matches!(inst.op, Op::Set(..)) {
                        fail_drops.push(operand);
                    }
                } else {
                    drops.push(operand);
                    fail_drops.push(operand);
                }
            }
            if let Some(dest) = inst.dest {
                live.remove(dest.0 as usize);
            }
            for &operand in &operands {
                if counted.contains(operand.0 as usize) {
                    live.insert(operand.0 as usize);
                }
            }
            inst.fail_drops = fail_drops;
            for &drop in drops.iter().rev() {
                rewritten.push(Inst::new(None, Op::Drop(drop)));
            }
            rewritten.push(inst);
        }
        rewritten.reverse();
        function.blocks[index].insts = rewritten;
    }
    split_branch_edges(function, &live_in, &counted);
}

/// Gives an instruction that writes a counted local it also reads, without taking it, a fresh
/// destination copied into the local afterwards. The old value then dies at the instruction and
/// is dropped after it, before the copy overwrites the local, instead of being lost.
fn retarget_self_updates(function: &mut Function) {
    for index in 0..function.blocks.len() {
        let old = std::mem::take(&mut function.blocks[index].insts);
        let mut rewritten = Vec::with_capacity(old.len());
        for mut inst in old {
            let Some(dest) = inst.dest else {
                rewritten.push(inst);
                continue;
            };
            let operands = inst.op.operands();
            let reads = operands.iter().filter(|&&operand| operand == dest).count();
            let consumable = operands
                .iter()
                .position(|&operand| operand == dest)
                .is_some_and(|position| inst.op.can_consume(position));
            if !function.ty(dest).is_counted() || reads == 0 || (reads == 1 && consumable) {
                rewritten.push(inst);
                continue;
            }
            function.locals.push(function.ty(dest));
            let fresh = Local(function.locals.len() as u32 - 1);
            inst.dest = Some(fresh);
            rewritten.push(inst);
            rewritten.push(Inst::new(Some(dest), Op::Copy(fresh)));
        }
        function.blocks[index].insts = rewritten;
    }
}

/// The counted locals live into each block.
fn liveness(function: &Function, counted: &Bits) -> Vec<Bits> {
    let locals = function.locals.len();
    let blocks = function.blocks.len();
    let mut uses = vec![Bits::new(locals); blocks];
    let mut defs = vec![Bits::new(locals); blocks];
    for (index, block) in function.blocks.iter().enumerate() {
        let (block_uses, block_defs) = (&mut uses[index], &mut defs[index]);
        for inst in &block.insts {
            for operand in inst.op.operands() {
                let local = operand.0 as usize;
                if counted.contains(local) && !block_defs.contains(local) {
                    block_uses.insert(local);
                }
            }
            if let Some(dest) = inst.dest {
                block_defs.insert(dest.0 as usize);
            }
        }
        if let Terminator::Return(value) = block.terminator {
            let local = value.0 as usize;
            if counted.contains(local) && !block_defs.contains(local) {
                block_uses.insert(local);
            }
        }
    }
    let mut live_in = vec![Bits::new(locals); blocks];
    let mut changed = true;
    while changed {
        changed = false;
        for index in (0..blocks).rev() {
            let mut live = Bits::new(locals);
            for successor in function.blocks[index].terminator.successors() {
                live.union(&live_in[successor.0 as usize]);
            }
            live.subtract(&defs[index]);
            live.union(&uses[index]);
            if live != live_in[index] {
                live_in[index] = live;
                changed = true;
            }
        }
    }
    live_in
}

fn live_out(function: &Function, live_in: &[Bits], index: usize, counted: &Bits) -> Bits {
    let mut live = Bits::new(function.locals.len());
    for successor in function.blocks[index].terminator.successors() {
        live.union(&live_in[successor.0 as usize]);
    }
    if let Terminator::Return(value) = function.blocks[index].terminator
        && counted.contains(value.0 as usize)
    {
        live.insert(value.0 as usize);
    }
    live
}

/// Gives each branch edge whose target reads fewer locals than the other side a block that drops
/// the difference.
fn split_branch_edges(function: &mut Function, live_in: &[Bits], counted: &Bits) {
    let blocks = function.blocks.len();
    for index in 0..blocks {
        let Terminator::Branch {
            condition,
            then,
            otherwise,
        } = function.blocks[index].terminator
        else {
            continue;
        };
        let mut out = live_in[then.0 as usize].clone();
        out.union(&live_in[otherwise.0 as usize]);
        let edge = |target: BlockId, function: &mut Function| -> BlockId {
            let mut dead = out.clone();
            dead.subtract(&live_in[target.0 as usize]);
            let drops: Vec<Inst> = dead
                .iter()
                .filter(|&local| counted.contains(local))
                .map(|local| Inst::new(None, Op::Drop(Local(local as u32))))
                .collect();
            if drops.is_empty() {
                return target;
            }
            function.blocks.push(Block {
                insts: drops,
                terminator: Terminator::Jump(target),
            });
            BlockId(function.blocks.len() as u32 - 1)
        };
        let then = edge(then, function);
        let otherwise = edge(otherwise, function);
        function.blocks[index].terminator = Terminator::Branch {
            condition,
            then,
            otherwise,
        };
    }
}

/// A set of local indices.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bits(SmallVec<[u64; 2]>);

impl Bits {
    pub fn new(size: usize) -> Bits {
        Bits(SmallVec::from_elem(0, size.div_ceil(64)))
    }

    pub fn insert(&mut self, index: usize) {
        self.0[index / 64] |= 1 << (index % 64);
    }

    pub fn remove(&mut self, index: usize) {
        self.0[index / 64] &= !(1 << (index % 64));
    }

    pub fn contains(&self, index: usize) -> bool {
        self.0
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }

    fn union(&mut self, other: &Bits) {
        for (word, other) in self.0.iter_mut().zip(&other.0) {
            *word |= other;
        }
    }

    fn subtract(&mut self, other: &Bits) {
        for (word, other) in self.0.iter_mut().zip(&other.0) {
            *word &= !other;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(index, &word)| {
            let mut word = word;
            std::iter::from_fn(move || {
                if word == 0 {
                    return None;
                }
                let bit = word.trailing_zeros() as usize;
                word &= word - 1;
                Some(index * 64 + bit)
            })
        })
    }
}
