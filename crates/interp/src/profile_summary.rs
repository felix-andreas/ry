//! `ry run --profile-summary FILE`: a profile of where a run spends its time that holds no
//! source code, identifiers or data, so a user can share one from a private code base.
//!
//! The R thread publishes what it is doing through [`heap::activity`]; a sampling thread reads
//! that word every millisecond and counts the values it sees. The summary reports the shares of
//! those samples together with the event counters the hooks keep. The only names it prints are
//! base R's builtins and the packages that own native code; a package outside the public list
//! below is printed as a hash of its name.

use {
    crate::{
        interp,
        unwind::{R, error},
        values,
    },
    heap::{
        object::{Header, Sexp, SexpType},
        activity::{self, Activity, Counter},
    },
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::{
        fmt::Write as _,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread::JoinHandle,
        time::{Duration, Instant},
    },
};

/// The environment variable that names the summary file; `--profile-summary` sets it.
pub const VARIABLE: &str = "RY_PROFILE_SUMMARY";

const INTERVAL: Duration = Duration::from_millis(1);

struct Sampler {
    path: String,
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
    fn record(&mut self, word: u32, owners: &[u32], names: &[usize], global: u32) {
        *self.words.entry(word).or_insert(0) += 1;
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
            | Activity::JitCompile => (innermost_owner, Slot::Overhead),
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

/// Starts collecting a summary when [`VARIABLE`] names a file.
pub fn start_from_environment() {
    let Some(path) = std::env::var_os(VARIABLE).filter(|path| !path.is_empty()) else {
        return;
    };
    let path = path.to_string_lossy().into_owned();
    // Subprocesses (callr, parallel's socket clusters) would overwrite the file with a summary
    // of themselves, so only this process profiles.
    // SAFETY: the session starts before R code can start threads that read the environment.
    unsafe { std::env::remove_var(VARIABLE) };
    activity::set_enabled(true);
    let global = activity::label(GLOBAL_LABEL);
    let thread = std::thread::spawn(move || {
        let mut samples = Samples::default();
        let mut owners = Vec::with_capacity(activity::FRAME_SLOTS);
        let mut names = Vec::with_capacity(activity::FRAME_SLOTS);
        while !STOP.load(Ordering::Relaxed) {
            std::thread::sleep(INTERVAL);
            if READY.load(Ordering::Relaxed) {
                let word = activity::current();
                activity::read_frames(&mut owners, &mut names);
                samples.record(word, &owners, &names, global);
            } else {
                samples.startup += 1;
            }
        }
        samples
    });
    *lock(&SAMPLER) = Some(Sampler {
        path,
        process: std::process::id(),
        started: Instant::now(),
        thread,
    });
}

/// The label of code whose top level is the global environment: scripts and the console.
const GLOBAL_LABEL: &str = "R_GlobalEnv";

/// Publishes a call of the closure `function` through `call` for the sampler, until the scope
/// drops.
pub unsafe fn enter_closure(call: Sexp, function: Sexp) -> activity::FrameScope {
    if !activity::enabled() {
        return activity::no_frame();
    }
    let owner = crate::dispatch::top_env(function.tag());
    let owner = {
        let mut labels = lock(&OWNER_LABELS);
        match labels.get(&(owner.as_ptr() as usize)) {
            Some(&label) => label,
            None => {
                let name = if owner == values::global_env() {
                    GLOBAL_LABEL.to_owned()
                } else {
                    crate::envir::namespace_name(owner).unwrap_or_else(|| "other".to_owned())
                };
                let label = activity::label(&name);
                labels.insert(owner.as_ptr() as usize, label);
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
    activity::push_frame(owner, name)
}

/// Owner labels by environment address. Namespaces and the global environment live for the whole
/// session, so an address names one owner; another top level (an environment `sys.source` used)
/// can be freed and its address reused, which at worst merges two such owners under "other".
static OWNER_LABELS: Mutex<FxHashMap<usize, u32>> = Mutex::new(FxHashMap::with_hasher(
    rustc_hash::FxBuildHasher,
));

/// Marks the end of start-up: samples after this count toward the time split.
pub fn mark_ready() {
    if activity::enabled() {
        *lock(&READY_AT) = Some(Instant::now());
        READY.store(true, Ordering::Relaxed);
    }
}

/// Stops sampling and writes the summary, if one was being collected.
pub fn finish() -> R<()> {
    let Some(sampler) = lock(&SAMPLER).take() else {
        return Ok(());
    };
    if sampler.process != std::process::id() {
        activity::set_enabled(false);
        return Ok(());
    }
    STOP.store(true, Ordering::Relaxed);
    activity::set_enabled(false);
    let elapsed = sampler.started.elapsed();
    let samples = sampler
        .thread
        .join()
        .map_err(|_| error("the profile summary's sampling thread panicked"))?;
    let ready = lock(&READY_AT).map(|ready| ready.duration_since(sampler.started));
    let text =
        report(&samples, elapsed, ready).map_err(|_| error("cannot format the profile summary"))?;
    std::fs::write(&sampler.path, text).map_err(|failure| {
        error(format!(
            "cannot write the profile summary to '{}': {failure}",
            sampler.path
        ))
    })
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
        INTERVAL.as_millis()
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
        writeln!(
            out,
            "| closure | self | inclusive |\n| --- | ---: | ---: |"
        )?;
        let mut ranked = base_closures.clone();
        ranked.sort_by(|left, right| {
            let key = |time: &Attribution| if inclusive { time.inclusive } else { time.own };
            key(&right.1).cmp(&key(&left.1)).then_with(|| left.0.cmp(&right.0))
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
    writeln!(out, "| most objects live | {} |", stats.max_live_objects)?;
    writeln!(
        out,
        "| most vector memory live (MB) | {:.1} |",
        stats.max_vector_cells as f64 * 8.0 / 1e6
    )?;
    Ok(out)
}
