//! What the R thread is doing right now, for the profile summary (`ry run --profile-summary`).
//!
//! The runtime publishes its current activity as one word: an [`Activity`] in the low byte and a
//! detail (a builtin's index, a native library's label) above it. A sampling thread reads the word
//! at a fixed interval and counts each value, so the summary's time split costs the R thread only a
//! store on entry to an activity and one on exit. Every hook first checks [`enabled`], which is
//! false unless a summary was asked for, so a normal run pays one predictable branch per hook.
//!
//! Counters count events (closure calls, promises forced, dispatches) on the R thread. They are
//! plain relaxed loads and stores rather than read-modify-write atomics, because only the R thread
//! writes them and a locked add costs as much as the work some of them count.

use std::sync::{
    Mutex,
    atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

/// The categories of the time split, in the order the summary prints them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Activity {
    /// Anything no hook claims: start-up, parsing, printing, the console.
    Other = 0,
    /// Tier 0: the bytecode interpreter's own instructions, including its inline fast paths for
    /// arithmetic and subsetting.
    Interpreter,
    /// The AST evaluator, for code that is not compiled to bytecode.
    Evaluator,
    /// A closure call's own overhead: matching arguments, creating promises and the frame.
    CallSetup,
    /// Variable and function lookups that leave the inline caches.
    Lookup,
    /// A base R builtin or internal; the detail is its index.
    Builtin,
    /// Finding an S3 method (`UseMethod`, `NextMethod`, internal dispatch).
    S3Dispatch,
    /// Selecting an S4 method.
    S4Dispatch,
    /// Native code reached through `.Call`, `.External` or `.C`; the detail is a [`label`].
    Native,
    /// The builtins that signal conditions and establish handlers and restarts. The R thread
    /// never publishes this: the summary moves builtin samples here by the builtin's name.
    Conditions,
    /// `sys.call`, `parent.frame`, `match.call` and their relatives, classified like
    /// [`Activity::Conditions`].
    Introspection,
    /// The garbage collector.
    Collector,
    /// Compiling a body to machine code.
    JitCompile,
    /// Machine code the JIT produced.
    Compiled,
    /// The summary's own bookkeeping that cannot be a store: counting a collection's dead
    /// objects by type, which reads every dead object's header.
    Profiler,
}

impl Activity {
    pub const ALL: [Activity; 15] = [
        Activity::Other,
        Activity::Interpreter,
        Activity::Evaluator,
        Activity::CallSetup,
        Activity::Lookup,
        Activity::Builtin,
        Activity::S3Dispatch,
        Activity::S4Dispatch,
        Activity::Native,
        Activity::Conditions,
        Activity::Introspection,
        Activity::Collector,
        Activity::JitCompile,
        Activity::Compiled,
        Activity::Profiler,
    ];

    pub fn from_word(word: u32) -> Activity {
        Activity::ALL
            .get((word & 0xff) as usize)
            .copied()
            .unwrap_or(Activity::Other)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Activity::Other => "other (start-up, parsing, printing)",
            Activity::Interpreter => "tier 0 bytecode instructions",
            Activity::Evaluator => "AST evaluator (uncompiled code)",
            Activity::CallSetup => "closure call setup (matching, promises, frame)",
            Activity::Lookup => "variable and function lookup (cache misses)",
            Activity::Builtin => "builtins",
            Activity::S3Dispatch => "S3 method lookup",
            Activity::S4Dispatch => "S4 method selection",
            Activity::Native => "native code (.Call, .External, .C)",
            Activity::Conditions => "condition handling",
            Activity::Introspection => "frame introspection (sys.call, parent.frame, ...)",
            Activity::Collector => "garbage collection",
            Activity::JitCompile => "JIT compilation",
            Activity::Compiled => "JIT-compiled code",
            Activity::Profiler => "profile summary bookkeeping",
        }
    }
}

/// Event counts the summary reports beside the time split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Counter {
    ClosureCalls,
    NativeEntries,
    BuiltinCalls,
    PromisesCreated,
    ArgumentReferences,
    PromisesForced,
    FormalsMatchedByName,
    DotsCalls,
    S3Dispatches,
    S4Dispatches,
    NativeCalls,
    ConditionsSignalled,
    HandlersEstablished,
    JitCompiles,
    JitDeoptimizations,
    JitEntries,
    LookupMisses,
    FramesReified,
    PackageCalls,
    PackageCallsDirect,
    PackageCallsObserved,
    WrappersInlined,
    DoCallsInlined,
}

impl Counter {
    pub const ALL: [Counter; 23] = [
        Counter::ClosureCalls,
        Counter::NativeEntries,
        Counter::BuiltinCalls,
        Counter::PromisesCreated,
        Counter::ArgumentReferences,
        Counter::PromisesForced,
        Counter::FormalsMatchedByName,
        Counter::DotsCalls,
        Counter::S3Dispatches,
        Counter::S4Dispatches,
        Counter::NativeCalls,
        Counter::ConditionsSignalled,
        Counter::HandlersEstablished,
        Counter::JitCompiles,
        Counter::JitDeoptimizations,
        Counter::JitEntries,
        Counter::LookupMisses,
        Counter::FramesReified,
        Counter::PackageCalls,
        Counter::PackageCallsDirect,
        Counter::PackageCallsObserved,
        Counter::WrappersInlined,
        Counter::DoCallsInlined,
    ];

    pub fn describe(self) -> &'static str {
        match self {
            Counter::ClosureCalls => "closure calls",
            Counter::NativeEntries => "closure calls run natively, without a frame",
            Counter::BuiltinCalls => "builtin calls (outside inline fast paths)",
            Counter::PromisesCreated => "promises created",
            Counter::ArgumentReferences => "arguments passed as references to a caller's slot",
            Counter::PromisesForced => "promises forced",
            Counter::FormalsMatchedByName => "calls matched by name or partially",
            Counter::DotsCalls => "calls passing `...`",
            Counter::S3Dispatches => "S3 dispatches",
            Counter::S4Dispatches => "S4 dispatches",
            Counter::NativeCalls => "native calls",
            Counter::ConditionsSignalled => "conditions signalled",
            Counter::HandlersEstablished => "handlers established",
            Counter::JitCompiles => "JIT compilations",
            Counter::JitDeoptimizations => "JIT deoptimizations",
            Counter::JitEntries => "entries into compiled code",
            Counter::LookupMisses => "lookups past the inline caches",
            Counter::FramesReified => "slot frames turned into environments",
            Counter::PackageCalls => "calls to closures of package namespaces",
            Counter::PackageCallsDirect => {
                "package calls that ran in slots, with no argument promise and no environment"
            }
            Counter::PackageCallsObserved => {
                "package calls whose frame was reified because code observed its environment"
            }
            Counter::WrappersInlined => "calls of one-builtin wrappers run without their body",
            Counter::DoCallsInlined => "do.call calls run without its body",
        }
    }
}

/// The most builtins [`count_builtin`] keeps a count for; ry defines fewer than this.
pub const BUILTIN_SLOTS: usize = 4096;

/// The most [`label`]s [`count_package_fallback`] keeps a count for.
pub const LABEL_SLOTS: usize = 1024;

/// What makes a call leave its fast path, one bit each: [`SUMMARY`] while a profile summary is
/// collected, [`PROFILE_TICK`] once per `Rprof` sampling interval, and the interpreter's own
/// observers of calls ([`DEBUGGER`], [`MOVES`], [`NO_OBJECT_SYSTEMS`]). They share one byte so
/// a call tests all of them with a single load ([`hooks`]). The byte is atomic because `Rprof`'s
/// timer thread sets its bit; every other bit is written by the R thread, by whatever changes
/// the state the bit stands for ([`set_hook`]).
static HOOKS: AtomicU8 = AtomicU8::new(0);
/// The [`HOOKS`] bit [`enabled`] reads.
pub const SUMMARY: u8 = 1;
/// The [`HOOKS`] bit set by `Rprof`'s timer thread and cleared once the R thread records a
/// sample. GNU R takes its samples in a `SIGPROF` handler; ry checks the bit where calls begin.
pub const PROFILE_TICK: u8 = 2;
/// The [`HOOKS`] bit set while a debugger is attached to the interpreter.
pub const DEBUGGER: u8 = 4;
/// The [`HOOKS`] bit set while a call that moves an argument into its callee is running.
pub const MOVES: u8 = 8;
/// The [`HOOKS`] bit set while the session refuses to define S4, R6 and reference classes.
pub const NO_OBJECT_SYSTEMS: u8 = 16;
static CURRENT: AtomicU32 = AtomicU32::new(0);
static COUNTERS: [AtomicU64; Counter::ALL.len()] =
    [const { AtomicU64::new(0) }; Counter::ALL.len()];
static BUILTINS: [AtomicU64; BUILTIN_SLOTS] = [const { AtomicU64::new(0) }; BUILTIN_SLOTS];
static LABELS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PACKAGE_FALLBACKS: [AtomicU64; LABEL_SLOTS] = [const { AtomicU64::new(0) }; LABEL_SLOTS];
static REIFICATIONS: [AtomicU64; LABEL_SLOTS] = [const { AtomicU64::new(0) }; LABEL_SLOTS];
static FRAME_OWNERS: [AtomicU32; FRAME_SLOTS] = [const { AtomicU32::new(0) }; FRAME_SLOTS];
static FRAME_NAMES: [AtomicUsize; FRAME_SLOTS] = [const { AtomicUsize::new(0) }; FRAME_SLOTS];
static FRAME_BODIES: [AtomicUsize; FRAME_SLOTS] = [const { AtomicUsize::new(0) }; FRAME_SLOTS];
static FRAME_DEPTH: AtomicU32 = AtomicU32::new(0);

/// The deepest closure frame the sampler sees; a sample deeper than this is attributed to the
/// frame at this depth.
pub const FRAME_SLOTS: usize = 1024;

/// Whether a profile summary is being collected.
#[inline(always)]
pub fn enabled() -> bool {
    hooks() & SUMMARY != 0
}

/// Starts or stops publishing activities and counting events.
pub fn set_enabled(on: bool) {
    if on {
        HOOKS.fetch_or(SUMMARY, Ordering::Relaxed);
    } else {
        HOOKS.fetch_and(!SUMMARY, Ordering::Relaxed);
    }
}

/// The hook bits that are set; zero while nothing observes calls.
#[inline(always)]
pub fn hooks() -> u8 {
    HOOKS.load(Ordering::Relaxed)
}

/// Whether `Rprof`'s interval elapsed since the last sample.
#[inline(always)]
pub fn profile_ticked() -> bool {
    hooks() & PROFILE_TICK != 0
}

/// Sets (`Rprof`'s timer) or clears (a recorded sample, a stopped profile) the profile tick.
pub fn set_profile_tick(on: bool) {
    if on {
        HOOKS.fetch_or(PROFILE_TICK, Ordering::Relaxed);
    } else {
        HOOKS.fetch_and(!PROFILE_TICK, Ordering::Relaxed);
    }
}

/// Sets or clears one of the interpreter's bits in [`HOOKS`], as the state it stands for changes.
pub fn set_hook(bit: u8, on: bool) {
    if on {
        HOOKS.fetch_or(bit, Ordering::Relaxed);
    } else {
        HOOKS.fetch_and(!bit, Ordering::Relaxed);
    }
}

/// The current activity word, for the sampling thread.
pub fn current() -> u32 {
    CURRENT.load(Ordering::Relaxed)
}

/// An activity entered by [`enter`], restored to the one before it when dropped.
#[must_use]
pub struct Scope {
    previous: u32,
    active: bool,
}

/// Publishes `activity` with `detail` until the returned scope drops. A non-local exit that
/// skips the drop (a `longjmp` through native code) leaves the word stale only until the next
/// hook overwrites it.
#[inline(always)]
pub fn enter(activity: Activity, detail: u32) -> Scope {
    if !enabled() {
        return Scope {
            previous: 0,
            active: false,
        };
    }
    let previous = CURRENT.load(Ordering::Relaxed);
    CURRENT.store(activity as u32 | (detail << 8), Ordering::Relaxed);
    Scope {
        previous,
        active: true,
    }
}

impl Drop for Scope {
    #[inline(always)]
    fn drop(&mut self) {
        if self.active {
            CURRENT.store(self.previous, Ordering::Relaxed);
        }
    }
}

#[inline(always)]
pub fn count(counter: Counter) {
    if enabled() {
        bump(&COUNTERS[counter as usize]);
    }
}

#[inline(always)]
pub fn count_builtin(index: usize) {
    if enabled()
        && let Some(slot) = BUILTINS.get(index)
    {
        bump(slot);
    }
}

#[inline(always)]
fn bump(counter: &AtomicU64) {
    counter.store(counter.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

/// Counts a package call that took the generic call protocol, under the [`label`] of the reason.
pub fn count_package_fallback(reason: u32) {
    if let Some(slot) = PACKAGE_FALLBACKS.get(reason as usize) {
        bump(slot);
    }
}

/// The package calls [`count_package_fallback`] counted under each reason, largest first.
pub fn package_fallbacks() -> Vec<(String, u64)> {
    by_label(&PACKAGE_FALLBACKS)
}

/// Counts a slot frame turned into an environment, under the [`label`] of what asked for it.
pub fn count_reification(reason: u32) {
    if let Some(slot) = REIFICATIONS.get(reason as usize) {
        bump(slot);
    }
}

/// The slot frames [`count_reification`] counted under each reason, largest first.
pub fn reifications() -> Vec<(String, u64)> {
    by_label(&REIFICATIONS)
}

fn by_label(slots: &[AtomicU64; LABEL_SLOTS]) -> Vec<(String, u64)> {
    let mut counts: Vec<(String, u64)> = slots
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| {
            let count = slot.load(Ordering::Relaxed);
            (count > 0).then(|| {
                (
                    label_name(id as u32).unwrap_or_else(|| "?".to_owned()),
                    count,
                )
            })
        })
        .collect();
    counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    counts
}

pub fn counter(counter: Counter) -> u64 {
    COUNTERS[counter as usize].load(Ordering::Relaxed)
}

pub fn builtin_count(index: usize) -> u64 {
    BUILTINS
        .get(index)
        .map_or(0, |slot| slot.load(Ordering::Relaxed))
}

/// A small id for `name`, such as a native library's package, to use as an activity's detail.
/// The same name always gets the same id.
pub fn label(name: &str) -> u32 {
    let mut labels = match LABELS.lock() {
        Ok(labels) => labels,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(index) = labels.iter().position(|label| label == name) {
        return index as u32;
    }
    labels.push(name.to_owned());
    (labels.len() - 1) as u32
}

/// The name [`label`] gave `id`.
pub fn label_name(id: u32) -> Option<String> {
    let labels = match LABELS.lock() {
        Ok(labels) => labels,
        Err(poisoned) => poisoned.into_inner(),
    };
    labels.get(id as usize).cloned()
}

/// A closure frame published by [`push_frame`], popped when dropped.
#[must_use]
pub struct FrameScope {
    depth: u32,
    active: bool,
}

/// Publishes a closure call for the sampler: `owner` is a [`label`] for the package (or other top
/// level) the closure belongs to, and `name` is the address of the symbol the call used, which is
/// never freed, or 0 for an anonymous function. `body` is the address of the closure's body, which
/// tells closures of one name apart and counts a body's entries however many closures share it.
///
/// The sampler reads the stack without synchronizing with the R thread, so a sample taken during
/// a push or pop can see one frame too many or too few. That is noise of one sample, and it keeps
/// the push to three stores.
#[inline(always)]
pub fn push_frame(owner: u32, name: usize, body: usize) -> FrameScope {
    let depth = FRAME_DEPTH.load(Ordering::Relaxed);
    if let (Some(owner_slot), Some(name_slot), Some(body_slot)) = (
        FRAME_OWNERS.get(depth as usize),
        FRAME_NAMES.get(depth as usize),
        FRAME_BODIES.get(depth as usize),
    ) {
        owner_slot.store(owner, Ordering::Relaxed);
        name_slot.store(name, Ordering::Relaxed);
        body_slot.store(body, Ordering::Relaxed);
    }
    FRAME_DEPTH.store(depth + 1, Ordering::Release);
    FrameScope {
        depth,
        active: true,
    }
}

/// A scope that publishes nothing, for when no summary is being collected.
pub fn no_frame() -> FrameScope {
    FrameScope {
        depth: 0,
        active: false,
    }
}

impl Drop for FrameScope {
    #[inline(always)]
    fn drop(&mut self) {
        // Restoring the saved depth rather than decrementing heals the stack after a non-local
        // exit through native code skipped the drops of inner frames.
        if self.active {
            FRAME_DEPTH.store(self.depth, Ordering::Release);
        }
    }
}

/// Copies the published closure frames, outermost first, into `owners`, `names` and `bodies`.
pub fn read_frames(owners: &mut Vec<u32>, names: &mut Vec<usize>, bodies: &mut Vec<usize>) {
    owners.clear();
    names.clear();
    bodies.clear();
    let depth = (FRAME_DEPTH.load(Ordering::Acquire) as usize).min(FRAME_SLOTS);
    for index in 0..depth {
        owners.push(FRAME_OWNERS[index].load(Ordering::Relaxed));
        names.push(FRAME_NAMES[index].load(Ordering::Relaxed));
        bodies.push(FRAME_BODIES[index].load(Ordering::Relaxed));
    }
}


/// Exact allocation counts by object kind, and the call paths of a sample of them, while a
/// profile summary is collected with `RY_ALLOC_SITES` set to the sampling period.
static ALLOCATED: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
static ALLOCATION_PERIOD: AtomicU64 = AtomicU64::new(0);
static ALLOCATION_SITES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

pub fn set_allocation_period(period: u64) {
    ALLOCATION_PERIOD.store(period, Ordering::Relaxed);
}

#[inline(always)]
pub fn allocated(kind: u32) {
    if enabled() {
        allocated_slow(kind);
    }
}

#[cold]
#[inline(never)]
fn allocated_slow(kind: u32) {
    let Some(counter) = ALLOCATED.get(kind as usize) else {
        return;
    };
    let count = counter.load(Ordering::Relaxed) + 1;
    counter.store(count, Ordering::Relaxed);
    let period = ALLOCATION_PERIOD.load(Ordering::Relaxed);
    if period == 0 || count % period != 0 {
        return;
    }
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    let mut frames: Vec<&str> = Vec::new();
    for line in trace.lines() {
        let line = line.trim_start();
        let Some((index, name)) = line.split_once(": ") else {
            continue;
        };
        if index.parse::<u32>().is_err() {
            continue;
        }
        if name.contains("activity::")
            || name.contains("backtrace")
            || name.contains("Heap>::alloc")
            || name.contains("Heap>::init_small")
            || name.contains("heap::heap::Heap")
        {
            continue;
        }
        frames.push(name);
        if frames.len() == 10 {
            break;
        }
    }
    let key = format!("{kind}\t{}", frames.join(" < "));
    let mut sites = match ALLOCATION_SITES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match sites.iter_mut().find(|(site, _)| *site == key) {
        Some((_, count)) => *count += 1,
        None => sites.push((key, 1)),
    }
}

pub fn allocation_counts() -> [u64; 32] {
    std::array::from_fn(|kind| ALLOCATED[kind].load(Ordering::Relaxed))
}

pub fn allocation_sites() -> Vec<(String, u64)> {
    match ALLOCATION_SITES.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}
