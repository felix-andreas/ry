//! ry's R object model and garbage collector.
//!
//! The collector is non-moving, because native code keeps raw pointers into objects, and
//! generational with sticky mark bits: an object that survives a collection keeps its mark and is
//! old from then on, so a minor collection marks only young objects and sweeps only the young
//! list. A write barrier on every store of a handle into an object records old objects that come
//! to point at young ones. Small objects live in size-segregated pages; large ones are allocated
//! one by one.
//!
//! The roots are the `PROTECT` stack, the precious multiset behind `R_PreserveObject`, the handle
//! stack that keeps every object native code receives alive until its foreign call returns, the
//! registered globals, every interned symbol, and any [`RootSource`] an interpreter or JIT
//! registers for its frames.

// Every accessor dereferences a raw handle, so the whole crate is an FFI-style unsafe layer and an
// `unsafe` block inside each `unsafe fn` would only add noise.
#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has the same contract, stated once on `object::Sexp`: each handle it
// takes must point at a live object of the kind the function expects.
#![allow(clippy::missing_safety_doc)]

pub mod arena;
pub mod env;
pub mod image;
pub use image::{Fixed, ImageError, Relocation};
pub mod large;
pub mod object;
pub mod records;
mod space;
pub mod strings;

use {
    crate::{
        env::Frame,
        large::{MAPPED_BYTES, MappedPool},
        object::{
            EnvironmentObject, NA_LOGICAL, REFCOUNT_MAX, Sexp, SexpType, Shape, Slot, SymbolObject,
            TripleObject, VECTOR_DATA_OFFSET, flag_bits, gc_bits, gp, object_size, slot_for,
        },
        records::{Layouts, slot_kind},
        space::{SIZE_CLASS_BY_WORDS, SLOT_SIZES, Space, Swept},
        strings::{Encoding, StringTable},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::{
        alloc::Layout,
        cell::Cell,
        ffi::c_void,
        fmt,
        io::Write,
        ptr::NonNull,
        rc::Rc,
        time::{Duration, Instant},
    },
};

/// The longest vector R allows, `R_XLEN_T_MAX`.
pub const MAX_VECTOR_LENGTH: isize = 4_503_599_627_370_496;
/// GNU R's default `PROTECT` stack size, `R_PPSSIZE`.
pub const DEFAULT_PROTECT_LIMIT: usize = 50_000;
/// The bytes one pairlist cell, promise, closure or binding cell takes, for sizing a
/// [`Heap::reserve`].
pub const TRIPLE_BYTES: usize = size_of::<TripleObject>();
/// The bytes one environment takes, for sizing a [`Heap::reserve`]. Its frame's table is
/// allocated outside the heap and not counted.
pub const ENVIRONMENT_BYTES: usize = size_of::<EnvironmentObject>();

pub struct Heap {
    space: Space,
    /// The largest single allocation the heap attempts (`memory_ceiling`).
    allocation_ceiling: usize,
    large: FxHashSet<Sexp>,
    /// Freed mappings of large vectors kept for the next one.
    mapped: MappedPool,
    /// Large objects allocated since the last collection, which a minor collection sweeps.
    young_large: Vec<Sexp>,
    /// Whether the collections running are the deeper ones of an escalation.
    escalating: bool,
    /// Bytes of large vectors allocated since the last major collection.
    large_allocated_since_major: usize,
    remembered: Vec<Sexp>,
    /// While a deeper collection traces a tenured object: whether one of its children seen so
    /// far is younger, which puts the object in the remembered set.
    tracing_tenured: bool,
    younger_child: bool,
    /// The vector cells of the tenured objects, as of the last intermediate or major collection,
    /// to which an intermediate one adds what it tenures.
    tenured_vector_cells: u64,
    escaped: Vec<Sexp>,
    /// The elements each escaped list held when it escaped. Native stores through the pointer
    /// bypass the counts, so retiring the escape recounts the list from this snapshot.
    escaped_lists: Vec<(Sexp, Vec<Sexp>)>,
    /// Objects a store left without a counted reference, which the next reconcile frees if no
    /// root holds them either (`Heap::reconcile`).
    zero_count: Vec<Sexp>,
    /// Frames of returned calls that the next reconcile may release ([`Heap::release_frame`]).
    /// They are not roots: a collection drops the ones it frees.
    returned: Vec<Returned>,
    /// The table size that runs the next reconcile. Entries a root holds stay in the table, so
    /// the trigger counts the entries added since the last one.
    reconcile_at: usize,
    /// The large-vector bytes the entries added since the last reconcile would free, as
    /// [`Heap::note_zero`] estimates them.
    zero_bytes: usize,
    /// Live objects whose fields do not count (`flag_bits::UNTRACKED`): their fields are roots
    /// for a reconcile.
    untracked: Vec<Sexp>,
    /// Under [`GcPolicy::verify`], what a reconcile would have freed, checked against the full
    /// collection that runs instead.
    expect_dead: Vec<Sexp>,
    weak_refs: Vec<Sexp>,
    /// Weak references whose keys died, kept alive with their keys until their finalizers run.
    ready_to_finalize: Vec<Sexp>,
    /// The weak-pair lists the current collection reached, cleared of dead keys after marking.
    weak_pairs: Vec<Sexp>,
    mark_stack: Vec<Sexp>,
    strings: StringTable,
    symbols: FxHashMap<Sexp, Sexp>,
    /// Symbols interned and objects preserved since the last collection. A minor collection marks
    /// only these: every older entry of the symbol table and the precious set is already marked,
    /// and walking those tables touched one cold header per entry on every minor collection.
    new_roots: Vec<Sexp>,
    /// The tag and layout interners of ry's records, tuples and tags.
    pub layouts: Layouts,
    pub roots: Roots,
    sources: Vec<(u64, Rc<dyn RootSource>)>,
    next_source_id: u64,
    policy: GcPolicy,
    /// Bytes of small objects allocated since the last collection.
    young_bytes: usize,
    /// The `young_bytes` past which a small allocation takes the slow path, which runs a minor
    /// collection when one is due. It is derived from the policy and `old_small_bytes`, and stored
    /// because the allocation fast path then needs one compare against it; it is zero under
    /// collection stress, so every allocation takes the slow path. It changes only where its
    /// inputs do: a collection and [`Heap::update_policy`].
    small_trigger: usize,
    /// Bytes of large vectors allocated since the last collection, kept apart from
    /// `young_bytes` because they have their own budget.
    young_large_bytes: usize,
    /// Bytes of old small objects and of old large vectors, apart because the nursery's budget
    /// scales with the first alone.
    old_small_bytes: usize,
    old_large_bytes: usize,
    stats: GcStats,
    inhibit: u32,
    stress_counter: u64,
    known: WellKnown,
    /// Bumped whenever the set of bindings outside function frames changes (a binding created or
    /// removed, an enclosure replaced), so a cached lookup that resolved through those
    /// environments knows it is still valid when the epoch is unchanged.
    binding_epoch: u64,
    /// Bumped whenever an enclosure is replaced. A symbol's lookup stamp adds it to the symbol's
    /// own epoch, so a cache keyed by symbol sees both kinds of change.
    enclosure_epoch: u64,
    /// `Rprofmem()`'s reporter and its threshold in bytes. A vector whose reported size exceeds
    /// the threshold is passed to the reporter; `usize::MAX` turns reporting off, so the check
    /// on every allocation is one compare.
    allocation_reporter: Option<fn(usize) -> bool>,
    report_threshold: usize,
    /// The file `RY_GC_LOG` names, which receives one line per collection: its kind, its
    /// elapsed time in nanoseconds and the bytes of small objects and large vectors it found
    /// young and the old generation's bytes after it. Measuring pause distributions over a whole test suite needs every pause, which
    /// `gc.time()`'s totals cannot give.
    pause_log: Option<std::fs::File>,
}

/// A returned call's frame that waits for a reconcile to be released ([`Heap::release_frame`]).
struct Returned {
    env: Sexp,
    /// The closures the call created in `env`, other than its result.
    closures: SmallVec<[Sexp; 2]>,
    /// How many of those a collection freed, whose references to `env` stay counted.
    swept: u32,
}

/// A formal bound by [`Heap::define_formals`]. `missing` marks a formal the call did not supply,
/// for `missing()`.
#[derive(Clone, Copy)]
pub struct Formal {
    pub symbol: Sexp,
    pub value: Sexp,
    pub missing: bool,
}

/// The objects every R session has, created with the heap.
#[derive(Clone, Copy)]
pub struct WellKnown {
    pub nil: Sexp,
    pub unbound: Sexp,
    pub missing_arg: Sexp,
    pub na_string: Sexp,
    pub blank_string: Sexp,
    pub blank_scalar_string: Sexp,
    pub empty_env: Sexp,
    pub base_env: Sexp,
    pub base_namespace: Sexp,
    pub global_env: Sexp,
    pub namespace_registry: Sexp,
    pub true_value: Sexp,
    pub false_value: Sexp,
    pub logical_na_value: Sexp,
}

pub struct Roots {
    pub protect: ProtectStack,
    precious: FxHashMap<Sexp, u32>,
    /// Objects kept alive for the rest of the current scope. A foreign call truncates it on
    /// return, which gives native code its handle scope.
    handles: RootStack,
    /// What the C API has handed to native code in the native calls in progress. Each call owns
    /// the entries above the base it started at, and only its most recent ones stay rooted (see
    /// [`Heap::hand_out`]), so a loop's dropped temporaries can be collected before it returns.
    handouts: RootStack,
    globals: RootStack,
}

/// How many of a native call's most recent handouts stay rooted at least. Code that is correct
/// under GNU R protects anything it keeps across an allocation, so as long as the C API allocates
/// only where GNU R does, it never needs an older handout rooted; the window forgives a forgotten
/// `PROTECT` on a recent temporary.
pub const HANDOUT_WINDOW: usize = 1024;

/// The `PROTECT` stack. `base` is where the innermost native call's protections start, so
/// native code cannot unprotect what its caller protected.
pub struct ProtectStack {
    stack: RootStack,
    pub limit: usize,
    base: usize,
}

/// A stack of roots that knows how much of itself the last collection scanned. Every entry
/// below `scanned` has been there, unchanged, since that collection marked it, and a mark stays
/// until the next major collection, so a minor collection skips those entries: marking them
/// again would find them marked and do nothing. Without this, a long handle scope (unserializing
/// a large object roots every cell it reads) made every minor collection rescan the whole scope.
#[derive(Default)]
struct RootStack {
    entries: Vec<Sexp>,
    scanned: usize,
}

/// Something that holds handles the collector cannot see otherwise, such as an interpreter's
/// frame stack or a JIT's stack maps.
pub trait RootSource {
    fn trace(&self, marker: &mut Marker<'_>);
}

pub struct Marker<'a> {
    stack: &'a mut Vec<Sexp>,
    /// `None` while the zero-count table is reconciled: every object a source names is then
    /// collected into `stack` as a root, without marking or tracing.
    space: Option<&'a mut Space>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Collection {
    /// Traces from the roots and the remembered set and frees young objects only.
    Minor,
    /// Unmarks the younger old generation, traces everything outside the oldest one, and frees
    /// the dead objects of both younger generations; survivors join the oldest. Tenured objects
    /// are neither traced nor freed, which is what makes it cheaper than a major collection.
    Intermediate,
    Major,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GcStress {
    Off,
    /// Collect before every allocation. Every eighth collection is a major one while the heap
    /// is small; past that, one per sixteenth of the live objects.
    EveryAllocation,
}

/// When `RY_GC_STRESS` turns collection stress on for a whole run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GcStressStart {
    /// `RY_GC_STRESS=startup`: from the heap's first allocation, base R's loading included.
    Startup,
    /// Any other non-empty value but `0`: once the session has started, so a script runs under
    /// stress without paying for base R's loading under it. The host that runs the script
    /// switches it on.
    Script,
}

/// Opens the collection log `RY_GC_LOG` names for appending, so the processes of one run,
/// such as a test suite's workers, share a file.
fn open_pause_log() -> Option<std::fs::File> {
    let path = std::env::var_os("RY_GC_LOG")?;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => Some(file),
        Err(error) => panic!(
            "cannot open the collection log {}: {error}",
            path.to_string_lossy()
        ),
    }
}

/// The collection stress `RY_GC_STRESS` asks for, if any.
pub fn gc_stress_requested() -> Option<GcStressStart> {
    match std::env::var("RY_GC_STRESS").as_deref() {
        Err(_) | Ok("" | "0") => None,
        Ok("startup") => Some(GcStressStart::Startup),
        Ok(_) => Some(GcStressStart::Script),
    }
}

pub struct GcPolicy {
    /// Bytes of young small objects that trigger a minor collection while the old generation is
    /// small; past twice this size, half the old generation's bytes trigger one instead.
    pub young_limit: usize,
    /// Bytes of young large vectors that trigger a minor collection, which also grows to half
    /// the old generation.
    pub young_large_limit: usize,
    /// GNU R's `R_VSize` in bytes, over large vectors only: a large vector that would take them
    /// past it runs a collection, and a deeper one when the old large vectors alone would still
    /// pass it. It is adjusted after each major collection by [`HeapGrowth`], and never drops
    /// below `min_heap_limit`.
    pub heap_limit: usize,
    pub min_heap_limit: usize,
    /// GNU R's `R_NSize` in bytes, over the small-object pages: a minor collection that leaves
    /// the old small objects too close to it runs a deeper one. It moves like `heap_limit`.
    /// Small objects get a limit of their own because base R's alone take about 30 MB, which
    /// under one limit for both left large temporaries a few megabytes before every minor
    /// collection escalated to a major one.
    pub node_limit: usize,
    pub min_node_limit: usize,
    pub growth: HeapGrowth,
    pub stress: GcStress,
    /// Checks the write barrier's invariant before every minor collection, and replaces each
    /// reconcile's frees with a full collection that panics if it reaches one of them
    /// (`RY_GC_VERIFY`).
    pub verify: bool,
    /// Adds each collection's processor and elapsed time to the statistics, which `gc.time()`
    /// turns on.
    pub timing: bool,
    /// Receives [`Heap::report`]'s text after each collection, which `gcinfo(TRUE)` asks for.
    /// It runs while collection is still inhibited, so an allocation it makes cannot collect
    /// under an allocation that was in progress.
    pub reporter: Option<unsafe fn(&str)>,
}

/// How [`GcPolicy::heap_limit`] follows the live data, with GNU R's fractions for its vector
/// heap. After a major collection the limit rises to what is needed (the survivors plus the
/// allocation that ran the collection); when that need fills more than `grow_fraction` of it,
/// it grows by `grow_increment` of itself, and when it fills less than `shrink_fraction`, it
/// shrinks by `shrink_increment` of itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeapGrowth {
    pub grow_fraction: f64,
    pub grow_increment: f64,
    pub shrink_fraction: f64,
    pub shrink_increment: f64,
}

impl HeapGrowth {
    pub const DEFAULT: HeapGrowth = HeapGrowth {
        grow_fraction: 0.70,
        grow_increment: 0.20,
        shrink_fraction: 0.30,
        shrink_increment: 0.20,
    };

    /// GNU R's settings from its environment variables: `R_GC_MEM_GROW` picks a preset (0
    /// never grows by more than the need, 1 is the default, 2 grows by 30%, 3 by 40% from half
    /// full), then `R_GC_GROWFRAC` (0.35 to 0.75), `R_GC_GROWINCRFRAC` and
    /// `R_GC_VGROWINCRFRAC` (0.05 to 0.80) override single fractions. A value outside its
    /// range is ignored, as in GNU R.
    pub fn from_environment(variable: impl Fn(&str) -> Option<String>) -> HeapGrowth {
        let number = |name: &str| variable(name).and_then(|value| value.trim().parse::<f64>().ok());
        let mut growth = HeapGrowth::DEFAULT;
        match number("R_GC_MEM_GROW").map(|level| level.trunc() as i64) {
            Some(0) => growth.grow_increment = 0.0,
            Some(2) => growth.grow_increment = 0.3,
            Some(3) => {
                growth.grow_increment = 0.4;
                growth.grow_fraction = 0.5;
            }
            _ => {}
        }
        if let Some(fraction) =
            number("R_GC_GROWFRAC").filter(|value| (0.35..=0.75).contains(value))
        {
            growth.grow_fraction = fraction;
        }
        for name in ["R_GC_GROWINCRFRAC", "R_GC_VGROWINCRFRAC"] {
            if let Some(fraction) = number(name).filter(|value| (0.05..=0.80).contains(value)) {
                growth.grow_increment = fraction;
            }
        }
        growth
    }

    /// The limit after a major collection that left `needed` bytes to hold. A `thrashing`
    /// heap grows as if the need filled more than the grow fraction.
    pub fn next_limit(
        &self,
        limit: usize,
        needed: usize,
        minimum: usize,
        thrashing: bool,
    ) -> usize {
        // A minor collection escalates to a major one when less than a fifth of the smallest
        // limit is left (`Heap::escalate_if_full`), so that fifth is part of the need, as in GNU
        // R's `AdjustHeapSize`. Leaving it out lets a live set just under the grow fraction
        // keep the limit where every later minor collection escalates.
        let needed = needed + minimum / 5;
        let mut limit = limit.max(needed) as f64;
        let occupancy = needed as f64 / limit;
        if thrashing || occupancy > self.grow_fraction {
            // GNU R's smallest step, 80,000 cells of 8 bytes, keeps a zero increment from
            // collecting on every allocation once the need reaches the limit.
            limit += GROW_STEP_MIN + self.grow_increment * limit;
        } else if occupancy < self.shrink_fraction {
            limit -= self.shrink_increment * limit;
        }
        (limit as usize).max(needed).max(minimum)
    }
}

const GROW_STEP_MIN: f64 = 80_000.0 * 8.0;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GcStats {
    pub minor_collections: u64,
    pub intermediate_collections: u64,
    pub major_collections: u64,
    pub objects_freed: u64,
    /// Large vectors freed by reconciling the zero-count table, apart from a collection.
    pub vectors_released: u64,
    pub live_objects: u64,
    pub live_bytes: u64,
    pub pages: u64,
    pub large_objects: u64,
    /// The live objects' vector data in 8-byte cells, GNU R's `Vcells`. A major collection
    /// counts it afresh, an intermediate one adds what it marked to the tenured objects' cells,
    /// and a minor one adds what it marked to the total, since each starts with exactly the
    /// objects it cannot free still marked.
    pub live_vector_cells: u64,
    /// The most objects and vector cells live after any collection since the last reset.
    pub max_live_objects: u64,
    pub max_vector_cells: u64,
    /// Processor time in user and system mode and elapsed time spent collecting while
    /// [`GcPolicy::timing`] was on.
    pub user_time: Duration,
    pub system_time: Duration,
    pub elapsed_time: Duration,
}

impl GcStats {
    pub fn collections(&self) -> u64 {
        self.minor_collections + self.intermediate_collections + self.major_collections
    }
}

/// What `gc()` reports, in GNU R's units: objects for its `Ncells` and 8-byte cells of vector
/// data for its `Vcells`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub objects: u64,
    pub vector_cells: u64,
    /// Megabytes held by the objects themselves, apart from their vector data.
    pub object_megabytes: f64,
    pub vector_megabytes: f64,
    /// The usage at which the next major collection runs. The heap triggers one on its total
    /// bytes, so each count is scaled by the ratio of that limit to the bytes in use.
    pub object_trigger: u64,
    pub vector_trigger: u64,
    pub object_trigger_megabytes: f64,
    pub vector_trigger_megabytes: f64,
    pub max_objects: u64,
    pub max_vector_cells: u64,
    pub max_object_megabytes: f64,
    pub max_vector_megabytes: f64,
}

/// GNU R's `MAXIDSIZE`: the longest symbol name, in bytes.
pub const MAX_SYMBOL_BYTES: usize = 10_000;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HeapError {
    CannotAllocate { bytes: usize },
    NegativeLength,
    TooLong,
    InvalidVectorType { kind: SexpType, length: isize },
    ProtectOverflow,
    UnprotectUnderflow { available: usize, requested: usize },
    UnprotectPointerNotFound,
    ReprotectIndex { available: usize, index: usize },
    EmbeddedNul { text: String },
    StringTooLong,
    SymbolTooLong,
    EmptySymbol,
    EmptyEnvironment,
    LockedBinding { name: String },
    LockedEnvironment,
    LockedEnvironmentRemove,
    NotResizable,
    ResizeBeyondCapacity { capacity: isize, length: isize },
    IncompatibleRetype { from: SexpType, to: SexpType },
}

impl fmt::Display for HeapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeapError::CannotAllocate { bytes } => {
                let bytes = *bytes as f64;
                let (amount, unit) = if bytes >= 1024.0 * 1024.0 * 1024.0 {
                    (bytes / (1024.0 * 1024.0 * 1024.0), "Gb")
                } else if bytes >= 1024.0 * 1024.0 {
                    (bytes / (1024.0 * 1024.0), "Mb")
                } else {
                    (bytes / 1024.0, "Kb")
                };
                write!(
                    formatter,
                    "cannot allocate vector of size {amount:.1} {unit}"
                )
            }
            HeapError::NegativeLength => {
                write!(formatter, "negative length vectors are not allowed")
            }
            HeapError::TooLong => write!(formatter, "vector is too large"),
            HeapError::InvalidVectorType { kind, length } => write!(
                formatter,
                "invalid type/length ({}/{length}) in vector allocation",
                kind.name()
            ),
            HeapError::ProtectOverflow => write!(formatter, "protect(): protection stack overflow"),
            HeapError::UnprotectUnderflow {
                available,
                requested,
            } => write!(
                formatter,
                "unprotect(): only {available} protected items, can't unprotect {requested}"
            ),
            HeapError::UnprotectPointerNotFound => {
                write!(formatter, "unprotect_ptr: pointer not found")
            }
            HeapError::ReprotectIndex { available, index } => write!(
                formatter,
                "R_Reprotect: only {available} protected items, can't reprotect index {index}"
            ),
            HeapError::EmbeddedNul { text } => {
                write!(formatter, "embedded nul in string: '{text}'")
            }
            HeapError::StringTooLong => {
                write!(formatter, "R character strings are limited to 2^31-1 bytes")
            }
            HeapError::SymbolTooLong => {
                write!(formatter, "variable names are limited to 10000 bytes")
            }
            HeapError::EmptySymbol => write!(formatter, "attempt to use zero-length variable name"),
            HeapError::EmptyEnvironment => {
                write!(formatter, "cannot assign values in the empty environment")
            }
            HeapError::LockedBinding { name } => {
                write!(
                    formatter,
                    "cannot change value of locked binding for '{name}'"
                )
            }
            HeapError::LockedEnvironment => {
                write!(formatter, "cannot add bindings to a locked environment")
            }
            HeapError::LockedEnvironmentRemove => {
                write!(
                    formatter,
                    "cannot remove bindings from a locked environment"
                )
            }
            HeapError::NotResizable => write!(formatter, "the vector is not resizable"),
            HeapError::IncompatibleRetype { from, to } => write!(
                formatter,
                "cannot change the type of a '{}' object to '{}'",
                from.name(),
                to.name()
            ),
            HeapError::ResizeBeyondCapacity { capacity, length } => write!(
                formatter,
                "a resizable vector of capacity {capacity} cannot grow to length {length}"
            ),
        }
    }
}

impl std::error::Error for HeapError {}

pub type HeapResult<T> = Result<T, HeapError>;

const LARGE_CLASS: u8 = u8::MAX;

impl Heap {
    pub fn new() -> Heap {
        Heap::with_policy(GcPolicy::default())
    }

    pub fn with_policy(policy: GcPolicy) -> Heap {
        let mut heap = Heap {
            allocation_ceiling: memory_ceiling(),
            space: Space::new(),
            large: FxHashSet::default(),
            mapped: MappedPool::new(policy.heap_limit),
            young_large: Vec::new(),
            escalating: false,
            large_allocated_since_major: 0,
            remembered: Vec::new(),
            tracing_tenured: false,
            younger_child: false,
            tenured_vector_cells: 0,
            escaped: Vec::new(),
            escaped_lists: Vec::new(),
            zero_count: Vec::new(),
            returned: Vec::new(),
            reconcile_at: RECONCILE_ENTRIES,
            zero_bytes: 0,
            untracked: Vec::new(),
            expect_dead: Vec::new(),
            weak_refs: Vec::new(),
            ready_to_finalize: Vec::new(),
            weak_pairs: Vec::new(),
            mark_stack: Vec::new(),
            binding_epoch: 0,
            enclosure_epoch: 0,
            allocation_reporter: None,
            report_threshold: usize::MAX,
            strings: StringTable::default(),
            symbols: FxHashMap::default(),
            new_roots: Vec::new(),
            layouts: Layouts::default(),
            roots: Roots {
                protect: ProtectStack {
                    stack: RootStack::default(),
                    limit: DEFAULT_PROTECT_LIMIT,
                    base: 0,
                },
                precious: FxHashMap::default(),
                handles: RootStack::default(),
                handouts: RootStack::default(),
                globals: RootStack::default(),
            },
            sources: Vec::new(),
            next_source_id: 0,
            policy,
            young_bytes: 0,
            small_trigger: 0,
            young_large_bytes: 0,
            old_small_bytes: 0,
            old_large_bytes: 0,
            stats: GcStats::default(),
            inhibit: 1,
            stress_counter: 0,
            pause_log: open_pause_log(),
            // Overwritten below, before anything reads it.
            known: WellKnown::placeholder(),
        };
        // SAFETY: the heap is fresh, and collection stays inhibited until every well-known object
        // is a registered root.
        unsafe { heap.create_well_known() };
        heap.inhibit = 0;
        heap.refresh_small_trigger();
        heap
    }

    pub fn known(&self) -> &WellKnown {
        &self.known
    }

    pub fn nil(&self) -> Sexp {
        self.known.nil
    }

    pub fn stats(&self) -> GcStats {
        self.stats
    }

    /// The bytes of every object allocated and not yet freed, garbage included: what the heap
    /// occupies now, which the two collection limits bound together.
    pub fn allocated_bytes(&self) -> usize {
        self.old_bytes() + self.young_bytes + self.young_large_bytes
    }

    fn old_bytes(&self) -> usize {
        self.old_small_bytes + self.old_large_bytes
    }

    /// The live objects as of the last collection, in `gc()`'s units.
    pub fn usage(&self) -> Usage {
        const MEGABYTE: f64 = 1024.0 * 1024.0;
        let stats = &self.stats;
        let vector_bytes = stats.live_vector_cells * 8;
        let object_bytes = stats.live_bytes.saturating_sub(vector_bytes);
        let limit = self.policy.heap_limit + self.policy.node_limit;
        let scale = limit as f64 / (stats.live_bytes.max(1) as f64);
        let scale = scale.max(1.0);
        let megabytes = |bytes: f64| (bytes / MEGABYTE * 10.0).ceil() / 10.0;
        // Each object's share of the bytes beside its vector data, for the peak's megabytes.
        let per_object = object_bytes as f64 / stats.live_objects.max(1) as f64;
        Usage {
            objects: stats.live_objects,
            vector_cells: stats.live_vector_cells,
            object_megabytes: megabytes(object_bytes as f64),
            vector_megabytes: megabytes(vector_bytes as f64),
            object_trigger: (stats.live_objects as f64 * scale).ceil() as u64,
            vector_trigger: (stats.live_vector_cells as f64 * scale).ceil() as u64,
            object_trigger_megabytes: megabytes(object_bytes as f64 * scale),
            vector_trigger_megabytes: megabytes(vector_bytes as f64 * scale),
            max_objects: stats.max_live_objects,
            max_vector_cells: stats.max_vector_cells,
            max_object_megabytes: megabytes(stats.max_live_objects as f64 * per_object),
            max_vector_megabytes: megabytes(stats.max_vector_cells as f64 * 8.0),
        }
    }

    /// Sets the peak usage to the current one, as `gc(reset = TRUE)` does.
    pub fn reset_max_usage(&mut self) {
        self.stats.max_live_objects = self.stats.live_objects;
        self.stats.max_vector_cells = self.stats.live_vector_cells;
    }

    /// The report `gcinfo(TRUE)` prints after a collection, in GNU R's wording: the number of
    /// collections, split into minor and major ones (GNU R's levels 0 and 2; ry has no level
    /// 1), the level of the last one, and the megabytes in use against the next trigger.
    pub fn report(&self, last: Collection) -> String {
        let stats = &self.stats;
        let usage = self.usage();
        let percent = |used: f64, trigger: f64| {
            if trigger > 0.0 {
                (100.0 * used / trigger).round() as u64
            } else {
                0
            }
        };
        format!(
            "Garbage collection {} = {}+{}+{} (level {}) ... \n\
             {:.1} Mbytes of cons cells used ({}%)\n\
             {:.1} Mbytes of vectors used ({}%)\n",
            stats.collections(),
            stats.minor_collections,
            stats.intermediate_collections,
            stats.major_collections,
            match last {
                Collection::Minor => 0,
                Collection::Intermediate => 1,
                Collection::Major => 2,
            },
            usage.object_megabytes,
            percent(usage.objects as f64, usage.object_trigger as f64),
            usage.vector_megabytes,
            percent(usage.vector_cells as f64, usage.vector_trigger as f64),
        )
    }

    pub fn policy(&self) -> &GcPolicy {
        &self.policy
    }

    /// Changes the collection policy. The allocation fast path reads a budget derived from it,
    /// which this recomputes, so the policy has no other mutable access.
    pub fn update_policy(&mut self, change: impl FnOnce(&mut GcPolicy)) {
        change(&mut self.policy);
        self.refresh_small_trigger();
    }

    /// Recomputes [`Heap::small_trigger`] from the policy and the old generation's size.
    fn refresh_small_trigger(&mut self) {
        // Like GNU R's trigger for cons cells, the budget grows with the old small objects: a
        // fixed one would collect every few allocations in a session that loaded many packages,
        // rescanning the young objects each time. Old large vectors do not count, as GNU R's
        // cell trigger ignores vector memory, because a session holding a few big vectors would
        // otherwise let as many bytes of small garbage pile up.
        self.small_trigger = match self.policy.stress {
            GcStress::EveryAllocation => 0,
            GcStress::Off => self.policy.young_limit.max(self.old_small_bytes / 2),
        };
    }

    /// Suspends collection until the matching [`Heap::allow_collection`], for code that builds
    /// an object graph it cannot root piecewise.
    pub fn inhibit_collection(&mut self) {
        self.inhibit += 1;
    }

    pub fn allow_collection(&mut self) {
        self.inhibit = self.inhibit.saturating_sub(1);
    }

    // Allocation.

    /// A vector of `length` elements. Atomic elements start zeroed, character elements as `""`
    /// and list elements as `NULL`, as in GNU R.
    /// A vector of plain numbers whose elements are not initialized, for a kernel that writes
    /// every element before anything reads the vector. Skipping the zeroing matters for the
    /// large results of whole-vector arithmetic.
    ///
    /// # Safety
    /// The caller must write every element before the vector is read.
    pub unsafe fn alloc_vector_uninit(
        &mut self,
        kind: SexpType,
        length: usize,
    ) -> HeapResult<Sexp> {
        if !matches!(
            kind,
            SexpType::Logical
                | SexpType::Integer
                | SexpType::Real
                | SexpType::I64
                | SexpType::Complex
                | SexpType::Raw
        ) {
            return self.alloc_vector(kind, length as isize);
        }
        if length > MAX_VECTOR_LENGTH as usize {
            return Err(HeapError::TooLong);
        }
        let size = object_size(kind, length).ok_or(HeapError::TooLong)?;
        let object = self.allocate_with(kind, size, &[], VECTOR_DATA_OFFSET)?;
        object.set_length_and_capacity(length as isize, length as isize);
        self.report_allocation(kind, length);
        Ok(object)
    }

    /// Reports every vector allocation whose size, as GNU R counts it, exceeds `threshold`
    /// bytes to `reporter`, or stops reporting when `reporter` is `None`.
    pub fn report_allocations(&mut self, reporter: Option<fn(usize) -> bool>, threshold: usize) {
        self.report_threshold = if reporter.is_some() {
            threshold
        } else {
            usize::MAX
        };
        self.allocation_reporter = reporter;
    }

    /// Replaces the reporting threshold and returns the old one, so a caller can hide its own
    /// allocations by setting `usize::MAX` and restoring the old threshold afterwards.
    pub fn swap_report_threshold(&mut self, threshold: usize) -> usize {
        if self.allocation_reporter.is_none() {
            return usize::MAX;
        }
        std::mem::replace(&mut self.report_threshold, threshold)
    }

    /// GNU R allocates vectors of up to 128 data bytes from pages of small cells and reports
    /// only the larger ones, each as its data rounded up to 8 bytes plus a 48-byte header. The
    /// reporter returns false to stop reporting, because it runs while the heap is borrowed and
    /// so cannot turn reporting off through the heap itself.
    #[inline]
    fn report_allocation(&mut self, kind: SexpType, length: usize) {
        if self.report_threshold == usize::MAX {
            return;
        }
        let element = match kind {
            SexpType::Raw => 1,
            SexpType::Logical | SexpType::Integer => 4,
            SexpType::Complex => 16,
            _ => 8,
        };
        let data = length.saturating_mul(element).div_ceil(8).saturating_mul(8);
        let size = data.saturating_add(48);
        if data > 128
            && size > self.report_threshold
            && let Some(reporter) = self.allocation_reporter
            && !reporter(size)
        {
            self.report_allocations(None, 0);
        }
    }

    pub unsafe fn alloc_vector(&mut self, kind: SexpType, length: isize) -> HeapResult<Sexp> {
        if !kind.is_vector() {
            return Err(HeapError::InvalidVectorType { kind, length });
        }
        if length < 0 {
            return Err(HeapError::NegativeLength);
        }
        if length > MAX_VECTOR_LENGTH {
            return Err(HeapError::TooLong);
        }
        let size = object_size(kind, length as usize).ok_or(HeapError::TooLong)?;
        // Vectors of handles are filled below, so only the header needs zeroing first.
        let zeroed = if matches!(
            kind,
            SexpType::String | SexpType::List | SexpType::Expression
        ) {
            VECTOR_DATA_OFFSET
        } else {
            size
        };
        let object = self.allocate_with(kind, size, &[], zeroed)?;
        object.set_length_and_capacity(length, length);
        match kind {
            SexpType::String => {
                let blank = self.known.blank_string;
                object.slice_mut::<Sexp>().fill(blank);
            }
            SexpType::List | SexpType::Expression => {
                let nil = self.known.nil;
                object.slice_mut::<Sexp>().fill(nil);
            }
            _ => {}
        }
        self.report_allocation(kind, length as usize);
        Ok(object)
    }

    /// `R_allocResizableVector`: a vector that `resize_vector` can shrink and regrow up to
    /// `capacity` without copying.
    pub unsafe fn alloc_resizable_vector(
        &mut self,
        kind: SexpType,
        capacity: isize,
    ) -> HeapResult<Sexp> {
        let object = self.alloc_vector(kind, capacity)?;
        object.set_flag(flag_bits::RESIZABLE, true);
        Ok(object)
    }

    /// `R_resizeVector`. Growing exposes elements initialized to the vector's empty value.
    pub unsafe fn resize_vector(&mut self, vector: Sexp, length: isize) -> HeapResult<()> {
        if vector.flags() & flag_bits::RESIZABLE == 0 {
            return Err(HeapError::NotResizable);
        }
        let capacity = vector.capacity();
        if length < 0 {
            return Err(HeapError::NegativeLength);
        }
        if length > capacity {
            return Err(HeapError::ResizeBeyondCapacity { capacity, length });
        }
        let old = vector.length();
        if length > old {
            let element = vector.kind().element_size().unwrap_or(0);
            match vector.kind() {
                SexpType::String | SexpType::List | SexpType::Expression => {
                    let fill = if vector.kind() == SexpType::String {
                        self.known.blank_string
                    } else {
                        self.known.nil
                    };
                    for index in old..length {
                        vector.set_element_unbarriered(index as usize, fill);
                    }
                }
                _ => std::ptr::write_bytes(
                    vector.data().add(old as usize * element),
                    0,
                    (length - old) as usize * element,
                ),
            }
        } else if matches!(
            vector.kind(),
            SexpType::String | SexpType::List | SexpType::Expression
        ) {
            for index in length..old {
                let dropped = vector.element(index as usize);
                vector.set_element_unbarriered(index as usize, self.known.nil);
                if dropped.release_reference() {
                    self.note_zero(dropped);
                }
            }
        }
        vector.set_length(length);
        Ok(())
    }

    /// Lets `resize_vector` change the length of a vector allocated with a fixed length, within
    /// the length it was allocated with. GNU R before 4.6 let native code shrink any vector this
    /// way (`SETLENGTH`), and binaries built then still do.
    pub unsafe fn mark_resizable(&mut self, vector: Sexp) {
        vector.set_flag(flag_bits::RESIZABLE, true);
    }

    /// Makes `list`, of even length, hold `(key, value)` pairs weakly: a pair whose key is
    /// reachable only through the list is cleared at the next collection. An inline cache uses
    /// this so that it never keeps the environment it was filled from alive.
    pub unsafe fn mark_weak_pairs(&mut self, list: Sexp) {
        list.set_flag(flag_bits::WEAK_PAIRS, true);
    }

    /// Changes an object's type to another of the same shape (`SET_TYPEOF`): pairlist, call and
    /// dots cells among themselves, integer and logical vectors, lists and expression vectors.
    pub unsafe fn retype(&mut self, object: Sexp, kind: SexpType) -> HeapResult<()> {
        use SexpType::*;
        let compatible = |a: SexpType, b: SexpType| {
            matches!(
                (a, b),
                (Pairlist | Language | Dots, Pairlist | Language | Dots)
                    | (Integer | Logical, Integer | Logical)
                    | (List | Expression, List | Expression)
            )
        };
        if object.kind() == kind {
            return Ok(());
        }
        if object.is_altrep() || !compatible(object.kind(), kind) {
            return Err(HeapError::IncompatibleRetype {
                from: object.kind(),
                to: kind,
            });
        }
        object.header().kind.set(kind as u8);
        Ok(())
    }

    pub unsafe fn is_resizable(&self, vector: Sexp) -> bool {
        vector.flags() & flag_bits::RESIZABLE != 0
    }

    /// A triple-shaped object: a pairlist cell, call, closure, promise or similar. The three
    /// handles stay rooted across the allocation.
    #[inline]
    pub unsafe fn alloc_triple(
        &mut self,
        kind: SexpType,
        first: Sexp,
        second: Sexp,
        third: Sexp,
    ) -> HeapResult<Sexp> {
        self.alloc_triple_keeping(kind, first, second, third, None)
    }

    /// A promise over `expression` in `env` whose attribute slot holds the compiled `code`.
    /// Setting the slot on the fresh object needs no write barrier, which `set_attrib` would run.
    pub unsafe fn alloc_compiled_promise(
        &mut self,
        unforced: Sexp,
        expression: Sexp,
        env: Sexp,
        code: Sexp,
    ) -> HeapResult<Sexp> {
        let promise =
            self.alloc_triple_keeping(SexpType::Promise, unforced, expression, env, Some(code))?;
        promise.set_attrib_unbarriered(code);
        code.increment_refcount();
        Ok(promise)
    }

    /// `alloc_triple` that also keeps `extra` alive across the allocation.
    #[inline(always)]
    unsafe fn alloc_triple_keeping(
        &mut self,
        kind: SexpType,
        first: Sexp,
        second: Sexp,
        third: Sexp,
        extra: Option<Sexp>,
    ) -> HeapResult<Sexp> {
        let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
        let keep = [first, second, third, extra.unwrap_or(first)];
        let object = self.allocate_with(kind, size, &keep, 0)?;
        fill_triple(object, kind, first, second, third);
        Ok(object)
    }

    /// Runs the collection check for `bytes` of small objects at once, keeping `keep` alive
    /// across the collection it may run. The `_uncollected` allocators never collect, so a
    /// caller that builds a known batch of objects, such as a closure call's environment,
    /// binding cells and promises, pays for one check instead of one per object, and the
    /// objects it builds need no root until its next allocation that can collect.
    ///
    /// The reservation is a budget, not memory set aside: allocating past it is allowed and
    /// only moves the next collection earlier. Size it with [`TRIPLE_BYTES`] and
    /// [`ENVIRONMENT_BYTES`].
    #[inline]
    pub unsafe fn reserve(&mut self, bytes: usize, keep: &[Sexp]) {
        if self.young_bytes + bytes > self.small_trigger {
            self.reserve_slow(bytes, keep);
        }
    }

    /// [`Heap::alloc_triple`] without the collection check, after a [`Heap::reserve`].
    #[inline]
    pub unsafe fn alloc_triple_uncollected(
        &mut self,
        kind: SexpType,
        first: Sexp,
        second: Sexp,
        third: Sexp,
    ) -> HeapResult<Sexp> {
        let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate_uncollected(kind, size, 0)?;
        fill_triple(object, kind, first, second, third);
        Ok(object)
    }

    /// [`Heap::cons`] without the collection check, after a [`Heap::reserve`].
    #[inline]
    pub unsafe fn cons_uncollected(&mut self, car: Sexp, cdr: Sexp) -> HeapResult<Sexp> {
        self.alloc_triple_uncollected(SexpType::Pairlist, car, cdr, self.known.nil)
    }

    /// [`Heap::alloc_compiled_promise`] without the collection check, after a
    /// [`Heap::reserve`].
    pub unsafe fn alloc_compiled_promise_uncollected(
        &mut self,
        unforced: Sexp,
        expression: Sexp,
        env: Sexp,
        code: Sexp,
    ) -> HeapResult<Sexp> {
        let promise =
            self.alloc_triple_uncollected(SexpType::Promise, unforced, expression, env)?;
        promise.set_attrib_unbarriered(code);
        code.increment_refcount();
        Ok(promise)
    }

    #[inline]
    pub unsafe fn cons(&mut self, car: Sexp, cdr: Sexp) -> HeapResult<Sexp> {
        self.alloc_triple(SexpType::Pairlist, car, cdr, self.known.nil)
    }

    /// An ALTREP instance of `class` with the given vector type. It has the triple shape, with
    /// the data slots first and the class last, as GNU R stores them.
    pub unsafe fn alloc_altrep(
        &mut self,
        kind: SexpType,
        class: Sexp,
        data1: Sexp,
        data2: Sexp,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Pairlist, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(kind, size, &[class, data1, data2])?;
        object.set_altrep_flag();
        object.set_car_unbarriered(data1);
        object.set_cdr_unbarriered(data2);
        object.set_tag_unbarriered(class);
        data1.increment_refcount();
        data2.increment_refcount();
        Ok(object)
    }

    pub unsafe fn alloc_environment(&mut self, enclosure: Sexp) -> HeapResult<Sexp> {
        self.alloc_environment_with_capacity(enclosure, 0)
    }

    /// An environment whose frame has room for `bindings` bindings before it grows.
    pub unsafe fn alloc_environment_with_capacity(
        &mut self,
        enclosure: Sexp,
        bindings: usize,
    ) -> HeapResult<Sexp> {
        self.reserve(ENVIRONMENT_BYTES, &[enclosure]);
        self.alloc_environment_uncollected(enclosure, bindings)
    }

    /// [`Heap::alloc_environment_with_capacity`] without the collection check, after a
    /// [`Heap::reserve`].
    pub unsafe fn alloc_environment_uncollected(
        &mut self,
        enclosure: Sexp,
        bindings: usize,
    ) -> HeapResult<Sexp> {
        // Every field is written below, so zeroing the slot first would store it twice.
        let object = self.allocate_uncollected(SexpType::Environment, ENVIRONMENT_BYTES, 0)?;
        std::ptr::write(object.frame_ptr(), Frame::with_capacity(bindings));
        self.space.set_finalizable(object);
        object.set_enclosure_unbarriered(enclosure);
        enclosure.increment_refcount();
        Ok(object)
    }

    pub unsafe fn alloc_external_ptr(
        &mut self,
        address: *mut c_void,
        tag: Sexp,
        protected: Sexp,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::ExternalPtr, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(SexpType::ExternalPtr, size, &[tag, protected])?;
        object.set_external_address(address);
        object.set_external_tag_unbarriered(tag);
        object.set_external_protected_unbarriered(protected);
        tag.increment_refcount();
        protected.increment_refcount();
        Ok(object)
    }

    /// A weak reference. `c_finalizer` is a C function address, or zero.
    pub unsafe fn alloc_weak_ref(
        &mut self,
        key: Sexp,
        value: Sexp,
        finalizer: Sexp,
        c_finalizer: usize,
        finalize_on_exit: bool,
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::WeakRef, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(SexpType::WeakRef, size, &[key, value, finalizer])?;
        let payload = object.weak_payload();
        payload.key.set(key);
        payload.value.set(value);
        payload.finalizer.set(finalizer);
        payload.c_finalizer.set(c_finalizer);
        payload.finalize_on_exit.set(finalize_on_exit);
        // The fields are traced specially but count like any other, so a frame that is a
        // finalizer's key is not released when its call returns (`release_frame`).
        key.increment_refcount();
        value.increment_refcount();
        finalizer.increment_refcount();
        self.weak_refs.push(object);
        Ok(object)
    }

    pub unsafe fn alloc_primitive(&mut self, kind: SexpType, index: u32) -> HeapResult<Sexp> {
        let size = object_size(kind, 0).ok_or(HeapError::TooLong)?;
        let object = self.allocate(kind, size, &[])?;
        object.set_primitive_index(index);
        Ok(object)
    }

    /// A record, tuple or tag of the interned `layout`, whose slots hold `values` in layout order.
    /// Each value is stored raw or as a handle as [`slot_for`] decides, and stays rooted across
    /// the allocation.
    pub unsafe fn alloc_record(&mut self, layout: u32, values: &[Sexp]) -> HeapResult<Sexp> {
        let object = self.alloc_counted_record(layout, values)?;
        // Immutable: a modification always builds a new value.
        object.mark_not_mutable();
        Ok(object)
    }

    /// A record whose reference count tracks its holders, so a holder that owns its only
    /// reference may update it in place through [`Heap::set_record_slot`]. Dictionaries and sets
    /// use it.
    pub unsafe fn alloc_counted_record(
        &mut self,
        layout: u32,
        values: &[Sexp],
    ) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Record, values.len()).ok_or(HeapError::TooLong)?;
        if values.len() > u32::MAX as usize {
            return Err(HeapError::TooLong);
        }
        let object = self.allocate(SexpType::Record, size, values)?;
        let slots: smallvec::SmallVec<[Slot; 4]> = values
            .iter()
            .enumerate()
            .map(|(index, &value)| slot_for(value, index))
            .collect();
        for slot in &slots {
            if let Slot::Handle(handle) = slot {
                handle.increment_refcount();
            }
        }
        object.init_record(layout, &slots);
        self.observe_slots(layout, &slots);
        Ok(object)
    }

    /// An immutable record from slots compiled code already split into raw numbers and handles,
    /// so a number never has to be boxed on its way in. A [`Slot::Real`] must not carry a bit
    /// pattern [`slot_for`] keeps as a handle, and only the first [`crate::object::UNBOXED_SLOTS`] may be raw.
    pub unsafe fn alloc_record_slots(&mut self, layout: u32, slots: &[Slot]) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Record, slots.len()).ok_or(HeapError::TooLong)?;
        if slots.len() > u32::MAX as usize {
            return Err(HeapError::TooLong);
        }
        let handles: smallvec::SmallVec<[Sexp; 4]> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Handle(handle) => Some(*handle),
                _ => None,
            })
            .collect();
        let object = self.allocate(SexpType::Record, size, &handles)?;
        for handle in &handles {
            handle.increment_refcount();
        }
        object.init_record(layout, slots);
        object.mark_not_mutable();
        self.observe_slots(layout, slots);
        Ok(object)
    }

    /// Adds the kinds of `slots`, a new value of layout `layout`, to the layout's slot profile.
    unsafe fn observe_slots(&mut self, layout: u32, slots: &[Slot]) {
        for (index, slot) in slots.iter().enumerate() {
            self.layouts.observe(layout, index, slot_kind_of(*slot));
        }
    }

    /// Replaces slot `index` of `record`, storing `value` raw or as a handle as [`slot_for`]
    /// decides.
    pub unsafe fn set_record_slot(&mut self, record: Sexp, index: usize, value: Sexp) {
        let slot = slot_for(value, index);
        let old = record.record_handle(index);
        if let Slot::Handle(handle) = slot {
            handle.increment_refcount();
            self.write_barrier(record, handle);
        }
        if let Some(old) = old {
            old.decrement_refcount();
        }
        record.set_record_slot_unbarriered(index, slot);
        self.layouts
            .observe(record.record_layout(), index, slot_kind_of(slot));
    }

    /// The interned `CHARSXP` for `bytes` in `encoding`, which `Rf_mkCharLenCE` returns. ASCII
    /// text is always stored unmarked, so the same ASCII bytes give the same object in every
    /// encoding.
    pub unsafe fn mk_char(&mut self, bytes: &[u8], encoding: Encoding) -> HeapResult<Sexp> {
        if bytes.len() > i32::MAX as usize {
            return Err(HeapError::StringTooLong);
        }
        let scan = strings::scan(bytes);
        if scan.any_nul {
            let text = String::from_utf8_lossy(bytes).replace('\0', "\\0");
            return Err(HeapError::EmbeddedNul { text });
        }
        let bits = if scan.ascii {
            gp::ASCII
        } else {
            encoding.gp_bits()
        };
        let hash = strings::hash(&scan, bits & gp::ENCODING);
        match self.strings.find(hash, bytes, bits & gp::ENCODING) {
            Some(found) => Ok(found),
            None => self.intern_new(bytes, bits, hash),
        }
    }

    /// Allocates and interns a string the table does not hold. It is kept out of line so that
    /// re-interning an existing string, the common case, runs without its register pressure.
    #[cold]
    #[inline(never)]
    unsafe fn intern_new(&mut self, bytes: &[u8], bits: u16, hash: u32) -> HeapResult<Sexp> {
        let object = self.new_char(bytes, bits)?;
        object.set_capacity((u64::from(hash) << 32) as isize);
        object.set_gp_bits(gp::CACHED, true);
        self.strings.insert(hash, object, &mut self.space);
        Ok(object)
    }

    unsafe fn new_char(&mut self, bytes: &[u8], bits: u16) -> HeapResult<Sexp> {
        let size = object_size(SexpType::Char, bytes.len()).ok_or(HeapError::StringTooLong)?;
        let object = self.allocate(SexpType::Char, size, &[])?;
        object.set_length_and_capacity(bytes.len() as isize, 0);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), object.data(), bytes.len());
        object.set_gp(bits);
        Ok(object)
    }

    /// The interned symbol named `name`, which `Rf_install` returns.
    pub unsafe fn install(&mut self, name: &[u8]) -> HeapResult<Sexp> {
        if name.len() > MAX_SYMBOL_BYTES {
            return Err(HeapError::SymbolTooLong);
        }
        if name.is_empty() {
            return Err(HeapError::EmptySymbol);
        }
        let name = self.mk_char(name, Encoding::Native)?;
        self.install_char(name)
    }

    /// The interned symbol whose print name is the `CHARSXP` `name`.
    pub unsafe fn install_char(&mut self, name: Sexp) -> HeapResult<Sexp> {
        if let Some(&symbol) = self.symbols.get(&name) {
            return Ok(symbol);
        }
        if name.length() as usize > MAX_SYMBOL_BYTES {
            return Err(HeapError::SymbolTooLong);
        }
        let nil = self.known.nil;
        let symbol = self.alloc_triple(SexpType::Symbol, name, nil, nil)?;
        symbol.mark_not_mutable();
        self.symbols.insert(name, symbol);
        self.new_roots.push(symbol);
        Ok(symbol)
    }

    /// The interned symbol named `name`, if one exists, without installing it.
    pub unsafe fn find_symbol(&self, name: &[u8]) -> Option<Sexp> {
        let name = self.find_interned(name, 0)?;
        self.symbols.get(&name).copied()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn string_count(&self) -> usize {
        self.strings.len()
    }

    // Stores, each through the write barrier and the reference counts.

    /// Records a store of `child` into `parent`. Only an old parent that is not yet remembered
    /// can come to point at a younger child, and the parent's header, which the store is about to
    /// write near, says whether it is one.
    #[inline(always)]
    pub unsafe fn write_barrier(&mut self, parent: Sexp, child: Sexp) {
        if parent.gc_bits() & (gc_bits::OLD | gc_bits::REMEMBERED) != gc_bits::OLD {
            return;
        }
        self.write_barrier_slow(parent, child);
    }

    #[inline(never)]
    unsafe fn write_barrier_slow(&mut self, parent: Sexp, child: Sexp) {
        debug_assert!(self.space.is_marked(parent), "an old object lost its mark");
        if younger(parent, child) {
            parent.set_gc_bit(gc_bits::REMEMBERED, true);
            self.remembered.push(parent);
        }
    }

    #[inline(always)]
    unsafe fn store(&mut self, parent: Sexp, old: Sexp, new: Sexp) {
        if parent.flags() & flag_bits::UNTRACKED == 0 {
            new.increment_refcount();
            if old.release_reference() {
                self.note_zero(old);
            }
        }
        self.write_barrier(parent, new);
    }

    /// Stops a cons-like object's fields from counting as references to what they hold.
    pub unsafe fn untrack_references(&mut self, object: Sexp) {
        if object.flags() & flag_bits::UNTRACKED != 0 || object.kind().shape() != Shape::Triple {
            return;
        }
        object.car().decrement_refcount();
        object.cdr().decrement_refcount();
        object.tag().decrement_refcount();
        object.set_flag(flag_bits::UNTRACKED, true);
        self.untracked.push(object);
    }

    pub unsafe fn set_car(&mut self, cell: Sexp, value: Sexp) {
        self.store(cell, cell.car(), value);
        cell.set_car_unbarriered(value);
    }

    pub unsafe fn set_cdr(&mut self, cell: Sexp, value: Sexp) {
        self.store(cell, cell.cdr(), value);
        cell.set_cdr_unbarriered(value);
    }

    pub unsafe fn set_tag(&mut self, cell: Sexp, value: Sexp) {
        self.store(cell, cell.tag(), value);
        cell.set_tag_unbarriered(value);
    }

    /// Stores `count` elements of `from`, starting at `from_start`, into `to` from `to_start`,
    /// as `set_element` would one at a time but with one barrier decision for the whole run.
    /// Neither vector may be ALTREP, both ranges must be in bounds, and when `to` is `from` the
    /// ranges must not overlap.
    pub unsafe fn copy_elements(
        &mut self,
        to: Sexp,
        to_start: usize,
        from: Sexp,
        from_start: usize,
        count: usize,
    ) {
        debug_assert!(
            to != from || to_start + count <= from_start || from_start + count <= to_start,
            "copy_elements ranges overlap"
        );
        let tracked = to.flags() & flag_bits::UNTRACKED == 0;
        for offset in 0..count {
            let value = from.stored_element(from_start + offset);
            if tracked {
                value.increment_refcount();
                let old = to.stored_element(to_start + offset);
                if old.release_reference() {
                    self.note_zero(old);
                }
            }
            to.set_element_unbarriered(to_start + offset, value);
        }
        if count == 0 || to.gc_bits() & (gc_bits::OLD | gc_bits::REMEMBERED) != gc_bits::OLD {
            return;
        }
        if (0..count).any(|offset| younger(to, to.stored_element(to_start + offset))) {
            to.set_gc_bit(gc_bits::REMEMBERED, true);
            self.remembered.push(to);
        }
    }

    #[inline(always)]
    pub unsafe fn set_element(&mut self, vector: Sexp, index: usize, value: Sexp) {
        if vector.is_altrep()
            && let Some(elements) = crate::object::altrep_elements()
        {
            return (elements.write)(vector, index, value);
        }
        self.store(vector, vector.stored_element(index), value);
        vector.set_element_unbarriered(index, value);
    }

    /// Giving an Array any attribute turns it back into a plain R vector, because an Array is
    /// attribute-free by definition.
    pub unsafe fn set_attrib(&mut self, object: Sexp, value: Sexp) {
        self.store(object, object.attrib(), value);
        object.set_attrib_unbarriered(value);
        if value != self.known.nil && object.is_array() {
            object.set_array(false);
        }
    }

    pub unsafe fn set_enclosure(&mut self, env: Sexp, value: Sexp) {
        self.binding_epoch += 1;
        self.enclosure_epoch += 1;
        self.store(env, env.enclosure(), value);
        env.set_enclosure_unbarriered(value);
    }

    pub unsafe fn set_external_tag(&mut self, pointer: Sexp, value: Sexp) {
        self.store(pointer, pointer.external_tag(), value);
        pointer.set_external_tag_unbarriered(value);
    }

    pub unsafe fn set_external_protected(&mut self, pointer: Sexp, value: Sexp) {
        self.store(pointer, pointer.external_protected(), value);
        pointer.set_external_protected_unbarriered(value);
    }

    /// Records that native code received a writable pointer to `vector`'s handle elements, so
    /// its stores bypass the barrier and every minor collection must scan it.
    pub unsafe fn escape(&mut self, vector: Sexp) {
        if vector.gc_bits() & gc_bits::ESCAPED == 0 {
            vector.set_gc_bit(gc_bits::ESCAPED, true);
            self.escaped.push(vector);
            if matches!(vector.kind(), SexpType::List | SexpType::Expression)
                && vector.tracks_references()
            {
                let elements = (0..vector.length() as usize)
                    .map(|index| vector.stored_element(index))
                    .collect();
                self.escaped_lists.push((vector, elements));
            }
        }
    }

    /// Ends every escape once the outermost native call returns. Native code may store through
    /// a writable element pointer only while the call that took it runs (GNU R's collector never
    /// sees those stores at all), so each escaped vector that is old is remembered instead: the
    /// next minor collection rescans it once, catching the stores made meanwhile, and from then
    /// on it is an ordinary object whose stores go through the barrier. Without this a column a
    /// package touched once would be rescanned by every minor collection for the rest of the
    /// session.
    pub unsafe fn retire_escaped(&mut self) {
        // Native code may have replaced elements without counting either side, so the counts
        // move from what the list held when it escaped to what it holds now.
        for (vector, before) in std::mem::take(&mut self.escaped_lists) {
            for index in 0..vector.length() as usize {
                vector.stored_element(index).increment_refcount();
            }
            for old in before {
                if old.release_reference() {
                    self.note_zero(old);
                }
            }
        }
        let mut escaped = std::mem::take(&mut self.escaped);
        for vector in escaped.drain(..) {
            vector.set_gc_bit(gc_bits::ESCAPED, false);
            if vector.gc_bits() & (gc_bits::OLD | gc_bits::REMEMBERED) == gc_bits::OLD {
                vector.set_gc_bit(gc_bits::REMEMBERED, true);
                self.remembered.push(vector);
            }
        }
        self.escaped = escaped;
    }

    // Environments.

    fn is_symbol_frame(&self, env: Sexp) -> bool {
        env == self.known.base_env || env == self.known.base_namespace
    }

    /// The binding cell for `symbol` in `env`'s own frame.
    pub unsafe fn frame_cell(&self, env: Sexp, symbol: Sexp) -> Option<Sexp> {
        if self.is_symbol_frame(env) {
            let cell = symbol.cdr();
            return (cell.kind() == SexpType::Binding).then_some(cell);
        }
        if env == self.known.empty_env {
            return None;
        }
        (*env.frame_ptr()).find(symbol)
    }

    /// The binding cells of `env`'s own frame. The global frame's bindings live on the symbols,
    /// so it has none here.
    pub unsafe fn local_cells(&self, env: Sexp) -> impl Iterator<Item = Sexp> + '_ {
        let frame = (!self.is_symbol_frame(env) && env != self.known.empty_env)
            // SAFETY: an environment other than these owns its frame for as long as it lives.
            .then(|| unsafe { &*env.frame_ptr() });
        frame.into_iter().flat_map(Frame::cells)
    }

    /// `Rf_defineVar`: binds `symbol` in `env`'s own frame.
    pub unsafe fn define(&mut self, env: Sexp, symbol: Sexp, value: Sexp) -> HeapResult<()> {
        if let Some(cell) = self.frame_cell(env, symbol) {
            self.set_binding_value(cell, value)?;
            // A formal that was missing stops being missing once it is assigned, as in GNU R.
            cell.set_gp_bits(gp::BINDING_MISSING | gp::BINDING_ABSENT, false);
            return Ok(());
        }
        if env == self.known.empty_env {
            return Err(HeapError::EmptyEnvironment);
        }
        if env.has_gp(gp::FRAME_LOCKED) {
            return Err(HeapError::LockedEnvironment);
        }
        let cell =
            self.alloc_triple_keeping(SexpType::Binding, symbol, value, self.known.nil, Some(env))?;
        if !env.has_gp(gp::FUNCTION_FRAME) || symbol.has_gp(gp::WATCHED) {
            self.binding_epoch += 1;
            bump_symbol_epoch(symbol);
        }
        if self.is_symbol_frame(env) {
            self.store(symbol, symbol.cdr(), cell);
            symbol.set_cdr_unbarriered(cell);
        } else {
            self.write_barrier(env, cell);
            (*env.frame_ptr()).insert(symbol, cell);
        }
        Ok(())
    }

    /// Binds each of `bindings` in `env`, a new function frame, as a closure call binds its
    /// matched formals. The cells are allocated as one batch, with a single collection check
    /// that keeps every value alive, so the values need no other root.
    pub unsafe fn define_formals(&mut self, env: Sexp, bindings: &[Formal]) -> HeapResult<()> {
        let request = TRIPLE_BYTES * bindings.len();
        if self.young_bytes + request > self.small_trigger {
            let keep: SmallVec<[Sexp; 16]> = bindings
                .iter()
                .map(|binding| binding.value)
                .chain([env])
                .collect();
            self.reserve_slow(request, &keep);
        }
        self.define_formals_uncollected(env, bindings)
    }

    /// [`Heap::define_formals`] without the collection check, after a [`Heap::reserve`] that
    /// counted one [`TRIPLE_BYTES`] per binding.
    pub unsafe fn define_formals_uncollected(
        &mut self,
        env: Sexp,
        bindings: &[Formal],
    ) -> HeapResult<()> {
        debug_assert!(env.has_gp(gp::FUNCTION_FRAME));
        let nil = self.known.nil;
        let frame = &mut *env.frame_ptr();
        let mut first = true;
        for binding in bindings {
            let cell = self.allocate_uncollected(SexpType::Binding, TRIPLE_BYTES, 0)?;
            cell.set_car_unbarriered(binding.symbol);
            cell.set_cdr_unbarriered(binding.value);
            cell.set_tag_unbarriered(nil);
            binding.symbol.increment_refcount();
            binding.value.increment_refcount();
            nil.increment_refcount();
            if binding.missing {
                cell.set_gp_bits(gp::BINDING_MISSING, true);
            }
            frame.insert(binding.symbol, cell);
            // A collection may have made `env` old since it was allocated, and the cells are
            // all young, so the first cell's barrier check covers every one, including when a
            // later allocation fails and leaves the cells before it in the frame.
            if first {
                self.write_barrier(env, cell);
                first = false;
            }
        }
        Ok(())
    }

    pub unsafe fn set_binding_value(&mut self, cell: Sexp, value: Sexp) -> HeapResult<()> {
        if cell.has_gp(gp::BINDING_LOCKED) {
            return Err(HeapError::LockedBinding {
                name: String::from_utf8_lossy(cell.car().car().bytes()).into_owned(),
            });
        }
        if cell.has_gp(gp::WATCHED) {
            self.binding_epoch += 1;
            // A watched cell is one some lookup was compiled against, and compiled code checks
            // its speculations by the symbol's stamp alone while that stamp stays put, so a new
            // value has to move the stamp.
            bump_symbol_epoch(cell.car());
        }
        self.store(cell, cell.cdr(), value);
        cell.set_cdr_unbarriered(value);
        Ok(())
    }

    /// GNU R's `R_CleanupEnvir`: when a closure call returns `result`, the call's frame `env`
    /// gives up its bindings if nothing counts a reference to it but its own closures and
    /// promises (the cycles that a helper defined in the body or a default argument make).
    /// Counted stores release what the bindings held, so a large vector bound only there is
    /// freed at the next reconcile instead of waiting for a collection, which by then may have
    /// tenured it. The bindings give up `result` too, when it is a binding's value: the call's
    /// handle scope roots it for the caller, and a reconcile frees nothing a handle holds.
    ///
    /// Every reference to an environment from another object counts: bindings, list elements,
    /// attributes, a closure's or promise's environment, an enclosure and a weak reference's
    /// fields. Interpreter frames and handles do not, but none outlives the returned call.
    ///
    /// `closures` are the closures the call created in `env`. One that nothing counts may still
    /// be held by a root the counts do not see, such as a handle or a native call's protect
    /// stack, so a frame whose release needs it dead waits for the next reconcile, which checks
    /// the roots (`Heap::release_returned`). So does a frame the frames of calls it made still
    /// held through their promises when it returned, because those wait too.
    pub unsafe fn release_frame(&mut self, env: Sexp, result: Sexp, closures: &[Sexp]) {
        let count = env.refcount();
        if env == result
            || count == REFCOUNT_MAX
            || !env.has_gp(gp::FUNCTION_FRAME)
            || self.is_symbol_frame(env)
        {
            return;
        }
        // Only a reconcile flags the roots, and a closure the frame made may be held by one that
        // counts nothing, such as native state that keeps an R callback, so at return only
        // promises count as the frame's own cycles.
        if count as usize == self.frame_cycles(env, result, false) {
            if !self.roots.precious.contains_key(&env) {
                self.clear_frame(env);
            }
            return;
        }
        let closures: SmallVec<[Sexp; 2]> = closures
            .iter()
            .copied()
            .filter(|&closure| closure != result && closure.tag() == env)
            .collect();
        self.zero_bytes += self
            .local_cells(env)
            .map(|cell| cell.cdr())
            .filter(|&value| value.refcount() == 1 && reclaimable(value))
            .map(|value| self.reclaimable_bytes(value))
            .sum::<usize>();
        self.returned.push(Returned {
            env,
            closures,
            swept: 0,
        });
    }

    /// The counted references to `env` that its own bindings make: a promise, or with
    /// `closures` a closure, bound only there whose environment is `env` and that no root
    /// flagged by a reconcile holds.
    unsafe fn frame_cycles(&self, env: Sexp, result: Sexp, closures: bool) -> usize {
        self.local_cells(env)
            .filter(|cell| {
                let value = cell.cdr();
                cell.tracks_references()
                    && value != result
                    && value.refcount() == 1
                    && (value.kind() == SexpType::Promise
                        || (closures && value.kind() == SexpType::Closure))
                    && value.tag() == env
                    && value.gc_bits() & gc_bits::HELD == 0
            })
            .count()
    }

    /// Releases the bindings of a frame nothing else references, and its enclosure.
    unsafe fn clear_frame(&mut self, env: Sexp) {
        let cells: SmallVec<[Sexp; 16]> = self.local_cells(env).collect();
        let nil = self.known.nil;
        for &cell in &cells {
            if cell.has_gp(gp::BINDING_LOCKED | gp::ACTIVE_BINDING) || !cell.tracks_references() {
                continue;
            }
            let value = cell.cdr();
            match value.kind() {
                SexpType::Promise if value.refcount() == 1 => self.release_promise(value),
                SexpType::Dots if value.refcount() == 1 => {
                    let mut dots = value;
                    while matches!(dots.kind(), SexpType::Dots | SexpType::Pairlist) {
                        let promise = dots.car();
                        if promise.kind() == SexpType::Promise && promise.refcount() == 1 {
                            self.release_promise(promise);
                        }
                        dots = dots.cdr();
                    }
                }
                _ => {}
            }
            self.store(cell, value, nil);
            cell.set_cdr_unbarriered(nil);
        }
        // The enclosure counts a reference to the frame of the function that defined the callee,
        // which can be released in turn when that function returns.
        self.drop_enclosure(env);
    }

    /// Releases the frames that waited for a reconcile ([`Heap::release_frame`]), in the order
    /// their calls returned, so a callee's frame gives up its promises before its caller's frame
    /// is counted. The roots are flagged [`gc_bits::HELD`]; `frames_held` are the frames a root
    /// other than a handle holds, because the caller's handle scope roots every frame it made.
    unsafe fn release_returned(&mut self, frames_held: &[bool]) {
        let returned = std::mem::take(&mut self.returned);
        let unbound = self.known.unbound;
        for (entry, &held) in returned.iter().zip(frames_held) {
            let env = entry.env;
            let count = env.refcount();
            if held || count == REFCOUNT_MAX {
                continue;
            }
            let dead = entry
                .closures
                .iter()
                .filter(|&&closure| {
                    closure.refcount() == 0
                        && closure.tag() == env
                        && closure.gc_bits() & gc_bits::HELD == 0
                })
                .count();
            if count as usize == self.frame_cycles(env, unbound, true) + dead + entry.swept as usize
            {
                self.clear_frame(env);
            }
        }
    }

    /// Drops `env`'s reference to its enclosure once nothing counts a reference to `env` and it
    /// is not `result`: an environment a call made for itself, such as a dispatch context, is
    /// garbage then, and its enclosure may be a frame that can be released.
    pub unsafe fn release_enclosure(&mut self, env: Sexp, result: Sexp) {
        if env != result && env.refcount() == 0 && !self.roots.precious.contains_key(&env) {
            self.drop_enclosure(env);
        }
    }

    /// Lookups cache nothing about a garbage environment, so the enclosure epochs stay.
    unsafe fn drop_enclosure(&mut self, env: Sexp) {
        let empty = self.known.empty_env;
        self.store(env, env.enclosure(), empty);
        env.set_enclosure_unbarriered(empty);
    }

    /// GNU R's `unpromiseArgs`: once a closure call returns, the argument list `arguments` that
    /// the caller built for it gives up its promises, and each promise that only the list held
    /// gives up its value and environment. The caller must not read the list or its promises
    /// again.
    pub unsafe fn release_arguments(&mut self, arguments: Sexp) {
        if !arguments.tracks_references() {
            return;
        }
        let nil = self.known.nil;
        let mut cell = arguments;
        while cell.kind() == SexpType::Pairlist {
            let value = cell.car();
            if value.kind() == SexpType::Promise && value.refcount() == 1 {
                self.release_promise(value);
            }
            self.store(cell, value, nil);
            cell.set_car_unbarriered(nil);
            cell = cell.cdr();
        }
    }

    /// Clears a promise that only a released frame or argument list held.
    unsafe fn release_promise(&mut self, promise: Sexp) {
        let value = promise.car();
        if value != self.known.unbound {
            self.store(promise, value, self.known.unbound);
            promise.set_car_unbarriered(self.known.unbound);
        }
        let env = promise.tag();
        if env != self.known.nil {
            self.store(promise, env, self.known.nil);
            promise.set_tag_unbarriered(self.known.nil);
        }
    }

    /// Removes `symbol` from `env`'s own frame and reports whether it was bound.
    pub unsafe fn remove_binding(&mut self, env: Sexp, symbol: Sexp) -> HeapResult<bool> {
        if env.has_gp(gp::FRAME_LOCKED) {
            return Err(HeapError::LockedEnvironmentRemove);
        }
        self.binding_epoch += 1;
        bump_symbol_epoch(symbol);
        if self.is_symbol_frame(env) {
            let cell = symbol.cdr();
            if cell.kind() != SexpType::Binding {
                return Ok(false);
            }
            self.store(symbol, cell, self.known.nil);
            symbol.set_cdr_unbarriered(self.known.nil);
            self.clear_removed_cell(cell);
            return Ok(true);
        }
        if env == self.known.empty_env {
            return Ok(false);
        }
        let Some(cell) = (*env.frame_ptr()).remove(symbol) else {
            return Ok(false);
        };
        self.clear_removed_cell(cell);
        Ok(true)
    }

    /// An inline cache can still hold a removed cell until the code next runs, and the value
    /// must not stay reachable through it: `rm(x); gc()` has to let a finalizer on `x` run.
    unsafe fn clear_removed_cell(&mut self, cell: Sexp) {
        let unbound = self.known.unbound;
        self.store(cell, cell.cdr(), unbound);
        cell.set_cdr_unbarriered(unbound);
    }

    /// A stamp for lookups of `symbol` through environments other than function frames: it
    /// changes whenever a binding of `symbol` appears or disappears in such an environment, a
    /// watched binding of it is assigned, or any enclosure is replaced, so a cell cached under an
    /// unchanged stamp is still the one a fresh walk would find.
    pub unsafe fn lookup_stamp(&self, symbol: Sexp) -> u64 {
        symbol.symbol_payload().epoch.get() + self.enclosure_epoch
    }

    /// The first binding of `symbol` on the search path, from the global environment to the
    /// base environment. GNU R keeps the same cache, because every free variable of top-level
    /// code and of functions defined there resolves along the search path, which in a session
    /// with many attached packages means many large frames to probe.
    ///
    /// The flag says whether the cell may be cached under the symbol's stamp: it may not when
    /// the walk passed a function frame, whose bindings come and go without moving the stamp.
    pub unsafe fn global_cell(&self, symbol: Sexp) -> Option<(Sexp, bool)> {
        let payload = symbol.symbol_payload();
        let stamp = self.lookup_stamp(symbol);
        if payload.global_stamp.get() == stamp {
            return payload.global_cell.get().map(|cell| (cell, true));
        }
        let mut env = self.known.global_env;
        let mut cacheable = true;
        while env != self.known.empty_env {
            cacheable &= !env.has_gp(gp::FUNCTION_FRAME);
            if let Some(cell) = self.frame_cell(env, symbol) {
                if cacheable {
                    payload.global_cell.set(Some(cell));
                    payload.global_stamp.set(stamp);
                }
                return Some((cell, cacheable));
            }
            env = env.enclosure();
        }
        None
    }

    /// See the field: an unchanged epoch means no binding outside a function frame appeared or
    /// disappeared, and no enclosure changed.
    pub fn binding_epoch(&self) -> u64 {
        self.binding_epoch
    }

    /// The symbols `env` binds, in no particular order: cheaper than [`Heap::frame_symbols`]
    /// for a caller that sorts them or only needs the set.
    pub unsafe fn bound_symbols(&self, env: Sexp) -> Vec<Sexp> {
        if self.is_symbol_frame(env) || env == self.known.empty_env {
            return self.frame_symbols(env);
        }
        (*env.frame_ptr()).symbols().collect()
    }

    /// The symbols bound in `env`'s own frame, in the order GNU R lists them.
    pub unsafe fn frame_symbols(&self, env: Sexp) -> Vec<Sexp> {
        if self.is_symbol_frame(env) {
            return self
                .symbols
                .values()
                .copied()
                .filter(|symbol| symbol.cdr().kind() == SexpType::Binding)
                .collect();
        }
        if env == self.known.empty_env {
            return Vec::new();
        }
        (*env.frame_ptr())
            .listed_cells()
            .iter()
            .map(|cell| cell.car())
            .collect()
    }

    /// Makes `env`'s frame hashed, as `new.env(hash = TRUE, size = size)` creates it, which
    /// decides the order the frame is listed in.
    pub unsafe fn set_frame_hashed(&mut self, env: Sexp, size: i32) {
        if !self.is_symbol_frame(env) && env != self.known.empty_env {
            (*env.frame_ptr()).set_hashed(size);
        }
    }

    /// The binding count of each bucket of `env`'s hash table and the table's chain counter, as
    /// GNU R's `env.profile` reports them, or `None` when the frame is not hashed.
    pub unsafe fn frame_hash_profile(&self, env: Sexp) -> Option<(Vec<usize>, usize)> {
        if self.is_symbol_frame(env) || env == self.known.empty_env {
            return None;
        }
        unsafe { (*env.frame_ptr()).hash_profile() }
    }

    /// Counts a binding of `symbol` that `env` holds only for the length of one operation, as
    /// GNU R's complex assignment binds `*tmp*`, in the table that orders a hashed frame. The
    /// binding itself is never visible, but its insert and removal move GNU R's chain counter
    /// and so can grow the table, which changes the order the frame lists in.
    pub unsafe fn count_transient_binding(&mut self, env: Sexp, symbol: Sexp) {
        if self.is_symbol_frame(env) || env == self.known.empty_env {
            return;
        }
        unsafe { (*env.frame_ptr()).count_transient(symbol) }
    }

    /// Marks the bindings of a closure frame made so far as its formals.
    pub unsafe fn mark_frame_formals(&mut self, env: Sexp) {
        (*env.frame_ptr()).mark_formals();
    }

    // Roots.

    /// Keeps `object` alive until the current handle scope ends.
    #[inline]
    pub fn root(&mut self, object: Sexp) {
        self.roots.handles.push(object);
    }

    #[inline]
    pub fn handle_mark(&self) -> usize {
        self.roots.handles.len()
    }

    #[inline]
    pub fn release_handles(&mut self, mark: usize) {
        self.roots.handles.truncate(mark);
    }

    /// Roots `object` for the native call whose handouts start at `base`. Once the call holds
    /// twice the window, the older half is dropped in one move, which keeps the cost per handout
    /// constant and the live set bounded however long the call's loop runs.
    pub fn hand_out(&mut self, object: Sexp, base: usize) {
        let handouts = &mut self.roots.handouts;
        if handouts.len() - base >= 2 * HANDOUT_WINDOW {
            handouts.remove_range(base, base + HANDOUT_WINDOW);
        }
        handouts.push(object);
    }

    pub fn handout_mark(&self) -> usize {
        self.roots.handouts.len()
    }

    pub fn release_handouts(&mut self, mark: usize) {
        self.roots.handouts.truncate(mark);
    }

    /// Keeps `object` alive for the rest of the heap's life.
    pub fn add_global_root(&mut self, object: Sexp) {
        self.roots.globals.push(object);
    }

    /// `R_PreserveObject`. Preserving twice needs two releases.
    pub fn preserve(&mut self, object: Sexp) {
        *self.roots.precious.entry(object).or_insert(0) += 1;
        // An old object keeps its mark, so a minor collection would skip it anyway. The caller
        // is preserving the object, so it is live.
        if unsafe { object.gc_bits() } & gc_bits::OLD == 0 {
            self.new_roots.push(object);
        }
    }

    /// `R_ReleaseObject`. Releasing an object that is not preserved does nothing, as in GNU R.
    pub fn release(&mut self, object: Sexp) {
        if let Some(count) = self.roots.precious.get_mut(&object) {
            *count -= 1;
            if *count == 0 {
                self.roots.precious.remove(&object);
            }
        }
    }

    pub fn add_root_source(&mut self, source: Rc<dyn RootSource>) -> u64 {
        let id = self.next_source_id;
        self.next_source_id += 1;
        self.sources.push((id, source));
        id
    }

    pub fn remove_root_source(&mut self, id: u64) {
        self.sources.retain(|(source_id, _)| *source_id != id);
    }

    /// Weak references whose keys have died and whose finalizers should now run. The caller
    /// runs them; each stays alive until it is taken.
    pub fn take_ready_finalizers(&mut self) -> Vec<Sexp> {
        std::mem::take(&mut self.ready_to_finalize)
    }

    /// Weak references marked to finalize at exit, for `R_RunExitFinalizers`.
    pub unsafe fn take_exit_finalizers(&mut self) -> Vec<Sexp> {
        let (exit, kept): (Vec<Sexp>, Vec<Sexp>) = self
            .weak_refs
            .iter()
            .partition(|weak| weak.weak_finalize_on_exit());
        self.weak_refs = kept;
        exit
    }

    /// Clears a weak reference after its finalizer ran or was cancelled.
    pub unsafe fn clear_weak_ref(&mut self, weak: Sexp) {
        let nil = self.known.nil;
        let payload = weak.weak_payload();
        payload.key.set(nil);
        payload.value.set(nil);
        payload.finalizer.set(nil);
        payload.c_finalizer.set(0);
    }

    // Collection.

    #[inline(always)]
    unsafe fn allocate(&mut self, kind: SexpType, size: usize, keep: &[Sexp]) -> HeapResult<Sexp> {
        self.allocate_with(kind, size, keep, size)
    }

    /// Allocates an object and zeroes its first `zeroed` bytes. The rest is left as it was,
    /// which only a vector of plain numbers that its caller fills in may rely on. `keep` stays
    /// alive across a collection the allocation runs, which only the cold path does, so a
    /// caller's `keep` array costs nothing on the common path once this is inlined.
    #[inline(always)]
    unsafe fn allocate_with(
        &mut self,
        kind: SexpType,
        size: usize,
        keep: &[Sexp],
        zeroed: usize,
    ) -> HeapResult<Sexp> {
        match SIZE_CLASS_BY_WORDS.get(size.div_ceil(8)) {
            Some(&class) => self.allocate_small(kind, class as usize, size, zeroed, keep),
            None => self.allocate_large(kind, size, zeroed, keep),
        }
    }

    /// The young generation's fast path: a budget check and a pointer bump in the size class's
    /// pool. Everything else, the collection included, is on the cold path.
    #[inline(always)]
    unsafe fn allocate_small(
        &mut self,
        kind: SexpType,
        class: usize,
        size: usize,
        zeroed: usize,
        keep: &[Sexp],
    ) -> HeapResult<Sexp> {
        let young = self.young_bytes + size;
        if young > self.small_trigger {
            return self.allocate_small_slow(kind, class, size, zeroed, keep);
        }
        let memory = self
            .space
            .allocate(class)
            .ok_or(HeapError::CannotAllocate { bytes: size })?;
        self.young_bytes = young;
        Ok(self.init_small(memory, kind, class, zeroed))
    }

    #[cold]
    #[inline(never)]
    unsafe fn allocate_small_slow(
        &mut self,
        kind: SexpType,
        class: usize,
        size: usize,
        zeroed: usize,
        keep: &[Sexp],
    ) -> HeapResult<Sexp> {
        self.maybe_collect(size, false, keep);
        let memory = self
            .space
            .allocate(class)
            .ok_or(HeapError::CannotAllocate { bytes: size })?;
        self.young_bytes += size;
        Ok(self.init_small(memory, kind, class, zeroed))
    }

    #[cold]
    #[inline(never)]
    unsafe fn reserve_slow(&mut self, bytes: usize, keep: &[Sexp]) {
        self.maybe_collect(bytes, false, keep);
    }

    #[inline(never)]
    unsafe fn allocate_large(
        &mut self,
        kind: SexpType,
        size: usize,
        zeroed: usize,
        keep: &[Sexp],
    ) -> HeapResult<Sexp> {
        self.maybe_collect(size, true, keep);
        self.allocate_uncollected(kind, size, zeroed)
    }

    /// [`Heap::allocate_with`] without the collection check, for a caller that ran it once for
    /// a batch of objects.
    #[inline(always)]
    unsafe fn allocate_uncollected(
        &mut self,
        kind: SexpType,
        size: usize,
        zeroed: usize,
    ) -> HeapResult<Sexp> {
        let Some(&class) = SIZE_CLASS_BY_WORDS.get(size.div_ceil(8)) else {
            return self.allocate_large_uncollected(kind, size, zeroed);
        };
        let class = class as usize;
        let memory = self
            .space
            .allocate(class)
            .ok_or(HeapError::CannotAllocate { bytes: size })?;
        self.young_bytes += size;
        Ok(self.init_small(memory, kind, class, zeroed))
    }

    /// Zeroes a fresh small slot's first `zeroed` bytes and writes its header. Every slot size
    /// is a multiple of 8 bytes, and zeroing word by word keeps the common small object clear
    /// of a call into `memset`.
    #[inline(always)]
    unsafe fn init_small(
        &self,
        memory: NonNull<u8>,
        kind: SexpType,
        class: usize,
        zeroed: usize,
    ) -> Sexp {
        let words = memory.as_ptr().cast::<u64>();
        for word in 0..SLOT_SIZES[class].min(zeroed.next_multiple_of(8)) / 8 {
            words.add(word).write(0);
        }
        let object = Sexp::from_non_null(memory.cast());
        object.init_header(kind, class as u8, 0, self.known.nil);
        object
    }

    #[inline(never)]
    unsafe fn allocate_large_uncollected(
        &mut self,
        kind: SexpType,
        size: usize,
        zeroed: usize,
    ) -> HeapResult<Sexp> {
        if size > self.allocation_ceiling {
            return Err(HeapError::CannotAllocate { bytes: size });
        }
        let layout = Layout::from_size_align(size, 16)
            .map_err(|_| HeapError::CannotAllocate { bytes: size })?;
        let memory = if size >= MAPPED_BYTES {
            self.mapped.allocate(size, zeroed)
        } else if zeroed >= size {
            NonNull::new(std::alloc::alloc_zeroed(layout))
        } else {
            NonNull::new(std::alloc::alloc(layout)).inspect(|memory| {
                std::ptr::write_bytes(memory.as_ptr(), 0, zeroed);
            })
        }
        .ok_or(HeapError::CannotAllocate { bytes: size })?;
        let object = Sexp::from_non_null(memory.cast());
        object.init_header(kind, LARGE_CLASS, 0, self.known.nil);
        self.large.insert(object);
        self.young_large.push(object);
        self.young_large_bytes += size;
        self.large_allocated_since_major += size;
        Ok(object)
    }

    unsafe fn maybe_collect(&mut self, request: usize, large: bool, keep: &[Sexp]) {
        if self.inhibit > 0 {
            return;
        }
        // A reconcile scans every root, so it runs once the table could free enough to pay for
        // that, or has grown enough to need bounding. Running one at every large allocation
        // made package test suites a sixth slower, because entries a root holds stay listed.
        if self.zero_bytes >= RECONCILE_BYTES
            || self.zero_count.len() + self.returned.len() >= self.reconcile_at
        {
            self.reconcile(keep);
        }
        match self.policy.stress {
            GcStress::EveryAllocation => {
                // A major collection marks the whole heap, so running one every few allocations
                // makes stress quadratic in the heap's size: base R's loading never finished in a
                // debug build. Spacing majors by a sixteenth of the live objects bounds their
                // marking at about sixteen objects per allocation while still running them often
                // enough to free an old object whose last reference was dropped.
                // The spacing counts objects, so a loop of a few huge vectors would promote
                // them for thousands of allocations; the byte escalation below bounds that the
                // same way it bounds the ordinary schedule.
                self.stress_counter += 1;
                let spacing = (self.stats.live_objects / 16).max(8);
                if self.stress_counter >= spacing {
                    self.stress_counter = 0;
                    self.collect_keeping(Collection::Major, keep, request);
                } else if self.stress_counter == spacing / 2 {
                    self.collect_keeping(Collection::Intermediate, keep, request);
                } else {
                    self.collect_keeping(Collection::Minor, keep, request);
                    self.escalate_if_full(keep, request);
                }
            }
            GcStress::Off => {
                if self.collection_due(request, large) {
                    self.collect_keeping(Collection::Minor, keep, request);
                    self.escalate_if_full(keep, request);
                }
            }
        }
    }

    /// A minor collection keeps every old object, so the temporaries it found live and promoted
    /// stay until a deeper one. Escalate, as GNU R does, when what is left would not hold the
    /// request and a fifth of the smallest limit: first to an intermediate collection, which
    /// frees the temporaries promoted since the last deeper one without tracing the tenured
    /// heap, and only if that leaves the heap still full to a major one. Without the margin,
    /// every large vector allocated just below the limit would run a collection that frees
    /// nothing, and only a major one moves the limit.
    unsafe fn escalate_if_full(&mut self, keep: &[Sexp], request: usize) {
        for kind in [Collection::Intermediate, Collection::Major] {
            let policy = &self.policy;
            let vectors_full =
                self.old_large_bytes + request + policy.min_heap_limit / 5 > policy.heap_limit;
            let nodes_full = self.old_small_bytes + policy.min_node_limit / 5 > policy.node_limit;
            if !vectors_full && !nodes_full {
                break;
            }
            if kind == Collection::Major {
                self.stress_counter = 0;
            }
            self.escalating = true;
            self.collect_keeping(kind, keep, request);
            self.escalating = false;
        }
    }

    /// Remembers that the last counted reference to `object` went away, when a reconcile could
    /// reclaim it: a list, whose release gives up its elements, or a large atomic vector, whose
    /// memory a reconcile frees.
    #[inline]
    unsafe fn note_zero(&mut self, object: Sexp) {
        if !reclaimable(object) {
            return;
        }
        // An entry a root held at the last reconcile is still listed, but losing a counted
        // reference again is news about it all the same.
        if object.gc_bits() & gc_bits::ZERO_LISTED == 0 {
            object.set_gc_bit(gc_bits::ZERO_LISTED, true);
            self.zero_count.push(object);
        }
        self.zero_bytes += self.reclaimable_bytes(object);
    }

    /// The bytes freeing `object` would return: its own if it is a large atomic vector, and for
    /// a list, those of the large atomic vectors among its first elements that it alone holds,
    /// which is where a data frame's or a `list(...)` of results' columns are.
    unsafe fn reclaimable_bytes(&self, object: Sexp) -> usize {
        if object.kind() != SexpType::List {
            return self.size_of(object);
        }
        (0..(object.length() as usize).min(64))
            .map(|index| object.stored_element(index))
            .filter(|&element| {
                element.refcount() == 1 && element.kind() != SexpType::List && reclaimable(element)
            })
            .map(|element| self.size_of(element))
            .sum()
    }

    /// Frees what the zero-count table proves dead without waiting for a collection, the way
    /// GNU R's reference counts would if it freed on them, which it does not.
    ///
    /// An entry with no counted reference can still be held through a reference the counts do
    /// not see: a root (the protect stack, the precious set, a native call's handouts, an
    /// interpreter frame), the fields of an untracked object, an escaped list's elements, a weak
    /// reference, or the allocation's own `keep`. Those references are gathered shallowly and
    /// flagged, which costs one pass over the roots rather than a trace of the heap. Every other
    /// reference to a heap object is a counted field, so an entry at count zero that no root
    /// names is unreachable.
    ///
    /// A dead list gives up its elements, which may cascade to their own last references, and
    /// stays for the sweep with its fields cleared; a dead large atomic vector is freed at once,
    /// which is what returns a temporary's pages before the next collection would.
    unsafe fn reconcile(&mut self, keep: &[Sexp]) {
        if self.inhibit > 0 || (self.zero_count.is_empty() && self.returned.is_empty()) {
            return;
        }
        self.zero_bytes = 0;
        // Releasing a returned frame lists what its bindings held, so the frames go first, with
        // the roots flagged.
        let mut roots = Vec::new();
        if !self.returned.is_empty() {
            let handles_from;
            (roots, handles_from) = self.shallow_roots(keep);
            for &root in &roots[..handles_from] {
                root.set_gc_bit(gc_bits::HELD, true);
            }
            let frames_held: Vec<bool> = self
                .returned
                .iter()
                .map(|entry| entry.env.gc_bits() & gc_bits::HELD != 0)
                .collect();
            for &root in &roots[handles_from..] {
                root.set_gc_bit(gc_bits::HELD, true);
            }
            self.release_returned(&frames_held);
        }
        let entries = std::mem::take(&mut self.zero_count);
        let mut work: Vec<Sexp> = Vec::new();
        for &object in &entries {
            if object.refcount() == 0 {
                work.push(object);
            } else {
                object.set_gc_bit(gc_bits::ZERO_LISTED, false);
            }
        }
        if work.is_empty() {
            for root in roots.drain(..) {
                root.set_gc_bit(gc_bits::HELD, false);
            }
            self.reconcile_at = RECONCILE_ENTRIES;
            return;
        }
        if roots.is_empty() {
            roots = self.shallow_roots(keep).0;
            for &root in &roots {
                root.set_gc_bit(gc_bits::HELD, true);
            }
        }
        // Held entries stay in the table: the root that holds one can drop it without a store,
        // and the next reconcile looks again.
        let mut held = Vec::new();
        work.retain(|&object| {
            if object.gc_bits() & (gc_bits::HELD | gc_bits::ESCAPED) != 0 {
                held.push(object);
                false
            } else {
                object.set_gc_bit(gc_bits::ZERO_LISTED, false);
                true
            }
        });
        let verify = self.policy.verify;
        let nil = self.known.nil;
        let mut freed = false;
        while let Some(object) = work.pop() {
            if object.gc_bits() & gc_bits::RELEASED != 0 {
                continue;
            }
            object.set_gc_bit(gc_bits::RELEASED, true);
            let give_up = |child: Sexp, work: &mut Vec<Sexp>, held: &mut Vec<Sexp>| {
                if !child.release_reference()
                    || child.gc_bits() & gc_bits::RELEASED != 0
                    || !(reclaimable(child) || child.kind() == SexpType::Pairlist)
                {
                    return;
                }
                if child.gc_bits() & (gc_bits::HELD | gc_bits::ESCAPED) == 0 {
                    child.set_gc_bit(gc_bits::ZERO_LISTED, false);
                    work.push(child);
                } else if reclaimable(child) && child.gc_bits() & gc_bits::ZERO_LISTED == 0 {
                    child.set_gc_bit(gc_bits::ZERO_LISTED, true);
                    held.push(child);
                }
            };
            give_up(object.attrib(), &mut work, &mut held);
            object.set_attrib_unbarriered(nil);
            match object.kind() {
                SexpType::List => {
                    for index in 0..object.length() as usize {
                        give_up(object.stored_element(index), &mut work, &mut held);
                        object.set_element_unbarriered(index, nil);
                    }
                }
                SexpType::Pairlist => {
                    give_up(object.car(), &mut work, &mut held);
                    give_up(object.cdr(), &mut work, &mut held);
                    give_up(object.tag(), &mut work, &mut held);
                    object.set_car_unbarriered(nil);
                    object.set_cdr_unbarriered(nil);
                    object.set_tag_unbarriered(nil);
                }
                _ => {
                    if verify {
                        self.expect_dead.push(object);
                    } else if object.gc_bits() & gc_bits::REMEMBERED == 0 {
                        self.free_released(object);
                        freed = true;
                    }
                    continue;
                }
            }
            if verify {
                self.expect_dead.push(object);
            }
        }
        for root in roots.drain(..) {
            root.set_gc_bit(gc_bits::HELD, false);
        }
        self.zero_count = held;
        self.reconcile_at = self.zero_count.len() + RECONCILE_ENTRIES;
        if freed {
            self.refresh_live_stats();
        }
        if verify && !self.expect_dead.is_empty() {
            // Nothing was freed, so the full collection that checks the released objects are
            // unreachable is also the one that reclaims them.
            self.collect_keeping(Collection::Major, keep, 0);
        }
    }

    /// Drops the zero-count entries a collection is about to sweep, and gives up what the dead
    /// lists among them held, with the lists those hold in turn. Sweeping decrements nothing,
    /// so without this a list a frame released, which a collection found before a reconcile
    /// did, would leave its columns counted forever, and no reconcile could free them once a
    /// later copy of the data frame died.
    unsafe fn release_swept_lists(&mut self) {
        let entries = std::mem::take(&mut self.zero_count);
        let mut dying: Vec<Sexp> = Vec::new();
        for object in entries {
            if self.space.is_marked(object) {
                self.zero_count.push(object);
            } else if object.kind() == SexpType::List && !object.is_altrep() {
                dying.push(object);
            }
        }
        let nil = self.known.nil;
        while let Some(object) = dying.pop() {
            let attributes = object.attrib();
            object.set_attrib_unbarriered(nil);
            let mut children: SmallVec<[Sexp; 16]> = SmallVec::new();
            children.push(attributes);
            for index in 0..object.length() as usize {
                children.push(object.stored_element(index));
                object.set_element_unbarriered(index, nil);
            }
            for child in children {
                if !child.release_reference() {
                    continue;
                }
                if !self.space.is_marked(child) {
                    if child.kind() == SexpType::List && !child.is_altrep() {
                        dying.push(child);
                    }
                } else {
                    self.note_zero(child);
                }
            }
        }
    }

    /// Every object a reference the counts do not see points at, for [`Heap::reconcile`], with
    /// the handles last and where they start. A root's own fields are counted, so its children
    /// need no visit.
    unsafe fn shallow_roots(&mut self, keep: &[Sexp]) -> (Vec<Sexp>, usize) {
        let mut roots = Vec::with_capacity(4096);
        roots.extend(self.known.all());
        roots.extend_from_slice(keep);
        let stacks = &self.roots;
        for stack in [&stacks.globals, &stacks.protect.stack, &stacks.handouts] {
            roots.extend_from_slice(&stack.entries);
        }
        roots.extend(stacks.precious.keys().copied());
        let sources = std::mem::take(&mut self.sources);
        for (_, source) in &sources {
            source.trace(&mut Marker {
                stack: &mut roots,
                space: None,
            });
        }
        self.sources = sources;
        for &weak in self.weak_refs.iter().chain(&self.ready_to_finalize) {
            roots.extend([
                weak,
                weak.weak_key(),
                weak.weak_value(),
                weak.weak_finalizer(),
            ]);
        }
        for &object in &self.untracked {
            roots.extend([object.car(), object.cdr(), object.tag(), object.attrib()]);
        }
        for &vector in &self.escaped {
            if !vector.is_altrep() && matches!(vector.kind(), SexpType::List | SexpType::Expression)
            {
                roots.extend(
                    (0..vector.length() as usize).map(|index| vector.stored_element(index)),
                );
            }
        }
        let handles_from = roots.len();
        roots.extend_from_slice(&self.roots.handles.entries);
        (roots, handles_from)
    }

    /// Frees a large atomic vector a reconcile found dead. A young vector's bytes stay counted
    /// until the next minor collection, so a reconcile returns pages early without putting that
    /// collection off: the dead temporaries only a collection finds, such as a forced promise's
    /// value, would pile up meanwhile, and a loop of `y <- rep(runif(1e6), 10)` then peaked a
    /// quarter higher. An old vector's bytes are uncounted at once, because only the deeper
    /// collections recount the old generation, and the stale bytes made every minor collection
    /// of a vector loop escalate to a full one that found nothing more to free.
    unsafe fn free_released(&mut self, object: Sexp) {
        if object.gc_bits() & gc_bits::MARK == 0 {
            if let Some(position) = self.young_large.iter().rposition(|&young| young == object) {
                self.young_large.swap_remove(position);
            }
        } else {
            self.old_large_bytes = self.old_large_bytes.saturating_sub(self.size_of(object));
        }
        self.free_large(object);
        self.stats.vectors_released += 1;
        self.stats.objects_freed += 1;
    }

    /// Whether allocating `request` more bytes, as one large vector or as small objects, runs a
    /// collection.
    fn collection_due(&self, request: usize, large: bool) -> bool {
        if self.inhibit > 0 {
            return false;
        }
        if self.policy.stress == GcStress::EveryAllocation {
            return true;
        }
        // Large vectors get a budget of their own. They do not crowd the nursery's cache, and
        // a reader such as `fread` allocates every column before filling any, so charging
        // them to the nursery would buy a collection per column that rescans the columns
        // already allocated.
        if large {
            let old_bytes = self.old_bytes();
            self.young_large_bytes + request > self.policy.young_large_limit.max(old_bytes / 2)
                || self.old_large_bytes + self.young_large_bytes + request > self.policy.heap_limit
        } else {
            self.young_bytes + request > self.small_trigger
        }
    }

    /// Runs a collection now. Every live object must be reachable from a root.
    /// A major collection asked for this way also returns the freed large vectors' kept memory
    /// to the kernel, as GNU R's `gc()` returns free memory.
    pub unsafe fn collect(&mut self, kind: Collection) {
        self.collect_keeping(kind, &[], 0);
        if kind == Collection::Major && self.inhibit == 0 {
            self.mapped.flush();
        }
    }

    /// Runs a collection for an allocation of `request` bytes, which a major collection counts
    /// as needed when it adjusts the heap limit.
    unsafe fn collect_keeping(&mut self, kind: Collection, keep: &[Sexp], request: usize) {
        if self.inhibit > 0 {
            return;
        }
        self.inhibit += 1;
        let started = self
            .policy
            .timing
            .then(|| (Instant::now(), process_times()));
        let logged = self
            .pause_log
            .is_some()
            .then(|| (Instant::now(), self.young_bytes, self.young_large_bytes));
        self.space.marked_vector_cells = 0;
        self.space.tenured_vector_cells = 0;
        if kind != Collection::Major && self.policy.verify {
            self.verify_barrier();
        }
        match kind {
            Collection::Minor => self.space.promote_bits = gc_bits::OLD,
            Collection::Intermediate => {
                self.unmark_untenured();
                self.space.promote_bits = gc_bits::OLD | gc_bits::TENURED;
            }
            Collection::Major => {
                self.clear_marks();
                self.space.promote_bits = gc_bits::OLD | gc_bits::TENURED;
            }
        }
        for &object in keep {
            self.mark(object);
        }
        self.mark_roots(kind);
        if kind == Collection::Intermediate {
            // A tenured object stays marked, so tracing stops at it; the ones that may point at
            // younger objects are exactly the remembered and escaped ones, so tracing starts
            // from them too. The others are unmarked now, and traced if anything reaches them.
            // Tracing remembers again each tenured parent still left pointing at a younger child.
            for index in 0..self.remembered.len() {
                let parent = self.remembered[index];
                parent.set_gc_bit(gc_bits::REMEMBERED, false);
                if parent.gc_bits() & gc_bits::TENURED != 0 {
                    self.mark_stack.push(parent);
                }
            }
            self.remembered.clear();
            for index in 0..self.escaped.len() {
                let parent = self.escaped[index];
                if parent.gc_bits() & gc_bits::TENURED != 0 {
                    self.mark_stack.push(parent);
                }
            }
        }
        if kind == Collection::Minor {
            // Old objects are already marked, so their young children are pushed directly.
            for index in 0..self.remembered.len() {
                let parent = self.remembered[index];
                self.mark_stack.push(parent);
            }
            // An escaped character vector can only have come to hold CHARSXPs, which point at
            // nothing, so marking every young CHARSXP covers whatever native code stored into it
            // without rereading its elements, each a cache miss on another object's header. A
            // young CHARSXP that is in fact garbage survives until the next deeper collection.
            let mut escaped_strings = false;
            for index in 0..self.escaped.len() {
                let parent = self.escaped[index];
                if parent.kind() == SexpType::String
                    && !parent.is_altrep()
                    && self.space.is_marked(parent)
                {
                    escaped_strings = true;
                    self.mark(parent.attrib());
                } else {
                    self.mark_stack.push(parent);
                }
            }
            if escaped_strings {
                // Every CHARSXP but the rooted `NA_STRING` is interned, so the young ones are
                // the strings interned since the last collection.
                self.space.mark_young_interned();
                for index in 0..self.strings.young_large().len() {
                    let string = self.strings.young_large()[index];
                    self.mark(string);
                }
            }
        }
        self.drain();
        self.process_weak_refs();
        self.clear_dead_weak_pairs();
        // Before the sweep, while a mark still means "survives". A returned frame the sweep frees
        // gives up its bindings first: sweeping decrements nothing, and those bindings may be the
        // only counted references left to its caller's frame, which also waits to be released.
        let returned = std::mem::take(&mut self.returned);
        for entry in returned {
            if self.space.is_marked(entry.env) {
                self.returned.push(entry);
            } else {
                self.clear_frame(entry.env);
            }
        }
        self.release_swept_lists();
        let space = &self.space;
        self.escaped.retain(|&object| space.is_marked(object));
        self.escaped_lists
            .retain(|(object, _)| space.is_marked(*object));
        for entry in &mut self.returned {
            let env = entry.env;
            entry.closures.retain(|&mut closure| {
                let swept = !space.is_marked(closure);
                if swept && closure.tag() == env {
                    entry.swept += 1;
                }
                !swept
            });
        }
        self.untracked.retain(|&object| space.is_marked(object));
        if !self.expect_dead.is_empty() {
            let expect_dead = std::mem::take(&mut self.expect_dead);
            for &object in &expect_dead {
                if self.space.is_marked(object) {
                    panic!(
                        "a reconcile would have freed a live {:?} of length {} (refcount {})",
                        object.kind(),
                        object.length(),
                        object.refcount()
                    );
                }
            }
        }
        match kind {
            Collection::Minor => {
                self.strings.remove_dead_young(&self.space);
                self.sweep_young();
                self.stats.minor_collections += 1;
            }
            Collection::Intermediate => {
                self.strings.remove_dead(&self.space);
                self.sweep_all();
                self.space.forget_dead_tenured();
                self.stats.intermediate_collections += 1;
            }
            Collection::Major => {
                self.strings.remove_dead(&self.space);
                self.sweep_all();
                self.space.forget_dead_tenured();
                self.stats.major_collections += 1;
                // The limit shrinks by a fifth at most per major collection rather than to what
                // survived, so an explicit `gc()` between two large operations does not drop it
                // below the next one's peak and buy a full collection in the middle of it.
                let policy = &mut self.policy;
                // A full heap that forces a major collection before the program allocated a
                // limit's worth of vectors since the last one means the limit is too close to
                // what the program keeps in flight between collections: each such major
                // collection tenures that working set, which only the next major collection
                // can free. The survivors alone cannot show it, so the limit grows as if they
                // filled it.
                let thrashing =
                    self.escalating && self.large_allocated_since_major < policy.heap_limit;
                self.large_allocated_since_major = 0;
                policy.heap_limit = policy.growth.next_limit(
                    policy.heap_limit,
                    self.old_large_bytes + request,
                    policy.min_heap_limit,
                    thrashing,
                );
                policy.node_limit = policy.growth.next_limit(
                    policy.node_limit,
                    self.old_small_bytes,
                    policy.min_node_limit,
                    false,
                );
                self.mapped.limit = policy.heap_limit;
                self.mapped.trim();
            }
        }
        match kind {
            // A minor collection promotes a tenured parent's young children only to the younger
            // old generation, so the parent may still point below its own and stays remembered
            // until a deeper collection tenures its children.
            Collection::Minor => self.remembered.retain(|&parent| {
                let tenured = parent.gc_bits() & gc_bits::TENURED != 0;
                if !tenured {
                    parent.set_gc_bit(gc_bits::REMEMBERED, false);
                }
                tenured
            }),
            // A deeper collection emptied the set before it marked, and tracing put back the
            // tenured parents of the survivors it aged only to the younger old generation.
            Collection::Intermediate | Collection::Major => {}
        }
        self.young_bytes = 0;
        self.young_large_bytes = 0;
        self.refresh_small_trigger();
        let stats = &mut self.stats;
        stats.live_vector_cells = match kind {
            Collection::Minor => stats.live_vector_cells + self.space.marked_vector_cells,
            Collection::Intermediate => self.tenured_vector_cells + self.space.marked_vector_cells,
            Collection::Major => self.space.marked_vector_cells,
        };
        match kind {
            Collection::Minor => {}
            Collection::Intermediate => {
                self.tenured_vector_cells += self.space.tenured_vector_cells
            }
            Collection::Major => self.tenured_vector_cells = self.space.tenured_vector_cells,
        }
        stats.max_live_objects = stats.max_live_objects.max(stats.live_objects);
        stats.max_vector_cells = stats.max_vector_cells.max(stats.live_vector_cells);
        if let Some((elapsed, (user, system))) = started {
            let (user_now, system_now) = process_times();
            stats.user_time += user_now.saturating_sub(user);
            stats.system_time += system_now.saturating_sub(system);
            stats.elapsed_time += elapsed.elapsed();
        }
        let old_bytes = self.old_bytes();
        if let (Some((started, young, young_large)), Some(log)) = (logged, &mut self.pause_log) {
            let kind = match kind {
                Collection::Minor => "minor",
                Collection::Intermediate => "intermediate",
                Collection::Major => "major",
            };
            let line = format!(
                "{kind} {} {young} {young_large} {}\n",
                started.elapsed().as_nanos(),
                old_bytes
            );
            if let Err(error) = log.write_all(line.as_bytes()) {
                panic!("cannot write the collection log RY_GC_LOG names: {error}");
            }
        }
        if let Some(reporter) = self.policy.reporter {
            reporter(&self.report(kind));
        }
        self.inhibit -= 1;
    }

    #[inline]
    unsafe fn mark(&mut self, object: Sexp) {
        if self.space.mark(object) && !is_leaf(object, self.known.nil) {
            self.mark_stack.push(object);
        }
        if self.tracing_tenured && !self.younger_child && !self.space.is_tenured(object) {
            self.younger_child = true;
        }
    }

    unsafe fn mark_roots(&mut self, kind: Collection) {
        let known = self.known;
        for object in known.all() {
            self.mark(object);
        }
        let roots = &mut self.roots;
        for stack in [
            &mut roots.globals,
            &mut roots.protect.stack,
            &mut roots.handles,
            &mut roots.handouts,
        ] {
            let start = match kind {
                Collection::Intermediate | Collection::Major => 0,
                Collection::Minor => stack.scanned,
            };
            for &object in &stack.entries[start..] {
                if self.space.mark(object) {
                    self.mark_stack.push(object);
                }
            }
            stack.scanned = stack.entries.len();
        }
        // Marking only touches the mark stack, so the tables can be borrowed beside it instead of
        // copied out on every collection; a package with tens of thousands of symbols made that
        // copy a large part of each minor collection.
        let mark_stack = &mut self.mark_stack;
        let space = &mut self.space;
        let mut mark = |object: Sexp| {
            if space.mark(object) {
                mark_stack.push(object);
            }
        };
        match kind {
            Collection::Intermediate | Collection::Major => {
                self.roots.precious.keys().for_each(|&object| mark(object));
                self.symbols.values().for_each(|&symbol| mark(symbol));
            }
            Collection::Minor => self.new_roots.iter().for_each(|&object| mark(object)),
        }
        self.new_roots.clear();
        // Tracing only marks, so it cannot register or remove a source while the list is out.
        let sources = std::mem::take(&mut self.sources);
        for (_, source) in &sources {
            let mut marker = Marker {
                stack: &mut self.mark_stack,
                space: Some(&mut self.space),
            };
            source.trace(&mut marker);
        }
        self.sources = sources;
        // A weak reference lives until its key dies; only its key and value are weak.
        for index in 0..self.weak_refs.len() {
            self.mark(self.weak_refs[index]);
        }
        for index in 0..self.ready_to_finalize.len() {
            let weak = self.ready_to_finalize[index];
            self.mark(weak);
            self.mark(weak.weak_key());
            self.mark(weak.weak_value());
            self.mark(weak.weak_finalizer());
        }
    }

    unsafe fn drain(&mut self) {
        while let Some(object) = self.mark_stack.pop() {
            self.trace(object);
        }
    }

    unsafe fn trace(&mut self, object: Sexp) {
        // Collection ages each survivor by one generation, so a deeper collection can leave a
        // tenured object pointing at a child it aged only to the younger old generation; the
        // write barrier never saw that pointer, so tracing remembers the parent instead.
        self.tracing_tenured = self.space.promote_bits & gc_bits::TENURED != 0
            && object.gc_bits() & gc_bits::TENURED != 0;
        self.younger_child = false;
        self.trace_children(object);
        if self.younger_child && object.gc_bits() & gc_bits::REMEMBERED == 0 {
            object.set_gc_bit(gc_bits::REMEMBERED, true);
            self.remembered.push(object);
        }
        self.tracing_tenured = false;
    }

    unsafe fn trace_children(&mut self, object: Sexp) {
        self.mark(object.attrib());
        if object.is_altrep() {
            self.mark(object.car());
            self.mark(object.cdr());
            self.mark(object.tag());
            return;
        }
        match object.kind().shape() {
            Shape::Vector => {
                if object.flags() & flag_bits::WEAK_PAIRS != 0 {
                    self.weak_pairs.push(object);
                } else if object.kind() == SexpType::String {
                    // A character vector usually repeats a few strings, like a column of
                    // categories, and marking one again only finds it marked. A small table of
                    // the addresses this vector already marked skips the repeats. Its elements
                    // are CHARSXPs, which hold no references, so a skipped repeat loses nothing.
                    let mut marked = [0usize; 64];
                    for &element in object.slice::<Sexp>() {
                        let address = element.as_ptr() as usize;
                        let slot = (address >> 4) as u8 as usize % marked.len();
                        if marked[slot] == address {
                            continue;
                        }
                        marked[slot] = address;
                        self.mark(element);
                    }
                } else if matches!(object.kind(), SexpType::List | SexpType::Expression) {
                    for index in 0..object.length() as usize {
                        self.mark(object.element(index));
                    }
                }
            }
            Shape::Triple => {
                self.mark(object.car());
                self.mark(object.cdr());
                self.mark(object.tag());
            }
            Shape::Environment => {
                self.mark(object.enclosure());
                for cell in (*object.frame_ptr()).cells() {
                    self.mark(cell);
                }
            }
            Shape::ExternalPtr => {
                self.mark(object.external_tag());
                self.mark(object.external_protected());
            }
            Shape::Record => {
                for index in 0..object.record_slot_count() {
                    if let Some(handle) = object.record_handle(index) {
                        self.mark(handle);
                    }
                }
            }
            // Weak references are traced by `process_weak_refs`.
            Shape::WeakRef | Shape::Primitive => {}
        }
    }

    /// Ephemeron semantics: a weak reference's value and finalizer are reachable only while its
    /// key is, so the values are marked to a fixpoint before any key is declared dead. Every
    /// reference with a dead key is then taken off at once: one with a finalizer is revived with
    /// its key, value and finalizer until the finalizer has run, and one without is cleared.
    /// Because all dying references leave together, what the revived finalizers reach cannot
    /// revive another, so one pass suffices.
    unsafe fn process_weak_refs(&mut self) {
        self.mark_live_weak_values();
        let mut dying = Vec::new();
        let mut index = 0;
        while index < self.weak_refs.len() {
            let weak = self.weak_refs[index];
            if self.space.is_marked(weak.weak_key()) {
                index += 1;
            } else {
                dying.push(self.weak_refs.swap_remove(index));
            }
        }
        for weak in dying {
            let has_finalizer =
                weak.weak_c_finalizer() != 0 || weak.weak_finalizer() != self.known.nil;
            if has_finalizer {
                self.mark(weak.weak_key());
                self.mark(weak.weak_value());
                self.mark(weak.weak_finalizer());
                self.ready_to_finalize.push(weak);
            } else {
                self.clear_weak_ref(weak);
            }
        }
        // Every reference still registered has a key the first fixpoint marked, so marking what
        // the dying finalizers reach cannot revive another value: it only keeps those objects.
        self.drain();
    }

    /// Runs after every weak reference is settled, so a key a finalizer revived counts as live.
    /// A live key's value stays unmarked when it is reachable only through the pair, so the
    /// pair is cleared then too rather than left pointing at a freed object.
    unsafe fn clear_dead_weak_pairs(&mut self) {
        let nil = self.known.nil;
        for list in std::mem::take(&mut self.weak_pairs) {
            for pair in 0..list.length() as usize / 2 {
                let key = list.element(pair * 2);
                let value = list.element(pair * 2 + 1);
                if !self.space.is_marked(key) || !self.space.is_marked(value) {
                    key.decrement_refcount();
                    value.decrement_refcount();
                    list.set_element_unbarriered(pair * 2, nil);
                    list.set_element_unbarriered(pair * 2 + 1, nil);
                }
            }
        }
    }

    /// Marks the value and finalizer of every weak reference whose key is marked, to a
    /// fixpoint.
    unsafe fn mark_live_weak_values(&mut self) {
        loop {
            let mut changed = false;
            for index in 0..self.weak_refs.len() {
                let weak = self.weak_refs[index];
                if self.space.is_marked(weak.weak_key()) {
                    for child in [weak.weak_value(), weak.weak_finalizer()] {
                        if self.space.mark(child) {
                            self.mark_stack.push(child);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
            self.drain();
        }
    }

    /// The write barrier's invariant: an old object that is neither remembered nor escaped
    /// points only to old objects, and a tenured one only to tenured objects. A violation means
    /// some store skipped the barrier, and the next minor or intermediate collection would free
    /// a live child.
    unsafe fn verify_barrier(&self) {
        let check = |parent: Sexp| {
            let tenured = parent.gc_bits() & gc_bits::TENURED != 0;
            let slot_tenured = self.space.is_tenured_slot(parent);
            if slot_tenured.is_some_and(|slot| slot != tenured)
                || (tenured && !self.space.is_marked(parent))
            {
                panic!(
                    "the header of a {:?} says it is {}tenured, but the collector disagrees",
                    parent.kind(),
                    if tenured { "" } else { "not " },
                );
            }
            if self.space.is_marked(parent) != (parent.gc_bits() & gc_bits::OLD != 0) {
                panic!(
                    "the header of a {:?} says it is {}, but its mark disagrees",
                    parent.kind(),
                    if parent.gc_bits() & gc_bits::OLD != 0 {
                        "old"
                    } else {
                        "young"
                    },
                );
            }
            if !self.space.is_marked(parent)
                || parent.gc_bits() & (gc_bits::REMEMBERED | gc_bits::ESCAPED) != 0
            {
                return;
            }
            let mut children = vec![parent.attrib()];
            if parent.is_altrep() {
                children.extend([parent.car(), parent.cdr(), parent.tag()]);
            } else {
                match parent.kind().shape() {
                    Shape::Vector
                        if matches!(
                            parent.kind(),
                            SexpType::String | SexpType::List | SexpType::Expression
                        ) =>
                    {
                        children.extend(
                            (0..parent.length() as usize).map(|index| parent.element(index)),
                        );
                    }
                    Shape::Triple => children.extend([parent.car(), parent.cdr(), parent.tag()]),
                    Shape::Environment => {
                        children.push(parent.enclosure());
                        let frame = parent.frame_ptr();
                        if !frame.is_null() {
                            children.extend((*frame).cells());
                        }
                    }
                    Shape::ExternalPtr => {
                        children.extend([parent.external_tag(), parent.external_protected()])
                    }
                    Shape::Record => children.extend(
                        (0..parent.record_slot_count())
                            .filter_map(|slot| parent.record_handle(slot)),
                    ),
                    _ => {}
                }
            }
            for (position, child) in children.into_iter().enumerate() {
                if !self.space.is_marked(child) || younger(parent, child) {
                    panic!(
                        "write barrier violated: {} {:?} (length {}) holds {} {:?} at child {position}",
                        if tenured { "tenured" } else { "old" },
                        parent.kind(),
                        if parent.kind().shape() == Shape::Vector {
                            parent.length()
                        } else {
                            0
                        },
                        if self.space.is_marked(child) {
                            "untenured"
                        } else {
                            "young"
                        },
                        child.kind(),
                    );
                }
            }
        };
        // Only marked objects are old, and the marked slots are exactly the live old objects:
        // an allocated but unmarked slot may never have held one.
        for object in self.space.marked_objects() {
            check(object);
        }
        for &object in &self.large {
            check(object);
        }
    }

    /// Unmarks every object outside the oldest generation, as an intermediate collection starts.
    unsafe fn unmark_untenured(&mut self) {
        self.space.unmark_untenured();
        for &object in &self.large {
            if object.gc_bits() & gc_bits::TENURED == 0 {
                object.set_gc_bit(gc_bits::MARK, false);
            }
        }
    }

    unsafe fn clear_marks(&mut self) {
        self.space.clear_marks();
        for &object in &self.large {
            object.set_gc_bit(gc_bits::MARK, false);
        }
        for parent in self.remembered.drain(..) {
            parent.set_gc_bit(gc_bits::REMEMBERED, false);
        }
    }

    /// Moves every object into the oldest generation without tracing. Loading a start-up image
    /// allocates megabytes that live as long as the session, and every collection after it that
    /// is not major would otherwise trace all of them again only to keep them. The heap then
    /// holds little else than the image, so the few dead objects tenured with it wait for the
    /// next major collection.
    pub unsafe fn promote_young(&mut self) {
        if self.inhibit > 0 {
            return;
        }
        self.inhibit += 1;
        let mut cells = self.space.tenure_all();
        for &object in &self.large {
            let bits = gc_bits::MARK | gc_bits::OLD | gc_bits::TENURED;
            if object.gc_bits() & bits != bits {
                object.set_gc_bit(bits, true);
            }
            cells += crate::space::vector_cells(object);
        }
        // Every young string is marked, so this only resets the young bookkeeping.
        self.strings.remove_dead_young(&self.space);
        self.sweep_young();
        for parent in self.remembered.drain(..) {
            parent.set_gc_bit(gc_bits::REMEMBERED, false);
        }
        self.young_bytes = 0;
        self.young_large_bytes = 0;
        self.refresh_small_trigger();
        self.tenured_vector_cells = cells;
        self.stats.live_vector_cells = cells;
        self.inhibit -= 1;
    }

    unsafe fn sweep_young(&mut self) {
        let mut swept = Swept::default();
        let change = self.space.sweep_young(&mut swept);
        self.old_small_bytes = self.old_small_bytes.saturating_add_signed(change);
        let young_large = std::mem::take(&mut self.young_large);
        for &object in &young_large {
            if object.gc_bits() & gc_bits::MARK != 0 {
                self.old_large_bytes += self.size_of(object);
            } else {
                self.free_large(object);
                swept.freed += 1;
            }
        }
        self.young_large = young_large;
        self.young_large.clear();
        self.stats.objects_freed += swept.freed;
        self.refresh_live_stats();
    }

    unsafe fn sweep_all(&mut self) {
        let mut swept = Swept::default();
        self.old_small_bytes = self.space.sweep_all(&mut swept);
        let mut old_large_bytes = 0;
        let large: Vec<Sexp> = self.large.iter().copied().collect();
        for object in large {
            if object.gc_bits() & gc_bits::MARK != 0 {
                old_large_bytes += self.size_of(object);
            } else {
                self.free_large(object);
                swept.freed += 1;
            }
        }
        self.young_large.clear();
        self.old_large_bytes = old_large_bytes;
        self.stats.objects_freed += swept.freed;
        self.refresh_live_stats();
    }

    fn refresh_live_stats(&mut self) {
        self.stats.live_objects = (self.space.live_slots() + self.large.len()) as u64;
        self.stats.live_bytes = self.old_bytes() as u64;
        self.stats.large_objects = self.large.len() as u64;
        self.stats.pages = self.space.page_count() as u64;
    }

    unsafe fn size_of(&self, object: Sexp) -> usize {
        let class = object.size_class();
        if class != LARGE_CLASS {
            return SLOT_SIZES[class as usize];
        }
        let length = match object.kind() {
            SexpType::Char => object.length() as usize,
            SexpType::Record => object.record_slot_count(),
            _ => object.capacity() as usize,
        };
        object_size(object.kind(), length).unwrap_or(0)
    }

    unsafe fn free_large(&mut self, object: Sexp) {
        let size = self.size_of(object);
        self.large.remove(&object);
        let memory = object.as_ptr().cast::<u8>();
        if size >= MAPPED_BYTES {
            self.mapped.release(NonNull::new_unchecked(memory), size);
        } else {
            std::alloc::dealloc(memory, Layout::from_size_align_unchecked(size, 16));
        }
    }

    unsafe fn create_well_known(&mut self) {
        let nil = self
            .allocate(
                SexpType::Nil,
                object_size(SexpType::Nil, 0).unwrap_or(40),
                &[],
            )
            .unwrap_or_else(|error| panic!("cannot create R_NilValue: {error}"));
        nil.set_car_unbarriered(nil);
        nil.set_cdr_unbarriered(nil);
        nil.set_tag_unbarriered(nil);
        // The nil object is created first, so its attributes pointed at a placeholder.
        nil.set_attrib_unbarriered(nil);
        nil.mark_not_mutable();
        self.known.nil = nil;
        let result = (|| -> HeapResult<()> {
            let blank_string = self.mk_char(b"", Encoding::Native)?;
            self.known.blank_string = blank_string;
            self.add_global_root(blank_string);
            // NA_STRING is a CHARSXP of its own that is never interned, so "NA" and NA differ.
            let na_string = self.new_char(b"NA", gp::ASCII)?;
            self.known.na_string = na_string;
            let unbound = self.alloc_triple(SexpType::Symbol, nil, nil, nil)?;
            self.known.unbound = unbound;
            let missing_arg = self.alloc_triple(SexpType::Symbol, blank_string, nil, nil)?;
            self.known.missing_arg = missing_arg;
            let blank_scalar = self.alloc_vector(SexpType::String, 1)?;
            blank_scalar.set_element_unbarriered(0, blank_string);
            self.known.blank_scalar_string = blank_scalar;
            let empty_env = self.alloc_environment(nil)?;
            self.known.empty_env = empty_env;
            let base_env = self.alloc_environment(empty_env)?;
            self.known.base_env = base_env;
            let global_env = self.alloc_environment(base_env)?;
            self.known.global_env = global_env;
            self.set_frame_hashed(global_env, 0);
            let base_namespace = self.alloc_environment(global_env)?;
            self.known.base_namespace = base_namespace;
            let namespace_registry = self.alloc_environment(nil)?;
            self.known.namespace_registry = namespace_registry;
            let mut logical = |value: i32| -> HeapResult<Sexp> {
                let object = self.alloc_vector_inhibited(SexpType::Logical, 1)?;
                *(object.data() as *mut i32) = value;
                Ok(object)
            };
            let (true_value, false_value, na_value) =
                (logical(1)?, logical(0)?, logical(NA_LOGICAL)?);
            self.known.true_value = true_value;
            self.known.false_value = false_value;
            self.known.logical_na_value = na_value;
            for object in self.known.all() {
                object.mark_not_mutable();
            }
            Ok(())
        })();
        if let Err(error) = result {
            panic!("cannot create the well-known R objects: {error}");
        }
    }

    unsafe fn alloc_vector_inhibited(&mut self, kind: SexpType, length: isize) -> HeapResult<Sexp> {
        let size = object_size(kind, length as usize).ok_or(HeapError::TooLong)?;
        let object = self.allocate(kind, size, &[])?;
        object.set_length_and_capacity(length, length);
        Ok(object)
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // SAFETY: the heap owns every large object, and nothing outlives it. The space drops
        // its own objects.
        unsafe {
            let large: Vec<Sexp> = self.large.iter().copied().collect();
            for object in large {
                self.free_large(object);
            }
        }
    }
}

impl WellKnown {
    fn placeholder() -> WellKnown {
        let dangling = Sexp::from_non_null(NonNull::dangling());
        WellKnown {
            nil: dangling,
            unbound: dangling,
            missing_arg: dangling,
            na_string: dangling,
            blank_string: dangling,
            blank_scalar_string: dangling,
            empty_env: dangling,
            base_env: dangling,
            base_namespace: dangling,
            global_env: dangling,
            namespace_registry: dangling,
            true_value: dangling,
            false_value: dangling,
            logical_na_value: dangling,
        }
    }

    fn all(&self) -> [Sexp; 14] {
        [
            self.nil,
            self.unbound,
            self.missing_arg,
            self.na_string,
            self.blank_string,
            self.blank_scalar_string,
            self.empty_env,
            self.base_env,
            self.base_namespace,
            self.global_env,
            self.namespace_registry,
            self.true_value,
            self.false_value,
            self.logical_na_value,
        ]
    }
}

/// Whether storing `child` into `parent`, an old object, makes the parent point below its own
/// generation: at a young object, or from a tenured parent at an untenured one. The parent's
/// generation bits must all be set in the child's.
#[inline(always)]
unsafe fn younger(parent: Sexp, child: Sexp) -> bool {
    parent.gc_bits() & (gc_bits::OLD | gc_bits::TENURED) & !child.gc_bits() != 0
}

/// Whether tracing `object` would mark nothing: a CHARSXP or an atomic vector (logical, integer,
/// double, complex or raw) that has no attributes and is not ALTREP.
/// Such an object is marked without a trip through the mark stack, which a string vector with a
/// million distinct elements would otherwise grow by a million entries, and the stack keeps its
/// capacity for the rest of the session.
#[inline]
unsafe fn is_leaf(object: Sexp, nil: Sexp) -> bool {
    matches!(
        object.kind(),
        SexpType::Char
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::I64
            | SexpType::Complex
            | SexpType::Raw
    ) && !object.is_altrep()
        && object.attrib() == nil
}

impl Default for GcPolicy {
    fn default() -> Self {
        GcPolicy {
            // A larger nursery collects less often but touches more fresh memory before its
            // first collection, and first touches are what a short-lived process pays for most.
            young_limit: 8 * 1024 * 1024,
            // Counts only large vectors allocated since the last collection, unlike GNU R's
            // 64 MB `R_VSIZE`, which also counts live data. A 64 MB budget let dead temporaries
            // of vectorized kernels pile up to about 50 MB above GNU R's peak at a million rows.
            young_large_limit: 16 * 1024 * 1024,
            // GNU R's default `R_VSIZE`.
            heap_limit: 64 * 1024 * 1024,
            min_heap_limit: 64 * 1024 * 1024,
            // About twice GNU R's default `R_NSIZE` (350,000 cells of 56 bytes), because ry's
            // base packages take more small-object bytes than GNU R's cons cells and a smaller
            // limit only buys full collections while they load.
            node_limit: 32 * 1024 * 1024,
            min_node_limit: 32 * 1024 * 1024,
            growth: HeapGrowth::from_environment(|name| std::env::var(name).ok()),
            stress: match gc_stress_requested() {
                Some(GcStressStart::Startup) => GcStress::EveryAllocation,
                _ => GcStress::Off,
            },
            verify: std::env::var_os("RY_GC_VERIFY").is_some(),
            timing: false,
            reporter: None,
        }
    }
}

impl ProtectStack {
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    pub fn base(&self) -> usize {
        self.base
    }

    /// Starts a native call's protection region and returns the previous base, which the call
    /// restores with [`ProtectStack::leave`].
    pub fn enter(&mut self) -> usize {
        std::mem::replace(&mut self.base, self.stack.len())
    }

    /// Ends a native call's region, dropping whatever it left protected, and reports how deep
    /// the stack was before it was cut back, so an imbalance can be reported.
    pub fn leave(&mut self, saved_base: usize) -> usize {
        let depth = self.stack.len();
        self.stack.truncate(self.base);
        self.base = saved_base;
        depth
    }

    /// Cuts the stack back to `depth`, as a jump to an enclosing context does.
    pub fn restore(&mut self, depth: usize) {
        self.stack.truncate(depth.max(self.base));
    }

    pub fn protect(&mut self, object: Sexp) -> HeapResult<usize> {
        if self.stack.len() >= self.limit {
            return Err(HeapError::ProtectOverflow);
        }
        self.stack.push(object);
        Ok(self.stack.len() - 1)
    }

    pub fn unprotect(&mut self, count: usize) -> HeapResult<()> {
        let available = self.stack.len() - self.base;
        if count > available {
            return Err(HeapError::UnprotectUnderflow {
                available,
                requested: count,
            });
        }
        self.stack.truncate(self.stack.len() - count);
        Ok(())
    }

    /// `UNPROTECT_PTR`: removes the topmost protection of `object` wherever it is.
    pub fn unprotect_object(&mut self, object: Sexp) -> HeapResult<()> {
        let position = self.stack.entries[self.base..]
            .iter()
            .rposition(|&protected| protected == object)
            .ok_or(HeapError::UnprotectPointerNotFound)?;
        self.stack
            .remove_range(self.base + position, self.base + position + 1);
        Ok(())
    }

    pub fn reprotect(&mut self, object: Sexp, index: usize) -> HeapResult<()> {
        let available = self.stack.len();
        let slot = self
            .stack
            .entries
            .get_mut(index)
            .ok_or(HeapError::ReprotectIndex { available, index })?;
        *slot = object;
        self.stack.scanned = self.stack.scanned.min(index);
        Ok(())
    }
}

impl RootStack {
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    fn push(&mut self, object: Sexp) {
        self.entries.push(object);
    }

    #[inline]
    fn truncate(&mut self, length: usize) {
        if length >= self.entries.len() {
            return;
        }
        self.entries.truncate(length);
        self.scanned = self.scanned.min(self.entries.len());
    }

    /// Removes `start..end`, which moves every later entry down, so none of those counts as
    /// scanned any more.
    fn remove_range(&mut self, start: usize, end: usize) {
        self.entries.drain(start..end);
        self.scanned = self.scanned.min(start);
    }
}

impl Marker<'_> {
    pub unsafe fn mark(&mut self, object: Sexp) {
        match &mut self.space {
            Some(space) => {
                if space.mark(object) {
                    self.stack.push(object);
                }
            }
            None => self.stack.push(object),
        }
    }
}

impl Sexp {
    unsafe fn set_length_and_capacity(self, length: isize, capacity: isize) {
        self.set_length(length);
        self.set_capacity(capacity);
    }
}

/// Entries the zero-count table gathers before a reconcile bounds it, however little they would
/// free.
const RECONCILE_ENTRIES: usize = 1 << 16;

/// Large-vector bytes the zero-count table must stand to free before a reconcile runs for them.
/// A package's test session has about 20,000 roots, which take about 100 µs to scan, so a
/// reconcile pays for itself only when it frees megabytes.
const RECONCILE_BYTES: usize = 4 << 20;

/// Whether a reconcile can reclaim `object` once nothing holds it: a list, by giving up its
/// elements, or a large atomic vector, by freeing it. ALTREP objects keep their own data.
unsafe fn reclaimable(object: Sexp) -> bool {
    !object.is_altrep()
        && match object.kind() {
            SexpType::List => true,
            SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Raw => object.size_class() == LARGE_CLASS,
            _ => false,
        }
}

/// Every type code a vector can have, for generators and tests.
pub const VECTOR_TYPES: [SexpType; 9] = [
    SexpType::Logical,
    SexpType::Integer,
    SexpType::Real,
    SexpType::I64,
    SexpType::Complex,
    SexpType::String,
    SexpType::List,
    SexpType::Expression,
    SexpType::Raw,
];

const _: () = assert!(VECTOR_DATA_OFFSET.is_multiple_of(16));
const _: () = assert!(REFCOUNT_MAX == u16::MAX);

/// Physical memory plus swap on Linux, where the allocator reserves address space without
/// committing it, so a vector far larger than the machine would be handed out and fail only when
/// touched. GNU R gets its "cannot allocate vector" error from the kernel's overcommit heuristic
/// refusing such a request, and this bound gives the same answer before asking. With
/// `vm.overcommit_memory` set to 1 the kernel grants any request, and so does GNU R, so there is
/// no bound; macOS and Windows also grant oversized requests to GNU R, so they have none either.
#[cfg(target_os = "linux")]
fn memory_ceiling() -> usize {
    let always_overcommit =
        std::fs::read("/proc/sys/vm/overcommit_memory").is_ok_and(|mode| mode.trim_ascii() == b"1");
    if always_overcommit {
        return usize::MAX;
    }
    // SAFETY: `sysinfo` only writes the struct it is given.
    let mut info: libc::sysinfo = unsafe { std::mem::zeroed() };
    if unsafe { libc::sysinfo(&mut info) } != 0 {
        return usize::MAX;
    }
    let unit = (info.mem_unit as usize).max(1);
    (info.totalram as usize)
        .saturating_add(info.totalswap as usize)
        .saturating_mul(unit)
}

#[cfg(not(target_os = "linux"))]
fn memory_ceiling() -> usize {
    usize::MAX
}

/// The process's processor time so far in user and in system mode.
#[cfg(unix)]
fn process_times() -> (Duration, Duration) {
    // SAFETY: `getrusage` only writes the struct it is given.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return (Duration::ZERO, Duration::ZERO);
    }
    let duration = |time: libc::timeval| {
        Duration::new(
            time.tv_sec.max(0) as u64,
            (time.tv_usec.max(0) as u32) * 1000,
        )
    };
    (duration(usage.ru_utime), duration(usage.ru_stime))
}

/// The process's processor time so far in user and in system mode.
#[cfg(windows)]
fn process_times() -> (Duration, Duration) {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    let empty = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (empty, empty, empty, empty);
    // SAFETY: `GetProcessTimes` only writes the four structs it is given.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return (Duration::ZERO, Duration::ZERO);
    }
    // A FILETIME counts 100-nanosecond intervals.
    let duration = |time: FILETIME| {
        Duration::from_nanos(
            ((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)) * 100,
        )
    };
    (duration(user), duration(kernel))
}

unsafe fn bump_symbol_epoch(symbol: Sexp) {
    let epoch = &symbol.symbol_payload().epoch;
    epoch.set(epoch.get() + 1);
}

/// Writes a fresh triple-shaped object's three fields, which a fresh object needs no write
/// barrier for, and a symbol's extra fields.
#[inline(always)]
unsafe fn fill_triple(object: Sexp, kind: SexpType, first: Sexp, second: Sexp, third: Sexp) {
    object.set_car_unbarriered(first);
    object.set_cdr_unbarriered(second);
    object.set_tag_unbarriered(third);
    first.increment_refcount();
    second.increment_refcount();
    third.increment_refcount();
    if kind == SexpType::Symbol {
        let symbol = object.as_ptr().cast::<SymbolObject>();
        std::ptr::addr_of_mut!((*symbol).epoch).write(Cell::new(0));
        std::ptr::addr_of_mut!((*symbol).global_cell).write(Cell::new(None));
        std::ptr::addr_of_mut!((*symbol).global_stamp).write(Cell::new(u64::MAX));
    }
}

/// The [`slot_kind`] bit of a record slot's value.
unsafe fn slot_kind_of(slot: Slot) -> u8 {
    match slot {
        Slot::Real(_) => slot_kind::DOUBLE,
        Slot::Integer(_) => slot_kind::INTEGER,
        Slot::Logical(_) => slot_kind::LOGICAL,
        Slot::Handle(handle) => unsafe {
            if handle.kind() == SexpType::Record && handle.attrib().kind() == SexpType::Nil {
                slot_kind::RECORD
            } else {
                slot_kind::OTHER
            }
        },
    }
}
