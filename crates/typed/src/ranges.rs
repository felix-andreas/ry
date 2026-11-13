//! The range of values each whole-number local can hold, so the backend can leave out a check
//! that cannot fire.
//!
//! ```r
//! for (i in List.range(1, n)) {
//!     quantity <- i %% 7 + 1
//! }
//! ```
//!
//! The counter starts at 1 and only counts up, so it is never negative, which means `i %% 7`
//! needs no correction for a negative dividend and lies in `0..=6`. Adding 1 to that cannot
//! overflow, so the sum needs no overflow test either.
//!
//! A range covers every value any assignment gives the local, anywhere in the function. Each
//! assignment's range follows from the ranges of its operands, and the ranges are the smallest
//! that agree with every assignment, found by starting from nothing and growing until nothing
//! changes. A bound that keeps growing, such as a counter's upper end, jumps to the limit of
//! its type after a few rounds, which bounds the work.
//!
//! A test narrows a range for the code it guards. In
//!
//! ```r
//! fib <- function(n: I64) -> I64 if (n < 2) n else fib(n - 1) + fib(n - 2)
//! ```
//!
//! `n` is at least 2 in the `else` branch, so neither subtraction can overflow. A test counts
//! when a block ends by branching on a comparison of two `I64` locals made in that block, each
//! local is assigned at most once in the function, and the branch is the only way into its
//! target. The comparison then still describes both locals in every block the target
//! dominates: a local assigned once changes only when its one assignment runs again, and the
//! way from there back into such a block leads through the comparison and the branch.
//!
//! The machine code starts every local that is not a parameter at zero, so a local that some
//! path reads before assigning it has 0 in its range as well.
//!
//! The types set the widest range: all of `i64` for an `I64`, all of `i32` for an integer,
//! where the lowest value is `NA`, 0 and 1 for a `Bool`, and those two and `NA` for a logical.
//! An integer or logical whose range starts above the lowest `i32` is therefore never `NA`.

use {
    crate::{
        ir::{Arith, Compare, Constant, Function, Inst, Item, Local, Op, Terminator, Type},
        ownership::Bits,
    },
    smallvec::SmallVec,
};

/// `NA` of an integer or a logical.
pub const NA: i64 = i32::MIN as i64;

/// Every value from `lo` to `hi`. A range with `lo > hi` holds no value: no assignment of the
/// local was reached, or the tests around a block contradict each other, so the block never
/// runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
    pub lo: i64,
    pub hi: i64,
}

impl Range {
    pub const EMPTY: Range = Range {
        lo: i64::MAX,
        hi: i64::MIN,
    };

    /// The widest range a local of this type has. A type whose values are not whole numbers
    /// gets all of `i64`, which says nothing.
    pub fn of_type(ty: Type) -> Range {
        let (lo, hi) = match ty {
            Type::Int => (NA, i64::from(i32::MAX)),
            Type::Lgl => (NA, 1),
            Type::Bool => (0, 1),
            _ => (i64::MIN, i64::MAX),
        };
        Range { lo, hi }
    }

    pub fn is_empty(self) -> bool {
        self.lo > self.hi
    }

    /// The smallest range that covers both.
    fn join(self, other: Range) -> Range {
        Range {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    fn meet(self, other: Range) -> Range {
        Range {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
        }
    }
}

/// The bounds of `x op y` for `+`, `-` and `*` as plain whole numbers, before any overflow. An
/// operation whose bounds lie within `i64` cannot overflow. Neither range may be empty.
pub fn exact_bounds(op: Arith, x: Range, y: Range) -> Option<(i128, i128)> {
    let (x_lo, x_hi) = (i128::from(x.lo), i128::from(x.hi));
    let (y_lo, y_hi) = (i128::from(y.lo), i128::from(y.hi));
    Some(match op {
        Arith::Add => (x_lo + y_lo, x_hi + y_hi),
        Arith::Sub => (x_lo - y_hi, x_hi - y_lo),
        Arith::Mul => {
            let products = [x_lo * y_lo, x_lo * y_hi, x_hi * y_lo, x_hi * y_hi];
            let lowest = products.into_iter().min()?;
            let highest = products.into_iter().max()?;
            (lowest, highest)
        }
        _ => return None,
    })
}

/// The ranges of a function's locals: for every local the range over the whole function, and
/// for every block the tests that hold in it.
pub struct Ranges {
    locals: Vec<Range>,
    /// For each block the innermost test that holds in it, as an index into `tests`, or `NONE`.
    scopes: Vec<u32>,
    tests: Vec<Test>,
}

/// `left op right` holds, in the blocks whose scope reaches this test.
struct Test {
    op: Compare,
    left: Local,
    right: Local,
    /// The next test out that holds in the same blocks, or `NONE`.
    outer: u32,
}

const NONE: u32 = u32::MAX;

impl Ranges {
    /// The range of `local` where an instruction of block `block` reads it.
    pub fn of(&self, block: usize, local: Local) -> Range {
        let mut range = self.locals[local.0 as usize];
        let mut scope = self.scopes[block];
        while let Some(test) = self.tests.get(scope as usize) {
            if test.left == local {
                range = narrowed(range, test.op, self.locals[test.right.0 as usize]);
            } else if test.right == local {
                range = narrowed(range, mirrored(test.op), self.locals[test.left.0 as usize]);
            }
            scope = test.outer;
        }
        range
    }
}

/// The part of `range` whose values can stand in `value op other` for some value of `other`.
fn narrowed(range: Range, op: Compare, other: Range) -> Range {
    if other.is_empty() {
        return Range::EMPTY;
    }
    let bound = match op {
        Compare::Eq => other,
        // Only a range of one value rules a value out.
        Compare::Ne => return range,
        Compare::Lt => Range {
            lo: i64::MIN,
            hi: other.hi.saturating_sub(1),
        },
        Compare::Le => Range {
            lo: i64::MIN,
            hi: other.hi,
        },
        Compare::Gt => Range {
            lo: other.lo.saturating_add(1),
            hi: i64::MAX,
        },
        Compare::Ge => Range {
            lo: other.lo,
            hi: i64::MAX,
        },
    };
    range.meet(bound)
}

/// The comparison with its operands swapped.
fn mirrored(op: Compare) -> Compare {
    match op {
        Compare::Eq | Compare::Ne => op,
        Compare::Lt => Compare::Gt,
        Compare::Le => Compare::Ge,
        Compare::Gt => Compare::Lt,
        Compare::Ge => Compare::Le,
    }
}

/// The comparison that holds exactly when `op` does not.
fn negated(op: Compare) -> Compare {
    match op {
        Compare::Eq => Compare::Ne,
        Compare::Ne => Compare::Eq,
        Compare::Lt => Compare::Ge,
        Compare::Le => Compare::Gt,
        Compare::Gt => Compare::Le,
        Compare::Ge => Compare::Lt,
    }
}

/// How many rounds a bound may grow before it jumps to the limit of its type.
const ROUNDS_BEFORE_LIMIT: u32 = 3;

/// How many rounds take back what the jump to the limit overshot.
const TIGHTENING_ROUNDS: u32 = 3;

pub fn ranges(function: &Function) -> Ranges {
    let order = reverse_postorder(function);
    let sites = assignment_sites(function);
    let (scopes, tests) = branch_tests(function, &order, &sites);
    // What a local holds without any assignment: a parameter its argument, and a local some
    // path reads before assigning it the zero it starts as.
    let mut given = vec![Range::EMPTY; function.locals.len()];
    for (param, &ty) in function.locals[..function.params].iter().enumerate() {
        given[param] = Range::of_type(ty);
    }
    for local in read_before_assignment(function, &order).iter() {
        given[local] = Range { lo: 0, hi: 0 };
    }
    let mut ranges = Ranges {
        locals: given.clone(),
        scopes,
        tests,
    };

    let mut round = 0;
    loop {
        let mut changed = false;
        for (block, body) in function.blocks.iter().enumerate() {
            for inst in &body.insts {
                for dest in inst.assigned() {
                    let assigned = assigned_range(function, &ranges, block, inst, dest);
                    if assigned.is_empty() {
                        continue;
                    }
                    let known = &mut ranges.locals[dest.0 as usize];
                    let mut grown = assigned.join(*known);
                    if grown == *known {
                        continue;
                    }
                    if round >= ROUNDS_BEFORE_LIMIT && !known.is_empty() {
                        let widest = Range::of_type(function.ty(dest));
                        if grown.lo < known.lo {
                            grown.lo = widest.lo;
                        }
                        if grown.hi > known.hi {
                            grown.hi = widest.hi;
                        }
                    }
                    *known = grown;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
        round += 1;
    }

    // The ranges now agree with every assignment, but a bound that jumped to its limit took
    // along the bounds computed from it, such as `i %% 7` of a counter `i`. Computing every
    // range again from the ranges found keeps them in agreement and takes that back. A local
    // with one assignment gets its new range at once, so a chain of such locals tightens in
    // one round, and a local with several gets the range that covers all of them at the end
    // of the round.
    for _ in 0..TIGHTENING_ROUNDS {
        let mut changed = false;
        let mut covering = given.clone();
        for (block, body) in function.blocks.iter().enumerate() {
            for inst in &body.insts {
                for dest in inst.assigned() {
                    let assigned = assigned_range(function, &ranges, block, inst, dest);
                    if assigned.is_empty() {
                        continue;
                    }
                    let index = dest.0 as usize;
                    if assigned_once(sites[index]) && given[index].is_empty() {
                        let known = &mut ranges.locals[index];
                        let tightened = assigned.meet(*known);
                        changed |= tightened != *known;
                        *known = tightened;
                    } else {
                        covering[index] = assigned.join(covering[index]);
                    }
                }
            }
        }
        for (index, covering) in covering.into_iter().enumerate() {
            if covering.is_empty() || (assigned_once(sites[index]) && given[index].is_empty()) {
                continue;
            }
            let known = &mut ranges.locals[index];
            let tightened = covering.meet(*known);
            changed |= tightened != *known;
            *known = tightened;
        }
        if !changed {
            break;
        }
    }

    // A local no reached assignment gives a value is only read by code that never runs, and
    // the widest range is right for any code.
    for (local, range) in ranges.locals.iter_mut().enumerate() {
        if range.is_empty() {
            *range = Range::of_type(function.locals[local]);
        }
    }
    ranges
}

/// The range of the value `inst`, an instruction of block `block`, assigns `dest`. It is empty
/// while an operand has no range yet, and when the instruction always gives up.
fn assigned_range(
    function: &Function,
    ranges: &Ranges,
    block: usize,
    inst: &Inst,
    dest: Local,
) -> Range {
    let widest = Range::of_type(function.ty(dest));
    if inst.dest != Some(dest) {
        return widest;
    }
    let whole = |local: Local| {
        matches!(
            function.ty(local),
            Type::I64 | Type::Int | Type::Lgl | Type::Bool
        )
    };
    let of = |local: Local| ranges.of(block, local);
    let range = match inst.op {
        Op::Const(constant) => match constant {
            Constant::Int(value) | Constant::Lgl(value) => {
                let value = i64::from(value);
                Range {
                    lo: value,
                    hi: value,
                }
            }
            Constant::I64(value) => Range {
                lo: value,
                hi: value,
            },
            _ => widest,
        },
        Op::Copy(source) | Op::LglToInt(source) | Op::Truth(source) if whole(source) => of(source),
        Op::ToI64(source) if whole(source) => {
            let source_range = of(source);
            if function.ty(source) == Type::Int && !source_range.is_empty() {
                // The conversion gives up on `NA`.
                Range {
                    lo: source_range.lo.max(NA + 1),
                    hi: source_range.hi,
                }
            } else {
                source_range
            }
        }
        // The conversion gives up outside the counts an integer holds.
        Op::CountOfI64(source) => of(source).meet(Range {
            lo: 0,
            hi: i64::from(i32::MAX),
        }),
        // A length written to an integer is truncated, so only a wide destination is known
        // to hold a count.
        Op::Length(_) | Op::ByteCount(_) if function.ty(dest) == Type::I64 => Range {
            lo: 0,
            hi: widest.hi,
        },
        Op::Step(a, b) => {
            let (x, y) = (of(a), of(b));
            if x.is_empty() || y.is_empty() {
                return Range::EMPTY;
            }
            Range {
                lo: x.lo.saturating_add(y.lo),
                hi: x.hi.saturating_add(y.hi),
            }
        }
        Op::Arith(op, Item::I64, a, b) => {
            let (x, y) = (of(a), of(b));
            if x.is_empty() || y.is_empty() {
                return Range::EMPTY;
            }
            match op {
                Arith::Add | Arith::Sub | Arith::Mul => match exact_bounds(op, x, y) {
                    // The operation gives up when it overflows, so the result is in `i64`.
                    Some((lo, hi)) => Range {
                        lo: lo.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
                        hi: hi.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
                    },
                    None => widest,
                },
                // `%%` rounds toward the divisor's sign, so the result has that sign and is
                // closer to zero than the divisor, and a dividend that is not negative never
                // grows.
                Arith::Mod if y.lo > 0 => Range {
                    lo: 0,
                    hi: if x.lo >= 0 {
                        x.hi.min(y.hi - 1)
                    } else {
                        y.hi - 1
                    },
                },
                Arith::Mod if y.hi < 0 => Range {
                    lo: y.lo + 1,
                    hi: 0,
                },
                Arith::IntDiv if y.lo > 0 && x.lo >= 0 => Range {
                    lo: x.lo / y.hi,
                    hi: x.hi / y.lo,
                },
                Arith::Min => Range {
                    lo: x.lo.min(y.lo),
                    hi: x.hi.min(y.hi),
                },
                Arith::Max => Range {
                    lo: x.lo.max(y.lo),
                    hi: x.hi.max(y.hi),
                },
                _ => widest,
            }
        }
        _ => widest,
    };
    range.meet(widest)
}

/// Where each local's one assignment is, as a block and an instruction in it, or `UNASSIGNED`
/// or `REASSIGNED` for a local with none or several.
fn assignment_sites(function: &Function) -> Vec<(u32, u32)> {
    let mut sites = vec![UNASSIGNED; function.locals.len()];
    for (block, body) in function.blocks.iter().enumerate() {
        for (index, inst) in body.insts.iter().enumerate() {
            for dest in inst.assigned() {
                let site = &mut sites[dest.0 as usize];
                *site = if *site == UNASSIGNED {
                    (block as u32, index as u32)
                } else {
                    REASSIGNED
                };
            }
        }
    }
    sites
}

const UNASSIGNED: (u32, u32) = (u32::MAX, 0);
const REASSIGNED: (u32, u32) = (u32::MAX, 1);

fn assigned_once(site: (u32, u32)) -> bool {
    site.0 != u32::MAX
}

/// The tests that narrow ranges: for each block the innermost test that holds in it, and the
/// tests themselves, each linked to the next one out.
fn branch_tests(function: &Function, order: &[u32], sites: &[(u32, u32)]) -> (Vec<u32>, Vec<Test>) {
    let blocks = function.blocks.len();
    let mut scopes = vec![NONE; blocks];
    let mut tests = Vec::new();

    let mut entries = vec![0u32; blocks];
    let mut candidates = false;
    for body in &function.blocks {
        for successor in body.terminator.successors() {
            entries[successor.0 as usize] += 1;
        }
        candidates |= matches!(body.terminator, Terminator::Branch { .. });
    }
    if !candidates {
        return (scopes, tests);
    }

    let mut position = vec![u32::MAX; blocks];
    for (index, &block) in order.iter().enumerate() {
        position[block as usize] = index as u32;
    }
    let dominators = immediate_dominators(function, order, &position);
    let dominates = |above: u32, mut below: u32| {
        while position[below as usize] > position[above as usize] {
            below = dominators[below as usize];
        }
        below == above
    };

    // A block's dominator comes before it in this order, so its scope is already known.
    for &block in order {
        let dominator = dominators[block as usize];
        if dominator != block {
            scopes[block as usize] = scopes[dominator as usize];
        }
        if entries[block as usize] != 1 || dominator == block {
            continue;
        }
        // The one way in is from the dominator, since every path to the block ends with it.
        let from = &function.blocks[dominator as usize];
        let Terminator::Branch {
            condition,
            then,
            otherwise,
        } = from.terminator
        else {
            continue;
        };
        if then == otherwise {
            continue;
        }
        let (site_block, site_index) = sites[condition.0 as usize];
        if site_block != dominator {
            continue;
        }
        let Some(&Inst {
            op: Op::Compare(op, Item::I64, left, right),
            ..
        }) = from.insts.get(site_index as usize)
        else {
            continue;
        };
        // The operand holds the value the comparison read for as long as its one assignment
        // does not run again, and that assignment lies on the way to the comparison.
        let settled = |local: Local| {
            let (block, index) = sites[local.0 as usize];
            if (block, index) == UNASSIGNED {
                function.is_param(local)
            } else if (block, index) == REASSIGNED {
                false
            } else if block == dominator {
                index < site_index
            } else {
                position[block as usize] != u32::MAX && dominates(block, dominator)
            }
        };
        if !settled(left) || !settled(right) {
            continue;
        }
        let op = if then.0 == block { op } else { negated(op) };
        tests.push(Test {
            op,
            left,
            right,
            outer: scopes[block as usize],
        });
        scopes[block as usize] = tests.len() as u32 - 1;
    }
    (scopes, tests)
}

/// The locals some path reads before it assigns them, which therefore read as zero there.
fn read_before_assignment(function: &Function, order: &[u32]) -> Bits {
    let locals = function.locals.len();
    // The locals a path to the start of each block may not have assigned yet.
    let mut entering = vec![Bits::new(locals); function.blocks.len()];
    if let Some(entry) = entering.first_mut() {
        for local in function.params..locals {
            entry.insert(local);
        }
    }
    let mut read = Bits::new(locals);
    // The sets only grow, so the last round, which changes none of them, sees each block with
    // its final set and records every such read.
    let mut changed = true;
    while changed {
        changed = false;
        for &block in order {
            let body = &function.blocks[block as usize];
            let mut open = entering[block as usize].clone();
            for inst in &body.insts {
                for operand in inst.op.operands() {
                    if open.contains(operand.0 as usize) {
                        read.insert(operand.0 as usize);
                    }
                }
                for dest in inst.assigned() {
                    open.remove(dest.0 as usize);
                }
            }
            if let Terminator::Branch {
                condition: local, ..
            }
            | Terminator::Return(local) = body.terminator
                && open.contains(local.0 as usize)
            {
                read.insert(local.0 as usize);
            }
            for successor in body.terminator.successors() {
                let target = &mut entering[successor.0 as usize];
                if !target.covers(&open) {
                    target.union(&open);
                    changed = true;
                }
            }
        }
    }
    read
}

/// The blocks the entry reaches, each after every block that leads to it except along a loop's
/// back edge.
fn reverse_postorder(function: &Function) -> Vec<u32> {
    let mut order = Vec::with_capacity(function.blocks.len());
    let mut visited = vec![false; function.blocks.len()];
    let mut stack: SmallVec<[(u32, u32); 16]> = SmallVec::new();
    if function.blocks.is_empty() {
        return order;
    }
    visited[0] = true;
    stack.push((0, 0));
    while let Some(&mut (block, ref mut next)) = stack.last_mut() {
        let successors = function.blocks[block as usize].terminator.successors();
        match successors.get(*next as usize) {
            Some(successor) => {
                *next += 1;
                if !visited[successor.0 as usize] {
                    visited[successor.0 as usize] = true;
                    stack.push((successor.0, 0));
                }
            }
            None => {
                order.push(block);
                stack.pop();
            }
        }
    }
    order.reverse();
    order
}

/// Each reached block's immediate dominator, and the entry's own number for the entry. A block
/// the entry does not reach keeps its own number too.
fn immediate_dominators(function: &Function, order: &[u32], position: &[u32]) -> Vec<u32> {
    const UNKNOWN: u32 = u32::MAX;
    let blocks = function.blocks.len();
    let mut predecessors: Vec<SmallVec<[u32; 2]>> = vec![SmallVec::new(); blocks];
    for &block in order {
        for successor in function.blocks[block as usize].terminator.successors() {
            predecessors[successor.0 as usize].push(block);
        }
    }
    let mut dominators = vec![UNKNOWN; blocks];
    if let Some(&entry) = order.first() {
        dominators[entry as usize] = entry;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for &block in order.iter().skip(1) {
            let mut chosen = UNKNOWN;
            for &predecessor in &predecessors[block as usize] {
                if dominators[predecessor as usize] == UNKNOWN {
                    continue;
                }
                if chosen == UNKNOWN {
                    chosen = predecessor;
                    continue;
                }
                let (mut a, mut b) = (chosen, predecessor);
                while a != b {
                    while position[a as usize] > position[b as usize] {
                        a = dominators[a as usize];
                    }
                    while position[b as usize] > position[a as usize] {
                        b = dominators[b as usize];
                    }
                }
                chosen = a;
            }
            if dominators[block as usize] != chosen {
                dominators[block as usize] = chosen;
                changed = true;
            }
        }
    }
    for (block, dominator) in dominators.iter_mut().enumerate() {
        if *dominator == UNKNOWN {
            *dominator = block as u32;
        }
    }
    dominators
}
