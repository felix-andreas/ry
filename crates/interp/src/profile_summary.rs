//! `ry run --profile-summary FILE`: a profile of where a run spends its time that holds no
//! source code, identifiers or data, so a user can share one from a private code base.
//!
//! The R thread publishes what it is doing through [`heap::activity`]; a sampling thread reads
//! that word every millisecond and counts the values it sees. The summary reports the shares of
//! those samples together with the event counters the hooks keep. The only names it prints are
//! base R's builtins and the packages that own native code; a package outside the public list
//! below is printed as a hash of its name, keyed with a secret ([`private_key`]).
//!
//! A sample also belongs to a call stack, which only the R thread can read. The sampling thread
//! therefore queues each sample and sets [`activity::SAMPLE_TICK`], and the R thread files the
//! queued samples under its frames the next time its stack is about to change
//! ([`resolve_samples`]): the stack it reads then is the one every queued sample was taken in. A
//! call pays for this with the test of the hook flags it makes anyway, so a summarized run takes
//! the same call paths as any other.
//!
//! The same sampler measures benchmark regions: `.ry_bench_region(name)` marks where a region
//! starts (and the one before it ends), and `RY_PROFILE_REGIONS` names a file that gets one JSON
//! line per region with its wall time, its samples by activity and its event counts.

use {
    crate::{
        Frame, FrameRole,
        builtins::{BuiltinDef, Call, builtin},
        callgrind, interp,
        unwind::{R, error},
        values,
    },
    heap::{
        activity::{self, Activity, Counter},
        object::{Header, Sexp, SexpType},
    },
    rustc_hash::FxHashMap,
    siphasher::sip::{SipHasher13, SipHasher24},
    smallvec::SmallVec,
    std::{
        ffi::CString,
        fmt::Write as _,
        hash::Hasher,
        io::{self, Write as _},
        path::{Path, PathBuf},
        sync::{
            Mutex, TryLockError,
            atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        },
        thread::JoinHandle,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
};

/// The environment variable that names the summary file; `--profile-summary` sets it.
pub const VARIABLE: &str = "RY_PROFILE_SUMMARY";

/// The environment variable that names a directory where every session writes a summary of its
/// own process. Child processes inherit it, so a test suite that starts workers gets one summary
/// per worker.
pub const DIRECTORY_VARIABLE: &str = "RY_PROFILE_SUMMARY_DIR";

/// The environment variable that names the file the benchmark regions are written to.
pub const REGIONS_VARIABLE: &str = "RY_PROFILE_REGIONS";

/// The environment variable that names a file for every sampled call stack with its function
/// names, for comparing a run function by function against GNU R's `Rprof` output
/// (`tools/bench/compare_profiles.py`). Unlike the summary, this file names private code, so it is
/// written only when asked for, and only together with a summary.
pub const STACKS_VARIABLE: &str = "RY_PROFILE_STACKS";

/// The environment variable that sets the sampling interval in microseconds (default 1000).
pub const INTERVAL_VARIABLE: &str = "RY_PROFILE_INTERVAL_US";

/// The environment variable that gives the key private package names are hashed with, as 32
/// hexadecimal digits, in place of the key file [`private_key`] keeps.
pub const KEY_VARIABLE: &str = "RY_PROFILE_KEY";

/// The sampling interval in microseconds, fixed when sampling starts.
static INTERVAL_MICROS: AtomicU64 = AtomicU64::new(1000);

struct Sampler {
    /// The summary file, when a summary is collected.
    path: Option<PathBuf>,
    /// The regions file, when regions are measured.
    regions: Option<PathBuf>,
    /// The stacks file, when stacks are kept.
    stacks: Option<PathBuf>,
    /// The process that started sampling. A forked child inherits this record but not the
    /// sampling thread, so it writes nothing.
    process: u32,
    /// The directory the session started in, which decides the project whose key hashes
    /// private package names.
    started_in: Option<PathBuf>,
    started: Instant,
    thread: JoinHandle<Samples>,
}

#[derive(Default)]
struct Samples {
    /// Samples taken before the session finished starting, whatever they show.
    startup: u64,
    /// Samples by activity word.
    words: FxHashMap<u32, u64>,
    /// Samples by closure, keyed by owner label and symbol address.
    closures: FxHashMap<(u32, usize), Attribution>,
    /// Samples by package label.
    packages: FxHashMap<u32, PackageTime>,
    /// Samples by the body of the innermost closure and by category; body 0 is time outside any
    /// closure. The entry count that decides a body's bucket is known only when the run ends.
    bodies: FxHashMap<usize, [u64; Activity::ALL.len()]>,
    /// Samples by activity word for each benchmark region, indexed like [`Regions::names`].
    regions: Vec<FxHashMap<u32, u64>>,
    /// Samples by whole stack (owner and name per frame, outermost first) and activity word,
    /// kept only when [`STACKS_VARIABLE`] names a file.
    stacks: Option<FxHashMap<(Vec<(u32, usize)>, u32), u64>>,
}

#[derive(Default, Clone, Copy)]
struct Attribution {
    /// Samples where this closure is the innermost frame.
    own: u64,
    /// Samples where this closure is anywhere on the stack.
    inclusive: u64,
}

#[derive(Default, Clone, Copy)]
struct PackageTime {
    /// Samples running the package's R code: tier 0, the evaluator, compiled code and builtins
    /// called from it.
    code: u64,
    /// Samples in the package's native library.
    native: u64,
    /// Samples the interpreter spends on the package's behalf: call setup, lookups, method
    /// dispatch, collection and compilation.
    overhead: u64,
    /// Samples where the package is anywhere on the stack.
    inclusive: u64,
}

impl Samples {
    /// Files `count` samples of the activity `word`, all taken in the stack whose frames, from
    /// the outermost, have these owner labels, name symbols and bodies.
    fn record(
        &mut self,
        word: u32,
        count: u64,
        owners: &[u32],
        names: &[usize],
        bodies: &[usize],
        global: u32,
    ) {
        *self.words.entry(word).or_insert(0) += count;
        if let Some(stacks) = &mut self.stacks {
            let frames = owners.iter().copied().zip(names.iter().copied()).collect();
            *stacks.entry((frames, word)).or_insert(0) += count;
        }
        self.bodies
            .entry(bodies.last().copied().unwrap_or(0))
            .or_insert([0; Activity::ALL.len()])[category(word) as usize] += count;
        let innermost_owner = owners.last().copied().unwrap_or(global);
        let innermost_name = names.last().copied().unwrap_or(0);
        if !owners.is_empty() {
            self.closures
                .entry((innermost_owner, innermost_name))
                .or_default()
                .own += count;
        }
        let activity = Activity::from_word(word);
        let (package, slot) = match activity {
            Activity::Native => (word >> 8, Slot::Native),
            Activity::CallSetup
            | Activity::Lookup
            | Activity::S3Dispatch
            | Activity::S4Dispatch
            | Activity::Collector
            | Activity::JitCompile
            | Activity::Profiler => (innermost_owner, Slot::Overhead),
            _ => (innermost_owner, Slot::Code),
        };
        let time = self.packages.entry(package).or_default();
        match slot {
            Slot::Code => time.code += count,
            Slot::Native => time.native += count,
            Slot::Overhead => time.overhead += count,
        }
        // Stacks are short, so a linear scan for an earlier occurrence beats building a set.
        let mut seen_packages: SmallVec<[u32; 16]> = SmallVec::new();
        seen_packages.push(package);
        for (index, (&owner, &name)) in owners.iter().zip(names).enumerate() {
            let earlier = owners[..index]
                .iter()
                .zip(&names[..index])
                .any(|(&other_owner, &other_name)| other_owner == owner && other_name == name);
            if !earlier {
                self.closures.entry((owner, name)).or_default().inclusive += count;
            }
            if !seen_packages.contains(&owner) {
                seen_packages.push(owner);
            }
        }
        if owners.is_empty() && !seen_packages.contains(&global) {
            seen_packages.push(global);
        }
        for package in seen_packages {
            self.packages.entry(package).or_default().inclusive += count;
        }
    }
}

enum Slot {
    Code,
    Native,
    Overhead,
}

static SAMPLER: Mutex<Option<Sampler>> = Mutex::new(None);
static STOP: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static READY_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The samples the sampling thread took since the R thread last filed them, counted by activity
/// word. The R thread's stack is the same for every waiting sample, because it files them
/// before the stack changes, so their order carries nothing and a long stretch without a call
/// or a return fills one slot per distinct activity.
///
/// A slot holds an activity word above a count of samples, and zero while it is free. The
/// sampling thread adds to a slot and the R thread takes it, with no lock between them: a
/// forked child inherits a lock in the state the fork found it and no sampling thread to
/// release it, so a lock the sampling thread could hold would stop the child at its first call.
static WAITING: [AtomicU64; WAITING_SLOTS] = [const { AtomicU64::new(0) }; WAITING_SLOTS];
const WAITING_SLOTS: usize = 1 << 8;

/// The most frames a sample names, counted from the innermost: filing walks them at every
/// tick, which a recursion thousands of frames deep would otherwise pay for.
const SAMPLED_FRAMES: usize = 1024;

/// The samples of a summary by call stack, while one is collected. Only the R thread reads or
/// writes it.
static STACKED: Mutex<Option<Stacked>> = Mutex::new(None);

struct Stacked {
    /// The samples filed under the stack they were taken in.
    samples: Samples,
    /// Owner labels by environment address. Namespaces and the global environment live for the
    /// whole session, so an address names one owner; another top level (an environment
    /// `sys.source` used) can be freed and its address reused, which at worst merges two such
    /// owners under "other".
    owners: FxHashMap<usize, u32>,
    /// The label of the global environment, which owns a sample taken outside every closure.
    global: u32,
}
/// The benchmark region running now, as its index in [`Regions::names`] plus one; zero outside
/// any region. The sampler reads it to file each sample under its region.
static REGION: AtomicU32 = AtomicU32::new(0);

/// The benchmark regions marked by `.ry_bench_region`, in the order they first started.
struct Regions {
    names: Vec<String>,
    /// Wall time spent in each region, summed over its entries.
    seconds: Vec<Duration>,
    /// Each region's event counts, summed over its entries.
    counters: Vec<[u64; Counter::ALL.len()]>,
    /// The open region: its index, when it started and the event counts at that point.
    open: Option<(usize, Instant, [u64; Counter::ALL.len()])>,
}

static REGIONS: Mutex<Regions> = Mutex::new(Regions {
    names: Vec::new(),
    seconds: Vec::new(),
    counters: Vec::new(),
    open: None,
});

/// Starts sampling when [`VARIABLE`] or [`DIRECTORY_VARIABLE`] asks for a summary or
/// [`REGIONS_VARIABLE`] for benchmark regions.
pub fn start_from_environment() {
    let path = if let Some(path) = std::env::var_os(VARIABLE).filter(|path| !path.is_empty()) {
        // Subprocesses (callr, parallel's socket clusters) would overwrite the file with a
        // summary of themselves, so only this process profiles.
        // SAFETY: the session starts before R code can start threads that read the environment.
        unsafe { std::env::remove_var(VARIABLE) };
        Some(PathBuf::from(path))
    } else if let Some(directory) =
        std::env::var_os(DIRECTORY_VARIABLE).filter(|directory| !directory.is_empty())
    {
        // Process ids are reused over a long CI run, so the start time keeps names apart.
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        Some(
            PathBuf::from(directory)
                .join(format!("ry-profile-{started}-{}.md", std::process::id())),
        )
    } else {
        None
    };
    let regions = std::env::var_os(REGIONS_VARIABLE)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if regions.is_some() {
        // SAFETY: as above.
        unsafe { std::env::remove_var(REGIONS_VARIABLE) };
    }
    if path.is_none() && regions.is_none() {
        return;
    }
    if let Some(micros) = std::env::var(INTERVAL_VARIABLE)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .filter(|&micros| micros > 0)
    {
        INTERVAL_MICROS.store(micros, Ordering::Relaxed);
    }
    let interval = Duration::from_micros(INTERVAL_MICROS.load(Ordering::Relaxed));
    // Both files are written when the session ends, after R code may have changed the working
    // directory (into `tempdir()`, which the session then deletes), so a relative path is fixed
    // against the directory the session started in.
    let path = path.map(|path| std::path::absolute(&path).unwrap_or(path));
    let regions = regions.map(|path| std::path::absolute(&path).unwrap_or(path));
    let stacks = std::env::var_os(STACKS_VARIABLE)
        .filter(|stacks| !stacks.is_empty())
        .map(|stacks| {
            // SAFETY: as above, no R code has started a thread yet.
            unsafe { std::env::remove_var(STACKS_VARIABLE) };
            let stacks = PathBuf::from(stacks);
            std::path::absolute(&stacks).unwrap_or(stacks)
        })
        .filter(|_| path.is_some());
    let stacked = path.is_some();
    if stacked {
        *lock(&STACKED) = Some(Stacked {
            samples: Samples {
                stacks: stacks.is_some().then(FxHashMap::default),
                ..Samples::default()
            },
            owners: FxHashMap::default(),
            global: activity::label(GLOBAL_LABEL),
        });
    }
    if let Some(period) = std::env::var("RY_ALLOC_SITES")
        .ok()
        .and_then(|period| period.parse::<u64>().ok())
    {
        activity::set_allocation_period(period);
    }
    activity::set_enabled(true);
    let thread = std::thread::spawn(move || {
        let mut samples = Samples::default();
        while !STOP.load(Ordering::Relaxed) {
            std::thread::sleep(interval);
            if !READY.load(Ordering::Relaxed) {
                samples.startup += 1;
                continue;
            }
            let word = activity::current();
            if stacked {
                if !queue_sample(word) {
                    *samples.words.entry(word).or_insert(0) += 1;
                }
                activity::set_hook(activity::SAMPLE_TICK, true);
            }
            if let Some(region) = (REGION.load(Ordering::Relaxed) as usize).checked_sub(1) {
                if samples.regions.len() <= region {
                    samples.regions.resize_with(region + 1, FxHashMap::default);
                }
                *samples.regions[region].entry(word).or_insert(0) += 1;
            }
        }
        samples
    });
    *lock(&SAMPLER) = Some(Sampler {
        path,
        regions,
        stacks,
        process: std::process::id(),
        started_in: std::env::current_dir().ok(),
        started: Instant::now(),
        thread,
    });
}

/// Whether the sampler runs.
pub fn sampling() -> bool {
    lock(&SAMPLER).is_some()
}

/// The builtin that marks benchmark regions.
pub const BUILTINS: &[BuiltinDef] =
    &[builtin(".ry_bench_region", 1, do_ry_bench_region).invisible()];

/// `.ry_bench_region(name)`: ends the open benchmark region and starts the one called `name`;
/// an empty name starts none. Starting a region again adds to its totals.
unsafe fn do_ry_bench_region(call: &Call<'_>) -> R<Sexp> {
    let name = unsafe { values::require_str(call.arg(0), "name")? };
    switch_region(&name);
    Ok(values::nil())
}

/// Closes the open region and opens the one called `name`, unless `name` is empty. Under
/// callgrind each region also becomes its own dump, named after the region.
fn switch_region(name: &str) {
    let mut regions = lock(&REGIONS);
    if let Some((index, started, before)) = regions.open.take() {
        REGION.store(0, Ordering::Relaxed);
        let elapsed = started.elapsed();
        regions.seconds[index] += elapsed;
        for ((total, after), before) in regions.counters[index]
            .iter_mut()
            .zip(counters())
            .zip(before)
        {
            *total += after.saturating_sub(before);
        }
        // A name with a NUL byte cannot reach callgrind whole, so the dump drops those bytes.
        let dump = CString::new(regions.names[index].replace('\0', "")).unwrap_or_default();
        callgrind::end_region(&dump);
    }
    if name.is_empty() {
        return;
    }
    let index = match regions.names.iter().position(|known| known == name) {
        Some(index) => index,
        None => {
            regions.names.push(name.to_owned());
            regions.seconds.push(Duration::ZERO);
            regions.counters.push([0; Counter::ALL.len()]);
            regions.names.len() - 1
        }
    };
    regions.open = Some((index, Instant::now(), counters()));
    REGION.store(index as u32 + 1, Ordering::Relaxed);
    callgrind::start_region();
}

fn counters() -> [u64; Counter::ALL.len()] {
    Counter::ALL.map(activity::counter)
}

/// The label of code whose top level is the global environment: scripts and the console.
const GLOBAL_LABEL: &str = "R_GlobalEnv";

/// What a closure call holds while boundary calls are recorded: its record, until the scope
/// drops.
pub struct ClosureScope(Option<crate::capture::Scope>);

impl ClosureScope {
    /// Hands the call's result to the boundary-call recorder before the scope drops.
    #[inline(always)]
    pub unsafe fn returned(&mut self, result: &R<Sexp>) {
        if let Some(capture) = &mut self.0 {
            unsafe { capture.returned(result) };
        }
    }
}

/// Records a call of the closure `function` through `call` as a boundary call while those are
/// recorded: its caller's frame is `sys_parent`, and the dispatch variables of a method are in
/// `method_vars` or bound in its frame environment `env` (`NULL` for a slot frame). Every
/// closure call on the general path passes here, so with the recorder off it costs one test of
/// the hook flags.
#[inline(always)]
pub unsafe fn enter_closure(
    call: Sexp,
    function: Sexp,
    sys_parent: Sexp,
    method_vars: &[(Sexp, Sexp)],
    env: Sexp,
) -> ClosureScope {
    if activity::hooks() & activity::CAPTURE == 0 {
        return ClosureScope(None);
    }
    ClosureScope(Some(unsafe {
        crate::capture::enter(call, function, sys_parent, method_vars, env)
    }))
}

/// What a summary records of the frame the general path has just pushed for a call of the
/// closure `function`: the samples that waited, filed under the `beneath` frames the stack held
/// before the push, and, for a call that is `counted`, the call and an entry of its body. A
/// frame that is not counted stands for a call something else counted (an inlined call that
/// compiled code handed over) or for none (compiled typed code's frame for a call it makes).
/// The caller has checked [`activity::enabled`]. A call on tier 0's lean path counts in its code
/// instead ([`crate::bytecode::profile::CALLS`]).
#[cold]
#[inline(never)]
pub(crate) unsafe fn entering(function: Sexp, beneath: usize, counted: bool) {
    if activity::sample_ticked() {
        unsafe { resolve_samples(beneath) };
    }
    if counted {
        activity::record(Counter::ClosureCalls);
        unsafe { crate::bytecode::count_body_entry(function) };
    }
}

/// Counts a call of the closure `function` that compiled code runs inline, without a frame,
/// while a profile summary is collected.
pub unsafe fn count_inlined_call(function: Sexp) {
    activity::record(Counter::InlinedCalls);
    unsafe { crate::bytecode::count_body_entry(function) };
}

/// Files the samples the sampling thread queued under the first `depth` frames of the R
/// thread's call stack. Whatever pushes or pops a frame calls this when
/// [`activity::sample_ticked`], with the depth the stack has without that change, so every
/// queued sample was taken in the stack read here.
#[cold]
#[inline(never)]
pub(crate) unsafe fn resolve_samples(depth: usize) {
    // Naming a frame's package can run R code (a namespace's `spec` behind an active binding),
    // whose calls come back here: they leave the samples to the filing that is under way.
    let mut guard = match STACKED.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    let Some(stacked) = guard.as_mut() else {
        return;
    };
    let running = activity::current();
    activity::publish(Activity::Profiler);
    // Cleared before the slots are read: a sample queued after its slot was read sets the flag
    // again.
    activity::set_hook(activity::SAMPLE_TICK, false);
    let mut waiting: SmallVec<[u64; 8]> = SmallVec::new();
    for slot in &WAITING {
        if slot.load(Ordering::SeqCst) != 0 {
            waiting.push(slot.swap(0, Ordering::SeqCst));
        }
    }
    if !waiting.is_empty() {
        let mut owners: SmallVec<[u32; 64]> = SmallVec::new();
        let mut names: SmallVec<[usize; 64]> = SmallVec::new();
        let mut bodies: SmallVec<[usize; 64]> = SmallVec::new();
        let frames = &interp().frames;
        for index in depth.saturating_sub(SAMPLED_FRAMES)..depth.min(frames.len()) {
            let frame = &frames[index];
            if frame.role == FrameRole::Context || frame.function.raw().kind() != SexpType::Closure
            {
                continue;
            }
            let (owner, name, body) = unsafe { frame_identity(stacked, frame) };
            owners.push(owner);
            names.push(name);
            bodies.push(body);
        }
        let global = stacked.global;
        for held in waiting {
            stacked.samples.record(
                (held >> 32) as u32,
                held & u64::from(u32::MAX),
                &owners,
                &names,
                &bodies,
                global,
            );
        }
    }
    drop(guard);
    activity::restore(running);
}

/// Counts one sample of the activity `word` for the R thread, on the sampling thread: in the
/// slot that holds the word, or in a free one. Answers false when every slot holds another
/// word, which takes hundreds of distinct activities without one call or return.
fn queue_sample(word: u32) -> bool {
    let start = (word.wrapping_mul(0x9E37_79B9) >> 24) as usize;
    for probe in 0..WAITING_SLOTS {
        let slot = &WAITING[(start + probe) % WAITING_SLOTS];
        let mut held = slot.load(Ordering::SeqCst);
        loop {
            // The exchange fails against a slot the R thread takes meanwhile, so no sample is
            // added to a count that was already filed.
            let counted = if held == 0 {
                u64::from(word) << 32 | 1
            } else if (held >> 32) as u32 == word && held as u32 != u32::MAX {
                held + 1
            } else {
                break;
            };
            match slot.compare_exchange_weak(held, counted, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return true,
                Err(current) => held = current,
            }
        }
    }
    false
}

/// What a sample records of `frame`: the label of the package that owns its closure, the symbol
/// the call names it by (zero for a call of anything else), and its body.
unsafe fn frame_identity(stacked: &mut Stacked, frame: &Frame) -> (u32, usize, usize) {
    let function = frame.function.raw();
    let owner = unsafe { crate::dispatch::top_env(function.tag()) };
    let body = unsafe { crate::bytecode::closure_body(function) }.as_ptr() as usize;
    let owner = *stacked
        .owners
        .entry(owner.as_ptr() as usize)
        .or_insert_with(|| {
            let name = if owner == values::global_env() {
                GLOBAL_LABEL.to_owned()
            } else {
                unsafe { crate::envir::namespace_name(owner) }.unwrap_or_else(|| "other".to_owned())
            };
            activity::label(&name)
        });
    // `pkg::f()`, `obj$f()` and `obj@f()` take the name `f`; GNU R's `Rprof()` writes these as
    // `pkg::f`, `obj$f` and `obj@f`, and `tools/bench/compare_profiles.py` reduces those to `f` so
    // the two engines' rows match.
    let mut head = frame.call.raw().car();
    if head.kind() == SexpType::Language
        && head.car().kind() == SexpType::Symbol
        && matches!(
            &*unsafe { values::symbol_name(head.car()) },
            "::" | ":::" | "$" | "@"
        )
    {
        head = head.cdr().cdr().car();
    }
    let name = if head.kind() == SexpType::Symbol {
        head.as_ptr() as usize
    } else {
        0
    };
    (owner, name, body)
}

/// Marks the end of start-up: samples after this count toward the time split.
pub fn mark_ready() {
    if activity::enabled() {
        *lock(&READY_AT) = Some(Instant::now());
        READY.store(true, Ordering::Relaxed);
    }
}

/// Closes the open benchmark region, stops sampling, and writes the summary and the regions file
/// that were asked for.
pub fn finish() -> R<()> {
    switch_region("");
    let Some(sampler) = lock(&SAMPLER).take() else {
        return Ok(());
    };
    if sampler.process != std::process::id() {
        activity::set_enabled(false);
        return Ok(());
    }
    STOP.store(true, Ordering::Relaxed);
    let elapsed = sampler.started.elapsed();
    let sampled = sampler
        .thread
        .join()
        .map_err(|_| error("the profile summary's sampling thread panicked"))?;
    // The samples still waiting are filed under the stack the session ends in.
    unsafe { resolve_samples(interp().frames.len()) };
    activity::set_enabled(false);
    let samples = match lock(&STACKED).take() {
        Some(mut stacked) => {
            // What the sampling thread could not queue belongs to no stack it can name.
            for (&word, &count) in &sampled.words {
                stacked
                    .samples
                    .record(word, count, &[], &[], &[], stacked.global);
            }
            Samples {
                startup: sampled.startup,
                regions: sampled.regions,
                ..stacked.samples
            }
        }
        None => sampled,
    };
    if let Some(path) = &sampler.regions {
        write_file(path, &regions_report(&samples), "the benchmark regions")?;
    }
    let Some(path) = &sampler.path else {
        return Ok(());
    };
    let ready = lock(&READY_AT).map(|ready| ready.duration_since(sampler.started));
    let key = private_key(sampler.started_in.as_deref()).map_err(error)?;
    let entries = unsafe { crate::bytecode::take_body_entries() };
    let text = report(&samples, &entries, elapsed, ready, &key)
        .map_err(|_| error("cannot format the profile summary"))?;
    write_file(path, &text, "the profile summary")?;
    if let (Some(path), Some(stacks)) = (&sampler.stacks, &samples.stacks) {
        write_file(path, &stack_lines(stacks), "the profile stacks")?;
    }
    Ok(())
}

fn write_file(path: &Path, text: &str, what: &str) -> R<()> {
    let written = match path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => {
            std::fs::create_dir_all(directory).and_then(|()| std::fs::write(path, text))
        }
        _ => std::fs::write(path, text),
    };
    written.map_err(|failure| {
        error(format!(
            "cannot write {what} to '{}': {failure}",
            path.display()
        ))
    })
}

/// One line per distinct stack and activity: the sample count, the activity (`builtin:NAME` for
/// a builtin, `native:PACKAGE` for native code), and the frames outermost first as
/// `owner::function`, separated by `;`. The owner is a namespace, `R_GlobalEnv` or `other`.
fn stack_lines(stacks: &FxHashMap<(Vec<(u32, usize)>, u32), u64>) -> String {
    let mut owners: FxHashMap<u32, String> = FxHashMap::default();
    let mut functions: FxHashMap<usize, String> = FxHashMap::default();
    let mut lines: Vec<String> = stacks
        .iter()
        .map(|((frames, word), count)| {
            let activity = Activity::from_word(*word);
            let activity = match activity {
                Activity::Builtin => format!(
                    "builtin:{}",
                    builtin_name((word >> 8) as usize).unwrap_or("?")
                ),
                Activity::Native => format!(
                    "native:{}",
                    activity::label_name(word >> 8).unwrap_or_else(|| "unknown".to_owned())
                ),
                _ => activity.describe().to_owned(),
            };
            let frames: Vec<String> = frames
                .iter()
                .map(|&(owner, name)| {
                    let owner = owners.entry(owner).or_insert_with(|| {
                        activity::label_name(owner).unwrap_or_else(|| "unknown".to_owned())
                    });
                    let function = functions.entry(name).or_insert_with(|| {
                        // SAFETY: a nonzero `name` is the address of a symbol, and symbols are
                        // never freed.
                        match unsafe { Sexp::from_raw(name as *mut Header) } {
                            Some(symbol) => unsafe { values::symbol_name(symbol).into_owned() },
                            None => "(anonymous)".to_owned(),
                        }
                    });
                    format!("{owner}::{function}")
                })
                .collect();
            format!("{count}\t{activity}\t{}\n", frames.join(";"))
        })
        .collect();
    lines.sort();
    lines.concat()
}

/// One JSON object per line for each benchmark region, in the order the regions first started.
fn regions_report(samples: &Samples) -> String {
    let regions = lock(&REGIONS);
    let mut out = String::new();
    for (index, name) in regions.names.iter().enumerate() {
        let mut categories = [0u64; Activity::ALL.len()];
        if let Some(words) = samples.regions.get(index) {
            for (&word, &count) in words {
                categories[category(word) as usize] += count;
            }
        }
        let activities: Vec<String> = Activity::ALL
            .iter()
            .map(|&activity| format!("\"{}\":{}", activity.key(), categories[activity as usize]))
            .collect();
        let counters: Vec<String> = Counter::ALL
            .iter()
            .zip(&regions.counters[index])
            .map(|(counter, count)| format!("\"{}\":{count}", counter.key()))
            .collect();
        out.push_str(&format!(
            "{{\"region\":{},\"seconds\":{:.6},\"samples\":{},\"activities\":{{{}}},\"counters\":{{{}}}}}\n",
            json_string(name),
            regions.seconds[index].as_secs_f64(),
            categories.iter().sum::<u64>(),
            activities.join(","),
            counters.join(","),
        ));
    }
    out
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Where a sample's time goes in the split: its activity, except that a builtin which signals
/// conditions or inspects the call stack counts under that category instead.
fn category(word: u32) -> Activity {
    let activity = Activity::from_word(word);
    if activity != Activity::Builtin {
        return activity;
    }
    match builtin_name((word >> 8) as usize) {
        Some(name) if CONDITION_BUILTINS.contains(&name) => Activity::Conditions,
        Some(name) if INTROSPECTION_BUILTINS.contains(&name) => Activity::Introspection,
        _ => Activity::Builtin,
    }
}

const CONDITION_BUILTINS: &[&str] = &[
    "stop",
    "warning",
    ".signalCondition",
    ".addCondHands",
    ".resetCondHands",
    ".addGlobHands",
    ".addRestart",
    ".getRestart",
    ".invokeRestart",
    ".dfltStop",
    ".dfltWarn",
    "seterrmessage",
    "geterrmessage",
    "printDeferredWarnings",
    "conditionMessage",
    "conditionCall",
];

const INTROSPECTION_BUILTINS: &[&str] = &[
    "sys.call",
    "sys.function",
    "sys.frame",
    "sys.nframe",
    "sys.calls",
    "sys.frames",
    "sys.on.exit",
    "sys.parent",
    "sys.parents",
    "parent.frame",
    "match.call",
    "nargs",
];

fn builtin_name(index: usize) -> Option<&'static str> {
    interp()
        .builtins
        .get(index)
        .map(|definition| definition.name)
}

/// The packages whose names the summary prints: R's base and recommended packages and the
/// dependencies ry benchmarks against. Any other package's name is hashed, so a private
/// package's name does not leave the machine.
const PUBLIC_PACKAGES: &[&str] = &[
    "base",
    "compiler",
    "datasets",
    "graphics",
    "grDevices",
    "grid",
    "methods",
    "parallel",
    "splines",
    "stats",
    "stats4",
    "tcltk",
    "tools",
    "utils",
    "MASS",
    "Matrix",
    "lattice",
    "nlme",
    "survival",
    "boot",
    "class",
    "cluster",
    "codetools",
    "foreign",
    "KernSmooth",
    "rpart",
    "spatial",
    "nnet",
    "mgcv",
    "R",
    "abind",
    "assertthat",
    "azidentity",
    "AzureAuth",
    "AzureKeyVault",
    "AzureStor",
    "AzureTableStor",
    "brio",
    "cachem",
    "callr",
    "cli",
    "data.table",
    "DBI",
    "devtools",
    "digest",
    "doParallel",
    "esgtoolkit",
    "FLSSS",
    "foreach",
    "fs",
    "git2r",
    "httr",
    "httr2",
    "jsonlite",
    "lgr",
    "loadfast",
    "lubridate",
    "memoise",
    "NlcOptim",
    "odbc",
    "ompr",
    "ompr.roi",
    "openxlsx",
    "partitions",
    "pdftools",
    "PerformanceAnalytics",
    "plumber",
    "polars",
    "pool",
    "progress",
    "purrr",
    "quadprog",
    "R6",
    "rappdirs",
    "Rblpapi",
    "RcppDE",
    "RcppSimdJson",
    "readr",
    "readxl",
    "renv",
    "Rglpk",
    "rlang",
    "rmgarch",
    "ROI.plugin.highs",
    "rugarch",
    "rvest",
    "snakecase",
    "stringr",
    "stringi",
    "testthat",
    "tryCatchLog",
    "uuid",
    "withr",
    "xml2",
    "xts",
    "YieldCurve",
    "zeallot",
    "zoo",
    "Rcpp",
    "RcppArmadillo",
    "magrittr",
    "glue",
    "vctrs",
    "tibble",
    "dplyr",
    "tidyr",
    "processx",
    "ps",
    "fastmap",
    "later",
    "promises",
    "httpuv",
    "curl",
    "openssl",
    "yaml",
    "xfun",
    "knitr",
    "rmarkdown",
    "htmltools",
    "bit",
    "bit64",
    "vroom",
    "tzdb",
    "timechange",
    "ROI",
    "highs",
    "Rsolnp",
    "ks",
    "pkgload",
    "waldo",
    "diffobj",
    "desc",
    "rprojroot",
    "pkgbuild",
    "lifecycle",
    "ellipsis",
    "pillar",
    "fansi",
    "utf8",
    "crayon",
    "backports",
    "sys",
    "askpass",
    "mime",
    "base64enc",
    "zip",
    "nloptr",
    "numDeriv",
    "truncnorm",
    "SkewHyperbolic",
    "GeneralizedHyperbolic",
    "spd",
    "ff",
    "TTR",
    "quantmod",
    "Rglpk",
    "slam",
    "registry",
    "checkmate",
    "listenv",
    "lpSolveAPI",
];

/// The packages of the R distribution, whose closures the summary ranks by name.
const BASE_PACKAGES: &[&str] = &[
    "base",
    "compiler",
    "graphics",
    "grDevices",
    "grid",
    "methods",
    "parallel",
    "splines",
    "stats",
    "stats4",
    "tcltk",
    "tools",
    "utils",
];

fn owner_display(label: u32, key: &[u8; 16]) -> String {
    match activity::label_name(label) {
        Some(name) if name == GLOBAL_LABEL => "(scripts and the console)".to_owned(),
        Some(name) if name == "other" => "(other top levels)".to_owned(),
        Some(name) => package_display(&name, key),
        None => "unknown".to_owned(),
    }
}

/// A package's name as the summary prints it: a public name as it is, any other name as a keyed
/// hash. With an unkeyed hash, anyone who guesses a private name could confirm the guess by
/// hashing it. With a secret key, a name gets the same label in every summary made with that
/// key, but a guess cannot be checked.
fn package_display(name: &str, key: &[u8; 16]) -> String {
    if PUBLIC_PACKAGES.contains(&name) {
        return name.to_owned();
    }
    let mut hasher = SipHasher24::new_with_key(key);
    hasher.write(name.as_bytes());
    format!("private-{:08x}", hasher.finish() as u32)
}

/// The secret key private package names are hashed with, and where it came from.
struct PrivateKey {
    key: [u8; 16],
    source: KeySource,
}

enum KeySource {
    /// [`KEY_VARIABLE`] gave it.
    Variable,
    /// The key file of the project the session started in.
    Project,
    /// The user's key file for sessions that start outside any project.
    OutsideProjects,
    /// Drawn for this run alone, because no key file could be kept, for the reason given.
    ThisRun(String),
}

/// The files that mark a project's root directory: ry's configuration files and a package's
/// `DESCRIPTION`. The nearest directory with any of them wins, as on the command line.
const PROJECT_MARKERS: &[&str] = &["ry.toml", "roughly.toml", "DESCRIPTION"];

/// The key for a summary of a session that started in `started_in`: [`KEY_VARIABLE`] when it is
/// set, else the key of the project that contains `started_in`, else the user's key for sessions
/// outside projects.
///
/// Key files live in the user's cache directory under `ry/profile-keys`, never inside a project
/// where they could be committed with the code. A project's file is named by a hash of the
/// project's path, so a moved project gets a new key. A key is drawn from the operating system's
/// randomness the first time it is needed. When no file can be kept, the summary uses a key for
/// that run alone and says so, rather than losing the run's profile.
fn private_key(started_in: Option<&Path>) -> Result<PrivateKey, String> {
    if let Some(text) = std::env::var_os(KEY_VARIABLE).filter(|text| !text.is_empty()) {
        let key = text
            .to_str()
            .and_then(parse_key)
            .ok_or_else(|| format!("{KEY_VARIABLE} must be 32 hexadecimal digits"))?;
        return Ok(PrivateKey {
            key,
            source: KeySource::Variable,
        });
    }
    let Some(cache) = rcmd::cache_directory() else {
        return run_key(
            "there is no cache directory (none of XDG_CACHE_HOME, LOCALAPPDATA and HOME is set)"
                .to_owned(),
        );
    };
    let directory = cache.join("ry").join("profile-keys");
    let (path, source) = match started_in.and_then(project_root) {
        Some(root) => {
            let mut hasher = SipHasher13::new();
            hasher.write(root.as_os_str().as_encoded_bytes());
            (
                directory.join(format!("{:016x}", hasher.finish())),
                KeySource::Project,
            )
        }
        None => (
            directory.join("outside-projects"),
            KeySource::OutsideProjects,
        ),
    };
    match read_or_create_key(&path) {
        Ok(key) => Ok(PrivateKey { key, source }),
        // The summary is shared, so the reason leaves out the path, which can name the user.
        Err(failure) => run_key(format!(
            "no key file can be kept in the cache directory ({failure})"
        )),
    }
}

/// The nearest directory at or above `start` that holds one of the [`PROJECT_MARKERS`]. The
/// path is resolved first, so a project reached through a symbolic link keeps one key.
fn project_root(start: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    start
        .ancestors()
        .find(|directory| {
            PROJECT_MARKERS
                .iter()
                .any(|marker| directory.join(marker).is_file())
        })
        .map(Path::to_path_buf)
}

fn run_key(reason: String) -> Result<PrivateKey, String> {
    Ok(PrivateKey {
        key: random_key()?,
        source: KeySource::ThisRun(reason),
    })
}

fn random_key() -> Result<[u8; 16], String> {
    let mut key = [0u8; 16];
    getrandom::fill(&mut key).map_err(|failure| {
        format!("cannot draw a key for the profile summary from the operating system: {failure}")
    })?;
    Ok(key)
}

/// The key in the file at `path`, creating the file with a new random key when there is none.
fn read_or_create_key(path: &Path) -> io::Result<[u8; 16]> {
    match read_key(path) {
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => {}
        read => return read,
    }
    let key = random_key().map_err(io::Error::other)?;
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    write_key(path, &key)
}

/// Writes `key` to a new file at `path` and returns it, or returns the key already there when
/// another session created the file first.
fn write_key(path: &Path, key: &[u8; 16]) -> io::Result<[u8; 16]> {
    // The key goes to a file of this process's own, which is then linked under the final name.
    // The link fails when another session got there first, and both sessions then use that
    // session's key, so no session reads a half-written key or replaces one already in use.
    let staging = path.with_extension(format!("{}.new", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&staging)
        .and_then(|mut file| writeln!(file, "{}", format_key(key)))
        .and_then(|()| std::fs::hard_link(&staging, path));
    let removed = std::fs::remove_file(&staging);
    match written {
        Ok(()) => removed.map(|()| *key),
        Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => read_key(path),
        Err(failure) => Err(failure),
    }
}

fn read_key(path: &Path) -> io::Result<[u8; 16]> {
    let text = std::fs::read_to_string(path)?;
    parse_key(text.trim()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the file does not hold 32 hexadecimal digits",
        )
    })
}

fn parse_key(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; 16];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * index..2 * index + 2)?, 16).ok()?;
    }
    Some(key)
}

fn format_key(key: &[u8; 16]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn report(
    samples: &Samples,
    entries: &FxHashMap<usize, u64>,
    elapsed: Duration,
    ready: Option<Duration>,
    key: &PrivateKey,
) -> Result<String, std::fmt::Error> {
    let total: u64 = samples.words.values().sum();
    let seconds = |count: u64| {
        let after_startup = elapsed.saturating_sub(ready.unwrap_or_default());
        if total == 0 {
            0.0
        } else {
            after_startup.as_secs_f64() * count as f64 / total as f64
        }
    };
    let percent = |count: u64| {
        if total == 0 {
            0.0
        } else {
            100.0 * count as f64 / total as f64
        }
    };
    let mut categories = [0u64; Activity::ALL.len()];
    let mut builtins: FxHashMap<usize, u64> = FxHashMap::default();
    for (&word, &count) in &samples.words {
        categories[category(word) as usize] += count;
        if Activity::from_word(word) == Activity::Builtin {
            *builtins.entry((word >> 8) as usize).or_insert(0) += count;
        }
    }
    let mut out = String::new();
    writeln!(out, "# ry profile summary\n")?;
    writeln!(
        out,
        "ry {}, {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )?;
    writeln!(out, "Wall time: {:.3} s", elapsed.as_secs_f64())?;
    if let Some(ready) = ready {
        writeln!(out, "Start-up: {:.3} s", ready.as_secs_f64())?;
    }
    writeln!(
        out,
        "Samples after start-up: {total} (one per {} ms)",
        INTERVAL_MICROS.load(Ordering::Relaxed) as f64 / 1000.0
    )?;
    let hashed_with = match &key.source {
        KeySource::Variable => format!("the key in {KEY_VARIABLE}"),
        KeySource::Project => "this project's key".to_owned(),
        KeySource::OutsideProjects => "your key for code outside projects".to_owned(),
        KeySource::ThisRun(reason) => format!("a key for this run alone, because {reason}"),
    };
    writeln!(
        out,
        "Private package names are hashed with {hashed_with}.\n"
    )?;

    writeln!(out, "## Time split\n")?;
    writeln!(out, "| activity | share | seconds |\n| --- | ---: | ---: |")?;
    let mut order: Vec<Activity> = Activity::ALL.to_vec();
    order.sort_by_key(|activity| std::cmp::Reverse(categories[*activity as usize]));
    for activity in order {
        let count = categories[activity as usize];
        if count > 0 {
            writeln!(
                out,
                "| {} | {:.1}% | {:.3} |",
                activity.describe(),
                percent(count),
                seconds(count)
            )?;
        }
    }

    // The tier-up threshold, so the last bucket is the bodies that compile.
    const BUCKETS: [(&str, u64); 4] = [
        ("under 10", 10),
        ("10 to 99", 100),
        ("100 to 2,999", 3000),
        ("3,000 or more", u64::MAX),
    ];
    let mut bucketed = [[0u64; Activity::ALL.len()]; BUCKETS.len() + 1];
    let mut bodies_in = [0u64; BUCKETS.len()];
    for &entered in entries.values() {
        if let Some(index) = BUCKETS.iter().position(|&(_, below)| entered < below) {
            bodies_in[index] += 1;
        }
    }
    for (&body, counts) in &samples.bodies {
        let bucket = match entries.get(&body) {
            Some(&entered) if body != 0 => BUCKETS
                .iter()
                .position(|&(_, below)| entered < below)
                .unwrap_or(BUCKETS.len() - 1),
            _ => BUCKETS.len(),
        };
        for (slot, count) in bucketed[bucket].iter_mut().zip(counts) {
            *slot += count;
        }
    }
    writeln!(
        out,
        "\n## Time by how often the innermost closure body was entered\n"
    )?;
    writeln!(
        out,
        "A sample counts toward the closure it is running in, whatever it does there. Entries are \
         totals for the whole run, counted per body.\n"
    )?;
    writeln!(
        out,
        "| entries | bodies | share | tier 0 | tier 1 | AST evaluator | builtins | call setup | \
         native | collector | other |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )?;
    for (index, counts) in bucketed.iter().enumerate() {
        let sum: u64 = counts.iter().sum();
        let part = |activities: &[Activity]| -> u64 {
            activities
                .iter()
                .map(|&activity| counts[activity as usize])
                .sum()
        };
        let named = [
            Activity::Interpreter,
            Activity::Compiled,
            Activity::Evaluator,
            Activity::Builtin,
            Activity::CallSetup,
            Activity::Native,
            Activity::Collector,
        ];
        let label = BUCKETS
            .get(index)
            .map_or("outside closures", |&(label, _)| label);
        let bodies = bodies_in
            .get(index)
            .map_or_else(String::new, u64::to_string);
        write!(out, "| {label} | {bodies} | {:.1}% |", percent(sum))?;
        for activity in named {
            write!(out, " {:.1}% |", percent(part(&[activity])))?;
        }
        writeln!(out, " {:.1}% |", percent(sum - part(&named)))?;
    }

    writeln!(out, "\n## Builtins by time (top 30)\n")?;
    writeln!(
        out,
        "| builtin | share | seconds | calls |\n| --- | ---: | ---: | ---: |"
    )?;
    let mut by_time: Vec<(usize, u64)> = builtins.into_iter().collect();
    by_time.sort_by_key(|&(index, count)| (std::cmp::Reverse(count), index));
    for &(index, count) in by_time.iter().take(30) {
        writeln!(
            out,
            "| `{}` | {:.1}% | {:.3} | {} |",
            builtin_name(index).unwrap_or("?"),
            percent(count),
            seconds(count),
            activity::builtin_count(index)
        )?;
    }

    writeln!(out, "\n## Builtins by calls (top 30)\n")?;
    writeln!(out, "| builtin | calls |\n| --- | ---: |")?;
    let mut by_calls: Vec<(usize, u64)> = (0..interp().builtins.len())
        .map(|index| (index, activity::builtin_count(index)))
        .filter(|&(_, calls)| calls > 0)
        .collect();
    by_calls.sort_by_key(|&(index, calls)| (std::cmp::Reverse(calls), index));
    for &(index, calls) in by_calls.iter().take(30) {
        writeln!(
            out,
            "| `{}` | {calls} |",
            builtin_name(index).unwrap_or("?")
        )?;
    }

    writeln!(out, "\n## Packages by inclusive time\n")?;
    writeln!(
        out,
        "Self time is split into the package's R code (with the builtins it calls), its native \
         code, and the interpreter's overhead on its behalf (call setup, lookups, method \
         dispatch, collection, compilation).\n"
    )?;
    writeln!(
        out,
        "| package | inclusive | self | R code | native | overhead |\n\
         | --- | ---: | ---: | ---: | ---: | ---: |"
    )?;
    let mut packages: Vec<(String, PackageTime)> = samples
        .packages
        .iter()
        .map(|(&label, &time)| (owner_display(label, &key.key), time))
        .collect();
    packages.sort_by(|left, right| {
        right
            .1
            .inclusive
            .cmp(&left.1.inclusive)
            .then_with(|| left.0.cmp(&right.0))
    });
    for (name, time) in &packages {
        writeln!(
            out,
            "| {name} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% |",
            percent(time.inclusive),
            percent(time.code + time.native + time.overhead),
            percent(time.code),
            percent(time.native),
            percent(time.overhead),
        )?;
    }

    let base_closures: Vec<(String, Attribution)> = samples
        .closures
        .iter()
        .filter_map(|(&(owner, name), &time)| {
            let package = activity::label_name(owner)?;
            if !BASE_PACKAGES.contains(&package.as_str()) {
                return None;
            }
            // SAFETY: a nonzero `name` is the address of a symbol, and symbols are never freed.
            let function = match unsafe { Sexp::from_raw(name as *mut Header) } {
                Some(symbol) => unsafe { values::symbol_name(symbol).into_owned() },
                None => "(anonymous)".to_owned(),
            };
            Some((format!("{package}::{function}"), time))
        })
        .collect();
    for (title, inclusive) in [("self", false), ("inclusive", true)] {
        writeln!(out, "\n## Base R closures by {title} time (top 30)\n")?;
        writeln!(out, "| closure | self | inclusive |\n| --- | ---: | ---: |")?;
        let mut ranked = base_closures.clone();
        ranked.sort_by(|left, right| {
            let key = |time: &Attribution| if inclusive { time.inclusive } else { time.own };
            key(&right.1)
                .cmp(&key(&left.1))
                .then_with(|| left.0.cmp(&right.0))
        });
        for (name, time) in ranked.iter().take(30) {
            writeln!(
                out,
                "| `{name}` | {:.1}% | {:.1}% |",
                percent(time.own),
                percent(time.inclusive)
            )?;
        }
    }

    let calls = activity::counter(Counter::PackageCalls);
    if calls > 0 {
        let direct = activity::counter(Counter::PackageCallsDirect);
        let fallbacks = by_reason_name(activity::package_fallbacks());
        let observing = |reason: &str| crate::eval::OBSERVING_REASONS.contains(&reason);
        let observed: u64 = fallbacks
            .iter()
            .filter(|(reason, _)| observing(reason))
            .map(|(_, count)| count)
            .sum();
        let share = |part: u64, whole: u64| {
            if whole == 0 {
                0.0
            } else {
                100.0 * part as f64 / whole as f64
            }
        };
        writeln!(out, "\n## Package calls\n")?;
        writeln!(
            out,
            "{direct} of {calls} calls to package closures skipped the generic call protocol \
             ({:.1}%), and {:.1}% of the {} whose environment no code observed.\n",
            share(direct, calls),
            share(direct, calls - observed),
            calls - observed,
        )?;
        writeln!(
            out,
            "| reason for the generic protocol | calls | share | observed |\n| --- | ---: | ---: | :---: |"
        )?;
        for (reason, count) in fallbacks {
            writeln!(
                out,
                "| {reason} | {count} | {:.1}% | {} |",
                share(count, calls),
                if observing(reason) { "yes" } else { "" },
            )?;
        }
    }

    let reifications = by_reason_name(activity::reifications());
    if !reifications.is_empty() {
        let total: u64 = reifications.iter().map(|(_, count)| count).sum();
        writeln!(out, "\n## Slot frames turned into environments\n")?;
        writeln!(out, "| reason | frames | share |\n| --- | ---: | ---: |")?;
        for (reason, count) in reifications {
            writeln!(
                out,
                "| {reason} | {count} | {:.1}% |",
                100.0 * count as f64 / total as f64
            )?;
        }
    }

    let assignments = activity::general_assignments();
    if !assignments.is_empty() {
        let total: u64 = assignments.iter().map(|(_, count)| count).sum();
        writeln!(out, "\n## Complex assignments that built their calls\n")?;
        writeln!(
            out,
            "Each of these {total} assignments built at least one getter or replacement call of \
             its target, with a promise per value passed. A row names the replacement functions \
             from the outside in, then what the variable held.\n"
        )?;
        writeln!(
            out,
            "| target | assignments | share |\n| --- | ---: | ---: |"
        )?;
        for (shape, count) in assignments.into_iter().take(40) {
            writeln!(
                out,
                "| {shape} | {count} | {:.1}% |",
                100.0 * count as f64 / total as f64
            )?;
        }
    }

    let refusals = activity::jit_refusals();
    if !refusals.is_empty() {
        writeln!(out, "\n## JIT compile attempts refused, by reason\n")?;
        writeln!(
            out,
            "Each row counts the times tier 1 tried to compile a body and left it to tier 0 for \
             that reason, and one body can be refused more than once. `RY_JIT_LOG=1` prints the \
             same reasons, each with the body it refused.\n"
        )?;
        writeln!(out, "| reason | attempts |\n| --- | ---: |")?;
        for (reason, count) in refusals {
            writeln!(out, "| {reason} | {count} |")?;
        }
    }

    writeln!(out, "\n## Events\n")?;
    writeln!(out, "| event | count |\n| --- | ---: |")?;
    for counter in Counter::ALL {
        writeln!(
            out,
            "| {} | {} |",
            counter.describe(),
            activity::counter(counter)
        )?;
    }
    let stats = crate::heap().stats();
    writeln!(out, "| minor collections | {} |", stats.minor_collections)?;
    writeln!(out, "| major collections | {} |", stats.major_collections)?;
    writeln!(out, "| objects freed | {} |", stats.objects_freed)?;
    let mut kinds: Vec<(u64, &str)> = (0..32u32)
        .filter_map(|code| {
            let count = *stats.freed_by_kind.get(code as usize)?;
            let name = if code == 26 {
                "binding cell"
            } else {
                SexpType::from_code(code)?.name()
            };
            (count > 0).then_some((count, name))
        })
        .collect();
    kinds.sort_by(|a, b| b.cmp(a));
    for (count, name) in kinds {
        writeln!(out, "| objects freed: {name} | {count} |")?;
    }
    let kind_name = |code: usize| {
        if code == 26 {
            "binding cell"
        } else {
            SexpType::from_code(code as u32).map_or("?", |kind| kind.name())
        }
    };
    let allocated = activity::allocation_counts();
    for (code, count) in allocated.iter().enumerate() {
        if *count > 0 {
            writeln!(out, "| objects allocated: {} | {count} |", kind_name(code))?;
        }
    }
    writeln!(out, "| most objects live | {} |", stats.max_live_objects)?;
    writeln!(
        out,
        "| most vector memory live (MB) | {:.1} |",
        stats.max_vector_cells as f64 * 8.0 / 1e6
    )?;
    let mut sites = activity::allocation_sites();
    if !sites.is_empty() {
        sites.sort_by_key(|site| std::cmp::Reverse(site.1));
        writeln!(out, "\n## Allocation sites (sampled)\n")?;
        writeln!(out, "| kind | samples | call path, innermost first |")?;
        writeln!(out, "| --- | ---: | --- |")?;
        for (site, count) in sites {
            let (kind, path) = site.split_once('\t').unwrap_or(("", &site));
            let kind = kind.parse::<usize>().map_or("?", kind_name);
            writeln!(out, "| {kind} | {count} | `{path}` |")?;
        }
    }
    Ok(out)
}

/// The counts of `reasons` under the names the summary prints, largest first. A site and a
/// builtin of one name (`parent.frame`) are one row.
fn by_reason_name(reasons: Vec<(u32, u64)>) -> Vec<(&'static str, u64)> {
    let mut named: Vec<(&'static str, u64)> = Vec::new();
    for (reason, count) in reasons {
        let name = crate::eval::reason_name(reason);
        match named.iter_mut().find(|(held, _)| *held == name) {
            Some((_, total)) => *total += count,
            None => named.push((name, count)),
        }
    }
    named.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    named
}

#[cfg(test)]
mod tests {
    use super::{format_key, package_display, parse_key, read_or_create_key};

    #[test]
    fn private_names_depend_on_the_key() {
        let first = [7u8; 16];
        let second = [8u8; 16];
        let label = package_display("privatepkg", &first);
        assert!(label.starts_with("private-") && label.len() == "private-".len() + 8);
        assert_eq!(label, package_display("privatepkg", &first));
        assert_ne!(label, package_display("privatepkg", &second));
        assert_ne!(label, package_display("otherpkg", &first));
        assert_eq!(package_display("data.table", &first), "data.table");
    }

    #[test]
    fn keys_round_trip_through_text() {
        let key: [u8; 16] = std::array::from_fn(|index| (index * 17) as u8);
        assert_eq!(parse_key(&format_key(&key)), Some(key));
        assert_eq!(parse_key("00"), None);
        assert_eq!(parse_key(&"g".repeat(32)), None);
        assert_eq!(parse_key(&"é".repeat(16)), None);
    }

    #[test]
    fn a_key_file_is_created_once_and_read_after() {
        let directory = std::env::temp_dir().join(format!("ry-profile-key-{}", std::process::id()));
        let path = directory.join("profile-keys").join("project");
        let created = read_or_create_key(&path).expect("the key file is created");
        assert_eq!(read_or_create_key(&path).ok(), Some(created));
        let entries = std::fs::read_dir(directory.join("profile-keys"))
            .expect("the key directory exists")
            .count();
        assert_eq!(entries, 1, "the staging file is removed");
        std::fs::remove_dir_all(&directory).expect("the test directory is removed");
    }
}
