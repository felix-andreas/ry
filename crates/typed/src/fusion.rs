//! Fuses a count of a logical list with the list comparisons and logical operators that built
//! it. `List.count_if(time >= t & event, \(b) b)` would otherwise build two lists only to count
//! one of them; fused, [`Op::CountTrue`] reads `time`, `t` and `event` in one pass, and the
//! comparisons that built the lists are removed when nothing else reads them.
//!
//! The pass follows a counted list back through the instructions that define it, across local
//! variables and the copies an assignment or an inlined call makes, but only within straight-line
//! code that ends at the count: the count's block and the blocks before it that each have one
//! predecessor, which jumps to it. There every instruction runs exactly once before the count, so
//! an input read later by the count holds the value the comparison read, unless something
//! assigns it in between, which the pass checks.

use {
    crate::{
        ir::{BlockId, Function, Item, Local, Op, Terminator, Type},
        values::{
            ELEMENTWISE_INPUTS, ELEMENTWISE_STEPS, MASK_AND, MASK_DBL, MASK_I64, MASK_NOT, MASK_OR,
        },
    },
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
};

/// The most blocks a straight-line run before a count spans.
const MAX_RUN: usize = 64;

/// Marks an operand byte of a step under construction as an earlier step, not an input. The
/// steps are numbered after the inputs once every input is known.
const STEP: u32 = 0x80;

/// Fuses every count of a logical list in `function` with the operations that built the list,
/// then removes those operations where nothing else reads their results.
pub fn fuse(function: &mut Function) {
    let predecessors = jump_predecessors(function);
    let mut absorbed: Vec<(BlockId, usize)> = Vec::new();
    for block in 0..function.blocks.len() {
        for index in 0..function.blocks[block].insts.len() {
            let Op::CountTrue { program, inputs } = &function.blocks[block].insts[index].op else {
                continue;
            };
            let [list] = inputs[..] else {
                continue;
            };
            if !program.is_empty() {
                continue;
            }
            let run = Run::new(function, &predecessors, BlockId(block as u32), index);
            let mut fusion = Fusion {
                function,
                run: &run,
                inputs: SmallVec::new(),
                steps: SmallVec::new(),
                absorbed: Vec::new(),
            };
            let Some(Operand::Step(_)) = fusion.logical(list, run.end) else {
                continue;
            };
            if fusion.steps.len() > ELEMENTWISE_STEPS || fusion.inputs.len() > ELEMENTWISE_INPUTS {
                continue;
            }
            let base = fusion.inputs.len() as u32;
            let number = |byte: u32| match byte & STEP {
                0 => byte,
                _ => base + (byte & !STEP),
            };
            let program = fusion
                .steps
                .iter()
                .map(|&step| {
                    step & 0xFF | number(step >> 8 & 0xFF) << 8 | number(step >> 16 & 0xFF) << 16
                })
                .collect();
            absorbed.extend(fusion.absorbed);
            function.blocks[block].insts[index].op = Op::CountTrue {
                program,
                inputs: fusion.inputs,
            };
        }
    }
    remove_unread(function, &absorbed);
}

/// For each block, the block before it when that block is its only predecessor and ends in a
/// jump to it.
fn jump_predecessors(function: &Function) -> Vec<Option<BlockId>> {
    let mut counts = vec![0u32; function.blocks.len()];
    let mut from: Vec<Option<BlockId>> = vec![None; function.blocks.len()];
    for (index, block) in function.blocks.iter().enumerate() {
        for successor in block.terminator.successors() {
            let target = successor.0 as usize;
            counts[target] += 1;
            if matches!(block.terminator, Terminator::Jump(_)) {
                from[target] = Some(BlockId(index as u32));
            }
        }
    }
    // The entry block runs first, so nothing comes before it even when a loop jumps back to it.
    from.iter()
        .zip(&counts)
        .enumerate()
        .map(|(index, (&from, &count))| from.filter(|_| index != 0 && count == 1))
        .collect()
}

/// The straight-line code that ends at a count: its instructions numbered in the order they
/// run, and where each local is assigned among them.
struct Run {
    /// The blocks in the order they run, each with the number of its first instruction.
    blocks: SmallVec<[(BlockId, u32); 8]>,
    /// The number of the count itself.
    end: u32,
    /// The numbers of the instructions that assign each local, in increasing order.
    assignments: FxHashMap<Local, SmallVec<[u32; 2]>>,
}

impl Run {
    fn new(
        function: &Function,
        predecessors: &[Option<BlockId>],
        block: BlockId,
        index: usize,
    ) -> Run {
        let mut chain: SmallVec<[BlockId; 8]> = SmallVec::from_slice(&[block]);
        while chain.len() < MAX_RUN {
            let Some(before) = predecessors[chain[chain.len() - 1].0 as usize] else {
                break;
            };
            // A cycle of blocks that each jump to the next is unreachable from the entry, and
            // never a run.
            if chain.contains(&before) {
                chain.clear();
                chain.push(block);
                break;
            }
            chain.push(before);
        }
        chain.reverse();
        let mut blocks = SmallVec::new();
        let mut assignments: FxHashMap<Local, SmallVec<[u32; 2]>> = FxHashMap::default();
        let mut number = 0u32;
        for &id in &chain {
            blocks.push((id, number));
            let insts = &function.blocks[id.0 as usize].insts;
            let count = if id == block { index } else { insts.len() };
            for inst in &insts[..count] {
                if let Some(dest) = inst.dest {
                    assignments.entry(dest).or_default().push(number);
                }
                number += 1;
            }
        }
        Run {
            blocks,
            end: number,
            assignments,
        }
    }

    /// The instruction numbered `number`, with its block and its index there.
    fn instruction(&self, number: u32) -> Option<(BlockId, usize)> {
        let &(block, first) = self
            .blocks
            .iter()
            .rev()
            .find(|&&(_, first)| first <= number)?;
        Some((block, (number - first) as usize))
    }

    /// The number of the last instruction before `before` that assigns `local`.
    fn reaching(&self, local: Local, before: u32) -> Option<u32> {
        self.assignments
            .get(&local)?
            .iter()
            .rev()
            .copied()
            .find(|&number| number < before)
    }

    /// Whether `local` holds at the count the value it held when instruction `read` read it.
    fn unchanged(&self, local: Local, read: u32) -> bool {
        self.assignments.get(&local).is_none_or(|numbers| {
            numbers
                .iter()
                .all(|&number| number < read || number >= self.end)
        })
    }
}

#[derive(Clone, Copy)]
enum Operand {
    Input(u32),
    Step(u32),
}

struct Fusion<'f> {
    function: &'f Function,
    run: &'f Run,
    inputs: SmallVec<[Local; 4]>,
    /// The steps, with each operand that is a step marked by [`STEP`].
    steps: SmallVec<[u32; 8]>,
    absorbed: Vec<(BlockId, usize)>,
}

impl Fusion<'_> {
    /// The logical list `local` holds when instruction `read` reads it, as the step that
    /// computes it or, where its definition does not fuse, as an input. `None` when it is an
    /// input whose value changes before the count.
    fn logical(&mut self, local: Local, read: u32) -> Option<Operand> {
        let lanes = |ty: Type| {
            matches!(
                ty,
                Type::Vector {
                    item: Item::Lgl,
                    array: true
                }
            )
        };
        if let Some(fused) = self.defined(local, read, lanes) {
            return Some(fused);
        }
        match self.function.ty(local) {
            Type::Bool => self.input(local, read),
            ty if lanes(ty) => self.input(local, read),
            _ => None,
        }
    }

    /// The step computing `local` from its definition before `read`, when that definition fuses.
    fn defined(
        &mut self,
        local: Local,
        read: u32,
        lanes: impl Fn(Type) -> bool,
    ) -> Option<Operand> {
        let number = self.run.reaching(local, read)?;
        let (block, index) = self.run.instruction(number)?;
        let function = self.function;
        let inst = &function.blocks[block.0 as usize].insts[index];
        let ty = |local: Local| function.ty(local);
        let saved = (self.inputs.len(), self.steps.len(), self.absorbed.len());
        let operand = match inst.op {
            Op::Copy(source) if ty(source) == ty(local) => self.logical(source, number),
            Op::VectorCompare(op, item @ (Item::I64 | Item::Dbl), a, b) if lanes(ty(local)) => {
                let code = if item == Item::I64 {
                    MASK_I64
                } else {
                    MASK_DBL
                } + op as u32;
                match (self.numeric(a, item, number), self.numeric(b, item, number)) {
                    (Some(x), Some(y)) => Some(self.step(code, x, y)),
                    _ => None,
                }
            }
            Op::VectorLogic(and, a, b) if lanes(ty(a)) && lanes(ty(b)) => {
                let code = if and { MASK_AND } else { MASK_OR };
                match (self.logical(a, number), self.logical(b, number)) {
                    (Some(x), Some(y)) => Some(self.step(code, x, y)),
                    _ => None,
                }
            }
            Op::VectorNot(a) if lanes(ty(a)) => self
                .logical(a, number)
                .map(|x| self.step(MASK_NOT, x, Operand::Input(0))),
            _ => None,
        };
        match operand {
            // A definition that only forwards an input does not fuse anything, and leaves the
            // count reading the copy.
            Some(Operand::Step(step)) => {
                self.absorbed.push((block, index));
                Some(Operand::Step(step))
            }
            _ => {
                (self.inputs, self.steps, self.absorbed) = (
                    self.inputs[..saved.0].into(),
                    self.steps[..saved.1].into(),
                    self.absorbed[..saved.2].to_vec(),
                );
                None
            }
        }
    }

    /// A numeric operand of a comparison of `item`s: a list of them or a scalar.
    fn numeric(&mut self, local: Local, item: Item, read: u32) -> Option<Operand> {
        let fits = match self.function.ty(local) {
            Type::Vector {
                item: found,
                array: true,
            } => found == item,
            Type::I64 => item == Item::I64,
            Type::Dbl => item == Item::Dbl,
            _ => false,
        };
        if fits { self.input(local, read) } else { None }
    }

    fn input(&mut self, local: Local, read: u32) -> Option<Operand> {
        if !self.run.unchanged(local, read) {
            return None;
        }
        let position = match self.inputs.iter().position(|&input| input == local) {
            Some(position) => position,
            None => {
                self.inputs.push(local);
                self.inputs.len() - 1
            }
        };
        Some(Operand::Input(position as u32))
    }

    fn step(&mut self, code: u32, left: Operand, right: Operand) -> Operand {
        let pack = |operand: Operand| match operand {
            Operand::Input(input) => input,
            Operand::Step(step) => STEP | step,
        };
        self.steps.push(code | pack(left) << 8 | pack(right) << 16);
        Operand::Step(self.steps.len() as u32 - 1)
    }
}

/// Removes the absorbed instructions whose results nothing reads any more, and then those whose
/// results only removed instructions read.
fn remove_unread(function: &mut Function, absorbed: &[(BlockId, usize)]) {
    if absorbed.is_empty() {
        return;
    }
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
    let mut removed: Vec<(BlockId, usize)> = Vec::new();
    loop {
        let before = removed.len();
        for &(block, index) in absorbed {
            if removed.contains(&(block, index)) {
                continue;
            }
            let inst = &function.blocks[block.0 as usize].insts[index];
            if inst.dest.is_some_and(|dest| reads[dest.0 as usize] == 0) {
                for operand in inst.op.operands() {
                    reads[operand.0 as usize] -= 1;
                }
                removed.push((block, index));
            }
        }
        if removed.len() == before {
            break;
        }
    }
    removed.sort_unstable_by_key(|&(block, index)| (block.0, std::cmp::Reverse(index)));
    for (block, index) in removed {
        function.blocks[block.0 as usize].insts.remove(index);
    }
}
