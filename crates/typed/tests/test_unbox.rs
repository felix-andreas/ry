//! The unboxing pass on hand-built IR: which boxes it removes, and which uses keep a box.

use {
    smallvec::SmallVec,
    typed::{
        ir::{Builder, Function, Item, Local, Op, Terminator, Type},
        shapes,
        unbox::unbox,
    },
};

fn pair() -> u32 {
    match shapes::intern(0, &[Item::Dbl, Item::Dbl]) {
        Some(shape) => shape,
        None => panic!("a pair of doubles fits a box"),
    }
}

fn construct(builder: &mut Builder, shape: u32, fields: &[Local]) -> Local {
    builder.emit(
        Type::Record(shape),
        Op::Construct(shape, SmallVec::from_slice(fields)),
    )
}

fn constructs(function: &Function) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|inst| matches!(inst.op, Op::Construct(..)))
        .count()
}

fn field_reads(function: &Function) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter(|inst| matches!(inst.op, Op::Field(..)))
        .count()
}

#[test]
fn a_box_only_read_through_copies_is_removed() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, Type::Dbl]);
    let record = construct(&mut builder, shape, &[Local(0), Local(1)]);
    let variable = builder.emit(Type::Record(shape), Op::Copy(record));
    let argument = builder.emit(Type::Record(shape), Op::Copy(variable));
    let second = builder.emit(Type::Dbl, Op::Field(argument, shape, 1));
    builder.terminate(Terminator::Return(second));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 0);
    assert_eq!(field_reads(&function), 0);
    // The read copies the local that holds the second field since the construction.
    let block = &function.blocks[0];
    let Some(&Op::Copy(held)) = block.insts.last().map(|inst| &inst.op) else {
        panic!("the field read is a copy: {:?}", block.insts);
    };
    assert!(
        block
            .insts
            .iter()
            .any(|inst| inst.dest == Some(held) && matches!(inst.op, Op::Copy(Local(1))))
    );
}

#[test]
fn a_read_sees_the_field_as_it_was_at_the_construction() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl]);
    let variable = builder.emit(Type::Dbl, Op::Copy(Local(0)));
    let record = construct(&mut builder, shape, &[variable, variable]);
    // The variable the box was built from changes before the box is read.
    builder.push(Some(variable), Op::Math(typed::ir::Math::Neg, Item::Dbl, variable));
    let read = builder.emit(Type::Dbl, Op::Field(record, shape, 0));
    builder.terminate(Terminator::Return(read));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 0);
    let Some(&Op::Copy(held)) = function.blocks[0].insts.last().map(|inst| &inst.op) else {
        panic!("the field read is a copy");
    };
    assert_ne!(held, variable);
}

#[test]
fn a_box_that_leaves_the_function_stays() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl]);
    let record = construct(&mut builder, shape, &[Local(0), Local(0)]);
    builder.emit(Type::Dbl, Op::Field(record, shape, 0));
    builder.terminate(Terminator::Return(record));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 1);
    assert_eq!(field_reads(&function), 1);
}

#[test]
fn a_box_in_a_variable_assigned_twice_stays() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl]);
    let first = construct(&mut builder, shape, &[Local(0), Local(0)]);
    let variable = builder.emit(Type::Record(shape), Op::Copy(first));
    let second = construct(&mut builder, shape, &[Local(0), Local(0)]);
    builder.push(Some(variable), Op::Copy(second));
    let read = builder.emit(Type::Dbl, Op::Field(variable, shape, 0));
    builder.terminate(Terminator::Return(read));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 2);
}

#[test]
fn a_read_before_the_construction_in_a_loop_keeps_the_box() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, Type::Bool]);
    let body = builder.block();
    let exit = builder.block();
    let record = builder.local(Type::Record(shape));
    builder.terminate(Terminator::Jump(body));
    builder.switch_to(body);
    // The read runs before the construction, so from the second pass on it reads the box of
    // the pass before.
    let read = builder.emit(Type::Dbl, Op::Field(record, shape, 0));
    builder.push(
        Some(record),
        Op::Construct(shape, SmallVec::from_slice(&[read, Local(0)])),
    );
    builder.terminate(Terminator::Branch {
        condition: Local(1),
        then: body,
        otherwise: exit,
    });
    builder.switch_to(exit);
    builder.terminate(Terminator::Return(read));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 1);
}

#[test]
fn a_box_built_on_one_branch_and_read_after_the_join_stays() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, Type::Bool]);
    let then = builder.block();
    let join = builder.block();
    let record = builder.local(Type::Record(shape));
    builder.terminate(Terminator::Branch {
        condition: Local(1),
        then,
        otherwise: join,
    });
    builder.switch_to(then);
    builder.push(
        Some(record),
        Op::Construct(shape, SmallVec::from_slice(&[Local(0), Local(0)])),
    );
    builder.terminate(Terminator::Jump(join));
    builder.switch_to(join);
    let read = builder.emit(Type::Dbl, Op::Field(record, shape, 0));
    builder.terminate(Terminator::Return(read));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 1);
}

#[test]
fn a_box_held_only_by_a_removed_box_is_removed_too() {
    let inner = pair();
    let outer = match shapes::intern(1, &[Item::Record(inner), Item::Dbl]) {
        Some(shape) => shape,
        None => panic!("a record and a double fit a box"),
    };
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl]);
    let point = construct(&mut builder, inner, &[Local(0), Local(0)]);
    let holder = construct(&mut builder, outer, &[point, Local(0)]);
    let held = builder.emit(Type::Record(inner), Op::Field(holder, outer, 0));
    let read = builder.emit(Type::Dbl, Op::Field(held, inner, 1));
    builder.terminate(Terminator::Return(read));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 0);
    assert_eq!(field_reads(&function), 0);
}
