//! ry's optimizing tier: a JIT that compiles hot tier-0 bytecode to machine code with Cranelift.
//!
//! Tier 0 (the bytecode interpreter in `interp`) counts how often a body starts and how often its
//! loops jump back, and calls this crate through [`bytecode::Tier`] once a body is hot. The JIT
//! builds a typed plan from the bytecode and its type feedback ([`analysis`]), translates it to
//! machine code ([`codegen`]) and runs it on tier 0's own register window, so on any failed
//! speculation it can hand the activation back to the interpreter mid-body ([`runtime`]).
//!
//! `RY_JIT` selects the mode: `on` (the default) compiles hot code, `off` never compiles, and
//! `force` compiles every body on its second run and every loop on its first backward jump,
//! which is what the differential tests use to run everything through compiled code.

#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes are live objects of this heap.
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

mod analysis;
mod codegen;
mod native;
pub use native::has_native;
mod runtime;

use {
    analysis::{Plan, Refusal, Speculation, Work},
    bumpalo::Bump,
    codegen::Generated,
    heap::{
        activity::{self, Activity, Counter},
        object::{Header, Sexp, SexpType, gp},
    },
    interp::{
        annotations,
        bytecode::{self, FEEDBACK, Machine, Op, PROFILE, Stepper, TIER1, Tier, TierExit, profile},
        heap, interp,
        unwind::{R, error},
        values,
    },
    runtime::{Activation, Child, Compiled, MAX_COLD_REFUSALS, MAX_COMPILES, Parked},
    std::{
        cell::{Cell, RefCell},
        ffi::c_void,
        mem::MaybeUninit,
        panic::resume_unwind,
        sync::OnceLock,
        time::Instant,
    },
};

/// How the JIT is configured for the session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Off,
    On,
    Force,
}

impl Mode {
    /// The mode `RY_JIT` asks for.
    pub fn from_environment() -> Mode {
        match std::env::var("RY_JIT").as_deref() {
            Ok("off" | "0" | "false") => Mode::Off,
            Ok("force") => Mode::Force,
            _ => Mode::On,
        }
    }
}

/// Calls a body makes before it compiles, in the default mode. Compiling a body costs about as
/// much as interpreting it a few thousand times, and loading a package runs many bodies (the
/// methods package's, for each `setClass`) only a few dozen times: at 10 calls, compiling them
/// took half the time of a large package's S4 definitions. On a test suite, where most bodies
/// run a few hundred to a few thousand times, compiling at 1000 calls cost 2.7% more
/// instructions than not compiling at all and 3000 calls cost 1.5%; a sweep from 300 to 10,000
/// had its minimum at 3000. Code that is really hot passes 3000 calls within milliseconds.
const ENTRY_THRESHOLD: i32 = 3000;
/// The model that decides whether a loop-free body repays compiling at its entry threshold.
/// Such a body saves a bounded amount per call, so it compiles only when its expected calls
/// times that saving exceed the compilation's cost. Calibrated with callgrind on the benchmark
/// workloads: compiling costs about 2M instructions plus 50k per bytecode word (a 50-word body
/// costs 4.5M, `isTRUE`'s 188 words 11M).
const COMPILE_FIXED: u64 = 2_000_000;
const COMPILE_PER_WORD: u64 = 50_000;
/// What one call of a loop-free body saves in compiled code, in instructions. Entering
/// compiled code from tier 0 costs about [`ENTRY_COST`] more than starting the same body in
/// the dispatch loop: the tier's entry checks, the activation record, the register window of
/// the root cells, and a boxed result. Against that, an instruction compiled code runs
/// natively saves about [`SAVING_PER_NATIVE`], which includes the variable loads and stores
/// around it that stay in machine registers, and a call it inlines into the caller's frame
/// saves about [`SAVING_PER_CALL`]. A body whose saving does not exceed the entry cost never
/// compiles on its own, however hot it is; its calls from compiled code can still be inlined
/// there.
///
/// A closure call inlined with a frame of its own saves about [`SAVING_PER_FRAME`]: tier 0's
/// call setup, its return and the dispatch between them, less the window and the activation
/// record the frame's first start writes. The callee's instructions count as the callee's
/// plan has them.
///
/// An instruction compiled code hands back to tier 0 costs about [`STEP_COST`] more than the
/// same instruction in the dispatch loop: the helper call with its panic guard, the dirty
/// slots written back before it and the registers read again after it.
///
/// `development/design/generic-tier.md` holds the measurements behind these numbers.
const ENTRY_COST: u64 = 380;
const SAVING_PER_NATIVE: u64 = 130;
const SAVING_PER_CALL: u64 = 1000;
const SAVING_PER_FRAME: u64 = 450;
const STEP_COST: u64 = 180;
/// A body's expected calls are its share of the closure calls the session made over its
/// recording window, times this many calls. The session's call count measures how much
/// interpreted work ran between two calls of the body: it is deterministic and does not move
/// when an optimization makes calls allocate less, which an allocation clock did (slot frames
/// cut testthat's allocation per call and pushed `isTRUE` over the line). The horizon is fixed
/// rather than taken from how long the session has run, because that undercounts callbacks that
/// turn hot early: the functional workload's first callbacks got 13 times the calls such an
/// estimate predicted. Its value sits between the two measured cases: `isTRUE` in a test suite
/// (one call in 63, needs a horizon of at least 600,000) and the functional workload's
/// slowest-rate callbacks, `x^2` and `Reduce`'s (one call in 9, need at most 390,000). Against
/// `1 << 18`, which lost those two callbacks, it saved functional 10.8M instructions.
const HORIZON_CALLS: u64 = 1 << 19;
/// A body with a hot loop is expected to take this many more backward jumps, the horizon of
/// [`HORIZON_CALLS`] for loops, or as many as it has taken if that is more ([`unpaid_loop`]).
/// A body of a few hundred words then compiles at the back-edge threshold when a pass through
/// its loop saves one native instruction, and a body of several thousand words whose loops
/// save about that per pass, such as base R's `rbind.data.frame`, waits for a few million.
const HORIZON_EDGES: u64 = 1 << 19;
/// Backward jumps a body takes before a loop compiles, in the default mode, for the same reason.
const BACK_EDGE_THRESHOLD: i32 = 10_000;
/// Tier 1 compiles a body in time that grows with its length, about 10 us per instruction word,
/// while a hot body's saving per entry depends on how much of it runs, not on how long it is.
/// So in the default mode a body longer than this many words, on turning hot, waits for one more
/// threshold of entries or backward jumps per this many words before it compiles, the way V8
/// scales its tier-up budget by bytecode length. Base R's `rbind` body (9731 words, 200 ms to
/// compile) then compiles only after about 120,000 calls; a 250-word body does not wait. The wait
/// doubles with each compile the body already had, because a body that deoptimized and recompiled
/// (base `Reduce`, called with a new callback type from each site) pays the whole compile again.
const SIZE_WAIT_WORDS: usize = 250;

thread_local! {
    static MODE: Cell<Mode> = const { Cell::new(Mode::On) };
    /// The arena every compilation's plan lives in. It is reset after each compilation, which
    /// keeps its largest chunk, so a compilation usually allocates nothing from the system.
    static ARENA: RefCell<Bump> = RefCell::new(Bump::new());
}

/// Installs the JIT in the current interpreter, in the mode `RY_JIT` asks for.
pub fn install() {
    install_mode(Mode::from_environment());
}

/// How many closure calls the compiled code of `function`'s body runs as inlined frames, or
/// zero when the body has no compiled code.
pub unsafe fn inlined_frames(function: Sexp) -> usize {
    unsafe {
        let code = function.cdr();
        if function.kind() != SexpType::Closure || code.kind() != SexpType::Bytecode {
            return 0;
        }
        let tier1 = code.cdr().element(TIER1);
        if tier1 == values::nil() {
            return 0;
        }
        let address = tier1.external_address() as *const Compiled;
        address
            .as_ref()
            .map_or(0, |compiled| compiled.frames.len().saturating_sub(1))
    }
}

/// How many times compiled code has left for tier 0 on this thread.
pub fn deoptimizations() -> u64 {
    runtime::DEOPTIMIZATIONS.with(Cell::get)
}

pub fn install_mode(mode: Mode) {
    let (entries, back_edges) = match mode {
        Mode::Off => {
            bytecode::set_tier(None);
            return;
        }
        Mode::On => (ENTRY_THRESHOLD, BACK_EDGE_THRESHOLD),
        // Compiling on the second run rather than the first gives the compiled code the type
        // feedback of one interpreted run, so forced compilation exercises the specialized paths.
        Mode::Force => (2, 1),
    };
    let (entries, back_edges) = thresholds_override().unwrap_or((entries, back_edges));
    MODE.with(|current| current.set(mode));
    bytecode::set_tier(Some(Tier {
        enter,
        entry_threshold: entries,
        worth_recording,
        back_edge,
        back_edge_threshold: back_edges,
        call: native::call,
        ahead: typed::ahead::compile,
    }));
}

/// `RY_JIT_THRESHOLDS=ENTRIES,BACK_EDGES` replaces the mode's thresholds, for sweeping them; `0`
/// stands for never.
fn thresholds_override() -> Option<(i32, i32)> {
    let text = std::env::var("RY_JIT_THRESHOLDS").ok()?;
    let mut parts = text
        .split(',')
        .map(|part| match part.trim().parse::<i32>() {
            Ok(0) => Some(bytecode::NEVER),
            Ok(value) => Some(value),
            Err(_) => None,
        });
    Some((parts.next()??, parts.next()??))
}

unsafe fn enter(machine: &mut Machine<'_>) -> Option<TierExit> {
    unsafe {
        // Almost every entry finds the code it compiled earlier, so that path makes one
        // call, into the code.
        let compiled = match specialized(machine) {
            Some(compiled) => compiled,
            None => compile_at_entry(machine)?,
        };
        run(machine, compiled, 0)
    }
}

/// The code for an entry of a body that has none it can enter, compiled if the body is due.
#[inline(never)]
unsafe fn compile_at_entry<'a>(machine: &mut Machine<'_>) -> Option<&'a Compiled> {
    unsafe {
        let counters = values::ints(machine.code.cdr().element(PROFILE));
        let entries = counters[profile::ENTRIES];
        let threshold = bytecode::tier().map_or(i32::MAX, |tier| tier.entry_threshold);
        if entries >= threshold {
            compiled_for(machine, true, None)
        } else if entries == 1 && counters[profile::TIER_STATE] == 0 && fully_typed(machine) {
            first_call(machine)
        } else {
            compiled_for(machine, false, None)
        }
    }
}

/// The code compiled for the activation's own enclosure, or portable code last proven for it,
/// which almost every entry of a compiled body finds: [`compiled_for`]'s answer without the
/// cases that change the body's state.
#[inline(always)]
unsafe fn specialized<'a>(machine: &Machine<'_>) -> Option<&'a Compiled> {
    unsafe {
        let constants = machine.code.cdr();
        let tier1 = constants.element(TIER1);
        if tier1 == values::nil() {
            return None;
        }
        let address = tier1.external_address() as *const Compiled;
        if address.is_null() {
            return None;
        }
        let enclosure = machine.enclosure();
        let entered = if (*address).portable {
            (*address).checked.get().weak_key() == enclosure
        } else {
            // A dead enclosure's address may be reused by the live one, so the weak key decides.
            (*address).enclosure == enclosure
                && tier1.external_protected().weak_key() != values::nil()
        };
        if !entered {
            return None;
        }
        let state = values::ints(constants.element(PROFILE))[profile::TIER_STATE];
        if state >= 0 && state & runtime::LOOPS_ONLY != 0 {
            return None;
        }
        Some(&*address)
    }
}

/// Refuses a body for good, before it records, when no profile could give its compiled code
/// anything to run natively. Forced compilation keeps such bodies, as [`compile_in`] does.
unsafe fn worth_recording(machine: &Machine<'_>) -> bool {
    unsafe {
        let profile = values::ints_mut(machine.code.cdr().element(PROFILE));
        profile[profile::WINDOW] = clock();
        if MODE.with(Cell::get) == Mode::Force || analysis::shape(machine.code).may_run_natively() {
            return true;
        }
        if logging() {
            eprintln!(
                "jit: not recording {}: {}",
                describe(machine.code),
                analysis::NEVER_NATIVE
            );
        }
        profile[profile::TIER_STATE] = runtime::refused(profile[profile::TIER_STATE]);
        false
    }
}

/// What running `work` once in compiled code is expected to save over tier 0, by the model
/// at [`ENTRY_COST`]: zero when handing instructions back costs more than the native work
/// saves.
fn saving(work: Work) -> u64 {
    (SAVING_PER_NATIVE * work.native as u64
        + SAVING_PER_CALL * work.calls as u64
        + SAVING_PER_FRAME * work.frames as u64)
        .saturating_sub(STEP_COST * work.steps as u64)
}

/// What one call of a body is expected to save when tier 0 enters its compiled code: zero when
/// entering costs more than the body's work saves.
fn saving_per_call(work: Work) -> u64 {
    saving(work).saturating_sub(ENTRY_COST)
}

/// Why a loop-free body that just turned hot does not compile for the `work` its compiled code
/// would do, by the model at [`COMPILE_FIXED`], or `None` when it is expected to repay its
/// compilation. A body that saves too little per call to repay it even if every call of the
/// session were its own is [`analysis::SMALL`], for good; one that is only called too rarely
/// is [`analysis::RARE`], and is asked again later. A body that compiled before passed the
/// second test once already.
unsafe fn unpaid(machine: &Machine<'_>, work: Work) -> Option<&'static str> {
    unsafe {
        let per_call = saving_per_call(work);
        let words = values::ints(machine.code.car()).len() as u64;
        let cost = COMPILE_FIXED + COMPILE_PER_WORD * words;
        if HORIZON_CALLS * per_call < cost {
            return Some(analysis::SMALL);
        }
        let profile = values::ints(machine.code.cdr().element(PROFILE));
        if runtime::compiles(profile[profile::TIER_STATE]) > 0 {
            return None;
        }
        let threshold = bytecode::tier().map_or(0, |tier| tier.entry_threshold);
        let window_entries =
            u64::try_from(threshold - bytecode::recording_start(threshold)).unwrap_or(0);
        let window_calls = u64::from(clock().wrapping_sub(profile[profile::WINDOW]) as u32).max(1);
        let calls = window_entries * HORIZON_CALLS / window_calls;
        let saving = calls * per_call;
        if logging() {
            eprintln!(
                "jit: {} expects {calls} calls ({window_entries} of {window_calls} calls) saving {saving} against a compile of {cost}",
                describe(machine.code)
            );
        }
        (saving < cost).then_some(analysis::RARE)
    }
}

/// Why a body does not compile for the hot loop whose one pass does `hot`, or `None` when the
/// loop is expected to repay the compilation: the backward jumps the body is expected to take
/// ([`HORIZON_EDGES`]) must save more than the compile costs. Past the horizon the test is
/// whether the jumps already taken would have repaid it, so the compile never costs more than
/// tier 0 has lost by then. A refusal is [`analysis::UNPAID_LOOP`] and sets the wait: the loop
/// asks again when its body has taken the backward jumps the test needs, and at least as many
/// again as it has taken, so the attempts on a body whose plan keeps changing stay few. A body
/// that compiled before passed the test once already.
unsafe fn unpaid_loop(machine: &Machine<'_>, hot: Work) -> Option<&'static str> {
    unsafe {
        let profile = values::ints_mut(machine.code.cdr().element(PROFILE));
        if runtime::compiles(profile[profile::TIER_STATE]) > 0 {
            return None;
        }
        let words = values::ints(machine.code.car()).len() as u64;
        let cost = COMPILE_FIXED + COMPILE_PER_WORD * words;
        let seen = runtime::back_edges_seen(profile);
        let per_pass = saving(hot);
        let expected = seen.max(HORIZON_EDGES);
        if logging() {
            eprintln!(
                "jit: {} expects {expected} backward jumps ({seen} taken) saving {per_pass} each against a compile of {cost}",
                describe(machine.code)
            );
        }
        if expected.saturating_mul(per_pass) >= cost {
            return None;
        }
        // A loop that saves nothing itself waits for the body's other loops to double the count.
        let needed = cost.checked_div(per_pass).map_or(0, |needed| needed + 1);
        let more = needed.saturating_sub(seen).max(seen).max(1);
        let threshold = bytecode::tier().map_or(0, |tier| tier.back_edge_threshold);
        let start = i64::from(threshold) - i64::try_from(more).unwrap_or(i64::MAX);
        runtime::restart_back_edges(
            profile,
            i32::try_from(start.max(i64::from(i32::MIN / 2))).unwrap_or(i32::MIN / 2),
        );
        Some(analysis::UNPAID_LOOP)
    }
}

/// The session's call clock ([`interp::interp::Interp::calls_ever`]), as [`profile::WINDOW`]
/// holds it.
fn clock() -> i32 {
    interp().calls_ever as i32
}

/// Whether every formal of the running closure carries a checked annotation. Such a closure
/// compiles at its first call, because its signature gives the types the profile would
/// otherwise have to collect.
unsafe fn fully_typed(machine: &Machine<'_>) -> bool {
    unsafe {
        machine.function.kind() == SexpType::Closure
            && annotations::declared_formals(
                machine.function.car(),
                machine.code.cdr().element(bytecode::SOURCE),
            )
            .1
    }
}

/// Compiles a fully typed closure at its first call. A body that still needs a type only
/// running it can show (the result of an untyped call, say) is refused as cold here without
/// counting against it, so it then tiers up like any other body once it is hot.
unsafe fn first_call<'a>(machine: &Machine<'_>) -> Option<&'a Compiled> {
    unsafe {
        let result = compile(machine, true, Trigger::FirstCall);
        let counters = values::ints_mut(machine.code.cdr().element(PROFILE));
        match result {
            Ok(compiled) => {
                if logging() {
                    eprintln!("jit: the compile above ran at the first call");
                }
                counters[profile::TIER_STATE] = 1;
                Some(compiled)
            }
            Err(Refusal(reason)) => {
                if logging() {
                    eprintln!(
                        "jit: not compiling {} at its first call: {reason}",
                        describe(machine.code)
                    );
                }
                // The plan had no profile yet, so a verdict on how much would run natively is not
                // final: only a refusal that no profile can change stops the body for good.
                if reason != analysis::COLD
                    && reason != analysis::NOTHING_NATIVE
                    && reason != analysis::MOSTLY_TIER0
                    && reason != analysis::SMALL
                    && reason != analysis::STEPPING_LOOP
                    && reason != analysis::BRIEF
                {
                    counters[profile::TIER_STATE] = runtime::refused(counters[profile::TIER_STATE]);
                    counters[profile::RECORDING] = 0;
                }
                None
            }
        }
    }
}

unsafe fn back_edge(machine: &mut Machine<'_>, target: usize) -> Option<TierExit> {
    unsafe {
        let compiled = compiled_for(machine, true, Some(target))?;
        if !compiled.osr.contains(&target) {
            return None;
        }
        run(machine, compiled, target)
    }
}

/// The compiled code for the activation, compiling it now when `hot` and it has none.
/// `looping` is the target of the backward jump that asked, if one did: the loop is running in
/// tier 0 right now, so waiting for the code after it to record its types would only keep the
/// loop slow.
unsafe fn compiled_for<'a>(
    machine: &Machine<'_>,
    hot: bool,
    looping: Option<usize>,
) -> Option<&'a Compiled> {
    unsafe {
        let in_loop = looping.is_some();
        let constants = machine.code.cdr();
        // A body that compiled only for its loop would spend each call handing its instructions
        // back to tier 0, so calls keep running in tier 0 and only the loop enters compiled code.
        let state = values::ints(constants.element(PROFILE))[profile::TIER_STATE];
        if !in_loop && state >= 0 && state & runtime::LOOPS_ONLY != 0 {
            return None;
        }
        let tier1 = constants.element(TIER1);
        if tier1 != values::nil() {
            let address = tier1.external_address() as *const Compiled;
            if !address.is_null() && (*address).portable {
                let compiled = &*address;
                let enclosure = machine.enclosure();
                if compiled.checked.get().weak_key() != enclosure
                    && !runtime::sibling_of_checked(compiled, enclosure)
                {
                    // The epoch and stamps were proven for another enclosure's lookups, so the
                    // next run walks every speculation from this one before trusting them.
                    let heap = heap();
                    let weak = heap
                        .alloc_weak_ref(enclosure, values::nil(), values::nil(), 0, false)
                        .ok()?;
                    heap.set_element(tier1.external_protected(), 0, weak);
                    compiled.checked.set(weak);
                    compiled.epoch.set(u64::MAX);
                    for stamp in &compiled.stamps {
                        stamp.set(u64::MAX);
                    }
                }
                return Some(compiled);
            }
            let counters = values::ints_mut(constants.element(PROFILE));
            if tier1.external_protected().weak_key() != values::nil() {
                let compiled = &*(tier1.external_address() as *const Compiled);
                // Closures made by one `function` expression share their body but not their
                // enclosure, which the compiled lookups are specialized to. The first time
                // another enclosure runs the body, the code gives way to a portable compile.
                if compiled.enclosure == machine.enclosure() {
                    return Some(compiled);
                }
                if counters[profile::TIER_STATE] < 0
                    || counters[profile::TIER_STATE] & runtime::SHARED != 0
                {
                    return None;
                }
                counters[profile::TIER_STATE] |= runtime::SHARED;
                runtime::invalidate(machine.code);
                return None;
            }
            if counters[profile::TIER_STATE] >= 0 {
                counters[profile::TIER_STATE] |= runtime::SHARED;
            }
            // The enclosure the code was compiled for died, and no activation can still run the
            // code, because a running one keeps its enclosure reachable. The address is cleared
            // so that a copy of the constants sharing this pointer does not free it twice.
            let address = tier1.external_address() as *mut Compiled;
            if !address.is_null() {
                tier1.set_external_address(std::ptr::null_mut());
                drop(Box::from_raw(address));
            }
            heap().set_element(constants, TIER1, values::nil());
        }
        let counters = values::ints_mut(constants.element(PROFILE));
        let state = counters[profile::TIER_STATE];
        if !hot || state < 0 {
            return None;
        }
        if runtime::compiles(state) >= MAX_COMPILES {
            counters[profile::TIER_STATE] = runtime::refused(counters[profile::TIER_STATE]);
            counters[profile::RECORDING] = 0;
            return None;
        }
        if state & runtime::SIZE_WAITED == 0 && MODE.with(Cell::get) == Mode::On {
            let rounds = (values::ints(machine.code.car()).len() / SIZE_WAIT_WORDS)
                << runtime::compiles(state);
            if rounds > 0 {
                counters[profile::TIER_STATE] = state | runtime::SIZE_WAITED;
                let wait = |threshold: i32| {
                    i32::try_from(rounds)
                        .unwrap_or(i32::MAX)
                        .saturating_mul(threshold)
                        .min(i32::MAX / 2)
                };
                // The counters climb back to the threshold before the tier is asked again.
                // Recording stops for the wait and restarts when a counter nears its threshold
                // again, so the compile still sees fresh feedback without a large body
                // recording through every round of its wait.
                if let Some(tier) = bytecode::tier() {
                    counters[profile::ENTRIES] = counters[profile::ENTRIES]
                        .min(tier.entry_threshold)
                        .saturating_sub(wait(tier.entry_threshold));
                    runtime::restart_back_edges(counters, -wait(tier.back_edge_threshold));
                    counters[profile::RECORDING] = 0;
                }
                return None;
            }
        }
        let patient = !in_loop && runtime::cold_refusals(state) < MAX_COLD_REFUSALS;
        let trigger = match looping {
            Some(target) => Trigger::HotLoop { target },
            None => Trigger::HotEntry,
        };
        let result = compile(machine, patient, trigger);
        // Compiling reads the profile too, so the counters are borrowed again afterwards.
        let counters = values::ints_mut(constants.element(PROFILE));
        match result {
            Ok(compiled) => {
                counters[profile::TIER_STATE] = state + 1;
                Some(compiled)
            }
            Err(Refusal(reason)) => {
                if logging() {
                    eprintln!("jit: not compiling {}: {reason}", describe(machine.code));
                }
                // A body the tier gave up on, or that now compiles only for its loop, stops
                // recording at entry: nothing reads that feedback, and refused bodies are often
                // the hot ones, so recording on every call of them was most of what the tier
                // cost a test suite. A loop starts recording again halfway to its threshold.
                // A body with loops that saves too little per call can still gain in a loop
                // that runs long, so that verdict stops only its calls from asking.
                let small_around_loops = reason == analysis::SMALL
                    && values::length(constants.element(bytecode::LOOPS)) > 0;
                if reason == analysis::MOSTLY_TIER0 || small_around_loops {
                    counters[profile::TIER_STATE] = state | runtime::LOOPS_ONLY;
                    counters[profile::ENTRIES] = runtime::NEVER_HOT;
                    counters[profile::RECORDING] = 0;
                    // The loop restarts recording when its counter reaches half the threshold
                    // again, which a counter already past that point would never do.
                    runtime::restart_back_edges(counters, 0);
                } else if reason == analysis::UNPAID_LOOP {
                    // The refusal set the wait, which reopens the recording window on the way
                    // back to the threshold.
                    counters[profile::RECORDING] = 0;
                } else if reason == analysis::RARE {
                    // Each attempt records a window of feedback, so a body that keeps turning out
                    // rare waits twice as many entries each time, the way doubling bounds the
                    // cost of waiting against the cost of attempts. The window reopens on the way
                    // back to the threshold, so the next attempt measures the rate afresh.
                    let refusals = runtime::rare_refusals(state);
                    let threshold = bytecode::tier().map_or(0, |tier| tier.entry_threshold);
                    let wait = threshold.saturating_mul(1 << refusals.min(12));
                    counters[profile::ENTRIES] = threshold.saturating_sub(wait);
                    counters[profile::RECORDING] = 0;
                    if refusals < 0xf {
                        counters[profile::TIER_STATE] = state + runtime::RARE_REFUSAL;
                    }
                } else if reason == analysis::COLD
                    && (bytecode::tier().is_some_and(|tier| tier.entry_threshold < 2)
                        || values::ints(constants.element(FEEDBACK))
                            .iter()
                            .any(|&bits| bits != 0))
                {
                    // The next attempt waits for as many entries or backward jumps as this one did,
                    // so the body runs long enough to record the types it is missing. A body whose
                    // window recorded nothing at all does not run its instructions (a native
                    // prologue finishes every call), so waiting would only repeat the plan. At a
                    // threshold of one entry the attempt comes before the body has run at all, so
                    // an empty window says nothing there.
                    counters[profile::TIER_STATE] = state + runtime::COLD_REFUSAL;
                    counters[profile::ENTRIES] = 0;
                    runtime::restart_back_edges(counters, 0);
                } else {
                    counters[profile::TIER_STATE] = runtime::refused(counters[profile::TIER_STATE]);
                    counters[profile::RECORDING] = 0;
                }
                None
            }
        }
    }
}

/// Whether `RY_JIT_LOG` asks for compile, refusal and deoptimization lines. Read once, because
/// unwinds through compiled frames deoptimize often enough that an environment lookup each time
/// shows up in test suites.
pub(crate) fn logging() -> bool {
    static LOGGING: OnceLock<bool> = OnceLock::new();
    *LOGGING.get_or_init(|| std::env::var_os("RY_JIT_LOG").is_some())
}

pub(crate) unsafe fn describe(code: Sexp) -> String {
    unsafe {
        let source = code.cdr().element(bytecode::SOURCE);
        interp::deparse::deparse_one_line(source)
            .chars()
            .take(60)
            .collect()
    }
}

/// What asked for a compilation, which decides how much the compiled code must save to pay
/// for itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Trigger {
    /// A fully typed closure's first call, before anything shows the body is hot.
    FirstCall,
    /// The body was entered [`ENTRY_THRESHOLD`] times.
    HotEntry,
    /// A loop in the body took [`BACK_EDGE_THRESHOLD`] backward jumps and is running now.
    /// `target` is where its backward jump goes.
    HotLoop { target: usize },
}

unsafe fn compile<'a>(
    machine: &Machine<'_>,
    patient: bool,
    trigger: Trigger,
) -> Result<&'a Compiled, Refusal> {
    let _activity = activity::enter(Activity::JitCompile, 0);
    let compiled = ARENA.with(|arena| match arena.try_borrow_mut() {
        Ok(mut arena) => {
            let compiled = compile_in(&arena, machine, patient, trigger);
            arena.reset();
            compiled
        }
        // Compilation never runs R code, so it does not nest; a fresh arena keeps it correct if
        // that ever changes.
        Err(_) => compile_in(&Bump::new(), machine, patient, trigger),
    });
    match &compiled {
        Ok(_) => activity::count(Counter::JitCompiles),
        // The reasons are the ones `RY_JIT_LOG` prints, so the summary's table and the log agree.
        Err(Refusal(reason)) => {
            activity::count(Counter::JitRefusals);
            if activity::enabled() {
                activity::count_jit_refusal(activity::label(reason));
            }
        }
    }
    compiled
}

unsafe fn compile_in<'a>(
    arena: &Bump,
    machine: &Machine<'_>,
    patient: bool,
    trigger: Trigger,
) -> Result<&'a Compiled, Refusal> {
    unsafe {
        let enclosure = machine.enclosure();
        let forced = MODE.with(Cell::get) == Mode::Force;
        let shape = analysis::shape(machine.code);
        if !forced && !shape.may_run_natively() {
            return Err(Refusal(analysis::NEVER_NATIVE));
        }
        if !forced
            && trigger == Trigger::HotEntry
            && !shape.loops
            && machine.function.has_gp(gp::NATIVE_ENTRY)
        {
            return Err(Refusal(analysis::NATIVE_ENTRY));
        }
        // Without a closure call, the shape bounds what any plan of the body could save, so
        // it decides before the plan is made. A call may be inlined with a frame, whose callee
        // adds its own work, so only the plan says what such a body saves.
        if !forced
            && trigger == Trigger::HotEntry
            && !shape.loops
            && shape.frames == 0
            && let Some(reason) = unpaid(machine, shape.work())
        {
            return Err(Refusal(reason));
        }
        let started = Instant::now();
        let state = values::ints(machine.code.cdr().element(PROFILE))[profile::TIER_STATE];
        let plan = analysis::plan(
            analysis::Arena(arena),
            machine.code,
            analysis::Frame::Closure {
                formals: machine.function.car(),
                enclosure,
            },
            patient,
            state >= 0 && state & runtime::SHARED != 0,
        )?;
        let planned = started.elapsed();
        // Forced compilation keeps such bodies, so the differential tests cover the code that
        // glues tier 0's steps together.
        // Entering a body with loops at the top or at a loop costs about the same, so what
        // decides between the two is the work around the loops alone.
        let hot = match trigger {
            Trigger::HotLoop { target } => Some(target),
            Trigger::FirstCall | Trigger::HotEntry => None,
        };
        let work = plan.work(values::ints(machine.code.cdr().element(FEEDBACK)), hot);
        if !forced {
            let reason = if !plan.runs_natively() {
                Some(analysis::NOTHING_NATIVE)
            } else if plan.loop_headers.is_empty() {
                (trigger == Trigger::HotEntry)
                    .then(|| unpaid(machine, work.whole))
                    .flatten()
            } else if saving(work.looped) == 0 {
                Some(analysis::STEPPING_LOOP)
            } else if hot.is_some() {
                unpaid_loop(machine, work.hot)
            } else if work.outside.steps > 0 && saving(work.outside) == 0 {
                Some(analysis::MOSTLY_TIER0)
            } else if trigger == Trigger::HotEntry {
                // A call does the work around the loops and the loops' own, at the weight
                // the plan gives them.
                unpaid(
                    machine,
                    Work {
                        native: work.outside.native + work.looped.native,
                        calls: work.outside.calls + work.looped.calls,
                        frames: work.outside.frames + work.looped.frames,
                        steps: work.outside.steps + work.looped.steps,
                    },
                )
            } else {
                None
            };
            if let Some(reason) = reason {
                if logging() {
                    eprintln!(
                        "jit: {} would leave to tier 0: {}",
                        describe(machine.code),
                        tier0_steps(&plan, machine.code)
                    );
                }
                return Err(Refusal(reason));
            }
            if trigger == Trigger::FirstCall && !plan.may_run_long() {
                return Err(Refusal(analysis::BRIEF));
            }
        }
        let Generated {
            entry,
            exits,
            frames,
            size,
        } = codegen::compile(&plan, machine.code)?;
        let generated = started.elapsed() - planned;
        let mut inlined = Vec::new();
        inlined_plans(&plan, &mut inlined);
        // An inlined closure looks its functions up from its own environment, whatever the
        // compiled body's enclosure is.
        let rooted: Box<[bool]> = plan
            .speculations
            .iter()
            .map(|speculation| speculation.env == enclosure)
            .chain(
                inlined
                    .iter()
                    .flat_map(|(_, callee)| callee.speculations.iter().map(|_| false)),
            )
            .collect();
        let speculations: Vec<Speculation> = plan
            .speculations
            .iter()
            .chain(
                inlined
                    .iter()
                    .flat_map(|(_, callee)| callee.speculations.iter()),
            )
            .cloned()
            .collect();
        let portable = speculations
            .iter()
            .zip(&rooted)
            .all(|(speculation, &rooted)| {
                (rooted || long_lived(speculation.env))
                    && owner(speculation).is_some_and(|env| long_lived(env))
            });
        if logging() {
            let steps = tier0_steps(&plan, machine.code);
            eprintln!(
                "jit: compiled {} ({trigger:?}, {} words) in {} us (analysis {} us, codegen {} us; {size} bytes; {} speculations{}; {} native, {} calls, {} steps, {} inlined, {} frames, {} loops; tier 0 runs: {})",
                describe(machine.code),
                values::ints(machine.code.car()).len(),
                (planned + generated).as_micros(),
                planned.as_micros(),
                generated.as_micros(),
                speculations.len(),
                if portable { ", portable" } else { "" },
                work.whole.native,
                work.whole.calls + work.whole.frames,
                work.whole.steps,
                plan.actions
                    .iter()
                    .filter(|action| matches!(action, analysis::Action::Inline { .. }))
                    .count(),
                frames.len() - 1,
                plan.loop_headers.len(),
                steps
            );
        }
        let osr = plan
            .loop_headers
            .iter()
            .map(|&block| plan.insts[plan.blocks[block].start].pc)
            .collect();
        let compiled = Box::new(Compiled {
            entry,
            code: machine.code,
            enclosure,
            portable,
            checked: Cell::new(values::nil()),
            rooted,
            extent: frames.iter().map(runtime::Layout::top).max().unwrap_or(0),
            frames,
            loops: plan.loops,
            // No epoch or stamp matches, so the first run checks the compiling activation's own
            // frame.
            epoch: Cell::new(u64::MAX),
            stamps: speculations.iter().map(|_| Cell::new(u64::MAX)).collect(),
            speculations,
            osr,
            exits,
            declined_frames: Cell::new(0),
            declined_loads: Cell::new(0),
            leaves_since: Cell::new(
                values::ints(machine.code.cdr().element(PROFILE))[profile::ENTRIES],
            ),
            leaves: Cell::new(0),
            retired: Cell::new(false),
        });
        // Everything the machine code and the speculations point to stays reachable from the
        // external pointer while the enclosure lives, so the collector never frees an object
        // compiled code embeds. They hang off a weak reference keyed on the enclosure, because
        // the body outlives any one closure: R6 gives every object's methods the same body, and a
        // strong reference would keep the first object compiled for alive with its finalizer.
        // Portable code embeds nothing that dies with the enclosure, so it holds its objects
        // directly; element 0 is the weak reference `checked` names, set here to the compiling
        // enclosure.
        let mut kept = vec![enclosure];
        kept.extend(plan.slots.iter().copied());
        for (function, callee) in &inlined {
            // The code is kept apart from the closure, whose body can be replaced.
            kept.extend([*function, function.cdr()]);
            kept.extend(callee.slots.iter().copied());
        }
        for speculation in &compiled.speculations {
            kept.extend([speculation.symbol, speculation.cell, speculation.function]);
            speculation.symbol.set_gp_bits(gp::WATCHED, true);
            speculation.cell.set_gp_bits(gp::WATCHED, true);
        }
        let heap = heap();
        let mark = heap.handle_mark();
        let built = (|| -> R<Sexp> {
            let protected = values::alloc(SexpType::List, kept.len())?;
            heap.root(protected);
            for (index, object) in kept.iter().enumerate() {
                heap.set_element(protected, index, *object);
            }
            let weak = if compiled.portable {
                heap.alloc_weak_ref(enclosure, values::nil(), values::nil(), 0, false)
            } else {
                heap.alloc_weak_ref(enclosure, protected, values::nil(), 0, false)
            }
            .map_err(|_| error("the JIT could not allocate its code object"))?;
            heap.root(weak);
            let held = if compiled.portable {
                heap.set_element(protected, 0, weak);
                compiled.checked.set(weak);
                protected
            } else {
                weak
            };
            let address = Box::into_raw(compiled) as *mut c_void;
            heap.alloc_external_ptr(address, values::nil(), held)
                .map_err(|_| error("the JIT could not allocate its code object"))
        })();
        heap.release_handles(mark);
        let pointer = built.map_err(|_| Refusal("out of memory"))?;
        heap.set_element(machine.code.cdr(), TIER1, pointer);
        // Optimized code records nothing, and tier 0 stops too: only a recompile reads the
        // feedback, and discarding the code turns recording back on before that.
        let counters = values::ints_mut(machine.code.cdr().element(PROFILE));
        counters[profile::RECORDING] = 0;
        counters[profile::NEXT] = i32::MIN;
        Ok(&*(pointer.external_address() as *const Compiled))
    }
}

/// The instructions a plan leaves to tier 0, which are where compiled code is slow. Each lists
/// the types of the registers it reads, which say why it has no native form, and a call names
/// the function its source calls.
unsafe fn tier0_steps(plan: &analysis::Plan<'_>, code: Sexp) -> String {
    let steps: Vec<String> = plan
        .insts
        .iter()
        .zip(&plan.actions)
        .enumerate()
        .filter(|(_, (_, action))| matches!(action, analysis::Action::Step { .. }))
        .map(|(position, (inst, _))| {
            let arguments = match plan.before(position) {
                Some(before) => {
                    let types: Vec<String> = analysis::reads(inst)
                        .iter()
                        .filter_map(|&register| before.registers.get(register))
                        .map(|ty| format!("{ty:?}"))
                        .collect();
                    format!("({})", types.join(", "))
                }
                None => String::new(),
            };
            let call = match inst.op {
                Op::CallBuiltin | Op::CallGeneric | Op::DispatchSubject => Some(inst.operand(2)),
                Op::Internal => Some(inst.operand(1)),
                _ => None,
            };
            let head = call
                .map(|constant| unsafe { code.cdr().element(constant).car() })
                .filter(|head| head.kind() == SexpType::Symbol)
                .map(|head| format!(" {}", unsafe { values::symbol_name(head) }))
                .unwrap_or_default();
            format!("{:?}{head}@{}{arguments}", inst.op, inst.pc)
        })
        .collect();
    if steps.is_empty() {
        "nothing".to_string()
    } else {
        steps.join(", ")
    }
}

/// Whether `env` lives as long as the session normally does: the global and base environments,
/// a namespace, a namespace's imports, or an environment on the search path.
pub(crate) unsafe fn long_lived(env: Sexp) -> bool {
    unsafe {
        let known = heap().known();
        // A namespace's imports environment has the base namespace as its parent and a `name`
        // attribute. A call frame of a base function has the same parent but no attributes,
        // and holds a fresh binding of each local function on every call.
        if env == known.global_env
            || env == known.base_env
            || (env.enclosure() == known.base_namespace
                && values::get_attr(env, values::symbols().name) != values::nil())
            || interp::envir::is_namespace(env)
        {
            return true;
        }
        let mut search = known.global_env.enclosure();
        while search != known.empty_env {
            if search == env {
                return true;
            }
            search = search.enclosure();
        }
        false
    }
}

/// The environment whose frame holds the binding a speculation found.
pub(crate) unsafe fn owner(speculation: &analysis::Speculation) -> Option<Sexp> {
    unsafe {
        let heap = heap();
        let empty = heap.known().empty_env;
        let mut env = speculation.env;
        while env != empty {
            if heap.frame_cell(env, speculation.symbol) == Some(speculation.cell) {
                return Some(env);
            }
            env = env.enclosure();
        }
        None
    }
}

/// The closures `plan` inlines as frames ([`analysis::Action::Frame`]), at any depth, with
/// their plans. A closure inlined at several calls appears once per call.
fn inlined_plans<'a>(plan: &'a Plan<'a>, found: &mut Vec<(Sexp, &'a Plan<'a>)>) {
    for action in plan.actions.iter() {
        if let analysis::Action::Frame {
            function, callee, ..
        } = action
        {
            found.push((*function, callee));
            inlined_plans(callee, found);
        }
    }
}

/// Runs compiled code for the activation from `entry_pc`. `None` hands the activation to the
/// interpreter unchanged.
#[inline(always)]
unsafe fn run(machine: &mut Machine<'_>, compiled: &Compiled, entry_pc: usize) -> Option<TierExit> {
    unsafe {
        // A loop entry resumes an activation whose frame may have bound a name before the
        // compiler started watching it, which no epoch change records.
        if !runtime::valid(compiled, machine, entry_pc != 0) {
            runtime::invalidate(machine.code);
            return None;
        }
        let body = compiled.frames.first()?;
        let cells = machine.base + body.registers;
        // The slot root cells, then the windows of the inlined calls, which start as `NULL`
        // like any register. Growing the file clears what it adds, so only registers the
        // file already held need clearing.
        let root_top = cells + body.slots.len();
        let held = bytecode::register_top().clamp(cells, root_top + compiled.extent);
        if bytecode::reserve_registers(root_top + compiled.extent).is_err() {
            return None;
        }
        std::slice::from_raw_parts_mut(bytecode::register_address(cells), held - cells)
            .fill(values::nil());
        // Most compiled bodies have at most a few loops, so their state lives in a stack array.
        // A `SmallVec` here would be built by value and moved, copying its whole inline buffer
        // on every call, which costs a body without loops more than the loop state itself.
        let mut inline = [const { MaybeUninit::<i64>::uninit() }; 4 * runtime::LOOP_WORDS];
        let mut spilled: Vec<i64>;
        let words = runtime::LOOP_WORDS * compiled.loops;
        // Most generic bodies have no loop at all, and those enter without this.
        let loops: *mut i64 = if words == 0 {
            std::ptr::null_mut()
        } else {
            let loops: &mut [i64] = match inline.get_mut(..words) {
                Some(loops) => {
                    loops.fill(MaybeUninit::new(0));
                    std::slice::from_raw_parts_mut(loops.as_mut_ptr().cast(), words)
                }
                None => {
                    spilled = vec![0i64; words];
                    &mut spilled
                }
            };
            for (words, state) in loops
                .as_chunks_mut::<{ runtime::LOOP_WORDS }>()
                .0
                .iter_mut()
                .zip(machine.loops.iter())
            {
                words.copy_from_slice(&[
                    state.index as i64,
                    state.count as i64,
                    state.first.to_bits() as i64,
                    state.step.to_bits() as i64,
                ]);
            }
            loops.as_mut_ptr()
        };
        let mut parked = Parked {
            pending: None,
            panic: None,
        };
        let parked: *mut Parked = &mut parked;
        // An inlined call's activation is written by the call's first start in this entry,
        // which a null `compiled` asks for, so the storage costs a body that inlines nothing
        // only the stack space.
        let mut inline_children = [const { MaybeUninit::<Child>::uninit() }; 4];
        let mut spilled_children: Vec<MaybeUninit<Child>>;
        let count = compiled.frames.len() - 1;
        let children: *mut Child = if count <= inline_children.len() {
            inline_children.as_mut_ptr().cast()
        } else {
            spilled_children = Vec::with_capacity(count);
            spilled_children.as_mut_ptr().cast()
        };
        for index in 0..count {
            (&raw mut (*children.add(index)).activation.compiled).write(std::ptr::null());
        }
        // Tier 0 releases down to `machine.mark`, and the instructions compiled code hands back
        // run on this machine. An inlined call's activation records the stack height at its
        // first start, and every later start expects to find it there, so while the body runs
        // the mark is the height at entry. Whatever lies between the two marks stays rooted until
        // the body returns, which only delays its release.
        let frame_mark = machine.mark;
        machine.mark = heap().handle_mark();
        let mut activation = Activation {
            registers: bytecode::register_address(machine.base),
            loops,
            visible: &mut interp().visible,
            entry_pc: entry_pc as i64,
            header: bytecode::register_address(machine.header),
            hints: bytecode::lookup_hints(machine.code),
            enclosure: machine.enclosure(),
            machine: machine as *mut Machine<'_> as *mut c_void,
            compiled,
            layout: body,
            parked,
            children,
            resume_pc: 0,
            root_top,
            stepper: Stepper::new(),
        };
        activity::count(Counter::JitEntries);
        let running = activity::enter(Activity::Compiled, 0);
        let activation_address: *mut Activation = &mut activation;
        let pending = std::ptr::addr_of!((*parked).pending);
        interp().held_unwinds.push(pending);
        let result: *mut Header = (compiled.entry)(activation_address);
        machine.mark = frame_mark;
        let held = interp().held_unwinds.pop();
        debug_assert_eq!(held, Some(pending), "compiled code entries nest");
        drop(running);
        if let Some(payload) = (*parked).panic.take() {
            resume_unwind(payload);
        }
        let pending = (*parked).pending.take();
        match result as usize {
            runtime::DECLINED => {
                bytecode::release_registers(machine.base + body.registers);
                return None;
            }
            runtime::DEOPTIMIZED => {
                bytecode::release_registers(machine.base + body.registers);
                activity::count(Counter::JitDeoptimizations);
                return Some(TierExit::Resume {
                    pc: activation.resume_pc,
                    pending,
                });
            }
            _ => {}
        }
        Some(TierExit::Finished(
            match (Sexp::from_raw(result), pending) {
                // An error parked by a helper whose failure compiled code does not branch on.
                (_, Some(unwind)) => Err(unwind),
                (Some(value), None) => Ok(value),
                (None, None) => Err(error("compiled code failed without an error")),
            },
        ))
    }
}
