//! The fusion pass on hand-built IR: which counts share one pass, and what keeps two apart.

use {
    smallvec::SmallVec,
    typed::{
        fusion::fuse,
        ir::{Arith, Builder, Compare, Constant, Function, Item, Local, Op, Terminator, Type},
    },
};

const WHOLES: Type = Type::Vector {
    item: Item::I64,
    array: true,
};
const LOGICALS: Type = Type::Vector {
    item: Item::Lgl,
    array: true,
};

/// The count of the true items of `list`, as the lowerer emits it.
fn count(builder: &mut Builder, list: Local) -> Local {
    let count = builder.local(Type::I64);
    builder.push(
        None,
        Op::CountTrue {
            program: SmallVec::new(),
            inputs: SmallVec::from_slice(&[list]),
            counts: SmallVec::from_slice(&[(0, count)]),
        },
    );
    count
}

fn greater(builder: &mut Builder, list: Local, bound: Local) -> Local {
    builder.emit(
        LOGICALS,
        Op::VectorCompare(Compare::Gt, Item::I64, list, bound),
    )
}

fn sum(builder: &mut Builder, counts: &[Local]) -> Local {
    let mut total = counts[0];
    for &count in &counts[1..] {
        total = builder.emit(Type::I64, Op::Arith(Arith::Add, Item::I64, total, count));
    }
    total
}

/// The inputs, steps and results of each count of `function`, in order.
fn passes(function: &Function) -> Vec<(usize, usize, usize)> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.op {
            Op::CountTrue {
                program,
                inputs,
                counts,
            } => Some((inputs.len(), program.len(), counts.len())),
            _ => None,
        })
        .collect()
}

fn list_operations(function: &Function) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|inst| matches!(inst.op, Op::VectorCompare(..) | Op::VectorLogic(..)))
        .count()
}

#[test]
fn counts_in_a_row_share_one_pass_and_their_common_steps() {
    // The log-rank statistic's body: `time`, `event`, `treated` and the event time `t`.
    let mut builder = Builder::new(
        String::from("test"),
        &[WHOLES, LOGICALS, LOGICALS, Type::I64],
    );
    let (time, event, treated, t) = (Local(0), Local(1), Local(2), Local(3));
    let risk = builder.emit(LOGICALS, Op::VectorCompare(Compare::Ge, Item::I64, time, t));
    let at = builder.emit(LOGICALS, Op::VectorCompare(Compare::Eq, Item::I64, time, t));
    let died = builder.emit(LOGICALS, Op::VectorLogic(true, at, event));
    let at_risk = count(&mut builder, risk);
    let risk_treated = builder.emit(LOGICALS, Op::VectorLogic(true, risk, treated));
    let treated_at_risk = count(&mut builder, risk_treated);
    let deaths = count(&mut builder, died);
    let died_treated = builder.emit(LOGICALS, Op::VectorLogic(true, died, treated));
    let treated_deaths = count(&mut builder, died_treated);
    let events = count(&mut builder, event);
    let total = sum(
        &mut builder,
        &[at_risk, treated_at_risk, deaths, treated_deaths, events],
    );
    builder.terminate(Terminator::Return(total));
    let mut function = builder.function;
    fuse(&mut function);
    // Four inputs, and five steps: each comparison once, and the three `&`.
    assert_eq!(passes(&function), [(4, 5, 5)]);
    assert_eq!(list_operations(&function), 0);
}

#[test]
fn a_count_that_reads_an_earlier_count_keeps_its_own_pass() {
    let mut builder = Builder::new(String::from("test"), &[WHOLES, Type::I64]);
    let (list, bound) = (Local(0), Local(1));
    let above = greater(&mut builder, list, bound);
    let first = count(&mut builder, above);
    let above_first = greater(&mut builder, list, first);
    let second = count(&mut builder, above_first);
    let total = sum(&mut builder, &[first, second]);
    builder.terminate(Terminator::Return(total));
    let mut function = builder.function;
    fuse(&mut function);
    assert_eq!(passes(&function), [(2, 1, 1), (2, 1, 1)]);
}

#[test]
fn an_input_assigned_between_two_counts_keeps_them_apart() {
    let mut builder = Builder::new(String::from("test"), &[WHOLES, Type::I64]);
    let list = Local(0);
    let bound = builder.emit(Type::I64, Op::Copy(Local(1)));
    let above = greater(&mut builder, list, bound);
    let first = count(&mut builder, above);
    builder.push(Some(bound), Op::Const(Constant::I64(7)));
    let above_seven = greater(&mut builder, list, bound);
    let second = count(&mut builder, above_seven);
    let total = sum(&mut builder, &[first, second]);
    builder.terminate(Terminator::Return(total));
    let mut function = builder.function;
    fuse(&mut function);
    assert_eq!(passes(&function), [(2, 1, 1), (2, 1, 1)]);
}

#[test]
fn a_call_between_two_counts_keeps_them_apart() {
    let mut builder = Builder::new(String::from("test"), &[WHOLES, Type::I64, Type::I64]);
    let (list, low, high) = (Local(0), Local(1), Local(2));
    let above_low = greater(&mut builder, list, low);
    let first = count(&mut builder, above_low);
    builder.push(None, Op::CallR(0, SmallVec::new()));
    let above_high = greater(&mut builder, list, high);
    let second = count(&mut builder, above_high);
    let total = sum(&mut builder, &[first, second]);
    builder.terminate(Terminator::Return(total));
    let mut function = builder.function;
    fuse(&mut function);
    assert_eq!(passes(&function), [(2, 1, 1), (2, 1, 1)]);
}

#[test]
fn a_result_read_between_two_counts_keeps_them_apart() {
    // The second count writes a variable that is read, with its earlier value, after the first.
    let mut builder = Builder::new(String::from("test"), &[WHOLES, Type::I64, Type::I64]);
    let (list, low, high) = (Local(0), Local(1), Local(2));
    let variable = builder.emit(Type::I64, Op::Const(Constant::I64(0)));
    let above_low = greater(&mut builder, list, low);
    let first = count(&mut builder, above_low);
    let earlier = builder.emit(Type::I64, Op::Copy(variable));
    let above_high = greater(&mut builder, list, high);
    builder.push(
        None,
        Op::CountTrue {
            program: SmallVec::new(),
            inputs: SmallVec::from_slice(&[above_high]),
            counts: SmallVec::from_slice(&[(0, variable)]),
        },
    );
    let total = sum(&mut builder, &[first, earlier, variable]);
    builder.terminate(Terminator::Return(total));
    let mut function = builder.function;
    fuse(&mut function);
    assert_eq!(passes(&function), [(2, 1, 1), (2, 1, 1)]);
}

#[test]
fn counts_across_a_jump_share_one_pass_and_counts_across_a_branch_do_not() {
    let mut builder = Builder::new(String::from("test"), &[WHOLES, Type::I64, Type::I64]);
    let (list, low, high) = (Local(0), Local(1), Local(2));
    let above_low = greater(&mut builder, list, low);
    let first = count(&mut builder, above_low);
    let next = builder.block();
    builder.terminate(Terminator::Jump(next));
    builder.switch_to(next);
    let above_high = greater(&mut builder, list, high);
    let second = count(&mut builder, above_high);
    let condition = builder.emit(
        Type::Bool,
        Op::Compare(Compare::Gt, Item::I64, first, second),
    );
    let (then, otherwise) = (builder.block(), builder.block());
    builder.terminate(Terminator::Branch {
        condition,
        then,
        otherwise,
    });
    builder.switch_to(then);
    let below = builder.emit(
        LOGICALS,
        Op::VectorCompare(Compare::Lt, Item::I64, list, low),
    );
    let third = count(&mut builder, below);
    builder.terminate(Terminator::Return(third));
    builder.switch_to(otherwise);
    builder.terminate(Terminator::Return(second));
    let mut function = builder.function;
    fuse(&mut function);
    assert_eq!(passes(&function), [(3, 2, 2), (2, 1, 1)]);
}
