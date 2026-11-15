//! The loop-fusion pass on hand-built IR: which loops become one, what keeps two apart, and
//! that a function computes the same with its loops fused as without.
//!
//! The loops are built the way the lowerer builds `List.map`, `List.keep_oks` and `List.walk`,
//! over lists of `I64`s, and a small evaluator of the instructions they use runs a function
//! before and after the pass.

use {
    smallvec::SmallVec,
    typed::{
        ir::{
            Arith, BlockId, Builder, Compare, Constant, Function, Item, Local, Op, Terminator, Type,
        },
        loops::fuse_loops,
        unbox::remove_unread,
    },
};

const WHOLES: Type = Type::Vector {
    item: Item::I64,
    array: true,
};

/// The list a pipeline starts from, and a number its loops may read.
const ROWS: Local = Local(0);
const LIMIT: Local = Local(1);

/// What a loop's body computes from its item.
#[derive(Clone, Copy, Default)]
struct Body {
    /// The body's value is the item plus this.
    add: i64,
    /// The value also counts in the item's position.
    with_position: bool,
    /// The body gives up the call for an item equal to this.
    fails_at: Option<i64>,
    /// The value also counts in a variable every loop of the function shares.
    reads_shared: bool,
    /// The body adds one to that variable.
    writes_shared: bool,
    /// The body calls a function.
    calls: bool,
}

/// How a filter makes its empty list.
#[derive(Clone, Copy, PartialEq)]
enum Capacity {
    /// A vector of no items, as `List.keep_oks` makes it.
    None,
    /// A list with room for as many items as the filter reads.
    Length,
    /// A list with room for `LIMIT` items, which fails when that is negative.
    Limit,
}

/// One loop of a pipeline. `input` numbers the list it reads: 0 is `ROWS`, and each map and
/// filter before it adds the list it makes.
#[derive(Clone, Copy)]
enum Stage {
    /// `List.map`: a list of the body's value for each item. With `sized_by`, the new list is
    /// as long as that list, not as the one the loop reads.
    Map {
        input: usize,
        body: Body,
        sized_by: Option<usize>,
    },
    /// `List.keep_oks`: the body's values for the items whose value is not a multiple of
    /// `modulus`, stopping after an item equal to `leaves_at`, and giving up the call when it
    /// keeps nothing if `refuses_empty`. With `made_early`, the empty list is made a block
    /// ahead of the loop, with a test between them that gives up the call, as a `for` loop
    /// that appends to a list made before it does.
    Keep {
        input: usize,
        body: Body,
        modulus: i64,
        leaves_at: Option<i64>,
        refuses_empty: bool,
        capacity: Capacity,
        made_early: bool,
    },
    /// `List.walk`: the sum of the body's values, starting at the list's length with
    /// `starts_at_length`, and stopping before an item equal to `leaves_at`.
    Walk {
        input: usize,
        body: Body,
        leaves_at: Option<i64>,
        starts_at_length: bool,
    },
}

/// A function of `ROWS` and `LIMIT` that runs the stages in order and returns the sum of what
/// the walks gave and the shared variable, and with `lengths` of every list's length too. With
/// `returns`, it returns that list.
#[derive(Clone, Default)]
struct Pipeline {
    stages: Vec<Stage>,
    lengths: bool,
    returns: Option<usize>,
}

fn build(pipeline: &Pipeline) -> Function {
    let mut builder = Builder::new(String::from("test"), &[WHOLES, Type::I64]);
    let shared = builder.emit(Type::I64, Op::Copy(LIMIT));
    let tally = whole(&mut builder, 0);
    let mut lists = vec![ROWS];
    for &stage in &pipeline.stages {
        match stage {
            Stage::Map {
                input,
                body,
                sized_by,
            } => {
                let sized_by = sized_by.map(|list| lists[list]);
                let output = map(&mut builder, lists[input], sized_by, body, shared);
                lists.push(output);
            }
            Stage::Keep {
                input,
                body,
                modulus,
                leaves_at,
                refuses_empty,
                capacity,
                made_early,
            } => {
                let held = builder.emit(WHOLES, Op::Copy(lists[input]));
                let output = builder.local(WHOLES);
                if made_early {
                    make_empty(&mut builder, output, capacity, held);
                    let unlucky = whole(&mut builder, 13);
                    let refused = builder.emit(
                        Type::Bool,
                        Op::Compare(Compare::Eq, Item::I64, LIMIT, unlucky),
                    );
                    fail_if(&mut builder, refused);
                }
                let preheader = builder.current;
                let positions = positions(&mut builder, held);
                let element = builder.emit(Type::I64, Op::ItemAt(held, positions.counter));
                let value = lower_body(&mut builder, body, element, positions.counter, shared);
                let modulus = whole(&mut builder, modulus);
                let rest =
                    builder.emit(Type::I64, Op::Arith(Arith::Mod, Item::I64, value, modulus));
                let zero = whole(&mut builder, 0);
                let dropped =
                    builder.emit(Type::Bool, Op::Compare(Compare::Eq, Item::I64, rest, zero));
                let (kept, join) = (builder.block(), builder.block());
                builder.terminate(Terminator::Branch {
                    condition: dropped,
                    then: join,
                    otherwise: kept,
                });
                builder.switch_to(kept);
                let pushed = builder.emit(WHOLES, Op::Push(output, value));
                builder.push(Some(output), Op::Copy(pushed));
                match leaves_at {
                    Some(last) => {
                        let last = whole(&mut builder, last);
                        let leaves = builder.emit(
                            Type::Bool,
                            Op::Compare(Compare::Eq, Item::I64, element, last),
                        );
                        builder.terminate(Terminator::Branch {
                            condition: leaves,
                            then: positions.exit,
                            otherwise: join,
                        });
                    }
                    None => builder.terminate(Terminator::Jump(join)),
                }
                builder.switch_to(join);
                next_position(&mut builder, &positions);
                if !made_early {
                    builder.switch_to(preheader);
                    make_empty(&mut builder, output, capacity, held);
                    builder.switch_to(positions.exit);
                }
                if refuses_empty {
                    let kept_count = builder.emit(Type::Int, Op::Length(output));
                    let zero = int(&mut builder, 0);
                    let empty = builder.emit(Type::Bool, Op::Same(kept_count, zero));
                    fail_if(&mut builder, empty);
                }
                lists.push(output);
            }
            Stage::Walk {
                input,
                body,
                leaves_at,
                starts_at_length,
            } => {
                let list = lists[input];
                let start = if starts_at_length {
                    builder.emit(Type::I64, Op::Length(list))
                } else {
                    whole(&mut builder, 0)
                };
                let held = builder.emit(WHOLES, Op::Copy(list));
                let state = builder.emit(Type::I64, Op::Copy(start));
                let positions = positions(&mut builder, held);
                let element = builder.emit(Type::I64, Op::ItemAt(held, positions.counter));
                if let Some(last) = leaves_at {
                    let last = whole(&mut builder, last);
                    let leaves = builder.emit(
                        Type::Bool,
                        Op::Compare(Compare::Eq, Item::I64, element, last),
                    );
                    let go = builder.block();
                    builder.terminate(Terminator::Branch {
                        condition: leaves,
                        then: positions.exit,
                        otherwise: go,
                    });
                    builder.switch_to(go);
                }
                let value = lower_body(&mut builder, body, element, positions.counter, shared);
                let next = builder.emit(Type::I64, Op::Arith(Arith::Add, Item::I64, state, value));
                builder.push(Some(state), Op::Copy(next));
                next_position(&mut builder, &positions);
                add_to(&mut builder, tally, state);
            }
        }
    }
    if pipeline.lengths {
        for &list in &lists[1..] {
            let length = builder.emit(Type::I64, Op::Length(list));
            add_to(&mut builder, tally, length);
        }
    }
    add_to(&mut builder, tally, shared);
    let result = match pipeline.returns {
        Some(list) => lists[list],
        None => tally,
    };
    builder.terminate(Terminator::Return(result));
    builder.function
}

/// Assigns `output` an empty list, for a filter of the list `held`.
fn make_empty(builder: &mut Builder, output: Local, capacity: Capacity, held: Local) {
    let fresh = match capacity {
        Capacity::None => {
            let zero = int(builder, 0);
            builder.emit(Type::vector(Item::I64), Op::NewVector(Item::I64, zero))
        }
        Capacity::Length => {
            let room = builder.emit(Type::Int, Op::Length(held));
            builder.emit(Type::vector(Item::I64), Op::EmptyList(Item::I64, room))
        }
        Capacity::Limit => builder.emit(Type::vector(Item::I64), Op::EmptyList(Item::I64, LIMIT)),
    };
    let fresh = builder.emit(WHOLES, Op::Relabel(fresh, WHOLES));
    builder.push(Some(output), Op::Copy(fresh));
}

/// `List.map` over `list`, as the lowerer emits it.
fn map(
    builder: &mut Builder,
    list: Local,
    sized_by: Option<Local>,
    body: Body,
    shared: Local,
) -> Local {
    let held = builder.emit(WHOLES, Op::Copy(list));
    let count = builder.emit(Type::Int, Op::Length(sized_by.unwrap_or(held)));
    let preheader = builder.current;
    let output = builder.local(WHOLES);
    let positions = positions(builder, held);
    let element = builder.emit(Type::I64, Op::ItemAt(held, positions.counter));
    let value = lower_body(builder, body, element, positions.counter, shared);
    let updated = builder.emit(WHOLES, Op::Set(output, positions.counter, value));
    builder.push(Some(output), Op::Copy(updated));
    next_position(builder, &positions);
    builder.switch_to(preheader);
    let fresh = builder.emit(Type::vector(Item::I64), Op::NewVector(Item::I64, count));
    let fresh = builder.emit(WHOLES, Op::Relabel(fresh, WHOLES));
    builder.push(Some(output), Op::Copy(fresh));
    builder.switch_to(positions.exit);
    output
}

fn lower_body(
    builder: &mut Builder,
    body: Body,
    element: Local,
    counter: Local,
    shared: Local,
) -> Local {
    if let Some(bad) = body.fails_at {
        let bad = whole(builder, bad);
        let hit = builder.emit(
            Type::Bool,
            Op::Compare(Compare::Eq, Item::I64, element, bad),
        );
        fail_if(builder, hit);
    }
    if body.calls {
        builder.push(None, Op::CallTyped(0, SmallVec::from_slice(&[element])));
    }
    let add = whole(builder, body.add);
    let mut value = builder.emit(Type::I64, Op::Arith(Arith::Add, Item::I64, element, add));
    if body.with_position {
        value = builder.emit(Type::I64, Op::Arith(Arith::Add, Item::I64, value, counter));
    }
    if body.reads_shared {
        value = builder.emit(Type::I64, Op::Arith(Arith::Add, Item::I64, value, shared));
    }
    if body.writes_shared {
        let one = whole(builder, 1);
        add_to(builder, shared, one);
    }
    value
}

fn whole(builder: &mut Builder, value: i64) -> Local {
    builder.emit(Type::I64, Op::Const(Constant::I64(value)))
}

fn int(builder: &mut Builder, value: i32) -> Local {
    builder.emit(Type::Int, Op::Const(Constant::Int(value)))
}

fn add_to(builder: &mut Builder, variable: Local, value: Local) {
    let sum = builder.emit(Type::I64, Op::Arith(Arith::Add, Item::I64, variable, value));
    builder.push(Some(variable), Op::Copy(sum));
}

fn fail_if(builder: &mut Builder, condition: Local) {
    let (failed, go) = (builder.block(), builder.block());
    builder.terminate(Terminator::Branch {
        condition,
        then: failed,
        otherwise: go,
    });
    builder.switch_to(failed);
    builder.terminate(Terminator::Fail);
    builder.switch_to(go);
}

struct Positions {
    counter: Local,
    end: Local,
    header: BlockId,
    exit: BlockId,
}

/// Opens a loop over the positions of `held`, as the lowerer's `positions` does, and leaves
/// the builder in the loop's body.
fn positions(builder: &mut Builder, held: Local) -> Positions {
    let count = builder.emit(Type::I64, Op::Length(held));
    let one = whole(builder, 1);
    let counter = builder.emit(Type::I64, Op::Copy(one));
    let end = builder.emit(Type::I64, Op::Copy(count));
    let empty = builder.emit(Type::Bool, Op::Compare(Compare::Lt, Item::I64, end, one));
    let (body, exit) = (builder.block(), builder.block());
    builder.terminate(Terminator::Branch {
        condition: empty,
        then: exit,
        otherwise: body,
    });
    builder.switch_to(body);
    let header = builder.block();
    builder.terminate(Terminator::Jump(header));
    builder.switch_to(header);
    builder.push(None, Op::Poll);
    Positions {
        counter,
        end,
        header,
        exit,
    }
}

/// Closes the body of a loop `positions` opened, and continues after the loop.
fn next_position(builder: &mut Builder, positions: &Positions) {
    let done = builder.emit(Type::Bool, Op::Same(positions.counter, positions.end));
    let step = builder.block();
    builder.terminate(Terminator::Branch {
        condition: done,
        then: positions.exit,
        otherwise: step,
    });
    builder.switch_to(step);
    let one = whole(builder, 1);
    let next = builder.emit(Type::I64, Op::Step(positions.counter, one));
    builder.push(Some(positions.counter), Op::Copy(next));
    builder.terminate(Terminator::Jump(positions.header));
    builder.switch_to(positions.exit);
}

/// What a local holds while the evaluator runs a function.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Unset,
    Number(i64),
    Flag(bool),
    List(Vec<i64>),
}

fn number(locals: &[Value], local: Local) -> i64 {
    match &locals[local.0 as usize] {
        Value::Number(value) => *value,
        other => panic!("local {} holds {other:?}, not a number", local.0),
    }
}

fn list(locals: &[Value], local: Local) -> &Vec<i64> {
    match &locals[local.0 as usize] {
        Value::List(items) => items,
        other => panic!("local {} holds {other:?}, not a list", local.0),
    }
}

/// What `function` returns for these arguments, or `None` where it gives up the call. A read
/// of a variable nothing assigned, and an unchecked read outside a list, are bugs of whoever
/// built the function, so they panic.
fn run(function: &Function, rows: &[i64], limit: i64) -> Option<Value> {
    let mut locals = vec![Value::Unset; function.locals.len()];
    locals[ROWS.0 as usize] = Value::List(rows.to_vec());
    locals[LIMIT.0 as usize] = Value::Number(limit);
    let mut block = 0;
    let mut steps = 0;
    loop {
        let body = &function.blocks[block];
        for inst in &body.insts {
            steps += 1;
            assert!(steps < 1_000_000, "the function does not end");
            let value = match inst.op {
                Op::Const(Constant::I64(value)) => Value::Number(value),
                Op::Const(Constant::Int(value)) => Value::Number(value as i64),
                Op::Copy(from) | Op::Relabel(from, _) => {
                    let value = locals[from.0 as usize].clone();
                    assert!(value != Value::Unset, "local {} is read unset", from.0);
                    value
                }
                Op::Arith(op, Item::I64, left, right) => {
                    let (left, right) = (number(&locals, left), number(&locals, right));
                    Value::Number(match op {
                        Arith::Add => left.checked_add(right)?,
                        Arith::Mod => left.checked_rem_euclid(right)?,
                        other => panic!("the evaluator does not know {other:?}"),
                    })
                }
                Op::Compare(op, Item::I64, left, right) => {
                    let (left, right) = (number(&locals, left), number(&locals, right));
                    Value::Flag(match op {
                        Compare::Eq => left == right,
                        Compare::Lt => left < right,
                        other => panic!("the evaluator does not know {other:?}"),
                    })
                }
                Op::Same(left, right) => {
                    Value::Flag(number(&locals, left) == number(&locals, right))
                }
                Op::Step(counter, by) => {
                    Value::Number(number(&locals, counter) + number(&locals, by))
                }
                Op::Length(of) => Value::Number(list(&locals, of).len() as i64),
                Op::CountOfI64(count) => {
                    let count = number(&locals, count);
                    if !(0..=i32::MAX as i64).contains(&count) {
                        return None;
                    }
                    Value::Number(count)
                }
                Op::ItemAt(from, position) => {
                    let items = list(&locals, from);
                    let position = number(&locals, position);
                    assert!(
                        position >= 1 && position as usize <= items.len(),
                        "an unchecked read at {position} of {} items",
                        items.len()
                    );
                    Value::Number(items[position as usize - 1])
                }
                Op::Set(into, position, value) => {
                    let mut items = list(&locals, into).clone();
                    let position = number(&locals, position);
                    if position < 1 || position as usize > items.len() {
                        return None;
                    }
                    items[position as usize - 1] = number(&locals, value);
                    Value::List(items)
                }
                Op::Push(onto, value) => {
                    let mut items = list(&locals, onto).clone();
                    items.push(number(&locals, value));
                    Value::List(items)
                }
                Op::NewVector(Item::I64, count) => {
                    let count = number(&locals, count);
                    if count < 0 {
                        return None;
                    }
                    Value::List(vec![0; count as usize])
                }
                Op::EmptyList(Item::I64, capacity) => {
                    if number(&locals, capacity) < 0 {
                        return None;
                    }
                    Value::List(Vec::new())
                }
                Op::Poll => continue,
                ref other => panic!("the evaluator does not know {other:?}"),
            };
            if let Some(dest) = inst.dest {
                locals[dest.0 as usize] = value;
            }
        }
        block = match body.terminator {
            Terminator::Jump(BlockId(next)) => next as usize,
            Terminator::Branch {
                condition,
                then: BlockId(then),
                otherwise: BlockId(otherwise),
            } => match locals[condition.0 as usize] {
                Value::Flag(true) => then as usize,
                Value::Flag(false) => otherwise as usize,
                ref other => panic!("a branch on {other:?}"),
            },
            Terminator::Return(value) => return Some(locals[value.0 as usize].clone()),
            Terminator::Fail => return None,
        };
    }
}

fn count(function: &Function, counted: impl Fn(&Op) -> bool) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|inst| counted(&inst.op))
        .count()
}

fn lists_made(function: &Function) -> usize {
    count(function, |op| {
        matches!(op, Op::NewVector(..) | Op::EmptyList(..))
    })
}

/// The inputs every pipeline runs on: lists of no item, of one, and of several, with the items
/// the bodies single out, and a limit of each sign.
const INPUTS: &[(&[i64], i64)] = &[
    (&[], 0),
    (&[1, 2], 13),
    (&[], -1),
    (&[4], 2),
    (&[3], 0),
    (&[1, 2, 3, 4, 5, 6], 1),
    (&[6, 5, 4, 3, 2, 1], -2),
    (&[2, 2, 7, 2], 3),
    (&[0, 9, 0, 9, 0], 5),
];

/// The pipeline with its loops fused, after checking that it computes what the unfused
/// pipeline does on every input.
fn fused(pipeline: &Pipeline) -> (Function, Function) {
    let original = build(pipeline);
    let mut fused = original.clone();
    fuse_loops(&mut fused);
    remove_unread(&mut fused);
    for &(rows, limit) in INPUTS {
        assert_eq!(
            run(&fused, rows, limit),
            run(&original, rows, limit),
            "for {rows:?} and {limit}, fused:\n{fused}\nfrom:\n{original}"
        );
    }
    (original, fused)
}

fn assert_fuses_all(pipeline: &Pipeline) -> Function {
    let (original, fused) = fused(pipeline);
    assert_eq!(lists_made(&fused), 0, "{fused}\nfrom:\n{original}");
    assert_eq!(count(&fused, |op| matches!(op, Op::Poll)), 1, "{fused}");
    assert_eq!(
        count(&fused, |op| matches!(op, Op::Push(..) | Op::Set(..))),
        0,
        "{fused}"
    );
    fused
}

fn assert_keeps_all(pipeline: &Pipeline) {
    let (original, fused) = fused(pipeline);
    assert_eq!(lists_made(&fused), lists_made(&original), "{fused}");
    assert_eq!(
        count(&fused, |op| matches!(op, Op::Poll)),
        count(&original, |op| matches!(op, Op::Poll)),
        "{fused}"
    );
}

fn map_of(input: usize) -> Stage {
    Stage::Map {
        input,
        body: Body {
            add: 1,
            ..Body::default()
        },
        sized_by: None,
    }
}

fn keep_of(input: usize) -> Stage {
    Stage::Keep {
        input,
        body: Body::default(),
        modulus: 3,
        leaves_at: None,
        refuses_empty: true,
        capacity: Capacity::None,
        made_early: false,
    }
}

fn walk_of(input: usize) -> Stage {
    Stage::Walk {
        input,
        body: Body::default(),
        leaves_at: None,
        starts_at_length: false,
    }
}

#[test]
fn a_map_a_filter_and_a_walk_become_one_loop() {
    let fused = assert_fuses_all(&Pipeline {
        stages: vec![map_of(0), keep_of(1), walk_of(2)],
        ..Pipeline::default()
    });
    // The one read left is of the list the pipeline starts from.
    assert_eq!(
        count(&fused, |op| matches!(op, Op::ItemAt(..))),
        1,
        "{fused}"
    );
}

#[test]
fn the_length_of_a_fused_list_is_still_known() {
    assert_fuses_all(&Pipeline {
        stages: vec![map_of(0), keep_of(1), walk_of(2)],
        lengths: true,
        ..Pipeline::default()
    });
}

#[test]
fn a_walk_that_reads_positions_gets_them_from_the_loop_it_joins() {
    let positioned = Body {
        with_position: true,
        ..Body::default()
    };
    for producer in [map_of(0), keep_of(0)] {
        assert_fuses_all(&Pipeline {
            stages: vec![
                producer,
                Stage::Walk {
                    input: 1,
                    body: positioned,
                    leaves_at: None,
                    starts_at_length: false,
                },
            ],
            ..Pipeline::default()
        });
    }
}

#[test]
fn a_filter_that_stops_early_still_feeds_the_loop_after_it() {
    assert_fuses_all(&Pipeline {
        stages: vec![
            Stage::Keep {
                input: 0,
                body: Body::default(),
                modulus: 5,
                leaves_at: Some(4),
                refuses_empty: false,
                capacity: Capacity::Length,
                made_early: false,
            },
            walk_of(1),
        ],
        lengths: true,
        ..Pipeline::default()
    });
}

#[test]
fn a_list_made_a_block_ahead_of_the_loop_that_fills_it_is_fused() {
    assert_fuses_all(&Pipeline {
        stages: vec![
            Stage::Keep {
                input: 0,
                body: Body::default(),
                modulus: 3,
                leaves_at: Some(5),
                refuses_empty: false,
                capacity: Capacity::Length,
                made_early: true,
            },
            Stage::Walk {
                input: 1,
                body: Body::default(),
                leaves_at: None,
                starts_at_length: false,
            },
        ],
        lengths: true,
        ..Pipeline::default()
    });
}

#[test]
fn a_walk_may_start_from_the_length_of_a_mapped_list_but_not_of_a_filtered_one() {
    let from_length = |input| Stage::Walk {
        input,
        body: Body::default(),
        leaves_at: None,
        starts_at_length: true,
    };
    assert_fuses_all(&Pipeline {
        stages: vec![map_of(0), from_length(1)],
        ..Pipeline::default()
    });
    // How many items a filter keeps is known only once its loop has run.
    assert_keeps_all(&Pipeline {
        stages: vec![keep_of(0), from_length(1)],
        ..Pipeline::default()
    });
}

#[test]
fn a_loop_that_leaves_early_keeps_the_list_it_reads() {
    assert_keeps_all(&Pipeline {
        stages: vec![
            map_of(0),
            Stage::Walk {
                input: 1,
                body: Body::default(),
                leaves_at: Some(4),
                starts_at_length: false,
            },
        ],
        ..Pipeline::default()
    });
    assert_keeps_all(&Pipeline {
        stages: vec![
            map_of(0),
            Stage::Keep {
                input: 1,
                body: Body::default(),
                modulus: 3,
                leaves_at: Some(5),
                refuses_empty: false,
                capacity: Capacity::None,
                made_early: false,
            },
        ],
        ..Pipeline::default()
    });
}

#[test]
fn a_list_two_loops_read_or_the_function_returns_stays() {
    assert_keeps_all(&Pipeline {
        stages: vec![map_of(0), walk_of(1), walk_of(1)],
        ..Pipeline::default()
    });
    assert_keeps_all(&Pipeline {
        stages: vec![map_of(0), walk_of(1)],
        returns: Some(1),
        ..Pipeline::default()
    });
}

#[test]
fn loops_that_share_a_variable_one_of_them_assigns_stay_apart() {
    let reads = Body {
        reads_shared: true,
        ..Body::default()
    };
    let writes = Body {
        writes_shared: true,
        ..Body::default()
    };
    let walk = |body| Stage::Walk {
        input: 1,
        body,
        leaves_at: None,
        starts_at_length: false,
    };
    let map = |body| Stage::Map {
        input: 0,
        body,
        sized_by: None,
    };
    assert_keeps_all(&Pipeline {
        stages: vec![map(reads), walk(writes)],
        ..Pipeline::default()
    });
    assert_keeps_all(&Pipeline {
        stages: vec![map(writes), walk(reads)],
        ..Pipeline::default()
    });
    assert_keeps_all(&Pipeline {
        stages: vec![map(writes), walk(writes)],
        ..Pipeline::default()
    });
    assert_fuses_all(&Pipeline {
        stages: vec![map(reads), walk(reads)],
        ..Pipeline::default()
    });
}

#[test]
fn a_list_as_long_as_another_list_stays() {
    // The map over the filtered list fills a list as long as the first one, which gives up
    // the call unless the two are equally long.
    assert_keeps_all(&Pipeline {
        stages: vec![
            Stage::Keep {
                input: 0,
                body: Body::default(),
                modulus: 3,
                leaves_at: None,
                refuses_empty: false,
                capacity: Capacity::Length,
                made_early: false,
            },
            Stage::Map {
                input: 0,
                body: Body::default(),
                sized_by: Some(1),
            },
            walk_of(2),
        ],
        ..Pipeline::default()
    });
}

#[test]
fn a_list_whose_capacity_may_be_negative_stays() {
    let keep = |capacity| Stage::Keep {
        input: 0,
        body: Body::default(),
        modulus: 3,
        leaves_at: None,
        refuses_empty: false,
        capacity,
        made_early: false,
    };
    assert_keeps_all(&Pipeline {
        stages: vec![keep(Capacity::Limit), walk_of(1)],
        ..Pipeline::default()
    });
    assert_fuses_all(&Pipeline {
        stages: vec![keep(Capacity::Length), walk_of(1)],
        ..Pipeline::default()
    });
}

#[test]
fn a_call_in_either_loop_keeps_the_list() {
    let calls = Body {
        calls: true,
        ..Body::default()
    };
    let pipelines = [
        vec![
            Stage::Map {
                input: 0,
                body: calls,
                sized_by: None,
            },
            walk_of(1),
        ],
        vec![
            map_of(0),
            Stage::Walk {
                input: 1,
                body: calls,
                leaves_at: None,
                starts_at_length: false,
            },
        ],
    ];
    for stages in pipelines {
        // The evaluator runs no calls, so only the function's shape is compared.
        let original = build(&Pipeline {
            stages,
            ..Pipeline::default()
        });
        let mut fused = original.clone();
        fuse_loops(&mut fused);
        assert_eq!(fused.to_string(), original.to_string());
    }
}

#[test]
fn a_body_that_gives_up_gives_up_the_fused_call_too() {
    let fails = |at| Body {
        fails_at: Some(at),
        ..Body::default()
    };
    // The filter gives up at an item the map only reaches later, and the other way around.
    for (first, second) in [(5, 3), (3, 5), (9, 9)] {
        assert_fuses_all(&Pipeline {
            stages: vec![
                Stage::Map {
                    input: 0,
                    body: fails(first),
                    sized_by: None,
                },
                Stage::Keep {
                    input: 1,
                    body: fails(second),
                    modulus: 4,
                    leaves_at: None,
                    refuses_empty: true,
                    capacity: Capacity::None,
                    made_early: false,
                },
                walk_of(2),
            ],
            lengths: true,
            ..Pipeline::default()
        });
    }
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
            .unwrap_or(0x5EED_1234);
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
                "replay: FUZZ_SEED={} cargo test -p typed --test test_loops",
                self.seed
            );
        }
    }
}

fn random_body(rng: &mut Rng) -> Body {
    Body {
        add: rng.below(4) as i64,
        with_position: rng.once_in(3),
        fails_at: rng.once_in(4).then(|| rng.below(10) as i64),
        reads_shared: rng.once_in(5),
        writes_shared: rng.once_in(8),
        calls: false,
    }
}

/// A list the stage after `made` lists reads: most often the one made last.
fn random_input(rng: &mut Rng, made: usize) -> usize {
    if rng.once_in(4) {
        rng.below(made + 1)
    } else {
        made
    }
}

fn random_pipeline(rng: &mut Rng) -> Pipeline {
    let mut stages = Vec::new();
    let mut made = 0;
    for _ in 0..2 + rng.below(3) {
        let input = random_input(rng, made);
        let body = random_body(rng);
        stages.push(match rng.below(3) {
            0 => {
                made += 1;
                Stage::Map {
                    input,
                    body,
                    sized_by: rng.once_in(8).then(|| rng.below(made)),
                }
            }
            1 => {
                made += 1;
                Stage::Keep {
                    input,
                    body,
                    modulus: 2 + rng.below(3) as i64,
                    leaves_at: rng.once_in(5).then(|| rng.below(10) as i64),
                    refuses_empty: rng.once_in(2),
                    capacity: match rng.below(6) {
                        0 => Capacity::Limit,
                        1 | 2 => Capacity::Length,
                        _ => Capacity::None,
                    },
                    made_early: rng.once_in(3),
                }
            }
            _ => Stage::Walk {
                input,
                body,
                leaves_at: rng.once_in(6).then(|| rng.below(10) as i64),
                starts_at_length: rng.once_in(4),
            },
        });
    }
    Pipeline {
        stages,
        lengths: rng.once_in(2),
        returns: (made > 0 && rng.once_in(10)).then(|| 1 + rng.below(made)),
    }
}

#[test]
fn fused_pipelines_compute_what_unfused_ones_do() {
    let mut rng = Rng::from_env();
    let pipelines = 3000;
    let mut fused_some = 0;
    for _ in 0..pipelines {
        let pipeline = random_pipeline(&mut rng);
        let original = build(&pipeline);
        let mut fused = original.clone();
        fuse_loops(&mut fused);
        remove_unread(&mut fused);
        fused_some += (lists_made(&fused) < lists_made(&original)) as usize;
        for &(fixed, limit) in INPUTS {
            let drawn: Vec<i64> = (0..rng.below(7)).map(|_| rng.below(10) as i64).collect();
            for rows in [fixed, &drawn[..]] {
                assert_eq!(
                    run(&fused, rows, limit),
                    run(&original, rows, limit),
                    "for {rows:?} and {limit}, fused:\n{fused}\nfrom:\n{original}"
                );
            }
        }
    }
    // A generator that drifts into pipelines the pass refuses would compare nothing.
    assert!(
        fused_some * 4 > pipelines,
        "only {fused_some} of {pipelines} pipelines had a list fused away"
    );
}
