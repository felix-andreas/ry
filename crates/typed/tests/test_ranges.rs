//! The ranges of whole-number locals on hand-built IR: what a counter, a remainder and a test
//! are known to hold, what is not known, and that no run of a random function ever reads a
//! value outside the range claimed for it.

use typed::{
    ir::{Arith, BlockId, Builder, Compare, Constant, Function, Item, Local, Op, Terminator, Type},
    ranges::{NA, Range, ranges},
};

fn constant(builder: &mut Builder, value: i64) -> Local {
    builder.emit(Type::I64, Op::Const(Constant::I64(value)))
}

fn arith(builder: &mut Builder, op: Arith, a: Local, b: Local) -> Local {
    builder.emit(Type::I64, Op::Arith(op, Item::I64, a, b))
}

fn compare(builder: &mut Builder, op: Compare, a: Local, b: Local) -> Local {
    builder.emit(Type::Bool, Op::Compare(op, Item::I64, a, b))
}

fn branch(builder: &mut Builder, condition: Local, then: BlockId, otherwise: BlockId) {
    builder.terminate(Terminator::Branch {
        condition,
        then,
        otherwise,
    });
}

const ALL: Range = Range {
    lo: i64::MIN,
    hi: i64::MAX,
};

#[test]
fn a_counter_from_one_is_never_negative_and_its_remainder_is_small() {
    let mut builder = Builder::new("counter".to_string(), &[Type::I64]);
    let end = Local(0);
    let body = builder.block();
    let after = builder.block();
    let one = constant(&mut builder, 1);
    let counter = builder.emit(Type::I64, Op::Copy(one));
    builder.terminate(Terminator::Jump(body));

    builder.switch_to(body);
    let item = builder.emit(Type::I64, Op::Copy(counter));
    let seven = constant(&mut builder, 7);
    let remainder = arith(&mut builder, Arith::Mod, item, seven);
    let quantity = arith(&mut builder, Arith::Add, remainder, one);
    let three = constant(&mut builder, 3);
    let third = arith(&mut builder, Arith::IntDiv, item, three);
    let next = builder.emit(Type::I64, Op::Step(counter, one));
    builder.push(Some(counter), Op::Copy(next));
    let more = compare(&mut builder, Compare::Le, counter, end);
    branch(&mut builder, more, body, after);

    builder.switch_to(after);
    builder.terminate(Terminator::Return(quantity));

    let found = ranges(&builder.function);
    assert_eq!(
        found.of(1, item),
        Range {
            lo: 1,
            hi: i64::MAX
        }
    );
    assert_eq!(found.of(1, remainder), Range { lo: 0, hi: 6 });
    assert_eq!(found.of(2, quantity), Range { lo: 1, hi: 7 });
    assert_eq!(
        found.of(1, third),
        Range {
            lo: 0,
            hi: i64::MAX / 3
        }
    );
    assert_eq!(found.of(1, end), ALL);
}

/// `if (n < 2) n else { ... }`, where the `else` side splits and joins again.
#[test]
fn a_test_bounds_its_operand_in_every_block_its_branch_dominates() {
    let mut builder = Builder::new("guarded".to_string(), &[Type::I64]);
    let n = Local(0);
    let small = builder.block();
    let large = builder.block();
    let left = builder.block();
    let right = builder.block();
    let joined = builder.block();
    let two = constant(&mut builder, 2);
    let below = compare(&mut builder, Compare::Lt, n, two);
    branch(&mut builder, below, small, large);

    builder.switch_to(small);
    builder.terminate(Terminator::Return(n));

    builder.switch_to(large);
    let one = constant(&mut builder, 1);
    let previous = arith(&mut builder, Arith::Sub, n, one);
    let ten = constant(&mut builder, 10);
    let tiny = compare(&mut builder, Compare::Le, previous, ten);
    branch(&mut builder, tiny, left, right);

    builder.switch_to(left);
    builder.terminate(Terminator::Jump(joined));
    builder.switch_to(right);
    builder.terminate(Terminator::Jump(joined));

    builder.switch_to(joined);
    let before = arith(&mut builder, Arith::Sub, n, two);
    builder.terminate(Terminator::Return(before));

    let found = ranges(&builder.function);
    assert_eq!(found.of(0, n), ALL);
    assert_eq!(found.of(1, n).hi, 1);
    for block in [2, 3, 4, 5] {
        assert_eq!(found.of(block, n).lo, 2, "block {block}");
    }
    assert_eq!(
        found.of(2, previous),
        Range {
            lo: 1,
            hi: i64::MAX - 1
        }
    );
    assert_eq!(found.of(3, previous), Range { lo: 1, hi: 10 });
    assert_eq!(found.of(4, previous).lo, 11);
    // The two sides join again, so neither side's test holds there.
    assert_eq!(found.of(5, previous).lo, 1);
    assert_eq!(found.of(5, before).lo, 0);
}

#[test]
fn a_test_says_nothing_about_a_local_assigned_twice() {
    let mut builder = Builder::new("reassigned".to_string(), &[Type::I64, Type::I64]);
    let (n, big) = (Local(0), Local(1));
    let then = builder.block();
    let otherwise = builder.block();
    let x = builder.emit(Type::I64, Op::Copy(n));
    let two = constant(&mut builder, 2);
    let above = compare(&mut builder, Compare::Ge, x, two);
    branch(&mut builder, above, then, otherwise);

    builder.switch_to(then);
    let lowered = arith(&mut builder, Arith::Sub, x, big);
    builder.push(Some(x), Op::Copy(lowered));
    builder.terminate(Terminator::Return(x));

    builder.switch_to(otherwise);
    builder.terminate(Terminator::Return(x));

    let found = ranges(&builder.function);
    assert_eq!(found.of(1, x), ALL);
    assert_eq!(found.of(2, x), ALL);
}

#[test]
fn equality_gives_the_other_operands_range_on_the_side_where_it_holds() {
    let mut builder = Builder::new("equal".to_string(), &[Type::I64]);
    let n = Local(0);
    let same = builder.block();
    let different = builder.block();
    let five = constant(&mut builder, 5);
    let equal = compare(&mut builder, Compare::Eq, n, five);
    branch(&mut builder, equal, same, different);
    builder.switch_to(same);
    builder.terminate(Terminator::Return(n));
    builder.switch_to(different);
    builder.terminate(Terminator::Return(n));

    let found = ranges(&builder.function);
    assert_eq!(found.of(1, n), Range { lo: 5, hi: 5 });
    assert_eq!(found.of(2, n), ALL);
}

#[test]
fn a_local_read_on_a_path_that_skips_its_assignment_may_be_zero() {
    let mut builder = Builder::new("skipped".to_string(), &[Type::Bool]);
    let flag = Local(0);
    let assigns = builder.block();
    let joined = builder.block();
    let sometimes = builder.local(Type::I64);
    let always = constant(&mut builder, 9);
    branch(&mut builder, flag, assigns, joined);

    builder.switch_to(assigns);
    builder.push(Some(sometimes), Op::Const(Constant::I64(5)));
    builder.terminate(Terminator::Jump(joined));

    builder.switch_to(joined);
    let sum = arith(&mut builder, Arith::Add, sometimes, always);
    builder.terminate(Terminator::Return(sum));

    let found = ranges(&builder.function);
    assert_eq!(found.of(2, sometimes), Range { lo: 0, hi: 5 });
    assert_eq!(found.of(2, always), Range { lo: 9, hi: 9 });
    assert_eq!(found.of(2, sum), Range { lo: 9, hi: 14 });
}

#[test]
fn a_logical_copied_from_a_condition_is_never_na() {
    let mut builder = Builder::new("logical".to_string(), &[Type::I64, Type::Lgl, Type::Int]);
    let (n, unknown, integer) = (Local(0), Local(1), Local(2));
    let zero = constant(&mut builder, 0);
    let condition = compare(&mut builder, Compare::Eq, n, zero);
    let stored = builder.emit(Type::Lgl, Op::Copy(condition));
    let tested = builder.emit(Type::Bool, Op::Truth(unknown));
    let wide = builder.emit(Type::I64, Op::ToI64(integer));
    builder.terminate(Terminator::Return(stored));

    let found = ranges(&builder.function);
    assert!(found.of(0, stored).lo > NA);
    assert_eq!(found.of(0, unknown).lo, NA);
    assert!(found.of(0, tested).lo > NA);
    assert_eq!(found.of(0, integer).lo, NA);
    assert_eq!(
        found.of(0, wide),
        Range {
            lo: NA + 1,
            hi: i64::from(i32::MAX)
        }
    );
}

/// Runs random functions of whole-number instructions, with branches, loops, reassigned
/// locals and reads of locals no path has assigned, and checks every value an instruction or
/// a branch reads against the range claimed for it in that block.
#[test]
fn no_run_reads_a_value_outside_its_range() {
    let mut rng = Rng::from_env();
    for _ in 0..10_000 {
        let function = random_function(&mut rng);
        let found = ranges(&function);
        for _ in 0..12 {
            let arguments = [interesting(&mut rng), interesting(&mut rng)];
            let mut values = vec![0i64; function.locals.len()];
            values[..2].copy_from_slice(&arguments);
            let mut block = 0usize;
            'run: for _ in 0..200 {
                let read = |local: Local, values: &[i64]| {
                    let value = values[local.0 as usize];
                    let range = found.of(block, local);
                    assert!(
                        range.lo <= value && value <= range.hi,
                        "local {} holds {value} in block {block}, outside {range:?}, for \
                         arguments {arguments:?} of\n{function}",
                        local.0
                    );
                    value
                };
                let body = &function.blocks[block];
                for inst in &body.insts {
                    let value = match inst.op {
                        Op::Const(Constant::I64(value)) => value,
                        Op::Copy(a) => read(a, &values),
                        Op::Step(a, b) => match read(a, &values).checked_add(read(b, &values)) {
                            Some(sum) => sum,
                            None => break 'run,
                        },
                        Op::Compare(op, _, a, b) => {
                            let (x, y) = (read(a, &values), read(b, &values));
                            i64::from(match op {
                                Compare::Eq => x == y,
                                Compare::Ne => x != y,
                                Compare::Lt => x < y,
                                Compare::Le => x <= y,
                                Compare::Gt => x > y,
                                Compare::Ge => x >= y,
                            })
                        }
                        Op::Arith(op, _, a, b) => {
                            let (x, y) = (read(a, &values), read(b, &values));
                            let result = match op {
                                Arith::Add => x.checked_add(y),
                                Arith::Sub => x.checked_sub(y),
                                Arith::Mul => x.checked_mul(y),
                                // The floored remainder and quotient, as `%%` and `%/%` give.
                                Arith::Mod if y == -1 => Some(0),
                                Arith::Mod if y != 0 => {
                                    let remainder = x % y;
                                    Some(if remainder != 0 && (remainder < 0) != (y < 0) {
                                        remainder + y
                                    } else {
                                        remainder
                                    })
                                }
                                Arith::IntDiv if y != 0 => x.checked_div(y).map(|quotient| {
                                    if x % y != 0 && (x % y < 0) != (y < 0) {
                                        quotient - 1
                                    } else {
                                        quotient
                                    }
                                }),
                                Arith::Min => Some(x.min(y)),
                                Arith::Max => Some(x.max(y)),
                                _ => None,
                            };
                            match result {
                                Some(result) => result,
                                None => break 'run,
                            }
                        }
                        _ => unreachable!("the generator makes no other instruction"),
                    };
                    if let Some(dest) = inst.dest {
                        values[dest.0 as usize] = value;
                    }
                }
                block = match body.terminator {
                    Terminator::Jump(target) => target.0 as usize,
                    Terminator::Branch {
                        condition,
                        then,
                        otherwise,
                    } => {
                        if read(condition, &values) != 0 {
                            then.0 as usize
                        } else {
                            otherwise.0 as usize
                        }
                    }
                    Terminator::Return(local) => {
                        read(local, &values);
                        break 'run;
                    }
                    Terminator::Fail => break 'run,
                };
            }
        }
    }
}

/// A value at or near a limit, or a small one.
fn interesting(rng: &mut Rng) -> i64 {
    const EDGES: [i64; 8] = [
        i64::MIN,
        i64::MIN + 1,
        i64::MAX,
        i64::MAX - 1,
        i32::MIN as i64,
        i32::MAX as i64,
        0,
        -1,
    ];
    match rng.below(4) {
        0 => EDGES[rng.below(EDGES.len())],
        _ => rng.below(21) as i64 - 8,
    }
}

fn random_function(rng: &mut Rng) -> Function {
    let mut builder = Builder::new("random".to_string(), &[Type::I64, Type::I64]);
    let mut numbers = vec![Local(0), Local(1)];
    let mut conditions = Vec::new();
    for _ in 0..1 + rng.below(3) {
        numbers.push(builder.local(Type::I64));
    }
    conditions.push(builder.local(Type::Bool));
    let blocks = 1 + rng.below(7);
    for _ in 1..blocks {
        builder.block();
    }
    for block in 0..blocks {
        builder.switch_to(BlockId(block as u32));
        let mut last_condition = None;
        for _ in 0..rng.below(6) {
            let a = numbers[rng.below(numbers.len())];
            let b = numbers[rng.below(numbers.len())];
            // A fresh destination is assigned once, which is what a test needs, and an old
            // one is assigned again.
            let fresh = rng.below(3) != 0;
            if rng.once_in(4) {
                let op = [
                    Compare::Eq,
                    Compare::Ne,
                    Compare::Lt,
                    Compare::Le,
                    Compare::Gt,
                    Compare::Ge,
                ][rng.below(6)];
                let dest = if fresh {
                    builder.local(Type::Bool)
                } else {
                    conditions[rng.below(conditions.len())]
                };
                builder.push(Some(dest), Op::Compare(op, Item::I64, a, b));
                conditions.push(dest);
                last_condition = Some(dest);
                continue;
            }
            let op = match rng.below(12) {
                0 | 1 => Op::Const(Constant::I64(interesting(rng))),
                2 => Op::Copy(a),
                3 => Op::Step(a, b),
                4 | 5 => Op::Arith(Arith::Add, Item::I64, a, b),
                6 | 7 => Op::Arith(Arith::Sub, Item::I64, a, b),
                8 => Op::Arith(Arith::Mul, Item::I64, a, b),
                9 => Op::Arith(Arith::Mod, Item::I64, a, b),
                10 => Op::Arith(Arith::IntDiv, Item::I64, a, b),
                _ => Op::Arith(
                    if rng.once_in(2) {
                        Arith::Min
                    } else {
                        Arith::Max
                    },
                    Item::I64,
                    a,
                    b,
                ),
            };
            // A parameter is assigned again now and then, which takes it out of the tests.
            let dest = if fresh {
                builder.local(Type::I64)
            } else if rng.once_in(5) {
                numbers[rng.below(2)]
            } else {
                numbers[2 + rng.below(numbers.len() - 2)]
            };
            builder.push(Some(dest), op);
            if !numbers.contains(&dest) {
                numbers.push(dest);
            }
        }
        let target = |rng: &mut Rng| BlockId(rng.below(blocks) as u32);
        let terminator = match rng.below(7) {
            0 => Terminator::Return(numbers[rng.below(numbers.len())]),
            1 => Terminator::Jump(target(rng)),
            2 => Terminator::Fail,
            _ => Terminator::Branch {
                // Most branches test the comparison the block just made.
                condition: last_condition
                    .filter(|_| !rng.once_in(5))
                    .unwrap_or_else(|| conditions[rng.below(conditions.len())]),
                then: target(rng),
                otherwise: target(rng),
            },
        };
        builder.terminate(terminator);
    }
    builder.function
}

/// splitmix64, seeded from `FUZZ_SEED`. It prints the replay command when the test fails.
struct Rng {
    seed: u64,
    state: u64,
}

impl Rng {
    fn from_env() -> Rng {
        let seed = std::env::var("FUZZ_SEED")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0x5EED_4321);
        Rng { seed, state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    /// True once in `of` draws.
    fn once_in(&mut self, of: usize) -> bool {
        self.below(of) == 0
    }
}

impl Drop for Rng {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "replay: FUZZ_SEED={} cargo test -p typed --test test_ranges",
                self.seed
            );
        }
    }
}
