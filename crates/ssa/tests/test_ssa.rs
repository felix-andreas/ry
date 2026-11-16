//! Random structured programs over mutable variables, with branches, counted loops and guards,
//! built through the IR's builder, compiled and run, and compared with a direct evaluation of
//! the same program. The programs read variables before defining them, define them on one arm
//! only, and leave through guards mid-block, which are the shapes SSA construction gets wrong.

use {
    ssa::{AbiParam, Block, Builder, CallConv, IntCC, Signature, Target, Variable, types::I64},
    syntax::testing::{SplitMix64, for_each_seed},
};

const VARIABLES: usize = 4;

enum Statement {
    Assign(usize, Expression),
    If(usize, i64, Vec<Statement>, Vec<Statement>),
    Loop(u8, Vec<Statement>),
    /// Leaves with the guard's exit value unless the variable differs from the constant.
    Guard(usize, i64),
}

enum Expression {
    Constant(i64),
    Copy(usize),
    Add(usize, usize),
    Sub(usize, usize),
    MulConstant(usize, i64),
    XorConstant(usize, i64),
}

#[test]
fn fuzz_compiled_programs_agree_with_evaluation() {
    for_each_seed(400, |seed| {
        let mut rng = SplitMix64(seed);
        let program = statements(&mut rng, 0);
        let address = compile(&program);
        // SAFETY: the function was built with exactly this signature.
        let compiled = unsafe {
            std::mem::transmute::<*const u8, extern "C" fn(i64, i64) -> i64>(address as *const u8)
        };
        for (a, b) in [(0, 0), (1, -1), (7, 3), (-40, 12), (i64::MAX, 2)] {
            let expected = evaluate(&program, a, b);
            assert_eq!(
                compiled(a, b),
                expected,
                "seed {seed}, arguments ({a}, {b})"
            );
        }
    });
}

fn statements(rng: &mut SplitMix64, depth: usize) -> Vec<Statement> {
    let count = 1 + rng.below(4);
    (0..count).map(|_| statement(rng, depth)).collect()
}

fn statement(rng: &mut SplitMix64, depth: usize) -> Statement {
    let variable = rng.below(VARIABLES);
    let constant = rng.below(9) as i64 - 4;
    match rng.below(if depth < 3 { 7 } else { 4 }) {
        0..=2 => Statement::Assign(variable, expression(rng)),
        3 => Statement::Guard(variable, constant),
        4 | 5 => Statement::If(
            variable,
            constant,
            statements(rng, depth + 1),
            if rng.chance(1, 3) {
                Vec::new()
            } else {
                statements(rng, depth + 1)
            },
        ),
        _ => Statement::Loop(rng.below(4) as u8, statements(rng, depth + 1)),
    }
}

fn expression(rng: &mut SplitMix64) -> Expression {
    let (left, right) = (rng.below(VARIABLES), rng.below(VARIABLES));
    let constant = rng.below(11) as i64 - 5;
    match rng.below(6) {
        0 => Expression::Constant(constant),
        1 => Expression::Copy(left),
        2 => Expression::Add(left, right),
        3 => Expression::Sub(left, right),
        4 => Expression::MulConstant(left, constant),
        _ => Expression::XorConstant(left, constant),
    }
}

/// The program's result, or the exit value of the first guard that fails.
fn evaluate(program: &[Statement], a: i64, b: i64) -> i64 {
    let mut variables = [a, b, 0, 0];
    match run(program, &mut variables) {
        Ok(()) => result(&variables),
        Err(exit) => exit,
    }
}

fn run(program: &[Statement], variables: &mut [i64; VARIABLES]) -> Result<(), i64> {
    for statement in program {
        match statement {
            Statement::Assign(target, expression) => {
                let read = |index: &usize| variables.get(*index).copied().unwrap_or(0);
                let value = match expression {
                    Expression::Constant(constant) => *constant,
                    Expression::Copy(source) => read(source),
                    Expression::Add(left, right) => read(left).wrapping_add(read(right)),
                    Expression::Sub(left, right) => read(left).wrapping_sub(read(right)),
                    Expression::MulConstant(source, constant) => {
                        read(source).wrapping_mul(*constant)
                    }
                    Expression::XorConstant(source, constant) => read(source) ^ constant,
                };
                if let Some(slot) = variables.get_mut(*target) {
                    *slot = value;
                }
            }
            Statement::If(variable, constant, then, otherwise) => {
                if variables.get(*variable).copied().unwrap_or(0) < *constant {
                    run(then, variables)?;
                } else {
                    run(otherwise, variables)?;
                }
            }
            Statement::Loop(count, body) => {
                for _ in 0..*count {
                    run(body, variables)?;
                }
            }
            Statement::Guard(variable, constant) => {
                if variables.get(*variable).copied().unwrap_or(0) == *constant {
                    return Err(exit_value(variables));
                }
            }
        }
    }
    Ok(())
}

fn result(variables: &[i64; VARIABLES]) -> i64 {
    variables
        .iter()
        .enumerate()
        .fold(0i64, |sum, (index, &value)| {
            sum.wrapping_add(value.wrapping_mul(index as i64 + 1))
        })
}

fn exit_value(variables: &[i64; VARIABLES]) -> i64 {
    result(variables) ^ 0x5555
}

fn compile(program: &[Statement]) -> usize {
    let mut signature = Signature::new(CallConv::Host);
    signature.params.extend([AbiParam::new(I64); 2]);
    signature.returns.push(AbiParam::new(I64));
    let mut builder = Builder::new(signature.clone());
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let variables: Vec<Variable> = (0..VARIABLES).map(|_| builder.declare_var(I64)).collect();
    let params: Vec<_> = builder.block_params(entry).to_vec();
    for (&variable, &param) in variables.iter().zip(&params) {
        builder.def_var(variable, param);
    }
    emit(&mut builder, &variables, program);
    let value = sum(&mut builder, &variables);
    builder.ins().return_(&[value]);
    builder.seal_all_blocks();
    let function = builder.finalize();
    let compiled = ssa::with_module(
        ssa::Allocator::Backtracking,
        |module| -> Result<usize, ssa::Error> {
            let id = module.declare(&signature)?;
            module.define(id, &function)?;
            module.finalize()?;
            Ok(module.address(id))
        },
    );
    match compiled {
        Ok(address) => address,
        Err(error) => panic!("{error:?}\n{function}"),
    }
}

fn emit(builder: &mut Builder, variables: &[Variable], program: &[Statement]) {
    let var = |index: usize| {
        variables
            .get(index)
            .copied()
            .unwrap_or(Variable::from_u32(0))
    };
    for statement in program {
        match statement {
            Statement::Assign(target, expression) => {
                let value = match *expression {
                    Expression::Constant(constant) => builder.ins().iconst(I64, constant),
                    Expression::Copy(source) => builder.use_var(var(source)),
                    Expression::Add(left, right) => {
                        let (left, right) =
                            (builder.use_var(var(left)), builder.use_var(var(right)));
                        builder.ins().iadd(left, right)
                    }
                    Expression::Sub(left, right) => {
                        let (left, right) =
                            (builder.use_var(var(left)), builder.use_var(var(right)));
                        builder.ins().isub(left, right)
                    }
                    Expression::MulConstant(source, constant) => {
                        let source = builder.use_var(var(source));
                        builder.ins().imul_imm_s(source, constant)
                    }
                    Expression::XorConstant(source, constant) => {
                        let source = builder.use_var(var(source));
                        builder.ins().bxor_imm_s(source, constant)
                    }
                };
                builder.def_var(var(*target), value);
            }
            Statement::If(variable, constant, then, otherwise) => {
                let value = builder.use_var(var(*variable));
                let condition = builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, value, *constant);
                let (then_block, otherwise_block, join) = (
                    builder.create_block(),
                    builder.create_block(),
                    builder.create_block(),
                );
                builder
                    .ins()
                    .brif(condition, then_block, &[], otherwise_block, &[]);
                for (block, body) in [(then_block, then), (otherwise_block, otherwise)] {
                    builder.switch_to_block(block);
                    emit(builder, variables, body);
                    builder.ins().jump(join, &[]);
                }
                builder.switch_to_block(join);
            }
            Statement::Loop(count, body) => {
                let counter = builder.declare_var(I64);
                let zero = builder.ins().iconst(I64, 0);
                builder.def_var(counter, zero);
                let (header, body_block, exit) = (
                    builder.create_block(),
                    builder.create_block(),
                    builder.create_block(),
                );
                builder.ins().jump(header, &[]);
                builder.switch_to_block(header);
                let current = builder.use_var(counter);
                let more =
                    builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedLessThan, current, i64::from(*count));
                builder.ins().brif(more, body_block, &[], exit, &[]);
                builder.switch_to_block(body_block);
                emit(builder, variables, body);
                let current = builder.use_var(counter);
                let next = builder.ins().iadd_imm_s(current, 1);
                builder.def_var(counter, next);
                builder.ins().jump(header, &[]);
                builder.switch_to_block(exit);
            }
            Statement::Guard(variable, constant) => {
                let value = builder.use_var(var(*variable));
                let holds = builder.ins().icmp_imm_s(IntCC::NotEqual, value, *constant);
                let exit: Block = builder.create_block();
                builder.ins().guard(holds, exit, &[]);
                let continuation = builder.current_block();
                builder.switch_to_block(exit);
                let value = sum(builder, variables);
                let value = builder.ins().bxor_imm_s(value, 0x5555);
                builder.ins().return_(&[value]);
                if let Some(continuation) = continuation {
                    builder.switch_to_block(continuation);
                }
            }
        }
    }
}

fn sum(builder: &mut Builder, variables: &[Variable]) -> ssa::Value {
    let mut total = builder.ins().iconst(I64, 0);
    for (index, &variable) in variables.iter().enumerate() {
        let value = builder.use_var(variable);
        let scaled = builder.ins().imul_imm_s(value, index as i64 + 1);
        total = builder.ins().iadd(total, scaled);
    }
    total
}

/// A relocatable object of two functions, where the outer one loads a word from the offset
/// table, calls a host function through another, and calls the inner one directly. Mapped
/// below a table that holds those words, it runs without any relocation applied at load, and
/// the JIT module refuses the table load instead of compiling it.
#[test]
fn relocatable_code_runs_against_its_table() {
    extern "C" fn triple(value: i64) -> i64 {
        3 * value
    }
    let mut host = Signature::new(CallConv::Host);
    host.params.push(AbiParam::new(I64));
    host.returns.push(AbiParam::new(I64));
    let mut object = ssa::Relocatable::new().expect("a relocatable object");
    let inner = Target::declare(&mut object, &host).expect("declare the inner function");
    let outer = Target::declare(&mut object, &host).expect("declare the outer function");
    let mut builder = Builder::new(host.clone());
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let value = builder.block_params(entry)[0];
    let doubled = builder.ins().iadd(value, value);
    builder.ins().return_(&[doubled]);
    builder.seal_all_blocks();
    Target::define(&mut object, inner, &builder.finalize()).expect("define the inner function");
    let outer_body = |builder: &mut Builder| {
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let value = builder.block_params(entry)[0];
        let callee = builder.import_function(inner, host.clone());
        let call = builder.ins().call(callee, &[value]);
        let doubled = builder.inst_results(call)[0];
        let signature = builder.import_signature(host.clone());
        let address = builder.ins().table_word(1);
        let call = builder.ins().call_indirect(signature, address, &[doubled]);
        let tripled = builder.inst_results(call)[0];
        let added = builder.ins().table_word(0);
        let sum = builder.ins().iadd(tripled, added);
        builder.ins().return_(&[sum]);
        builder.seal_all_blocks();
    };
    let mut builder = Builder::new(host.clone());
    outer_body(&mut builder);
    let function = builder.finalize();
    Target::define(&mut object, outer, &function).expect("define the outer function");
    let refused = ssa::with_module(ssa::Allocator::Backtracking, |module| {
        let id = module.declare(&host)?;
        module.define(id, &function).map(|_| ())
    });
    assert!(refused.is_err(), "the JIT module compiled a table load");
    let linked = object.finish(&mut |_| None);
    let start = |id: ssa::FuncId| match linked.functions[id.as_u32() as usize] {
        Ok(offset) => offset as usize,
        Err(error) => panic!("left out: {error:?}"),
    };
    let (inner_start, outer_start) = (start(inner), start(outer));
    assert!(inner_start < outer_start);
    const TABLE: usize = 1 << 16;
    let size = TABLE + linked.code.len().next_multiple_of(1 << 16);
    // SAFETY: a fresh private mapping, written before it is made executable.
    unsafe {
        let base = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        ) as *mut u8;
        assert_ne!(base as *mut libc::c_void, libc::MAP_FAILED);
        let code = base.add(TABLE);
        std::ptr::copy_nonoverlapping(linked.code.as_ptr(), code, linked.code.len());
        *(code.sub(8) as *mut i64) = 1000;
        *(code.sub(16) as *mut usize) = triple as *const () as usize;
        assert_eq!(
            libc::mprotect(
                code as *mut libc::c_void,
                size - TABLE,
                libc::PROT_READ | libc::PROT_EXEC
            ),
            0
        );
        let function =
            std::mem::transmute::<*const u8, extern "C" fn(i64) -> i64>(code.add(outer_start));
        assert_eq!(function(7), 3 * 14 + 1000);
    }
}
