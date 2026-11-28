//! The activation record compiled code runs on, and the helpers it calls.
//!
//! Compiled code calls back into Rust for everything that allocates, runs R code or can fail.
//! Each helper is `extern "C"` and catches panics: unwinding through Cranelift frames is
//! undefined, so a panic is parked in the activation and resumed once compiled code returns.
//! An R error, `break` or `next` is parked the same way, as the activation's pending unwind,
//! and compiled code reacts by deoptimizing, which hands the unwind to the interpreter.

use {
    crate::analysis::{Speculation, bit, resolve_function, status},
    heap::{
        activity,
        object::{
            Header, INTEGER_PREFIX, LOGICAL_PREFIX, NA_INTEGER, Sexp, SexpType, Slot,
            UNBOXED_SLOTS, gp, is_na_real,
        },
        records::NativeKind,
    },
    interp::{
        Signalling, arith,
        builtins::Arg,
        bytecode::{
            self, FEEDBACK, LoopState, Machine, PROFILE, StepEnd, Stepper, TIER1, feedback, profile,
        },
        coerce,
        eval::{self, Entering},
        heap, long_double, moves,
        standard_library::{self, CompiledKey},
        subset,
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
    /// The activation's header register, which holds its environment, or `NULL` while it runs
    /// in a slot frame whose slots follow the header. Compiled code reads it on every probe,
    /// because a callee can give the frame its environment while the activation runs.
    pub header: *mut Sexp,
    /// The code's lookup-slot hints ([`bytecode::lookup_hints`]).
    pub hints: *mut u32,
    /// Where lookups of names the frame does not bind start: its closure's environment.
    pub enclosure: Sexp,
    pub machine: *mut c_void,
    pub compiled: *const Compiled,
    /// The frame of the compiled body this activation runs: the body's own, or that of a
    /// closure call inlined in it.
    pub layout: *const Layout,
    /// What the helpers hold for the whole entry of compiled code, shared by the body's
    /// activation and those of its inlined calls.
    pub parked: *mut Parked,
    /// The activations of the body's inlined calls, by frame number less one.
    pub children: *mut Child,
    /// Where tier 0 continues after a deoptimization.
    pub resume_pc: usize,
    /// Where the compiled body's own register window ends in the register file, which the
    /// windows of its inlined calls are placed from ([`Inlined::header`]).
    pub root_top: usize,
    /// Tier 0's view of the activation, for the instructions compiled code hands back.
    pub stepper: Stepper,
}

/// What a helper holds until compiled code hands the activation back.
pub struct Parked {
    /// An R error, `break` or `next` a helper raised. The collector traces it here through
    /// [`interp::Interp::held_unwinds`].
    pub pending: Option<Unwind>,
    pub panic: Option<Box<dyn Any + Send>>,
}

/// The activation of an inlined closure call while it runs without a frame of its own: its
/// registers sit above its caller's in the register file, and the interpreter state a real
/// call would have saved in its frame is saved here.
///
/// Everything here but the three fields a call saves is the same for every start of the call
/// during one entry of the compiled body, because the call's register window sits at a fixed
/// place above the body's ([`Inlined::header`]). So the record is written once, by the first
/// start in an entry, which finds `activation.compiled` null.
#[repr(C)]
pub struct Child {
    pub activation: Activation,
    pub machine: Machine<'static>,
    inlined: *const Inlined,
    /// The closure's own srcref, which compiled code makes current when the call starts.
    function_srcref: Sexp,
    /// The caller's srcref, which compiled code saves here when the call starts and makes
    /// current again when it ends.
    srcref: Sexp,
}

const _: () = assert!(std::mem::offset_of!(Child, activation) == 0);

pub mod offset {
    pub const REGISTERS: i32 = 0;
    pub const LOOPS: i32 = 8;
    pub const VISIBLE: i32 = 16;
    pub const ENTRY_PC: i32 = 24;
    pub const HEADER: i32 = 32;
    pub const HINTS: i32 = 40;
    pub const ENCLOSURE: i32 = 48;
    pub const CHILD_FUNCTION_SRCREF: i32 =
        std::mem::offset_of!(super::Child, function_srcref) as i32;
    pub const CHILD_SRCREF: i32 = std::mem::offset_of!(super::Child, srcref) as i32;
}

const _: () = assert!(std::mem::offset_of!(Activation, header) == offset::HEADER as usize);
const _: () = assert!(std::mem::offset_of!(Activation, enclosure) == offset::ENCLOSURE as usize);
const _: () = assert!(std::mem::offset_of!(Activation, hints) == offset::HINTS as usize);

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
    /// An inlined call read a formal of the compiled body that holds a promise only tier 0
    /// forces, so the interpreter runs the instruction. It counts against the body
    /// ([`UNFORCED_FORMALS`]), not against the inlined callee as a hand-off does.
    pub const UNFORCED: i32 = 4;
}

thread_local! {
    /// How many times compiled code has left for tier 0 on this thread. A test reads it to pin
    /// that a body stops leaving at a point it left from repeatedly.
    pub static DEOPTIMIZATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Results of [`jit_step`] and the load helpers.
pub mod outcome {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const INVALIDATED: i32 = 2;
    /// A fused pass computed its expression, and execution goes on at its target.
    pub const FUSED: i32 = 3;
    /// A fused pass computed its expression, and a promise it forced invalidated the code.
    pub const FUSED_INVALIDATED: i32 = 5;
    /// The instruction returned from the activation, leaving the value in its destination
    /// register: a finished [`interp::bytecode::Op::Prologue`].
    pub const RETURNED: i32 = 6;
    /// Load results: the slot's new status, or one of these.
    pub const LOAD_FAILED: i32 = 0;
    pub const LOAD_INVALIDATED: i32 = 4;
    /// [`jit_call_builtin`] ran nothing: the builtin could run R code for an argument, so
    /// compiled code steps the call with the frame written back.
    pub const OBSERVED: i32 = 7;
}

/// How [`jit_step`] runs its instruction.
pub mod stepping {
    /// The instruction runs no R code, so its signals wait ([`Signalling`]) and nothing needs
    /// validating.
    pub const QUIET: i32 = 0;
    /// The instruction may run R code, so the speculations are checked afterwards.
    pub const VALIDATED: i32 = 1;
    /// The instruction signals where it raises, and the code after it relies on no speculation
    /// it could break.
    pub const PLAIN: i32 = 2;
}

/// What [`jit_element_step`] did with an element read or store.
pub mod element {
    /// Tier 0's fast handler ran the instruction, which stores in place.
    pub const HANDLED: i32 = 0;
    /// The generic handler ran the instruction, which may have rebound a store's variable.
    pub const STEPPED: i32 = 1;
    /// Nothing ran: the generic handler might run R code, so compiled code steps the
    /// instruction with the frame written back.
    pub const DECLINED: i32 = 2;
    /// The generic handler raised an error, which is parked.
    pub const FAILED: i32 = 3;
}

/// A compiled body. Owned by an external pointer in the code's `TIER1` constant, which keeps
/// every object the machine code embeds alive: through a weak reference keyed on the enclosure
/// for code tied to its enclosure, or directly for portable code.
pub struct Compiled {
    pub entry: unsafe extern "C" fn(*mut Activation) -> *mut Header,
    /// The body's code, whose `TIER1` constant owns this.
    pub code: Sexp,
    /// The enclosure the code was compiled for.
    pub enclosure: Sexp,
    /// Whether the code runs for any enclosure its lookups resolve the same way from. It does
    /// when every function it speculates on is bound in a long-lived environment (the global
    /// environment, the search path or a namespace), so nothing it embeds dies with an
    /// enclosure. A closure made afresh by each call of its parent (`vapply(x, function(v) ...)`)
    /// or each R6 object's copy of a method then shares one compilation.
    pub portable: bool,
    /// Whether portable code has a speculation that starts from the enclosure. Its epoch and
    /// stamps then speak for one enclosure at a time, `checked`; those of portable code without
    /// one speak for every enclosure.
    pub tied: bool,
    /// For tied code, a weak reference keyed on the enclosure whose lookups the epoch and
    /// stamps were last proven for; `NULL` otherwise. A weak reference rather than the
    /// enclosure's address, because a dead enclosure's address can come back as a new one whose
    /// lookups nobody checked.
    pub checked: Cell<Sexp>,
    /// Which speculations start from the enclosure, so that portable code resolves them from
    /// each activation's own.
    pub rooted: Box<[bool]>,
    /// The body's frame, then one frame per closure call inlined in it.
    pub frames: Box<[Layout]>,
    /// How many registers above the body's own window its inlined calls need at most
    /// ([`Layout::top`]). An entry of the body reserves them once.
    pub extent: usize,
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
    /// The write-back records the machine code embeds the addresses of ([`jit_write_back`]).
    #[expect(
        dead_code,
        reason = "owned for the machine code, which reads the records"
    )]
    pub writes: Box<[Box<[u32]>]>,
    /// How often an inlined call could not start without its frame for a reason that is not
    /// the callee's: the stack was near its limit, or something observes every call. When that
    /// is frequent ([`frequent`]) the code is discarded and the body marked [`NO_FRAMES`].
    pub declined_frames: Cell<u32>,
    /// How often an inlined call's read of one of the body's formals ([`jit_peek_slot`],
    /// [`jit_load_passed`]) found a promise it could not force without running code. Past
    /// [`MAX_FRAME_HANDOFFS`] the code is discarded and the body marked [`UNFORCED_FORMALS`].
    pub declined_loads: Cell<u32>,
    /// How often the code has left for a reason no recompile removes (an instruction failed,
    /// or an inline fast path of the body's own frame did not apply), and the body's entry
    /// count when that count started. When the leaves are frequent among the entries since
    /// ([`frequent`]) the code is discarded and the body runs in tier 0 from then on.
    /// Otherwise the count starts again, so only leaves close together retire the code.
    pub leaves: Cell<u32>,
    pub leaves_since: Cell<i32>,
    /// Whether the code discarded itself. Activations still running it keep counting, and by
    /// then the body may hold its recompiled code, which a second discard would throw away.
    pub retired: Cell<bool>,
}

/// One frame of a compiled body: what its register window holds and which variables its slots
/// stand for.
pub struct Layout {
    /// Tier 0's register count; the slot root cells follow.
    pub registers: usize,
    pub slots: Box<[Sexp]>,
    /// Each slot's position in the code's slot table, which numbers a slot frame's variables,
    /// or `None` for a symbol the table does not number.
    pub frame: Box<[Option<usize>]>,
    /// By slot, a bitset of the variables the code loads at every read because a read called
    /// an active binding.
    pub volatile: Box<[u64]>,
    /// By slot, where the frame that binds a free variable held it at the last load, which
    /// [`heap::Heap::frame_cell_hinted`] tries first.
    pub hints: Box<[Cell<u32>]>,
    /// The call the frame belongs to, or `None` for the compiled body's own frame.
    pub inlined: Option<Inlined>,
}

impl Layout {
    /// Where the register window of this frame ends, counted from the end of the compiled
    /// body's own window, which is where a call inlined in this frame puts its header.
    pub fn top(&self) -> usize {
        match &self.inlined {
            Some(inlined) => {
                inlined.header + 1 + inlined.frame_slots + self.registers + self.slots.len()
            }
            None => 0,
        }
    }
}

/// The closure call an inlined frame stands for, which is what building its real frame needs.
pub struct Inlined {
    pub function: Sexp,
    /// The call's header register, counted from the end of the compiled body's own window:
    /// the end of the calling frame's window ([`Layout::top`]). The frame's slots follow the
    /// header, then tier 0's registers and the slot root cells, as in a frame tier 0 enters.
    /// Calls inlined in one frame share the place, because only one of them runs at a time.
    pub header: usize,
    /// How many slots the closure's frame has ([`bytecode::slot_count`]).
    pub frame_slots: usize,
    /// The closure's compiled body when it was inlined.
    pub code: Sexp,
    pub call: Sexp,
    /// The frame the call is made from.
    pub parent: u32,
    /// The call instruction in the parent's code, where an error of the call is raised.
    pub call_pc: u32,
    /// The parent's register that receives the result.
    pub dst: u32,
    /// Where an exit of the call finds each formal's argument, by position. An exit saves a
    /// bitset of the formals the call has forced, then one cell per [`ArgumentSource::Saved`]
    /// formal, in order.
    pub formals: Box<[ArgumentSource]>,
    /// For each formal, the position of its argument in the call, or a negative number when
    /// the call leaves the formal out ([`ArgumentSource::Omitted`]).
    pub positions: Box<[i32]>,
    /// How often the call started without a frame or tried to.
    pub entries: Cell<u32>,
    /// How often the call needed its real frame because of what the callee is or does: it
    /// handed an instruction to tier 0, an instruction of it had a condition to signal, or
    /// the closure is being debugged or has another body by now. When that is frequent ([`frequent`]) the code is discarded and the callee is
    /// no longer inlined ([`NO_ELISION`]).
    pub handoffs: Cell<u32>,
}

/// Where the argument of a formal that an inlined call has forced is found when the call gets
/// its real frame, which binds the formal to a forced promise of that value.
#[derive(Clone, Copy)]
pub enum ArgumentSource {
    /// In the exit, in this [`representation`]: the caller evaluated the argument.
    Saved(i32),
    /// In the binding of this formal of the compiled body, which was passed on unread; the
    /// read forced that binding's promise.
    Root(Sexp),
    /// Nowhere: compiled code never reads the formal, so it never forces it.
    Unread,
    /// This literal: a constant argument, or the default of a formal an inlined caller's call
    /// left out, which that caller passed on.
    Literal(Sexp),
    /// Nowhere, because the call leaves the formal out: the frame binds the formal's default
    /// or the missing argument, as tier 0 binds it.
    Omitted,
}

/// How many times something about a compiled body's inlined calls may go wrong before the
/// body is recompiled to avoid it.
pub const MAX_FRAME_HANDOFFS: u32 = 16;

/// Whether `count` fallbacks in `entries` inlined calls are enough to recompile: more than
/// one call in eight, and enough of them to say so. A rare fallback costs less than the frame
/// every other call saves.
fn frequent(count: u32, entries: u64) -> bool {
    count >= MAX_FRAME_HANDOFFS && u64::from(count) * 8 > entries
}

/// What one deoptimization point writes back before tier 0 resumes at `pc`.
///
/// Each point is the exit of a frame state: it only saves its raw values into a buffer, one
/// 8-byte cell each in the order below, and continues in one shared block that calls
/// [`jit_deopt_exit`] with the state's number. Boxing and flushing in a cold block per point
/// would make those blocks most of a function's code and most of its compile time.
pub struct Exit {
    pub pc: usize,
    pub why: i32,
    /// The frame the point is in, an index into [`Compiled::frames`].
    pub frame: u32,
    /// For a point in an inlined call, the exit that describes the caller's frame once the
    /// call has returned. Its `pc` is where the caller continues.
    pub parent: Option<u32>,
    /// Where the point's cells start in the buffer, after those of its callers' frames.
    pub base: u32,
    /// Registers held unboxed, with their [`representation`]: one cell for the value, and a
    /// second for the integer flag of a [`representation::NUM`].
    pub registers: Box<[(u32, i32)]>,
    /// Slots that may be dirty: one cell for the status, then the value cells as for a
    /// register, none for a [`representation::BOXED`] slot.
    pub slots: Box<[(u32, i32)]>,
    /// The loops around `pc`, [`LOOP_WORDS`] cells each, outermost first.
    pub loops: Box<[u32]>,
    pub handoffs: Handoffs,
}

/// How often a deoptimization point of a body's own frame handed its instruction to tier 0
/// ([`count_exit`]).
#[derive(Default)]
pub struct Handoffs {
    count: Cell<u32>,
    /// The body's entry count when `count` started.
    since: Cell<i32>,
    /// How many iterations the innermost `for` loop around the point ran between the
    /// hand-offs counted.
    iterations: Cell<u32>,
    /// That loop's index at the last hand-off.
    index: Cell<i64>,
}

/// Whether every function lookup compiled code assumed still resolves the same way for an
/// activation `machine`. Every change that could alter such a lookup moves the
/// binding epoch and the symbol's lookup stamp (see `gp::WATCHED`), so an unchanged epoch, or
/// unchanged stamps of the speculated symbols, prove the lookups without walking them.
/// `thorough` walks them anyway, for an activation whose frame may have bound a watched name
/// before the name was watched. Tied code running for an enclosure other than the checked
/// one or its siblings walks, and leaves the epoch and stamps to the checked enclosure.
#[inline]
pub unsafe fn valid(compiled: &Compiled, machine: &Machine<'_>, thorough: bool) -> bool {
    // Every entry asks, and almost every time nothing moved the epoch since the last proof.
    if !thorough
        && heap().binding_epoch() == compiled.epoch.get()
        && unsafe { proven_for(compiled, machine.enclosure()) }
    {
        return true;
    }
    unsafe { prove(compiled, machine, thorough) }
}

#[inline(never)]
unsafe fn prove(compiled: &Compiled, machine: &Machine<'_>, thorough: bool) -> bool {
    let heap = heap();
    let epoch = heap.binding_epoch();
    let enclosure = machine.enclosure();
    let checked = unsafe { proven_for(compiled, enclosure) };
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
            && machine
                .local_binding(speculation.symbol)
                .is_some_and(|value| may_be_function(value))
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
unsafe fn still_valid(compiled: &Compiled, machine: &Machine<'_>, epoch: u64) -> bool {
    unsafe { heap().binding_epoch() == epoch || valid(compiled, machine, false) }
}

/// Whether the epoch and stamps of `compiled` speak for an activation with `enclosure`. Code
/// compiled for one enclosure only runs for that one.
#[inline(always)]
pub unsafe fn proven_for(compiled: &Compiled, enclosure: Sexp) -> bool {
    unsafe { !compiled.tied || compiled.checked.get().weak_key() == enclosure }
}

/// Whether portable `compiled` runs for `enclosure`, which is not the checked enclosure but
/// shares its parent. The closures one function expression makes on each call of its parent
/// (`make_scaler <- function(k) function(x) x * k`) are siblings in this sense, so they share
/// the checked enclosure's proof instead of each walking every speculation.
#[inline]
pub unsafe fn sibling_of_checked(compiled: &Compiled, enclosure: Sexp) -> bool {
    unsafe {
        let key = compiled.checked.get().weak_key();
        compiled.tied && key != values::nil() && key.enclosure() == enclosure.enclosure()
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

/// A tier state flag: the body was long enough that it waited for extra entries or backward
/// jumps before its next compile (see `jit::SIZE_WAIT_WORDS`), and it has waited. Invalidation
/// clears it, so every recompile waits again.
pub const SIZE_WAITED: i32 = 1 << 17;

/// The profile's tier state counts the body's refusals as too rarely called (`analysis::RARE`)
/// in four bits from here, so each attempt can wait twice as long as the one before.
pub const RARE_REFUSAL: i32 = 1 << 18;

/// A tier state flag: the body's code, compiled for one enclosure, met another, as the
/// functions one `function` expression makes on each call of its parent do. Its compiles then
/// speculate only on lookups every such enclosure resolves the same way, so that one portable
/// compilation serves them all instead of one per enclosure.
pub const SHARED: i32 = 1 << 22;

/// A tier state flag: calls of the body inlined without a frame kept needing one, so the
/// compiler no longer inlines it.
pub const NO_ELISION: i32 = 1 << 23;

/// A tier state flag: inlined calls in the body's compiled code kept reading a formal whose
/// promise needs code to force, which hands the call to tier 0. The compiler then neither
/// reads a formal for an inlined call before the body itself has, nor passes one on to an
/// inlined call that reads it.
pub const UNFORCED_FORMALS: i32 = 1 << 24;

/// A tier state flag: the body's inlined calls kept being unable to start without their
/// frames for reasons that are not the callees', as when the body runs under a debugger. Each
/// such call left compiled code, so the compiler no longer inlines calls with frames of their
/// own in the body, and its closure calls are tier 0's.
pub const NO_FRAMES: i32 = 1 << 25;

/// A tier state flag, the one a code image keeps: the body compiled in the default mode, where
/// the compile had to pass every test of what it repays. Those tests measure how often the body
/// is called and how long its loops run. A session started from a trained image has measured
/// neither when the body arrives hot at its first call, so the flag stands for them ([`repaid`]).
pub const TRAINED: i32 = profile::TIER_TRAINED;

const _: () = assert!(TRAINED > NO_FRAMES);

/// Whether a body the tier may still compile passed the tests of what its compile repays once
/// already: it compiled in this session, or in the one that trained its code image.
pub fn repaid(state: i32) -> bool {
    state >= 0 && (compiles(state) > 0 || state & TRAINED != 0)
}

/// The tier state of a body the tier no longer compiles and whose calls it no longer inlines
/// without a frame either.
pub const NEVER_ELIDED: i32 = -2;

/// Whether a body's tier state lets its calls be inlined without a frame. A body the tier
/// refused to compile on its own is still inlined: what makes it not worth compiling alone,
/// mostly that it runs nothing natively, says nothing about its calls.
pub fn elidable(state: i32) -> bool {
    if state < 0 {
        state == -1
    } else {
        state & NO_ELISION == 0
    }
}

/// The tier state of a body the tier stops compiling, which keeps whether its calls may still
/// be inlined.
pub fn refused(state: i32) -> i32 {
    if elidable(state) { -1 } else { NEVER_ELIDED }
}

pub fn rare_refusals(state: i32) -> i32 {
    (state / RARE_REFUSAL) & 0xf
}

/// Every backward jump the body has taken under the tier.
pub fn back_edges_seen(counters: &[i32]) -> u64 {
    u64::try_from(counters[profile::EDGES_BEFORE].saturating_add(counters[profile::BACK_EDGES]))
        .unwrap_or(0)
}

/// Makes the body's backward jumps count up from `start`, keeping [`back_edges_seen`].
pub fn restart_back_edges(counters: &mut [i32], start: i32) {
    counters[profile::EDGES_BEFORE] = counters[profile::EDGES_BEFORE]
        .saturating_add(counters[profile::BACK_EDGES])
        .saturating_sub(start);
    counters[profile::BACK_EDGES] = start;
}

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
    // Within the recording window before the threshold, tier 0 records type feedback, so the
    // activation that just deoptimized records the type that broke the speculation and the
    // recompile sees it.
    // A body that compiles only for its loop records through its loop, from half the back-edge
    // threshold, and its calls must never ask the tier again, so it keeps the entry count that
    // stops them asking.
    let loops_only = state >= 0 && state & LOOPS_ONLY != 0;
    counters[profile::ENTRIES] = if loops_only {
        NEVER_HOT
    } else {
        bytecode::tier().map_or(0, |tier| bytecode::recording_start(tier.entry_threshold))
    };
    restart_back_edges(counters, 0);
    counters[profile::NEXT] = i32::MIN;
    if compiles(state) >= MAX_COMPILES {
        counters[profile::TIER_STATE] = refused(state);
        counters[profile::RECORDING] = 0;
    } else if state >= 0 {
        counters[profile::TIER_STATE] = state & !SIZE_WAITED;
        counters[profile::RECORDING] = i32::from(!loops_only);
    }
}

unsafe fn machine<'a>(activation: &Activation) -> &'a mut Machine<'a> {
    unsafe { &mut *(activation.machine as *mut Machine<'a>) }
}

unsafe fn compiled<'a>(activation: &Activation) -> &'a Compiled {
    unsafe { &*activation.compiled }
}

unsafe fn layout<'a>(activation: &Activation) -> &'a Layout {
    unsafe { &*activation.layout }
}

/// Runs `body`, parking a panic in the activation and returning `fallback` instead.
///
/// An inlined call that runs without a frame is the current call of the warnings and errors
/// `body` raises, as a builtin called without a frame is: the interpreter's call marker names
/// it for as long as `body` runs. Compiled code itself raises nothing, so the marker is set
/// here and not when the call starts, which a hot loop does far more often than it calls a
/// helper that could raise.
unsafe fn guarded<T>(
    activation: *mut Activation,
    fallback: T,
    body: impl FnOnce(&mut Activation) -> T,
) -> T {
    let activation = unsafe { &mut *activation };
    let parked = activation.parked;
    if unsafe { (*parked).panic.is_some() } {
        return fallback;
    }
    // `body` runs from one call site, so that it inlines into the unwinding frame rather than
    // costing every helper call a frame of its own.
    let caller = unsafe { &layout(activation).inlined }
        .as_ref()
        .map(|inlined| {
            let state = interp::interp();
            state
                .assignment_call
                .replace((values::unbound(), inlined.call, state.frames.len()))
        });
    let result = catch_unwind(AssertUnwindSafe(|| body(&mut *activation)));
    if let Some(caller) = caller {
        interp::interp().assignment_call = caller;
    }
    match result {
        Ok(value) => value,
        Err(payload) => {
            unsafe { (*parked).panic = Some(payload) };
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
///
/// `body` must not release below the mark either, because compiled code holds the handles
/// beneath it: an inlined call's activation records the stack height at its first start. The
/// check is one compare on a path that already calls out, so it is on in release builds too. It
/// also means a helper outside `scoped` must leave no handles behind, or the next checked
/// helper aborts.
unsafe fn scoped<T>(body: impl FnOnce() -> T) -> T {
    let mark = heap().handle_mark();
    let value = body();
    let heap = heap();
    if heap.handle_mark() < mark {
        handles_released_below(mark);
    }
    heap.release_handles(mark);
    value
}

#[cold]
#[inline(never)]
fn handles_released_below(mark: usize) -> ! {
    panic!(
        "a helper released handles compiled code holds: the handle stack fell from {mark} to {}",
        heap().handle_mark()
    )
}

/// Holds `unwind` until compiled code hands the activation back. The collector traces it there
/// through [`interp::Interp::held_unwinds`].
fn park(activation: &mut Activation, unwind: Unwind) {
    unsafe { (*activation.parked).pending = Some(unwind) };
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
        if (*(*activation).parked).pending.is_some() {
            return fallback;
        }
        guarded(activation, fallback, body)
    }
}

/// The frame slot that holds `slot`'s variable, while the activation runs in a slot frame
/// without an environment. A helper may call this outside its panic guard, so it does not index.
unsafe fn frame_slot(
    activation: &Activation,
    machine: &Machine<'_>,
    slot: i64,
) -> Option<*mut Sexp> {
    unsafe {
        let position = (*layout(activation).frame.get(slot as usize)?)?;
        machine.frame_slot(position)
    }
}

unsafe fn slot_cell(activation: &Activation, slot: i64) -> *mut Sexp {
    unsafe {
        activation
            .registers
            .add(layout(activation).registers + slot as usize)
    }
}

/// Runs `step`, one tier-0 instruction of an activation. When `deferred` is set the
/// instruction signals nothing ([`Signalling`]): an error it raises comes back unsignalled, and
/// tier 0 signals it after the exit the error takes. `None` says the instruction gave up before
/// a signal that runs R code, so the exit must carry no error, which makes tier 0 run the
/// instruction again. The instruction runs no R code, so nothing nests.
///
/// A closure call without a frame always defers, because a handler would see a stack without
/// the call. A body's own frame defers where the instruction runs no R code otherwise: a
/// handler reads the frame, so the variables compiled code holds must be written back first,
/// which the exit does and which a write-back before every such instruction would cost each
/// time.
fn step_unsignalled<T>(deferred: bool, step: impl FnOnce() -> T) -> Option<T> {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            interp::interp().signalling = Signalling::Immediate;
        }
    }
    // `step` runs from one call site, so that it inlines into the caller.
    let restore = deferred.then(|| {
        interp::interp().signalling = Signalling::Deferred;
        Restore
    });
    let result = step();
    let abandoned = restore.is_some() && interp::interp().signalling == Signalling::Abandoned;
    drop(restore);
    (!abandoned).then_some(result)
}

/// Runs the tier-0 instruction at `pc` as `mode` says, one of [`stepping`]'s.
pub unsafe extern "C" fn jit_step(activation: *mut Activation, pc: i64, mode: i32) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let machine = machine(activation);
            let heap = heap();
            let epoch = heap.binding_epoch();
            let mark = heap.handle_mark();
            let deferred = mode == stepping::QUIET || layout(activation).inlined.is_some();
            let validate = mode == stepping::VALIDATED;
            let stepper = &mut activation.stepper;
            let stepped = step_unsignalled(deferred, || {
                bytecode::step_one(stepper, machine, pc as usize)
            });
            if heap.handle_mark() < mark {
                handles_released_below(mark);
            }
            heap.release_handles(mark);
            match stepped {
                Some(StepEnd::Next)
                    if validate && !still_valid(compiled(activation), machine, epoch) =>
                {
                    outcome::INVALIDATED
                }
                Some(StepEnd::Next) => outcome::OK,
                Some(StepEnd::Returned) => outcome::RETURNED,
                Some(StepEnd::Failed) => {
                    let unwind = activation.stepper.failure();
                    park(activation, unwind);
                    outcome::FAILED
                }
                // The instruction gave up before a signal, and tier 0 runs it again, so the
                // unwind it gave up with is dropped.
                None => {
                    drop(activation.stepper.failure());
                    outcome::FAILED
                }
            }
        })
    }
}

/// Runs the call at `pc` of a builtin that runs no R code for plain arguments
/// ([`bytecode::call_builtin_step`]), so nothing needs validating: [`outcome::OK`] or
/// [`outcome::FAILED`]. An argument the builtin could run R code for ([`observed`]) declines
/// the call with [`outcome::OBSERVED`] before anything ran. A nonzero `record` records the
/// result's type.
pub unsafe extern "C" fn jit_call_builtin(
    activation: *mut Activation,
    pc: i64,
    record: i32,
) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let machine = machine(activation);
            let code = machine.code.car().data().cast::<i32>().cast_const();
            let first = *code.add(pc as usize + 4) as usize;
            let count = *code.add(pc as usize + 5) as usize;
            let arguments = std::slice::from_raw_parts(activation.registers.add(first), count);
            if arguments.iter().any(|&argument| observed(argument)) {
                return outcome::OBSERVED;
            }
            let stepper = &mut activation.stepper;
            let called = step_unsignalled(true, || {
                scoped(|| bytecode::call_builtin_step(stepper, machine, pc as usize, record != 0))
            });
            match called {
                Some(Ok(())) => outcome::OK,
                Some(Err(unwind)) => {
                    park(activation, unwind);
                    outcome::FAILED
                }
                None => outcome::FAILED,
            }
        })
    }
}

/// Whether a builtin that runs no R code for plain arguments could run some for `value`: an
/// object's method, an environment's active binding, or the methods of an ALTREP class, which
/// a package may write in R. A compact sequence is an ALTREP vector of ry's own classes.
unsafe fn observed(value: Sexp) -> bool {
    unsafe {
        value.is_object()
            || value.kind() == SexpType::Environment
            || (value.is_altrep() && values::compact_sequence(value).is_none())
    }
}

/// Runs the element read or store at `pc` through tier 0's fast handler for it
/// ([`bytecode::fast_step`]) when `fast` is set, then through its generic handler where that
/// runs no R code ([`bytecode::quiet_step`]), so nothing needs validating: one of [`element`]'s
/// outcomes. The fast handler runs no R code, so it runs outside the panic guard, which would
/// cost every hit a stack frame.
pub unsafe extern "C" fn jit_element_step(activation: *mut Activation, pc: i64, fast: i32) -> i32 {
    unsafe {
        #[cfg(debug_assertions)]
        let mark = heap().handle_mark();
        if fast != 0 && bytecode::fast_step(machine(&*activation), pc as usize) {
            #[cfg(debug_assertions)]
            assert_eq!(
                heap().handle_mark(),
                mark,
                "a fast handler left handles behind, which the next stepped call releases"
            );
            return element::HANDLED;
        }
        checked(activation, element::FAILED, |activation| {
            let machine = machine(activation);
            let stepper = &mut activation.stepper;
            let stepped =
                step_unsignalled(true, || bytecode::quiet_step(stepper, machine, pc as usize));
            match stepped {
                Some(None) => element::DECLINED,
                Some(Some(Ok(()))) => element::STEPPED,
                Some(Some(Err(unwind))) => {
                    park(activation, unwind);
                    element::FAILED
                }
                None => element::FAILED,
            }
        })
    }
}

/// Runs the fused pass of the `Fused` instruction at `pc`: [`outcome::FUSED`] when it computed
/// the expression, [`outcome::OK`] when it declined and the ordinary code runs, and either
/// invalidated outcome when an argument's promise it forced rebound a function the code
/// speculated on.
pub unsafe extern "C" fn jit_fused(activation: *mut Activation, pc: i64) -> i32 {
    unsafe {
        checked(activation, outcome::FAILED, |activation| {
            let machine = machine(activation);
            let epoch = heap().binding_epoch();
            let fused = scoped(|| bytecode::fused_step(machine, pc as usize));
            let valid = || still_valid(compiled(activation), machine, epoch);
            match fused {
                Err(unwind) => {
                    park(activation, unwind);
                    outcome::FAILED
                }
                Ok(true) if valid() => outcome::FUSED,
                Ok(true) => outcome::FUSED_INVALIDATED,
                Ok(false) if valid() => outcome::OK,
                Ok(false) => outcome::INVALIDATED,
            }
        })
    }
}

/// Loads the variable `GetVar`, `Argument` or `Subscript` at `pc` reads into its slot. Returns
/// the slot's status. A load tier 0 must run ([`bytecode::load_declines`]) declines with
/// [`outcome::LOAD_FAILED`] and no error, which hands the instruction to tier 0.
pub unsafe extern "C" fn jit_load(activation: *mut Activation, pc: i64, slot: i64) -> i32 {
    unsafe {
        checked(activation, outcome::LOAD_FAILED, |activation| {
            let machine = machine(activation);
            match scoped(|| bytecode::load_declines(machine, pc as usize)) {
                Ok(false) => {}
                Ok(true) => return outcome::LOAD_FAILED,
                Err(unwind) => {
                    park(activation, unwind);
                    return outcome::LOAD_FAILED;
                }
            }
            let epoch = heap().binding_epoch();
            let reads = interp::interp().active_binding_reads;
            let in_slots = machine.env() == values::nil();
            let loaded = scoped(|| bytecode::load_variable(machine, pc as usize));
            let result = finish_load(activation, slot, loaded, epoch);
            // A promise the load forced can reach this frame (`sys.frame()`) and bind its
            // variables, which moves them out of their slots, so the values compiled code holds
            // may be stale. The load is recorded as an active binding's is.
            let moved = in_slots && machine.env() != values::nil();
            // Code that would reuse the value of a load that called an active binding hands the
            // activation to tier 0 after the load, and the recompile reads the variable again at
            // every use. Comparing a count also catches a binding that a forced promise read,
            // which only costs that variable its reuse. Code that already loads the variable at
            // every read says so in its metadata rather than in an argument, because an extra
            // argument at every load site costs the register allocator an instruction in loops.
            if result != outcome::LOAD_FAILED
                && (interp::interp().active_binding_reads != reads || moved)
                && !bit(&layout(activation).volatile, slot as usize)
            {
                bytecode::record_active_binding(machine.code, pc as usize);
                return outcome::LOAD_INVALIDATED;
            }
            result
        })
    }
}

/// Records the type of a value compiled code loaded inline and found to be of a type it did not
/// speculate on, so that the recompile after the deoptimization sees it. The inline load skips
/// recording otherwise, because a value of the speculated type adds nothing to the profile.
pub unsafe extern "C" fn jit_record(activation: *mut Activation, pc: i64, value: *mut Header) {
    unsafe {
        if let Some(value) = Sexp::from_raw(value) {
            bytecode::record_value(machine(&*activation).code, pc as usize, value);
        }
    }
}

/// Finds a variable as compiled code's inline probe would when the activation's frame is
/// reified or the variable is free in the body: in the activation's own environment, else in its
/// enclosure when that is a function's frame, through the lookup slot's hint. Only a plain value
/// or an evaluated promise counts, since that case runs no code and changes nothing; the result
/// is the value with its low bit set when the frame does not own it (the enclosure bound it, or
/// it is a promise's value, which the caller may hold too), or 0 when tier 0's lookup must run.
/// The probe lives out of line because inlining both frame walks at every variable load
/// made a large body's code grow by about a hundred instructions per load.
pub unsafe extern "C" fn jit_probe(
    activation: *const Activation,
    symbol: *mut Header,
    lookup: i64,
) -> u64 {
    unsafe {
        let activation = &*activation;
        let Some(symbol) = Sexp::from_raw(symbol) else {
            return 0;
        };
        let heap = heap();
        let hint = &mut *activation.hints.add(lookup as usize);
        let env = *activation.header;
        let (local, enclosure) = if env == values::nil() {
            (None, activation.enclosure)
        } else {
            (heap.frame_cell_hinted(env, symbol, hint), env.enclosure())
        };
        let (cell, tag) = match local {
            Some(cell) => (cell, 0),
            None if enclosure.has_gp(gp::FUNCTION_FRAME) => {
                match heap.frame_cell_hinted(enclosure, symbol, hint) {
                    Some(cell) => (cell, 1),
                    None => return 0,
                }
            }
            None => return 0,
        };
        if cell.has_gp(gp::ACTIVE_BINDING) {
            return 0;
        }
        let value = cell.cdr();
        if value == values::missing_arg() || value == values::unbound() {
            return 0;
        }
        let (value, tag) = if value.kind() == SexpType::Promise {
            match value.car() {
                forced if forced == values::unbound() => return 0,
                forced => (forced, 1),
            }
        } else {
            (value, tag)
        };
        value.as_ptr() as u64 | tag
    }
}

/// Loads a slot's variable for an element assignment, which has no inline cache.
pub unsafe extern "C" fn jit_load_slot(activation: *mut Activation, slot: i64) -> i32 {
    unsafe {
        checked(activation, outcome::LOAD_FAILED, |activation| {
            let machine = machine(activation);
            let symbol = layout(activation).slots[slot as usize];
            let epoch = heap().binding_epoch();
            let in_slots = machine.env() == values::nil();
            let loaded = scoped(|| {
                if machine.env() == values::nil() {
                    return match frame_slot(activation, machine, slot).map(|slot| *slot) {
                        Some(value) if value.kind() == SexpType::Promise => {
                            let forced = if value.car() == values::unbound() {
                                let forced = eval::force_promise(value)?;
                                heap().root(forced);
                                forced
                            } else {
                                value.car()
                            };
                            Ok((forced, false))
                        }
                        Some(value)
                            if value != values::unbound() && value != values::missing_arg() =>
                        {
                            Ok((value, true))
                        }
                        Some(value) if value == values::missing_arg() => {
                            let env = machine
                                .reify(eval::Reify::Site(eval::Site::MissingArgumentLookup))?;
                            eval::eval_symbol(symbol, env).map(|value| (value, true))
                        }
                        _ => eval::eval_symbol(symbol, machine.enclosure())
                            .map(|value| (value, false)),
                    };
                }
                let local = heap()
                    .frame_cell(machine.env(), symbol)
                    .is_some_and(|cell| cell.cdr().kind() != SexpType::Promise);
                eval::eval_symbol(symbol, machine.env()).map(|value| (value, local))
            });
            let result = finish_load(activation, slot, loaded, epoch);
            // As in `jit_load`: a forced promise that moved the frame's variables out of their
            // slots may have rebound them.
            if result != outcome::LOAD_FAILED && in_slots && machine.env() != values::nil() {
                return outcome::LOAD_INVALIDATED;
            }
            result
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
                    && !still_valid(compiled(activation), machine(activation), epoch)
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
            let found = peek_variable(activation, slot);
            // Only a formal is worth counting: the recompile stops reading formals early,
            // and reads every other variable exactly as before.
            if found == outcome::LOAD_FAILED
                && let Some(&symbol) = layout(activation).slots.get(slot as usize)
                && values::pairlist_items(machine(activation).function.car())
                    .iter()
                    .any(|&(formal, _)| formal == symbol)
            {
                count_declined_load(compiled(activation));
            }
            found
        })
    }
}

/// Counts a time an inlined call could not take one of the compiled body's formals without
/// running code. Once that is frequent among the body's inlined calls, the code is discarded
/// and the body marked [`UNFORCED_FORMALS`].
unsafe fn count_declined_load(compiled: &Compiled) {
    let count = compiled.declined_loads.get().saturating_add(1);
    compiled.declined_loads.set(count);
    if frequent(count, frame_entries(compiled)) {
        unsafe { retire(compiled, UNFORCED_FORMALS) };
    }
}

/// How often the inlined calls of `compiled` have started, over all of them.
fn frame_entries(compiled: &Compiled) -> u64 {
    compiled
        .frames
        .iter()
        .filter_map(|layout| layout.inlined.as_ref())
        .map(|inlined| u64::from(inlined.entries.get()))
        .sum()
}

/// Discards `compiled`, once, and marks its body with the tier state `flag` so that the
/// recompile avoids what kept going wrong.
unsafe fn retire(compiled: &Compiled, flag: i32) {
    if compiled.retired.replace(true) {
        return;
    }
    unsafe {
        let counters = values::ints_mut(compiled.code.cdr().element(PROFILE));
        if counters[profile::TIER_STATE] >= 0 {
            counters[profile::TIER_STATE] |= flag;
        }
        invalidate(compiled.code);
    }
}

unsafe fn peek_variable(activation: &mut Activation, slot: i64) -> i32 {
    unsafe {
        {
            let machine = machine(activation);
            let Some(&symbol) = layout(activation).slots.get(slot as usize) else {
                return outcome::LOAD_FAILED;
            };
            let heap = heap();
            let empty = heap.known().empty_env;
            let mut env = machine.env();
            let mut local = true;
            if env == values::nil() {
                if let Some(frame_slot) = frame_slot(activation, machine, slot) {
                    let mut value = *frame_slot;
                    let promised = value.kind() == SexpType::Promise;
                    if promised {
                        value = value.car();
                    }
                    if value == values::missing_arg() || value == values::unbound() {
                        return outcome::LOAD_FAILED;
                    }
                    *slot_cell(activation, slot) = value;
                    return if promised { 2 } else { 1 };
                }
                env = machine.enclosure();
                local = false;
            }
            while env != empty {
                if let Some(cell) = heap.frame_cell(env, symbol) {
                    if cell.has_gp(gp::ACTIVE_BINDING) {
                        return outcome::LOAD_FAILED;
                    }
                    let mut value = cell.cdr();
                    let promised = value.kind() == SexpType::Promise;
                    if promised {
                        if value.car() == values::unbound() {
                            return outcome::LOAD_FAILED;
                        }
                        value = value.car();
                    }
                    if value == values::missing_arg() || value == values::unbound() {
                        return outcome::LOAD_FAILED;
                    }
                    *slot_cell(activation, slot) = value;
                    return if local && !promised { 1 } else { 2 };
                }
                local = false;
                env = env.enclosure();
            }
            outcome::LOAD_FAILED
        }
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
            let machine = machine(activation);
            // A closure a slot frame makes holds the frame's pending environment, which becomes
            // the frame's environment, as the same object, once something reads it.
            let env = match machine.env() {
                env if env == values::nil() => eval::pending_env_of(machine.header),
                env => env,
            };
            let matches = value.kind() == SexpType::Closure
                && value.car() == formals
                && value.tag() == env
                && bytecode::closure_body(value) == body;
            matches as i32
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

/// Leaves compiled code at exit `exit`, whose raw values are in `scratch`: writes the frame
/// back as the exit lists it and deoptimizes like [`jit_deopt`]. An exit inside an inlined
/// call first gives every inlined call around it its real frame and runs those calls to
/// their ends in tier 0 ([`materialize`]).
pub unsafe extern "C" fn jit_deopt_exit(
    activation: *mut Activation,
    scratch: *const i64,
    exit: i64,
) -> *mut Header {
    unsafe {
        let compiled = compiled(&*activation);
        let Some(exit) = compiled.exits.get(exit as usize) else {
            panic!("compiled code named a deoptimization exit it does not have");
        };
        if exit.parent.is_some() {
            return guarded(activation, std::ptr::null_mut(), |root| {
                materialize(root, compiled, exit, scratch)
            });
        }
        write_back(activation, exit, scratch);
        if exit.why == reason::PENDING && (*(*activation).parked).pending.is_some() {
            count_leave(compiled);
        } else if exit.why == reason::PENDING || exit.why == reason::HANDOFF {
            count_exit(&*activation, compiled, exit);
        }
        jit_deopt(activation, exit.pc as i64, exit.why)
    }
}

/// One slot of an exit as compiled code saved it.
#[derive(Clone, Copy)]
struct SavedSlot {
    slot: u32,
    status: i64,
    /// The value's [`representation`], never [`representation::NUM`], and its bits. A boxed
    /// value is in the slot's root cell instead.
    kind: i32,
    bits: i64,
}

/// Boxes the registers `exit` lists into `activation`'s window and writes its loops' words,
/// reading the exit's raw values from `scratch`. Returns the slots it lists, which the caller
/// writes to the frame, and the cell after the last one it read, where an inlined call's exit
/// goes on with its arguments ([`Inlined::formals`]).
unsafe fn restore(
    activation: *mut Activation,
    exit: &Exit,
    scratch: *const i64,
) -> (SmallVec<[SavedSlot; 8]>, *const i64) {
    unsafe {
        let mut cell = scratch.add(exit.base as usize);
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
        let mut slots = SmallVec::new();
        for &(slot, kind) in &exit.slots {
            let status = i64::from(next().cast::<i32>().read());
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
            slots.push(SavedSlot {
                slot,
                status,
                kind,
                bits,
            });
        }
        for &index in &exit.loops {
            let words = (*activation).loops.add(LOOP_WORDS * index as usize);
            for word in 0..LOOP_WORDS {
                *words.add(word) = next().read();
            }
        }
        (slots, cell)
    }
}

/// [`restore`], then writes the dirty slots to the frame.
unsafe fn write_back(activation: *mut Activation, exit: &Exit, scratch: *const i64) {
    unsafe {
        let (slots, _) = restore(activation, exit, scratch);
        flush_dirty(activation, &slots);
    }
}

unsafe fn flush_dirty(activation: *mut Activation, slots: &[SavedSlot]) {
    for saved in slots {
        if saved.status == status::RUNTIME_DIRTY {
            // A failure parks its error, which tier 0 raises when it resumes.
            unsafe { jit_flush(activation, i64::from(saved.slot), saved.kind, saved.bits) };
        }
    }
}

/// Leaves compiled code at `innermost`, an exit inside an inlined call. Up to here the call,
/// and every inlined call around it, ran without a frame, so each one first gets the frame
/// tier 0 would have built for it, outermost first. Then the calls finish in tier 0 from the
/// inside out: the innermost resumes at the exit's instruction, and each caller resumes after
/// its call with the result, or raises the callee's error at the call. The compiled body
/// itself deoptimizes the same way, which is this function's result.
unsafe fn materialize(
    root: *mut Activation,
    compiled: &Compiled,
    innermost: &Exit,
    scratch: *const i64,
) -> *mut Header {
    unsafe {
        let mut chain: SmallVec<[&Exit; 4]> = SmallVec::new();
        chain.push(innermost);
        while let Some(parent) = chain.last().and_then(|exit| exit.parent) {
            let Some(exit) = compiled.exits.get(parent as usize) else {
                panic!("a deoptimization exit names a caller's exit that does not exist");
            };
            chain.push(exit);
        }
        let inlined_of = |exit: &Exit| -> &Inlined {
            match compiled
                .frames
                .get(exit.frame as usize)
                .and_then(|layout| layout.inlined.as_ref())
            {
                Some(inlined) => inlined,
                None => panic!("a deoptimization exit of an inlined call is in no inlined frame"),
            }
        };
        let child_of = |exit: &Exit| (*root).children.add(exit.frame as usize - 1);
        let (Some(&body), Some(&outermost)) = (chain.last(), chain.iter().rev().nth(1)) else {
            panic!("an inlined call's deoptimization exit has no caller");
        };
        let state = interp::interp();
        let parked = (*root).parked;
        if crate::logging() {
            let code = inlined_of(innermost).code;
            let op = values::ints(code.car())
                .get(innermost.pc)
                .and_then(|&word| bytecode::op_of(word as u32));
            eprintln!(
                "jit: deopt at pc {} ({op:?}, reason {}) in {} inlined in {}",
                innermost.pc,
                innermost.why,
                crate::describe(code),
                crate::describe(compiled.code)
            );
        }
        write_back(root, body, scratch);
        // The windows of the body's other inlined calls lie above the innermost call's, and
        // tier 0 puts the next call's window at the end of the file.
        let window = compiled
            .frames
            .get(innermost.frame as usize)
            .map_or(0, Layout::top);
        bytecode::release_registers((*root).root_top + window);
        let srcref = state.srcref;
        // A speculation on the callee's own feedback failed, so the recompile needs the type
        // that broke it. The callee may not be recording, so it records while this call
        // finishes in tier 0, which runs the instruction that saw the value.
        let recording =
            (innermost.why == reason::SPECULATION && (*parked).pending.is_none()).then(|| {
                invalidate(compiled.code);
                let counters = values::ints_mut(inlined_of(innermost).code.cdr().element(PROFILE));
                std::mem::replace(&mut counters[profile::RECORDING], 1)
            });
        // A condition the callee has to signal under its frame is the callee's doing as much
        // as an instruction it hands to tier 0: a builtin that warns under a calling handler
        // on every call would otherwise leave every time.
        if innermost.why == reason::HANDOFF || innermost.why == reason::PENDING {
            count_handoff(compiled, inlined_of(innermost));
        }

        // Chain positions of the frames pushed so far with their depths, outermost first.
        let mut entered: SmallVec<[(usize, usize); 4]> = SmallVec::new();
        let mut failed = None;
        for (position, &exit) in chain.iter().enumerate().rev().skip(1) {
            let inlined = inlined_of(exit);
            let child = child_of(exit);
            let activation = &raw mut (*child).activation;
            let caller = match inlined.parent {
                0 => *(*root).header,
                parent => {
                    *(*(*root).children.add(parent as usize - 1))
                        .activation
                        .header
                }
            };
            let (slots, arguments) = restore(activation, exit, scratch);
            match enter_inlined(&*root, child, inlined, caller, arguments) {
                Ok(depth) => {
                    entered.push((position, depth));
                    flush_dirty(activation, &slots);
                }
                Err(unwind) => {
                    failed = Some(unwind);
                    break;
                }
            }
        }

        let mut result = match failed {
            Some(unwind) => Err(unwind),
            None => {
                let Some((_, depth)) = entered.pop() else {
                    panic!("an inlined call's deoptimization exit has no frame to resume");
                };
                let machine = &(*child_of(innermost)).machine;
                state.srcref = srcref;
                let result = bytecode::resume_slots(
                    machine.function,
                    machine.header,
                    innermost.pc,
                    (*parked).pending.take(),
                );
                if let Some(recording) = recording {
                    let counters = inlined_of(innermost).code.cdr().element(PROFILE);
                    values::ints_mut(counters)[profile::RECORDING] = recording;
                }
                eval::leave(depth, result)
            }
        };
        while let Some((position, depth)) = entered.pop() {
            let (Some(&exit), Some(&inner)) =
                (chain.get(position), chain.get(position.wrapping_sub(1)))
            else {
                panic!("a frame resumes after an inlined call that has no exit");
            };
            let callee = inlined_of(inner);
            let child = child_of(exit);
            let (function, header) = ((*child).machine.function, (*child).machine.header);
            let resumed = match result {
                Ok(value) => {
                    *(*child).activation.registers.add(callee.dst as usize) = value;
                    bytecode::resume_slots(function, header, exit.pc, None)
                }
                Err(unwind) => {
                    bytecode::resume_slots(function, header, callee.call_pc as usize, Some(unwind))
                }
            };
            result = eval::leave(depth, resumed);
        }
        let callee = inlined_of(outermost);
        match result {
            Ok(value) => {
                *(*root).registers.add(callee.dst as usize) = value;
                jit_deopt(root, body.pc as i64, reason::HANDOFF)
            }
            Err(unwind) => {
                park(&mut *root, unwind);
                count_leave(compiled);
                jit_deopt(root, i64::from(callee.call_pc), reason::PENDING)
            }
        }
    }
}

/// Gives the inlined call `child` ran without a frame its real one: binds each formal in the
/// frame's slots as the call would have, and pushes the frame, whose depth is the result.
/// `caller` is the calling frame's environment, or `NULL` while that frame, which is then the
/// slot owner, has none. `arguments` is the part of the call's exit that says what it forced:
/// a bitset of formals, then the arguments the caller evaluated ([`Inlined::formals`]).
///
/// A formal the call has not forced is bound to a promise of its argument over the caller, as
/// tier 0 binds it, so reading it later evaluates the argument then. A formal it has forced
/// is bound to a forced promise of the argument's value.
unsafe fn enter_inlined(
    root: &Activation,
    child: *mut Child,
    inlined: &Inlined,
    caller: Sexp,
    arguments: *const i64,
) -> R<usize> {
    unsafe {
        let state = interp::interp();
        let header = (*child).machine.header;
        let mark = heap().handle_mark();
        // The caller's position is current again from here, for the frame that is pushed and
        // for a failure before that.
        state.srcref = (*child).srcref;
        // The slots hold whatever the calls inlined beside this one left there, because the
        // calls of one caller share a window and each lays its registers out after its own
        // slot count. Tier 0 reads a slot as bound unless it holds the unbound marker.
        std::slice::from_raw_parts_mut(bytecode::register_address(header + 1), inlined.frame_slots)
            .fill(values::unbound());
        let forced = arguments.read() as u64;
        let mut saved = arguments.add(1);
        let passed = values::pairlist_items(inlined.call.cdr());
        let mut args: SmallVec<[Arg; 4]> = passed
            .iter()
            .map(|&(tag, value)| Arg { tag, value })
            .collect();
        let (mut missing, mut promised) = (0u64, 0u64);
        let mut formal = inlined.function.car();
        for (index, (&source, &position)) in
            inlined.formals.iter().zip(&inlined.positions).enumerate()
        {
            let default = formal.car();
            formal = formal.cdr();
            let cell = saved;
            if matches!(source, ArgumentSource::Saved(_)) {
                saved = saved.add(1);
            }
            let argument = passed.get(position as usize).filter(|_| position >= 0);
            let bound = (|| {
                let Some(&(_, expression)) = argument else {
                    return Ok(default);
                };
                // Tier 0 binds a constant argument as the object the call holds.
                if !matches!(
                    expression.kind(),
                    SexpType::Symbol | SexpType::Language | SexpType::Promise | SexpType::Bytecode
                ) {
                    return Ok(expression);
                }
                if forced >> index & 1 == 0 {
                    return if caller == values::nil() {
                        eval::slot_promise(state.slot_owner as usize, expression)
                    } else {
                        eval::make_promise(expression, caller)
                    };
                }
                let value = match source {
                    ArgumentSource::Saved(representation::INTEGER) => {
                        values::scalar_integer(cell.cast::<i32>().read())?
                    }
                    ArgumentSource::Saved(representation::DOUBLE) => {
                        values::scalar_real(cell.cast::<f64>().read())?
                    }
                    ArgumentSource::Saved(representation::LOGICAL) => {
                        values::scalar_logical(cell.cast::<i32>().read())?
                    }
                    ArgumentSource::Saved(_) => match Sexp::from_raw(cell.read() as *mut Header) {
                        Some(value) => value,
                        None => panic!("an inlined call's exit saved no object for an argument"),
                    },
                    ArgumentSource::Root(symbol) => match passed_binding(root, symbol) {
                        Some(held) if held.kind() == SexpType::Promise => held.car(),
                        Some(held) => held,
                        None => panic!("an inlined call forced a formal its caller does not bind"),
                    },
                    ArgumentSource::Literal(value) => value,
                    ArgumentSource::Unread | ArgumentSource::Omitted => {
                        panic!("an inlined call forced an argument compiled code never reads")
                    }
                };
                eval::forced_promise(expression, value)
            })();
            let value = match bound {
                Ok(value) => value,
                Err(unwind) => {
                    // The slots bound so far keep their counts, which only makes their values
                    // look shared.
                    bytecode::release_registers(header);
                    return Err(unwind);
                }
            };
            value.increment_refcount();
            *bytecode::register_address(header + 1 + index) = value;
            match argument {
                Some(_) => {
                    if let Some(arg) = args.get_mut(position as usize) {
                        arg.value = value;
                    }
                }
                // Tier 0 marks a formal that takes its default as missing, and makes the
                // promise of a default that is code once the frame is pushed.
                None if default != values::missing_arg() => {
                    missing |= 1 << index;
                    if !(default.kind().is_atomic() || default == values::nil()) {
                        promised |= 1 << index;
                    }
                }
                None => {}
            }
        }
        heap().release_handles(mark);
        let entered = eval::enter(
            inlined.call,
            inlined.function,
            caller,
            &args,
            &[],
            header as u32,
            missing,
            promised,
            Entering::Midway,
        )?;
        if entered.header == interp::NO_SLOTS {
            panic!("an inlined call ran whole where only its frame was to be pushed");
        }
        Ok(entered.depth as usize)
    }
}

/// What the formal `symbol` of the compiled body `root` runs is bound to, unless the binding
/// is active.
unsafe fn passed_binding(root: &Activation, symbol: Sexp) -> Option<Sexp> {
    unsafe {
        let machine = machine(root);
        let env = machine.env();
        if env == values::nil() {
            machine.local_binding(symbol)
        } else {
            heap()
                .frame_cell(env, symbol)
                .filter(|cell| !cell.has_gp(gp::ACTIVE_BINDING))
                .map(|cell| cell.cdr())
        }
    }
}

/// Counts a time the inlined call `inlined` needed its real frame because of the callee.
/// Once that is frequent the callee is marked so that no compile inlines it, and the compiled
/// body is discarded.
unsafe fn count_handoff(compiled: &Compiled, inlined: &Inlined) {
    let count = inlined.handoffs.get().saturating_add(1);
    inlined.handoffs.set(count);
    if !frequent(count, u64::from(inlined.entries.get())) {
        return;
    }
    unsafe {
        let counters = values::ints_mut(inlined.code.cdr().element(PROFILE));
        let state = counters[profile::TIER_STATE];
        counters[profile::TIER_STATE] = match state {
            _ if state == profile::TIER_OFF => state,
            ..0 => NEVER_ELIDED,
            _ => state | NO_ELISION,
        };
        retire(compiled, 0);
    }
}

/// Counts a time the body's own frame handed the instruction of `exit` to tier 0, or gave
/// it up before a signal. A hand-off costs several times what a step of the instruction
/// does, so once the hand-offs are frequent ([`frequent`]) among the body's entries and the
/// iterations of the loop around the instruction, the instruction is marked
/// ([`feedback::HANDED_OFF`]) and the code discarded: the recompile makes the instruction
/// tier 0's step, which never leaves. Otherwise the count starts again, so only hand-offs
/// close together mark it.
///
/// Compiled code counts no iterations, so those between two hand-offs come from the loop's
/// index: its advance since the last one, or the index itself where the loop started again.
unsafe fn count_exit(activation: &Activation, compiled: &Compiled, exit: &Exit) {
    if compiled.retired.get() {
        return;
    }
    unsafe {
        let constants = compiled.code.cdr();
        let entered = values::ints(constants.element(PROFILE))[profile::ENTRIES];
        let handoffs = &exit.handoffs;
        let count = handoffs.count.get().saturating_add(1);
        handoffs.count.set(count);
        let index = match exit.loops.last() {
            Some(&innermost) => *activation.loops.add(LOOP_WORDS * innermost as usize),
            None => 0,
        };
        let last = handoffs.index.replace(index);
        if count == 1 {
            handoffs.since.set(entered.saturating_sub(1));
            handoffs.iterations.set(0);
        } else {
            let advance = if index > last { index - last } else { index };
            let advance = u32::try_from(advance).unwrap_or(u32::MAX);
            handoffs
                .iterations
                .set(handoffs.iterations.get().saturating_add(advance));
        }
        if count < MAX_FRAME_HANDOFFS {
            return;
        }
        // Code a failed speculation discarded keeps running in the activations that entered
        // it, and the body's entries then count for the code that replaced it.
        let tier1 = constants.element(TIER1);
        if tier1 == values::nil()
            || !std::ptr::eq(tier1.external_address().cast::<Compiled>(), compiled)
        {
            return;
        }
        let work = |handoffs: &Handoffs| {
            let entries = entered.saturating_sub(handoffs.since.get());
            u64::try_from(entries).unwrap_or(0) + u64::from(handoffs.iterations.get())
        };
        if !frequent(count, work(handoffs)) {
            handoffs.count.set(0);
            return;
        }
        let bits = values::ints_mut(constants.element(FEEDBACK));
        let marked = bits
            .get(exit.pc)
            .is_none_or(|&bits| bits as u32 & feedback::HANDED_OFF != 0);
        if crate::logging() {
            eprintln!(
                "jit: {} handed pc {} to tier 0 {count} times{}",
                crate::describe(compiled.code),
                exit.pc,
                if marked { " as a step" } else { "" },
            );
        }
        // The instruction is already what the mark asks for, so no recompile keeps it in
        // compiled code, and the body is cheaper in tier 0.
        if marked {
            retire(compiled, 0);
            let counters = values::ints_mut(constants.element(PROFILE));
            counters[profile::TIER_STATE] = refused(counters[profile::TIER_STATE]);
            counters[profile::RECORDING] = 0;
            return;
        }
        // One recompile serves every point that is on its way to the same verdict.
        for other in &compiled.exits {
            let halfway = other.handoffs.count.get() * 2 >= MAX_FRAME_HANDOFFS
                && u64::from(other.handoffs.count.get()) * 8 > work(&other.handoffs);
            if other.parent.is_none()
                && (std::ptr::eq(other, exit) || halfway)
                && let Some(bits) = bits.get_mut(other.pc)
            {
                *bits |= feedback::HANDED_OFF as i32;
            }
        }
        retire(compiled, 0);
    }
}

/// Counts a time `compiled` left for a reason a recompile would meet again. Leaving costs more
/// than several entries save, so a body that leaves often is cheaper in tier 0: an error that
/// unwinds through the body on most calls, as under a `tryCatch` that catches one each time,
/// would otherwise pay for the exit every time.
unsafe fn count_leave(compiled: &Compiled) {
    // A loop entered by on-stack replacement runs compiled without an entry being counted,
    // so the entries of a body with loops do not say how much its code ran.
    if compiled.retired.get() || compiled.loops != 0 {
        return;
    }
    let count = compiled.leaves.get().saturating_add(1);
    compiled.leaves.set(count);
    if count < MAX_FRAME_HANDOFFS {
        return;
    }
    unsafe {
        let constants = compiled.code.cdr();
        // Code a failed speculation discarded keeps running in the activations that entered
        // it, and the body's entries then count for the code that replaced it.
        let tier1 = constants.element(TIER1);
        if tier1 == values::nil()
            || !std::ptr::eq(tier1.external_address().cast::<Compiled>(), compiled)
        {
            return;
        }
        let counters = values::ints_mut(constants.element(PROFILE));
        let entered = counters[profile::ENTRIES];
        let entries = entered.saturating_sub(compiled.leaves_since.get());
        if !frequent(count, u64::try_from(entries).unwrap_or(0)) {
            compiled.leaves.set(0);
            compiled.leaves_since.set(entered);
            return;
        }
        if crate::logging() {
            eprintln!(
                "jit: {} left compiled code {count} times in {entries} entries",
                crate::describe(compiled.code)
            );
        }
        retire(compiled, 0);
        let counters = values::ints_mut(constants.element(PROFILE));
        counters[profile::TIER_STATE] = refused(counters[profile::TIER_STATE]);
        counters[profile::RECORDING] = 0;
    }
}

/// Counts a time an inlined call of `compiled` could not start without its frame for a
/// reason that is not the callee's. Once that is frequent among the body's inlined calls, the
/// body is recompiled without inlined frames ([`NO_FRAMES`]).
unsafe fn count_declined_frame(compiled: &Compiled) {
    let count = compiled.declined_frames.get().saturating_add(1);
    compiled.declined_frames.set(count);
    if frequent(count, frame_entries(compiled)) {
        unsafe { retire(compiled, NO_FRAMES) };
    }
}

/// Starts the inlined call of frame `frame`, made from `parent`, without a frame of its own,
/// and returns the call's activation. Until the call ends or deoptimizes, it is visible to R
/// code only as the current call of warnings ([`guarded`]), so compiled code hands every
/// instruction that could run R code to tier 0, which gives the call its frame first.
///
/// Returns null when the call needs its frame from the start, as tier 0's own frame entry
/// decides. Compiled code then hands the call instruction to tier 0. An established calling
/// handler does not need the frame: an instruction run for the call signals nothing
/// ([`step_unsignalled`]), so a handler runs only after an exit has built every frame.
///
/// A hot loop runs this once per iteration for each call it inlines, so the path that starts
/// the call only tests what can change between two starts and writes nothing but the entry
/// count: compiled code saves the caller's srcref itself, and the call's handle mark is the
/// one its first start took, because every helper leaves the handle stack as it found it.
/// It cannot panic, which is why it runs outside [`guarded`].
pub unsafe extern "C" fn jit_enter_frame(parent: *mut Activation, frame: i64) -> *mut Activation {
    unsafe {
        let child = (*parent).children.add(frame as usize - 1);
        if (*child).activation.compiled.is_null() {
            return first_start(parent, child, frame);
        }
        start(parent, child)
    }
}

/// What every start of an inlined call tests ([`jit_enter_frame`]).
#[inline(always)]
unsafe fn start(parent: *mut Activation, child: *mut Child) -> *mut Activation {
    unsafe {
        let inlined = &*(*child).inlined;
        let state = interp::interp();
        let function = inlined.function;
        inlined.entries.set(inlined.entries.get().saturating_add(1));
        // The frames an exit would push all fit under the depth limit, so pushing them
        // cannot fail there.
        let compiled: &Compiled = &*(*child).activation.compiled;
        let depth = state.frames.len() + compiled.frames.len();
        // The activation was written for the environment the closure had at the first start,
        // which `environment<-` can replace between two starts.
        let enclosure = function.tag();
        let needs_frame = function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP)
            || function.cdr() != inlined.code
            || enclosure != (*child).activation.enclosure
            || enclosure.has_gp(gp::SLOT_FRAME);
        let hooks = activity::hooks();
        if needs_frame
            || (*(*parent).parked).pending.is_some()
            || hooks & activity::CAPTURE != 0
            || !state.object_systems
            || state.debugger.is_some()
            || moves::tracking()
            || depth > state.max_depth as usize
        {
            return declined(parent, child, needs_frame);
        }
        if hooks & activity::SUMMARY != 0 {
            interp::profile_summary::count_inlined_call(function);
        }
        debug_assert!((*child).machine.mark == heap().handle_mark());
        &raw mut (*child).activation
    }
}

/// [`jit_enter_frame`] for a call's first start in an entry of the compiled body, which
/// writes the call's activation. A body that calls its callee once per entry starts every
/// call here, so this path cannot panic either and runs outside [`guarded`]. It is out of
/// line, so that the other starts need no stack frame.
#[inline(never)]
unsafe fn first_start(parent: *mut Activation, child: *mut Child, frame: i64) -> *mut Activation {
    unsafe {
        let Some(function) = prepare(&*parent, child, frame) else {
            debug_assert!(
                false,
                "compiled code entered an inlined call it does not have"
            );
            return std::ptr::null_mut();
        };
        // Loading a namespace looks at the call stack, so the loader always has its frame.
        if function == interp::interp().namespace_loader {
            let inlined = &*(*child).inlined;
            inlined.entries.set(inlined.entries.get().saturating_add(1));
            let handed_off = declined(parent, child, true);
            (*child).activation.compiled = std::ptr::null();
            return handed_off;
        }
        start(parent, child)
    }
}

/// Counts a start [`jit_enter_frame`] declined, against the callee when it `needs_frame` and
/// against the compiled body otherwise, and answers null.
#[cold]
#[inline(never)]
unsafe fn declined(
    parent: *mut Activation,
    child: *mut Child,
    needs_frame: bool,
) -> *mut Activation {
    unsafe {
        guarded(parent, std::ptr::null_mut(), |parent| {
            let compiled: &Compiled = &*(*child).activation.compiled;
            if needs_frame {
                count_handoff(compiled, &*(*child).inlined);
            } else if (*parent.parked).pending.is_none() {
                count_declined_frame(compiled);
            }
            std::ptr::null_mut()
        })
    }
}

/// The most words of an inlined call's window that compiled code clears with one store each
/// when the call ends, instead of calling [`jit_clear_window`].
pub const CLEARED_INLINE: usize = 16;

/// Writes `child`, the activation of the inlined call of frame `frame`, for this entry of the
/// compiled body, before the call's first start in it. Answers the call's closure, or `None`
/// for a frame the compiled body does not inline.
#[inline(always)]
unsafe fn prepare(parent: &Activation, child: *mut Child, frame: i64) -> Option<Sexp> {
    unsafe {
        let layout = compiled(parent).frames.get(frame as usize)?;
        let inlined = layout.inlined.as_ref()?;
        let function = inlined.function;
        let header = parent.root_top + inlined.header;
        let base = header + 1 + inlined.frame_slots;
        // The window holds `NULL` from the entry of the compiled body, which is what the
        // header and the slot root cells start as. Nothing reads the frame's slots while the
        // call has no frame, and `enter_inlined` fills them when it gets one.
        child.write(Child {
            activation: Activation {
                registers: bytecode::register_address(base),
                loops: std::ptr::null_mut(),
                visible: parent.visible,
                entry_pc: 0,
                header: bytecode::register_address(header),
                hints: bytecode::lookup_hints(inlined.code),
                enclosure: function.tag(),
                machine: (&raw mut (*child).machine).cast(),
                compiled: parent.compiled,
                layout,
                parked: parent.parked,
                children: parent.children,
                resume_pc: 0,
                root_top: parent.root_top,
                stepper: Stepper::new(),
            },
            machine: Machine {
                function,
                code: inlined.code,
                header,
                base,
                mark: heap().handle_mark(),
                loops: &mut [],
            },
            inlined,
            function_srcref: values::get_attr(function, values::symbols().srcref),
            srcref: values::nil(),
        });
        Some(function)
    }
}

/// Clears the window of the inlined call `activation` ran without a frame, for a window too
/// large for compiled code to clear inline ([`CLEARED_INLINE`]): the call's registers and slot
/// root cells hold `NULL` again, so that a collection frees what the call left in them, as it
/// does for a frame tier 0 returned from. Nothing was bound in the frame's slots, so there is
/// nothing to release.
pub unsafe extern "C" fn jit_clear_window(activation: *mut Activation) {
    unsafe {
        let child = &*activation.cast::<Child>();
        let layout = &*child.activation.layout;
        std::slice::from_raw_parts_mut(
            child.activation.registers,
            layout.registers + layout.slots.len(),
        )
        .fill(values::nil());
    }
}

/// Loads the variable of `slot` for an inlined call that runs without a frame and does not
/// bind the variable, from the closure's environment and without running any code. A promise
/// must already be forced, and an active binding or a missing variable declines with
/// [`outcome::LOAD_FAILED`], which hands the load to tier 0. Otherwise the result is the
/// slot's status.
pub unsafe extern "C" fn jit_load_free(activation: *mut Activation, slot: i64) -> i32 {
    unsafe {
        let activation = &*activation;
        let layout = layout(activation);
        let (Some(&symbol), Some(hint)) = (
            layout.slots.get(slot as usize),
            layout.hints.get(slot as usize),
        ) else {
            return outcome::LOAD_FAILED;
        };
        let heap = heap();
        let empty = heap.known().empty_env;
        let mut env = activation.enclosure;
        let mut position = hint.get();
        while env != empty {
            if let Some(cell) = heap.frame_cell_hinted(env, symbol, &mut position) {
                hint.set(position);
                if cell.has_gp(gp::ACTIVE_BINDING) {
                    return outcome::LOAD_FAILED;
                }
                let mut value = cell.cdr();
                if value.kind() == SexpType::Promise {
                    value = value.car();
                }
                if value == values::missing_arg() || value == values::unbound() {
                    return outcome::LOAD_FAILED;
                }
                *slot_cell(activation, slot) = value;
                return status::RUNTIME_ENCLOSING as i32;
            }
            env = env.enclosure();
        }
        outcome::LOAD_FAILED
    }
}

/// Loads the formal of `slot` for the inlined call `callee` from the formal `symbol` of
/// `caller`, the root activation, which passed the variable without reading it
/// ([`crate::analysis::Passing::Variable`]). A `forcing` load is the callee's own read of the
/// formal, which is where a promise of the argument would be forced, so a promise the variable
/// holds is forced when that runs no code ([`eval::settled_without_code`]). A load that is not
/// `forcing` takes a promise only once it is forced. Any other promise, an active binding or
/// a missing variable declines with [`outcome::LOAD_FAILED`], which hands the load to tier 0
/// and counts against the caller's code.
pub unsafe extern "C" fn jit_load_passed(
    caller: *mut Activation,
    symbol: *mut Header,
    callee: *mut Activation,
    slot: i64,
    forcing: i64,
) -> i32 {
    unsafe {
        checked(caller, outcome::LOAD_FAILED, |caller| {
            let Some(symbol) = Sexp::from_raw(symbol) else {
                return outcome::LOAD_FAILED;
            };
            let value = match passed_binding(caller, symbol) {
                Some(value) if value.kind() == SexpType::Promise => {
                    if forcing != 0 && value.car() == values::unbound() {
                        match scoped(|| eval::settled_without_code(value)) {
                            Ok(settled) => settled,
                            Err(unwind) => {
                                park(caller, unwind);
                                return outcome::LOAD_FAILED;
                            }
                        }
                    } else {
                        Some(value.car())
                    }
                }
                held => held,
            };
            match value {
                Some(value) if value != values::missing_arg() && value != values::unbound() => {
                    *slot_cell(&*callee, slot) = value;
                    status::RUNTIME_ENCLOSING as i32
                }
                _ => {
                    count_declined_load(compiled(caller));
                    outcome::LOAD_FAILED
                }
            }
        })
    }
}

/// Whether anything may read the activation's frame after its body returns, as a flag. A slot
/// frame without an environment dies with the return unless exit code runs on its slots or
/// promises and closures made over it are still pending, which are the cases tier 0 finishes
/// a frame for ([`interp::eval`]'s `finish_frame`). Compiled code asks at a return and writes
/// its dirty slots back only then.
pub unsafe extern "C" fn jit_frame_read_later(activation: *mut Activation) -> i32 {
    unsafe {
        let machine = machine(&*activation);
        let dies = machine.env() == values::nil()
            && interp::interp().frames.last().is_some_and(|frame| {
                frame.slots as usize == machine.header
                    && frame.env.raw() == values::nil()
                    && frame.pending.raw() == values::nil()
                    && frame.on_exit.is_empty()
            });
        i32::from(!dies)
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
        // A write into a slot frame that collects nothing cannot fail, so it needs neither the
        // panic guard nor a handle scope, which cost more than the store.
        if (*(*activation).parked).panic.is_none() {
            if let Some(position) = overwrite_slot(&*activation, slot as usize, kind, bits) {
                if position < 64
                    && let Some(frame) = interp::interp().frames.last_mut()
                {
                    frame.missing &= !(1 << position);
                }
                return 1;
            }
            let machine = machine(&*activation);
            if let Some(frame) = frame_slot(&*activation, machine, slot) {
                if kind == representation::BOXED {
                    let value = *slot_cell(&*activation, slot);
                    machine.hand_over_slot(frame, value);
                    return 1;
                }
                if let Some(result) = flush_scalar(activation, frame, slot, kind, bits) {
                    return result;
                }
            }
        }
        flush_guarded(activation, slot, kind, bits)
    }
}

/// Writes the unboxed scalar `bits` of the [`representation`] `kind` back to `slot` of a
/// compiled body's own slot frame by storing it into the box the variable holds, which a loop
/// that writes the same variables back on every iteration finds there. The box takes the
/// element when it is a plain scalar of the kind that only the binding references, which is
/// what `bytecode::Machine::overwrite` tests. Returns the slot's position in the frame, whose
/// missing bit the caller clears as an assignment does, or `None` when the write needs
/// [`jit_flush`]'s other paths.
///
/// An inlined call's variables take those paths: its window is no frame of the interpreter's.
#[inline(always)]
unsafe fn overwrite_slot(
    activation: &Activation,
    slot: usize,
    kind: i32,
    bits: i64,
) -> Option<usize> {
    unsafe {
        let kind = match kind {
            representation::INTEGER => SexpType::Integer,
            representation::LOGICAL => SexpType::Logical,
            representation::DOUBLE => SexpType::Real,
            _ => return None,
        };
        let layout = layout(activation);
        if layout.inlined.is_some() {
            return None;
        }
        let position = (*layout.frame.get(slot)?)?;
        let nil = values::nil();
        if *activation.header != nil {
            return None;
        }
        let current = *activation.header.add(1 + position);
        if current.kind() != kind
            || current.is_altrep()
            || current.is_array()
            || current.refcount() != 1
            || current.length() != 1
            || current.attrib() != nil
        {
            return None;
        }
        if kind == SexpType::Real {
            current
                .data()
                .cast::<f64>()
                .write(f64::from_bits(bits as u64));
        } else {
            current.data().cast::<i32>().write(bits as i32);
        }
        *slot_cell(activation, slot as i64) = current;
        Some(position)
    }
}

/// [`jit_flush`] of an unboxed scalar into the slot `frame` of a slot frame: into the box the
/// variable holds when nothing else references it, and otherwise into a new box when the heap
/// has one to give without collecting. `None` leaves the write to the guarded path.
#[inline]
unsafe fn flush_scalar(
    activation: *mut Activation,
    frame: *mut Sexp,
    slot: i64,
    kind: i32,
    bits: i64,
) -> Option<i32> {
    unsafe {
        let (kind, element, payload) = match kind {
            representation::INTEGER => (
                SexpType::Integer,
                f64::from(bits as i32),
                u64::from(bits as u32),
            ),
            representation::LOGICAL => (
                SexpType::Logical,
                f64::from(bits as i32),
                u64::from(bits as u32),
            ),
            representation::DOUBLE => (SexpType::Real, f64::from_bits(bits as u64), bits as u64),
            _ => return None,
        };
        let machine = machine(&*activation);
        let symbol = layout(&*activation).slots[slot as usize];
        let cell = slot_cell(&*activation, slot);
        if let Some(value) = machine.overwrite(Some(frame), symbol, kind, element) {
            *cell = value;
            return Some(1);
        }
        let value = heap().try_alloc_scalar(kind, [payload, 0])?;
        *cell = value;
        match machine.assign(Some(frame), symbol, value) {
            Ok(()) => Some(1),
            Err(unwind) => {
                park(&mut *activation, unwind);
                Some(0)
            }
        }
    }
}

/// How many words of a write-back record precede its entries: the entry count, and the byte
/// offset of the homes from the frame's statuses.
pub const RECORD_HEADER: usize = 2;

/// A write-back record's entry for `slot`, whose value has the [`representation`] `kind`.
pub fn record_entry(slot: usize, kind: i32) -> u32 {
    (slot as u32) << 3 | kind as u32
}

/// Writes every dirty slot `record` lists back to the environment and marks it clean. A site
/// that writes many slots back passes one record instead of testing and flushing each slot in
/// its own code, which is what made a large loop body's code grow with sites times slots.
/// `statuses` is the frame's status cells, and the homes that follow them hold the values of
/// the slots compiled code keeps unboxed, which the site stored there. Returns 1 on success.
pub unsafe extern "C" fn jit_write_back(
    activation: *mut Activation,
    statuses: *mut i32,
    record: *const u32,
) -> i32 {
    unsafe { i32::from(write_record(activation, statuses, record, false)) }
}

/// [`jit_load`] after writing the dirty slots of `record` back, because the load can force a
/// promise whose code looks at the frame. An unboxed slot stays dirty, as
/// `crate::analysis::State::exposed` says, unless the load hands the activation to tier 0:
/// the exit writes dirty variables back, which would undo what the promise assigned to one.
pub unsafe extern "C" fn jit_load_exposed(
    activation: *mut Activation,
    pc: i64,
    slot: i64,
    statuses: *mut i32,
    record: *const u32,
) -> i32 {
    unsafe {
        if !write_record(activation, statuses, record, true) {
            return outcome::LOAD_FAILED;
        }
        let result = jit_load(activation, pc, slot);
        settle_record(statuses, record, result);
        result
    }
}

/// [`jit_load_slot`] after writing the dirty slots of `record` back, as [`jit_load_exposed`].
pub unsafe extern "C" fn jit_load_slot_exposed(
    activation: *mut Activation,
    slot: i64,
    statuses: *mut i32,
    record: *const u32,
) -> i32 {
    unsafe {
        if !write_record(activation, statuses, record, true) {
            return outcome::LOAD_FAILED;
        }
        let result = jit_load_slot(activation, slot);
        settle_record(statuses, record, result);
        result
    }
}

/// Writes the dirty slots of `record` back. A written slot becomes clean, except an unboxed
/// one when `exposing`. Returns whether every write succeeded; a failure is parked.
unsafe fn write_record(
    activation: *mut Activation,
    statuses: *mut i32,
    record: *const u32,
    exposing: bool,
) -> bool {
    unsafe {
        let homes = statuses.cast::<u8>().add(*record.add(1) as usize);
        let unguarded = (*(*activation).parked).panic.is_none();
        // The positions of the slots written in place, whose missing bits one store clears
        // after the last of them.
        let mut written = 0u64;
        let mut complete = true;
        for index in 0..*record as usize {
            let entry = *record.add(RECORD_HEADER + index);
            let slot = (entry >> 3) as usize;
            let status = statuses.add(slot);
            if i64::from(*status) != status::RUNTIME_DIRTY {
                continue;
            }
            let home = homes.add(16 * slot);
            let (kind, bits) = match (entry & 7) as i32 {
                representation::BOXED => (representation::BOXED, 0),
                representation::DOUBLE => (representation::DOUBLE, home.cast::<i64>().read()),
                representation::NUM => {
                    let double = home.cast::<f64>().read();
                    if home.add(8).cast::<i32>().read() == 0 {
                        (representation::DOUBLE, double.to_bits() as i64)
                    } else if double.is_nan() {
                        (representation::INTEGER, i64::from(NA_INTEGER))
                    } else {
                        (representation::INTEGER, i64::from(double as i32))
                    }
                }
                kind => (kind, i64::from(home.cast::<i32>().read())),
            };
            if unguarded && let Some(position) = overwrite_slot(&*activation, slot, kind, bits) {
                if position < 64 {
                    written |= 1 << position;
                }
            } else if jit_flush(activation, slot as i64, kind, bits) == 0 {
                complete = false;
                break;
            }
            if kind == representation::BOXED || !exposing {
                *status = status::RUNTIME_LOCAL as i32;
            }
        }
        if written != 0
            && let Some(frame) = interp::interp().frames.last_mut()
        {
            frame.missing &= !written;
        }
        complete
    }
}

/// Marks the unboxed slots an exposing [`write_record`] left dirty as clean when the load's
/// `result` hands the activation to tier 0.
unsafe fn settle_record(statuses: *mut i32, record: *const u32, result: i32) {
    unsafe {
        if result != outcome::LOAD_FAILED && result != outcome::LOAD_INVALIDATED {
            return;
        }
        for index in 0..*record as usize {
            let entry = *record.add(RECORD_HEADER + index);
            let status = statuses.add((entry >> 3) as usize);
            if (entry & 7) as i32 != representation::BOXED
                && i64::from(*status) == status::RUNTIME_DIRTY
            {
                *status = status::RUNTIME_LOCAL as i32;
            }
        }
    }
}

/// [`jit_flush`] for every write-back but a boxed value into a slot frame. Kept out of line so
/// that the store's path saves no registers for it.
#[inline(never)]
unsafe fn flush_guarded(activation: *mut Activation, slot: i64, kind: i32, bits: i64) -> i32 {
    unsafe {
        guarded(activation, 0, |activation| {
            let machine = machine(activation);
            let symbol = layout(activation).slots[slot as usize];
            let cell = slot_cell(activation, slot);
            let frame = frame_slot(activation, machine, slot);
            let element = match kind {
                representation::INTEGER => Some((SexpType::Integer, f64::from(bits as i32))),
                representation::LOGICAL => Some((SexpType::Logical, f64::from(bits as i32))),
                representation::DOUBLE => Some((SexpType::Real, f64::from_bits(bits as u64))),
                _ => None,
            };
            // A loop writes the same variables back on every iteration, so reusing the box the
            // variable holds saves an allocation each time.
            if let Some((kind, element)) = element
                && let Some(value) = machine.overwrite(frame, symbol, kind, element)
            {
                *cell = value;
                return 1;
            }
            let result = scoped(|| -> R<()> {
                let value = match kind {
                    representation::INTEGER => values::scalar_integer(bits as i32)?,
                    representation::DOUBLE => values::scalar_real(f64::from_bits(bits as u64))?,
                    representation::LOGICAL => values::scalar_logical(bits as i32)?,
                    _ => *cell,
                };
                *cell = value;
                machine.assign(frame, symbol, value)?;
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
            DEOPTIMIZATIONS.with(|count| count.set(count.get() + 1));
            if crate::logging() {
                let op = values::ints(machine.code.car())
                    .get(pc as usize)
                    .and_then(|&word| bytecode::op_of(word as u32));
                eprintln!(
                    "jit: deopt at pc {pc} ({op:?}, reason {why}) in {}",
                    crate::describe(machine.code)
                );
            }
            if why == reason::SPECULATION && (*activation.parked).pending.is_none() {
                invalidate(machine.code);
            }
            for index in 0..compiled.loops {
                let words = activation.loops.add(LOOP_WORDS * index);
                if let Some(state) = machine.loops.get_mut(index) {
                    *state = LoopState::vector(
                        (*words).max(0) as usize,
                        (*words.add(1)).max(0) as usize,
                        f64::from_bits(*words.add(2) as u64),
                        f64::from_bits(*words.add(3) as u64),
                    );
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

/// `vector[[position]] <- value` on a plain list nothing else references, in place or appended
/// within the list's capacity as tier 0 appends. Returns 0, having changed nothing, for anything
/// else, which tier 0 then runs: a `NULL` value, which deletes the element, a list that must
/// grow or be copied first, or one with attributes, which may be names to extend.
pub unsafe extern "C" fn jit_store_list_element(
    vector: *mut Header,
    position: i64,
    value: *mut Header,
) -> i32 {
    unsafe {
        let (Some(vector), Some(value)) = (Sexp::from_raw(vector), Sexp::from_raw(value)) else {
            return 0;
        };
        // Storing a list into itself must copy it first (`x[[2]] <- x`), which tier 0 does.
        if vector.kind() != SexpType::List
            || value == values::nil()
            || value == vector
            || vector.attrib() != values::nil()
            || vector.is_altrep()
            || vector.is_array()
            || values::maybe_shared(vector)
        {
            return 0;
        }
        let length = vector.length();
        let Ok(offset) = usize::try_from(position - 1) else {
            return 0;
        };
        if position - 1 == length as i64 && heap().resize_vector(vector, length + 1).is_err() {
            return 0;
        }
        if offset >= vector.length() as usize {
            return 0;
        }
        values::set_elt(vector, offset, value);
        1
    }
}

/// The words each `for` loop keeps in [`Activation::loops`]: the index, the count, and the
/// first element and increment of a compact sequence as double bits (an increment of 0 when
/// the loop reads the sequence's storage), as in tier 0's [`LoopState`].
pub const LOOP_WORDS: usize = 4;

/// Prepares a `for` loop sequence as tier 0's `ForInit` does: a factor becomes its labels, a
/// compact sequence is returned as it is, with its first element and increment stored in loop
/// `index`'s words, and any other ALTREP vector is expanded into an ordinary one, with an
/// increment of 0. The sequence's length goes in the loop's count word.
pub unsafe extern "C" fn jit_loop_sequence(
    activation: *mut Activation,
    value: *mut Header,
    index: i64,
) -> *mut Header {
    unsafe {
        let words = (*activation).loops.add(LOOP_WORDS * index as usize);
        boxed(activation, || match Sexp::from_raw(value) {
            Some(mut value) => {
                if values::inherits(value, "factor") {
                    value = coerce::factor_as_character(value)?;
                }
                let steps = values::compact_sequence(value).map(|(_, first, step)| (first, step));
                let (first, step) = steps.unwrap_or((0.0, 0.0));
                let sequence = match steps {
                    Some(_) => value,
                    None => values::materialize(value)?,
                };
                *words.add(1) = values::length(sequence) as i64;
                *words.add(2) = first.to_bits() as i64;
                *words.add(3) = step.to_bits() as i64;
                Ok(sequence)
            }
            None => Ok(values::nil()),
        })
    }
}

/// Element `index` of the sequence a `for` loop of unknown element type steps through, as tier
/// 0's `ForStep` reads it: a list's element as it is, a compact sequence's computed from its
/// first element and increment, and any other atomic vector's as a new length-one vector. Null
/// without an error for a sequence tier 0 steps another way, such as a pairlist, which a loop
/// entered at its header may hold, and that hands the step to tier 0.
pub unsafe extern "C" fn jit_loop_element(
    activation: *mut Activation,
    sequence: *mut Header,
    index: i64,
) -> *mut Header {
    unsafe {
        guarded(activation, std::ptr::null_mut(), |activation| {
            let Some(sequence) = Sexp::from_raw(sequence) else {
                return std::ptr::null_mut();
            };
            let index = index as usize;
            let element = match (sequence.kind(), values::compact_sequence(sequence)) {
                (SexpType::Integer, Some((count, first, step))) if index < count => {
                    scoped(|| values::scalar_integer((first + step * index as f64) as i32))
                }
                (SexpType::Real, Some((count, first, step))) if index < count => {
                    scoped(|| values::scalar_real(first + step * index as f64))
                }
                _ if sequence.is_altrep() || index >= sequence.length() as usize => {
                    return std::ptr::null_mut();
                }
                (SexpType::List | SexpType::Expression, _) => Ok(sequence.element(index)),
                (
                    SexpType::Logical
                    | SexpType::Integer
                    | SexpType::Real
                    | SexpType::Complex
                    | SexpType::String
                    | SexpType::Raw,
                    _,
                ) => scoped(|| subset::vector_element(sequence, index)),
                _ => return std::ptr::null_mut(),
            };
            match element {
                Ok(element) => element.as_ptr(),
                Err(unwind) => {
                    park(activation, unwind);
                    std::ptr::null_mut()
                }
            }
        })
    }
}

/// A plain copy of `value` when it is an attribute-free compact sequence of `kind`, for a load
/// that expects a plain vector, such as a formal declared `integer[]` called with `1:n`, or for
/// an assignment into a variable the body changes. An attribute-free plain vector of `kind`,
/// such as the one `a:a` gives, comes back as it is, and anything else gives null, which fails
/// the caller's guard. The sequence itself stays compact, as GNU R's element reads leave it.
///
/// A compact sequence longer than [`SEQUENCE_COPY_LIMIT`] gives null too: R keeps it compact
/// unless an assignment into it runs, and `seq_len(.Machine$integer.max)` would take 8 GB. For
/// either refusal of an ALTREP vector, the caller's `pc`, unless negative, is marked
/// [`feedback::UNCOPIED_ALTREP`], so the recompile keeps the variable boxed.
pub unsafe extern "C" fn jit_sequence_contents(
    activation: *mut Activation,
    value: *mut Header,
    kind: i64,
    pc: i64,
) -> *mut Header {
    unsafe {
        guarded(activation, std::ptr::null_mut(), |activation| {
            let Some(value) = Sexp::from_raw(value) else {
                return std::ptr::null_mut();
            };
            if value.kind() as i64 != kind || value.attrib() != values::nil() {
                return std::ptr::null_mut();
            }
            if !value.is_altrep() {
                return value.as_ptr();
            }
            if values::compact_sequence(value).is_none()
                || values::length(value) > SEQUENCE_COPY_LIMIT
            {
                let code = machine(&*activation).code;
                if let Ok(pc) = usize::try_from(pc)
                    && let Some(bits) = values::ints_mut(code.cdr().element(FEEDBACK)).get_mut(pc)
                {
                    if crate::logging() {
                        eprintln!(
                            "jit: pc {pc} of {} keeps an ALTREP vector boxed",
                            crate::describe(code)
                        );
                    }
                    *bits |= feedback::UNCOPIED_ALTREP as i32;
                }
                return std::ptr::null_mut();
            }
            match scoped(|| values::read_contents(value)) {
                Ok(contents) => contents.as_ptr(),
                Err(unwind) => {
                    park(activation, unwind);
                    std::ptr::null_mut()
                }
            }
        })
    }
}

/// The most elements [`jit_sequence_contents`] copies, which bounds the memory a speculation
/// adds to what R allocates.
const SEQUENCE_COPY_LIMIT: usize = 1 << 18;

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

/// The entry count behind `Dict.is_empty()` or `Set.is_empty()`, or -1 when tier 0 must run the
/// call.
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

/// `from:to` when it makes integers, or `NULL` for tier 0 to make anything else: doubles,
/// the error for a missing bound, or, after parking it, an exhausted heap's error.
pub unsafe extern "C" fn jit_colon(activation: *mut Activation, from: f64, to: f64) -> *mut Header {
    unsafe {
        checked(activation, std::ptr::null_mut(), |activation| {
            let range = -f64::from(i32::MAX)..=f64::from(i32::MAX);
            if !range.contains(&from) || !range.contains(&to) || from != from.floor() {
                return std::ptr::null_mut();
            }
            match scoped(|| arith::colon(from, to)) {
                Ok(sequence) if sequence.kind() == SexpType::Integer => sequence.as_ptr(),
                Ok(_) => std::ptr::null_mut(),
                Err(unwind) => {
                    park(activation, unwind);
                    std::ptr::null_mut()
                }
            }
        })
    }
}

/// How many closure calls may still nest before R's expression depth limit, which a native
/// function call counts down instead of pushing frames.
///
/// A native call that runs out of budget fails as a whole, and tier 0, running it again, tries
/// the native function at every call nested inside, each of which nests as deep again before
/// it fails. Under a raised limit (`options(expressions = 500000)`) a recursion past the limit
/// therefore did work quadratic in the limit before it reported the error. So the budget also
/// shrinks with the interpreter's own depth, which bounds the nesting of all those attempts
/// along one recursion by [`RETRY_NESTING`] times the logarithm of the depth. At R's default
/// limit of 5000 the depth limit is always the smaller of the two.
pub extern "C" fn jit_depth_budget() -> i64 {
    let state = interp::interp();
    let frames = state.frames.len() as i64;
    (i64::from(state.max_depth) - frames).min(RETRY_NESTING / frames.max(1))
}

const RETRY_NESTING: i64 = 8_000_000;

/// The stack limit a native function called from compiled code compares its frames against
/// ([`typed::native_stack_limit`]).
pub extern "C" fn jit_stack_limit() -> u64 {
    typed::native_stack_limit() as u64
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
    long_double::real_modulo(a, b)
}

/// Whether `%%` on doubles must warn about a total loss of accuracy, which compiled code
/// leaves to tier 0: 1 when it must, else 0.
pub extern "C" fn jit_modulus_loses_accuracy(a: f64, b: f64) -> i32 {
    i32::from(long_double::modulus_loses_accuracy(a, b))
}

/// `%/%` on doubles, as the interpreter computes it.
pub extern "C" fn jit_real_integer_divide(a: f64, b: f64) -> f64 {
    long_double::real_integer_divide(a, b)
}

/// `%%` on integers: `NA` for an `NA` operand or a zero divisor, else the sign of the divisor.
pub extern "C" fn jit_integer_modulo(a: i32, b: i32) -> i32 {
    arith::integer_modulo(a, b)
}

/// `%/%` on integers.
pub extern "C" fn jit_integer_divide(a: i32, b: i32) -> i32 {
    arith::integer_divide(a, b)
}
