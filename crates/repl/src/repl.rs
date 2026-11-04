//! An R console that `dlopen`s the R shared library and resolves every C-API
//! symbol at runtime, so nothing here needs R at build time.

pub mod console;
mod gnu_r_home;
pub mod libr;

// Re-exported so hosts implement `SessionCompleter` against the same reedline version.
pub use reedline;
use std::{
    error::Error,
    ffi::{CString, c_char, c_int, c_void},
    fmt, fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct ReplError(pub String);

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for ReplError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keybindings {
    #[default]
    Emacs,
    Vi,
}

pub trait SessionCompleter: Send {
    /// Called with every accepted input line, so completions see session definitions.
    fn accept(&mut self, line: &str);
    /// `position` is a byte offset into `line`.
    fn complete(&mut self, line: &str, position: usize) -> Vec<reedline::Suggestion>;
}

/// Lets a host act between commands without owning the console.
pub trait ConsoleHooks: Send {
    /// Called when the user submits a command at R's top-level prompt, before R reads it. R
    /// code returned here runs first, as its own top-level command.
    fn before_command(&mut self) -> Option<String>;
}

/// Sees R input before the engine parses it: each top-level command typed at the console, and a
/// script fed whole.
pub trait InputFilter: Send {
    fn filter(&mut self, input: &str) -> Filtered;
    /// Ctrl-C at the console drops the input the filter holds.
    fn interrupt(&mut self) {}
}

/// What an [`InputFilter`] does with a piece of input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filtered {
    /// Evaluate this text in place of the input, and of any input held before it.
    Run(String),
    /// The command is unfinished: keep it and read its next line at a continuation prompt.
    Hold,
    /// Drop the input, and any held before it, unevaluated.
    Refuse,
}

#[derive(Default)]
pub struct RunOptions {
    pub keybindings: Keybindings,
    /// `.Call` routines registered before anything runs.
    pub routines: Vec<libr::CallRoutine>,
    /// Evaluated in the global environment after startup and before the first prompt. They run
    /// outside R's top-level loop, so each must catch its own errors.
    pub startup: Vec<String>,
    /// R input fed before `file`; in batch mode a top-level error in it quits.
    pub input: String,
    /// Fed to R before the first prompt.
    pub file: Option<PathBuf>,
    /// End the session when the script is consumed; a top-level error quits with
    /// a failing status.
    pub batch: bool,
    /// What the script reads with `commandArgs(trailingOnly = TRUE)`.
    pub arguments: Vec<String>,
    pub completer: Option<Box<dyn SessionCompleter>>,
    pub hooks: Option<Box<dyn ConsoleHooks>>,
    pub filter: Option<Box<dyn InputFilter>>,
    /// The package `startup` loads. ry's runtime starts analyzing it while base R loads; GNU R
    /// ignores it.
    pub package: Option<PathBuf>,
}

/// Returns only when R ends. R assumes it owns the thread it initializes on, so
/// call this from `main` without spawning.
pub fn run(options: RunOptions) -> Result<(), ReplError> {
    let api = libr::load()?;
    console::run(api, options)
}

/// Runs R code on ry's own runtime instead of GNU R: loads ry's `libR` (from `RY_LIBR`, or
/// beside the executable), evaluates `input`, then the script `file`, whose bytes are `source`
/// when given (the program `ry run` checked) and the file's contents otherwise. Both are optional, and
/// `load_all()` is attached before either runs. With a `verdict`, the loader calls it before the
/// package's first effect and exits with its status unless that is 0. Returns the exit status.
pub fn run_on_ry_runtime(
    file: Option<&std::path::Path>,
    source: Option<&[u8]>,
    input: Option<&str>,
    arguments: &[String],
    flags: RuntimeFlags,
    package: Option<&Path>,
    verdict: Option<PackageVerdict>,
) -> Result<i32, ReplError> {
    use std::ffi::{CString, c_char, c_int};
    let library = ry_runtime()?;
    if let Some(verdict) = verdict {
        type SetVerdict = unsafe extern "C" fn(PackageVerdict);
        let set_verdict = runtime_entry::<SetVerdict>(library, "ry_loader_set_verdict")?;
        // SAFETY: the entry point has this signature in every runtime of this build.
        unsafe { set_verdict(verdict) };
    }
    if let Some(package) = package {
        prepare_package(library, package, flags);
    }
    type Run = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        *const *const c_char,
        c_int,
    ) -> c_int;
    let run = runtime_entry::<Run>(library, "ry_run")?;
    if let Some(source) = source {
        type SetSource = unsafe extern "C" fn(*const c_char);
        let set_source = runtime_entry::<SetSource>(library, "ry_set_script_source")?;
        let source =
            CString::new(source).map_err(|_| ReplError("the script contains a NUL byte".into()))?;
        // SAFETY: the entry point has this signature in every runtime of this build.
        unsafe { set_source(source.as_ptr()) };
    }
    let path = file
        .map(|file| CString::new(file.to_string_lossy().as_bytes()))
        .transpose()
        .map_err(|_| ReplError("the script path contains a NUL byte".into()))?;
    let input = input
        .map(CString::new)
        .transpose()
        .map_err(|_| ReplError("the R input contains a NUL byte".into()))?;
    let arguments: Vec<CString> = arguments
        .iter()
        .map(|argument| CString::new(argument.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|_| ReplError("an argument contains a NUL byte".into()))?;
    let pointers: Vec<*const c_char> = arguments.iter().map(|argument| argument.as_ptr()).collect();
    let nullable =
        |value: &Option<CString>| value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    Ok(unsafe {
        run(
            nullable(&path),
            nullable(&input),
            pointers.len() as c_int,
            pointers.as_ptr(),
            flags.bits(),
        )
    })
}

/// Asked by the package loader of ry's runtime about a package, given its root as a
/// NUL-terminated path. The strict query returns 1 when the package's configuration turns strict
/// mode on. The check runs before a load of a strict package runs anything: 0 lets the load run,
/// 1 means strict mode refused the package, and any other value that the check failed.
pub type PackageCheck = extern "C" fn(*const std::ffi::c_char) -> std::ffi::c_int;

/// Makes ry's runtime ask `check` before every load of a strict package that changes
/// something, so code strict mode refuses never runs. Call it once, before running the runtime.
pub fn set_package_check_on_ry_runtime(
    strict_config: PackageCheck,
    check: PackageCheck,
) -> Result<(), ReplError> {
    let library = ry_runtime()?;
    type SetCheck = unsafe extern "C" fn(PackageCheck, PackageCheck);
    let set_check = runtime_entry::<SetCheck>(library, "ry_loader_set_check")?;
    // SAFETY: the entry point has this signature in every runtime of this build.
    unsafe { set_check(strict_config, check) };
    Ok(())
}

/// Called by the package loader before the parts of a load that may have effects: 0 lets them
/// run, and any other value is the status the process exits with. It may block.
pub type PackageVerdict = extern "C" fn() -> std::ffi::c_int;

/// What ry's runtime does with inline type annotations and the standard library; mirrors the
/// `RUN_FLAG_*` bits of `libr::eval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeFlags {
    /// Inline annotations become checks at function boundaries.
    pub annotation_checks: bool,
    /// S4, R6 and Reference Class definitions in user code are allowed.
    pub object_systems: bool,
    /// The project is in strict mode, so the package loader binds pure constants lazily.
    pub strict: bool,
    /// The standard library's names resolve bare; off, only as `std::List.map`.
    pub prelude: bool,
}

impl RuntimeFlags {
    fn bits(self) -> std::ffi::c_int {
        std::ffi::c_int::from(self.annotation_checks)
            | (std::ffi::c_int::from(self.object_systems) << 1)
            | (std::ffi::c_int::from(self.strict) << 2)
            | (std::ffi::c_int::from(!self.prelude) << 5)
    }
}

/// Runs the interactive console on ry's own runtime instead of GNU R. The runtime attaches the
/// package loader and evaluates `startup` before the first prompt, and before each command it
/// reloads a loaded package whose sources changed, so it brings its own `hooks` and refuses
/// a caller's. `routines` does not apply: ry's runtime has no `.Call` routines to register.
pub fn run_console_on_ry_runtime(
    mut options: RunOptions,
    flags: RuntimeFlags,
) -> Result<i32, ReplError> {
    use std::ffi::{CString, c_char, c_int, c_uchar};
    if options.hooks.is_some() {
        return Err(ReplError(
            "ry's runtime watches for reloads itself and takes no console hooks".into(),
        ));
    }
    let library = ry_runtime()?;
    if let Some(package) = &options.package {
        prepare_package(library, package, flags);
    }
    type ReadConsole = extern "C" fn(*const c_char, *mut c_uchar, c_int) -> c_int;
    type Repl = unsafe extern "C" fn(ReadConsole, *const c_char, c_int) -> c_int;
    let repl = runtime_entry::<Repl>(library, "ry_repl")?;
    let changed = runtime_entry::<unsafe extern "C" fn() -> c_int>(library, "ry_loader_changed")?;
    let startup = match options.startup.is_empty() {
        true => None,
        false => Some(
            CString::new(options.startup.join("\n"))
                .map_err(|_| ReplError("the startup code contains a NUL byte".into()))?,
        ),
    };
    options.hooks = Some(Box::new(RuntimeReloadWatcher { changed }));
    let banner = std::io::stdin()
        .is_terminal()
        .then(|| "ry console on ry's runtime. Type q() or Ctrl-D to quit.".to_owned());
    console::install_for_ry_runtime(options, banner)?;
    Ok(unsafe {
        repl(
            console::ry_read_console,
            startup
                .as_ref()
                .map_or(std::ptr::null(), |startup| startup.as_ptr()),
            flags.bits(),
        )
    })
}

/// Reads the debug client's next message for [`debug_on_ry_runtime`]; `libr::eval::DebugReceive`
/// states the contract.
pub type DebugReceive = unsafe extern "C" fn(c_int, *mut usize, *mut c_void) -> *const c_char;

/// Sends one message to the debug client; `libr::eval::DebugSend` states the contract.
pub type DebugSend = unsafe extern "C" fn(*const c_char, usize, *mut c_void);

/// Runs a Debug Adapter Protocol server on ry's runtime, which talks to the client through
/// `receive` and `send` with `data` passed back to both, and returns the debugged program's
/// exit status.
///
/// # Safety
///
/// The callbacks must keep their contracts, and `data` must stay valid, until this returns.
pub unsafe fn debug_on_ry_runtime(
    receive: DebugReceive,
    send: DebugSend,
    data: *mut c_void,
    flags: RuntimeFlags,
) -> Result<i32, ReplError> {
    let library = ry_runtime()?;
    type Adapter = unsafe extern "C" fn(DebugReceive, DebugSend, *mut c_void, c_int) -> c_int;
    let adapter = runtime_entry::<Adapter>(library, "ry_debug_adapter")?;
    Ok(unsafe { adapter(receive, send, data, flags.bits()) })
}

/// Has ry's runtime start analyzing the package at `package` in the background. Preparing
/// only saves time, so a runtime without the entry point, or a path with a NUL byte, loads the
/// package without it.
fn prepare_package(library: &'static RuntimeLibrary, package: &Path, flags: RuntimeFlags) {
    type Prepare = unsafe extern "C" fn(*const c_char, c_int);
    let (Ok(prepare), Ok(path)) = (
        runtime_entry::<Prepare>(library, "ry_loader_prepare"),
        CString::new(package.to_string_lossy().as_bytes()),
    ) else {
        return;
    };
    unsafe { prepare(path.as_ptr(), flags.bits()) };
}

/// Asks ry's runtime whether a loaded package changed on disk, and reloads it if so.
struct RuntimeReloadWatcher {
    changed: unsafe extern "C" fn() -> std::ffi::c_int,
}

impl ConsoleHooks for RuntimeReloadWatcher {
    fn before_command(&mut self) -> Option<String> {
        // SAFETY: the runtime is loaded for the life of the process, and the console calls its
        // hooks on the runtime's thread between commands.
        let changed = unsafe { (self.changed)() } != 0;
        changed.then(|| ".ry_autoreload()".to_owned())
    }
}

#[cfg(unix)]
type RuntimeLibrary = libloading::os::unix::Library;
#[cfg(not(unix))]
type RuntimeLibrary = libloading::Library;

fn ry_runtime() -> Result<&'static RuntimeLibrary, ReplError> {
    let (library_path, extracted) = ry_runtime_library()?;
    // On Linux, compiled packages record `libR.so` as a needed library, and loading ry's globally
    // lets the dynamic linker satisfy them from it. On macOS, `libr`'s loader rebinds GNU R's
    // package binaries to this library's install name, and packages ry builds look R's API up in
    // the flat namespace that RTLD_GLOBAL populates. On Windows a package DLL imports from
    // "R.dll" by module name, which the loader satisfies from this already loaded R.dll.
    #[cfg(unix)]
    let library = unsafe {
        RuntimeLibrary::open(
            Some(&library_path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
        )
    };
    #[cfg(not(unix))]
    let library = unsafe { RuntimeLibrary::new(&library_path) };
    let library = library.map_err(|error| {
        let hint = if extracted {
            format!(
                "; ry extracted it into {}, which may be on a `noexec` mount that cannot run \
                 code. Set XDG_CACHE_HOME to another directory, or RY_LIBR to ry's runtime \
                 elsewhere",
                library_path.parent().unwrap_or(&library_path).display()
            )
        } else {
            String::new()
        };
        ReplError(format!(
            "failed to load ry's runtime at {}: {error}{hint}",
            library_path.display()
        ))
    })?;
    let library: &'static RuntimeLibrary = Box::leak(Box::new(library));
    let built_from = runtime_id(library);
    if built_from.as_deref() != Some(RUNTIME_ID) {
        return Err(ReplError(stale_runtime_message(
            &library_path,
            built_from.as_deref(),
        )));
    }
    export_r_home()?;
    Ok(library)
}

/// The identity of the sources `ry` was built from, which `libR.so` must have been built from
/// too (`crates/libr/build_identity.rs` defines it).
const RUNTIME_ID: &str = env!("RY_RUNTIME_ID");

/// `None` for a library that exports no identity, which predates the check.
fn runtime_id(library: &'static RuntimeLibrary) -> Option<String> {
    type RuntimeId = unsafe extern "C" fn() -> *const std::ffi::c_char;
    let id = runtime_entry::<RuntimeId>(library, "ry_runtime_id").ok()?;
    let pointer = unsafe { id() };
    (!pointer.is_null()).then(|| {
        unsafe { std::ffi::CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    })
}

fn stale_runtime_message(library_path: &std::path::Path, built_from: Option<&str>) -> String {
    format!(
        "ry's runtime ({}) is out of date: it has build {}, but this ry is build {RUNTIME_ID}; rebuild it with `just build` or `cargo build -p libr` (same profile as ry)",
        library_path.display(),
        built_from.unwrap_or("unknown"),
    )
}

fn runtime_entry<T: Copy>(library: &'static RuntimeLibrary, name: &str) -> Result<T, ReplError> {
    let symbol = format!("{name}\0");
    unsafe { library.get::<T>(symbol.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|error| ReplError(format!("the loaded libR is not ry's runtime: {error}")))
}

/// ry's R home, whose `bin/R` runs ry's runtime (`rcmd::runtime_home`), over the base R tree
/// [`base_r`] picks.
pub fn ry_r_home() -> Result<PathBuf, ReplError> {
    let (libr, _) = ry_runtime_library()?;
    rcmd::runtime_home(&libr, &base_r()?).map_err(|error| ReplError(error.to_string()))
}

/// The base R tree ry's runtime loads: the one a set `R_HOME` names (a GNU R installation,
/// which emulates that R, or a home ry assembled, which a process started by its `bin/R`
/// inherits), else ry's own ([`bundled_base`]).
pub fn base_r() -> Result<PathBuf, ReplError> {
    match std::env::var_os("R_HOME").filter(|home| !home.is_empty()) {
        Some(home) => {
            let home = PathBuf::from(home);
            if !rcmd::base::holds_base_r(&home) {
                return Err(ReplError(format!(
                    "R_HOME is set to {}, which holds no base R; unset it to use ry's own base R",
                    home.display()
                )));
            }
            Ok(home)
        }
        None => bundled_base(),
    }
}

/// ry's own base R tree: one installed outside the executable (`rcmd::base::installed`), else
/// the copy a release embeds, extracted into the user's cache ([`extract_base`]).
pub fn bundled_base() -> Result<PathBuf, ReplError> {
    if let Some(base) = rcmd::base::installed() {
        if !rcmd::base::holds_base_r(&base) {
            return Err(ReplError(format!(
                "ry's base R at {} is incomplete (no library/base/R/base); rebuild it with \
                 `just base-r`, or point RY_BASE_R at a complete one",
                base.display()
            )));
        }
        return Ok(base);
    }
    match &EMBEDDED_BASE {
        Some(base) => extract_base(base),
        None => Err(ReplError(
            "this ry has no base R: it was built without RY_EMBED_BASE, and no base-r directory \
             is beside it or named by RY_BASE_R. Build one with `just base-r`, or set R_HOME to a \
             GNU R installation"
                .to_owned(),
        )),
    }
}

/// Points the runtime's session at ry's R home when `R_HOME` names none, so that `R.home()`,
/// `R.home("bin")/R` (which callr and `R CMD` run) and the base packages all come from it.
fn export_r_home() -> Result<(), ReplError> {
    if std::env::var_os("R_HOME").is_some_and(|home| !home.is_empty()) {
        return Ok(());
    }
    let home = ry_r_home()?;
    // SAFETY: ry's host threads (the LSP server's, the CLI's) never read the environment while
    // a runtime starts, and the runtime starts none of its own until its session is up; the
    // session's `read_renviron` relies on the same.
    unsafe { std::env::set_var("R_HOME", home) };
    Ok(())
}

/// ry's runtime library, and whether it was extracted from this executable. `RY_LIBR` wins,
/// then a library beside the executable, in `deps/` or in `../lib/ry/` (a development build or
/// a distribution's package), and last the copy a release embeds ([`extract_runtime`]).
fn ry_runtime_library() -> Result<(PathBuf, bool), ReplError> {
    if let Some(path) = std::env::var_os("RY_LIBR") {
        return Ok((PathBuf::from(path), false));
    }
    let executable = std::env::current_exe()
        .map_err(|error| ReplError(format!("cannot locate the ry executable: {error}")))?;
    let directory = executable.parent().unwrap_or(std::path::Path::new("."));
    // `libR.so` on Linux, `libR.dylib` on macOS and `R.dll` on Windows: the names compiled
    // packages link against.
    let library = format!(
        "{}R{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    for candidate in [
        directory.join(&library),
        directory.join("deps").join(&library),
        directory.join("../lib/ry").join(&library),
    ] {
        if candidate.exists() {
            return Ok((candidate, false));
        }
    }
    if let Some(runtime) = &EMBEDDED_RUNTIME {
        return extract_runtime(runtime).map(|path| (path, true));
    }
    Err(ReplError(format!(
        "ry's runtime ({library}) was not found beside {}; build it with `just build` or \
         `cargo build -p libr`, or set RY_LIBR to its path",
        executable.display()
    )))
}

/// A runtime built into the executable (`build.rs`, `RY_EMBED_RUNTIME`).
pub struct EmbeddedRuntime {
    /// A hash of the files, naming the directory they are extracted into.
    key: &'static str,
    /// The runtime library first, then the files that must sit beside it.
    files: &'static [EmbeddedFile],
}

struct EmbeddedFile {
    name: &'static str,
    size: u64,
    compressed: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/embedded_runtime.rs"));

/// ry's base R tree built into the executable (`build.rs`, `RY_EMBED_BASE`), as one compressed
/// archive whose format `build.rs` describes.
pub struct EmbeddedBase {
    /// A hash of the archive, naming the directory it is extracted into.
    key: &'static str,
    /// The archive's size once decompressed.
    size: u64,
    compressed: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/embedded_base.rs"));

/// Extracts the embedded base R tree into `ry/base/<key>/` of the user's cache, unless an earlier
/// run did, and returns its path. The tree is written into a staging directory beside its place
/// and renamed into it, so a process never sees a partial tree and concurrent first runs both
/// succeed.
fn extract_base(base: &EmbeddedBase) -> Result<PathBuf, ReplError> {
    let cache = rcmd::cache_directory().ok_or_else(|| {
        ReplError(
            "ry extracts its base R into a cache directory, and none of XDG_CACHE_HOME, \
             LOCALAPPDATA or HOME is set; set one, or RY_BASE_R to ry's base R"
                .to_owned(),
        )
    })?;
    let parent = cache.join("ry").join("base");
    let directory = parent.join(base.key);
    if rcmd::base::is_complete(&directory) {
        return Ok(directory);
    }
    let failed = |path: &Path, error: io::Error| {
        ReplError(format!(
            "cannot extract ry's base R to {}: {error}; set XDG_CACHE_HOME to a writable \
             directory, or RY_BASE_R to ry's base R",
            path.display()
        ))
    };
    fs::create_dir_all(&parent).map_err(|error| failed(&parent, error))?;
    let staging = parent.join(format!(".{}-{}", base.key, rcmd::staging_suffix()));
    let written = unpack_base(base, &staging);
    if let Err(error) = written {
        // A partial tree, say from a full disk, would otherwise stay in the cache for good.
        let mut message = failed(&staging, error).0;
        if staging.exists() {
            if let Err(cleanup) = fs::remove_dir_all(&staging) {
                message.push_str(&format!(
                    ", and removing the partial copy failed: {cleanup}"
                ));
            }
        }
        return Err(ReplError(message));
    }
    if let Err(error) = fs::rename(&staging, &directory) {
        // Another process renamed its copy into place first.
        let removed = fs::remove_dir_all(&staging);
        if !rcmd::base::is_complete(&directory) {
            return Err(failed(&directory, error));
        }
        removed.map_err(|error| failed(&staging, error))?;
    }
    Ok(directory)
}

fn corrupt_archive() -> io::Error {
    io::Error::other("the embedded base R archive is corrupt")
}

fn take<'a>(archive: &'a [u8], position: &mut usize, count: usize) -> io::Result<&'a [u8]> {
    let end = position.checked_add(count).ok_or_else(corrupt_archive)?;
    let bytes = archive.get(*position..end).ok_or_else(corrupt_archive)?;
    *position = end;
    Ok(bytes)
}

fn take_array<const N: usize>(archive: &[u8], position: &mut usize) -> io::Result<[u8; N]> {
    take(archive, position, N)?
        .try_into()
        .map_err(|_| corrupt_archive())
}

fn unpack_base(base: &EmbeddedBase, root: &Path) -> io::Result<()> {
    let archive = zstd::decode_all(base.compressed)?;
    if archive.len() as u64 != base.size {
        return Err(io::Error::other("the embedded base R archive is truncated"));
    }
    fs::create_dir_all(root)?;
    let mut position = 0;
    while position < archive.len() {
        let kind = archive[position];
        position += 1;
        let length = u32::from_le_bytes(take_array(&archive, &mut position)?) as usize;
        let relative = std::str::from_utf8(take(&archive, &mut position, length)?)
            .map_err(|_| corrupt_archive())?;
        if relative
            .split('/')
            .any(|part| part.is_empty() || part == "..")
        {
            return Err(corrupt_archive());
        }
        let path = root.join(relative);
        match kind {
            0 => fs::create_dir_all(&path)?,
            1 | 2 => {
                let size = u64::from_le_bytes(take_array(&archive, &mut position)?);
                let size = usize::try_from(size).map_err(|_| corrupt_archive())?;
                fs::write(&path, take(&archive, &mut position, size)?)?;
                #[cfg(unix)]
                if kind == 2 {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
                }
            }
            _ => return Err(corrupt_archive()),
        }
    }
    Ok(())
}

/// Extracts the embedded runtime into `ry/runtime/<key>/` of the user's cache, unless an
/// earlier run did, and returns the library's path. A file is written under a name of its own
/// and renamed into place, so a process never sees a partial file and concurrent first runs
/// both succeed; the library is renamed last, so a complete library means a complete directory.
fn extract_runtime(runtime: &EmbeddedRuntime) -> Result<PathBuf, ReplError> {
    let library = runtime
        .files
        .first()
        .ok_or_else(|| ReplError("the runtime embedded in ry is empty".to_owned()))?;
    let cache = rcmd::cache_directory().ok_or_else(|| {
        ReplError(
            "ry extracts its runtime into a cache directory, and none of XDG_CACHE_HOME, \
             LOCALAPPDATA or HOME is set; set one, or RY_LIBR to ry's runtime"
                .to_owned(),
        )
    })?;
    let directory = cache.join("ry").join("runtime").join(runtime.key);
    let library_path = directory.join(library.name);
    let complete = |path: &Path, file: &EmbeddedFile| {
        fs::metadata(path).is_ok_and(|metadata| metadata.len() == file.size)
    };
    if complete(&library_path, library) {
        return Ok(library_path);
    }
    let failed = |path: &Path, error: io::Error| {
        ReplError(format!(
            "cannot extract ry's runtime to {}: {error}; set XDG_CACHE_HOME to a writable \
             directory, or RY_LIBR to ry's runtime",
            path.display()
        ))
    };
    fs::create_dir_all(&directory).map_err(|error| failed(&directory, error))?;
    for file in runtime.files.iter().rev() {
        let path = directory.join(file.name);
        if file.name != library.name && complete(&path, file) {
            continue;
        }
        let staging = directory.join(format!(".{}.{}", file.name, rcmd::staging_suffix()));
        let bytes = zstd::decode_all(file.compressed).map_err(|error| failed(&path, error))?;
        let written = fs::write(&staging, &bytes);
        #[cfg(unix)]
        let written = written.and_then(|()| {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))
        });
        if let Err(error) = written {
            // A partial file, say from a full disk, would otherwise stay in the cache for good.
            if staging.exists() {
                fs::remove_file(&staging).map_err(|error| failed(&staging, error))?;
            }
            return Err(failed(&staging, error));
        }
        if let Err(error) = fs::rename(&staging, &path) {
            // Windows refuses to replace a library another process has loaded, which means a
            // concurrent run extracted it first.
            let removed = fs::remove_file(&staging);
            if !complete(&path, file) {
                return Err(failed(&path, error));
            }
            removed.map_err(|error| failed(&staging, error))?;
        }
    }
    Ok(library_path)
}
