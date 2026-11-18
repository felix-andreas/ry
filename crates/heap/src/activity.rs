//! What the R thread is doing right now, for the profile summary (`ry run --profile-summary`).
//!
//! The runtime publishes its activity as one word, an [`Activity`] in the low byte and a detail (a
//! builtin's index, a native library's label) above it, which a sampling thread reads at a fixed
//! interval, so the R thread pays one store on entry and one on exit. Every hook first checks
//! [`enabled`], so a run without a summary pays one predictable branch per hook. Counters of
//! events (closure calls, promises forced, dispatches) are relaxed loads and stores rather than
//! read-modify-write atomics, because only the R thread writes them and a locked add costs as much
//! as some of the work they count.
//!
//! A summary never decides which path a call takes. The bits that do are [`observers`]; the
//! summary's own two bits ([`SUMMARY`], [`SAMPLE_TICK`]) only add the recording to the path the
//! call takes anyway. The call stack of a sample is not published per call either: the sampling
//! thread sets [`SAMPLE_TICK`], and the R thread reads its own frames the next time its stack is
//! about to change, which is the stack the sample saw.

use std::sync::{
    Mutex,
    atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
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

    /// A stable snake_case name, for machine-readable output.
    pub fn key(self) -> &'static str {
        match self {
            Activity::Other => "other",
            Activity::Interpreter => "interpreter",
            Activity::Evaluator => "evaluator",
            Activity::CallSetup => "call_setup",
            Activity::Lookup => "lookup",
            Activity::Builtin => "builtin",
            Activity::S3Dispatch => "s3_dispatch",
            Activity::S4Dispatch => "s4_dispatch",
            Activity::Native => "native",
            Activity::Conditions => "conditions",
            Activity::Introspection => "introspection",
            Activity::Collector => "collector",
            Activity::JitCompile => "jit_compile",
            Activity::Compiled => "compiled",
            Activity::Profiler => "profiler",
        }
    }
}

/// Event counts the summary reports beside the time split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Counter {
    ClosureCalls,
    NativeEntries,
    InlinedCalls,
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
    JitRefusals,
    JitDeoptimizations,
    JitEntries,
    LookupMisses,
    FrameEnvironments,
    FramesReified,
    VariablesBoxed,
    FramesLifted,
    PackageCalls,
    PackageCallsDirect,
    DoCallsInlined,
}

impl Counter {
    pub const ALL: [Counter; 26] = [
        Counter::ClosureCalls,
        Counter::NativeEntries,
        Counter::InlinedCalls,
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
        Counter::JitRefusals,
        Counter::JitDeoptimizations,
        Counter::JitEntries,
        Counter::LookupMisses,
        Counter::FrameEnvironments,
        Counter::FramesReified,
        Counter::VariablesBoxed,
        Counter::FramesLifted,
        Counter::PackageCalls,
        Counter::PackageCallsDirect,
        Counter::DoCallsInlined,
    ];

    pub fn describe(self) -> &'static str {
        match self {
            Counter::ClosureCalls => "closure calls",
            Counter::NativeEntries => "closure calls run natively, without a frame",
            Counter::InlinedCalls => "closure calls inlined into compiled code, without a frame",
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
            Counter::JitRefusals => "JIT compile attempts refused",
            Counter::JitDeoptimizations => "JIT deoptimizations",
            Counter::JitEntries => "entries into compiled code",
            Counter::LookupMisses => "lookups past the inline caches",
            Counter::FrameEnvironments => "slot frames that made an environment to stand for them",
            Counter::FramesReified => {
                "slot frames that switched to reading their variables by name"
            }
            Counter::VariablesBoxed => "variables moved from a slot into a binding cell",
            Counter::FramesLifted => {
                "slot frames whose environment took every variable, to be listed or to outlive them"
            }
            Counter::PackageCalls => "calls to closures of package namespaces",
            Counter::PackageCallsDirect => {
                "package calls that ran in slots, with no argument promise and no environment"
            }
            Counter::DoCallsInlined => "do.call calls run without its body",
        }
    }

    /// A stable snake_case name, for machine-readable output.
    pub fn key(self) -> &'static str {
        match self {
            Counter::ClosureCalls => "closure_calls",
            Counter::NativeEntries => "native_entries",
            Counter::InlinedCalls => "inlined_calls",
            Counter::BuiltinCalls => "builtin_calls",
            Counter::PromisesCreated => "promises_created",
            Counter::ArgumentReferences => "argument_references",
            Counter::PromisesForced => "promises_forced",
            Counter::FormalsMatchedByName => "formals_matched_by_name",
            Counter::DotsCalls => "dots_calls",
            Counter::S3Dispatches => "s3_dispatches",
            Counter::S4Dispatches => "s4_dispatches",
            Counter::NativeCalls => "native_calls",
            Counter::ConditionsSignalled => "conditions_signalled",
            Counter::HandlersEstablished => "handlers_established",
            Counter::JitCompiles => "jit_compiles",
            Counter::JitRefusals => "jit_refusals",
            Counter::JitDeoptimizations => "jit_deoptimizations",
            Counter::JitEntries => "jit_entries",
            Counter::LookupMisses => "lookup_misses",
            Counter::FrameEnvironments => "frame_environments",
            Counter::FramesReified => "frames_reified",
            Counter::VariablesBoxed => "variables_boxed",
            Counter::FramesLifted => "frames_lifted",
            Counter::PackageCalls => "package_calls",
            Counter::PackageCallsDirect => "package_calls_direct",
            Counter::DoCallsInlined => "do_calls_inlined",
        }
    }
}

/// The most builtins [`record_builtin`] keeps a count for; ry defines fewer than this.
pub const BUILTIN_SLOTS: usize = 4096;

/// The most [`label`]s [`count_jit_refusal`] and [`count_general_assignment`] keep a count for.
pub const LABEL_SLOTS: usize = 1024;

/// The most reasons [`count_reification`] and [`count_package_fallback`] keep a count for. The
/// interpreter numbers the reasons: its own sites first, then one per builtin.
pub const REASON_SLOTS: usize = 64 + BUILTIN_SLOTS;

/// What a call's fast path asks about before it runs, one bit each. [`SUMMARY`] is set while
/// events are counted and activities published (a profile summary, benchmark regions, the
/// boundary-call recorder), and [`SAMPLE_TICK`] while a sample of the summary waits for its call
/// stack: a call under only these two takes the path it would take without them and records on
/// the way. The other bits are the [`observers`], which need the general path: [`CAPTURE`] while
/// boundary calls are recorded, [`PROFILE_TICK`] once per `Rprof` sampling interval, and the
/// interpreter's own observers of calls ([`DEBUGGER`], [`MOVES`], [`NO_OBJECT_SYSTEMS`]). They
/// share one byte so a call tests all of them with a single load ([`hooks`]). The byte is atomic
/// because the two sampling threads set their tick bits; every other bit is written by the R
/// thread, by whatever changes the state the bit stands for ([`set_hook`]).
static HOOKS: AtomicU8 = AtomicU8::new(0);
/// The [`HOOKS`] bit [`enabled`] reads.
pub const SUMMARY: u8 = 1;
/// The [`HOOKS`] bit the summary's sampling thread sets with each sample it queues, and the R
/// thread clears once it has filed the queued samples under its call stack.
pub const SAMPLE_TICK: u8 = 64;
/// The [`HOOKS`] bit set by `Rprof`'s timer thread and cleared once the R thread records a
/// sample. GNU R takes its samples in a `SIGPROF` handler; ry checks the bit where calls begin.
pub const PROFILE_TICK: u8 = 2;
/// The [`HOOKS`] bit set while a debugger is attached to the interpreter.
pub const DEBUGGER: u8 = 4;
/// The [`HOOKS`] bit set while a call that moves an argument into its callee is running.
pub const MOVES: u8 = 8;
/// The [`HOOKS`] bit set while the session refuses to define S4, R6 and reference classes.
pub const NO_OBJECT_SYSTEMS: u8 = 16;
/// The [`HOOKS`] bit set while the boundary-call recorder runs, which needs every closure call
/// to pass its hook on the general path. It is set only together with [`SUMMARY`]. Code that
/// runs without a call entry reads it on its own: closures that run natively without a frame,
/// to take their frame path instead, and the places that evaluate code in an environment (a
/// promise being forced, `eval`, and the C API's evaluators).
pub const CAPTURE: u8 = 32;
static CURRENT: AtomicU32 = AtomicU32::new(0);
static COUNTERS: [AtomicU64; Counter::ALL.len()] =
    [const { AtomicU64::new(0) }; Counter::ALL.len()];
static BUILTINS: [AtomicU64; BUILTIN_SLOTS] = [const { AtomicU64::new(0) }; BUILTIN_SLOTS];
static LABELS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PACKAGE_FALLBACKS: [AtomicU64; REASON_SLOTS] = [const { AtomicU64::new(0) }; REASON_SLOTS];
static REIFICATIONS: [AtomicU64; REASON_SLOTS] = [const { AtomicU64::new(0) }; REASON_SLOTS];
static JIT_REFUSALS: [AtomicU64; LABEL_SLOTS] = [const { AtomicU64::new(0) }; LABEL_SLOTS];
static GENERAL_ASSIGNMENTS: [AtomicU64; LABEL_SLOTS] = [const { AtomicU64::new(0) }; LABEL_SLOTS];
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

/// The hook bits that are set; zero while nothing observes or records calls.
#[inline(always)]
pub fn hooks() -> u8 {
    HOOKS.load(Ordering::Relaxed)
}

/// The set bits that need a call's general path; zero while a call may take its fast path,
/// whether or not a summary records it.
#[inline(always)]
pub fn observers() -> u8 {
    hooks() & !(SUMMARY | SAMPLE_TICK)
}

/// Whether a summary is collected and nothing needs a call's general path, so the call takes the
/// summarized variant of its fast path.
#[inline(always)]
pub fn summary_only() -> bool {
    let hooks = hooks();
    hooks != 0 && hooks & !(SUMMARY | SAMPLE_TICK) == 0
}

/// Whether a sample of the summary waits for the R thread's call stack.
#[inline(always)]
pub fn sample_ticked() -> bool {
    hooks() & SAMPLE_TICK != 0
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
    // In one order with the sample slots a tick announces (`SAMPLE_TICK`): the thread that
    // clears the bit then sees every sample queued before the bit was set.
    if on {
        HOOKS.fetch_or(bit, Ordering::SeqCst);
    } else {
        HOOKS.fetch_and(!bit, Ordering::SeqCst);
    }
}

/// The current activity word, for the sampling thread.
#[inline(always)]
pub fn current() -> u32 {
    CURRENT.load(Ordering::Relaxed)
}

/// Publishes `activity` without a scope, for a path that knows which activity follows it and
/// stores that one the same way. The caller has checked [`enabled`].
#[inline(always)]
pub fn publish(activity: Activity) {
    CURRENT.store(activity as u32, Ordering::Relaxed);
}

/// Publishes the activity word [`current`] answered before a [`publish`].
#[inline(always)]
pub fn restore(word: u32) {
    CURRENT.store(word, Ordering::Relaxed);
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

/// [`count`] where the caller has checked [`enabled`].
#[inline(always)]
pub fn record(counter: Counter) {
    bump(&COUNTERS[counter as usize]);
}

/// [`count`] for `events` events at once.
pub fn count_by(counter: Counter, events: u64) {
    if enabled() {
        let slot = &COUNTERS[counter as usize];
        slot.store(slot.load(Ordering::Relaxed) + events, Ordering::Relaxed);
    }
}

/// Counts a call of the builtin at `index` outside the inline fast paths. The caller has
/// checked [`enabled`].
#[inline(always)]
pub fn record_builtin(index: usize) {
    bump(&COUNTERS[Counter::BuiltinCalls as usize]);
    bump(&BUILTINS[index % BUILTIN_SLOTS]);
}

/// Publishes the builtin at `index` as the running activity and answers the word to
/// [`restore`] when it returns. The caller has checked [`enabled`].
#[inline(always)]
pub fn publish_builtin(index: usize) -> u32 {
    let running = CURRENT.load(Ordering::Relaxed);
    CURRENT.store(
        Activity::Builtin as u32 | ((index as u32) << 8),
        Ordering::Relaxed,
    );
    running
}

#[inline(always)]
fn bump(counter: &AtomicU64) {
    counter.store(counter.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

/// Counts a package call that took the generic call protocol, under the number of the reason.
pub fn count_package_fallback(reason: u32) {
    if let Some(slot) = PACKAGE_FALLBACKS.get(reason as usize) {
        bump(slot);
    }
}

/// The package calls [`count_package_fallback`] counted under each reason's number.
pub fn package_fallbacks() -> Vec<(u32, u64)> {
    by_reason(&PACKAGE_FALLBACKS)
}

/// Counts a slot frame turned into an environment, under the number of what asked for it.
pub fn count_reification(reason: u32) {
    if let Some(slot) = REIFICATIONS.get(reason as usize) {
        bump(slot);
    }
}

/// The slot frames [`count_reification`] counted under each reason's number.
pub fn reifications() -> Vec<(u32, u64)> {
    by_reason(&REIFICATIONS)
}

fn by_reason(slots: &[AtomicU64; REASON_SLOTS]) -> Vec<(u32, u64)> {
    slots
        .iter()
        .enumerate()
        .filter_map(|(reason, slot)| {
            let count = slot.load(Ordering::Relaxed);
            (count > 0).then_some((reason as u32, count))
        })
        .collect()
}

/// Counts a complex assignment that ran through the general routine, under the [`label`] of its
/// target's shape.
pub fn count_general_assignment(shape: u32) {
    if let Some(slot) = GENERAL_ASSIGNMENTS.get(shape as usize) {
        bump(slot);
    }
}

/// The assignments [`count_general_assignment`] counted under each shape, largest first.
pub fn general_assignments() -> Vec<(String, u64)> {
    by_label(&GENERAL_ASSIGNMENTS)
}

/// Counts a compile attempt tier 1 refused, under the [`label`] of the refusal's reason.
pub fn count_jit_refusal(reason: u32) {
    if let Some(slot) = JIT_REFUSALS.get(reason as usize) {
        bump(slot);
    }
}

/// The compile attempts [`count_jit_refusal`] counted under each reason, largest first.
pub fn jit_refusals() -> Vec<(String, u64)> {
    by_label(&JIT_REFUSALS)
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
        count_allocation(kind);
    }
}

#[inline(never)]
fn count_allocation(kind: u32) {
    let counter = &ALLOCATED[kind as usize % ALLOCATED.len()];
    let count = counter.load(Ordering::Relaxed) + 1;
    counter.store(count, Ordering::Relaxed);
    let period = ALLOCATION_PERIOD.load(Ordering::Relaxed);
    if period != 0 && count.is_multiple_of(period) {
        sample_allocation_site(kind);
    }
}

#[cold]
#[inline(never)]
fn sample_allocation_site(kind: u32) {
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
