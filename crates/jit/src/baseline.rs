//! The baseline tier: a single pass from tier-0 bytecode to machine code.
//!
//! Baseline code keeps tier 0's activation exactly: the same register window, the same frame,
//! the same inline caches and type feedback. Each instruction becomes a call to the step that
//! runs it in tier 0 ([`bytecode::BASELINE_STEPS`]), followed by direct branches on the pc the
//! step answers, so the code replaces tier 0's dispatch (decoding the op word, an indirect jump
//! per instruction) and nothing else. A handful of instructions that only move values or test a
//! header byte run inline. Because no value ever lives anywhere tier 0 would not find it, the
//! code needs no deoptimization metadata: any pc it does not expect (a `break` caught in a
//! loop, an on-stack replacement that resumed elsewhere) goes through a jump table indexed by
//! pc, and any pc is a valid place to leave or enter.
//!
//! Compiling costs one pass over the instruction words and one copy into executable memory, a
//! few microseconds for a typical body, so bodies get baseline code after a few hundred
//! entries or backward jumps, long before the optimizing tier would compile them.

use {
    crate::{
        assembler::{Arch, Assembler, Condition, Label, Reg, Width},
        logging,
    },
    heap::{
        env::layout as frame,
        object::{ENVIRONMENT_FRAME_OFFSET, Sexp, SexpType, VECTOR_DATA_OFFSET, gp, header_layout},
    },
    interp::{
        bytecode::{
            self, ARITHS, Arith, BASELINE, BASELINE_STEPS, Machine, Op, baseline_layout,
            operand_count,
        },
        heap,
        values::{self, missing_arg, nil, unbound},
    },
    std::{cell::RefCell, ffi::c_void, time::Instant},
};

/// Bodies longer than this many instruction words stay in tier 0: their code would be large,
/// and so long a body is rarely the hot part of a program.
const MAX_WORDS: usize = 1 << 16;

/// An `EndStatement` clears at most this many registers inline.
const MAX_INLINE_CLEARS: usize = 8;

/// The header byte offsets the inline paths read (see `heap::object::Header`).
const KIND: u8 = 0;
const FLAGS: u8 = 2;
const LENGTH: u8 = 16;
const DATA: u8 = VECTOR_DATA_OFFSET as u8;
const OBJECT_FLAG: u8 = 1 << 0;
const ALTREP_FLAG: u8 = 1 << 1;

/// Compiles baseline code for the activation's body and stores it in the body's `BASELINE`
/// constant, or stores the unbound marker there when the body does not compile, so it is not
/// tried again.
pub unsafe fn compile(machine: &Machine<'_>) {
    unsafe {
        let constants = machine.code.cdr();
        let started = Instant::now();
        let result = Arch::host()
            .ok_or("this host has no baseline backend")
            .and_then(|arch| assemble(arch, machine.code))
            .and_then(|bytes| install(&bytes).map(|address| (address, bytes.len())));
        let heap = heap();
        match result {
            Ok((address, size)) => match heap.alloc_external_ptr(address, nil(), nil()) {
                Ok(holder) => {
                    heap.set_element(constants, BASELINE, holder);
                    if logging() {
                        eprintln!(
                            "jit: baseline {} in {} us ({size} bytes)",
                            crate::describe(machine.code),
                            started.elapsed().as_micros()
                        );
                    }
                }
                Err(_) => heap.set_element(constants, BASELINE, unbound()),
            },
            Err(reason) => {
                if logging() {
                    eprintln!(
                        "jit: no baseline code for {}: {reason}",
                        crate::describe(machine.code)
                    );
                }
                heap.set_element(constants, BASELINE, unbound());
            }
        }
    }
}

/// The machine code for `code`, for `arch`.
pub unsafe fn assemble(arch: Arch, code: heap::object::Sexp) -> Result<Vec<u8>, &'static str> {
    unsafe {
        let words = values::ints(code.car());
        if words.len() > MAX_WORDS {
            return Err("the body is too long");
        }
        let constants = code.cdr();
        let mut asm = Assembler::new(arch);
        let dispatch = asm.label();
        let exit = asm.label();
        // One label per word; only instruction starts are ever bound or jumped to.
        let labels: Vec<Label> = (0..words.len()).map(|_| asm.label()).collect();
        let mut starts = vec![false; words.len()];
        let mut pc = 0;
        while pc < words.len() {
            let op = bytecode::op_of(words[pc] as u32).ok_or("an unknown opcode")?;
            starts[pc] = true;
            pc += 1 + operand_count(op);
        }
        if pc != words.len() {
            return Err("a truncated instruction");
        }
        asm.prologue(
            BASELINE_STEPS.as_ptr() as u64,
            baseline_layout::REGISTERS as u32,
            baseline_layout::INTERP as u32,
        );
        asm.jump(dispatch);
        let visible = baseline_layout::VISIBLE as u32;
        let nil_address = nil().as_ptr() as u64;
        // Slow paths of inline instructions, placed after the body so the fast paths stay dense.
        let mut cold: Vec<(Label, Op, usize, usize)> = Vec::new();
        let mut pc = 0;
        while pc < words.len() {
            let op = bytecode::op_of(words[pc] as u32).ok_or("an unknown opcode")?;
            let count = operand_count(op);
            let operand = |index: usize| words[pc + 1 + index] as usize;
            let label_of = |target: usize| -> Result<Label, &'static str> {
                match starts.get(target) {
                    Some(true) => Ok(labels[target]),
                    _ => Err("a jump to the middle of an instruction"),
                }
            };
            let next = pc + 1 + count;
            asm.bind(labels[pc]);
            match op {
                Op::Jump if operand(0) > pc => asm.jump(label_of(operand(0))?),
                Op::Resume => asm.jump(label_of(operand(0))?),
                Op::Move => asm.copy_register(operand(0) as u32, operand(1) as u32),
                Op::Constant => {
                    let value = constants.element(operand(1));
                    asm.store_register_constant(operand(0) as u32, value.as_ptr() as u64);
                    asm.store_interp_byte(visible, 1);
                }
                Op::GetVar => {
                    let slow = asm.label();
                    inline_get_var(
                        &mut asm,
                        operand(0) as u32,
                        constants.element(operand(1)),
                        operand(2) as u32,
                        visible,
                        slow,
                    );
                    cold.push((slow, op, pc, next));
                }
                Op::SetVar => {
                    let slow = asm.label();
                    inline_set_var(
                        &mut asm,
                        constants.element(operand(0)),
                        operand(1) as u32,
                        operand(2) as u32,
                        visible,
                        slow,
                    );
                    cold.push((slow, op, pc, next));
                }
                Op::Arithmetic if comparison_condition(ARITHS[operand(0)].0).is_some() => {
                    let slow = asm.label();
                    let operator = ARITHS[operand(0)].0;
                    inline_comparison(
                        &mut asm,
                        operator,
                        operand(1) as u32,
                        operand(2) as u32,
                        operand(3) as u32,
                        visible,
                        slow,
                    );
                    cold.push((slow, op, pc, next));
                }
                Op::Visible => asm.store_interp_byte(visible, 1),
                Op::Invisible => asm.store_interp_byte(visible, 0),
                Op::SetSrcref if operand(1) == 0 => {
                    let srcref = constants.element(operand(0));
                    asm.store_interp_constant(
                        baseline_layout::SRCREF as u32,
                        srcref.as_ptr() as u64,
                    );
                }
                Op::EndStatement if operand(2).saturating_sub(operand(1)) <= MAX_INLINE_CLEARS => {
                    asm.store_register_constant(operand(0) as u32, nil_address);
                    for register in operand(1)..operand(2) {
                        asm.store_register_constant(register as u32, nil_address);
                    }
                }
                Op::BranchUnlessBuiltin => {
                    asm.load_object(operand(0) as u32);
                    asm.branch_object_byte(
                        KIND,
                        SexpType::Builtin as u8,
                        Condition::NotEqual,
                        label_of(operand(1))?,
                    );
                }
                Op::BranchIfObject => {
                    asm.load_object(operand(0) as u32);
                    asm.branch_object_bits(
                        FLAGS,
                        OBJECT_FLAG,
                        Condition::NotEqual,
                        label_of(operand(1))?,
                    );
                }
                Op::BranchUnlessPrimitive => {
                    let primitive = constants.element(operand(1));
                    asm.load_object(operand(0) as u32);
                    asm.branch_object_is(
                        primitive.as_ptr() as u64,
                        Condition::NotEqual,
                        label_of(operand(2))?,
                    );
                }
                Op::JumpUnless => {
                    // A plain length-one logical that is not `NA` decides the branch inline;
                    // anything else runs the step, which reports errors and coerces.
                    let slow = asm.label();
                    let target = label_of(operand(2))?;
                    asm.load_object(operand(0) as u32);
                    asm.branch_object_byte(
                        KIND,
                        SexpType::Logical as u8,
                        Condition::NotEqual,
                        slow,
                    );
                    asm.branch_object_bits(FLAGS, ALTREP_FLAG, Condition::NotEqual, slow);
                    asm.branch_object_word(LENGTH, 1, Condition::NotEqual, slow);
                    asm.branch_object_int(DATA, 0, Condition::Equal, target);
                    asm.branch_object_int(
                        DATA,
                        heap::object::NA_LOGICAL as u32,
                        Condition::NotEqual,
                        label_of(next)?,
                    );
                    asm.bind(slow);
                    asm.call_step(op as usize, pc as u32);
                    asm.branch_accumulator(Condition::Equal, operand(2) as u32, target);
                    asm.branch_accumulator(Condition::NotEqual, next as u32, dispatch);
                }
                _ => {
                    asm.call_step(op as usize, pc as u32);
                    if let Some(target) = branch_target(op, &operand)
                        && target != next
                    {
                        asm.branch_accumulator(Condition::Equal, target as u32, label_of(target)?);
                    }
                    if op_falls_through(op) && next < words.len() {
                        asm.branch_accumulator(Condition::NotEqual, next as u32, dispatch);
                    } else {
                        asm.jump(dispatch);
                    }
                }
            }
            pc = next;
        }
        for (slow, op, pc, next) in cold {
            asm.bind(slow);
            asm.call_step(op as usize, pc as u32);
            asm.branch_accumulator(Condition::Equal, next as u32, labels[next]);
            asm.jump(dispatch);
        }
        // Falling off the end of the code cannot happen: every body ends in `Return`.
        asm.bind(dispatch);
        asm.branch_if_stop(exit);
        asm.jump_through_table();
        asm.bind(exit);
        asm.epilogue();
        let targets: Vec<Label> = (0..words.len())
            .map(|pc| if starts[pc] { labels[pc] } else { exit })
            .collect();
        asm.finish(&targets)
            .map(|finished| finished.bytes)
            .map_err(|_| "the code is too large for its branches")
    }
}

/// `GetVar` for a variable the activation's own frame binds to a plain value or a forced
/// promise, which is what most lookups in a function body find. Everything else (a binding in
/// an enclosing frame, an active binding, a promise still to force, a body recording type
/// feedback) branches to `slow`, which runs the step.
fn inline_get_var(
    asm: &mut Assembler,
    to: u32,
    symbol: Sexp,
    slot: u32,
    visible: u32,
    slow: Label,
) {
    asm.load(
        Width::W64,
        Reg::T0,
        Reg::Activation,
        baseline_layout::FEEDBACK as u32,
    );
    asm.test(Reg::T0, Reg::T0);
    asm.branch(Condition::NotEqual, slow);
    // Visible before the lookup, as in tier 0; the slow path sets it again.
    asm.store_interp_byte(visible, 1);
    probe_frame(asm, symbol, slot, slow);
    asm.load(Width::W16, Reg::T2, Reg::T3, header_layout::GP as u32);
    asm.test_constant(Reg::T2, u32::from(gp::ACTIVE_BINDING));
    asm.branch(Condition::NotEqual, slow);
    asm.load(Width::W64, Reg::T2, Reg::T3, header_layout::SECOND as u32);
    asm.constant(Reg::T1, missing_arg().as_ptr() as u64);
    asm.compare(Reg::T2, Reg::T1);
    asm.branch(Condition::Equal, slow);
    asm.constant(Reg::T0, unbound().as_ptr() as u64);
    asm.compare(Reg::T2, Reg::T0);
    asm.branch(Condition::Equal, slow);
    let plain = asm.label();
    let done = asm.label();
    asm.load(Width::W8, Reg::T1, Reg::T2, header_layout::KIND as u32);
    asm.compare_constant(Reg::T1, SexpType::Promise as u32);
    asm.branch(Condition::NotEqual, plain);
    asm.load(Width::W64, Reg::T1, Reg::T2, header_layout::FIRST as u32);
    asm.compare(Reg::T1, Reg::T0);
    asm.branch(Condition::Equal, slow);
    asm.store(Width::W64, Reg::T1, Reg::Window, to * 8);
    asm.jump(done);
    asm.bind(plain);
    asm.store(Width::W64, Reg::T2, Reg::Window, to * 8);
    asm.bind(done);
}

/// `SetVar` of a variable the activation's own frame binds to an ordinary cell: the store,
/// its reference counts and the binding flags, as `Heap::set_binding_value` does them. A new
/// binding, an active, locked or watched one, a store the write barrier must see, and one that
/// drops the old value's last counted reference branch to `slow`, which runs the step.
fn inline_set_var(
    asm: &mut Assembler,
    symbol: Sexp,
    from: u32,
    slot: u32,
    visible: u32,
    slow: Label,
) {
    probe_frame(asm, symbol, slot, slow);
    asm.load(Width::W16, Reg::T2, Reg::T3, header_layout::GP as u32);
    asm.test_constant(
        Reg::T2,
        u32::from(gp::ACTIVE_BINDING | gp::BINDING_LOCKED | gp::WATCHED),
    );
    asm.branch(Condition::NotEqual, slow);
    asm.load(Width::W8, Reg::T2, Reg::T3, header_layout::GC as u32);
    asm.and_constant(
        Reg::T2,
        u32::from(header_layout::GC_OLD | header_layout::GC_REMEMBERED),
    );
    asm.compare_constant(Reg::T2, u32::from(header_layout::GC_OLD));
    asm.branch(Condition::Equal, slow);
    asm.load(Width::W64, Reg::T1, Reg::Window, from * 8);
    asm.load(Width::W64, Reg::T0, Reg::T3, header_layout::SECOND as u32);
    let counted = asm.label();
    asm.load(Width::W8, Reg::T2, Reg::T3, header_layout::FLAGS as u32);
    asm.test_constant(Reg::T2, u32::from(header_layout::FLAG_UNTRACKED));
    asm.branch(Condition::NotEqual, counted);
    let refcount = header_layout::REFCOUNT as u32;
    let saturated = u32::from(heap::object::REFCOUNT_MAX);
    // The old value's last reference going away needs the heap's zero list: leave before
    // anything changed.
    asm.load(Width::W16, Reg::T2, Reg::T0, refcount);
    asm.compare_constant(Reg::T2, 1);
    asm.branch(Condition::Equal, slow);
    let incremented = asm.label();
    asm.load(Width::W16, Reg::T2, Reg::T1, refcount);
    asm.compare_constant(Reg::T2, saturated);
    asm.branch(Condition::Equal, incremented);
    asm.add_constant(Reg::T2, 1);
    asm.store(Width::W16, Reg::T2, Reg::T1, refcount);
    asm.bind(incremented);
    // Reloaded, because the new value can be the old one.
    asm.load(Width::W16, Reg::T2, Reg::T0, refcount);
    asm.compare_constant(Reg::T2, saturated);
    asm.branch(Condition::Equal, counted);
    asm.compare_constant(Reg::T2, 0);
    asm.branch(Condition::Equal, counted);
    asm.add_constant(Reg::T2, -1);
    asm.store(Width::W16, Reg::T2, Reg::T0, refcount);
    asm.bind(counted);
    asm.store(Width::W64, Reg::T1, Reg::T3, header_layout::SECOND as u32);
    asm.load(Width::W16, Reg::T2, Reg::T3, header_layout::GP as u32);
    asm.and_constant(
        Reg::T2,
        u32::from(!(gp::BINDING_MISSING | gp::BINDING_ABSENT)),
    );
    asm.store(Width::W16, Reg::T2, Reg::T3, header_layout::GP as u32);
    asm.store_interp_byte(visible, 0);
}

/// The branch conditions of a comparison operator: on two integers (signed) and on two
/// ordered doubles.
fn comparison_condition(operator: Arith) -> Option<(Condition, Condition)> {
    Some(match operator {
        Arith::Equal => (Condition::Equal, Condition::Equal),
        Arith::NotEqual => (Condition::NotEqual, Condition::NotEqual),
        Arith::Less => (Condition::Less, Condition::Below),
        Arith::Greater => (Condition::Greater, Condition::Above),
        Arith::LessEqual => (Condition::LessOrEqual, Condition::BelowOrEqual),
        Arith::GreaterEqual => (Condition::GreaterOrEqual, Condition::AboveOrEqual),
        _ => return None,
    })
}

/// A comparison of two attribute-free scalars of the same kind, both integers or both doubles,
/// neither `NA` nor NaN: the result is one of the shared `TRUE` and `FALSE` values, so it
/// allocates nothing. Every other operand, and a body recording type feedback, branches to
/// `slow`, which runs the step.
fn inline_comparison(
    asm: &mut Assembler,
    operator: Arith,
    to: u32,
    left: u32,
    right: u32,
    visible: u32,
    slow: Label,
) {
    let Some((integers, doubles)) = comparison_condition(operator) else {
        return asm.jump(slow);
    };
    asm.load(
        Width::W64,
        Reg::T0,
        Reg::Activation,
        baseline_layout::FEEDBACK as u32,
    );
    asm.test(Reg::T0, Reg::T0);
    asm.branch(Condition::NotEqual, slow);
    asm.load(Width::W64, Reg::T0, Reg::Window, left * 8);
    asm.load(Width::W64, Reg::T1, Reg::Window, right * 8);
    asm.load(Width::W8, Reg::T2, Reg::T0, header_layout::KIND as u32);
    asm.load(Width::W8, Reg::T3, Reg::T1, header_layout::KIND as u32);
    asm.compare(Reg::T2, Reg::T3);
    asm.branch(Condition::NotEqual, slow);
    asm.constant(Reg::T3, nil().as_ptr() as u64);
    for operand in [Reg::T0, Reg::T1] {
        asm.load(Width::W8, Reg::T2, operand, header_layout::FLAGS as u32);
        asm.test_constant(Reg::T2, u32::from(ALTREP_FLAG));
        asm.branch(Condition::NotEqual, slow);
        asm.load(Width::W64, Reg::T2, operand, header_layout::ATTRIB as u32);
        asm.compare(Reg::T2, Reg::T3);
        asm.branch(Condition::NotEqual, slow);
        asm.load(Width::W64, Reg::T2, operand, u32::from(LENGTH));
        asm.compare_constant(Reg::T2, 1);
        asm.branch(Condition::NotEqual, slow);
    }
    let truth = asm.label();
    let done = asm.label();
    let double = asm.label();
    asm.load(Width::W8, Reg::T2, Reg::T0, header_layout::KIND as u32);
    asm.compare_constant(Reg::T2, SexpType::Real as u32);
    asm.branch(Condition::Equal, double);
    asm.compare_constant(Reg::T2, SexpType::Integer as u32);
    asm.branch(Condition::NotEqual, slow);
    asm.load(Width::W32, Reg::T2, Reg::T0, u32::from(DATA));
    asm.load(Width::W32, Reg::T3, Reg::T1, u32::from(DATA));
    asm.constant(Reg::T0, heap::object::NA_INTEGER as u32 as u64);
    asm.compare32(Reg::T2, Reg::T0);
    asm.branch(Condition::Equal, slow);
    asm.compare32(Reg::T3, Reg::T0);
    asm.branch(Condition::Equal, slow);
    asm.compare32(Reg::T2, Reg::T3);
    asm.branch(integers, truth);
    let falsity = asm.label();
    asm.jump(falsity);
    asm.bind(double);
    asm.load_double(false, Reg::T0, u32::from(DATA));
    asm.load_double(true, Reg::T1, u32::from(DATA));
    asm.compare_doubles();
    asm.branch(Condition::Unordered, slow);
    asm.branch(doubles, truth);
    let known = heap().known();
    asm.bind(falsity);
    asm.store_register_constant(to, known.false_value.as_ptr() as u64);
    asm.jump(done);
    asm.bind(truth);
    asm.store_register_constant(to, known.true_value.as_ptr() as u64);
    asm.bind(done);
    asm.store_interp_byte(visible, 1);
}

/// Finds `symbol`'s binding cell in the activation's own frame at the position lookup `slot`
/// last found it, leaving it in `T3`, as `Frame::find_hinted` does on a hint that matches. A
/// frame without the symbol, a stale hint and a frame whose hints are off branch to `slow`.
/// Global and base frames keep their bindings on the symbols and an empty frame here, so their
/// filter rejects every symbol.
fn probe_frame(asm: &mut Assembler, symbol: Sexp, slot: u32, slow: Label) {
    const _: () = assert!(frame::SLOT == 16);
    let at = |field: usize| (ENVIRONMENT_FRAME_OFFSET + field) as u32;
    asm.load(
        Width::W64,
        Reg::T0,
        Reg::Activation,
        baseline_layout::ENV as u32,
    );
    asm.load(Width::W64, Reg::T1, Reg::T0, at(frame::FILTER));
    asm.constant(Reg::T2, frame::filter_bit(symbol));
    asm.test(Reg::T1, Reg::T2);
    asm.branch(Condition::Equal, slow);
    asm.load(Width::W32, Reg::T1, Reg::T0, at(frame::FORMALS));
    asm.test_constant(Reg::T1, frame::SHADOWED);
    asm.branch(Condition::NotEqual, slow);
    asm.load(
        Width::W64,
        Reg::T2,
        Reg::Activation,
        baseline_layout::HINTS as u32,
    );
    asm.load(Width::W32, Reg::T1, Reg::T2, slot * 4);
    asm.load(Width::W32, Reg::T2, Reg::T0, at(frame::LENGTH));
    asm.compare(Reg::T1, Reg::T2);
    asm.branch(Condition::AboveOrEqual, slow);
    asm.address(Reg::T3, Reg::T0, at(frame::STORAGE));
    let inline = asm.label();
    asm.load(Width::W32, Reg::T2, Reg::T0, at(frame::CAPACITY));
    asm.compare_constant(Reg::T2, frame::INLINE);
    asm.branch(Condition::BelowOrEqual, inline);
    asm.load(Width::W64, Reg::T3, Reg::T3, 0);
    asm.bind(inline);
    asm.shift_left(Reg::T1, 4);
    asm.add(Reg::T3, Reg::T1);
    asm.load(Width::W64, Reg::T2, Reg::T3, 0);
    asm.constant(Reg::T1, symbol.as_ptr() as u64);
    asm.compare(Reg::T2, Reg::T1);
    asm.branch(Condition::NotEqual, slow);
    asm.load(Width::W64, Reg::T3, Reg::T3, frame::SLOT_CELL as u32);
}

/// Where an instruction that is not handled inline can branch to besides the next instruction.
fn branch_target(op: Op, operand: &impl Fn(usize) -> usize) -> Option<usize> {
    match op {
        Op::Jump | Op::Resume => Some(operand(0)),
        Op::JumpUnless => Some(operand(2)),
        Op::BranchUnlessBuiltin | Op::BranchIfObject => Some(operand(1)),
        Op::MatchShape | Op::MatchTag | Op::MatchLiteral => Some(operand(2)),
        Op::Item => Some(operand(4)),
        Op::BranchUnlessPrimitive => Some(operand(2)),
        Op::ForStep => Some(operand(3)),
        Op::LogicLeft => Some(operand(4)),
        Op::Guard => Some(operand(4)),
        _ => None,
    }
}

fn op_falls_through(op: Op) -> bool {
    !matches!(op, Op::Jump | Op::Resume | Op::Return | Op::MatchFail)
}

/// Executable memory, handed out in chunks of whole pages and never returned: baseline code
/// lives as long as the session, as Cranelift's code does.
struct CodeArena {
    chunk: *mut u8,
    capacity: usize,
    used: usize,
}

const CHUNK: usize = 1 << 20;

thread_local! {
    static ARENA: RefCell<CodeArena> = const {
        RefCell::new(CodeArena { chunk: std::ptr::null_mut(), capacity: 0, used: 0 })
    };
}

/// Copies `bytes` into executable memory and answers the address of their first byte.
fn install(bytes: &[u8]) -> Result<*mut c_void, &'static str> {
    ARENA.with(|arena| {
        let mut arena = arena
            .try_borrow_mut()
            .map_err(|_| "the code arena is busy")?;
        let page = page_size();
        // Each body starts on a 64-byte boundary, the size of a cache line on both hosts.
        let start = arena.used.next_multiple_of(64);
        if arena.chunk.is_null() || start + bytes.len() > arena.capacity {
            let capacity = bytes.len().next_multiple_of(page).max(CHUNK);
            arena.chunk = map(capacity)?;
            arena.capacity = capacity;
            arena.used = 0;
            return place(&mut arena, 0, bytes, page);
        }
        place(&mut arena, start, bytes, page)
    })
}

fn place(
    arena: &mut CodeArena,
    start: usize,
    bytes: &[u8],
    page: usize,
) -> Result<*mut c_void, &'static str> {
    // Only the pages the new code touches change protection. They may hold code that is on the
    // stack, but none of it runs while they are writable: compiling runs no R code.
    let first = start / page * page;
    let end = (start + bytes.len()).next_multiple_of(page);
    unsafe {
        let pages = arena.chunk.add(first);
        protect(pages, end - first, false)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), arena.chunk.add(start), bytes.len());
        protect(pages, end - first, true)?;
        flush_instruction_cache(arena.chunk.add(start), bytes.len());
        arena.used = start + bytes.len();
        Ok(arena.chunk.add(start).cast())
    }
}

#[cfg(unix)]
fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(size).unwrap_or(4096).max(4096)
}

#[cfg(unix)]
fn map(capacity: usize) -> Result<*mut u8, &'static str> {
    // SAFETY: an anonymous private mapping aliases nothing.
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            capacity,
            libc::PROT_READ | libc::PROT_EXEC,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err("no executable memory");
    }
    Ok(address.cast())
}

#[cfg(unix)]
unsafe fn protect(pages: *mut u8, length: usize, executable: bool) -> Result<(), &'static str> {
    let protection = if executable {
        libc::PROT_READ | libc::PROT_EXEC
    } else {
        libc::PROT_READ | libc::PROT_WRITE
    };
    if unsafe { libc::mprotect(pages.cast(), length, protection) } != 0 {
        return Err("the code pages cannot change protection");
    }
    Ok(())
}

#[cfg(not(unix))]
fn page_size() -> usize {
    4096
}

#[cfg(not(unix))]
fn map(_capacity: usize) -> Result<*mut u8, &'static str> {
    Err("this host has no baseline code memory")
}

#[cfg(not(unix))]
unsafe fn protect(_pages: *mut u8, _length: usize, _executable: bool) -> Result<(), &'static str> {
    Err("this host has no baseline code memory")
}

/// Makes freshly written code visible to instruction fetch. x86-64 keeps its caches coherent;
/// aarch64 needs the data cache cleaned and the instruction cache invalidated line by line.
#[allow(unused_variables)]
unsafe fn flush_instruction_cache(start: *mut u8, length: usize) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        // CTR_EL0 gives the smallest data and instruction cache lines as log2 of words.
        let ctr: u64;
        std::arch::asm!("mrs {0}, ctr_el0", out(reg) ctr);
        let data_line = 4usize << ((ctr >> 16) & 0xF);
        let instruction_line = 4usize << (ctr & 0xF);
        let end = start as usize + length;
        let mut address = start as usize & !(data_line - 1);
        while address < end {
            std::arch::asm!("dc cvau, {0}", in(reg) address);
            address += data_line;
        }
        std::arch::asm!("dsb ish");
        let mut address = start as usize & !(instruction_line - 1);
        while address < end {
            std::arch::asm!("ic ivau, {0}", in(reg) address);
            address += instruction_line;
        }
        std::arch::asm!("dsb ish", "isb");
    }
}

/// Whether the body of `code` has baseline machine code.
pub unsafe fn has_baseline(code: heap::object::Sexp) -> bool {
    unsafe { code.cdr().element(BASELINE).kind() == SexpType::ExternalPtr }
}
