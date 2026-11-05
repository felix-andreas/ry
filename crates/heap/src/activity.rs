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
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
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
}

impl Activity {
    pub const ALL: [Activity; 14] = [
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
        }
    }
}

/// Event counts the summary reports beside the time split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Counter {
    ClosureCalls,
    BuiltinCalls,
    PromisesCreated,
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
}

impl Counter {
    pub const ALL: [Counter; 15] = [
        Counter::ClosureCalls,
        Counter::BuiltinCalls,
        Counter::PromisesCreated,
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
    ];

    pub fn describe(self) -> &'static str {
        match self {
            Counter::ClosureCalls => "closure calls",
            Counter::BuiltinCalls => "builtin calls (outside inline fast paths)",
            Counter::PromisesCreated => "promises created",
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
        }
    }
}

/// The most builtins [`count_builtin`] keeps a count for; ry defines fewer than this.
pub const BUILTIN_SLOTS: usize = 4096;

static ENABLED: AtomicBool = AtomicBool::new(false);
static CURRENT: AtomicU32 = AtomicU32::new(0);
static COUNTERS: [AtomicU64; Counter::ALL.len()] = [const { AtomicU64::new(0) }; Counter::ALL.len()];
static BUILTINS: [AtomicU64; BUILTIN_SLOTS] = [const { AtomicU64::new(0) }; BUILTIN_SLOTS];
static LABELS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FRAME_OWNERS: [AtomicU32; FRAME_SLOTS] = [const { AtomicU32::new(0) }; FRAME_SLOTS];
static FRAME_NAMES: [AtomicUsize; FRAME_SLOTS] = [const { AtomicUsize::new(0) }; FRAME_SLOTS];
static FRAME_DEPTH: AtomicU32 = AtomicU32::new(0);

/// The deepest closure frame the sampler sees; a sample deeper than this is attributed to the
/// frame at this depth.
pub const FRAME_SLOTS: usize = 1024;

/// Whether a profile summary is being collected.
#[inline(always)]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Starts or stops publishing activities and counting events.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
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

pub fn counter(counter: Counter) -> u64 {
    COUNTERS[counter as usize].load(Ordering::Relaxed)
}

pub fn builtin_count(index: usize) -> u64 {
    BUILTINS.get(index).map_or(0, |slot| slot.load(Ordering::Relaxed))
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
/// never freed, or 0 for an anonymous function.
///
/// The sampler reads the stack without synchronizing with the R thread, so a sample taken during
/// a push or pop can see one frame too many or too few. That is noise of one sample, and it keeps
/// the push to three stores.
#[inline(always)]
pub fn push_frame(owner: u32, name: usize) -> FrameScope {
    let depth = FRAME_DEPTH.load(Ordering::Relaxed);
    if let (Some(owner_slot), Some(name_slot)) = (
        FRAME_OWNERS.get(depth as usize),
        FRAME_NAMES.get(depth as usize),
    ) {
        owner_slot.store(owner, Ordering::Relaxed);
        name_slot.store(name, Ordering::Relaxed);
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

/// Copies the published closure frames, outermost first, into `owners` and `names`.
pub fn read_frames(owners: &mut Vec<u32>, names: &mut Vec<usize>) {
    owners.clear();
    names.clear();
    let depth = (FRAME_DEPTH.load(Ordering::Acquire) as usize).min(FRAME_SLOTS);
    for index in 0..depth {
        owners.push(FRAME_OWNERS[index].load(Ordering::Relaxed));
        names.push(FRAME_NAMES[index].load(Ordering::Relaxed));
    }
}
