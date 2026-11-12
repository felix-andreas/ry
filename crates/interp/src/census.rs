//! The native-share census, built only with the `census` feature: which closure bodies run in
//! the JIT's native code, and for how much of the run. With `RY_CENSUS=FILE`, a thread samples the
//! innermost running closure body every millisecond of wall time, together with whether it is in
//! native code, and the session's end writes one tab-separated row per body to `FILE`.
//!
//! The samples measure time, not instructions: callgrind cannot attribute instructions to a
//! closure, because tier 0 runs every body in the same handlers. A body's time includes the
//! builtins it calls (and code `eval` runs for it) but not the closure bodies it calls, which are
//! counted as their own. `tools/native-share.py` turns the file into the tables of
//! `.agents/memory/decisions.md`, "Native share: the 80% target needs install-time compilation".

use {
    crate::{bytecode, deparse, values},
    heap::object::{Header, Sexp},
    rustc_hash::FxHashMap,
    std::{
        cell::RefCell,
        fmt::Write,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread::JoinHandle,
        time::Duration,
    },
};

/// Marks a body key as an AST body rather than a code object. Objects are at least 8-byte
/// aligned, so the bit is free.
const TREE: usize = 2;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static NATIVE: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static SAMPLER: Mutex<Option<JoinHandle<FxHashMap<(usize, bool), u64>>>> = Mutex::new(None);

thread_local! {
    static BODIES: RefCell<FxHashMap<usize, Body>> = RefCell::new(FxHashMap::default());
}

#[derive(Default)]
struct Body {
    /// The body's source, which code objects of one closure share across recompiles.
    source: usize,
    calls: u64,
    native_entries: u64,
    /// The JIT's refusal reasons for the body, in order.
    refusals: Vec<&'static str>,
    /// The instructions a refused plan handed to tier 0, as the JIT labels them.
    steps: String,
}

/// Restores the body that ran before a hook replaced it, when the hooked activation ends.
pub struct Scope {
    current: usize,
    native: bool,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.store(self.current, Ordering::Relaxed);
        NATIVE.store(self.native, Ordering::Relaxed);
    }
}

/// Whether `RY_CENSUS` asks for a census. The first call starts the sampler.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("RY_CENSUS").is_some();
        if on && let Ok(mut slot) = SAMPLER.lock() {
            *slot = Some(std::thread::spawn(|| {
                let mut samples: FxHashMap<(usize, bool), u64> = FxHashMap::default();
                while !STOP.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                    let key = (CURRENT.load(Ordering::Relaxed), NATIVE.load(Ordering::Relaxed));
                    *samples.entry(key).or_insert(0) += 1;
                }
                samples
            }));
        }
        on
    })
}

/// A tier-0 activation of `code`, the compiled body of `function`, starts.
///
/// # Safety
/// `function` and `code` are live objects.
pub unsafe fn enter_code(function: Sexp, code: Sexp) -> Option<Scope> {
    if !on() {
        return None;
    }
    let key = code.as_ptr() as usize;
    count(key, || preserve(function, code, unsafe { code.cdr().element(bytecode::SOURCE) }));
    Some(enter(key, false))
}

/// `function`'s uncompiled `body` starts on the AST evaluator.
///
/// # Safety
/// `function` and `body` are live objects.
pub unsafe fn enter_tree(function: Sexp, body: Sexp) -> Option<Scope> {
    if !on() {
        return None;
    }
    let key = body.as_ptr() as usize | TREE;
    count(key, || preserve(function, body, body));
    Some(enter(key, false))
}

/// The JIT's native code for `code` starts.
pub fn enter_native(code: Sexp) -> Option<Scope> {
    if !on() {
        return None;
    }
    let key = code.as_ptr() as usize;
    BODIES.with(|bodies| {
        if let Some(body) = bodies.borrow_mut().get_mut(&key) {
            body.native_entries += 1;
        }
    });
    Some(enter(key, true))
}

/// The JIT declined to compile `code`, for `reason`.
pub fn refused(code: Sexp, reason: &'static str) {
    if !on() {
        return;
    }
    BODIES.with(|bodies| {
        if let Some(body) = bodies.borrow_mut().get_mut(&(code.as_ptr() as usize)) {
            body.refusals.push(reason);
        }
    });
}

/// The JIT planned `code` and would hand the instructions `steps` to tier 0.
pub fn steps(code: Sexp, steps: String) {
    if !on() {
        return;
    }
    BODIES.with(|bodies| {
        if let Some(body) = bodies.borrow_mut().get_mut(&(code.as_ptr() as usize)) {
            body.steps = steps;
        }
    });
}

/// Writes the census `RY_CENSUS` names, if any: one row per body, then one per sampled key that
/// is not a body (`top` for time outside every closure body).
///
/// # Safety
/// Called on the interpreter's thread at the end of the session.
pub unsafe fn finish() {
    let Some(path) = std::env::var_os("RY_CENSUS") else {
        return;
    };
    STOP.store(true, Ordering::Relaxed);
    let Some(sampler) = SAMPLER.lock().ok().and_then(|mut slot| slot.take()) else {
        return;
    };
    let Ok(samples) = sampler.join() else {
        eprintln!("census: the sampler thread panicked");
        return;
    };
    let mut by_key: FxHashMap<usize, (u64, u64)> = FxHashMap::default();
    for (&(key, native), &count) in &samples {
        let slot = by_key.entry(key).or_insert((0, 0));
        if native {
            slot.1 += count;
        } else {
            slot.0 += count;
        }
    }
    let mut out = String::from(
        "key\tkind\tsource\tcalls\tnative_entries\ttier_state\tsamples_interp\tsamples_native\trefusals\tsteps\tname\n",
    );
    BODIES.with(|bodies| {
        for (&key, body) in bodies.borrow().iter() {
            let (interp_samples, native_samples) = by_key.remove(&key).unwrap_or((0, 0));
            let tree = key & TREE != 0;
            let tier_state = match (tree, unsafe { Sexp::from_raw(key as *mut Header) }) {
                (false, Some(code)) => unsafe {
                    values::ints(code.cdr().element(bytecode::PROFILE))
                        [bytecode::profile::TIER_STATE]
                },
                _ => 0,
            };
            let Some(source) = (unsafe { Sexp::from_raw(body.source as *mut Header) }) else {
                continue;
            };
            let name: String = unsafe { deparse::deparse_one_line(source) }
                .chars()
                .take(120)
                .map(|character| if character == '\t' { ' ' } else { character })
                .collect();
            let mut refusals = body.refusals.clone();
            refusals.dedup();
            // Writing to a `String` cannot fail.
            if writeln!(
                out,
                "{key:x}\t{}\t{:x}\t{}\t{}\t{tier_state}\t{interp_samples}\t{native_samples}\t{}\t{}\t{name}",
                if tree { "tree" } else { "code" },
                body.source,
                body.calls,
                body.native_entries,
                refusals.join("|"),
                body.steps,
            )
            .is_err()
            {
                return;
            }
        }
    });
    for (key, (interp_samples, native_samples)) in by_key {
        let kind = if key == 0 { "top" } else { "unknown" };
        if writeln!(
            out,
            "{key:x}\t{kind}\t0\t0\t0\t0\t{interp_samples}\t{native_samples}\t\t\t{kind}"
        )
        .is_err()
        {
            return;
        }
    }
    if let Err(error) = std::fs::write(&path, out) {
        eprintln!("census: cannot write {}: {error}", path.to_string_lossy());
    }
}

/// Counts a call of the body `key`, registering it with `make` the first time.
fn count(key: usize, make: impl FnOnce() -> Body) {
    BODIES.with(|bodies| {
        bodies.borrow_mut().entry(key).or_insert_with(make).calls += 1;
    });
}

/// Keeps `function` and `object` alive for the session, so that the census's keys are never
/// reused by another object and `finish` can still read them.
fn preserve(function: Sexp, object: Sexp, source: Sexp) -> Body {
    let heap = crate::heap();
    heap.preserve(object);
    if function != values::nil() {
        heap.preserve(function);
    }
    Body {
        source: source.as_ptr() as usize,
        ..Body::default()
    }
}

fn enter(key: usize, native: bool) -> Scope {
    let saved = Scope {
        current: CURRENT.load(Ordering::Relaxed),
        native: NATIVE.load(Ordering::Relaxed),
    };
    CURRENT.store(key, Ordering::Relaxed);
    NATIVE.store(native, Ordering::Relaxed);
    saved
}
