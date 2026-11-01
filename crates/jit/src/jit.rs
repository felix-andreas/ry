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
mod runtime;

use analysis::Refusal;
use heap::object::{Header, Sexp, SexpType};
use interp::{
    bytecode::{self, Machine, PROFILE, TIER1, Tier, TierExit, profile},
    heap, interp,
    unwind::{R, error},
    values,
};
use runtime::{Activation, Compiled, MAX_COMPILES};
use std::{cell::Cell, ffi::c_void, panic::resume_unwind};

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

/// Calls a body makes before it compiles, in the default mode.
const ENTRY_THRESHOLD: i32 = 10;
/// Backward jumps a body takes before a loop compiles, in the default mode.
const BACK_EDGE_THRESHOLD: i32 = 500;

thread_local! {
    static ENTRY_LIMIT: Cell<i32> = const { Cell::new(ENTRY_THRESHOLD) };
    static STRAIGHT_LINE: Cell<bool> = const { Cell::new(false) };
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
    ENTRY_LIMIT.with(|limit| limit.set(entries));
    // A body without a loop runs each compiled instruction once per call, so entering compiled
    // code and forcing the arguments through helpers costs more than tier 0's dispatch saves.
    // Such bodies pay off only once calls go directly from compiled code to compiled code, so
    // the default mode leaves them to tier 0 and only `force` compiles them, for coverage.
    STRAIGHT_LINE.with(|straight_line| straight_line.set(mode == Mode::Force));
    bytecode::set_tier(Some(Tier {
        enter,
        back_edge,
        back_edge_threshold: back_edges,
    }));
}

unsafe fn enter(machine: &mut Machine<'_>) -> Option<TierExit> {
    unsafe {
        if bytecode::loop_count(machine.code) == 0 && !STRAIGHT_LINE.with(Cell::get) {
            return None;
        }
        let entries = values::ints(machine.code.cdr().element(PROFILE))[profile::ENTRIES];
        let compiled = compiled_for(machine, entries >= ENTRY_LIMIT.with(Cell::get))?;
        run(machine, compiled, 0)
    }
}

unsafe fn back_edge(machine: &mut Machine<'_>, target: usize) -> Option<TierExit> {
    unsafe {
        let compiled = compiled_for(machine, true)?;
        if !compiled.osr.contains(&target) {
            return None;
        }
        run(machine, compiled, target)
    }
}

/// The compiled code for the activation, compiling it now when `hot` and it has none.
unsafe fn compiled_for<'a>(machine: &Machine<'_>, hot: bool) -> Option<&'a Compiled> {
    unsafe {
        let constants = machine.code.cdr();
        let tier1 = constants.element(TIER1);
        if tier1 != values::nil() {
            let compiled = &*(tier1.external_address() as *const Compiled);
            // Closures made by one `function` expression share their body but not their
            // enclosure, which the compiled lookups are specialized to.
            return (compiled.enclosure == machine.env.enclosure()).then_some(compiled);
        }
        let counters = values::ints_mut(constants.element(PROFILE));
        if !hot || counters[profile::TIER_STATE] < 0 {
            return None;
        }
        counters[profile::TIER_STATE] += 1;
        if counters[profile::TIER_STATE] > MAX_COMPILES {
            counters[profile::TIER_STATE] = -1;
            return None;
        }
        match compile(machine) {
            Ok(compiled) => Some(compiled),
            Err(Refusal(reason)) => {
                if std::env::var_os("RY_JIT_LOG").is_some() {
                    eprintln!("jit: not compiling {}: {reason}", describe(machine.code));
                }
                if reason != analysis::COLD {
                    values::ints_mut(constants.element(PROFILE))[profile::TIER_STATE] = -1;
                }
                None
            }
        }
    }
}

unsafe fn describe(code: Sexp) -> String {
    unsafe {
        let source = code.cdr().element(bytecode::SOURCE);
        interp::deparse::deparse_one_line(source)
            .chars()
            .take(60)
            .collect()
    }
}

unsafe fn compile<'a>(machine: &Machine<'_>) -> Result<&'a Compiled, Refusal> {
    unsafe {
        let enclosure = machine.env.enclosure();
        let plan = analysis::plan(machine.code, machine.function.car(), enclosure)?;
        let entry = codegen::compile(&plan, machine.code)?;
        if std::env::var_os("RY_JIT_LOG").is_some() {
            eprintln!("jit: compiled {}", describe(machine.code));
        }
        let osr = plan
            .loop_headers
            .iter()
            .map(|&block| plan.insts[plan.blocks[block].start].pc)
            .collect();
        let compiled = Box::new(Compiled {
            entry,
            enclosure,
            registers: plan.registers,
            slots: plan.slots.clone(),
            loops: plan.loops,
            speculations: plan.speculations.clone(),
            epoch: Cell::new(heap().binding_epoch()),
            osr,
        });
        // Everything the machine code and the speculations point to stays reachable from the
        // external pointer, so the collector never frees an object compiled code embeds.
        let mut kept = vec![enclosure];
        kept.extend(plan.slots.iter().copied());
        for speculation in &plan.speculations {
            kept.extend([speculation.symbol, speculation.cell, speculation.function]);
            kept.extend(speculation.frames.iter().copied());
            kept.extend(speculation.home);
        }
        let heap = heap();
        let mark = heap.handle_mark();
        let built = (|| -> R<Sexp> {
            let protected = values::alloc(SexpType::List, kept.len())?;
            heap.root(protected);
            for (index, object) in kept.iter().enumerate() {
                heap.set_element(protected, index, *object);
            }
            let address = Box::into_raw(compiled) as *mut c_void;
            heap.alloc_external_ptr(address, values::nil(), protected)
                .map_err(|_| error("the JIT could not allocate its code object"))
        })();
        heap.release_handles(mark);
        let pointer = built.map_err(|_| Refusal("out of memory"))?;
        heap.set_element(machine.code.cdr(), TIER1, pointer);
        Ok(&*(pointer.external_address() as *const Compiled))
    }
}

/// Runs compiled code for the activation from `entry_pc`. `None` hands the activation to the
/// interpreter unchanged.
unsafe fn run(machine: &mut Machine<'_>, compiled: &Compiled, entry_pc: usize) -> Option<TierExit> {
    unsafe {
        if !runtime::valid(compiled) {
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
        let result: *mut Header = (compiled.entry)(&mut activation);
        if let Some(payload) = activation.panic.take() {
            resume_unwind(payload);
        }
        match result as usize {
            runtime::DECLINED => return None,
            runtime::DEOPTIMIZED => {
                return Some(TierExit::Resume {
                    pc: activation.resume_pc,
                    pending: activation.pending.take(),
                });
            }
            _ => {}
        }
        Some(TierExit::Finished(match Sexp::from_raw(result) {
            Some(value) => Ok(value),
            None => Err(activation
                .pending
                .take()
                .unwrap_or_else(|| error("compiled code failed without an error"))),
        }))
    }
}
