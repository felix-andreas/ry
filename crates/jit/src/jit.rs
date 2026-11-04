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

mod analysis;
mod codegen;
mod native;
pub use native::has_native;
mod runtime;

use {
    analysis::Refusal,
    bumpalo::Bump,
    heap::{
        activity::{self, Activity, Counter},
        object::{Header, Sexp, SexpType, gp},
    },
    interp::{
        annotations,
        bytecode::{self, Machine, Op, PROFILE, TIER1, Tier, TierExit, profile},
        heap, interp,
        unwind::{R, error},
        values,
    },
    runtime::{Activation, Compiled, MAX_COLD_REFUSALS, MAX_COMPILES},
    std::{
        cell::{Cell, RefCell},
        ffi::c_void,
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
/// Backward jumps a body takes before a loop compiles, in the default mode, for the same reason.
const BACK_EDGE_THRESHOLD: i32 = 10_000;

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
    MODE.with(|current| current.set(mode));
    bytecode::set_tier(Some(Tier {
        enter,
        entry_threshold: entries,
        back_edge,
        back_edge_threshold: back_edges,
        call: native::call,
    }));
}

unsafe fn enter(machine: &mut Machine<'_>) -> Option<TierExit> {
    unsafe {
        let counters = values::ints(machine.code.cdr().element(PROFILE));
        let entries = counters[profile::ENTRIES];
        let threshold = bytecode::tier().map_or(i32::MAX, |tier| tier.entry_threshold);
        let compiled = if entries >= threshold {
            compiled_for(machine, true, false)?
        } else if entries == 1 && counters[profile::TIER_STATE] == 0 && fully_typed(machine) {
            first_call(machine)?
        } else {
            compiled_for(machine, false, false)?
        };
        run(machine, compiled, 0)
    }
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
                    && reason != analysis::BRIEF
                {
                    counters[profile::TIER_STATE] = -1;
                }
                None
            }
        }
    }
}

unsafe fn back_edge(machine: &mut Machine<'_>, target: usize) -> Option<TierExit> {
    unsafe {
        let compiled = compiled_for(machine, true, true)?;
        if !compiled.osr.contains(&target) {
            return None;
        }
        run(machine, compiled, target)
    }
}

/// The compiled code for the activation, compiling it now when `hot` and it has none.
/// `in_loop` says a backward jump asked: the loop is running in tier 0 right now, so waiting for
/// the code after it to record its types would only keep the loop slow.
unsafe fn compiled_for<'a>(
    machine: &Machine<'_>,
    hot: bool,
    in_loop: bool,
) -> Option<&'a Compiled> {
    unsafe {
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
                let enclosure = machine.env.enclosure();
                if compiled.checked.get().weak_key() != enclosure {
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
            if tier1.external_protected().weak_key() != values::nil() {
                let compiled = &*(tier1.external_address() as *const Compiled);
                // Closures made by one `function` expression share their body but not their
                // enclosure, which the compiled lookups are specialized to.
                return (compiled.enclosure == machine.env.enclosure()).then_some(compiled);
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
            counters[profile::TIER_STATE] = -1;
            return None;
        }
        let patient = !in_loop && runtime::cold_refusals(state) < MAX_COLD_REFUSALS;
        let trigger = if in_loop {
            Trigger::HotLoop
        } else {
            Trigger::HotEntry
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
                if reason == analysis::MOSTLY_TIER0 {
                    counters[profile::TIER_STATE] = state | runtime::LOOPS_ONLY;
                    counters[profile::ENTRIES] = runtime::NEVER_HOT;
                } else if reason == analysis::COLD {
                    // The next attempt waits for as many entries or backward jumps as this one did,
                    // so the body runs long enough to record the types it is missing.
                    counters[profile::TIER_STATE] = state + runtime::COLD_REFUSAL;
                    counters[profile::ENTRIES] = 0;
                    counters[profile::BACK_EDGES] = 0;
                } else {
                    counters[profile::TIER_STATE] = -1;
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// A fully typed closure's first call, before anything shows the body is hot.
    FirstCall,
    /// The body was entered [`ENTRY_THRESHOLD`] times.
    HotEntry,
    /// A loop in the body took [`BACK_EDGE_THRESHOLD`] backward jumps and is running now.
    HotLoop,
}

unsafe fn compile<'a>(
    machine: &Machine<'_>,
    patient: bool,
    trigger: Trigger,
) -> Result<&'a Compiled, Refusal> {
    activity::count(Counter::JitCompiles);
    let _activity = activity::enter(Activity::JitCompile, 0);
    ARENA.with(|arena| match arena.try_borrow_mut() {
        Ok(mut arena) => {
            let compiled = compile_in(&arena, machine, patient, trigger);
            arena.reset();
            compiled
        }
        // Compilation never runs R code, so it does not nest; a fresh arena keeps it correct if
        // that ever changes.
        Err(_) => compile_in(&Bump::new(), machine, patient, trigger),
    })
}

unsafe fn compile_in<'a>(
    arena: &Bump,
    machine: &Machine<'_>,
    patient: bool,
    trigger: Trigger,
) -> Result<&'a Compiled, Refusal> {
    unsafe {
        let enclosure = machine.env.enclosure();
        let started = Instant::now();
        let plan = analysis::plan(
            analysis::Arena(arena),
            machine.code,
            machine.function.car(),
            enclosure,
            patient,
        )?;
        let planned = started.elapsed();
        // Forced compilation keeps such bodies, so the differential tests cover the code that
        // glues tier 0's steps together.
        if MODE.with(Cell::get) != Mode::Force {
            if !plan.runs_natively() {
                return Err(Refusal(analysis::NOTHING_NATIVE));
            }
            if trigger != Trigger::HotLoop && plan.native_actions() < plan.tier0_actions() {
                return Err(Refusal(analysis::MOSTLY_TIER0));
            }
            if trigger == Trigger::FirstCall && !plan.may_run_long() {
                return Err(Refusal(analysis::BRIEF));
            }
        }
        let (entry, exits, size) = codegen::compile(&plan, machine.code)?;
        let generated = started.elapsed() - planned;
        if logging() {
            // The instructions left to tier 0 are where compiled code is slow.
            // A builtin call lists its argument types, which say why it was not an intrinsic.
            let steps: Vec<String> = plan
                .insts
                .iter()
                .zip(&plan.actions)
                .enumerate()
                .filter(|(_, (_, action))| matches!(action, analysis::Action::Step { .. }))
                .map(|(position, (inst, _))| {
                    let arguments = match (inst.op, plan.before(position)) {
                        (Op::CallBuiltin, Some(before)) => {
                            let first = inst.operand(3);
                            let types: Vec<String> = (first..first + inst.operand(4))
                                .map(|register| format!("{:?}", before.registers[register]))
                                .collect();
                            format!("({})", types.join(", "))
                        }
                        _ => String::new(),
                    };
                    format!("{:?}@{}{arguments}", inst.op, inst.pc)
                })
                .collect();
            eprintln!(
                "jit: compiled {} in {} us (analysis {} us, codegen {} us; {size} bytes; {} speculations; {} native, {} loops; tier 0 runs: {})",
                describe(machine.code),
                (planned + generated).as_micros(),
                planned.as_micros(),
                generated.as_micros(),
                plan.speculations.len(),
                plan.native_actions(),
                plan.loop_headers.len(),
                if steps.is_empty() {
                    "nothing".to_string()
                } else {
                    steps.join(", ")
                }
            );
        }
        let osr = plan
            .loop_headers
            .iter()
            .map(|&block| plan.insts[plan.blocks[block].start].pc)
            .collect();
        let rooted: Box<[bool]> = plan
            .speculations
            .iter()
            .map(|speculation| speculation.env == enclosure)
            .collect();
        let portable = plan
            .speculations
            .iter()
            .zip(&rooted)
            .all(|(speculation, &rooted)| {
                (rooted || long_lived(speculation.env))
                    && owner(speculation).is_some_and(|env| long_lived(env))
            });
        let compiled = Box::new(Compiled {
            entry,
            enclosure,
            portable,
            checked: Cell::new(values::nil()),
            rooted,
            registers: plan.registers,
            slots: plan.slots.to_vec(),
            loops: plan.loops,
            speculations: plan.speculations.to_vec(),
            // No epoch or stamp matches, so the first run checks the compiling activation's own
            // frame.
            epoch: Cell::new(u64::MAX),
            stamps: plan
                .speculations
                .iter()
                .map(|_| Cell::new(u64::MAX))
                .collect(),
            osr,
            exits,
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
        for speculation in &plan.speculations {
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
        Ok(&*(pointer.external_address() as *const Compiled))
    }
}

/// Whether `env` lives as long as the session normally does: the global and base environments,
/// a namespace, a namespace's imports, or an environment on the search path.
unsafe fn long_lived(env: Sexp) -> bool {
    unsafe {
        let known = heap().known();
        if env == known.global_env
            || env == known.base_env
            || env.enclosure() == known.base_namespace
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
unsafe fn owner(speculation: &analysis::Speculation) -> Option<Sexp> {
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

/// Runs compiled code for the activation from `entry_pc`. `None` hands the activation to the
/// interpreter unchanged.
unsafe fn run(machine: &mut Machine<'_>, compiled: &Compiled, entry_pc: usize) -> Option<TierExit> {
    unsafe {
        // A loop entry resumes an activation whose frame may have bound a name before the
        // compiler started watching it, which no epoch change records.
        if !runtime::valid(compiled, machine.env, entry_pc != 0) {
            runtime::invalidate(machine.code);
            return None;
        }
        let cells = machine.base + compiled.registers;
        if bytecode::reserve_registers(cells + compiled.slots.len()).is_err() {
            return None;
        }
        for slot in 0..compiled.slots.len() {
            *bytecode::register_address(cells + slot) = values::nil();
        }
        let mut loops = vec![0i64; 2 * compiled.loops];
        for (index, state) in machine.loops.iter().enumerate().take(compiled.loops) {
            loops[2 * index] = state.index as i64;
            loops[2 * index + 1] = state.count as i64;
        }
        let mut activation = Activation {
            registers: bytecode::register_address(machine.base),
            loops: loops.as_mut_ptr(),
            visible: &mut interp().visible,
            entry_pc: entry_pc as i64,
            machine: machine as *mut Machine<'_> as *mut c_void,
            compiled,
            pending: None,
            panic: None,
            resume_pc: 0,
        };
        activity::count(Counter::JitEntries);
        let running = activity::enter(Activity::Compiled, 0);
        let result: *mut Header = (compiled.entry)(&mut activation);
        drop(running);
        if let Some(payload) = activation.panic.take() {
            resume_unwind(payload);
        }
        match result as usize {
            runtime::DECLINED => return None,
            runtime::DEOPTIMIZED => {
                activity::count(Counter::JitDeoptimizations);
                return Some(TierExit::Resume {
                    pc: activation.resume_pc,
                    pending: activation.pending.take(),
                });
            }
            _ => {}
        }
        Some(TierExit::Finished(match Sexp::from_raw(result) {
            // An error parked by a helper whose failure compiled code does not branch on.
            Some(_) if activation.pending.is_some() => Err(activation
                .pending
                .take()
                .unwrap_or_else(|| error("compiled code failed without an error"))),
            Some(value) => Ok(value),
            None => Err(activation
                .pending
                .take()
                .unwrap_or_else(|| error("compiled code failed without an error"))),
        }))
    }
}
