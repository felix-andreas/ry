//! The unboxing pass on hand-built IR: which boxes it removes, and which uses keep a box.

use {
    smallvec::SmallVec,
    typed::{
        ir::{Builder, Constant, Function, Item, Local, Op, Terminator, Type},
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
    // The read copies the variable of the second field, through the copy each assignment of the
    // box made of it.
    let block = &function.blocks[0];
    let Some(&Op::Copy(mut held)) = block.insts.last().map(|inst| &inst.op) else {
        panic!("the field read is a copy: {:?}", block.insts);
    };
    while let Some(&Op::Copy(source)) = block
        .insts
        .iter()
        .find(|inst| inst.dest == Some(held))
        .map(|inst| &inst.op)
    {
        held = source;
    }
    assert_eq!(held, Local(1));
}

#[test]
fn a_read_sees_the_field_as_it_was_at_the_construction() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl]);
    let variable = builder.emit(Type::Dbl, Op::Copy(Local(0)));
    let record = construct(&mut builder, shape, &[variable, variable]);
    // The variable the box was built from changes before the box is read.
    builder.push(
        Some(variable),
        Op::Math(typed::ir::Math::Neg, Item::Dbl, variable),
    );
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
fn a_variable_assigned_two_boxes_reads_the_one_it_holds() {
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
    assert_eq!(constructs(&function), 0);
    assert_eq!(field_reads(&function), 0);
    // The read copies the variable's own field, which both assignments of the variable assign.
    let insts = &function.blocks[0].insts;
    let Some(&Op::Copy(held)) = insts.last().map(|inst| &inst.op) else {
        panic!("the field read is a copy: {insts:?}");
    };
    let assignments = insts.iter().filter(|inst| inst.dest == Some(held)).count();
    assert_eq!(assignments, 2);
}

#[test]
fn a_read_of_the_box_of_the_pass_before_reads_its_fields() {
    let shape = pair();
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, Type::Bool]);
    let body = builder.block();
    let exit = builder.block();
    let record = construct(&mut builder, shape, &[Local(0), Local(0)]);
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
    assert_eq!(constructs(&function), 0);
    assert_eq!(field_reads(&function), 0);
    // The read copies the field's variable, and the construction after it assigns that variable
    // the value just read.
    let insts = &function.blocks[1].insts;
    let Some(&Op::Copy(held)) = insts.first().map(|inst| &inst.op) else {
        panic!("the field read is a copy: {insts:?}");
    };
    assert!(insts.iter().any(
        |inst| inst.dest == Some(held) && matches!(inst.op, Op::Copy(source) if source == read)
    ));
}

/// `:Ok(value)` and `:Err(reason)`, as the shapes of two tags with one payload each.
fn ok_and_err() -> (u32, u32) {
    match (
        shapes::intern(10, &[Item::Dbl]),
        shapes::intern(11, &[Item::I64]),
    ) {
        (Some(ok), Some(err)) => (ok, err),
        _ => panic!("a tag with one payload fits a box"),
    }
}

/// A function that builds `:Ok` on one branch and `:Err` on the other, assigns either to one
/// variable, and after the join tests the variable's shape and reads the payload of `:Ok`.
/// Returns the function and the variable.
fn built_on_two_branches() -> (Builder, Local) {
    let (ok, err) = ok_and_err();
    let union = Type::Tag(shapes::union_of(&[ok, err]));
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, Type::Bool, Type::I64]);
    let (then, otherwise, join) = (builder.block(), builder.block(), builder.block());
    let result = builder.local(union);
    builder.terminate(Terminator::Branch {
        condition: Local(1),
        then,
        otherwise,
    });
    builder.switch_to(then);
    let built = builder.emit(
        Type::Tag(shapes::union_of(&[ok])),
        Op::Construct(ok, SmallVec::from_slice(&[Local(0)])),
    );
    builder.push(Some(result), Op::Copy(built));
    builder.terminate(Terminator::Jump(join));
    builder.switch_to(otherwise);
    let built = builder.emit(
        Type::Tag(shapes::union_of(&[err])),
        Op::Construct(err, SmallVec::from_slice(&[Local(2)])),
    );
    builder.push(Some(result), Op::Copy(built));
    builder.terminate(Terminator::Jump(join));
    builder.switch_to(join);
    builder.emit(Type::Int, Op::ShapeOf(result));
    let read = builder.emit(Type::Dbl, Op::Field(result, ok, 0));
    builder.terminate(Terminator::Return(read));
    (builder, result)
}

#[test]
fn boxes_of_two_shapes_built_on_two_branches_and_read_after_the_join_are_removed() {
    let (ok, err) = ok_and_err();
    let (builder, _) = built_on_two_branches();
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 0);
    assert_eq!(field_reads(&function), 0);
    // Each branch assigns the shape it built, and the test after the join reads that variable.
    let shape_of = |block: usize| {
        function.blocks[block]
            .insts
            .iter()
            .find_map(|inst| match inst.op {
                Op::Const(Constant::Int(shape)) => Some(shape as u32),
                _ => None,
            })
    };
    assert_eq!(shape_of(1), Some(ok));
    assert_eq!(shape_of(2), Some(err));
    assert!(
        function.blocks[3]
            .insts
            .iter()
            .all(|inst| matches!(inst.op, Op::Copy(_)))
    );
    // Nothing reads the payload of `:Err`, so no variable holds it.
    assert!(
        !function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| matches!(inst.op, Op::Copy(Local(2))))
    );
}

#[test]
fn a_variable_that_leaves_the_function_keeps_every_box_copied_into_it() {
    let (mut builder, result) = built_on_two_branches();
    builder.terminate(Terminator::Return(result));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 2);
    assert_eq!(field_reads(&function), 1);
}

#[test]
fn a_tag_without_a_payload_joins_a_box_as_its_shape() {
    let (ok, _) = ok_and_err();
    let Some(none) = shapes::intern(12, &[]) else {
        panic!("a tag without a payload is a shape");
    };
    let union = Type::Tag(shapes::union_of(&[ok, none]));
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, Type::Bool]);
    let (then, join) = (builder.block(), builder.block());
    let constant = Constant::Object(shapes::constant(none));
    let result = builder.emit(union, Op::Const(constant));
    builder.terminate(Terminator::Branch {
        condition: Local(1),
        then,
        otherwise: join,
    });
    builder.switch_to(then);
    builder.push(
        Some(result),
        Op::Construct(ok, SmallVec::from_slice(&[Local(0)])),
    );
    builder.terminate(Terminator::Jump(join));
    builder.switch_to(join);
    let shape = builder.emit(Type::Int, Op::ShapeOf(result));
    builder.terminate(Terminator::Return(shape));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 0);
    let Some(Op::Const(Constant::Int(held))) =
        function.blocks[0].insts.first().map(|inst| &inst.op)
    else {
        panic!("the tag is its shape: {:?}", function.blocks[0].insts);
    };
    assert_eq!(*held as u32, none);
}

#[test]
fn a_box_an_instruction_other_than_a_construction_assigns_stays() {
    let shape = pair();
    let list = Type::Vector {
        item: Item::Record(shape),
        array: true,
    };
    let mut builder = Builder::new(String::from("test"), &[Type::Dbl, list, Type::I64]);
    let record = construct(&mut builder, shape, &[Local(0), Local(0)]);
    // The variable also holds a box out of a list, whose fields live in that box.
    builder.push(Some(record), Op::ItemAt(Local(1), Local(2)));
    let read = builder.emit(Type::Dbl, Op::Field(record, shape, 0));
    builder.terminate(Terminator::Return(read));
    let mut function = builder.function;
    unbox(&mut function);
    assert_eq!(constructs(&function), 1);
    assert_eq!(field_reads(&function), 1);
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
