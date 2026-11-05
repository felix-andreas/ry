//! The activation record compiled code runs on, and the helpers it calls.
//!
//! Compiled code calls back into Rust for everything that allocates, runs R code or can fail.
//! Each helper is `extern "C"` and catches panics: unwinding through Cranelift frames is
//! undefined, so a panic is parked in the activation and resumed once compiled code returns.
//! An R error, `break` or `next` is parked the same way, as the activation's pending unwind,
//! and compiled code reacts by deoptimizing, which hands the unwind to the interpreter.

use {
    crate::analysis::{Speculation, resolve_function},
    heap::{
        object::{
            Header, INTEGER_PREFIX, LOGICAL_PREFIX, NA_INTEGER, Sexp, SexpType, Slot,
            UNBOXED_SLOTS, gp, is_na_real,
        },
        records::NativeKind,
    },
    interp::{
        annotations::{self, Entry},
        arith,
        builtins::Arg,
        bytecode::{self, LoopState, Machine, PROFILE, TIER1, profile},
        eval, heap,
        standard_library::{self, CompiledKey},
        unwind::{R, Unwind, error},
        values,
    },
    smallvec::SmallVec,
    std::{
        any::Any,
        cell::Cell,
        ffi::c_void,
        panic::{AssertUnwindSafe, catch_unwind},
    },
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

/// A compiled body. Owned by an external pointer in the code's `TIER1` constant, which keeps
/// every object the machine code embeds alive: through a weak reference keyed on the enclosure
/// for code tied to its enclosure, or directly for portable code.
pub struct Compiled {
    pub entry: unsafe extern "C" fn(*mut Activation) -> *mut Header,
    /// The enclosure the code was compiled for.
    pub enclosure: Sexp,
    /// Whether the code runs for any enclosure its lookups resolve the same way from. It does
    /// when every function it speculates on is bound in a long-lived environment (the global
    /// environment, the search path or a namespace), so nothing it embeds dies with an
    /// enclosure. A closure made afresh by each call of its parent (`vapply(x, function(v) ...)`)
    /// or each R6 object's copy of a method then shares one compilation.
    pub portable: bool,
    /// For portable code, a weak reference keyed on the enclosure whose lookups the epoch and
    /// stamps were last proven for; `NULL` otherwise. A weak reference rather than the
    /// enclosure's address, because a dead enclosure's address can come back as a new one whose
    /// lookups nobody checked.
    pub checked: Cell<Sexp>,
    /// Which speculations start from the enclosure, so that portable code resolves them from
    /// each activation's own.
    pub rooted: Box<[bool]>,
    /// Tier 0's register count; the slot root cells follow.
    pub registers: usize,
    pub slots: Vec<Sexp>,
    pub loops: usize,
    pub speculations: Vec<Speculation>,
    /// The binding epoch the speculations were last checked against.
    pub epoch: Cell<u64>,
    /// Each speculation's symbol's lookup stamp when it was last checked.
    pub stamps: Vec<Cell<u64>>,
    /// The loop headers on-stack replacement can enter at.
    pub osr: Vec<usize>,
    /// Each deoptimization point's write-back, indexed by the number compiled code passes to
    /// [`jit_deopt_exit`].
    pub exits: Box<[Exit]>,
}

/// What one deoptimization point writes back before tier 0 resumes at `pc`.
///
/// Each point only stores its raw values into a scratch area, one 8-byte cell each in the order
/// below, and jumps to one shared exit that calls [`jit_deopt_exit`] with this point's number.
/// Boxing and flushing in a cold block per point would make those blocks most of a function's
/// code and most of its compile time.
pub struct Exit {
    pub pc: usize,
    pub why: i32,
    /// Registers held unboxed, with their [`representation`]: one cell for the value, and a
    /// second for the integer flag of a [`representation::NUM`].
    pub registers: Box<[(u32, i32)]>,
    /// Slots that may be dirty: one cell for the status, then the value cells as for a
    /// register, none for a [`representation::BOXED`] slot.
    pub slots: Box<[(u32, i32)]>,
}

impl Exit {
    /// The scratch cells this exit uses.
    pub fn cells(&self) -> usize {
        let value = |kind: i32| match kind {
            representation::NUM => 2,
            representation::BOXED => 0,
            _ => 1,
        };
        let registers: usize = self.registers.iter().map(|&(_, kind)| value(kind)).sum();
        let slots: usize = self.slots.iter().map(|&(_, kind)| 1 + value(kind)).sum();
        registers + slots
    }
}

/// Whether every function lookup compiled code assumed still resolves the same way for an
/// activation whose frame is `frame`. Every change that could alter such a lookup moves the
/// binding epoch and the symbol's lookup stamp (see `gp::WATCHED`), so an unchanged epoch, or
/// unchanged stamps of the speculated symbols, prove the lookups without walking them.
/// `thorough` walks them anyway, for an activation whose frame may have bound a watched name
/// before the name was watched. Portable code running for an enclosure other than the checked
/// one or its siblings walks, and leaves the epoch and stamps to the checked enclosure.
pub unsafe fn valid(compiled: &Compiled, frame: Sexp, thorough: bool) -> bool {
    let heap = heap();
    let epoch = heap.binding_epoch();
    let enclosure = frame.enclosure();
    let checked = !compiled.portable || compiled.checked.get().weak_key() == enclosure;
    let sibling = !checked && sibling_of_checked(compiled, enclosure);
    if !thorough && (checked || sibling) {
        // The epoch moves on any binding outside a function frame, which a test suite or a
        // package load makes all the time, so most moves concern names no speculation uses.
        let proven =
            epoch == compiled.epoch.get()
                || compiled.speculations.iter().zip(&compiled.stamps).all(
                    |(speculation, stamp)| heap.lookup_stamp(speculation.symbol) == stamp.get(),
                );
        if proven {
            compiled.epoch.set(epoch);
            // A sibling's lookups pass through its own frame and then the checked enclosure's
            // parents, which the epoch and stamps proved, so only that frame is left to check.
            if !sibling || !binds_rooted_function(compiled, enclosure) {
                return true;
            }
        }
    }
    for ((speculation, stamp), &rooted) in compiled
        .speculations
        .iter()
        .zip(&compiled.stamps)
        .zip(&compiled.rooted)
    {
        // The body's own lookups start past its frame, because the compiler cannot see the
        // frame's run-time bindings: `assign` into `parent.frame()`, a default argument, or the
        // locals `UseMethod` copies in can each bind the name there.
        if rooted
            && heap
                .frame_cell(frame, speculation.symbol)
                .is_some_and(|cell| may_be_function(cell.cdr()))
        {
            return false;
        }
        let start = if rooted { enclosure } else { speculation.env };
        match resolve_function(speculation.symbol, start) {
            Some(found)
                if found.cell == speculation.cell && found.function == speculation.function => {}
            _ => return false,
        }
        if checked {
            stamp.set(heap.lookup_stamp(speculation.symbol));
        }
    }
    if checked {
        compiled.epoch.set(epoch);
    }
    true
}

/// [`valid`] after running code that started at binding epoch `epoch`. The activation's
/// speculations held before that code ran, and every binding that could change a function
/// lookup moves the epoch, so an unmoved epoch proves them without reading them.
#[inline]
unsafe fn still_valid(compiled: &Compiled, frame: Sexp, epoch: u64) -> bool {
    unsafe { heap().binding_epoch() == epoch || valid(compiled, frame, false) }
}

/// Whether portable `compiled` runs for `enclosure`, which is not the checked enclosure but
/// shares its parent. The closures one function expression makes on each call of its parent
/// (`make_scaler <- function(k) function(x) x * k`) are siblings in this sense, so they share
/// the checked enclosure's proof instead of each walking every speculation.
pub unsafe fn sibling_of_checked(compiled: &Compiled, enclosure: Sexp) -> bool {
    unsafe {
        let key = compiled.checked.get().weak_key();
        key != values::nil() && key.enclosure() == enclosure.enclosure()
    }
}

/// Whether `env`'s own frame binds a name some rooted speculation looks up to something a
/// function lookup could stop at.
unsafe fn binds_rooted_function(compiled: &Compiled, env: Sexp) -> bool {
    let heap = heap();
    compiled
        .speculations
        .iter()
        .zip(&compiled.rooted)
        .any(|(speculation, &rooted)| {
            rooted
                && heap
                    .frame_cell(env, speculation.symbol)
                    .is_some_and(|cell| unsafe { may_be_function(cell.cdr()) })
        })
}

/// Whether a lookup of a function could stop at a binding holding `value`, which R skips only
/// when it is certainly not a function.
unsafe fn may_be_function(value: Sexp) -> bool {
    unsafe {
        let value = if value.kind() == SexpType::Promise {
            value.car()
        } else {
            value
        };
        value == values::unbound() || values::is_function(value)
    }
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
    (state / COLD_REFUSAL) & 0xff
}

/// A tier state flag: the body's compiled code would hand most of its instructions to tier 0,
/// so calls no longer compile it and only a hot loop in it does.
pub const LOOPS_ONLY: i32 = 1 << 16;

/// The entry count of a body that calls must not compile. Counting up from here never reaches
/// the entry threshold, so tier 0 stops asking the tier about the body's calls, and stops
/// recording type feedback for them unless the body has a loop that may still compile.
pub const NEVER_HOT: i32 = i32::MIN;

/// Discards `code`'s compiled code after a speculation failed. Activations still running it
/// keep running, so the objects it embeds stay rooted for the rest of the session.
pub unsafe fn invalidate(code: Sexp) {
    let constants = code.cdr();
    let tier1 = constants.element(TIER1);
    if tier1 == values::nil() {
        return;
    }
    if crate::logging() {
        eprintln!("jit: invalidated {}", crate::describe(code));
    }
    let heap = heap();
    heap.add_global_root(tier1);
    heap.set_element(constants, TIER1, values::nil());
    let counters = values::ints_mut(constants.element(PROFILE));
    let state = counters[profile::TIER_STATE];
    // Halfway to the threshold, tier 0 keeps recording type feedback, so the activation that
    // just deoptimized records the type that broke the speculation and the recompile sees it.
    // A body that compiles only for its loop records through its loop anyway, and its calls
    // must never ask the tier again, so it keeps the entry count that stops them asking.
    counters[profile::ENTRIES] = if state >= 0 && state & LOOPS_ONLY != 0 {
        NEVER_HOT
    } else {
        bytecode::tier().map_or(0, |tier| tier.entry_threshold / 2)
    };
    counters[profile::BACK_EDGES] = 0;
    if compiles(state) >= MAX_COMPILES {
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
unsafe fn guarded<T>(
    activation: *mut Activation,
    fallback: T,
    body: impl FnOnce(&mut Activation) -> T,
) -> T {
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
unsafe fn checked<T>(
    activation: *mut Activation,
    fallback: T,
    body: impl FnOnce(&mut Activation) -> T,
) -> T {
    unsafe {
        if (*activation).pending.is_some() {
            return fallback;
        }
        guarded(activation, fallback, body)
    }
}

unsafe fn slot_cell(activation: &Activation, slot: i64) -> *mut Sexp {
    unsafe {
        activation
            .registers
            .add(compiled(activation).registers + slot as usize)
    }
}

/// Runs the tier-0 instruction at `pc`, then checks the speculations when `validate` is set.
pub unsafe extern "C" fn jit_step(activation: *mut Activation, pc: i64, validate: i32) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let machine = machine(activation);
            let epoch = heap().binding_epoch();
            match scoped(|| bytecode::step_one(machine, pc as usize)) {
                Err(unwind) => {
                    park(activation, unwind);
                    outcome::FAILED
                }
                Ok(())
                    if validate != 0 && !still_valid(compiled(activation), machine.env, epoch) =>
                {
                    outcome::INVALIDATED
                }
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
            let epoch = heap().binding_epoch();
            let loaded = scoped(|| bytecode::load_variable(machine.code, machine.env, pc as usize));
            finish_load(activation, slot, loaded, epoch)
        })
    }
}

/// Loads a slot's variable for an element assignment, which has no inline cache.
pub unsafe extern "C" fn jit_load_slot(activation: *mut Activation, slot: i64) -> i32 {
    unsafe {
        checked(activation, outcome::LOAD_FAILED, |activation| {
            let machine = machine(activation);
            let symbol = compiled(activation).slots[slot as usize];
            let epoch = heap().binding_epoch();
            let loaded = scoped(|| {
                let local = heap().frame_cell(machine.env, symbol).is_some();
                eval::eval_symbol(symbol, machine.env).map(|value| (value, local))
            });
            finish_load(activation, slot, loaded, epoch)
        })
    }
}

unsafe fn finish_load(
    activation: &mut Activation,
    slot: i64,
    loaded: R<(Sexp, bool)>,
    epoch: u64,
) -> i32 {
    unsafe {
        match loaded {
            Err(unwind) => {
                park(activation, unwind);
                outcome::LOAD_FAILED
            }
            Ok((value, local)) => {
                *slot_cell(activation, slot) = value;
                // Loading can force a promise, which runs arbitrary code.
                if !compiled(activation).speculations.is_empty()
                    && !still_valid(compiled(activation), machine(activation).env, epoch)
                {
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
pub unsafe extern "C" fn jit_closure_matches(
    activation: *mut Activation,
    value: *mut Header,
    formals: *mut Header,
    body: *mut Header,
) -> i32 {
    unsafe {
        guarded(activation, 0, |activation| {
            let (Some(value), Some(formals), Some(body)) = (
                Sexp::from_raw(value),
                Sexp::from_raw(formals),
                Sexp::from_raw(body),
            ) else {
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
    signature: *mut Header,
    dst: i64,
) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let (Some(function), Some(call)) = (Sexp::from_raw(function), Sexp::from_raw(call))
            else {
                park(activation, error("compiled code passed a null call"));
                return outcome::FAILED;
            };
            let machine = machine(activation);
            let arguments = std::slice::from_raw_parts(arguments, 2 * count as usize);
            let epoch = heap().binding_epoch();
            let result = scoped(|| -> R<Sexp> {
                let heap = heap();
                let mut args: SmallVec<[Arg; 4]> = SmallVec::with_capacity(count as usize);
                for pair in arguments.as_chunks::<2>().0 {
                    let (kind, bits) = (pair[0] as i32, pair[1]);
                    let value = match kind {
                        representation::INTEGER => values::scalar_integer(bits as i32)?,
                        representation::DOUBLE => values::scalar_real(f64::from_bits(bits as u64))?,
                        representation::LOGICAL => values::scalar_logical(bits as i32)?,
                        _ => Sexp::from_raw(bits as *mut Header)
                            .ok_or_else(|| error("compiled code passed a null argument"))?,
                    };
                    heap.root(value);
                    args.push(Arg {
                        tag: values::nil(),
                        value,
                    });
                }
                if annotations::is_typed(function)
                    && let Some(value) = crate::native::call(function, &args)
                {
                    return value;
                }
                // `signature` is what the compiler decided about the callee's annotation checks:
                // null when they depend on the values, `NULL` for an untyped callee, otherwise
                // the signature of a typed callee whose arguments' types already satisfy it.
                let entry = match Sexp::from_raw(signature) {
                    None => {
                        // The values are at hand, so the checks read them here instead of
                        // looking each formal up again in the new frame.
                        let values: SmallVec<[Sexp; 4]> =
                            args.iter().map(|arg| arg.value).collect();
                        annotations::enter_positional(call, function, &values)?
                    }
                    Some(signature) if signature == values::nil() => Entry::Checked(None),
                    Some(signature) => Entry::Checked(Some(signature)),
                };
                let env = eval::bind_arguments(call, function, &args, None)?;
                eval::run_closure(call, function, env, machine.env, &args, entry)
            });
            match result {
                Err(unwind) => {
                    park(activation, unwind);
                    outcome::FAILED
                }
                Ok(value) => {
                    *activation.registers.add(dst as usize) = value;
                    if !still_valid(compiled(activation), machine.env, epoch) {
                        outcome::INVALIDATED
                    } else {
                        outcome::OK
                    }
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
    /// A [`crate::analysis::Ty::Num`]: a double and a flag saying it holds an integer. Only
    /// deoptimization exits use it.
    pub const NUM: i32 = 4;
}

/// Boxes the registers and flushes the dirty slots that exit `exit` lists, reading their raw
/// values from `scratch`, then deoptimizes like [`jit_deopt`].
pub unsafe extern "C" fn jit_deopt_exit(
    activation: *mut Activation,
    scratch: *const i64,
    exit: i64,
) -> *mut Header {
    unsafe {
        let Some(exit) = compiled(&*activation).exits.get(exit as usize) else {
            panic!("compiled code named a deoptimization exit it does not have");
        };
        let mut cell = scratch;
        let mut next = || {
            let current = cell;
            cell = cell.add(1);
            current
        };
        for &(register, kind) in &exit.registers {
            let value = next();
            let boxed = match kind {
                representation::INTEGER => jit_box_integer(activation, value.cast::<i32>().read()),
                representation::LOGICAL => jit_box_logical(activation, value.cast::<i32>().read()),
                representation::DOUBLE => jit_box_double(activation, value.cast::<f64>().read()),
                _ => jit_box_num(
                    activation,
                    value.cast::<f64>().read(),
                    next().cast::<i32>().read(),
                ),
            };
            // The window roots each box before the next allocation can collect. Boxing never
            // returns null: a failure parks its error and returns `NULL`.
            if let Some(boxed) = Sexp::from_raw(boxed) {
                *(*activation).registers.add(register as usize) = boxed;
            }
        }
        for &(slot, kind) in &exit.slots {
            let status = next().cast::<i32>().read();
            let (kind, bits) = match kind {
                representation::BOXED => (kind, 0),
                representation::DOUBLE => (kind, next().read()),
                representation::NUM => {
                    let double = next().cast::<f64>().read();
                    if next().cast::<i32>().read() == 0 {
                        (representation::DOUBLE, double.to_bits() as i64)
                    } else if double.is_nan() {
                        (representation::INTEGER, i64::from(NA_INTEGER))
                    } else {
                        (representation::INTEGER, i64::from(double as i32))
                    }
                }
                _ => (kind, i64::from(next().cast::<i32>().read())),
            };
            if i64::from(status) == crate::analysis::status::RUNTIME_DIRTY {
                // A failure parks its error, which the deoptimization below then raises.
                jit_flush(activation, i64::from(slot), kind, bits);
            }
        }
        jit_deopt(activation, exit.pc as i64, exit.why)
    }
}

/// Writes a dirty slot back to the environment. A boxed value hands the reference compiled
/// code held over to the binding. Returns 1 on success.
pub unsafe extern "C" fn jit_flush(
    activation: *mut Activation,
    slot: i64,
    kind: i32,
    bits: i64,
) -> i32 {
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
            if crate::logging() {
                eprintln!(
                    "jit: deopt at pc {pc} (reason {why}) in {}",
                    crate::describe(machine.code)
                );
            }
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
pub unsafe extern "C" fn jit_condition(
    activation: *mut Activation,
    value: *mut Header,
    call: *mut Header,
) -> i32 {
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
pub unsafe extern "C" fn jit_logic(
    activation: *mut Activation,
    value: *mut Header,
    call: *mut Header,
    is_and: i32,
    right: i32,
) -> i32 {
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
pub unsafe extern "C" fn jit_duplicate(
    activation: *mut Activation,
    slot: i64,
    dirty: i32,
) -> *mut Header {
    unsafe {
        guarded(activation, std::ptr::null_mut(), |activation| {
            let cell = slot_cell(activation, slot);
            let old = *cell;
            match scoped(|| values::shallow_duplicate(old)) {
                Ok(copy) => {
                    // Assigning an item into an Array keeps it an Array, as tier 0 does.
                    copy.set_array(old.is_array());
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

/// An ALTREP `for` loop sequence expanded into an ordinary vector, as tier 0's `ForInit`
/// expands it; any other value is returned as it is.
pub unsafe extern "C" fn jit_expand_sequence(
    activation: *mut Activation,
    value: *mut Header,
) -> *mut Header {
    unsafe {
        boxed(activation, || match Sexp::from_raw(value) {
            Some(value) => values::materialize(value),
            None => Ok(values::nil()),
        })
    }
}

unsafe fn boxed(activation: *mut Activation, make: impl FnOnce() -> R<Sexp>) -> *mut Header {
    unsafe {
        guarded(
            activation,
            values::nil().as_ptr(),
            |activation| match scoped(make) {
                Ok(value) => value.as_ptr(),
                // Only an exhausted heap gets here. The next helper sees the parked error.
                Err(unwind) => {
                    park(activation, unwind);
                    values::nil().as_ptr()
                }
            },
        )
    }
}

pub unsafe extern "C" fn jit_box_integer(activation: *mut Activation, value: i32) -> *mut Header {
    unsafe { boxed(activation, || values::scalar_integer(value)) }
}

pub unsafe extern "C" fn jit_box_double(activation: *mut Activation, value: f64) -> *mut Header {
    unsafe { boxed(activation, || values::scalar_real(value)) }
}

/// Boxes a [`crate::analysis::Ty::Num`]: an integer when `integer` is nonzero, else a double.
pub unsafe extern "C" fn jit_box_num(
    activation: *mut Activation,
    value: f64,
    integer: i32,
) -> *mut Header {
    if integer == 0 {
        return unsafe { boxed(activation, || values::scalar_real(value)) };
    }
    // The double came from an integer, so it converts back exactly; `NA` became NaN.
    let value = if value.is_nan() {
        NA_INTEGER
    } else {
        value as i32
    };
    unsafe { boxed(activation, || values::scalar_integer(value)) }
}

pub unsafe extern "C" fn jit_box_logical(activation: *mut Activation, value: i32) -> *mut Header {
    unsafe { boxed(activation, || values::scalar_logical(value)) }
}

/// A record, tuple or tag of `layout` whose `count` slots are `(kind, bits)` pairs in the
/// [`representation`] encoding. Numbers go into the record unboxed, without a scalar on the way.
pub unsafe extern "C" fn jit_construct(
    activation: *mut Activation,
    layout: i32,
    pairs: *const i64,
    count: i64,
) -> *mut Header {
    unsafe {
        boxed(activation, || {
            record_from_pairs(
                layout,
                std::slice::from_raw_parts(pairs, 2 * count as usize),
            )
        })
    }
}

/// [`jit_construct`] for a native function, which has no activation to park an error in: the
/// record, rooted until the native function returns, or null when the heap is exhausted, which
/// fails the native function so that tier 0 reports it.
pub unsafe extern "C" fn jit_native_record(layout: i32, pairs: *const i64, count: i64) -> i64 {
    unsafe {
        let heap = heap();
        let mark = heap.handle_mark();
        let record = record_from_pairs(
            layout,
            std::slice::from_raw_parts(pairs, 2 * count as usize),
        );
        heap.release_handles(mark);
        match record {
            Ok(record) => {
                heap.root(record);
                record.as_ptr() as i64
            }
            Err(_) => 0,
        }
    }
}

/// Where the handle stack stands, which a native function that allocates releases to.
pub extern "C" fn jit_native_mark() -> i64 {
    heap().handle_mark() as i64
}

/// Releases the handles a native function rooted since `mark`, keeping `keep` rooted when it is
/// not null: the record the function returns.
pub unsafe extern "C" fn jit_native_release(mark: i64, keep: *mut Header) {
    let heap = heap();
    heap.release_handles(mark as usize);
    if let Some(keep) = unsafe { Sexp::from_raw(keep) } {
        heap.root(keep);
    }
}

/// A record of `layout` from `(kind, bits)` pairs in the [`representation`] encoding.
unsafe fn record_from_pairs(layout: i32, pairs: &[i64]) -> R<Sexp> {
    unsafe {
        let count = pairs.len() / 2;
        let heap = heap();
        let mut slots: SmallVec<[Slot; 4]> = SmallVec::with_capacity(count);
        // Every box made here stays rooted until the record holds it, because the next
        // allocation may collect. Slots past the unboxed mask hold handles only.
        let mut rooted = |value: Sexp| {
            heap.root(value);
            Slot::Handle(value)
        };
        for (index, pair) in pairs.as_chunks::<2>().0.iter().enumerate() {
            let (kind, bits) = (pair[0] as i32, pair[1]);
            let wide = index >= UNBOXED_SLOTS;
            let slot = match kind {
                representation::INTEGER if wide => rooted(values::scalar_integer(bits as i32)?),
                representation::LOGICAL if wide => rooted(values::scalar_logical(bits as i32)?),
                representation::DOUBLE if wide => {
                    rooted(values::scalar_real(f64::from_bits(bits as u64))?)
                }
                representation::INTEGER => Slot::Integer(bits as i32),
                representation::LOGICAL => Slot::Logical(bits as i32),
                representation::DOUBLE => {
                    let number = f64::from_bits(bits as u64);
                    let upper = (bits as u64 >> 32) as u32;
                    // A NaN whose upper half reads as an integer's or logical's tag stays
                    // boxed, as `slot_for` keeps it.
                    if upper == INTEGER_PREFIX || upper == LOGICAL_PREFIX {
                        rooted(values::scalar_real(number)?)
                    } else {
                        Slot::Real(number)
                    }
                }
                _ => Slot::Handle(
                    Sexp::from_raw(bits as *mut Header)
                        .ok_or_else(|| error("compiled code passed a null slot"))?,
                ),
            };
            slots.push(slot);
        }
        Ok(heap.alloc_record_slots(layout as u32, &slots)?)
    }
}

/// Puts a new list of `length` elements, each `NULL`, in register `dst`, for a compiled
/// traversal to fill. Returns 0 when allocating fails, with the error parked.
pub unsafe extern "C" fn jit_list_new(activation: *mut Activation, length: i64, dst: i64) -> i32 {
    unsafe {
        checked(activation, 0, |activation| {
            match scoped(|| values::alloc(SexpType::List, length as usize)) {
                Ok(list) => {
                    *activation.registers.add(dst as usize) = list;
                    1
                }
                Err(unwind) => {
                    park(activation, unwind);
                    0
                }
            }
        })
    }
}

/// Stores the value `kind` and `bits` describe (see [`representation`]) at `index` of the list
/// in register `dst`, boxing a number. Returns 0 when boxing fails, with the error parked.
pub unsafe extern "C" fn jit_list_store(
    activation: *mut Activation,
    dst: i64,
    index: i64,
    kind: i32,
    bits: i64,
) -> i32 {
    unsafe {
        checked(activation, 0, |activation| {
            let stored = scoped(|| -> R<()> {
                let list = *activation.registers.add(dst as usize);
                let value = match kind {
                    representation::INTEGER => values::scalar_integer(bits as i32)?,
                    representation::DOUBLE => values::scalar_real(f64::from_bits(bits as u64))?,
                    representation::LOGICAL => values::scalar_logical(bits as i32)?,
                    _ => Sexp::from_raw(bits as *mut Header)
                        .ok_or_else(|| error("compiled code stored a null item"))?,
                };
                heap().set_element(list, index as usize, value);
                Ok(())
            });
            match stored {
                Ok(()) => 1,
                Err(unwind) => {
                    park(activation, unwind);
                    0
                }
            }
        })
    }
}

/// Cuts the list in register `dst` down to its first `count` elements. Returns 0 when
/// allocating the shorter list fails, with the error parked.
pub unsafe extern "C" fn jit_list_prefix(activation: *mut Activation, dst: i64, count: i64) -> i32 {
    unsafe {
        checked(activation, 0, |activation| {
            let cell = activation.registers.add(dst as usize);
            let list = *cell;
            let count = count as usize;
            if count == values::length(list) {
                return 1;
            }
            let shortened = scoped(|| -> R<Sexp> {
                let heap = heap();
                let shorter = values::alloc(SexpType::List, count)?;
                for index in 0..count {
                    heap.set_element(shorter, index, list.element(index));
                }
                Ok(shorter)
            });
            match shortened {
                Ok(shorter) => {
                    *cell = shorter;
                    1
                }
                Err(unwind) => {
                    park(activation, unwind);
                    0
                }
            }
        })
    }
}

/// Puts the keys vector of the dictionary (`dict` nonzero) or set `object` in register `keys`,
/// and a dictionary's values list in register `stored`, and returns its length. Returns -1,
/// handing the traversal to tier 0, when `object` is not one, or when it has keys and their
/// vector is not of type `key` (a negative `key` accepts any type).
pub unsafe extern "C" fn jit_collection_parts(
    activation: *mut Activation,
    object: i64,
    dict: i32,
    key: i32,
    keys: i64,
    stored: i64,
) -> i64 {
    unsafe {
        checked(activation, -1, |activation| {
            let Some(object) = Sexp::from_raw(object as *mut Header) else {
                return -1;
            };
            let kind = if dict != 0 {
                NativeKind::Dict
            } else {
                NativeKind::Set
            };
            match scoped(|| standard_library::collection_parts(object, kind)) {
                Ok(Some((vector, values, count))) => {
                    if count > 0 && key >= 0 && vector.kind() as i32 != key {
                        return -1;
                    }
                    *activation.registers.add(keys as usize) = vector;
                    *activation.registers.add(stored as usize) = values;
                    count as i64
                }
                Ok(None) => -1,
                Err(unwind) => {
                    park(activation, unwind);
                    -1
                }
            }
        })
    }
}

/// Puts what `List.find_first` and its kin answer in register `dst`: the element of the list
/// in register `list` at 0-based `position`, or its position when `index` is nonzero, or
/// `:Err(:NotFound)` for a negative `position`. Returns 0 when building it fails, with the
/// error parked.
pub unsafe extern "C" fn jit_found(
    activation: *mut Activation,
    dst: i64,
    list: i64,
    position: i64,
    index: i32,
) -> i32 {
    unsafe {
        checked(activation, 0, |activation| {
            let list = *activation.registers.add(list as usize);
            let position = usize::try_from(position).ok();
            match scoped(|| standard_library::found(list, position, index != 0)) {
                Ok(answer) => {
                    *activation.registers.add(dst as usize) = answer;
                    1
                }
                Err(unwind) => {
                    park(activation, unwind);
                    0
                }
            }
        })
    }
}

/// Stores the count a compiled `List.count_if` loop reached into register `dst`, as the `I64`
/// the library gives. Returns 0, with the error parked, when boxing it fails.
pub unsafe extern "C" fn jit_count(activation: *mut Activation, dst: i64, count: i64) -> i32 {
    unsafe {
        checked(activation, 0, |activation| {
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            match scoped(|| standard_library::whole_number(count)) {
                Ok(answer) => {
                    *activation.registers.add(dst as usize) = answer;
                    1
                }
                Err(unwind) => {
                    park(activation, unwind);
                    0
                }
            }
        })
    }
}

/// Key kinds [`jit_collection_position`] takes.
pub mod key {
    pub const INTEGER: i32 = 0;
    pub const DOUBLE: i32 = 1;
    pub const VALUE: i32 = 2;
}

fn collection_kind(set: i32) -> NativeKind {
    if set != 0 {
        NativeKind::Set
    } else {
        NativeKind::Dict
    }
}

/// The row count of `vector` when it is an ordinary matrix whose elements compiled code may
/// store into: a double, integer or logical vector that is neither ALTREP, an Array nor an
/// object, whose only attribute is a `dim` of two counts. -1 for anything else, whose element
/// assignment tier 0 runs.
pub unsafe extern "C" fn jit_matrix_rows(vector: *mut Header) -> i64 {
    unsafe {
        let Some(vector) = Sexp::from_raw(vector) else {
            return -1;
        };
        if !matches!(
            vector.kind(),
            SexpType::Real | SexpType::Integer | SexpType::Logical
        ) || vector.is_altrep()
            || vector.is_array()
            || vector.is_object()
        {
            return -1;
        }
        let attributes = vector.attrib();
        if attributes.kind() != SexpType::Pairlist
            || attributes.tag() != values::symbols().dim
            || attributes.cdr().kind() != SexpType::Nil
        {
            return -1;
        }
        let dim = attributes.car();
        if dim.kind() != SexpType::Integer || dim.is_altrep() {
            return -1;
        }
        match dim.slice::<i32>() {
            &[rows, _] if rows >= 0 => i64::from(rows),
            _ => -1,
        }
    }
}

/// The tag of a plain tag value, or -1 for anything else.
pub unsafe extern "C" fn jit_plain_tag(value: *mut Header) -> i64 {
    unsafe {
        Sexp::from_raw(value)
            .and_then(|value| interp::records::plain_tag(value))
            .map_or(-1, i64::from)
    }
}

/// 1 when `value` matches the literal pattern `pattern`, as a `match` arm tests it.
pub unsafe extern "C" fn jit_literal_matches(value: *mut Header, pattern: *mut Header) -> i32 {
    unsafe {
        match (Sexp::from_raw(value), Sexp::from_raw(pattern)) {
            (Some(value), Some(pattern)) => {
                i32::from(interp::records::literal_matches(pattern, value))
            }
            _ => 0,
        }
    }
}

/// `Dict.len()` or `Set.len()`: the entry count, or -1 when tier 0 must run the call.
pub unsafe extern "C" fn jit_collection_count(value: *mut Header, set: i32) -> i64 {
    unsafe {
        Sexp::from_raw(value)
            .and_then(|value| standard_library::compiled_count(value, collection_kind(set)))
            .map_or(-1, |count| count as i64)
    }
}

/// Where the key is in a dict or set: its position, -1 when it is absent, or -2 when tier 0
/// must run the call. `kind` says which of `integer`, `double` and `value` holds the key.
pub unsafe extern "C" fn jit_collection_position(
    collection: *mut Header,
    set: i32,
    kind: i32,
    integer: i32,
    double: f64,
    value: *mut Header,
) -> i64 {
    unsafe {
        let key = match kind {
            key::INTEGER => CompiledKey::Integer(integer),
            key::DOUBLE => CompiledKey::Double(double),
            _ => match Sexp::from_raw(value) {
                Some(value) => CompiledKey::Value(value),
                None => return -2,
            },
        };
        let Some(collection) = Sexp::from_raw(collection) else {
            return -2;
        };
        match standard_library::compiled_position(collection, collection_kind(set), key) {
            Some(Some(position)) => position as i64,
            Some(None) => -1,
            None => -2,
        }
    }
}

/// `Dict.get()`'s result for the entry at `position`, or for a missing key when it is -1.
pub unsafe extern "C" fn jit_dict_get(
    activation: *mut Activation,
    dict: *mut Header,
    position: i64,
) -> *mut Header {
    unsafe {
        boxed(activation, || match Sexp::from_raw(dict) {
            Some(dict) => {
                standard_library::compiled_get_result(dict, usize::try_from(position).ok())
            }
            None => Err(error("Dict.get() reached compiled code without a dict")),
        })
    }
}

/// Whether a calling handler is established, so a warning would run R code.
pub extern "C" fn jit_handlers_present() -> i32 {
    interp::interp().handlers.is_some() as i32
}

/// How many closure calls may still nest before R's expression depth limit, which a native
/// function call counts down instead of pushing frames.
pub extern "C" fn jit_depth_budget() -> i64 {
    let state = interp::interp();
    i64::from(state.max_depth) - state.frames.len() as i64
}

/// `^` on doubles, as tier 0 computes it.
pub extern "C" fn jit_power(base: f64, exponent: f64) -> f64 {
    if exponent == 2.0 && !is_na_real(base) {
        base * base
    } else {
        arith::power(base, exponent)
    }
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

/// Whether `%%` on doubles must warn about a total loss of accuracy, which compiled code
/// leaves to tier 0: 1 when it must, else 0.
pub extern "C" fn jit_modulus_loses_accuracy(a: f64, b: f64) -> i32 {
    i32::from(arith::modulus_loses_accuracy(a, b))
}

/// `%/%` on doubles, as the interpreter computes it.
pub extern "C" fn jit_real_integer_divide(a: f64, b: f64) -> f64 {
    arith::real_integer_divide(a, b)
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
