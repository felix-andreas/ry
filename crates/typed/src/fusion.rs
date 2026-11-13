//! Fuses a count of a logical list with the list comparisons and logical operators that built
//! it, and the counts of one stretch of code with each other. `List.count_if(time >= t & event,
//! \(b) b)` would otherwise build two lists only to count one of them; fused, [`Op::CountTrue`]
//! reads `time`, `t` and `event` in one pass, and the comparisons that built the lists are
//! removed when nothing else reads them. Several such counts in a row, as a statistic over
//! `time >= t`, `time >= t & treated` and `time == t & event` makes, then become one pass that
//! computes each shared comparison once.
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
            COUNTED_LISTS, ELEMENTWISE_INPUTS, ELEMENTWISE_STEPS, MASK_AND, MASK_DBL, MASK_I64,
            MASK_NOT, MASK_OR,
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
/// removes those operations where nothing else reads their results, and then merges the counts
/// of each stretch of straight-line code into one.
pub fn fuse(function: &mut Function) {
    let predecessors = jump_predecessors(function);
    let mut absorbed: Vec<(BlockId, usize)> = Vec::new();
    for block in 0..function.blocks.len() {
        for index in 0..function.blocks[block].insts.len() {
            let Op::CountTrue {
                program,
                inputs,
                counts,
            } = &function.blocks[block].insts[index].op
            else {
                continue;
            };
            let ([list], [(_, count)]) = (&inputs[..], &counts[..]) else {
                continue;
            };
            if !program.is_empty() {
                continue;
            }
            let (list, count) = (*list, *count);
            let run = Run::new(function, &predecessors, BlockId(block as u32), index);
            let mut fusion = Fusion {
                function,
                run: &run,
                program: Program::default(),
                absorbed: Vec::new(),
            };
            let Some(Operand::Step(step)) = fusion.logical(list, run.end) else {
                continue;
            };
            let program = fusion.program;
            if !program.fits() {
                continue;
            }
            absorbed.extend(fusion.absorbed);
            function.blocks[block].insts[index].op = Op::CountTrue {
                counts: SmallVec::from_slice(&[(program.number(Operand::Step(step)), count)]),
                program: program.encoded(),
                inputs: program.inputs,
            };
        }
    }
    remove_unread(function, &absorbed);
    merge_counts(function, &predecessors);
}

/// Moves each count into the count before it in the same straight-line code, where the earlier
/// one can compute both: one pass then reads the inputs they share once and computes a step
/// they share once.
fn merge_counts(function: &mut Function, predecessors: &[Option<BlockId>]) {
    for block in 0..function.blocks.len() {
        let mut index = 0;
        while index < function.blocks[block].insts.len() {
            let merged =
                earlier_count(function, predecessors, block, index).and_then(|(at, earlier)| {
                    let earlier_op = &function.blocks[at].insts[earlier].op;
                    let later_op = &function.blocks[block].insts[index].op;
                    Some((at, earlier, merged(function, earlier_op, later_op)?))
                });
            match merged {
                Some((at, earlier, op)) => {
                    function.blocks[at].insts[earlier].op = op;
                    function.blocks[block].insts.remove(index);
                }
                None => index += 1,
            }
        }
    }
}

/// The count that the count at `index` of `block` can move into: the nearest one before it in
/// its straight-line code, as its block and index. From that count on, no instruction may
/// assign an input of the later count, so the inputs hold there what they hold at the later
/// count, and none may read or assign one of its results. No instruction in between may call a
/// function either, because the later count can fail, and a failure moved before a call would
/// keep the call from running.
fn earlier_count(
    function: &Function,
    predecessors: &[Option<BlockId>],
    block: usize,
    index: usize,
) -> Option<(usize, usize)> {
    let later = &function.blocks[block].insts[index];
    let Op::CountTrue { inputs, .. } = &later.op else {
        return None;
    };
    let results = later.assigned();
    let (mut at, mut before) = (block, index);
    for _ in 0..MAX_RUN {
        while before > 0 {
            before -= 1;
            let inst = &function.blocks[at].insts[before];
            let assigns = inst
                .assigned()
                .iter()
                .any(|local| inputs.contains(local) || results.contains(local));
            let reads = inst
                .op
                .operands()
                .iter()
                .any(|operand| results.contains(operand));
            if assigns || reads {
                return None;
            }
            match inst.op {
                Op::CountTrue { .. } => return Some((at, before)),
                Op::CallR(..) | Op::CallTyped(..) => return None,
                _ => {}
            }
        }
        at = predecessors[at]?.0 as usize;
        before = function.blocks[at].insts.len();
    }
    None
}

/// One count that computes what the counts `earlier` and `later` compute, if they read a list
/// in common and a pass has room for the inputs, steps and results of both.
fn merged(function: &Function, earlier: &Op, later: &Op) -> Option<Op> {
    let (
        Op::CountTrue {
            program,
            inputs,
            counts,
        },
        Op::CountTrue {
            program: later_program,
            inputs: later_inputs,
            counts: later_counts,
        },
    ) = (earlier, later)
    else {
        return None;
    };
    // A pass fails unless all its lists have one length. A shared list ties the lengths of
    // the two counts together, so the pass fails only where one of them would have failed
    // alone; counts over unrelated lists of different lengths each succeed alone.
    let shares_a_list = later_inputs
        .iter()
        .any(|&input| matches!(function.ty(input), Type::Vector { .. }) && inputs.contains(&input));
    if !shares_a_list {
        return None;
    }
    let mut all = Program::decoded(program, inputs);
    let mut all_counts: SmallVec<[(Operand, Local); 8]> = counts
        .iter()
        .map(|&(number, local)| (operand(number, inputs.len()), local))
        .collect();
    let moved_inputs: SmallVec<[Operand; 8]> =
        later_inputs.iter().map(|&local| all.input(local)).collect();
    let mut moved_steps: SmallVec<[Operand; 8]> = SmallVec::new();
    let moved = |number: u32, steps: &[Operand]| match operand(number, later_inputs.len()) {
        Operand::Input(input) => moved_inputs.get(input as usize).copied(),
        Operand::Step(step) => steps.get(step as usize).copied(),
    };
    for &step in later_program {
        let code = step & 0xFF;
        let left = moved(step >> 8 & 0xFF, &moved_steps)?;
        // `MASK_NOT` has no right operand.
        let right = match code {
            MASK_NOT => Operand::Input(0),
            _ => moved(step >> 16 & 0xFF, &moved_steps)?,
        };
        moved_steps.push(all.step(code, left, right));
    }
    for &(number, local) in later_counts {
        all_counts.push((moved(number, &moved_steps)?, local));
    }
    if !all.fits() || all_counts.len() > COUNTED_LISTS {
        return None;
    }
    Some(Op::CountTrue {
        counts: all_counts
            .iter()
            .map(|&(operand, local)| (all.number(operand), local))
            .collect(),
        program: all.encoded(),
        inputs: all.inputs,
    })
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
                for local in inst.assigned() {
                    assignments.entry(local).or_default().push(number);
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

/// The operand a step or a count of an [`Op::CountTrue`] with `inputs` inputs numbers `number`.
fn operand(number: u32, inputs: usize) -> Operand {
    match number.checked_sub(inputs as u32) {
        Some(step) => Operand::Step(step),
        None => Operand::Input(number),
    }
}

/// The inputs and steps of a count while it is built or merged. A step is
/// `code | left << 8 | right << 16` as in [`Op::CountTrue`], except that an operand byte is an
/// input's position or, with [`STEP`] set, an earlier step's, so adding an input renumbers
/// nothing.
#[derive(Default)]
struct Program {
    inputs: SmallVec<[Local; 4]>,
    steps: SmallVec<[u32; 8]>,
}

impl Program {
    fn decoded(program: &[u32], inputs: &[Local]) -> Program {
        let mark = |byte: u32| match operand(byte, inputs.len()) {
            Operand::Input(input) => input,
            Operand::Step(step) => STEP | step,
        };
        Program {
            inputs: inputs.into(),
            steps: program
                .iter()
                .map(|&step| {
                    let right = match step & 0xFF {
                        MASK_NOT => 0,
                        _ => mark(step >> 16 & 0xFF),
                    };
                    step & 0xFF | mark(step >> 8 & 0xFF) << 8 | right << 16
                })
                .collect(),
        }
    }

    /// The steps as [`Op::CountTrue`] numbers their operands: inputs first, steps after them.
    fn encoded(&self) -> SmallVec<[u32; 8]> {
        let number = |byte: u32| match byte & STEP {
            0 => byte,
            _ => self.inputs.len() as u32 + (byte & !STEP),
        };
        self.steps
            .iter()
            .map(|&step| {
                step & 0xFF | number(step >> 8 & 0xFF) << 8 | number(step >> 16 & 0xFF) << 16
            })
            .collect()
    }

    fn number(&self, operand: Operand) -> u32 {
        match operand {
            Operand::Input(input) => input,
            Operand::Step(step) => self.inputs.len() as u32 + step,
        }
    }

    /// Whether one pass of the kernel has room for the inputs and steps.
    fn fits(&self) -> bool {
        self.steps.len() <= ELEMENTWISE_STEPS && self.inputs.len() <= ELEMENTWISE_INPUTS
    }

    fn input(&mut self, local: Local) -> Operand {
        let position = match self.inputs.iter().position(|&input| input == local) {
            Some(position) => position,
            None => {
                self.inputs.push(local);
                self.inputs.len() - 1
            }
        };
        Operand::Input(position as u32)
    }

    /// The step that computes `code` of the two operands, which is an earlier step when one
    /// already computes it.
    fn step(&mut self, code: u32, left: Operand, right: Operand) -> Operand {
        let pack = |operand: Operand| match operand {
            Operand::Input(input) => input,
            Operand::Step(step) => STEP | step,
        };
        let step = code | pack(left) << 8 | pack(right) << 16;
        let position = match self.steps.iter().position(|&found| found == step) {
            Some(position) => position,
            None => {
                self.steps.push(step);
                self.steps.len() - 1
            }
        };
        Operand::Step(position as u32)
    }
}

struct Fusion<'f> {
    function: &'f Function,
    run: &'f Run,
    program: Program,
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
        let saved = (
            self.program.inputs.len(),
            self.program.steps.len(),
            self.absorbed.len(),
        );
        let operand = match inst.op {
            Op::Copy(source) if ty(source) == ty(local) => self.logical(source, number),
            Op::VectorCompare(op, item @ (Item::I64 | Item::Dbl), a, b) if lanes(ty(local)) => {
                let code = if item == Item::I64 {
                    MASK_I64
                } else {
                    MASK_DBL
                } + op as u32;
                match (self.numeric(a, item, number), self.numeric(b, item, number)) {
                    (Some(x), Some(y)) => Some(self.program.step(code, x, y)),
                    _ => None,
                }
            }
            Op::VectorLogic(and, a, b) if lanes(ty(a)) && lanes(ty(b)) => {
                let code = if and { MASK_AND } else { MASK_OR };
                match (self.logical(a, number), self.logical(b, number)) {
                    (Some(x), Some(y)) => Some(self.program.step(code, x, y)),
                    _ => None,
                }
            }
            Op::VectorNot(a) if lanes(ty(a)) => self
                .logical(a, number)
                .map(|x| self.program.step(MASK_NOT, x, Operand::Input(0))),
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
                self.program.inputs.truncate(saved.0);
                self.program.steps.truncate(saved.1);
                self.absorbed.truncate(saved.2);
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
        self.run
            .unchanged(local, read)
            .then(|| self.program.input(local))
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
