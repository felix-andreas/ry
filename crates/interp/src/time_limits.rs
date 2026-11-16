//! `setTimeLimit()` and `setSessionTimeLimit()`: elapsed and CPU time limits on evaluation,
//! raised as GNU R raises them where it checks for a user interrupt.
//!
//! The evaluator's poll at every call and loop back edge reads one flag
//! ([`crate::unwind::INTERRUPT_REQUESTED`]), and compiled code reads the same flag at its back
//! edges, so the poll cannot read a clock. A watchdog thread waits for the nearest deadline
//! instead and raises the flag once it passes. The flag only makes the evaluator look: the
//! check that raises the error reads the clocks itself, as GNU R's `R_ProcessEvents` does, so
//! the thread never decides an outcome.

use {
    crate::{interp, unwind::R},
    std::{
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        thread::Thread,
        time::{Duration, Instant},
    },
};

/// The limits in force, as GNU R's `cpuLimit`, `cpuLimit2`, `cpuLimitValue` and their elapsed
/// counterparts. Times are seconds: elapsed time since the session started, CPU time as
/// `proc.time()` counts it (user and system, the children's included).
#[derive(Default)]
pub struct State {
    /// `setTimeLimit()`'s persistent limits, which every top-level expression restarts.
    cpu_value: Option<f64>,
    elapsed_value: Option<f64>,
    /// `setSessionTimeLimit()`'s deadlines.
    session_cpu: Option<f64>,
    session_elapsed: Option<f64>,
    /// The deadlines in force: the persistent limit restarted at the last reset, or the session
    /// deadline when that comes first.
    cpu: Option<f64>,
    elapsed: Option<f64>,
    /// The watchdog thread and the process that started it: a forked child has no copy of its
    /// parent's threads, so it starts its own.
    watchdog: Option<(u32, Thread)>,
}

/// Raised by the watchdog once a deadline has passed, and lowered when the evaluator checks.
pub static DUE: AtomicBool = AtomicBool::new(false);

/// The deadlines the watchdog waits for, as `f64` bits, with [`NONE`] for no deadline. Only the
/// evaluator's thread writes them, so the watchdog never holds a lock a fork could copy taken.
static CPU_DEADLINE: AtomicU64 = AtomicU64::new(NONE);
static ELAPSED_DEADLINE: AtomicU64 = AtomicU64::new(NONE);
const NONE: u64 = u64::MAX;

/// How often the watchdog reads the CPU clock while a CPU limit is armed: CPU time has no
/// deadline to sleep until.
const CPU_POLL: Duration = Duration::from_millis(20);

/// `.Internal(setTimeLimit(cpu, elapsed, transient))`. A transient limit applies until the next
/// top-level expression, because the reset there no longer finds it.
pub fn set_time_limit(cpu: f64, elapsed: f64, transient: bool) {
    let limits = &mut interp().time_limits;
    let (old_cpu, old_elapsed) = (limits.cpu_value, limits.elapsed_value);
    limits.cpu_value = positive(cpu);
    limits.elapsed_value = positive(elapsed);
    reset();
    if transient {
        let limits = &mut interp().time_limits;
        limits.cpu_value = old_cpu;
        limits.elapsed_value = old_elapsed;
    }
}

/// `.Internal(setSessionTimeLimit(cpu, elapsed))`. As in GNU R, the deadlines take effect at
/// the next top-level expression.
pub fn set_session_time_limit(cpu: f64, elapsed: f64) {
    let (cpu_now, elapsed_now) = (cpu_seconds(), elapsed_seconds());
    let limits = &mut interp().time_limits;
    limits.session_cpu = positive(cpu).map(|limit| limit + cpu_now);
    limits.session_elapsed = positive(elapsed).map(|limit| limit + elapsed_now);
}

/// GNU R's `resetTimeLimits`, run before every top-level expression: each persistent limit
/// starts again from now, unless the session deadline comes first.
pub fn reset() {
    let limits = &interp().time_limits;
    if limits.cpu_value.is_none()
        && limits.elapsed_value.is_none()
        && limits.session_cpu.is_none()
        && limits.session_elapsed.is_none()
        && limits.cpu.is_none()
        && limits.elapsed.is_none()
    {
        return;
    }
    let cpu = limits.cpu_value.map(|limit| cpu_seconds() + limit);
    let elapsed = limits.elapsed_value.map(|limit| elapsed_seconds() + limit);
    let limits = &mut interp().time_limits;
    limits.cpu = earliest(cpu, limits.session_cpu);
    limits.elapsed = earliest(elapsed, limits.session_elapsed);
    publish();
}

/// The check `R_CheckUserInterrupt` makes: raises the limit that has passed, after which no
/// limit applies until the next reset. A session limit that has passed is gone for good.
/// Until the watchdog reports a passed deadline it reads no clock, so native code that checks
/// for interrupts in a tight loop pays one load, where GNU R throttles its clock reads instead.
pub fn check() -> R<()> {
    if !DUE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let limits = &interp().time_limits;
    let elapsed = elapsed_seconds();
    if limits.elapsed.is_some_and(|deadline| elapsed > deadline) {
        let session = limits
            .session_elapsed
            .is_some_and(|deadline| elapsed > deadline);
        let limits = &mut interp().time_limits;
        if session {
            limits.session_elapsed = None;
        }
        return Err(expire(if session {
            "reached session elapsed time limit"
        } else {
            "reached elapsed time limit"
        }));
    }
    if let Some(deadline) = limits.cpu {
        let cpu = cpu_seconds();
        if cpu > deadline {
            let session = limits.session_cpu.is_some_and(|deadline| cpu > deadline);
            let limits = &mut interp().time_limits;
            if session {
                limits.session_cpu = None;
            }
            return Err(expire(if session {
                "reached session CPU time limit"
            } else {
                "reached CPU time limit"
            }));
        }
    }
    // The watchdog read its clock a moment early: lower the flag and let it wait again.
    publish();
    Ok(())
}

/// Starts the watchdog again in a forked child that inherited armed limits.
pub fn after_fork() {
    interp().time_limits.watchdog = None;
    publish();
}

fn expire(message: &str) -> crate::unwind::Unwind {
    let limits = &mut interp().time_limits;
    limits.cpu = None;
    limits.elapsed = None;
    publish();
    interp().interrupt_countdown = crate::unwind::INTERRUPT_CHECK_INTERVAL;
    crate::unwind::error(message)
}

/// Hands the deadlines in force to the watchdog, starting it the first time one is armed.
fn publish() {
    let limits = &mut interp().time_limits;
    let bits = |deadline: Option<f64>| deadline.map_or(NONE, f64::to_bits);
    CPU_DEADLINE.store(bits(limits.cpu), Ordering::Relaxed);
    ELAPSED_DEADLINE.store(bits(limits.elapsed), Ordering::Relaxed);
    DUE.store(false, Ordering::Relaxed);
    if limits.cpu.is_none() && limits.elapsed.is_none() && limits.watchdog.is_none() {
        return;
    }
    let process = std::process::id();
    match &limits.watchdog {
        Some((owner, thread)) if *owner == process => thread.unpark(),
        _ => {
            let started = interp().started;
            let spawned = std::thread::Builder::new()
                .name("ry-time-limits".into())
                .spawn(move || watch(started));
            match spawned {
                Ok(handle) => {
                    interp().time_limits.watchdog = Some((process, handle.thread().clone()));
                }
                // Nothing raises a limit without the thread, so say so rather than ignore it.
                Err(failure) => crate::write_console(
                    format!("Warning: cannot start the time limit watchdog: {failure}\n")
                        .as_bytes(),
                    true,
                ),
            }
        }
    }
}

fn watch(started: Instant) {
    loop {
        let cpu = load(&CPU_DEADLINE);
        let elapsed = load(&ELAPSED_DEADLINE);
        if DUE.load(Ordering::Relaxed) || (cpu.is_none() && elapsed.is_none()) {
            std::thread::park();
            continue;
        }
        let mut wait = None;
        let mut passed = false;
        if let Some(deadline) = elapsed {
            let remaining = deadline - started.elapsed().as_secs_f64();
            if remaining <= 0.0 {
                passed = true;
            } else {
                wait = Some(Duration::from_secs_f64(remaining));
            }
        }
        if let Some(deadline) = cpu {
            if cpu_seconds() > deadline {
                passed = true;
            } else {
                wait = Some(wait.map_or(CPU_POLL, |wait: Duration| wait.min(CPU_POLL)));
            }
        }
        if passed {
            DUE.store(true, Ordering::Relaxed);
            crate::unwind::INTERRUPT_REQUESTED.store(true, Ordering::Relaxed);
            continue;
        }
        if let Some(wait) = wait {
            std::thread::park_timeout(wait);
        }
    }
}

fn load(deadline: &AtomicU64) -> Option<f64> {
    let bits = deadline.load(Ordering::Relaxed);
    (bits != NONE).then(|| f64::from_bits(bits))
}

fn positive(limit: f64) -> Option<f64> {
    (limit.is_finite() && limit > 0.0).then_some(limit)
}

fn earliest(first: Option<f64>, second: Option<f64>) -> Option<f64> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (first, second) => first.or(second),
    }
}

fn cpu_seconds() -> f64 {
    let times = crate::platform::cpu_times();
    let (child_user, child_system) = times.children.unwrap_or((0.0, 0.0));
    times.user + times.system + child_user + child_system
}

fn elapsed_seconds() -> f64 {
    interp().started.elapsed().as_secs_f64()
}
