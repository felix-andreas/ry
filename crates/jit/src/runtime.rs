//! The activation record compiled code runs on, and the helpers it calls.
//!
//! Compiled code calls back into Rust for everything that allocates, runs R code or can fail.
//! Each helper is `extern "C"` and catches panics: unwinding through Cranelift frames is
//! undefined, so a panic is parked in the activation and resumed once compiled code returns.
//! An R error, `break` or `next` is parked the same way, as the activation's pending unwind,
//! and compiled code reacts by deoptimizing, which hands the unwind to the interpreter.

use crate::analysis::{Speculation, resolve_function};
use heap::object::{Header, NA_INTEGER, Sexp, SexpType, gp, is_na_real};
use interp::{
    arith,
    bytecode::{self, LoopState, Machine, PROFILE, TIER1, profile},
    builtins::Arg,
    eval, heap,
    unwind::{R, Unwind, error},
    values,
};
use smallvec::SmallVec;
use std::{
    any::Any,
    cell::Cell,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// What compiled code receives. The first fields are read by machine code at fixed offsets
/// ([`offset`]); the rest belong to the helpers.
#[repr(C)]
pub struct Activation {
    /// The activation's register window. Its first `registers` entries are tier 0's
    /// registers, and the next ones root the boxed values of the variable slots.
    pub registers: *mut Sexp,
    /// Each `for` loop's index and count, written on deoptimization and read on entry.
    pub loops: *mut i64,
    pub visible: *mut bool,
    /// Where execution starts: 0 for a call, or the loop header on-stack replacement enters.
    pub entry_pc: i64,
    pub machine: *mut c_void,
    pub compiled: *const Compiled,
    pub pending: Option<Unwind>,
    pub panic: Option<Box<dyn Any + Send>>,
    /// Where tier 0 continues after a deoptimization.
    pub resume_pc: usize,
}

pub mod offset {
    pub const REGISTERS: i32 = 0;
    pub const LOOPS: i32 = 8;
    pub const VISIBLE: i32 = 16;
    pub const ENTRY_PC: i32 = 24;
}

/// What compiled code returns when on-stack replacement declined to enter, because a value
/// tier 0 left in a register is not of the type the compiled loop expects.
pub const DECLINED: usize = 1;

/// What compiled code returns after deoptimizing: the interpreter continues at the activation's
/// `resume_pc`, with its `pending` unwind.
pub const DEOPTIMIZED: usize = 2;

/// Deoptimization reasons.
pub mod reason {
    /// A speculation failed: the code is discarded and the body recompiled later.
    pub const SPECULATION: i32 = 0;
    /// An instruction raised an error or a `break`/`next`; the interpreter handles it.
    pub const PENDING: i32 = 1;
    /// A user interrupt is pending; the interpreter raises it.
    pub const INTERRUPT: i32 = 2;
    /// An inline fast path does not apply, so the interpreter runs the instruction.
    pub const HANDOFF: i32 = 3;
}

/// Results of [`jit_step`] and the load helpers.
pub mod outcome {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const INVALIDATED: i32 = 2;
    /// Load results: the slot's new status, or one of these.
    pub const LOAD_FAILED: i32 = 0;
    pub const LOAD_INVALIDATED: i32 = 4;
}

/// A compiled body. Owned by an external pointer in the code's `TIER1` constant, whose
/// protected list keeps every object the machine code embeds alive.
pub struct Compiled {
    pub entry: unsafe extern "C" fn(*mut Activation) -> *mut Header,
    pub enclosure: Sexp,
    /// Tier 0's register count; the slot root cells follow.
    pub registers: usize,
    pub slots: Vec<Sexp>,
    pub loops: usize,
    pub speculations: Vec<Speculation>,
    /// The binding epoch the speculations were last checked against.
    pub epoch: Cell<u64>,
    /// The loop headers on-stack replacement can enter at.
    pub osr: Vec<usize>,
}

/// Whether every function lookup compiled code assumed still resolves the same way. Every
/// change that could alter such a lookup moves the binding epoch (see `gp::WATCHED`), so an
/// unchanged epoch proves the lookups without walking them.
pub unsafe fn valid(compiled: &Compiled) -> bool {
    let epoch = heap().binding_epoch();
    if epoch == compiled.epoch.get() {
        return true;
    }
    for speculation in &compiled.speculations {
        match resolve_function(speculation.symbol, speculation.env) {
            Some(found) if found.cell == speculation.cell && found.function == speculation.function => {}
            _ => return false,
        }
    }
    compiled.epoch.set(epoch);
    true
}

/// How many times a body may be compiled before the JIT gives up on it.
pub const MAX_COMPILES: i32 = 4;

/// How many times a body may be refused as cold before it compiles without the types it is
/// missing.
pub const MAX_COLD_REFUSALS: i32 = 3;

/// The profile's tier state counts compilations in its low byte and cold refusals above it,
/// because a cold refusal compiles nothing and so must not use up a compilation.
pub const COLD_REFUSAL: i32 = 1 << 8;

pub fn compiles(state: i32) -> i32 {
    state & (COLD_REFUSAL - 1)
}

pub fn cold_refusals(state: i32) -> i32 {
    state / COLD_REFUSAL
}

/// Discards `code`'s compiled code after a speculation failed. Activations still running it
/// keep running, so the objects it embeds stay rooted for the rest of the session.
pub unsafe fn invalidate(code: Sexp) {
    let constants = code.cdr();
    let tier1 = constants.element(TIER1);
    if tier1 == values::nil() {
        return;
    }
    let heap = heap();
    heap.add_global_root(tier1);
    heap.set_element(constants, TIER1, values::nil());
    let counters = values::ints_mut(constants.element(PROFILE));
    counters[profile::ENTRIES] = 0;
    counters[profile::BACK_EDGES] = 0;
    if compiles(counters[profile::TIER_STATE]) >= MAX_COMPILES {
        counters[profile::TIER_STATE] = -1;
    }
}

unsafe fn machine<'a>(activation: &Activation) -> &'a mut Machine<'a> {
    unsafe { &mut *(activation.machine as *mut Machine<'a>) }
}

unsafe fn compiled<'a>(activation: &Activation) -> &'a Compiled {
    unsafe { &*activation.compiled }
}

/// Runs `body`, parking a panic in the activation and returning `fallback` instead.
unsafe fn guarded<T>(activation: *mut Activation, fallback: T, body: impl FnOnce(&mut Activation) -> T) -> T {
    let activation = unsafe { &mut *activation };
    if activation.panic.is_some() {
        return fallback;
    }
    match catch_unwind(AssertUnwindSafe(|| body(&mut *activation))) {
        Ok(value) => value,
        Err(payload) => {
            activation.panic = Some(payload);
            fallback
        }
    }
}

/// Runs `body` with the handle stack restored afterwards. Tier 0 releases the handles an
/// activation pushed on every backward jump; compiled loops have no such point, so every
/// helper that can allocate releases what it pushed, including the boxing helpers, because
/// each interpreter constructor roots its result. Anything still needed is held by a register
/// or the environment by then: compiled code stores a helper's result in the register window
/// before it allocates again.
unsafe fn scoped<T>(body: impl FnOnce() -> T) -> T {
    let mark = heap().handle_mark();
    let value = body();
    heap().release_handles(mark);
    value
}

fn park(activation: &mut Activation, unwind: Unwind) {
    activation.pending = Some(unwind);
}

/// Like [`guarded`], but fails at once while an error is parked. A failed allocation in a box
/// helper parks its error and hands compiled code `NULL`, which has no failure branch, so the
/// next helper that would run interpreter code with that value fails instead and the error
/// reaches tier 0. Write-back and deoptimization helpers use [`guarded`], because they must
/// still run after an error to leave the frame as tier 0 expects it.
unsafe fn checked<T>(activation: *mut Activation, fallback: T, body: impl FnOnce(&mut Activation) -> T) -> T {
    unsafe {
        if (*activation).pending.is_some() {
            return fallback;
        }
        guarded(activation, fallback, body)
    }
}

unsafe fn slot_cell(activation: &Activation, slot: i64) -> *mut Sexp {
    unsafe { activation.registers.add(compiled(activation).registers + slot as usize) }
}

/// Runs the tier-0 instruction at `pc`, then checks the speculations when `validate` is set.
pub unsafe extern "C" fn jit_step(activation: *mut Activation, pc: i64, validate: i32) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let machine = machine(activation);
            match scoped(|| bytecode::step_one(machine, pc as usize)) {
                Err(unwind) => {
                    park(activation, unwind);
                    outcome::FAILED
                }
                Ok(()) if validate != 0 && !valid(compiled(activation)) => outcome::INVALIDATED,
                Ok(()) => outcome::OK,
            }
        })
    }
}

/// Loads the variable `GetVar` at `pc` reads into its slot. Returns the slot's status.
pub unsafe extern "C" fn jit_load(activation: *mut Activation, pc: i64, slot: i64) -> i32 {
    unsafe {
        checked(activation, outcome::LOAD_FAILED, |activation| {
            let machine = machine(activation);
            let loaded = scoped(|| bytecode::load_variable(machine.code, machine.env, pc as usize));
            finish_load(activation, slot, loaded)
        })
    }
}

/// Loads a slot's variable for an element assignment, which has no inline cache.
pub unsafe extern "C" fn jit_load_slot(activation: *mut Activation, slot: i64) -> i32 {
    unsafe {
        checked(activation, outcome::LOAD_FAILED, |activation| {
            let machine = machine(activation);
            let symbol = compiled(activation).slots[slot as usize];
            let loaded = scoped(|| {
                let local = heap().frame_cell(machine.env, symbol).is_some();
                eval::eval_symbol(symbol, machine.env).map(|value| (value, local))
            });
            finish_load(activation, slot, loaded)
        })
    }
}

unsafe fn finish_load(activation: &mut Activation, slot: i64, loaded: R<(Sexp, bool)>) -> i32 {
    unsafe {
        match loaded {
            Err(unwind) => {
                park(activation, unwind);
                outcome::LOAD_FAILED
            }
            Ok((value, local)) => {
                *slot_cell(activation, slot) = value;
                // Loading can force a promise, which runs arbitrary code.
                if !compiled(activation).speculations.is_empty() && !valid(compiled(activation)) {
                    return outcome::LOAD_INVALIDATED;
                }
                if local { 1 } else { 2 }
            }
        }
    }
}

/// Loads a slot's variable for an inlined call without running any code: a promise must
/// already be forced, and an active binding or a missing variable declines with
/// [`outcome::LOAD_FAILED`], which hands the call back to tier 0.
pub unsafe extern "C" fn jit_peek_slot(activation: *mut Activation, slot: i64) -> i32 {
    unsafe {
        checked(activation, outcome::LOAD_FAILED, |activation| {
            let machine = machine(activation);
            let symbol = compiled(activation).slots[slot as usize];
            let heap = heap();
            let empty = heap.known().empty_env;
            let mut env = machine.env;
            let mut local = true;
            while env != empty {
                if let Some(cell) = heap.frame_cell(env, symbol) {
                    if cell.has_gp(gp::ACTIVE_BINDING) {
                        return outcome::LOAD_FAILED;
                    }
                    let mut value = cell.cdr();
                    if value.kind() == SexpType::Promise {
                        if value.car() == values::unbound() {
                            return outcome::LOAD_FAILED;
                        }
                        value = value.car();
                    }
                    if value == values::missing_arg() || value == values::unbound() {
                        return outcome::LOAD_FAILED;
                    }
                    *slot_cell(activation, slot) = value;
                    return if local { 1 } else { 2 };
                }
                local = false;
                env = env.enclosure();
            }
            outcome::LOAD_FAILED
        })
    }
}

/// Whether `value` is a closure made from `formals` and `body` in the activation's
/// environment, which is what an inlined call of it assumes.
pub unsafe extern "C" fn jit_closure_matches(activation: *mut Activation, value: *mut Header, formals: *mut Header, body: *mut Header) -> i32 {
    unsafe {
        guarded(activation, 0, |activation| {
            let (Some(value), Some(formals), Some(body)) = (Sexp::from_raw(value), Sexp::from_raw(formals), Sexp::from_raw(body)) else {
                return 0;
            };
            let matches = value.kind() == SexpType::Closure
                && value.car() == formals
                && value.tag() == machine(activation).env
                && bytecode::closure_body(value) == body;
            matches as i32
        })
    }
}

/// Calls the closure `function` for `call` with arguments compiled code evaluated, and puts the
/// result in register `dst`. `arguments` holds a representation and its bits for each argument
/// (see [`representation`]). Each argument is bound to its value rather than to a promise: the
/// callee is frame-safe, so it never calls `substitute` or `missing`, and compiled code makes
/// the call only while no handler is established, so no other R code can look at its frame.
pub unsafe extern "C" fn jit_call(
    activation: *mut Activation,
    function: *mut Header,
    call: *mut Header,
    arguments: *const i64,
    count: i64,
    dst: i64,
) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let (Some(function), Some(call)) = (Sexp::from_raw(function), Sexp::from_raw(call)) else {
                park(activation, error("compiled code passed a null call"));
                return outcome::FAILED;
            };
            let machine = machine(activation);
            let arguments = std::slice::from_raw_parts(arguments, 2 * count as usize);
            let result = scoped(|| -> R<Sexp> {
                let heap = heap();
                let mut args: SmallVec<[Arg; 4]> = SmallVec::with_capacity(count as usize);
                for pair in arguments.chunks_exact(2) {
                    let (kind, bits) = (pair[0] as i32, pair[1]);
                    let value = match kind {
                        representation::INTEGER => values::scalar_integer(bits as i32)?,
                        representation::DOUBLE => values::scalar_real(f64::from_bits(bits as u64))?,
                        representation::LOGICAL => values::scalar_logical(bits as i32)?,
                        _ => Sexp::from_raw(bits as *mut Header).ok_or_else(|| error("compiled code passed a null argument"))?,
                    };
                    heap.root(value);
                    args.push(Arg { tag: values::nil(), value });
                }
                // The frame records no argument list: only `nargs`, method dispatch and
                // `standardGeneric` read it, and a frame-safe callee calls none of them.
                let env = eval::bind_arguments(call, function, &args, None)?;
                eval::run_closure(call, function, env, machine.env, values::nil())
            });
            match result {
                Err(unwind) => {
                    park(activation, unwind);
                    outcome::FAILED
                }
                Ok(value) => {
                    *activation.registers.add(dst as usize) = value;
                    if !valid(compiled(activation)) { outcome::INVALIDATED } else { outcome::OK }
                }
            }
        })
    }
}

/// Slot value representations for [`jit_flush`].
pub mod representation {
    pub const BOXED: i32 = 0;
    pub const INTEGER: i32 = 1;
    pub const DOUBLE: i32 = 2;
    pub const LOGICAL: i32 = 3;
}

/// Writes a dirty slot back to the environment. A boxed value hands the reference compiled
/// code held over to the binding. Returns 1 on success.
pub unsafe extern "C" fn jit_flush(activation: *mut Activation, slot: i64, kind: i32, bits: i64) -> i32 {
    unsafe {
        guarded(activation, 0, |activation| {
            let machine = machine(activation);
            let symbol = compiled(activation).slots[slot as usize];
            let cell = slot_cell(activation, slot);
            let result = scoped(|| -> R<()> {
                let value = match kind {
                    representation::INTEGER => values::scalar_integer(bits as i32)?,
                    representation::DOUBLE => values::scalar_real(f64::from_bits(bits as u64))?,
                    representation::LOGICAL => values::scalar_logical(bits as i32)?,
                    _ => *cell,
                };
                *cell = value;
                eval::assign_symbol(symbol, value, machine.env, false)?;
                if kind == representation::BOXED {
                    value.decrement_refcount();
                }
                Ok(())
            });
            match result {
                Ok(()) => 1,
                Err(unwind) => {
                    park(activation, unwind);
                    0
                }
            }
        })
    }
}

/// Gives the activation back to the interpreter at `pc`: tier 0 continues from there with the
/// registers, variables and loop states compiled code wrote back, and its result is the
/// activation's result. Returns null when that unwinds.
pub unsafe extern "C" fn jit_deopt(activation: *mut Activation, pc: i64, why: i32) -> *mut Header {
    unsafe {
        guarded(activation, std::ptr::null_mut(), |activation| {
            let machine = machine(activation);
            let compiled = compiled(activation);
            if why == reason::SPECULATION && activation.pending.is_none() {
                invalidate(machine.code);
            }
            for index in 0..compiled.loops {
                let index_value = *activation.loops.add(2 * index);
                let count = *activation.loops.add(2 * index + 1);
                if let Some(state) = machine.loops.get_mut(index) {
                    *state = LoopState::vector(index_value.max(0) as usize, count.max(0) as usize);
                }
            }
            activation.resume_pc = pc as usize;
            DEOPTIMIZED as *mut Header
        })
    }
}

/// `if (value)`: 1 or 0, or 2 after parking the error.
pub unsafe extern "C" fn jit_condition(activation: *mut Activation, value: *mut Header, call: *mut Header) -> i32 {
    unsafe {
        checked(activation, 2, |activation| {
            let (Some(value), Some(call)) = (Sexp::from_raw(value), Sexp::from_raw(call)) else {
                park(activation, error("compiled code passed a null condition"));
                return 2;
            };
            match scoped(|| eval::as_condition(value, call)) {
                Ok(flag) => flag as i32,
                Err(unwind) => {
                    park(activation, unwind);
                    2
                }
            }
        })
    }
}

/// An operand of `&&` or `||` as a logical scalar, or 2 after parking the error.
pub unsafe extern "C" fn jit_logic(activation: *mut Activation, value: *mut Header, call: *mut Header, is_and: i32, right: i32) -> i32 {
    unsafe {
        checked(activation, 2, |activation| {
            let (Some(value), Some(call)) = (Sexp::from_raw(value), Sexp::from_raw(call)) else {
                park(activation, error("compiled code passed a null operand"));
                return 2;
            };
            let operator = if is_and != 0 { "&&" } else { "||" };
            let side = if right != 0 { "y" } else { "x" };
            match scoped(|| eval::logical_operand_of(value, call, operator, side)) {
                Ok(flag) => flag,
                Err(unwind) => {
                    park(activation, unwind);
                    2
                }
            }
        })
    }
}

/// Replaces a slot's vector with a private copy compiled code may modify in place, owning one
/// reference to it. `dirty` says whether the slot owned a reference to the old vector.
pub unsafe extern "C" fn jit_duplicate(activation: *mut Activation, slot: i64, dirty: i32) -> *mut Header {
    unsafe {
        guarded(activation, std::ptr::null_mut(), |activation| {
            let cell = slot_cell(activation, slot);
            let old = *cell;
            match scoped(|| values::shallow_duplicate(old)) {
                Ok(copy) => {
                    copy.increment_refcount();
                    *cell = copy;
                    if dirty != 0 {
                        old.decrement_refcount();
                    }
                    copy.as_ptr()
                }
                Err(unwind) => {
                    park(activation, unwind);
                    std::ptr::null_mut()
                }
            }
        })
    }
}

unsafe fn boxed(activation: *mut Activation, make: impl FnOnce() -> R<Sexp>) -> *mut Header {
    unsafe {
        guarded(activation, values::nil().as_ptr(), |activation| match scoped(make) {
            Ok(value) => value.as_ptr(),
            // Only an exhausted heap gets here. The next helper sees the parked error.
            Err(unwind) => {
                park(activation, unwind);
                values::nil().as_ptr()
            }
        })
    }
}

pub unsafe extern "C" fn jit_box_integer(activation: *mut Activation, value: i32) -> *mut Header {
    unsafe { boxed(activation, || values::scalar_integer(value)) }
}

pub unsafe extern "C" fn jit_box_double(activation: *mut Activation, value: f64) -> *mut Header {
    unsafe { boxed(activation, || values::scalar_real(value)) }
}

pub unsafe extern "C" fn jit_box_logical(activation: *mut Activation, value: i32) -> *mut Header {
    unsafe { boxed(activation, || values::scalar_logical(value)) }
}

/// Whether a calling handler is established, so a warning would run R code.
pub extern "C" fn jit_handlers_present() -> i32 {
    interp::interp().handlers.is_some() as i32
}

/// `^` on doubles, as tier 0 computes it.
pub extern "C" fn jit_power(base: f64, exponent: f64) -> f64 {
    if exponent == 2.0 && !is_na_real(base) { base * base } else { arith::power(base, exponent) }
}

pub extern "C" fn jit_exp(value: f64) -> f64 {
    value.exp()
}

pub extern "C" fn jit_log(value: f64) -> f64 {
    value.ln()
}

pub extern "C" fn jit_real_modulo(a: f64, b: f64) -> f64 {
    arith::real_modulo(a, b)
}

/// `%%` on integers: `NA` for an `NA` operand or a zero divisor, else the sign of the divisor.
pub extern "C" fn jit_integer_modulo(a: i32, b: i32) -> i32 {
    if a == NA_INTEGER || b == NA_INTEGER || b == 0 {
        return NA_INTEGER;
    }
    let remainder = a.rem_euclid(b);
    remainder + if b < 0 && remainder != 0 { b } else { 0 }
}

/// `%/%` on integers.
pub extern "C" fn jit_integer_divide(a: i32, b: i32) -> i32 {
    if a == NA_INTEGER || b == NA_INTEGER || b == 0 {
        return NA_INTEGER;
    }
    (a as f64 / b as f64).floor() as i32
}

