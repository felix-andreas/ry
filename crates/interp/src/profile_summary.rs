//! `ry run --profile-summary FILE`: a profile of where a run spends its time that holds no
//! source code, identifiers or data, so a user can share one from a private code base.
//!
//! The R thread publishes what it is doing through [`heap::activity`]; a sampling thread reads
//! that word every millisecond and counts the values it sees. The summary reports the shares of
//! those samples together with the event counters the hooks keep. The only names it prints are
//! base R's builtins and the packages that own native code; a package outside the public list
//! below is printed as a hash of its name.
//!
//! The same sampler measures benchmark regions: `.ry_bench_region(name)` marks where a region
//! starts (and the one before it ends), and `RY_PROFILE_REGIONS` names a file that gets one JSON
//! line per region with its wall time, its samples by activity and its event counts.

use {
    crate::{
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
    smallvec::SmallVec,
    std::{
        ffi::CString,
        fmt::Write as _,
        path::{Path, PathBuf},
        sync::{
            Mutex,
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

/// The environment variable that sets the sampling interval in microseconds (default 1000).
pub const INTERVAL_VARIABLE: &str = "RY_PROFILE_INTERVAL_US";

/// The sampling interval in microseconds, fixed when sampling starts.
static INTERVAL_MICROS: AtomicU64 = AtomicU64::new(1000);

struct Sampler {
    /// The summary file, when a summary is collected.
    path: Option<PathBuf>,
    /// The regions file, when regions are measured.
    regions: Option<PathBuf>,
    /// The process that started sampling. A forked child inherits this record but not the
    /// sampling thread, so it writes nothing.
    process: u32,
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
    fn record(
        &mut self,
        word: u32,
        owners: &[u32],
        names: &[usize],
        bodies: &[usize],
        global: u32,
    ) {
        *self.words.entry(word).or_insert(0) += 1;
        self.bodies
            .entry(bodies.last().copied().unwrap_or(0))
            .or_insert([0; Activity::ALL.len()])[category(word) as usize] += 1;
        let innermost_owner = owners.last().copied().unwrap_or(global);
        let innermost_name = names.last().copied().unwrap_or(0);
        if !owners.is_empty() {
            self.closures
                .entry((innermost_owner, innermost_name))
                .or_default()
                .own += 1;
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
            Slot::Code => time.code += 1,
            Slot::Native => time.native += 1,
            Slot::Overhead => time.overhead += 1,
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
                self.closures.entry((owner, name)).or_default().inclusive += 1;
            }
            if !seen_packages.contains(&owner) {
                seen_packages.push(owner);
            }
        }
        if owners.is_empty() && !seen_packages.contains(&global) {
            seen_packages.push(global);
        }
        for package in seen_packages {
            self.packages.entry(package).or_default().inclusive += 1;
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
/// Whether closure calls publish their frames, which only the summary reads.
static PUBLISHING: AtomicBool = AtomicBool::new(false);
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
    let publishing = path.is_some();
    PUBLISHING.store(publishing, Ordering::Relaxed);
    activity::set_enabled(true);
    let global = activity::label(GLOBAL_LABEL);
    let thread = std::thread::spawn(move || {
        let mut samples = Samples::default();
        let mut owners = Vec::with_capacity(activity::FRAME_SLOTS);
        let mut names = Vec::with_capacity(activity::FRAME_SLOTS);
        let mut bodies = Vec::with_capacity(activity::FRAME_SLOTS);
        while !STOP.load(Ordering::Relaxed) {
            std::thread::sleep(interval);
            if !READY.load(Ordering::Relaxed) {
                samples.startup += 1;
                continue;
            }
            let word = activity::current();
            if publishing {
                activity::read_frames(&mut owners, &mut names, &mut bodies);
                samples.record(word, &owners, &names, &bodies, global);
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
        process: std::process::id(),
        started: Instant::now(),
        thread,
    });
}

/// Whether the sampler runs, which needs every closure call to pass the profiling hooks.
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

/// What a closure call holds while it runs with the profiling hooks on: its published frame and
/// its boundary-call record.
pub struct ClosureScope {
    _frame: activity::FrameScope,
    capture: crate::capture::Scope,
}

impl ClosureScope {
    /// Hands the call's result to the boundary-call recorder before the scope drops.
    pub unsafe fn returned(&mut self, result: &R<Sexp>) {
        unsafe { self.capture.returned(result) };
    }
}

/// Runs the profiling hooks for a call of the closure `function` through `call` whose caller's
/// frame is `sys_parent`, with the dispatch variables of a method in `method_vars` or bound in
/// its frame environment `env` (`NULL` for a slot frame): publishes the frame for the sampler
/// and records boundary calls, until the scope drops.
pub unsafe fn enter_closure(
    call: Sexp,
    function: Sexp,
    sys_parent: Sexp,
    method_vars: &[(Sexp, Sexp)],
    env: Sexp,
) -> R<ClosureScope> {
    if !activity::enabled() {
        return Ok(ClosureScope {
            _frame: activity::no_frame(),
            capture: crate::capture::Scope::none(),
        });
    }
    let capture = if activity::hooks() & activity::CAPTURE != 0 {
        unsafe { crate::capture::enter(call, function, sys_parent, method_vars, env)? }
    } else {
        crate::capture::Scope::none()
    };
    let frame = if PUBLISHING.load(Ordering::Relaxed) {
        unsafe { publish_frame(call, function) }
    } else {
        activity::no_frame()
    };
    Ok(ClosureScope {
        _frame: frame,
        capture,
    })
}

unsafe fn publish_frame(call: Sexp, function: Sexp) -> activity::FrameScope {
    let owner = crate::dispatch::top_env(function.tag());
    let body = crate::bytecode::closure_body(function).as_ptr() as usize;
    let owner = {
        let mut labels = lock(&OWNER_LABELS);
        *labels.entries.entry(body).or_insert(0) += 1;
        match labels.owners.get(&(owner.as_ptr() as usize)) {
            Some(&label) => label,
            None => {
                let name = if owner == values::global_env() {
                    GLOBAL_LABEL.to_owned()
                } else {
                    crate::envir::namespace_name(owner).unwrap_or_else(|| "other".to_owned())
                };
                let label = activity::label(&name);
                labels.owners.insert(owner.as_ptr() as usize, label);
                label
            }
        }
    };
    let mut head = call.car();
    if head.kind() == SexpType::Language
        && head.car().kind() == SexpType::Symbol
        && matches!(&*values::symbol_name(head.car()), "::" | ":::")
    {
        head = head.cdr().cdr().car();
    }
    let name = if head.kind() == SexpType::Symbol {
        head.as_ptr() as usize
    } else {
        0
    };
    activity::push_frame(owner, name, body)
}

/// What closure calls record while a summary is collected.
struct Calls {
    /// Owner labels by environment address. Namespaces and the global environment live for the
    /// whole session, so an address names one owner; another top level (an environment
    /// `sys.source` used) can be freed and its address reused, which at worst merges two such
    /// owners under "other".
    owners: FxHashMap<usize, u32>,
    /// Entries by closure body address. A body freed and reallocated elsewhere merges with the
    /// new one, which only matters for closures built and dropped at run time.
    entries: FxHashMap<usize, u64>,
}

static OWNER_LABELS: Mutex<Calls> = Mutex::new(Calls {
    owners: FxHashMap::with_hasher(rustc_hash::FxBuildHasher),
    entries: FxHashMap::with_hasher(rustc_hash::FxBuildHasher),
});

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
    activity::set_enabled(false);
    PUBLISHING.store(false, Ordering::Relaxed);
    let elapsed = sampler.started.elapsed();
    let samples = sampler
        .thread
        .join()
        .map_err(|_| error("the profile summary's sampling thread panicked"))?;
    if let Some(path) = &sampler.regions {
        write_file(path, &regions_report(&samples), "the benchmark regions")?;
    }
    let Some(path) = &sampler.path else {
        return Ok(());
    };
    let ready = lock(&READY_AT).map(|ready| ready.duration_since(sampler.started));
    let text =
        report(&samples, elapsed, ready).map_err(|_| error("cannot format the profile summary"))?;
    write_file(path, &text, "the profile summary")
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

fn owner_display(label: u32) -> String {
    match activity::label_name(label) {
        Some(name) if name == GLOBAL_LABEL => "(scripts and the console)".to_owned(),
        Some(name) if name == "other" => "(other top levels)".to_owned(),
        Some(name) => package_display(&name),
        None => "unknown".to_owned(),
    }
}

fn package_display(name: &str) -> String {
    if PUBLIC_PACKAGES.contains(&name) {
        return name.to_owned();
    }
    // FNV-1a: stable across runs and machines, so two summaries of one code base agree.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
    });
    format!("private-{:08x}", hash as u32)
}

fn report(
    samples: &Samples,
    elapsed: Duration,
    ready: Option<Duration>,
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
        "Samples after start-up: {total} (one per {} ms)\n",
        INTERVAL_MICROS.load(Ordering::Relaxed) as f64 / 1000.0
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
    let entries = std::mem::take(&mut lock(&OWNER_LABELS).entries);
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
        .map(|(&label, &time)| (owner_display(label), time))
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
        let observed = activity::counter(Counter::PackageCallsObserved);
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
        for (reason, count) in activity::package_fallbacks() {
            let observing = crate::eval::OBSERVING_REASONS.contains(&reason.as_str());
            writeln!(
                out,
                "| {reason} | {count} | {:.1}% | {} |",
                share(count, calls),
                if observing { "yes" } else { "" },
            )?;
        }
    }

    let reifications = activity::reifications();
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
    writeln!(out, "| most objects live | {} |", stats.max_live_objects)?;
    writeln!(
        out,
        "| most vector memory live (MB) | {:.1} |",
        stats.max_vector_cells as f64 * 8.0 / 1e6
    )?;
    Ok(out)
}
