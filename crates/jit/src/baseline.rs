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
        assembler::{Arch, Assembler, Condition, Label},
        logging,
    },
    heap::object::{SexpType, VECTOR_DATA_OFFSET},
    interp::{
        bytecode::{self, BASELINE, BASELINE_STEPS, Machine, Op, baseline_layout, operand_count},
        heap,
        values::{self, nil, unbound},
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
