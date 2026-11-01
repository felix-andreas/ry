//! The operating system as R sees it: environment variables, files and directories, time and
//! dates, the process, and the session constants (`.Machine`, `.Platform`, `R.version`).

use {
    crate::{
        builtins::{BuiltinDef, Call, builtin, internal, special},
        interp,
        platform::{Iconv, convert_all},
        unwind::{R, error},
        values::{
            self, alloc, get_attr, length, nil, scalar_bool, scalar_integer, scalar_real,
            scalar_string, string_vector, symbols,
        },
    },
    heap::{
        GcStress,
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Sexp, SexpType, is_na_real},
        strings::Encoding,
    },
    std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("internalsID", 0, |_| unsafe { scalar_string(INTERNALS_ID) }),
    internal("eSoftVersion", 0, do_esoft_version),
    internal("pcre_config", 0, |_| unsafe {
        let result = values::logical_vector(&[1, 1, 1, 1])?;
        let names = string_vector(&["UTF-8", "Unicode properties", "JIT", "stack"])?;
        values::set_attr(result, symbols().names, names)?;
        Ok(result)
    }),
    internal("icuSetCollate", -1, |_| Ok(nil())).invisible(),
    internal("icuGetCollate", 1, |_| unsafe {
        scalar_string("ICU not in use")
    }),
    internal("setNumMathThreads", 1, |_| unsafe { scalar_integer(1) }),
    internal("setMaxNumMathThreads", 1, |_| unsafe { scalar_integer(1) }),
    internal("isatty", 1, |call| unsafe {
        let descriptor = values::as_integer_scalar(call.arg(0));
        scalar_bool(descriptor >= 0 && libc::isatty(descriptor) == 1)
    }),
    internal("readRenviron", 1, do_read_renviron).invisible(),
    internal("debug", 3, |_| Ok(nil())).invisible(),
    internal("undebug", 1, |_| Ok(nil())).invisible(),
    internal("debugonce", 3, |_| Ok(nil())).invisible(),
    internal("isdebugged", 1, |_| unsafe { scalar_bool(false) }),
    internal("debugOnOff", 1, |_| unsafe { scalar_bool(false) }),
    internal("traceOnOff", 1, |_| unsafe { scalar_bool(false) }),
    internal("browserText", 1, |call| {
        Err(call.error("no browser context to query"))
    }),
    internal("browserCondition", 1, |call| {
        Err(call.error("no browser context to query"))
    }),
    internal("browserSetDebug", 1, |_| Ok(nil())).invisible(),
    internal("memory.profile", 0, |_| unsafe {
        values::integer_vector(&[])
    }),
    internal("file.symlink", 2, |call| unsafe {
        link_paths(call, |from, to| {
            crate::platform::symlink(Path::new(from), Path::new(to)).is_ok()
        })
    }),
    internal("file.link", 2, |call| unsafe {
        link_paths(call, |from, to| std::fs::hard_link(from, to).is_ok())
    }),
    internal("setFileTime", 2, do_sys_set_file_time).invisible(),
    internal("file.show", 5, |call| unsafe {
        for index in 0..length(call.arg(0)) {
            if let Some(path) = values::string_at(call.arg(0), index) {
                let text = std::fs::read_to_string(&*path).map_err(|failure| {
                    call.error(format!("cannot open file '{path}': {failure}"))
                })?;
                crate::write_console(text.as_bytes(), false);
            }
        }
        Ok(nil())
    })
    .invisible(),
    internal("file.choose", 1, |call| {
        Err(call.error("file.choose() needs an interactive session"))
    }),
    internal("curlVersion", 0, |_| unsafe { scalar_string("") }),
    internal("curlGetHeaders", 5, |call| {
        Err(call.error("libcurl is not available in this session"))
    }),
    internal("curlDownload", 6, |call| {
        Err(call.error("libcurl is not available in this session"))
    }),
    internal("machine", 0, |_| unsafe { scalar_string("Unix") }),
    internal("R.home", 0, |_| unsafe { scalar_string(&r_home()) }),
    internal("Sys.getenv", 2, do_sys_getenv),
    internal("Sys.setenv", 2, do_sys_setenv).invisible(),
    internal("Sys.unsetenv", 1, do_sys_unsetenv).invisible(),
    internal("commandArgs", 0, |_| unsafe {
        string_vector(&interp().command_args)
    }),
    builtin("proc.time", 0, do_proc_time),
    internal("Sys.time", 0, do_sys_time),
    internal("date", 0, |_| unsafe {
        let now = now_seconds();
        let (offset, _, zone) = resolve_zone("").offset_at(now);
        scalar_string(&format_broken_down(
            &broken_down(now + offset as f64),
            "%a %b %d %H:%M:%S %Y",
            &zone,
            offset,
        ))
    }),
    internal("file.exists", 1, |call| unsafe {
        map_paths(call, |path| Path::new(path).exists())
    }),
    internal("dir.exists", 1, |call| unsafe {
        map_paths(call, |path| Path::new(path).is_dir())
    }),
    internal("file.info", 2, do_file_info),
    internal("list.files", -1, do_list_files),
    internal("list.dirs", 3, do_list_dirs),
    internal("normalizePath", 3, do_normalize_path),
    internal("path.expand", 1, |call| unsafe {
        map_strings(call, crate::connections::expand_path)
    }),
    internal("basename", 1, |call| unsafe {
        map_strings(call, |path| {
            let trimmed = path.trim_end_matches('/');
            trimmed.rsplit('/').next().unwrap_or("").to_owned()
        })
    }),
    internal("dirname", 1, |call| unsafe {
        map_strings(call, |path| {
            let trimmed = path.trim_end_matches('/');
            match trimmed.rfind('/') {
                Some(0) => "/".to_owned(),
                Some(position) => trimmed[..position].to_owned(),
                None => ".".to_owned(),
            }
        })
    }),
    internal("file.path", 2, do_file_path),
    internal("tempfile", 3, do_tempfile),
    internal("tempdir", 1, |_| unsafe { scalar_string(&temp_dir()) }),
    internal("unlink", 4, do_unlink).invisible(),
    internal("file.remove", 1, |call| unsafe {
        map_paths(call, |path| std::fs::remove_file(path).is_ok())
    }),
    internal("file.rename", 2, do_file_rename),
    internal("file.create", 2, |call| unsafe {
        map_paths(call, |path| std::fs::write(path, b"").is_ok())
    }),
    internal("file.append", 2, do_file_append),
    internal("file.copy", 6, do_file_copy),
    internal("dir.create", 4, do_dir_create).invisible(),
    internal("file.access", 2, do_file_access),
    internal("getwd", 0, |_| unsafe {
        scalar_string(
            &std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        )
    }),
    internal("setwd", 1, do_setwd).invisible(),
    internal("Sys.glob", 2, do_sys_glob),
    internal("Sys.readlink", 1, |call| unsafe {
        map_strings(call, |path| {
            std::fs::read_link(path)
                .map(|target| target.display().to_string())
                .unwrap_or_default()
        })
    }),
    internal("Sys.which", 1, do_sys_which),
    internal("Sys.chmod", 3, do_sys_chmod).invisible(),
    internal("Sys.umask", 1, do_sys_umask),
    internal("Sys.getpid", 0, |_| unsafe {
        scalar_integer(std::process::id() as i32)
    }),
    internal("Sys.info", 0, do_sys_info),
    internal("Sys.setlocale", 2, do_sys_setlocale),
    internal("Sys.getlocale", 1, do_sys_getlocale),
    internal("l10n_info", 0, do_l10n_info),
    internal("Sys.localeconv", 0, |_| unsafe { values::named_list(&[]) }),
    internal("Sys.sleep", 1, |call| unsafe {
        {
            let seconds = values::as_real_scalar(call.arg(0));
            if !(seconds >= 0.0) {
                return Err(error("invalid 'time' value"));
            }
            if seconds.is_finite() && seconds > 0.0 {
                let total = Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX);
                // Slept in slices, so Ctrl-C stops the wait within one slice.
                let slice = Duration::from_millis(20);
                let start = Instant::now();
                while let Some(left) = total
                    .checked_sub(start.elapsed())
                    .filter(|left| !left.is_zero())
                {
                    crate::unwind::poll_interrupt()?;
                    std::thread::sleep(left.min(slice));
                }
            }
            Ok(nil())
        }
    })
    .invisible(),
    internal("Version", 0, do_version),
    internal("capabilities", 0, do_capabilities),
    internal("capabilitiesX11", 0, |_| unsafe { scalar_bool(false) }),
    internal("gc", 3, do_gc),
    internal("gcinfo", 1, |_| unsafe { scalar_bool(false) }).invisible(),
    internal("gctorture", 1, do_gctorture).invisible(),
    internal("gctorture2", 3, do_gctorture2).invisible(),
    builtin("gc.time", -1, |_| unsafe { values::real_vector(&[0.0; 5]) }),
    internal("mem.maxVSize", 1, |_| unsafe { scalar_real(f64::INFINITY) }),
    internal("mem.maxNSize", 1, |_| unsafe { scalar_real(f64::INFINITY) }),
    internal("quit", 3, do_quit),
    internal("system", -1, do_system),
    internal("Cstack_info", 0, |_| unsafe {
        values::integer_vector(&[7969177, 0, 1, NA_INTEGER])
    }),
    internal("setTimeLimit", 3, |_| Ok(nil())).invisible(),
    internal("setSessionTimeLimit", 2, |_| Ok(nil())).invisible(),
    internal("reg.finalizer", 3, do_reg_finalizer),
    internal("traceback", 1, |call| unsafe {
        crate::conditions::traceback_calls(values::as_integer_scalar(call.arg(0)).max(0) as usize)
    }),
    internal(".traceback", 1, |_| Ok(nil())),
    internal("Sys.setFileTime", 2, |_| unsafe { scalar_bool(true) }).invisible(),
    internal("Encoding", 1, do_encoding),
    internal("setEncoding", 2, do_set_encoding),
    builtin("enc2native", 1, |call| Ok(call.arg(0))),
    builtin("enc2utf8", 1, |call| Ok(call.arg(0))),
    internal("iconv", 6, do_iconv),
    internal("validUTF8", 1, do_valid_utf8),
    internal("validEnc", 1, |call| unsafe {
        map_strings_bool(call, |_| true)
    }),
    internal("as.POSIXlt", 2, do_as_posixlt),
    internal("as.POSIXct", 2, do_as_posixct),
    internal("format.POSIXlt", -1, do_format_posixlt),
    internal("strptime", 3, do_strptime),
    internal("Date2POSIXlt", 2, do_date_to_posixlt),
    internal("POSIXlt2Date", 1, do_posixlt_to_date),
    internal("balancePOSIXlt", 3, |call| unsafe {
        balance_posixlt(
            call.arg(0),
            values::is_true(call.arg(1)),
            values::is_true(call.arg(2)),
        )
    }),
    builtin("unCfillPOSIXlt", 1, |call| unsafe {
        let balanced = get_attr(call.arg(0), values::install("balanced")?);
        // The result is unclassed, which lets `lapply` iterate over the fields.
        if balanced != nil() && values::as_logical_scalar(balanced) == 1 {
            require_posixlt(call.arg(0))?;
            let result = values::shallow_duplicate(call.arg(0))?;
            values::remove_attr(result, symbols().class);
            Ok(result)
        } else {
            balance_posixlt(call.arg(0), true, false)
        }
    }),
    // ry has no interactive debugger, so `browser()` continues as it does in a
    // non-interactive GNU R session.
    special("browser", -1, |_| Ok(nil())).invisible(),
    builtin("tracemem", 1, |call| unsafe {
        scalar_string(&format!("<{:p}>", call.arg(0).as_ptr()))
    }),
    builtin("untracemem", 1, |_| Ok(nil())).invisible(),
    builtin("retracemem", -1, |_| Ok(nil())).invisible(),
    builtin(".primTrace", 1, |call| Ok(call.arg(0))).invisible(),
    builtin(".primUntrace", 1, |call| Ok(call.arg(0))).invisible(),
    internal("format.Date", 2, |call| {
        Err(call.error("format.Date is not supported"))
    }),
    internal("tzone_name", 0, |_| unsafe {
        scalar_string(&session_zone_name())
    }),
    internal("bindtextdomain", 2, |_| Ok(nil())),
    internal("gettext", 3, |call| Ok(call.arg(1))),
    internal("ngettext", 4, do_ngettext),
    internal("enc2native", 1, |call| Ok(call.arg(0))),
    internal("crc64", 1, |call| unsafe {
        scalar_string(&crc64_hex(
            values::first_string(call.arg(0))
                .unwrap_or_default()
                .as_bytes(),
        ))
    }),
    internal("mkjunction", 2, |_| unsafe { scalar_bool(false) }),
    internal("pkgload", -1, |_| Ok(nil())),
];

/// GNU R's identifier for its internals interface, which `loadNamespace` compares against the one a
/// package with compiled code recorded when it was installed. Matching it lets packages installed by
/// GNU R 4.3 load unchanged.
const INTERNALS_ID: &str = "2fdf6c18-697a-4ba7-b8ef-11c0d92f1327";

/// The R installation whose base and recommended packages the session loads. When none was
/// found this is GNU R's usual home, so `R.home()` still answers; [`missing_r_home`] says why.
pub fn r_home() -> String {
    located_r_home()
        .clone()
        .unwrap_or_else(|_| FALLBACK_R_HOME.to_owned())
}

/// The message a host prints, before starting a session, when no GNU R installation was found:
/// what ry looked for, where, and how to fix it.
pub fn missing_r_home() -> Option<String> {
    located_r_home().as_ref().err().cloned()
}

/// GNU R's usual home on the platform, which `R.home()` reports when no installation was found.
#[cfg(target_os = "macos")]
const FALLBACK_R_HOME: &str = "/Library/Frameworks/R.framework/Resources";
#[cfg(not(target_os = "macos"))]
const FALLBACK_R_HOME: &str = "/usr/lib/R";

fn located_r_home() -> &'static Result<String, String> {
    static HOME: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    HOME.get_or_init(find_r_home)
}

/// An R home is usable when it holds base R's R code, which the session sources first. A set
/// `R_HOME` is authoritative, as it is for GNU R, so an unusable one is not searched past.
fn find_r_home() -> Result<String, String> {
    let usable = |home: &str| Path::new(home).join("library/base/R/base").exists();
    let fix = "install GNU R, or set R_HOME to the directory `R RHOME` prints; \
               see https://ry-lang.org/installation/";
    // An exported but empty R_HOME, as some env files leave it, names no directory.
    if let Some(home) = std::env::var("R_HOME").ok().filter(|home| !home.is_empty()) {
        if usable(&home) {
            return Ok(home);
        }
        return Err(format!(
            "ry: ry's runtime loads base R from a GNU R installation, but R_HOME is {home}, \
             which holds none (no library/base/R/base)\n  fix: {fix}\n"
        ));
    }
    let mut searched = vec!["R_HOME (not set)".to_owned()];
    for candidate in crate::gnu_r_home::candidate_r_homes() {
        let candidate = candidate.to_string_lossy().into_owned();
        if usable(&candidate) {
            return Ok(candidate);
        }
        searched.push(candidate);
    }
    match std::process::Command::new("R").arg("RHOME").output() {
        Ok(output) if output.status.success() => {
            let home = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if usable(&home) {
                return Ok(home);
            }
            searched.push(format!("`R RHOME` ({home}, which holds no base R)"));
        }
        Ok(_) => searched.push("`R RHOME` (it failed)".to_owned()),
        Err(_) => searched.push("`R RHOME` (no `R` on PATH)".to_owned()),
    }
    Err(format!(
        "ry: ry's runtime loads base R from a GNU R installation, and none was found\n  \
         searched: {}\n  fix: {fix}\n",
        searched.join(", ")
    ))
}

fn temp_dir() -> String {
    static mut DIR: Option<String> = None;
    // SAFETY: single-threaded access.
    #[allow(static_mut_refs)]
    unsafe {
        DIR.get_or_insert_with(|| {
            let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
            let path = format!("{base}/RtmpRy{}", std::process::id());
            drop(std::fs::create_dir_all(&path));
            path
        })
        .clone()
    }
}

/// `Sys.which` as base R's Windows code calls it: each name's program on `PATH`, with `PATHEXT`
/// suffixes tried when the name has none, as a forward-slash path named by the name, or "".
/// Unix base R finds programs with `command -v` in R code instead.
unsafe fn do_sys_which(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let result = map_strings(call, |name| {
        find_program(name)
            .map(|path| path.display().to_string().replace('\\', "/"))
            .unwrap_or_default()
    })?;
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

fn find_program(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let candidate = Path::new(name);
    let extensions: Vec<String> = if candidate.extension().is_some() || !cfg!(windows) {
        vec![String::new()]
    } else {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| extension.to_ascii_lowercase())
            .chain([String::new()])
            .collect()
    };
    let directories: Vec<PathBuf> = if candidate.components().count() > 1 {
        vec![PathBuf::new()]
    } else {
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default()
    };
    directories.iter().find_map(|directory| {
        extensions.iter().find_map(|extension| {
            let path = directory.join(format!("{name}{extension}"));
            path.is_file().then_some(path)
        })
    })
}

unsafe fn map_strings(call: &Call<'_>, function: impl Fn(&str) -> String) -> R<Sexp> {
    let paths = call.arg(0);
    if paths.kind() != SexpType::String && paths != nil() {
        return Err(call.error("a character vector argument expected"));
    }
    let count = length(paths);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let charsxp = match values::string_at(paths, index) {
            Some(path) => values::mkchar(&function(&path))?,
            None => values::na_string(),
        };
        values::set_elt(result, index, charsxp);
    }
    Ok(result)
}

unsafe fn map_strings_bool(call: &Call<'_>, function: impl Fn(&str) -> bool) -> R<Sexp> {
    let strings = call.arg(0);
    let count = length(strings);
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        values::ints_mut(result)[index] =
            values::string_at(strings, index).is_some_and(|text| function(&text)) as i32;
    }
    Ok(result)
}

unsafe fn map_paths(call: &Call<'_>, function: impl Fn(&str) -> bool) -> R<Sexp> {
    let paths = call.arg(0);
    let count = length(paths);
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        values::ints_mut(result)[index] = match values::string_at(paths, index) {
            Some(path) => function(&crate::connections::expand_path(&path)) as i32,
            None => 0,
        };
    }
    Ok(result)
}

/// `file.append(file1, file2)`: appends each `file2` to the recycled `file1`, reporting per pair
/// whether it succeeded. A missing or unreadable `file2` is a failure, not an error.
unsafe fn do_file_append(call: &Call<'_>) -> R<Sexp> {
    let (targets, sources) = (call.arg(0), call.arg(1));
    if targets.kind() != SexpType::String || sources.kind() != SexpType::String {
        return Err(call.error("invalid 'file' argument"));
    }
    if length(targets) == 0 {
        return Err(call.error("nothing to append to"));
    }
    let count = if length(sources) == 0 {
        0
    } else {
        length(targets).max(length(sources))
    };
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        let target = values::string_at(targets, index % length(targets));
        let source = values::string_at(sources, index % length(sources));
        let appended = match (target, source) {
            (Some(target), Some(source)) => append_file(
                &crate::connections::expand_path(&target),
                &crate::connections::expand_path(&source),
            )
            .is_ok(),
            _ => false,
        };
        values::ints_mut(result)[index] = appended as i32;
    }
    Ok(result)
}

/// `file.copy(from, to, overwrite, recursive, copy.mode, copy.date)`. The R closure handles copies
/// to file names itself and calls this only when `to` is one existing directory, so each `from`
/// lands at `to/basename(from)`. A directory is copied only when `recursive` is TRUE.
unsafe fn do_file_copy(call: &Call<'_>) -> R<Sexp> {
    let (sources, target) = (call.arg(0), call.arg(1));
    if sources.kind() != SexpType::String {
        return Err(call.error("invalid 'from' argument"));
    }
    let Some(target) = values::first_string(target) else {
        return Err(call.error("invalid 'to' argument"));
    };
    let options = CopyOptions {
        overwrite: values::as_logical_scalar(call.arg(2)) == 1,
        recursive: values::as_logical_scalar(call.arg(3)) == 1,
        mode: values::as_logical_scalar(call.arg(4)) == 1,
        date: values::as_logical_scalar(call.arg(5)) == 1,
    };
    let directory = Path::new(&crate::connections::expand_path(&target)).to_path_buf();
    let result = alloc(SexpType::Logical, length(sources))?;
    for index in 0..length(sources) {
        let mut copied = false;
        if let Some(source) = values::string_at(sources, index) {
            let source = Path::new(&crate::connections::expand_path(&source)).to_path_buf();
            if let Some(name) = source.file_name() {
                let destination = directory.join(name);
                match copy_entry(&source, &destination, &options) {
                    Ok(outcome) => copied = outcome,
                    Err(failure) => crate::conditions::warning(format!(
                        "problem copying {} to {}: {}",
                        source.display(),
                        destination.display(),
                        crate::connections::os_message(&failure)
                    ))?,
                }
            }
        }
        values::ints_mut(result)[index] = copied as i32;
    }
    Ok(result)
}

struct CopyOptions {
    overwrite: bool,
    recursive: bool,
    mode: bool,
    date: bool,
}

/// Copies one file or directory tree, reporting FALSE for anything skipped: a directory without
/// `recursive`, or an existing file without `overwrite`.
fn copy_entry(source: &Path, target: &Path, options: &CopyOptions) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(source)?;
    if metadata.is_dir() {
        if !options.recursive {
            return Ok(false);
        }
        if !target.is_dir() {
            std::fs::create_dir(target)?;
        }
        let mut copied = true;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copied &= copy_entry(&entry.path(), &target.join(entry.file_name()), options)?;
        }
        if options.mode {
            std::fs::set_permissions(target, metadata.permissions())?;
        }
        return Ok(copied);
    }
    if target.exists() && !options.overwrite {
        return Ok(false);
    }
    std::fs::copy(source, target)?;
    if options.mode {
        std::fs::set_permissions(target, metadata.permissions())?;
    }
    if options.date {
        std::fs::File::options()
            .write(true)
            .open(target)?
            .set_modified(metadata.modified()?)?;
    }
    Ok(true)
}

fn append_file(target: &str, source: &str) -> std::io::Result<()> {
    let bytes = std::fs::read(source)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(target)?;
    std::io::Write::write_all(&mut file, &bytes)
}

unsafe fn do_sys_getenv(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let unset = values::first_string(call.arg(1)).map(|text| text.into_owned());
    if length(names) == 0 {
        let mut entries: Vec<String> = std::env::vars()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        entries.sort();
        return string_vector(&entries);
    }
    let count = length(names);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let name = values::string_at(names, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let charsxp = match std::env::var(&name) {
            Ok(value) => values::mkchar(&value)?,
            Err(_) => match &unset {
                Some(unset) => values::mkchar(unset)?,
                None => values::na_string(),
            },
        };
        values::set_elt(result, index, charsxp);
    }
    Ok(result)
}

unsafe fn do_sys_setenv(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let new_values = call.arg(1);
    let count = length(names);
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        let name = values::string_at(names, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let value = values::string_at(new_values, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        // SAFETY: the interpreter is single-threaded.
        std::env::set_var(&name, &value);
        values::ints_mut(result)[index] = 1;
    }
    Ok(result)
}

unsafe fn do_sys_unsetenv(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let count = length(names);
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        if let Some(name) = values::string_at(names, index) {
            // SAFETY: the interpreter is single-threaded.
            std::env::remove_var(name.as_ref());
        }
        values::ints_mut(result)[index] = 1;
    }
    Ok(result)
}

unsafe fn do_proc_time(_call: &Call<'_>) -> R<Sexp> {
    // GNU R reports whole milliseconds, which is what keeps printed timings short.
    let milliseconds = |seconds: f64| (seconds * 1000.0).round() / 1000.0;
    let times = crate::platform::cpu_times();
    let (user, system) = (milliseconds(times.user), milliseconds(times.system));
    let (child_user, child_system) = times.children.map_or((NA_REAL, NA_REAL), |(user, system)| {
        (milliseconds(user), milliseconds(system))
    });
    let elapsed = milliseconds(interp().started.elapsed().as_secs_f64());
    let result = values::real_vector(&[user, system, elapsed, child_user, child_system])?;
    let names = string_vector(&[
        "user.self",
        "sys.self",
        "elapsed",
        "user.child",
        "sys.child",
    ])?;
    values::set_attr(result, symbols().names, names)?;
    let class = scalar_string("proc_time")?;
    values::set_attr(result, symbols().class, class)?;
    Ok(result)
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

unsafe fn posixct(value: f64, tz: &str) -> R<Sexp> {
    let result = scalar_real(value)?;
    let class = string_vector(&["POSIXct", "POSIXt"])?;
    values::set_attr(result, symbols().class, class)?;
    if !tz.is_empty() {
        let tzone = values::install("tzone")?;
        values::set_attr(result, tzone, scalar_string(tz)?)?;
    }
    Ok(result)
}

unsafe fn do_sys_time(_call: &Call<'_>) -> R<Sexp> {
    posixct(now_seconds(), "")
}

// Time zones.

/// A time zone as R names it, with its rules from the system's tz database.
struct Zone {
    rules: jiff::tz::TimeZone,
    /// The abbreviation of a name the database does not know. GNU R, through the C library,
    /// treats such a zone as UTC under an abbreviation taken from the name's leading letters.
    unknown: Option<String>,
}

impl Zone {
    /// The UTC offset in seconds, whether daylight saving time is in effect, and the zone
    /// abbreviation at `seconds` since the epoch.
    fn offset_at(&self, seconds: f64) -> (i32, bool, String) {
        if let Some(abbreviation) = &self.unknown {
            return (0, false, abbreviation.clone());
        }
        let info = self.rules.to_offset_info(timestamp(seconds));
        (
            info.offset().seconds(),
            info.dst().is_dst(),
            info.abbreviation().to_owned(),
        )
    }

    /// The instant a wall-clock time names, given as seconds since the epoch read as UTC.
    /// A time skipped by a forward transition reads with the offset after it, and a repeated
    /// time reads as its first occurrence, as the C library's `mktime` does; a known `isdst`
    /// picks the matching side instead.
    fn local_to_utc(&self, local: f64, isdst: i32) -> f64 {
        if !local.is_finite() {
            return local;
        }
        if self.unknown.is_some() {
            return if isdst == 1 { local - 3600.0 } else { local };
        }
        let shift = cycles_into_range(local) * GREGORIAN_CYCLE;
        let local = local + shift;
        let time = broken_down(local);
        let fraction = local - local.floor();
        let datetime = jiff::civil::DateTime::new(
            time.year as i16,
            time.month as i8,
            time.day as i8,
            time.hour as i8,
            time.minute as i8,
            time.second.floor() as i8,
            0,
        );
        let Ok(datetime) = datetime else {
            return local - shift;
        };
        use jiff::tz::AmbiguousOffset;
        let offset = match self.rules.to_ambiguous_timestamp(datetime).offset() {
            AmbiguousOffset::Unambiguous { offset } => {
                // Like C's `mktime`, an `isdst` that contradicts the zone's rule at that wall
                // time reads the clock as the other kind of time, an hour apart.
                let seconds = offset.seconds() as f64;
                let instant = local.floor() - seconds;
                let dst = self.rules.to_offset_info(timestamp(instant)).dst().is_dst();
                // glibc applies this even in a zone that never observes daylight saving time.
                let correction = match (isdst, dst) {
                    (0, true) => -3600.0,
                    (1, false) => 3600.0,
                    _ => 0.0,
                };
                return instant - correction + fraction - shift;
            }
            // In a spring-forward gap glibc's `mktime` reads the clock with the offset before
            // the gap unless told the time is daylight time, so an unknown DST flag lands
            // after the gap: New York's 02:30 becomes 03:30 EDT.
            AmbiguousOffset::Gap { before, after } => {
                if isdst == 1 {
                    after
                } else {
                    before
                }
            }
            // glibc's `mktime` resolves a repeated time with the offset in force at the wall
            // clock read as UTC, so Berlin's 02:30 reads as CET and New York's 01:30 as EDT.
            AmbiguousOffset::Fold { before, after } => match isdst {
                0 => after,
                1 => before,
                _ => self.rules.to_offset(timestamp(local)),
            },
        };
        local.floor() - offset.seconds() as f64 + fraction - shift
    }

    /// The standard and daylight-saving abbreviations GNU R stores after the zone's name in
    /// a POSIXlt's `tzone`: the most recent of each kind the zone has used.
    fn abbreviations(&self) -> (String, String) {
        if let Some(abbreviation) = &self.unknown {
            return (abbreviation.clone(), abbreviation.clone());
        }
        let now = jiff::Timestamp::now();
        let current = self.rules.to_offset_info(now);
        let mut standard = (!current.dst().is_dst()).then(|| current.abbreviation().to_owned());
        let mut daylight = current
            .dst()
            .is_dst()
            .then(|| current.abbreviation().to_owned());
        for transition in self.rules.preceding(now) {
            if standard.is_some() && daylight.is_some() {
                break;
            }
            let slot = if transition.dst().is_dst() {
                &mut daylight
            } else {
                &mut standard
            };
            if slot.is_none() {
                *slot = Some(transition.abbreviation().to_owned());
            }
        }
        let standard = standard.unwrap_or_else(|| current.abbreviation().to_owned());
        let daylight = daylight.unwrap_or_else(|| standard.clone());
        (standard, daylight)
    }
}

/// The zone `tz` names, where the empty name is the session's zone: `TZ`, or else the system's.
fn resolve_zone(tz: &str) -> Zone {
    let name = if tz.is_empty() {
        session_zone_name()
    } else {
        tz.to_owned()
    };
    // glibc reads `TZ=":Europe/Berlin"` as the zone file name after the colon.
    let name = name.strip_prefix(':').map_or(name.clone(), str::to_owned);
    if name.is_empty() {
        return Zone {
            rules: jiff::tz::TimeZone::system(),
            unknown: None,
        };
    }
    let found = jiff::tz::TimeZone::get(&name).or_else(|_| jiff::tz::TimeZone::posix(&name));
    match found {
        Ok(rules) => Zone {
            rules,
            unknown: None,
        },
        Err(_) => {
            let letters: String = name.chars().take_while(char::is_ascii_alphabetic).collect();
            let abbreviation = if letters.len() >= 3 {
                letters
            } else {
                String::new()
            };
            Zone {
                rules: jiff::tz::TimeZone::UTC,
                unknown: Some(abbreviation),
            }
        }
    }
}

/// The session's zone name as GNU R reports it: the value of `TZ`, or the empty name when
/// `TZ` is unset and the system's zone applies.
fn session_zone_name() -> String {
    std::env::var("TZ").unwrap_or_default()
}

/// The instant `seconds` names, moved by whole 400-year cycles into the range jiff covers.
fn timestamp(seconds: f64) -> jiff::Timestamp {
    let whole = seconds.floor() + cycles_into_range(seconds) * GREGORIAN_CYCLE;
    jiff::Timestamp::from_second(whole as i64).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

/// The seconds in 400 Gregorian years, after which the calendar and the weekdays repeat.
const GREGORIAN_CYCLE: f64 = 146_097.0 * 86_400.0;

/// The number of 400-year cycles to add to `seconds` to land inside jiff's range (a year
/// within -9999..=9999), so dates far outside it read with the rules of an equivalent year.
fn cycles_into_range(seconds: f64) -> f64 {
    // A cycle short of each bound, so a shifted wall-clock time stays representable too.
    let upper = jiff::Timestamp::MAX.as_second() as f64 - GREGORIAN_CYCLE;
    let lower = jiff::Timestamp::MIN.as_second() as f64 + GREGORIAN_CYCLE;
    if seconds > upper {
        -((seconds - upper) / GREGORIAN_CYCLE).ceil()
    } else if seconds < lower {
        ((lower - seconds) / GREGORIAN_CYCLE).ceil()
    } else {
        0.0
    }
}

// Calendar arithmetic, in UTC.

/// Days since 1970-01-01 for a civil date.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// The civil date of a day count since 1970-01-01: year, month (1-12), day.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146096
    } / 146097;
    let day_of_era = shifted - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

struct BrokenDown {
    second: f64,
    minute: i64,
    hour: i64,
    day: i64,
    month: i64,
    year: i64,
    weekday: i64,
    yearday: i64,
}

fn broken_down(seconds: f64) -> BrokenDown {
    let days = (seconds / 86400.0).floor() as i64;
    let rest = seconds - days as f64 * 86400.0;
    let (year, month, day) = civil_from_days(days);
    let hour = (rest / 3600.0).floor() as i64;
    let minute = ((rest - hour as f64 * 3600.0) / 60.0).floor() as i64;
    let second = rest - hour as f64 * 3600.0 - minute as f64 * 60.0;
    let weekday = (days + 4).rem_euclid(7);
    let yearday = days - days_from_civil(year, 1, 1);
    BrokenDown {
        second,
        minute,
        hour,
        day,
        month,
        year,
        weekday,
        yearday,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

fn format_broken_down(time: &BrokenDown, format: &str, zone: &str, offset: i32) -> String {
    let mut output = String::new();
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        // The GNU extensions: a padding flag (`_` spaces, `0` zeros, `-` none) and a field width.
        let mut pad = None;
        while let Some(flag @ ('_' | '0' | '-')) = characters.peek().copied() {
            characters.next();
            pad = Some(flag);
        }
        let mut width = None;
        while let Some(digit) = characters.peek().and_then(|digit| digit.to_digit(10)) {
            characters.next();
            width = Some(width.unwrap_or(0) * 10 + digit as usize);
        }
        let Some(mut spec) = characters.next() else {
            output.push('%');
            break;
        };
        // The POSIX `E` and `O` modifiers ask for a locale's alternative forms, which the C
        // locale does not have, so they print the plain field; only `%OS` means something else.
        if spec == 'E' || (spec == 'O' && characters.peek() != Some(&'S')) {
            match characters.next() {
                Some(base) => spec = base,
                None => {
                    output.push('%');
                    output.push(spec);
                    break;
                }
            }
        }
        let start = output.len();
        match spec {
            'Y' => output.push_str(&time.year.to_string()),
            'y' => output.push_str(&format!("{:02}", time.year.rem_euclid(100))),
            'm' => output.push_str(&format!("{:02}", time.month)),
            'd' => output.push_str(&format!("{:02}", time.day)),
            'e' => output.push_str(&format!("{:2}", time.day)),
            'H' => output.push_str(&format!("{:02}", time.hour)),
            'I' => output.push_str(&format!(
                "{:02}",
                if time.hour % 12 == 0 {
                    12
                } else {
                    time.hour % 12
                }
            )),
            'M' => output.push_str(&format!("{:02}", time.minute)),
            'S' => output.push_str(&format!("{:02}", time.second.floor() as i64)),
            'p' => output.push_str(if time.hour < 12 { "AM" } else { "PM" }),
            'P' => output.push_str(if time.hour < 12 { "am" } else { "pm" }),
            's' => {
                let local = days_from_civil(time.year, 1, 1) + time.yearday;
                let seconds = local * 86400
                    + time.hour * 3600
                    + time.minute * 60
                    + time.second.floor() as i64
                    - i64::from(offset);
                output.push_str(&seconds.to_string());
            }
            'j' => output.push_str(&format!("{:03}", time.yearday + 1)),
            'B' => output.push_str(MONTHS[(time.month - 1) as usize]),
            'b' | 'h' => output.push_str(&MONTHS[(time.month - 1) as usize][..3]),
            'A' => output.push_str(WEEKDAYS[time.weekday as usize]),
            'a' => output.push_str(&WEEKDAYS[time.weekday as usize][..3]),
            'u' => output.push_str(&(if time.weekday == 0 { 7 } else { time.weekday }).to_string()),
            'w' => output.push_str(&time.weekday.to_string()),
            'U' => output.push_str(&format!("{:02}", (time.yearday + 7 - time.weekday) / 7)),
            'W' => output.push_str(&format!(
                "{:02}",
                (time.yearday + 7 - (time.weekday + 6) % 7) / 7
            )),
            'V' => output.push_str(&format!("{:02}", iso_week(time).1)),
            'G' => output.push_str(&iso_week(time).0.to_string()),
            'g' => output.push_str(&format!("{:02}", iso_week(time).0.rem_euclid(100))),
            'C' => output.push_str(&format!("{:02}", time.year.div_euclid(100))),
            'k' => output.push_str(&format!("{:2}", time.hour)),
            'l' => output.push_str(&format!(
                "{:2}",
                if time.hour % 12 == 0 {
                    12
                } else {
                    time.hour % 12
                }
            )),
            'x' => output.push_str(&format!(
                "{:02}/{:02}/{:02}",
                time.month,
                time.day,
                time.year.rem_euclid(100)
            )),
            'X' => output.push_str(&format!(
                "{:02}:{:02}:{:02}",
                time.hour,
                time.minute,
                time.second.floor() as i64
            )),
            'r' => output.push_str(&format!(
                "{:02}:{:02}:{:02} {}",
                if time.hour % 12 == 0 {
                    12
                } else {
                    time.hour % 12
                },
                time.minute,
                time.second.floor() as i64,
                if time.hour < 12 { "AM" } else { "PM" }
            )),
            'c' => output.push_str(&format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                &WEEKDAYS[time.weekday as usize][..3],
                &MONTHS[(time.month - 1) as usize][..3],
                time.day,
                time.hour,
                time.minute,
                time.second.floor() as i64,
                time.year
            )),
            'Z' => output.push_str(zone),
            'z' => {
                let sign = if offset < 0 { '-' } else { '+' };
                let minutes = offset.unsigned_abs() / 60;
                output.push_str(&format!("{sign}{:02}{:02}", minutes / 60, minutes % 60));
            }
            'F' => output.push_str(&format!("{}-{:02}-{:02}", time.year, time.month, time.day)),
            'T' => output.push_str(&format!(
                "{:02}:{:02}:{:02}",
                time.hour,
                time.minute,
                time.second.floor() as i64
            )),
            'D' => output.push_str(&format!(
                "{:02}/{:02}/{:02}",
                time.month,
                time.day,
                time.year.rem_euclid(100)
            )),
            'R' => output.push_str(&format!("{:02}:{:02}", time.hour, time.minute)),
            'n' => output.push('\n'),
            't' => output.push('\t'),
            '%' => output.push('%'),
            'O' => {
                if characters.peek() == Some(&'S') {
                    characters.next();
                    let digits = characters.peek().and_then(|digit| digit.to_digit(10));
                    match digits {
                        Some(digits) => {
                            characters.next();
                            let width = 3 + digits as usize;
                            // GNU R truncates to the digits asked for rather than rounding, so a
                            // second never prints as 60.
                            let scale = 10f64.powi(digits as i32);
                            let second = (time.second * scale).floor() / scale;
                            output.push_str(&format!(
                                "{:0width$.digits$}",
                                second,
                                width = width,
                                digits = digits as usize
                            ));
                        }
                        None => output.push_str(&format!("{:02}", time.second.floor() as i64)),
                    }
                }
            }
            other => {
                output.push('%');
                output.push(other);
            }
        }
        if (pad.is_some() || width.is_some()) && "YymdeHIMSjCGgUWVuwkls".contains(spec) {
            let piece = output.split_off(start);
            let trimmed = piece.trim_start();
            let (sign, digits) = match trimmed.strip_prefix('-') {
                Some(rest) => ("-", rest),
                None => ("", trimmed),
            };
            let digits = if pad.is_some() {
                let stripped = digits.trim_start_matches('0');
                if stripped.is_empty() { "0" } else { stripped }
            } else {
                digits
            };
            let natural = format!("{sign}{digits}");
            let width = match (pad, width) {
                (Some('-'), _) => 0,
                (_, Some(width)) => width,
                // A flag alone keeps the conversion's usual width.
                _ => piece.len(),
            };
            let fill = width.saturating_sub(natural.len());
            if pad == Some('_') || (pad.is_none() && piece.starts_with(' ')) {
                output.push_str(&" ".repeat(fill));
                output.push_str(&natural);
            } else {
                output.push_str(sign);
                output.push_str(&"0".repeat(fill));
                output.push_str(digits);
            }
        }
    }
    output
}

/// The ISO 8601 week-numbering year and week: weeks start on Monday, and week 1 is the one
/// that holds the year's first Thursday.
fn iso_week(time: &BrokenDown) -> (i64, i64) {
    let weeks_in = |year: i64| {
        let starts = |year: i64| {
            (year + year.div_euclid(4) - year.div_euclid(100) + year.div_euclid(400)).rem_euclid(7)
        };
        if starts(year) == 4 || starts(year - 1) == 3 {
            53
        } else {
            52
        }
    };
    let iso_weekday = if time.weekday == 0 { 7 } else { time.weekday };
    let week = (time.yearday + 1 - iso_weekday + 10).div_euclid(7);
    if week < 1 {
        (time.year - 1, weeks_in(time.year - 1))
    } else if week > weeks_in(time.year) {
        (time.year + 1, 1)
    } else {
        (time.year, week)
    }
}

const POSIXLT_NAMES: [&str; 11] = [
    "sec", "min", "hour", "mday", "mon", "year", "wday", "yday", "isdst", "zone", "gmtoff",
];

/// A POSIXlt list for `seconds` since the epoch.
unsafe fn posixlt_from_seconds(seconds: &[f64], tz: &str) -> R<Sexp> {
    let count = seconds.len();
    let fields: Vec<Sexp> = (0..11)
        .map(|field| match field {
            0 => alloc(SexpType::Real, count),
            9 => alloc(SexpType::String, count),
            _ => alloc(SexpType::Integer, count),
        })
        .collect::<R<_>>()?;
    let zone = resolve_zone(tz);
    let name = if tz.is_empty() {
        session_zone_name()
    } else {
        tz.to_owned()
    };
    let utc = name == "UTC" || name == "GMT";
    for (index, &value) in seconds.iter().enumerate() {
        if !value.is_finite() {
            // GNU R keeps a non-finite time, including NA versus NaN, in `sec`.
            values::reals_mut(fields[0])[index] = value;
            for field in 1..8 {
                values::ints_mut(fields[field])[index] = NA_INTEGER;
            }
            values::ints_mut(fields[8])[index] = -1;
            // UTC and GMT name themselves and have a known offset even for a missing time.
            let (zone_name, offset) = if utc {
                (name.as_str(), 0)
            } else {
                ("", NA_INTEGER)
            };
            values::set_elt(fields[9], index, values::mkchar(zone_name)?);
            values::ints_mut(fields[10])[index] = offset;
            continue;
        }
        let (offset, dst, abbreviation) = zone.offset_at(value);
        let time = broken_down(value + offset as f64);
        values::reals_mut(fields[0])[index] = time.second;
        values::ints_mut(fields[1])[index] = time.minute as i32;
        values::ints_mut(fields[2])[index] = time.hour as i32;
        values::ints_mut(fields[3])[index] = time.day as i32;
        values::ints_mut(fields[4])[index] = time.month as i32 - 1;
        values::ints_mut(fields[5])[index] = time.year as i32 - 1900;
        values::ints_mut(fields[6])[index] = time.weekday as i32;
        values::ints_mut(fields[7])[index] = time.yearday as i32;
        values::ints_mut(fields[8])[index] = dst as i32;
        values::set_elt(fields[9], index, values::mkchar(&abbreviation)?);
        values::ints_mut(fields[10])[index] = offset;
    }
    let list = values::list_vector(&fields)?;
    let field_names = string_vector(&POSIXLT_NAMES)?;
    values::set_attr(list, symbols().names, field_names)?;
    let class = string_vector(&["POSIXlt", "POSIXt"])?;
    values::set_attr(list, symbols().class, class)?;
    // UTC and GMT keep their bare name; every other zone, the session's included, adds its
    // standard and daylight-saving abbreviations.
    let tzone = values::install("tzone")?;
    let names = if utc {
        scalar_string(&name)?
    } else {
        let (standard, daylight) = zone.abbreviations();
        string_vector(&[name.as_str(), standard.as_str(), daylight.as_str()])?
    };
    values::set_attr(list, tzone, names)?;
    let balanced = values::install("balanced")?;
    values::set_attr(list, balanced, scalar_bool(true)?)?;
    Ok(list)
}

unsafe fn do_as_posixlt(call: &Call<'_>) -> R<Sexp> {
    let times = crate::coerce::coerce_vector(call.arg(0), SexpType::Real)?;
    let tz = values::first_string(call.arg(1))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let seconds: Vec<f64> = values::reals(times).to_vec();
    let result = posixlt_from_seconds(&seconds, &tz)?;
    set_year_names(result, values::names(call.arg(0)))?;
    Ok(result)
}

unsafe fn require_posixlt(list: Sexp) -> R<()> {
    if list.kind() != SexpType::List || length(list) < 9 {
        return Err(error(
            "a valid \"POSIXlt\" object is a list of at least 9 elements",
        ));
    }
    Ok(())
}

/// The instants a POSIXlt's wall-clock fields name in the zone `tz`.
unsafe fn posixlt_instants(list: Sexp, tz: &str) -> R<Vec<f64>> {
    let mut seconds = posixlt_seconds(list)?;
    let zone = resolve_zone(tz);
    // GNU R converts UTC and GMT itself rather than through `mktime`, which ignores `isdst`.
    let name = if tz.is_empty() {
        session_zone_name()
    } else {
        tz.to_owned()
    };
    let honors_isdst = name != "UTC" && name != "GMT";
    let isdst = if length(list) > 8 {
        list.element(8)
    } else {
        nil()
    };
    for (position, value) in seconds.iter_mut().enumerate() {
        let flag = if length(isdst) == 0 || !honors_isdst {
            -1
        } else {
            let flag = crate::subset::numeric_element(isdst, position % length(isdst));
            if flag.is_nan() { -1 } else { flag as i32 }
        };
        *value = zone.local_to_utc(*value, flag);
    }
    Ok(seconds)
}

/// The zone a POSIXlt's `tzone` attribute names, or the session's zone.
unsafe fn posixlt_zone(list: Sexp) -> R<String> {
    let tzone = get_attr(list, values::install("tzone")?);
    Ok(values::first_string(tzone)
        .map(|text| text.into_owned())
        .unwrap_or_default())
}

/// A POSIXlt's wall-clock fields as seconds since the epoch read as UTC.
unsafe fn posixlt_seconds(list: Sexp) -> R<Vec<f64>> {
    require_posixlt(list)?;
    let field = |index: usize| -> Sexp {
        if length(list) > index {
            list.element(index)
        } else {
            nil()
        }
    };
    let count = (0..6).map(|index| length(field(index))).max().unwrap_or(0);
    let at = |index: usize, position: usize| -> f64 {
        let vector = field(index);
        let size = length(vector);
        if size == 0 {
            return NA_REAL;
        }
        crate::subset::numeric_element(vector, position % size)
    };
    let mut seconds = Vec::with_capacity(count);
    for position in 0..count {
        let second = at(0, position);
        let minute = at(1, position);
        let hour = at(2, position);
        let day = at(3, position);
        let month = at(4, position);
        let year = at(5, position);
        if !second.is_finite() {
            seconds.push(second);
            continue;
        }
        if [minute, hour, day, month, year]
            .iter()
            .any(|value| value.is_nan())
        {
            seconds.push(NA_REAL);
            continue;
        }
        let total_months = year as i64 * 12 + month as i64;
        let year = 1900 + total_months.div_euclid(12);
        let month = total_months.rem_euclid(12) + 1;
        let days = days_from_civil(year, month, 1) + day as i64 - 1;
        seconds.push(days as f64 * 86400.0 + hour * 3600.0 + minute * 60.0 + second);
    }
    Ok(seconds)
}

/// Normalizes out-of-range POSIXlt fields, or with `fill_only` recycles every field to the
/// longest one and marks the result `balanced = NA`, as GNU R's `balancePOSIXlt` does.
unsafe fn balance_posixlt(list: Sexp, fill_only: bool, classed: bool) -> R<Sexp> {
    require_posixlt(list)?;
    let balanced_symbol = values::install("balanced")?;
    let mark = get_attr(list, balanced_symbol);
    // `balanced = TRUE` covers both requests, and `NA` (filled only) satisfies a fill.
    let already = mark.kind() == SexpType::Logical
        && length(mark) == 1
        && match values::ints(mark)[0] {
            NA_LOGICAL => fill_only,
            0 => false,
            _ => true,
        };
    let result = if already {
        let result = values::shallow_duplicate(list)?;
        crate::heap().root(result);
        result
    } else if fill_only {
        let count = (0..length(list))
            .map(|index| length(list.element(index)))
            .max()
            .unwrap_or(0);
        let result = values::shallow_duplicate(list)?;
        crate::heap().root(result);
        for index in 0..length(list) {
            let field = list.element(index);
            let size = length(field);
            if size == count || size == 0 {
                continue;
            }
            let filled = alloc(field.kind(), count)?;
            for position in 0..count {
                crate::subset::copy_element(field, position % size, filled, position)?;
            }
            values::set_elt(result, index, filled);
        }
        values::set_attr(result, balanced_symbol, values::scalar_logical(NA_LOGICAL)?)?;
        result
    } else {
        // A POSIXlt with `zone` and `gmtoff` balances through its instants, as `as.POSIXct`
        // reads them. One without stays without them: GNU R normalizes its wall clock as plain
        // calendar arithmetic and keeps its `isdst` and `tzone`.
        let tz = posixlt_zone(list)?;
        if length(list) >= 11 {
            let seconds = posixlt_instants(list, &tz)?;
            posixlt_from_seconds(&seconds, &tz)?
        } else {
            let wall = posixlt_seconds(list)?;
            let full = posixlt_from_seconds(&wall, "UTC")?;
            crate::heap().root(full);
            let fields = length(list);
            let result = alloc(SexpType::List, fields)?;
            crate::heap().root(result);
            for index in 0..fields.min(8) {
                values::set_elt(result, index, full.element(index));
            }
            if fields > 8 {
                let source = list.element(8);
                let isdst = alloc(SexpType::Integer, wall.len())?;
                for (position, flag) in values::ints_mut(isdst).iter_mut().enumerate() {
                    let value = if length(source) == 0 {
                        -1.0
                    } else {
                        crate::subset::numeric_element(source, position % length(source))
                    };
                    *flag = if value.is_nan() { -1 } else { value as i32 };
                }
                values::set_elt(result, 8, isdst);
            }
            let names: Vec<&str> = POSIXLT_NAMES.iter().take(fields).copied().collect();
            values::set_attr(result, symbols().names, string_vector(&names)?)?;
            values::set_attr(
                result,
                symbols().class,
                string_vector(&["POSIXlt", "POSIXt"])?,
            )?;
            let tzone_symbol = values::install("tzone")?;
            let tzone = get_attr(list, tzone_symbol);
            if tzone != nil() {
                values::set_attr(result, tzone_symbol, tzone)?;
            }
            values::set_attr(result, balanced_symbol, scalar_bool(true)?)?;
            result
        }
    };
    if !already && length(list) > 5 && length(result) > 5 {
        // The year's names recycle with the fields, as GNU R balances them.
        let names = values::names(list.element(5));
        let count = length(result.element(5));
        if names != nil() && length(names) > 0 && length(names) != count {
            let recycled = alloc(SexpType::String, count)?;
            for position in 0..count {
                values::set_elt(recycled, position, names.element(position % length(names)));
            }
            set_year_names(result, recycled)?;
        } else {
            set_year_names(result, names)?;
        }
    }
    if !classed {
        values::remove_attr(result, symbols().class);
    }
    Ok(result)
}

/// `as.POSIXct(x, tz)`: the instants a POSIXlt names in the zone `tz`.
unsafe fn do_as_posixct(call: &Call<'_>) -> R<Sexp> {
    let tz = values::first_string(call.arg(1))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let seconds = posixlt_instants(call.arg(0), &tz)?;
    let result = values::real_vector(&seconds)?;
    set_names_from_year(call.arg(0), result)?;
    Ok(result)
}

/// A POSIXlt keeps its element names on the `year` field.
unsafe fn set_year_names(list: Sexp, names: Sexp) -> R<()> {
    if names != nil() && length(list) > 5 && length(list.element(5)) == length(names) {
        values::set_attr(list.element(5), symbols().names, names)?;
    }
    Ok(())
}

/// Names `result` after the POSIXlt's year names. A ragged POSIXlt's year names can be shorter
/// than the result, and GNU R pads them with NA.
unsafe fn set_names_from_year(list: Sexp, result: Sexp) -> R<()> {
    let names = values::names(list.element(5));
    if names == nil() {
        return Ok(());
    }
    let count = length(result);
    let padded = if length(names) == count {
        names
    } else {
        let padded = alloc(SexpType::String, count)?;
        for position in 0..count {
            let name = if position < length(names) {
                names.element(position)
            } else {
                values::na_string()
            };
            values::set_elt(padded, position, name);
        }
        padded
    };
    values::set_attr(result, symbols().names, padded)
}

/// `format.POSIXlt(x, format, usetz)`, plus R 4.4's `digits`. A bare `%OS` prints that many
/// decimals; before R 4.4 the C code read them from `options("digits.secs")` itself.
unsafe fn do_format_posixlt(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let formats = call.arg(1);
    let use_tz = values::as_logical_scalar(call.arg(2)) == 1;
    let digits = if call.len() > 3 {
        call.arg(3)
    } else {
        crate::envir::get_option("digits.secs")
    };
    let second_digits = if digits == nil() {
        0
    } else {
        values::as_integer_scalar(digits).clamp(0, 6)
    };
    let seconds = posixlt_seconds(list)?;
    let count = seconds.len();
    let result = alloc(SexpType::String, count)?;
    let tzone = get_attr(list, values::install("tzone")?);
    let field = |index: usize| {
        if length(list) > index {
            list.element(index)
        } else {
            nil()
        }
    };
    let (isdst, zones, offsets) = (field(8), field(9), field(10));
    for (index, &value) in seconds.iter().enumerate() {
        if is_na_real(value) {
            values::set_elt(result, index, values::na_string());
            continue;
        }
        if !value.is_finite() {
            let text = if value.is_nan() {
                "NaN"
            } else if value > 0.0 {
                "Inf"
            } else {
                "-Inf"
            };
            values::set_elt(result, index, values::mkchar(text)?);
            continue;
        }
        let mut format = values::string_at(formats, index % length(formats).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".into());
        if second_digits > 0 {
            let pieces: Vec<&str> = format.split("%OS").collect();
            let mut widened = pieces[0].to_owned();
            for piece in &pieces[1..] {
                widened.push_str("%OS");
                if !piece.starts_with(|character: char| character.is_ascii_digit()) {
                    widened.push_str(&second_digits.to_string());
                }
                widened.push_str(piece);
            }
            format = widened;
        }
        // The element's own zone fields win; a POSIXlt without them falls back to `tzone`.
        let dst = if length(isdst) > 0 {
            crate::subset::numeric_element(isdst, index % length(isdst))
        } else {
            0.0
        };
        let zone = if zones.kind() == SexpType::String && length(zones) > 0 {
            values::string_at(zones, index % length(zones))
                .map(|text| text.into_owned())
                .unwrap_or_default()
        } else {
            let slot = if dst > 0.0 { 2 } else { 1 };
            values::string_at(tzone, slot.min(length(tzone).saturating_sub(1)))
                .map(|text| text.into_owned())
                .unwrap_or_default()
        };
        let known = if length(offsets) > 0 {
            Some(crate::subset::numeric_element(
                offsets,
                index % length(offsets),
            ))
            .filter(|offset| !offset.is_nan())
        } else {
            Some(0.0)
        };
        // An unknown offset (strptime leaves it NA outside UTC) comes from the zone's rules at
        // that wall-clock time, as C's `mktime` fills it in for GNU R.
        let offset = match known {
            Some(offset) => offset as i32,
            None => {
                let name = values::string_at(tzone, 0)
                    .map(|text| text.into_owned())
                    .unwrap_or_default();
                let flag = if dst.is_nan() { -1 } else { dst as i32 };
                (value - resolve_zone(&name).local_to_utc(value, flag)) as i32
            }
        };
        let mut text = format_broken_down(&broken_down(value), &format, &zone, offset);
        if use_tz && !zone.is_empty() {
            text.push(' ');
            text.push_str(&zone);
        }
        values::set_elt(result, index, values::mkchar(&text)?);
    }
    set_names_from_year(list, result)?;
    Ok(result)
}

/// A parsed time: whole wall-clock seconds since the epoch read as UTC, the fraction of a second
/// apart so that it keeps every digit the text gave, and the UTC offset a `%z` gave.
struct ParsedTime {
    whole: f64,
    fraction: f64,
    offset: Option<i64>,
}

/// Parses `text` with a `strptime` format.
fn parse_time(text: &str, format: &str, warnings: &mut Vec<String>) -> R<Option<ParsedTime>> {
    let mut fields = Parsed::default();
    let mut input = text;
    let matched = parse_with(&mut fields, &mut input, format);
    // Like GNU R, a `%Z` is an error only once parsing reaches it, and an out-of-range `%z`
    // warns, so a text that fails earlier stays a quiet NA.
    if fields.zone_requested {
        return Err(error("use of %Z for input is not supported"));
    }
    if fields.offset_out_of_range {
        warnings.push("values for %z outside +/-1400 are an error".to_owned());
    }
    Ok(matched.and_then(|()| resolve_fields(fields, warnings)))
}

fn resolve_fields(mut fields: Parsed, warnings: &mut Vec<String>) -> Option<ParsedTime> {
    if let (Some((week, monday_first)), Some(weekday), None) =
        (fields.week, fields.weekday, fields.yearday)
        && (fields.month.is_none() || fields.day.is_none())
        && let Some(year) = fields.year
    {
        // GNU R's formula from the weekday of 1 January, with the weekday not reduced modulo
        // 7, so that `%u` 7 (Sunday) closes a Monday-first week.
        let first = (days_from_civil(year, 1, 1) + 4).rem_euclid(7);
        let shift = i64::from(monday_first);
        fields.yearday = Some((7 - (first - shift)) % 7 + (week - 1) * 7 + weekday - shift + 1);
    }
    // GNU R fills an unspecified year, month or day from today's date, except that a month
    // without a day is invalid, since today's day need not exist in that month.
    if fields.month.is_some() && fields.day.is_none() && fields.yearday.is_none() {
        return None;
    }
    let (this_year, this_month, this_day) =
        if fields.year.is_some() && fields.month.is_some() && fields.day.is_some() {
            (0, 0, 0)
        } else {
            let now = now_seconds().floor();
            let (offset, _, _) = resolve_zone("").offset_at(now);
            civil_from_days((now as i64 + offset as i64).div_euclid(86400))
        };
    let year = match (fields.century, fields.year) {
        (Some(century), Some(year)) => century * 100 + year.rem_euclid(100),
        (Some(century), None) => century * 100,
        (None, year) => year.unwrap_or(this_year),
    };
    let mut hour = fields.hour;
    if let (Some(afternoon), true) = (fields.afternoon, fields.twelve_hour) {
        hour = hour % 12 + if afternoon { 12 } else { 0 };
    }
    let (mut month, mut day) = (
        fields.month.unwrap_or(this_month),
        fields.day.unwrap_or(this_day),
    );
    if let Some(yearday) = fields.yearday {
        if !(1..=if is_leap(year) { 366 } else { 365 }).contains(&yearday) {
            if yearday == 366 {
                warnings.push(format!("day-of-year 366 in year {year} is invalid"));
            }
            return None;
        }
        month = 1;
        day = yearday;
    } else {
        if !(1..=12).contains(&month) {
            return None;
        }
        let month_days = [
            31,
            if is_leap(year) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        if day < 1 || day > month_days[(month - 1) as usize] {
            return None;
        }
    }
    if hour > 24
        || fields.minute > 59
        || fields.second > 61.0
        || hour == 24 && (fields.minute > 0 || fields.second > 0.0)
    {
        return None;
    }
    let days = days_from_civil(year, month, 1) + day - 1;
    let whole = fields.second.floor();
    Some(ParsedTime {
        whole: days as f64 * 86400.0 + hour as f64 * 3600.0 + fields.minute as f64 * 60.0 + whole,
        fraction: fields.second - whole,
        offset: fields.offset,
    })
}

#[derive(Default)]
struct Parsed {
    year: Option<i64>,
    century: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    hour: i64,
    minute: i64,
    second: f64,
    yearday: Option<i64>,
    afternoon: Option<bool>,
    /// `%p` adjusts only an hour read with `%I` or `%l`.
    twelve_hour: bool,
    offset: Option<i64>,
    /// Sunday is 0; `%u` reads Sunday as 7, which GNU R keeps as 0.
    weekday: Option<i64>,
    /// A `%U` or `%W` week number, and whether weeks start on Monday (`%W`).
    week: Option<(i64, bool)>,
    zone_requested: bool,
    offset_out_of_range: bool,
}

/// Matches `input` against `format`, advancing `input` past what matched. Whitespace in the
/// format matches any run of whitespace, including none, and numeric fields skip leading blanks,
/// as in glibc's `strptime`.
fn parse_with(fields: &mut Parsed, input: &mut &str, format: &str) -> Option<()> {
    let mut characters = format.chars();
    while let Some(character) = characters.next() {
        if character.is_whitespace() {
            *input = input.trim_start();
            continue;
        }
        if character != '%' {
            *input = input.strip_prefix(character)?;
            continue;
        }
        let mut spec = characters.next()?;
        if spec == 'E' || spec == 'O' {
            let base = characters.next()?;
            // GNU R's strptime fails on a modifier POSIX does not allow, such as `%Ob`, and
            // lubridate relies on that to rank the formats it guesses.
            let allowed = if spec == 'E' {
                "cCyYxX"
            } else {
                "deHImMSUVWwy"
            };
            if !allowed.contains(base) {
                return None;
            }
            if spec == 'O' && base == 'S' {
                // `%OS` reads seconds with an optional fraction; `%OSn` is for output only, so on
                // input its digit is a literal the text must contain.
                let trimmed = input.trim_start();
                let integer = trimmed
                    .chars()
                    .take(2)
                    .take_while(char::is_ascii_digit)
                    .count();
                if integer == 0 {
                    return None;
                }
                let fraction = match trimmed[integer..].strip_prefix(['.', ',']) {
                    Some(rest) => 1 + rest.chars().take_while(char::is_ascii_digit).count(),
                    None => 0,
                };
                fields.second = trimmed[..integer + fraction]
                    .replace(',', ".")
                    .parse()
                    .ok()?;
                *input = &trimmed[integer + fraction..];
                continue;
            }
            spec = base;
        }
        match spec {
            'Y' => {
                fields.year = Some(number(input, 4)?);
                fields.century = None;
            }
            'y' => {
                let value = number(input, 2)?;
                fields.year = Some(if value < 69 {
                    2000 + value
                } else {
                    1900 + value
                });
            }
            'C' => fields.century = Some(number(input, 2)?),
            'm' => fields.month = Some(number(input, 2)?),
            'd' | 'e' => fields.day = Some(number(input, 2)?),
            'H' | 'k' => {
                fields.hour = number(input, 2)?;
                fields.twelve_hour = false;
            }
            'I' | 'l' => {
                fields.hour = number(input, 2)?;
                fields.twelve_hour = true;
                if !(1..=12).contains(&fields.hour) {
                    return None;
                }
            }
            'M' => fields.minute = number(input, 2)?,
            'S' => fields.second = number(input, 2)? as f64,
            'j' => fields.yearday = Some(number(input, 3)?),
            'p' | 'P' => {
                let lower = input.trim_start().to_ascii_lowercase();
                fields.afternoon = Some(match lower.get(..2) {
                    Some("am") => false,
                    Some("pm") => true,
                    _ => return None,
                });
                *input = &input.trim_start()[2..];
            }
            'B' | 'b' | 'h' => {
                let found = match_name(input, &MONTHS)?;
                fields.month = Some(found as i64 + 1);
            }
            'A' | 'a' => {
                match_name(input, &WEEKDAYS)?;
            }
            'w' => fields.weekday = Some(Some(number(input, 1)?).filter(|day| *day <= 6)?),
            'u' => {
                fields.weekday =
                    Some(Some(number(input, 1)?).filter(|day| (1..=7).contains(day))? % 7)
            }
            'U' | 'W' => fields.week = Some((number(input, 2)?, spec == 'W')),
            'V' => {
                number(input, 2)?;
            }
            'z' => match parse_offset(input) {
                Ok(offset) => fields.offset = Some(offset),
                Err(out_of_range) => {
                    fields.offset_out_of_range = out_of_range;
                    return None;
                }
            },
            'Z' => {
                fields.zone_requested = true;
                return None;
            }
            'n' | 't' => *input = input.trim_start(),
            '%' => *input = input.trim_start().strip_prefix('%')?,
            'F' => parse_with(fields, input, "%Y-%m-%d")?,
            'T' | 'X' => parse_with(fields, input, "%H:%M:%S")?,
            'D' => parse_with(fields, input, "%m/%d/%y")?,
            'x' => parse_with(fields, input, "%y/%m/%d")?,
            'R' => parse_with(fields, input, "%H:%M")?,
            'r' => parse_with(fields, input, "%I:%M:%S %p")?,
            'c' => parse_with(fields, input, "%a %b %e %H:%M:%S %Y")?,
            _ => return None,
        }
    }
    Some(())
}

/// A month or weekday name, in full or abbreviated to three letters, ignoring case. Returns its
/// position in `names`.
fn match_name(input: &mut &str, names: &[&str]) -> Option<usize> {
    let trimmed = input.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let full = names
        .iter()
        .position(|name| lower.starts_with(&name.to_ascii_lowercase()));
    let (position, consumed) = match full {
        Some(position) => (position, names[position].len()),
        None => (
            names
                .iter()
                .position(|name| lower.starts_with(&name[..3].to_ascii_lowercase()))?,
            3,
        ),
    };
    *input = &trimmed[consumed..];
    Some(position)
}

/// A field of at most `max_digits` digits, after leading blanks.
fn number(input: &mut &str, max_digits: usize) -> Option<i64> {
    let trimmed = input.trim_start();
    let digits = trimmed
        .chars()
        .take(max_digits)
        .take_while(char::is_ascii_digit)
        .count();
    if digits == 0 {
        return None;
    }
    *input = &trimmed[digits..];
    trimmed[..digits].parse().ok()
}

/// A `%z` offset in seconds. The error is true when the offset is well formed but beyond
/// +/-1400, which GNU R warns about.
fn parse_offset(input: &mut &str) -> Result<i64, bool> {
    let trimmed = input.trim_start();
    let sign = match trimmed.chars().next() {
        Some('+') => 1,
        Some('-') => -1,
        _ => return Err(false),
    };
    let value = trimmed
        .get(1..5)
        .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse::<i64>().ok())
        .ok_or(false)?;
    let (hours, minutes) = (value / 100, value % 100);
    if minutes > 59 {
        return Err(false);
    }
    if value > 1400 {
        return Err(true);
    }
    *input = &trimmed[5..];
    Ok(sign * (hours * 3600 + minutes * 60))
}

unsafe fn do_strptime(call: &Call<'_>) -> R<Sexp> {
    let texts = call.arg(0);
    let formats = call.arg(1);
    let tz = values::first_string(call.arg(2))
        .map(|text| text.into_owned())
        .unwrap_or_default();
    let count = length(texts);
    let zone = resolve_zone(&tz);
    let name = if tz.is_empty() {
        session_zone_name()
    } else {
        tz.clone()
    };
    let utc = name == "UTC" || name == "GMT";
    let mut parsed = Vec::with_capacity(count);
    let mut warnings = Vec::new();
    for index in 0..count {
        let format = values::string_at(formats, index % length(formats).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_default();
        parsed.push(match values::string_at(texts, index) {
            Some(text) => parse_time(&text, &format, &mut warnings)?,
            None => None,
        });
    }
    let seconds: Vec<f64> = parsed
        .iter()
        .map(|time| match time {
            None => NA_REAL,
            Some(time) => match time.offset {
                Some(offset) => time.whole - offset as f64,
                None => zone.local_to_utc(time.whole, -1),
            },
        })
        .collect();
    let result = posixlt_from_seconds(&seconds, &tz)?;
    let fields: Vec<Sexp> = (0..11)
        .map(|field| values::list_elt(result, field))
        .collect();
    for (index, time) in parsed.iter().enumerate() {
        let Some(time) = time else { continue };
        values::reals_mut(fields[0])[index] += time.fraction;
        if utc {
            continue;
        }
        // GNU R's strptime fills the fields from the text rather than from an instant, so the
        // UTC offset stays unknown outside UTC, and a time with an explicit offset carries no
        // abbreviation either.
        values::ints_mut(fields[10])[index] = NA_INTEGER;
        if time.offset.is_some() {
            values::set_elt(fields[9], index, values::mkchar("")?);
            continue;
        }
        let (offset, _, _) = zone.offset_at(seconds[index]);
        if seconds[index] + offset as f64 != time.whole {
            // A time in a spring-forward gap keeps its wall-clock fields. Through R 4.3 it
            // reports the zone in force after the gap, where the wall clock read with the
            // earlier offset lands; from R 4.4 on its DST flag is unknown and its zone empty.
            let (dst, abbreviation) = if r_at_least(4, 4) {
                (-1, String::new())
            } else {
                let (_, dst, abbreviation) = zone.offset_at(seconds[index]);
                (dst as i32, abbreviation)
            };
            let wall = broken_down(time.whole);
            values::ints_mut(fields[1])[index] = wall.minute as i32;
            values::ints_mut(fields[2])[index] = wall.hour as i32;
            values::ints_mut(fields[3])[index] = wall.day as i32;
            values::ints_mut(fields[4])[index] = wall.month as i32 - 1;
            values::ints_mut(fields[5])[index] = wall.year as i32 - 1900;
            values::ints_mut(fields[6])[index] = wall.weekday as i32;
            values::ints_mut(fields[7])[index] = wall.yearday as i32;
            values::ints_mut(fields[8])[index] = dst;
            values::set_elt(fields[9], index, values::mkchar(&abbreviation)?);
        }
    }
    if !utc {
        values::ints_mut(fields[10]).fill(NA_INTEGER);
    }
    set_year_names(result, values::names(texts))?;
    for message in warnings {
        crate::conditions::warning(message)?;
    }
    Ok(result)
}

unsafe fn do_date_to_posixlt(call: &Call<'_>) -> R<Sexp> {
    let dates = crate::coerce::coerce_vector(call.arg(0), SexpType::Real)?;
    let seconds: Vec<f64> = values::reals(dates)
        .iter()
        .map(|&days| {
            if days.is_finite() {
                days.floor() * 86400.0
            } else {
                days
            }
        })
        .collect();
    let result = posixlt_from_seconds(&seconds, "UTC")?;
    // GNU R breaks a date down as midnight UTC whatever `tz` is, then labels every field and the
    // `tzone` attribute with `tz`, so `as.POSIXct` later reads that wall clock in `tz`.
    let tz = values::first_string(call.arg(1))
        .map(|text| text.into_owned())
        .unwrap_or_else(|| "UTC".to_owned());
    let label = values::mkchar(&tz)?;
    let zones = result.element(9);
    for (index, &value) in seconds.iter().enumerate() {
        if value.is_finite() {
            values::set_elt(zones, index, label);
        }
    }
    values::set_attr(result, values::install("tzone")?, scalar_string(&tz)?)?;
    set_year_names(result, values::names(call.arg(0)))?;
    Ok(result)
}

unsafe fn do_posixlt_to_date(call: &Call<'_>) -> R<Sexp> {
    let seconds = posixlt_seconds(call.arg(0))?;
    let days: Vec<f64> = seconds
        .iter()
        .map(|&value| {
            if value.is_finite() {
                (value / 86400.0).floor()
            } else {
                value
            }
        })
        .collect();
    let result = values::real_vector(&days)?;
    set_names_from_year(call.arg(0), result)?;
    let class = scalar_string("Date")?;
    values::set_attr(result, symbols().class, class)?;
    Ok(result)
}

unsafe fn do_file_info(call: &Call<'_>) -> R<Sexp> {
    let paths = call.arg(0);
    let count = length(paths);
    let size = alloc(SexpType::Real, count)?;
    let is_dir = alloc(SexpType::Logical, count)?;
    let mode = alloc(SexpType::Integer, count)?;
    let mtime = alloc(SexpType::Real, count)?;
    for index in 0..count {
        let path = values::string_at(paths, index)
            .map(|text| crate::connections::expand_path(&text))
            .unwrap_or_default();
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                values::reals_mut(size)[index] = metadata.len() as f64;
                values::ints_mut(is_dir)[index] = metadata.is_dir() as i32;
                values::ints_mut(mode)[index] =
                    (crate::platform::file_mode(Path::new(&path), &metadata) & 0o777) as i32;
                values::reals_mut(mtime)[index] = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(NA_REAL, |duration| duration.as_secs_f64());
            }
            Err(_) => {
                values::reals_mut(size)[index] = NA_REAL;
                values::ints_mut(is_dir)[index] = NA_INTEGER;
                values::ints_mut(mode)[index] = NA_INTEGER;
                values::reals_mut(mtime)[index] = NA_REAL;
            }
        }
    }
    let octmode = scalar_string("octmode")?;
    values::set_attr(mode, symbols().class, octmode)?;
    let times = string_vector(&["POSIXct", "POSIXt"])?;
    values::set_attr(mtime, symbols().class, times)?;
    values::named_list(&[
        ("size", size),
        ("isdir", is_dir),
        ("mode", mode),
        ("mtime", mtime),
        ("ctime", mtime),
        ("atime", mtime),
    ])
}

fn walk(
    directory: &Path,
    recursive: bool,
    include_dirs: bool,
    output: &mut Vec<PathBuf>,
    all: bool,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !all && name.starts_with('.') {
            continue;
        }
        if path.is_dir() && recursive {
            if include_dirs {
                output.push(path.clone());
            }
            walk(&path, recursive, include_dirs, output, all);
        } else {
            output.push(path);
        }
    }
}

unsafe fn do_list_files(call: &Call<'_>) -> R<Sexp> {
    let directories = call.arg(0);
    let pattern = values::first_string(call.arg(1)).map(|text| text.into_owned());
    let all = values::as_logical_scalar(call.arg(2)) == 1;
    let full = values::as_logical_scalar(call.arg(3)) == 1;
    let recursive = values::as_logical_scalar(call.arg(4)) == 1;
    let ignore_case = values::as_logical_scalar(call.arg(5)) == 1;
    let include_dirs = values::as_logical_scalar(call.arg(6)) == 1;
    let no_dots = values::as_logical_scalar(call.arg(7)) == 1;
    // R 4.5 added `fixed`, which matches the pattern as a literal string.
    let fixed = values::as_logical_scalar(call.arg(8)) == 1;
    let regex = match &pattern {
        Some(pattern) => Some(
            regex::RegexBuilder::new(&if fixed {
                regex::escape(pattern)
            } else {
                crate::strings::translate_regex(pattern, true)
            })
            .case_insensitive(ignore_case)
            .build()
            .map_err(|failure| error(format!("invalid 'pattern' regular expression: {failure}")))?,
        ),
        None => None,
    };
    let mut names = Vec::new();
    for index in 0..length(directories) {
        let Some(directory) = values::string_at(directories, index) else {
            continue;
        };
        let directory = crate::connections::expand_path(&directory);
        let mut paths = Vec::new();
        walk(
            Path::new(&directory),
            recursive,
            include_dirs,
            &mut paths,
            all,
        );
        if all && !recursive && !no_dots {
            names.push(if full {
                format!("{directory}/.")
            } else {
                ".".into()
            });
            names.push(if full {
                format!("{directory}/..")
            } else {
                "..".into()
            });
        }
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(regex) = &regex
                && !regex.is_match(&name)
            {
                continue;
            }
            let shown = if full {
                path.display().to_string()
            } else {
                path.strip_prefix(&directory)
                    .map(|relative| relative.display().to_string())
                    .unwrap_or(name)
            };
            names.push(shown);
        }
    }
    crate::sort::sort_strings(&mut names);
    string_vector(&names)
}

unsafe fn do_list_dirs(call: &Call<'_>) -> R<Sexp> {
    let directories = call.arg(0);
    let full = values::as_logical_scalar(call.arg(1)) == 1;
    let recursive = values::as_logical_scalar(call.arg(2)) == 1;
    let mut names = Vec::new();
    for index in 0..length(directories) {
        let Some(directory) = values::string_at(directories, index) else {
            continue;
        };
        let directory = crate::connections::expand_path(&directory);
        names.push(if full {
            directory.clone()
        } else {
            String::new()
        });
        let mut paths = Vec::new();
        walk(Path::new(&directory), recursive, true, &mut paths, true);
        for path in paths.into_iter().filter(|path| path.is_dir()) {
            names.push(if full {
                path.display().to_string()
            } else {
                path.strip_prefix(&directory)
                    .map(|relative| relative.display().to_string())
                    .unwrap_or_default()
            });
        }
    }
    crate::sort::sort_strings(&mut names);
    string_vector(&names)
}

unsafe fn do_normalize_path(call: &Call<'_>) -> R<Sexp> {
    let paths = call.arg(0);
    let must_work = values::as_logical_scalar(call.arg(2));
    // Only Windows has a second separator to choose; elsewhere `winslash` is ignored, as in GNU R.
    let separator = if cfg!(windows) {
        let slash = call.arg(1);
        if slash.kind() != SexpType::String || length(slash) != 1 {
            return Err(call.error("'winslash' must be a character string"));
        }
        match values::string_at(slash, 0).as_deref() {
            Some(text @ ("/" | "\\")) => text.to_owned(),
            _ => return Err(call.error("'winslash' must be '/' or '\\\\'")),
        }
    } else {
        String::new()
    };
    // GNU R reports the Windows system message for a path that does not exist.
    let missing = if cfg!(windows) {
        "The system cannot find the file specified"
    } else {
        "No such file or directory"
    };
    let count = length(paths);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let Some(path) = values::string_at(paths, index) else {
            values::set_elt(result, index, values::na_string());
            continue;
        };
        let expanded = crate::connections::expand_path(&path);
        let normalized = match std::fs::canonicalize(&expanded) {
            Ok(canonical) if cfg!(windows) => {
                crate::platform::path_text(&canonical).replace('\\', &separator)
            }
            Ok(canonical) => crate::platform::path_text(&canonical),
            Err(_) => {
                if must_work == 1 {
                    return Err(call.error(format!("path[{}]=\"{path}\": {missing}", index + 1)));
                }
                if must_work == NA_INTEGER {
                    crate::conditions::warning_call(
                        call.call,
                        format!("path[{}]=\"{path}\": {missing}", index + 1),
                    )?;
                }
                // Windows makes a path that does not exist absolute, as GetFullPathName does;
                // elsewhere it comes back as given, after tilde expansion.
                match std::path::absolute(&expanded) {
                    Ok(absolute) if cfg!(windows) => {
                        crate::platform::path_text(&absolute).replace('\\', &separator)
                    }
                    _ => expanded,
                }
            }
        };
        values::set_elt(result, index, values::mkchar(&normalized)?);
    }
    Ok(result)
}

unsafe fn do_file_path(call: &Call<'_>) -> R<Sexp> {
    let parts = call.arg(0);
    let separator = values::first_string(call.arg(1))
        .map(|text| text.into_owned())
        .unwrap_or_else(|| "/".into());
    let count = parts.length() as usize;
    let pieces: Vec<Sexp> = (0..count)
        .map(|index| crate::coerce::coerce_vector(parts.element(index), SexpType::String))
        .collect::<R<_>>()?;
    if pieces.iter().any(|piece| length(*piece) == 0) {
        return alloc(SexpType::String, 0);
    }
    let longest = pieces.iter().map(|piece| length(*piece)).max().unwrap_or(0);
    let mut paths = Vec::with_capacity(longest);
    for index in 0..longest {
        let parts: Vec<String> = pieces
            .iter()
            .map(|piece| {
                values::string_at(*piece, index % length(*piece))
                    .map_or_else(|| "NA".into(), |text| text.into_owned())
            })
            .collect();
        paths.push(parts.join(&separator));
    }
    string_vector(&paths)
}

unsafe fn do_tempfile(call: &Call<'_>) -> R<Sexp> {
    let patterns = call.arg(0);
    let directories = call.arg(1);
    let extensions = call.arg(2);
    let count = length(patterns)
        .max(length(directories))
        .max(length(extensions));
    let mut paths = Vec::new();
    static mut COUNTER: u64 = 0;
    for index in 0..count {
        let pattern = values::string_at(patterns, index % length(patterns).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let directory = values::string_at(directories, index % length(directories).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_else(temp_dir);
        let extension = values::string_at(extensions, index % length(extensions).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_default();
        COUNTER += 1;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos() as u64)
            ^ COUNTER.wrapping_mul(0x9e3779b97f4a7c15);
        paths.push(format!("{directory}/{pattern}{unique:x}{extension}"));
    }
    string_vector(&paths)
}

unsafe fn do_unlink(call: &Call<'_>) -> R<Sexp> {
    let paths = call.arg(0);
    let recursive = values::as_logical_scalar(call.arg(1)) == 1;
    let mut failed = 0;
    for index in 0..length(paths) {
        let Some(path) = values::string_at(paths, index) else {
            continue;
        };
        let path = crate::connections::expand_path(&path);
        let target = Path::new(&path);
        let outcome = if target.is_dir() {
            if recursive {
                std::fs::remove_dir_all(target)
            } else {
                std::fs::remove_dir(target)
            }
        } else if target.exists() {
            std::fs::remove_file(target)
        } else {
            Ok(())
        };
        if outcome.is_err() {
            failed = 1;
        }
    }
    scalar_integer(failed)
}

unsafe fn do_file_rename(call: &Call<'_>) -> R<Sexp> {
    let from = call.arg(0);
    let to = call.arg(1);
    let count = length(from);
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        let source = values::string_at(from, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let target = values::string_at(to, index)
            .map(|text| text.into_owned())
            .unwrap_or_default();
        values::ints_mut(result)[index] = std::fs::rename(source, target).is_ok() as i32;
    }
    Ok(result)
}

unsafe fn do_dir_create(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "path")?;
    let recursive = values::as_logical_scalar(call.arg(2)) == 1;
    let path = crate::connections::expand_path(&path);
    let outcome = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    scalar_bool(outcome.is_ok())
}

unsafe fn do_setwd(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "dir")?;
    let previous = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    std::env::set_current_dir(crate::connections::expand_path(&path))
        .map_err(|_| call.error("cannot change working directory"))?;
    scalar_string(&previous)
}

unsafe fn do_sys_info(_call: &Call<'_>) -> R<Sexp> {
    let name = crate::platform::system_name();
    let user = name.user;
    let values_list = [
        name.sysname,
        name.release,
        name.version,
        name.nodename,
        name.machine,
        user.clone(),
        user.clone(),
        user,
    ];
    let result = string_vector(&values_list)?;
    let names = string_vector(&[
        "sysname",
        "release",
        "version",
        "nodename",
        "machine",
        "login",
        "user",
        "effective_user",
    ])?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

unsafe fn do_l10n_info(_call: &Call<'_>) -> R<Sexp> {
    let name = crate::platform::native_codeset();
    let upper = name.to_ascii_uppercase();
    let is_utf8 = matches!(upper.as_str(), "UTF-8" | "UTF8");
    let utf8 = scalar_bool(is_utf8)?;
    let latin1 = scalar_bool(matches!(
        upper.as_str(),
        "ISO-8859-1" | "ISO8859-1" | "LATIN1"
    ))?;
    // UTF-8 is the only multibyte codeset ry supports as a native encoding.
    let mbcs = scalar_bool(is_utf8)?;
    // GNU R on Windows reports code pages instead of a codeset name, and `tools` reads
    // `codepage` while it loads.
    #[cfg(windows)]
    {
        let codepage = scalar_integer(65001)?;
        let system =
            scalar_integer(i32::try_from(crate::platform::system_codepage()).unwrap_or(0))?;
        values::named_list(&[
            ("MBCS", mbcs),
            ("UTF-8", utf8),
            ("Latin-1", latin1),
            ("codepage", codepage),
            ("system.codepage", system),
        ])
    }
    #[cfg(not(windows))]
    {
        let codeset = scalar_string(&name)?;
        values::named_list(&[
            ("MBCS", mbcs),
            ("UTF-8", utf8),
            ("Latin-1", latin1),
            ("codeset", codeset),
        ])
    }
}

unsafe fn do_version(_call: &Call<'_>) -> R<Sexp> {
    // GNU R's own platform strings, which packages match on (`grepl("darwin", R.version$os)`).
    // CRAN's macOS builds target Darwin 20, the first release with Apple silicon.
    let (vendor, os) = if cfg!(target_os = "macos") {
        ("apple", "darwin20")
    } else if cfg!(windows) {
        ("w64", "mingw32")
    } else {
        ("pc", "linux-gnu")
    };
    let arch = std::env::consts::ARCH;
    let platform = format!("{arch}-{vendor}-{os}");
    let version = r_version();
    let (year, month, day) = (
        version.date.get(0..4).unwrap_or(""),
        version.date.get(5..7).unwrap_or(""),
        version.date.get(8..10).unwrap_or(""),
    );
    let fields: Vec<(&str, String)> = vec![
        ("platform", platform.clone()),
        ("arch", arch.to_owned()),
        ("os", os.to_owned()),
        ("system", format!("{arch}, {os}")),
        ("status", String::new()),
        ("major", version.major.to_string()),
        ("minor", format!("{}.{}", version.minor, version.patch)),
        ("year", year.to_owned()),
        ("month", month.to_owned()),
        ("day", day.to_owned()),
        ("svn rev", version.revision.to_string()),
        ("language", "R".to_owned()),
        (
            "version.string",
            format!(
                "R version {}.{}.{} ({})",
                version.major, version.minor, version.patch, version.date
            ),
        ),
        ("nickname", version.nickname.clone()),
    ];
    let items: Vec<(&str, Sexp)> = fields
        .iter()
        .map(|(name, value)| Ok((*name, scalar_string(value)?)))
        .collect::<R<_>>()?;
    values::named_list(&items)
}

/// The version of the GNU R whose base packages the session loads, which is the version the
/// session presents (`R.version`, `getRversion()`, the `Built:` field it writes). Presenting
/// that version, rather than a fixed one, is what keeps base R's own version checks honest:
/// `library()` warns about a package built under a newer R by comparing its `Built:` field
/// with `R.version`, and the default packages were built by exactly the R in `R_HOME`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// The release date, `YYYY-MM-DD`.
    pub date: String,
    /// The Subversion revision, at least [`MINIMUM_SVN_REVISION`].
    pub revision: u32,
    pub nickname: String,
}

impl RVersion {
    /// GNU R's `R_Version(major, minor, patch)`, as `R_VERSION` and the serialization header
    /// encode a version.
    pub fn code(&self) -> i32 {
        ((self.major << 16) + (self.minor << 8) + self.patch) as i32
    }

    pub fn number(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Whether the emulated R is `major.minor` or later. Behavior that GNU R changed between
/// supported releases follows the release in `R_HOME`, so the base packages' R code and the
/// internals it calls always agree.
pub fn r_at_least(major: u32, minor: u32) -> bool {
    let version = r_version();
    (version.major, version.minor) >= (major, minor)
}

/// ry implements `R_mapAttrib` and the resizable vectors whatever base it runs, and data.table's
/// guard for them also checks the revision, so ry reports at least this one
/// (`rcmd::MINIMUM_SVN_REVISION` writes the same floor into `Rversion.h`).
pub const MINIMUM_SVN_REVISION: u32 = 89194;

/// The oldest GNU R whose base packages ry's runtime loads. Older releases call internals
/// with argument lists ry does not implement.
pub const OLDEST_SUPPORTED_R: (u32, u32) = (4, 3);

/// The version of the R in [`r_home`], read once.
pub fn r_version() -> &'static RVersion {
    static VERSION: std::sync::OnceLock<RVersion> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| read_r_version(&r_home()))
}

/// Reads the version from the installation itself: the base package's `DESCRIPTION` holds
/// the version that `library()`'s checks compare against, and `SVN-REVISION` in the GNU R home
/// that base really lives in holds the revision and release date. ry's own R home links
/// `library/base` from a GNU R home, so the link is resolved first. The session cannot start
/// without a base package ([`missing_r_home`]), so a missing one is a broken installation.
pub fn read_r_version(home: &str) -> RVersion {
    let base = Path::new(home).join("library/base");
    let path = base.join("DESCRIPTION");
    let description = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("cannot read the base package's {}: {error}", path.display())
    });
    let field = |text: &str, name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
    };
    let version = field(&description, "Version")
        .unwrap_or_else(|| panic!("{} has no Version field", path.display()));
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let gnu_home = std::fs::canonicalize(&base)
        .ok()
        .and_then(|base| Some(base.parent()?.parent()?.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from(home));
    let svn = std::fs::read_to_string(gnu_home.join("SVN-REVISION")).unwrap_or_default();
    let built_date = field(&description, "Built")
        .and_then(|built| built.split(';').nth(2).map(|date| date.trim().to_owned()))
        .and_then(|date| date.get(0..10).map(str::to_owned));
    let date = field(&svn, "Last Changed Date")
        .or(built_date)
        .unwrap_or_default();
    let revision = field(&svn, "Revision")
        .and_then(|revision| revision.parse().ok())
        .unwrap_or(0);
    // Debian keeps the headers outside R_HOME and names them in the `R_INCLUDE_DIR` that
    // `bin/R` exports.
    let shared_include = std::env::var_os("R_INCLUDE_DIR").map(PathBuf::from);
    let nickname = [gnu_home.join("include"), Path::new(home).join("include")]
        .into_iter()
        .chain(shared_include)
        .find_map(|include| {
            let header = std::fs::read_to_string(include.join("Rversion.h")).ok()?;
            header.lines().find_map(|line| {
                let rest = line.trim().strip_prefix("#define R_NICK")?;
                Some(rest.trim().trim_matches('"').to_owned())
            })
        })
        .unwrap_or_default();
    RVersion {
        major,
        minor,
        patch,
        date,
        revision: revision.max(MINIMUM_SVN_REVISION),
        nickname,
    }
}

unsafe fn do_capabilities(_call: &Call<'_>) -> R<Sexp> {
    let names = [
        "jpeg",
        "png",
        "tiff",
        "tcltk",
        "X11",
        "aqua",
        "http/ftp",
        "sockets",
        "libxml",
        "fifo",
        "cledit",
        "iconv",
        "NLS",
        "Rprof",
        "profmem",
        "cairo",
        "ICU",
        "long.double",
        "libcurl",
    ];
    let flags = [1, 1, 1, 0, 0, 0, 1, 1, 0, 1, 0, 1, 0, 0, 0, 1, 0, 1, 0];
    let result = values::logical_vector(&flags)?;
    let names = string_vector(&names)?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

unsafe fn do_gc(_call: &Call<'_>) -> R<Sexp> {
    crate::heap().collect(heap::Collection::Major);
    (interp().hooks.run_finalizers)();
    let stats = crate::heap().stats();
    let result = values::real_vector(&[
        stats.live_objects as f64,
        0.0,
        (stats.live_bytes as f64) / 1048576.0,
        0.0,
        NA_REAL,
        NA_REAL,
        NA_REAL,
        NA_REAL,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ])?;
    Ok(result)
}

/// `gctorture(on)`: with `on`, every allocation collects first, which turns a missing root into
/// a crash where it happens. Returns the previous setting.
unsafe fn do_gctorture(call: &Call<'_>) -> R<Sexp> {
    let previous = set_gc_stress(values::as_logical_scalar(call.arg(0)) == 1);
    scalar_bool(previous)
}

/// `gctorture2(step, wait, inhibit_release)`: any positive `step` turns the stress on, since the
/// heap collects either before every allocation or by its normal policy.
unsafe fn do_gctorture2(call: &Call<'_>) -> R<Sexp> {
    let step = values::as_integer_scalar(call.arg(0));
    let previous = set_gc_stress(step != NA_INTEGER && step > 0);
    scalar_integer(i32::from(previous))
}

unsafe fn set_gc_stress(on: bool) -> bool {
    let policy = crate::heap().policy_mut();
    let previous = matches!(policy.stress, GcStress::EveryAllocation);
    policy.stress = if on {
        GcStress::EveryAllocation
    } else {
        GcStress::Off
    };
    previous
}

unsafe fn do_reg_finalizer(call: &Call<'_>) -> R<Sexp> {
    let key = call.arg(0);
    let finalizer = call.arg(1);
    if !matches!(key.kind(), SexpType::Environment | SexpType::ExternalPtr) {
        return Err(call.error("first argument must be environment or external pointer"));
    }
    if !matches!(
        finalizer.kind(),
        SexpType::Closure | SexpType::Builtin | SexpType::Special
    ) {
        return Err(call.error("second argument must be a function"));
    }
    let on_exit = values::as_logical_scalar(call.arg(2));
    if on_exit == NA_LOGICAL {
        return Err(call.error("third argument must be 'TRUE' or 'FALSE'"));
    }
    crate::heap().alloc_weak_ref(key, nil(), finalizer, 0, on_exit != 0)?;
    Ok(nil())
}

unsafe fn do_quit(call: &Call<'_>) -> R<Sexp> {
    let status = values::as_integer_scalar(call.arg(1));
    crate::session::finish();
    use std::io::Write;
    drop(std::io::stdout().flush());
    std::process::exit(if status == NA_INTEGER { 0 } else { status });
}

unsafe fn do_system(call: &Call<'_>) -> R<Sexp> {
    use std::io::Write;
    let command = values::require_string(call.arg(0), "command")?;
    let intern = values::as_logical_scalar(call.arg(1)) == 1;
    let mut process = std::process::Command::new("sh");
    process.arg("-c").arg(&command);
    if intern {
        process.stderr(std::process::Stdio::inherit());
        let output = process
            .output()
            .map_err(|failure| call.error(format!("error in running command: {failure}")))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = text.lines().collect();
        let result = string_vector(&lines)?;
        let status = output.status.code().unwrap_or(127);
        if status != 0 {
            values::set_attr(result, values::install("status")?, scalar_integer(status)?)?;
            crate::conditions::warning_call(
                call.call,
                format!("running command '{command}' had status {status}"),
            )?;
        }
        return Ok(result);
    }
    // The child writes straight to the terminal, as GNU R's does, so earlier output must be out
    // first to keep the order.
    std::io::stdout()
        .flush()
        .map_err(|failure| call.error(format!("cannot flush stdout: {failure}")))?;
    let status = process
        .status()
        .map_err(|failure| call.error(format!("error in running command: {failure}")))?;
    interp().visible = false;
    scalar_integer(status.code().unwrap_or(127))
}

unsafe fn do_encoding(call: &Call<'_>) -> R<Sexp> {
    let strings = call.arg(0);
    let count = length(strings);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let charsxp = strings.element(index);
        let name = if charsxp.has_gp(heap::object::gp::UTF8) {
            "UTF-8"
        } else if charsxp.has_gp(heap::object::gp::LATIN1) {
            "latin1"
        } else if charsxp.has_gp(heap::object::gp::BYTES) {
            "bytes"
        } else {
            "unknown"
        };
        values::set_elt(result, index, values::mkchar(name)?);
    }
    Ok(result)
}

/// `Encoding<-`: re-declares each string's encoding without touching its bytes. ASCII strings
/// never carry a mark.
unsafe fn do_set_encoding(call: &Call<'_>) -> R<Sexp> {
    let strings = call.arg(0);
    let names = call.arg(1);
    if strings.kind() != SexpType::String {
        return Err(call.error("a character vector argument expected"));
    }
    if names.kind() != SexpType::String || length(names) == 0 {
        return Err(call.error("a character vector 'value' expected"));
    }
    let result = values::shallow_duplicate(strings)?;
    for index in 0..length(result) {
        let charsxp = result.element(index);
        if charsxp == values::na_string() {
            continue;
        }
        let name = values::string_at(names, index % length(names)).unwrap_or_default();
        let encoding = match &*name {
            "latin1" => Encoding::Latin1,
            "UTF-8" => Encoding::Utf8,
            "bytes" => Encoding::Bytes,
            _ => Encoding::Native,
        };
        values::set_elt(
            result,
            index,
            values::mkchar_bytes(charsxp.bytes(), encoding)?,
        );
    }
    Ok(result)
}

/// `iconv(x, from, to, sub, mark, toRaw)` through the C library's `iconv(3)` as GNU R does, so
/// the encoding names and conversions are the system's; Windows has ry's own conversion.
unsafe fn do_iconv(call: &Call<'_>) -> R<Sexp> {
    let input = call.arg(0);
    // `iconvlist()` asks for the converter's encoding names with a NULL `x`. glibc cannot list
    // them, so GNU R answers NULL there and `iconvlist()` reads utils' `iconvlist` file instead.
    if input == nil() {
        return Ok(nil());
    }
    let from = values::first_string(call.arg(1)).map_or_else(String::new, |name| name.into_owned());
    let to = values::first_string(call.arg(2)).map_or_else(String::new, |name| name.into_owned());
    let substitute = values::string_at(call.arg(3), 0).map(|text| text.into_owned());
    let mark = values::as_logical_scalar(call.arg(4)) == 1;
    let to_raw = values::as_logical_scalar(call.arg(5)) == 1;
    let from_list = input.kind() == SexpType::List;
    if !from_list && input.kind() != SexpType::String {
        return Err(call.error("'x' must be a character vector"));
    }
    // As in GNU R, "" names the codeset of the current LC_CTYPE, so text that a C locale
    // cannot represent converts to NA; packages such as utf8 probe the locale this way.
    let system_name = |name: &str| -> String {
        match name {
            "" => crate::platform::native_codeset(),
            other => other.to_string(),
        }
    };
    let (from_name, to_name) = (system_name(&from), system_name(&to));
    let from_utf8 = matches!(from_name.to_ascii_uppercase().as_str(), "UTF-8" | "UTF8");
    let mut converter = Converter::open(&from_name, &to_name)
        .ok_or_else(|| call.error(format!("unsupported conversion from '{from}' to '{to}'")))?;
    let target = to_name.to_ascii_uppercase();
    let encoding = if !mark {
        Encoding::Native
    } else if matches!(target.as_str(), "UTF-8" | "UTF8") {
        Encoding::Utf8
    } else if matches!(
        target.as_str(),
        "LATIN1" | "ISO-8859-1" | "ISO8859-1" | "ISO_8859-1"
    ) {
        Encoding::Latin1
    } else {
        Encoding::Native
    };
    let count = length(input);
    let result = alloc(
        if to_raw {
            SexpType::List
        } else {
            SexpType::String
        },
        count,
    )?;
    for index in 0..count {
        let element = input.element(index);
        let bytes: &[u8] = if from_list {
            if element.kind() != SexpType::Raw {
                if !to_raw {
                    values::set_elt(result, index, values::na_string());
                }
                continue;
            }
            values::raws(element)
        } else {
            if element == values::na_string() {
                if !to_raw {
                    values::set_elt(result, index, values::na_string());
                }
                continue;
            }
            element.bytes()
        };
        match converter.convert(bytes, substitute.as_deref(), from_utf8) {
            Some(converted) if to_raw => {
                let vector = alloc(SexpType::Raw, converted.len())?;
                values::raws_mut(vector).copy_from_slice(&converted);
                values::set_elt(result, index, vector);
            }
            Some(converted) => {
                values::set_elt(result, index, values::mkchar_bytes(&converted, encoding)?);
            }
            None if to_raw => {}
            None => values::set_elt(result, index, values::na_string()),
        }
    }
    // GNU R converts a duplicate of a character `x`, so names and other attributes survive.
    if !from_list && !to_raw {
        values::copy_attributes(input, result)?;
    }
    Ok(result)
}

/// A conversion for `iconv()`, with R's handling of the bytes it cannot convert.
struct Converter(Iconv);

impl Converter {
    fn open(from: &str, to: &str) -> Option<Converter> {
        Iconv::open(from, to).map(Converter)
    }

    /// `bytes` converted, or `None` when they hold a sequence the target cannot represent and
    /// there is no substitute. A substitute of "byte" writes `<xx>` for each offending byte,
    /// "Unicode" and "c99" write the code point of an offending UTF-8 character, and any other
    /// text replaces each offending byte.
    fn convert(
        &mut self,
        bytes: &[u8],
        substitute: Option<&str>,
        from_utf8: bool,
    ) -> Option<Vec<u8>> {
        let mut output = Vec::with_capacity(bytes.len() + 16);
        let mut rest = bytes;
        self.0.reset();
        while let Err(_stop) = convert_all(&mut self.0, &mut rest, &mut output) {
            let substitute = substitute?;
            let skipped = match substitute {
                "Unicode" | "c99" if from_utf8 => {
                    let length = utf8_sequence_length(rest);
                    match std::str::from_utf8(&rest[..length])
                        .ok()
                        .and_then(|text| text.chars().next())
                    {
                        Some(character) => {
                            let code = u32::from(character);
                            let text = match (substitute, code > 0xFFFF) {
                                ("Unicode", false) => format!("<U+{code:04X}>"),
                                ("Unicode", true) => format!("<U+{code:08X}>"),
                                (_, false) => format!("\\u{code:04x}"),
                                (_, true) => format!("\\U{code:08x}"),
                            };
                            output.extend_from_slice(text.as_bytes());
                            length
                        }
                        None => {
                            output.extend_from_slice(format!("<{:02x}>", rest[0]).as_bytes());
                            1
                        }
                    }
                }
                "byte" | "Unicode" | "c99" => {
                    output.extend_from_slice(format!("<{:02x}>", rest.first()?).as_bytes());
                    1
                }
                text => {
                    output.extend_from_slice(text.as_bytes());
                    1
                }
            };
            rest = rest.get(skipped..)?;
        }
        // Flush any shift sequence a stateful target needs at the end.
        let mut buffer = [0u8; 64];
        let (written, _) = self.0.finish(&mut buffer);
        output.extend_from_slice(&buffer[..written]);
        Some(output)
    }
}

/// How many bytes the UTF-8 sequence at the start of `bytes` claims, at least one.
fn utf8_sequence_length(bytes: &[u8]) -> usize {
    let length = match bytes.first() {
        Some(byte) if *byte >= 0xF0 => 4,
        Some(byte) if *byte >= 0xE0 => 3,
        Some(byte) if *byte >= 0xC0 => 2,
        _ => 1,
    };
    length.min(bytes.len()).max(1)
}

unsafe fn do_ngettext(call: &Call<'_>) -> R<Sexp> {
    let count = values::as_integer_scalar(call.arg(0));
    Ok(if count == 1 { call.arg(1) } else { call.arg(2) })
}

/// The CRC-64 (ECMA-182, reflected) of `bytes` in lower-case hexadecimal without leading
/// zeros, as `crc64()` reports it.
pub fn crc64_hex(bytes: &[u8]) -> String {
    format!("{:x}", crc64(bytes))
}

fn crc64(bytes: &[u8]) -> u64 {
    let mut crc: u64 = !0;
    for &byte in bytes {
        crc ^= byte as u64;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xC96C5795D7870F42
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `.Machine`, as GNU R computes it on a 64-bit IEEE platform.
pub unsafe fn machine() -> R<Sexp> {
    let double = |value: f64| scalar_real(value);
    let integer = |value: i32| scalar_integer(value);
    values::named_list(&[
        ("double.eps", double(f64::EPSILON)?),
        ("double.neg.eps", double(f64::EPSILON / 2.0)?),
        ("double.xmin", double(f64::MIN_POSITIVE)?),
        ("double.xmax", double(f64::MAX)?),
        ("double.base", integer(2)?),
        ("double.digits", integer(53)?),
        ("double.rounding", integer(5)?),
        ("double.guard", integer(0)?),
        ("double.ulp.digits", integer(-52)?),
        ("double.neg.ulp.digits", integer(-53)?),
        ("double.exponent", integer(11)?),
        ("double.min.exp", integer(-1022)?),
        ("double.max.exp", integer(1024)?),
        ("integer.max", integer(i32::MAX)?),
        ("sizeof.long", integer(8)?),
        ("sizeof.longlong", integer(8)?),
        ("sizeof.longdouble", integer(16)?),
        ("sizeof.pointer", integer(8)?),
        ("sizeof.time_t", integer(8)?),
        ("longdouble.eps", double(1.0842021724855044e-19)?),
        ("longdouble.neg.eps", double(5.421010862427522e-20)?),
        ("longdouble.digits", integer(64)?),
        ("longdouble.rounding", integer(5)?),
        ("longdouble.guard", integer(0)?),
        ("longdouble.ulp.digits", integer(-63)?),
        ("longdouble.neg.ulp.digits", integer(-64)?),
        ("longdouble.exponent", integer(15)?),
        ("longdouble.min.exp", integer(-16382)?),
        ("longdouble.max.exp", integer(16384)?),
    ])
}

/// `.Platform`.
pub unsafe fn platform() -> R<Sexp> {
    let string = |text: &str| scalar_string(text);
    values::named_list(&[
        (
            "OS.type",
            string(if cfg!(windows) { "windows" } else { "unix" })?,
        ),
        ("file.sep", string("/")?),
        (
            "dynlib.ext",
            string(if cfg!(windows) { ".dll" } else { ".so" })?,
        ),
        ("GUI", string(if cfg!(windows) { "RTerm" } else { "X11" })?),
        ("endian", string("little")?),
        (
            "pkgType",
            string(if cfg!(windows) {
                "win.binary"
            } else {
                "source"
            })?,
        ),
        ("path.sep", string(if cfg!(windows) { ";" } else { ":" })?),
        // GNU R for Windows keeps its 64-bit build under `bin/x64` and package DLLs under
        // `libs/x64`, and `library.dynam` finds them through this.
        ("r_arch", string(if cfg!(windows) { "x64" } else { "" })?),
    ])
}

unsafe fn do_esoft_version(_: &Call<'_>) -> R<Sexp> {
    let entries = [
        ("zlib", "1.3"),
        ("bzlib", ""),
        ("xz", ""),
        ("PCRE", "10.42 2022-12-11"),
        ("ICU", ""),
        ("TRE", ""),
        ("iconv", "glibc"),
        ("readline", ""),
        ("BLAS", ""),
    ];
    let result = string_vector(
        &entries
            .iter()
            .map(|(_, version)| *version)
            .collect::<Vec<_>>(),
    )?;
    let names = string_vector(&entries.iter().map(|(name, _)| *name).collect::<Vec<_>>())?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

unsafe fn do_read_renviron(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "x")?;
    let path = crate::connections::expand_path(&path);
    let Ok(text) = std::fs::read_to_string(&path) else {
        crate::conditions::warning_call(
            call.call,
            format!("file '{path}' cannot be opened for reading"),
        )?;
        return scalar_bool(false);
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches(|quote| quote == '"' || quote == '\'');
        let value = expand_renviron_value(value);
        // SAFETY: the interpreter is single-threaded.
        std::env::set_var(name.trim(), value);
    }
    scalar_bool(true)
}

/// Expands `${NAME}` and `${NAME-default}` references, the forms `.Renviron` files use.
fn expand_renviron_value(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            result.push_str(&rest[start..]);
            return result;
        };
        let reference = &rest[start + 2..start + end];
        let (name, default) = reference.split_once('-').unwrap_or((reference, ""));
        match std::env::var(name) {
            Ok(found) if !found.is_empty() => result.push_str(&found),
            _ => result.push_str(&expand_renviron_value(default)),
        }
        rest = &rest[start + end + 1..];
    }
    result.push_str(rest);
    result
}

unsafe fn link_paths(call: &Call<'_>, function: impl Fn(&str, &str) -> bool) -> R<Sexp> {
    let from = call.arg(0);
    let to = call.arg(1);
    let count = length(from).max(length(to));
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        let source = values::string_at(from, index % length(from).max(1));
        let target = values::string_at(to, index % length(to).max(1));
        values::ints_mut(result)[index] = match (source, target) {
            (Some(source), Some(target)) => function(&source, &target) as i32,
            _ => 0,
        };
    }
    Ok(result)
}

/// `Sys.glob` calls the C library's `glob`, as GNU R does, so wildcards work in every path
/// component and each pattern's matches stay in pattern order, sorted only among themselves.
unsafe fn do_sys_glob(call: &Call<'_>) -> R<Sexp> {
    let patterns = call.arg(0);
    if patterns.kind() != SexpType::String {
        return Err(error("invalid 'paths' argument"));
    }
    let dirmark = values::as_logical_scalar(call.arg(1));
    if dirmark == NA_LOGICAL {
        return Err(error("invalid 'dirmark' argument"));
    }
    let expanded: Vec<String> = (0..length(patterns))
        .filter_map(|index| values::string_at(patterns, index))
        .map(|pattern| crate::connections::expand_path(&pattern))
        .collect();
    let matches = crate::platform::glob(&expanded, dirmark != 0)
        .map_err(|()| error("internal out-of-memory condition"))?;
    if matches.read_error {
        crate::conditions::warning("read error on directory")?;
    }
    string_vector(&matches.paths)
}

unsafe fn do_sys_setlocale(call: &Call<'_>) -> R<Sexp> {
    let number = values::as_integer_scalar(call.arg(0));
    let category = locale_category(number).ok_or_else(|| error("invalid 'category' argument"))?;
    let locale = call.arg(1);
    let Some(name) = (locale.kind() == SexpType::String && length(locale) == 1)
        .then(|| values::string_at(locale, 0))
        .flatten()
    else {
        return Err(error("invalid 'locale' argument"));
    };
    let Ok(text) = std::ffi::CString::new(name.as_bytes()) else {
        return Err(error("invalid 'locale' argument"));
    };
    let honoured = if category == libc::LC_ALL {
        // As in GNU R, LC_ALL sets the categories R uses but leaves LC_NUMERIC at C.
        let honoured = !set_locale(libc::LC_CTYPE, text.as_ptr()).is_null();
        if honoured {
            for category in [libc::LC_COLLATE, libc::LC_MONETARY, libc::LC_TIME] {
                set_locale(category, text.as_ptr());
            }
        }
        honoured
    } else {
        if category == libc::LC_NUMERIC && name != "C" {
            crate::conditions::warning("setting 'LC_NUMERIC' may cause R to function strangely")?;
        }
        !set_locale(category, text.as_ptr()).is_null()
    };
    crate::sort::refresh_collation();
    if !honoured {
        crate::conditions::warning(format!(
            "OS reports request to set locale to \"{name}\" cannot be honored"
        ))?;
        return scalar_string("");
    }
    scalar_string(&current_locale(category))
}

unsafe fn do_sys_getlocale(call: &Call<'_>) -> R<Sexp> {
    let category = locale_category(values::as_integer_scalar(call.arg(0)))
        .ok_or_else(|| error("invalid 'category' argument"))?;
    scalar_string(&current_locale(category))
}

/// Takes the locale of the categories GNU R uses from the environment, as it does at startup;
/// numbers and the rest stay in the C locale.
pub unsafe fn init_locale() {
    for category in [
        libc::LC_CTYPE,
        libc::LC_COLLATE,
        libc::LC_TIME,
        libc::LC_MONETARY,
        LC_MESSAGES,
        LC_PAPER,
        LC_MEASUREMENT,
    ] {
        set_locale(category, c"".as_ptr());
        #[cfg(windows)]
        use_utf8_codeset(category);
    }
    crate::sort::refresh_collation();
}

/// Moves a Windows locale category to the UTF-8 codeset of the same language and region
/// (`English_United States.1252` becomes `English_United States.utf8`). ry's native encoding is
/// UTF-8 on Windows, as GNU R's is from 4.2 on, and `ry.exe` asks Windows for the UTF-8 code page
/// in its manifest. A system that ignores the manifest (older Windows, Wine) still starts the C
/// runtime in the ANSI code page, which would convert native strings as Latin-1 and report a
/// locale that contradicts `l10n_info()`.
#[cfg(windows)]
unsafe fn use_utf8_codeset(category: libc::c_int) {
    let current = current_locale(category);
    let Some((language, codeset)) = current.rsplit_once('.') else {
        return;
    };
    if codeset.eq_ignore_ascii_case("utf8") || codeset.eq_ignore_ascii_case("utf-8") {
        return;
    }
    if let Ok(name) = std::ffi::CString::new(format!("{language}.UTF-8")) {
        set_locale(category, name.as_ptr());
    }
}

/// macOS's C library has no paper or measurement category, and Windows's has no messages
/// category either. These read as `""` and cannot be set, as in GNU R there.
#[cfg(unix)]
const LC_MESSAGES: libc::c_int = libc::LC_MESSAGES;
#[cfg(windows)]
const LC_MESSAGES: libc::c_int = -1;
#[cfg(target_os = "linux")]
const LC_PAPER: libc::c_int = libc::LC_PAPER;
#[cfg(target_os = "linux")]
const LC_MEASUREMENT: libc::c_int = libc::LC_MEASUREMENT;
#[cfg(not(target_os = "linux"))]
const LC_PAPER: libc::c_int = -1;
#[cfg(not(target_os = "linux"))]
const LC_MEASUREMENT: libc::c_int = -1;

/// The C library category for `Sys.setlocale`'s category number, which indexes
/// `c("LC_ALL", "LC_COLLATE", "LC_CTYPE", "LC_MONETARY", "LC_NUMERIC", "LC_TIME",
/// "LC_MESSAGES", "LC_PAPER", "LC_MEASUREMENT")`.
fn locale_category(number: i32) -> Option<libc::c_int> {
    Some(match number {
        1 => libc::LC_ALL,
        2 => libc::LC_COLLATE,
        3 => libc::LC_CTYPE,
        4 => libc::LC_MONETARY,
        5 => libc::LC_NUMERIC,
        6 => libc::LC_TIME,
        7 => LC_MESSAGES,
        8 => LC_PAPER,
        9 => LC_MEASUREMENT,
        _ => return None,
    })
}

/// `setlocale`, except that a category the platform lacks fails without calling it: Windows's
/// C runtime treats an unknown category as a fatal invalid parameter.
unsafe fn set_locale(category: libc::c_int, name: *const libc::c_char) -> *mut libc::c_char {
    if category < 0 {
        std::ptr::null_mut()
    } else {
        libc::setlocale(category, name)
    }
}

unsafe fn current_locale(category: libc::c_int) -> String {
    let current = set_locale(category, std::ptr::null());
    if current.is_null() {
        return String::new();
    }
    let name = std::ffi::CStr::from_ptr(current)
        .to_string_lossy()
        .into_owned();
    // Some C runtimes (Wine's) name the UTF-8 codeset by its code page number; Microsoft's
    // reports `.utf8`, which code testing the locale name for UTF-8 expects and which `setlocale`
    // accepts back.
    // The composite `LC_ALL` name joins every category as `LC_COLLATE=...;LC_CTYPE=...`.
    #[cfg(windows)]
    if name.contains(".65001") {
        return name
            .split(';')
            .map(|part| match part.strip_suffix(".65001") {
                Some(language) => format!("{language}.utf8"),
                None => part.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(";");
    }
    name
}

unsafe fn do_file_access(call: &Call<'_>) -> R<Sexp> {
    let paths = call.arg(0);
    let mode = values::as_integer_scalar(call.arg(1));
    let result = alloc(SexpType::Integer, length(paths))?;
    for index in 0..length(paths) {
        let accessible = values::string_at(paths, index)
            .and_then(|path| std::ffi::CString::new(crate::connections::expand_path(&path)).ok())
            .is_some_and(|path| libc::access(path.as_ptr(), mode & 7) == 0);
        values::ints_mut(result)[index] = if accessible { 0 } else { -1 };
    }
    Ok(result)
}

/// `Sys.umask(NA)` reports the mask visibly; setting a new one returns the old one invisibly.
unsafe fn do_sys_umask(call: &Call<'_>) -> R<Sexp> {
    let mode = values::as_integer_scalar(call.arg(0));
    let old = if mode == NA_INTEGER {
        crate::platform::umask()
    } else {
        interp().visible = false;
        crate::platform::set_umask(mode as u32)
    };
    let result = scalar_integer(old as i32)?;
    let octmode = scalar_string("octmode")?;
    values::set_attr(result, symbols().class, octmode)?;
    Ok(result)
}

unsafe fn do_sys_chmod(call: &Call<'_>) -> R<Sexp> {
    let (paths, modes) = (
        call.arg(0),
        crate::coerce::coerce_vector(call.arg(1), SexpType::Integer)?,
    );
    let use_umask = values::as_logical_scalar(call.arg(2)) == 1;
    let result = alloc(SexpType::Logical, length(paths))?;
    for index in 0..length(paths) {
        let mode = match length(modes) {
            0 => 0o777,
            count => values::ints(modes)
                .get(index % count)
                .copied()
                .unwrap_or(0o777) as u32,
        };
        let mode = if use_umask {
            mode & !crate::platform::umask()
        } else {
            mode
        };
        let changed = values::string_at(paths, index).is_some_and(|path| {
            crate::platform::set_file_mode(Path::new(&crate::connections::expand_path(&path)), mode)
                .is_ok()
        });
        values::ints_mut(result)[index] = changed as i32;
    }
    Ok(result)
}

unsafe fn do_sys_set_file_time(call: &Call<'_>) -> R<Sexp> {
    let (paths, times) = (
        call.arg(0),
        crate::coerce::coerce_vector(call.arg(1), SexpType::Real)?,
    );
    let result = alloc(SexpType::Logical, length(paths))?;
    for index in 0..length(paths) {
        let time = match length(times) {
            0 => None,
            count => values::reals(times)
                .get(index % count)
                .copied()
                .filter(|time| time.is_finite()),
        };
        let changed = match (values::string_at(paths, index), time) {
            (Some(path), Some(time)) => {
                set_modified_time(&crate::connections::expand_path(&path), time).is_ok()
            }
            _ => false,
        };
        values::ints_mut(result)[index] = changed as i32;
    }
    Ok(result)
}

fn set_modified_time(path: &str, seconds: f64) -> std::io::Result<()> {
    let out_of_range =
        || std::io::Error::new(std::io::ErrorKind::InvalidInput, "time out of range");
    let offset = Duration::try_from_secs_f64(seconds.abs()).map_err(|_| out_of_range())?;
    let time = if seconds >= 0.0 {
        std::time::UNIX_EPOCH.checked_add(offset)
    } else {
        std::time::UNIX_EPOCH.checked_sub(offset)
    }
    .ok_or_else(out_of_range)?;
    crate::platform::set_file_times(Path::new(path), time)
}

unsafe fn do_valid_utf8(call: &Call<'_>) -> R<Sexp> {
    let strings = call.arg(0);
    if strings.kind() != SexpType::String {
        return Err(call.error("invalid 'x' argument"));
    }
    let result = alloc(SexpType::Logical, length(strings))?;
    for index in 0..length(strings) {
        let charsxp = strings.element(index);
        let valid = charsxp == values::na_string() || std::str::from_utf8(charsxp.bytes()).is_ok();
        values::ints_mut(result)[index] = valid as i32;
    }
    Ok(result)
}
