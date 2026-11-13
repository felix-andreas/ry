//! Fuses a loop that fills a list with the loop that reads it, so the list is never built.
//!
//! ```r
//! parsed <- List.map(rows, parse_row)
//! people <- List.keep_oks(parsed, function(p) p)
//! totals <- List.walk(people, Dict.empty(), add_score)
//! ```
//!
//! lowers to three counted loops and two lists. The second loop only reads `parsed` item by
//! item, and the third only reads `people`, so each item can go straight from the loop that
//! makes it to the loop that uses it: the three bodies run one after the other in one loop,
//! and neither list exists. Once no list holds them, the results and records the items were
//! boxed in never leave the function, and [`crate::unbox`] replaces them with their fields.
//!
//! A list is fused away when all of this holds:
//!
//! - One counted loop, the producer, fills it: with a `Push` onto a list that starts empty,
//!   or with a `Set` at the loop's own counter into a list as long as the one the loop
//!   counts over, once per iteration. Making the list cannot fail, so its capacity, if it
//!   has one, is known not to be negative.
//! - Another counted loop, the consumer, reads it from the first position to the last, and
//!   never leaves early. Nothing else reads the list but its length.
//! - The consumer comes after the producer on one straight path, on which the only branches
//!   are tests that give up the whole call.
//! - Neither loop nor that path calls a function, so the only thing running the consumer's
//!   body earlier can change is which of two failures gives up the call, and every failure
//!   gives it up the same way.
//! - Moving the consumer's body into the producer changes no value either of them reads:
//!   neither assigns a variable the other reads or assigns.
//!
//! What the consumer needs before its first iteration, such as its own empty result list or a
//! starting state, moves to the end of the block that enters the producer. The list's length becomes a counter, so a
//! later `List.len` still has its answer.
//!
//! The pass runs before the reference counts are inserted, while a list is still a value and
//! `Push` and `Set` still return a new one.

use {
    crate::{
        ir::{Block, BlockId, Constant, Function, Inst, Local, Op, Terminator, Type},
        ownership::Bits,
    },
    smallvec::SmallVec,
};

/// How many lists one function may have fused away, which bounds the work the pass does on a
/// function it keeps finding lists in.
const MAX_FUSIONS: usize = 8;

/// How many blocks may lie between a producer and its consumer, and between where a list is
/// made and the loop that fills it.
const MAX_BETWEEN: usize = 8;

/// How many variables a list may pass through between the producer and the consumer.
const MAX_COPIES: usize = 8;

/// Fuses every producer of `function` with its consumer.
pub fn fuse_loops(function: &mut Function) {
    for _ in 0..MAX_FUSIONS {
        let steps = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(inst.op, Op::Step(..)))
            .count();
        if steps < 2 || !fuse_one(function) {
            break;
        }
    }
}

/// A loop as the lowerer emits it for a counter that runs from a start up to an end.
///
/// ```text
/// guard:   counter = Copy(start); ...; Branch { empty -> exit, otherwise: before }
/// before:  Jump(header)
/// header:  ... the body ...
/// latch:   ...; done = Same(counter, end); Branch { done -> exit, otherwise: step }
/// step:    ...; next = Step(counter, by); counter = Copy(next); Jump(header)
/// ```
#[derive(Clone, Copy)]
struct Loop {
    counter: Local,
    start: Local,
    end: Local,
    by: Local,
    guard: u32,
    before: u32,
    header: u32,
    latch: u32,
    step: u32,
    exit: u32,
}

/// Where an instruction is: its block and its index there.
type Site = (u32, u32);

/// How often each local is assigned, and where: its first and its last assignment.
struct Sites {
    counts: Vec<u32>,
    places: Vec<[Site; 2]>,
}

impl Sites {
    fn of(function: &Function) -> Sites {
        let mut sites = Sites {
            counts: vec![0; function.locals.len()],
            places: vec![[(0, 0); 2]; function.locals.len()],
        };
        for (block, body) in function.blocks.iter().enumerate() {
            for (index, inst) in body.insts.iter().enumerate() {
                for local in inst.assigned() {
                    let at = local.0 as usize;
                    let site = (block as u32, index as u32);
                    if sites.counts[at] == 0 {
                        sites.places[at][0] = site;
                    }
                    sites.places[at][1] = site;
                    sites.counts[at] += 1;
                }
            }
        }
        sites
    }

    /// Where `local` is assigned, when exactly one instruction assigns it.
    fn only(&self, local: Local) -> Option<Site> {
        (self.counts[local.0 as usize] == 1).then(|| self.places[local.0 as usize][0])
    }

    /// Where `local` is assigned, when exactly two instructions assign it.
    fn both(&self, local: Local) -> Option<[Site; 2]> {
        (self.counts[local.0 as usize] == 2).then(|| self.places[local.0 as usize])
    }
}

/// What the pass reads off a function before it changes anything.
struct Shape<'f> {
    function: &'f Function,
    sites: Sites,
    predecessors: Vec<SmallVec<[u32; 2]>>,
}

impl Shape<'_> {
    fn inst(&self, (block, index): Site) -> &Inst {
        &self.function.blocks[block as usize].insts[index as usize]
    }

    /// The one instruction that assigns `local`, and where it is.
    fn definition(&self, local: Local) -> Option<(Site, &Op)> {
        let site = self.sites.only(local)?;
        Some((site, &self.inst(site).op))
    }

    /// Whether `local` is always the `I64` one.
    fn is_one(&self, local: Local) -> bool {
        matches!(
            self.definition(local),
            Some((_, Op::Const(Constant::I64(1))))
        )
    }

    /// Whether `local` is always zero.
    fn is_zero(&self, local: Local) -> bool {
        matches!(
            self.definition(local),
            Some((_, Op::Const(Constant::I64(0) | Constant::Int(0))))
        )
    }

    fn terminator(&self, block: u32) -> &Terminator {
        &self.function.blocks[block as usize].terminator
    }
}

/// A list that one loop fills and another reads.
struct Fusion {
    producer: Loop,
    consumer: Loop,
    /// The blocks of each loop.
    producer_blocks: Bits,
    consumer_blocks: Bits,
    /// The blocks from the producer's exit to the consumer's guard, in order.
    between: SmallVec<[u32; 8]>,
    /// Every variable that holds the list, the result of the `Push` or `Set` among them.
    members: SmallVec<[Local; 8]>,
    /// The variable the producer fills.
    list: Local,
    /// Where the `Push` or `Set` is. The next instruction copies its result into `list`.
    fill: Site,
    /// The item the producer stores.
    item: Local,
    /// Where the empty list is made.
    creation: Site,
    /// For a list filled by `Set`: the list the producer counts over, whose length the filled
    /// list has.
    source: Option<Local>,
    /// The variable the consumer reads the list from.
    held: Local,
    /// Whether the consumer's body reads its counter for more than the item.
    reads_position: bool,
}

fn fuse_one(function: &mut Function) -> bool {
    let fused = {
        let shape = Shape {
            function,
            sites: Sites::of(function),
            predecessors: predecessors(function),
        };
        let loops = counted_loops(&shape);
        if loops.len() < 2 {
            return false;
        }
        let blocks: Vec<Option<Bits>> = loops
            .iter()
            .map(|counted| loop_blocks(&shape, counted))
            .collect();
        (0..loops.len()).find_map(|consumer| {
            let fusion = match_fusion(&shape, &loops, &blocks, consumer)?;
            rewrite(function, &fusion)
        })
    };
    match fused {
        Some(fused) => {
            *function = fused;
            true
        }
        None => false,
    }
}

fn predecessors(function: &Function) -> Vec<SmallVec<[u32; 2]>> {
    let mut predecessors = vec![SmallVec::new(); function.blocks.len()];
    for (index, block) in function.blocks.iter().enumerate() {
        for successor in block.terminator.successors() {
            let list: &mut SmallVec<[u32; 2]> = &mut predecessors[successor.0 as usize];
            if !list.contains(&(index as u32)) {
                list.push(index as u32);
            }
        }
    }
    predecessors
}

/// Every loop of the function that has the shape of [`Loop`].
fn counted_loops(shape: &Shape) -> SmallVec<[Loop; 4]> {
    let mut loops = SmallVec::new();
    for (step, block) in shape.function.blocks.iter().enumerate() {
        let step = step as u32;
        let Terminator::Jump(BlockId(header)) = block.terminator else {
            continue;
        };
        let [.., stepped, assigned] = &block.insts[..] else {
            continue;
        };
        let (&Op::Step(counter, by), Some(next)) = (&stepped.op, stepped.dest) else {
            continue;
        };
        if !matches!(assigned.op, Op::Copy(copied) if copied == next)
            || assigned.dest != Some(counter)
        {
            continue;
        }
        let last = block.insts.len() as u32 - 1;
        let Some(places) = shape.sites.both(counter) else {
            continue;
        };
        let Some(&start_site) = places.iter().find(|&&site| site != (step, last)) else {
            continue;
        };
        let Op::Copy(start) = shape.inst(start_site).op else {
            continue;
        };
        let guard = start_site.0;
        let [latch] = shape.predecessors[step as usize][..] else {
            continue;
        };
        let &Terminator::Branch {
            condition,
            then: BlockId(exit),
            otherwise: BlockId(continues),
        } = shape.terminator(latch)
        else {
            continue;
        };
        let Some(test) = shape.function.blocks[latch as usize].insts.last() else {
            continue;
        };
        let Op::Same(tested, end) = test.op else {
            continue;
        };
        if continues != step || test.dest != Some(condition) || tested != counter {
            continue;
        }
        let &Terminator::Branch {
            then: BlockId(skipped),
            otherwise: BlockId(before),
            ..
        } = shape.terminator(guard)
        else {
            continue;
        };
        let enters =
            matches!(shape.terminator(before), Terminator::Jump(BlockId(to)) if *to == header);
        let entries = &shape.predecessors[header as usize];
        if skipped != exit
            || !enters
            || !shape.function.blocks[before as usize].insts.is_empty()
            || shape.predecessors[before as usize][..] != [guard]
            || entries.len() != 2
            || !entries.contains(&before)
            || !entries.contains(&step)
        {
            continue;
        }
        loops.push(Loop {
            counter,
            start,
            end,
            by,
            guard,
            before,
            header,
            latch,
            step,
            exit,
        });
    }
    loops
}

/// The blocks of a loop: those its header reaches before the loop's exit. `None` when control
/// enters them anywhere but at the header, leaves them for anywhere but the exit, or returns
/// from the function among them.
fn loop_blocks(shape: &Shape, counted: &Loop) -> Option<Bits> {
    let mut blocks = Bits::new(shape.function.blocks.len());
    let mut pending: SmallVec<[u32; 8]> = SmallVec::from_slice(&[counted.header]);
    blocks.insert(counted.header as usize);
    while let Some(block) = pending.pop() {
        let terminator = shape.terminator(block);
        if matches!(terminator, Terminator::Return(_)) {
            return None;
        }
        for BlockId(successor) in terminator.successors() {
            if successor != counted.exit && !blocks.contains(successor as usize) {
                blocks.insert(successor as usize);
                pending.push(successor);
            }
        }
    }
    if blocks.contains(counted.guard as usize) || blocks.contains(counted.before as usize) {
        return None;
    }
    let enters_at_header = blocks.iter().all(|block| {
        block as u32 == counted.header
            || shape.predecessors[block]
                .iter()
                .all(|&from| blocks.contains(from as usize))
    });
    enters_at_header.then_some(blocks)
}

/// The list the loop `consumer` reads and the loop that fills it, when the two can be fused
/// as far as the function's shape tells. Whether their bodies get in each other's way is
/// checked once the list is gone ([`rewrite`]).
fn match_fusion(
    shape: &Shape,
    loops: &[Loop],
    blocks: &[Option<Bits>],
    consumer_index: usize,
) -> Option<Fusion> {
    let function = shape.function;
    let consumer = loops[consumer_index];
    let consumer_blocks = blocks[consumer_index].as_ref()?;

    // The consumer runs from the first position to the list's length and leaves only there.
    let (_, &Op::Copy(length)) = shape.definition(consumer.end)? else {
        return None;
    };
    let (_, &Op::Length(held)) = shape.definition(length)? else {
        return None;
    };
    let leaves = &shape.predecessors[consumer.exit as usize];
    if !shape.is_one(consumer.start)
        || !shape.is_one(consumer.by)
        || leaves.len() != 2
        || !leaves.contains(&consumer.guard)
        || !leaves.contains(&consumer.latch)
    {
        return None;
    }

    // From the variable the consumer reads back to the one the producer fills.
    let mut members: SmallVec<[Local; 8]> = SmallVec::new();
    let mut after: SmallVec<[Local; 8]> = SmallVec::new();
    let mut at = held;
    let (list, fills) = loop {
        if let Some(fills) = shape.sites.both(at) {
            break (at, fills);
        }
        let (_, &(Op::Copy(source) | Op::Relabel(source, _))) = shape.definition(at)? else {
            return None;
        };
        after.push(at);
        if after.len() > MAX_COPIES {
            return None;
        }
        at = source;
    };
    members.extend(after.iter().copied());
    members.push(list);

    // The list is assigned where it starts empty and where the producer has filled it.
    let fill_of = |site: Site| {
        let Op::Copy(filled) = shape.inst(site).op else {
            return None;
        };
        let (fill, op) = shape.definition(filled)?;
        let fills = matches!(*op, Op::Push(into, _) | Op::Set(into, _, _) if into == list);
        (fills && site == (fill.0, fill.1 + 1)).then_some((fill, filled))
    };
    let (first, (fill, filled)) = match (fill_of(fills[0]), fill_of(fills[1])) {
        (None, Some(found)) => (fills[0], found),
        (Some(found), None) => (fills[1], found),
        _ => return None,
    };
    members.push(filled);
    let Op::Copy(mut at) = shape.inst(first).op else {
        return None;
    };
    let mut before: SmallVec<[Site; 8]> = SmallVec::from_slice(&[first]);
    let creation = loop {
        let (site, op) = shape.definition(at)?;
        members.push(at);
        before.push(site);
        match *op {
            Op::NewVector(..) | Op::EmptyList(..) => break site,
            Op::Copy(source) | Op::Relabel(source, _) if before.len() <= MAX_COPIES => at = source,
            _ => return None,
        }
    };
    if members
        .iter()
        .any(|&member| !matches!(function.ty(member), Type::Vector { .. }))
    {
        return None;
    }

    // The producer is the loop whose body fills the list, and the list is made on the way
    // into it, once for each time the loop runs.
    let producer_index = (0..loops.len()).find(|&index| {
        index != consumer_index
            && blocks[index]
                .as_ref()
                .is_some_and(|blocks| blocks.contains(fill.0 as usize))
            && runs_into(shape, creation.0, loops[index].guard)
    })?;
    let producer = loops[producer_index];
    let producer_blocks = blocks[producer_index].as_ref()?;
    if !before
        .iter()
        .all(|site| runs_into(shape, site.0, producer.guard))
    {
        return None;
    }
    let (item, source) = match (&shape.inst(fill).op, &shape.inst(creation).op) {
        (&Op::Push(_, item), &Op::NewVector(_, length)) if shape.is_zero(length) => (item, None),
        (&Op::Push(_, item), &Op::EmptyList(_, capacity)) if never_negative(shape, capacity) => {
            (item, None)
        }
        (&Op::Set(_, position, item), &Op::NewVector(_, length))
            if creation.0 == producer.guard =>
        {
            let source = set_source(shape, &producer, producer_blocks, fill, creation, length)?;
            if position != producer.counter {
                return None;
            }
            (item, Some(source))
        }
        _ => return None,
    };
    let ends_in_producer = shape.predecessors[producer.exit as usize]
        .iter()
        .all(|&from| from == producer.guard || producer_blocks.contains(from as usize));
    if !ends_in_producer {
        return None;
    }

    // The consumer follows the producer on a straight path: jumps, and tests whose other side
    // gives up the call.
    let mut between: SmallVec<[u32; 8]> = SmallVec::from_slice(&[producer.exit]);
    let mut at = producer.exit;
    while at != consumer.guard {
        let next = match *shape.terminator(at) {
            Terminator::Jump(BlockId(next)) => next,
            Terminator::Branch {
                then: BlockId(then),
                otherwise: BlockId(otherwise),
                ..
            } if gives_up(function, then) => otherwise,
            _ => return None,
        };
        if shape.predecessors[next as usize][..] != [at] || between.len() == MAX_BETWEEN {
            return None;
        }
        between.push(next);
        at = next;
    }
    if between.iter().any(|&block| {
        producer_blocks.contains(block as usize) || consumer_blocks.contains(block as usize)
    }) {
        return None;
    }
    let assigned_between = |local: Local| {
        shape
            .sites
            .only(local)
            .is_some_and(|site| between.contains(&site.0))
    };
    if !after.iter().all(|&member| assigned_between(member)) {
        return None;
    }

    // Nothing reads the list but the consumer, item by item, and a length taken outside both
    // loops. The consumer's counter means nothing outside its loop.
    let mut reads_items = false;
    let mut reads_position = false;
    for (block, body) in function.blocks.iter().enumerate() {
        let in_producer = producer_blocks.contains(block);
        let in_consumer = consumer_blocks.contains(block);
        let on_path = between.contains(&(block as u32));
        if let Terminator::Return(value) = body.terminator
            && members.contains(&value)
        {
            return None;
        }
        for (index, inst) in body.insts.iter().enumerate() {
            let site = (block as u32, index as u32);
            if (in_producer || in_consumer || on_path)
                && matches!(inst.op, Op::CallR(..) | Op::CallTyped(..))
            {
                return None;
            }
            let item_read =
                matches!(inst.op, Op::ItemAt(from, at) if from == held && at == consumer.counter);
            let reads_counter = inst.op.operands().contains(&consumer.counter);
            if reads_counter && !in_consumer {
                return None;
            }
            if in_consumer && item_read {
                reads_items = true;
            } else if reads_counter
                && site != (consumer.latch, body.insts.len() as u32 - 1)
                && block as u32 != consumer.step
            {
                reads_position = true;
            }
            if !inst.op.operands().iter().any(|read| members.contains(read)) {
                continue;
            }
            let allowed = match inst.op {
                Op::Copy(_) | Op::Relabel(..) => {
                    inst.dest.is_some_and(|dest| members.contains(&dest))
                }
                Op::Push(..) | Op::Set(..) => site == fill,
                Op::ItemAt(..) => in_consumer && item_read,
                Op::Length(_) => !in_producer && !in_consumer,
                _ => false,
            };
            if !allowed {
                return None;
            }
        }
    }
    if !reads_items {
        return None;
    }
    let item_type = function.ty(item);
    let read_as_stored = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|inst| matches!(inst.op, Op::ItemAt(from, _) if from == held))
        .all(|inst| inst.dest.is_none_or(|dest| function.ty(dest) == item_type));
    if !read_as_stored {
        return None;
    }

    Some(Fusion {
        producer,
        consumer,
        producer_blocks: producer_blocks.clone(),
        consumer_blocks: consumer_blocks.clone(),
        between,
        members,
        list,
        fill,
        item,
        creation,
        source,
        held,
        reads_position,
    })
}

/// Whether control reaches the block `to` only by way of the block `from`, along blocks that
/// each have one way in.
fn runs_into(shape: &Shape, from: u32, to: u32) -> bool {
    let mut at = to;
    for _ in 0..MAX_BETWEEN {
        if at == from {
            return true;
        }
        let [before] = shape.predecessors[at as usize][..] else {
            return false;
        };
        at = before;
    }
    at == from
}

/// Whether `count` is known not to be negative, so that making a list with room for that many
/// items cannot fail, and the call still succeeds once the list is gone.
fn never_negative(shape: &Shape, count: Local) -> bool {
    match shape.definition(count) {
        Some((_, &Op::Const(Constant::Int(value)))) => value >= 0,
        Some((_, &Op::Const(Constant::I64(value)))) => value >= 0,
        Some((_, Op::Length(_) | Op::CountOfI64(_))) => true,
        _ => false,
    }
}

/// Whether `block` only gives up the call.
fn gives_up(function: &Function, block: u32) -> bool {
    let block = &function.blocks[block as usize];
    block.insts.is_empty() && matches!(block.terminator, Terminator::Fail)
}

/// For a list the producer fills with `Set` at its counter: the list the producer counts
/// over, when the filled list is made with that list's length and every iteration sets one
/// item, from the first to the last.
fn set_source(
    shape: &Shape,
    producer: &Loop,
    producer_blocks: &Bits,
    fill: Site,
    creation: Site,
    length: Local,
) -> Option<Local> {
    if !shape.is_one(producer.start) || !shape.is_one(producer.by) {
        return None;
    }
    // The loop leaves only where it has set the last item.
    let leaves = &shape.predecessors[producer.exit as usize];
    if leaves.len() != 2 || !leaves.contains(&producer.guard) || !leaves.contains(&producer.latch) {
        return None;
    }
    // The `Set` runs once per iteration: its block reaches the latch's test by jumps alone,
    // and nothing else jumps into the blocks on the way.
    let mut at = producer.latch;
    while at != fill.0 {
        let [from] = shape.predecessors[at as usize][..] else {
            return None;
        };
        if !matches!(shape.terminator(from), Terminator::Jump(_))
            || !producer_blocks.contains(from as usize)
        {
            return None;
        }
        at = from;
    }
    // The new list and the loop's end are both the length of one list, read while that list
    // cannot have been assigned in between: on the straight path into the guard.
    let (counted_at, &Op::Length(source)) = shape.definition(length)? else {
        return None;
    };
    let (_, &Op::Copy(end)) = shape.definition(producer.end)? else {
        return None;
    };
    let (end_at, &Op::Length(ended)) = shape.definition(end)? else {
        return None;
    };
    let guard = producer.guard;
    let straight =
        counted_at.0 == guard || shape.predecessors[guard as usize][..] == [counted_at.0];
    if ended != source || !straight || end_at.0 != guard || end_at.1 > creation.1 {
        return None;
    }
    let assigned_before = |site: Site, read: Site| {
        if site.0 == read.0 {
            site.1 < read.1
        } else {
            site.0 != guard
        }
    };
    let stable = shape.function.is_param(source)
        || shape
            .sites
            .only(source)
            .is_some_and(|site| assigned_before(site, counted_at) && assigned_before(site, end_at));
    stable.then_some(source)
}

/// The function with the list of `fusion` gone and its two loops one, or `None` when the
/// loops' bodies read or assign each other's variables.
///
/// The list goes first. Each variable that held it becomes its length, an `I64`, and for a
/// list filled by `Set`, whose length is its source's, also that length as an R integer, so
/// that no length needs a conversion that can fail. The consumer reads its item from a fresh
/// variable and stops counting. Then the checks run on the rewritten instructions, and last
/// the blocks are joined up: the producer's block splits after the item is stored, the first
/// half enters the consumer's body, and the consumer's latch continues in the second half.
fn rewrite(original: &Function, fusion: &Fusion) -> Option<Function> {
    let mut function = original.clone();
    let Fusion {
        producer, consumer, ..
    } = fusion;
    let with_integers = fusion.source.is_some();
    let first_length = function.locals.len() as u32;
    let per_member = if with_integers { 2 } else { 1 };
    for _ in &fusion.members {
        function.locals.push(Type::I64);
        if with_integers {
            function.locals.push(Type::Int);
        }
    }
    let length = |member: Local, integer: bool| {
        fusion
            .members
            .iter()
            .position(|&held| held == member)
            .map(|index| Local(first_length + index as u32 * per_member + integer as u32))
    };
    let item_type = function.ty(fusion.item);
    function.locals.push(item_type);
    let item = Local(function.locals.len() as u32 - 1);
    function.locals.push(Type::I64);
    let one = Local(function.locals.len() as u32 - 1);

    // Where the producer's block splits, as an index into its rewritten instructions.
    let mut split = 0;
    for (block, body) in function.blocks.iter_mut().enumerate() {
        let block = block as u32;
        let in_consumer = fusion.consumer_blocks.contains(block as usize);
        let old = std::mem::take(&mut body.insts);
        let count = old.len() as u32;
        body.insts.reserve(old.len());
        for (index, inst) in old.into_iter().enumerate() {
            let site = (block, index as u32);
            let copies_fill = site == (fusion.fill.0, fusion.fill.1 + 1);
            let leaves_consumer = (block == consumer.latch && site.1 + 1 == count)
                || (block == consumer.step && site.1 + 2 >= count)
                || (block == consumer.header && index == 0 && matches!(inst.op, Op::Poll));
            if leaves_consumer {
                continue;
            }
            let dest = inst.dest;
            let dest_length = |integer| dest.and_then(|dest| length(dest, integer));
            match inst.op {
                Op::NewVector(..) | Op::EmptyList(..) if site == fusion.creation => {
                    match fusion.source {
                        Some(source) => {
                            for integer in [false, true] {
                                body.insts
                                    .push(Inst::new(dest_length(integer), Op::Length(source)));
                            }
                        }
                        None => body
                            .insts
                            .push(Inst::new(dest_length(false), Op::Const(Constant::I64(0)))),
                    }
                }
                Op::Push(list, _) if site == fusion.fill => {
                    body.insts
                        .push(Inst::new(Some(one), Op::Const(Constant::I64(1))));
                    let grown = Op::Step(length(list, false)?, one);
                    body.insts.push(Inst::new(dest_length(false), grown));
                }
                // A `Set` leaves the length as it is, so the list's lengths are not assigned
                // in the loop, and what the consumer needs of them can move in front of it.
                Op::Set(..) if site == fusion.fill => {}
                Op::Copy(_) if copies_fill && fusion.source.is_some() => {}
                Op::Copy(source) | Op::Relabel(source, _)
                    if inst.dest.is_some_and(|dest| fusion.members.contains(&dest)) =>
                {
                    for integer in [false, true].into_iter().take(per_member as usize) {
                        let copy = Op::Copy(length(source, integer)?);
                        body.insts.push(Inst::new(dest_length(integer), copy));
                    }
                }
                Op::Length(list) if fusion.members.contains(&list) => {
                    let whole = inst.dest.is_some_and(|dest| original.ty(dest) == Type::I64);
                    let op = match (whole, with_integers) {
                        (true, _) => Op::Copy(length(list, false)?),
                        (false, true) => Op::Copy(length(list, true)?),
                        (false, false) => Op::CountOfI64(length(list, false)?),
                    };
                    body.insts.push(Inst::new(inst.dest, op));
                }
                Op::ItemAt(from, _) if in_consumer && from == fusion.held => {
                    body.insts.push(Inst::new(inst.dest, Op::Copy(item)));
                }
                _ => body.insts.push(inst),
            }
            if copies_fill {
                split = body.insts.len();
            }
        }
    }

    // What each loop reads and assigns, and what the consumer needs to be set before it runs.
    let locals = function.locals.len();
    let sets = |blocks: &Bits| {
        let (mut reads, mut writes) = (Bits::new(locals), Bits::new(locals));
        for block in blocks.iter() {
            let body = &function.blocks[block];
            for inst in &body.insts {
                for read in inst.op.operands() {
                    reads.insert(read.0 as usize);
                }
                for written in inst.assigned() {
                    writes.insert(written.0 as usize);
                }
            }
            if let Terminator::Branch { condition, .. } = body.terminator {
                reads.insert(condition.0 as usize);
            }
        }
        (reads, writes)
    };
    let (mut producer_reads, producer_writes) = sets(&fusion.producer_blocks);
    let (mut consumer_reads, consumer_writes) = sets(&fusion.consumer_blocks);
    producer_reads.insert(fusion.item.0 as usize);
    // The consumer's item and position are assigned for it just before its body runs, so
    // reading them is no reading of the producer's variables.
    consumer_reads.remove(item.0 as usize);
    consumer_reads.remove(consumer.counter.0 as usize);
    if consumer_writes.intersects(&producer_reads)
        || consumer_writes.intersects(&producer_writes)
        || consumer_reads.intersects(&producer_writes)
    {
        return None;
    }

    // The instructions between the loops that assign what the consumer reads or assigns move
    // in front of the producer, with the instructions between the loops they read from.
    let mut needed = consumer_reads.clone();
    needed.union(&consumer_writes);
    let mut moved: SmallVec<[Site; 16]> = SmallVec::new();
    for &block in fusion.between.iter().rev() {
        let body = &function.blocks[block as usize];
        for (index, inst) in body.insts.iter().enumerate().rev() {
            if inst
                .assigned()
                .iter()
                .any(|assigned| needed.contains(assigned.0 as usize))
            {
                moved.push((block, index as u32));
                for read in inst.op.operands() {
                    needed.insert(read.0 as usize);
                }
            }
        }
    }
    moved.reverse();

    // A moved instruction runs before the producer and before what came ahead of it between
    // the loops, so it must not read what those assign or assign what those read. One that
    // stays runs after the consumer, so it must not read what the consumer assigns.
    let (mut stay_reads, mut stay_writes) = (Bits::new(locals), Bits::new(locals));
    let guard_test = match function.blocks[producer.guard as usize].terminator {
        Terminator::Branch { condition, .. } => condition,
        _ => return None,
    };
    for &block in &fusion.between {
        let body = &function.blocks[block as usize];
        for (index, inst) in body.insts.iter().enumerate() {
            let reads = inst.op.operands();
            let writes = inst.assigned();
            let read = |set: &Bits| reads.iter().any(|local| set.contains(local.0 as usize));
            let written = |set: &Bits| writes.iter().any(|local| set.contains(local.0 as usize));
            if moved.contains(&(block, index as u32)) {
                let has_effects = matches!(
                    inst.op,
                    Op::CallR(..) | Op::CallTyped(..) | Op::Poll | Op::Dup(_) | Op::Drop(_)
                );
                if has_effects
                    || read(&producer_writes)
                    || read(&stay_writes)
                    || written(&producer_reads)
                    || written(&producer_writes)
                    || written(&stay_reads)
                    || written(&stay_writes)
                    || writes.contains(&guard_test)
                {
                    return None;
                }
            } else {
                if read(&consumer_writes) {
                    return None;
                }
                for local in reads {
                    stay_reads.insert(local.0 as usize);
                }
                for local in writes {
                    stay_writes.insert(local.0 as usize);
                }
            }
        }
        if let Terminator::Branch { condition, .. } = body.terminator {
            if consumer_writes.contains(condition.0 as usize) {
                return None;
            }
            stay_reads.insert(condition.0 as usize);
        }
    }

    // The checks passed: move what the consumer needs, and join the blocks.
    let mut hoisted: Vec<Inst> = Vec::with_capacity(moved.len());
    for &block in &fusion.between {
        let body = &mut function.blocks[block as usize];
        let old = std::mem::take(&mut body.insts);
        for (index, inst) in old.into_iter().enumerate() {
            if moved.contains(&(block, index as u32)) {
                hoisted.push(inst);
            } else {
                body.insts.push(inst);
            }
        }
        // The producer's block cannot be one of these, so `split` still points into it.
    }
    function.blocks[producer.guard as usize]
        .insts
        .extend(hoisted);

    let filling = &mut function.blocks[fusion.fill.0 as usize];
    let rest = filling.insts.split_off(split);
    filling
        .insts
        .push(Inst::new(Some(item), Op::Copy(fusion.item)));
    if fusion.reads_position {
        // The consumer's position is the producer's for a list set at each position, and the
        // number of items pushed so far for a list that grows.
        let position = match fusion.source {
            Some(_) => producer.counter,
            None => length(fusion.list, false)?,
        };
        filling
            .insts
            .push(Inst::new(Some(consumer.counter), Op::Copy(position)));
    }
    let continues = std::mem::replace(
        &mut filling.terminator,
        Terminator::Jump(BlockId(consumer.header)),
    );
    function.blocks.push(Block {
        insts: rest,
        terminator: continues,
    });
    let second_half = BlockId(function.blocks.len() as u32 - 1);
    function.blocks[consumer.latch as usize].terminator = Terminator::Jump(BlockId(consumer.step));
    function.blocks[consumer.step as usize].terminator = Terminator::Jump(second_half);
    function.blocks[consumer.guard as usize].terminator = Terminator::Jump(BlockId(consumer.exit));
    // Nothing reaches the block that entered the consumer's loop any more. It must not stay
    // a way into the producer's loop, which now holds the consumer's header.
    function.blocks[consumer.before as usize].terminator = Terminator::Fail;
    Some(function)
}
