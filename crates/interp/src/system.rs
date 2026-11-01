//! The operating system as R sees it: environment variables, files and directories, time and
//! dates, the process, and the session constants (`.Machine`, `.Platform`, `R.version`).

use crate::{
    builtins::{BuiltinDef, Call, builtin, internal, special},
    interp,
    unwind::{R, error},
    values::{
        self, alloc, get_attr, length, nil, scalar_bool, scalar_integer, scalar_real, scalar_string,
        string_vector, symbols,
    },
};
use heap::{
    GcStress,
    object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Sexp, SexpType, is_na_real},
    strings::Encoding,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("internalsID", 0, |_| unsafe { scalar_string(INTERNALS_ID) }),
    internal("eSoftVersion", 0, do_esoft_version),
    internal("pcre_config", 0, |_| unsafe {
        let result = values::logical_vector(&[1, 1, 1, 1])?;
        let names = string_vector(&["UTF-8", "Unicode properties", "JIT", "stack"])?;
        values::set_attr(result, symbols().names, names)?;
        Ok(result)
    }),
    internal("icuSetCollate", -1, |_| unsafe { Ok(nil()) }).invisible(),
    internal("icuGetCollate", 1, |_| unsafe { scalar_string("ICU not in use") }),
    internal("setNumMathThreads", 1, |_| unsafe { scalar_integer(1) }),
    internal("setMaxNumMathThreads", 1, |_| unsafe { scalar_integer(1) }),
    internal("isatty", 1, |call| unsafe {
        let descriptor = values::as_integer_scalar(call.arg(0));
        scalar_bool(descriptor >= 0 && libc::isatty(descriptor) == 1)
    }),
    internal("readRenviron", 1, do_read_renviron).invisible(),
    internal("debug", 3, |_| unsafe { Ok(nil()) }).invisible(),
    internal("undebug", 1, |_| unsafe { Ok(nil()) }).invisible(),
    internal("debugonce", 3, |_| unsafe { Ok(nil()) }).invisible(),
    internal("isdebugged", 1, |_| unsafe { scalar_bool(false) }),
    internal("debugOnOff", 1, |_| unsafe { scalar_bool(false) }),
    internal("traceOnOff", 1, |_| unsafe { scalar_bool(false) }),
    internal("browserText", 1, |call| unsafe { Err(call.error("no browser context to query")) }),
    internal("browserCondition", 1, |call| unsafe { Err(call.error("no browser context to query")) }),
    internal("browserSetDebug", 1, |_| unsafe { Ok(nil()) }).invisible(),
    internal("memory.profile", 0, |_| unsafe { values::integer_vector(&[]) }),
    internal("file.symlink", 2, |call| unsafe { link_paths(call, |from, to| std::os::unix::fs::symlink(from, to).is_ok()) }),
    internal("file.link", 2, |call| unsafe { link_paths(call, |from, to| std::fs::hard_link(from, to).is_ok()) }),
    internal("file.show", 5, |call| unsafe {
        for index in 0..length(call.arg(0)) {
            if let Some(path) = values::string_at(call.arg(0), index) {
                let text = std::fs::read_to_string(&*path).map_err(|failure| call.error(format!("cannot open file '{path}': {failure}")))?;
                crate::write_console(text.as_bytes(), false);
            }
        }
        Ok(nil())
    }).invisible(),
    internal("file.choose", 1, |call| unsafe { Err(call.error("file.choose() needs an interactive session")) }),
    internal("curlVersion", 0, |_| unsafe { scalar_string("") }),
    internal("curlGetHeaders", 5, |call| unsafe { Err(call.error("libcurl is not available in this session")) }),
    internal("curlDownload", 6, |call| unsafe { Err(call.error("libcurl is not available in this session")) }),
    internal("machine", 0, |_| unsafe { scalar_string("Unix") }),
    internal("R.home", 0, |_| unsafe { scalar_string(&r_home()) }),
    internal("Sys.getenv", 2, do_sys_getenv),
    internal("Sys.setenv", 2, do_sys_setenv).invisible(),
    internal("Sys.unsetenv", 1, do_sys_unsetenv).invisible(),
    internal("commandArgs", 0, |_| unsafe { string_vector(&interp().command_args) }),
    builtin("proc.time", 0, do_proc_time),
    internal("Sys.time", 0, do_sys_time),
    internal("date", 0, |_| unsafe { scalar_string(&format_time(now_seconds(), "%a %b %d %H:%M:%S %Y")) }),
    internal("file.exists", 1, |call| unsafe { map_paths(call, |path| Path::new(path).exists()) }),
    internal("dir.exists", 1, |call| unsafe { map_paths(call, |path| Path::new(path).is_dir()) }),
    internal("file.info", 2, do_file_info),
    internal("list.files", 8, do_list_files),
    internal("list.dirs", 3, do_list_dirs),
    internal("normalizePath", 3, do_normalize_path),
    internal("path.expand", 1, |call| unsafe { map_strings(call, crate::connections::expand_path) }),
    internal("basename", 1, |call| unsafe { map_strings(call, |path| {
        let trimmed = path.trim_end_matches('/');
        trimmed.rsplit('/').next().unwrap_or("").to_owned()
    }) }),
    internal("dirname", 1, |call| unsafe { map_strings(call, |path| {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(0) => "/".to_owned(),
            Some(position) => trimmed[..position].to_owned(),
            None => ".".to_owned(),
        }
    }) }),
    internal("file.path", 2, do_file_path),
    internal("tempfile", 3, do_tempfile),
    internal("tempdir", 1, |_| unsafe { scalar_string(&temp_dir()) }),
    internal("unlink", 4, do_unlink).invisible(),
    internal("file.remove", 1, |call| unsafe { map_paths(call, |path| std::fs::remove_file(path).is_ok()) }),
    internal("file.rename", 2, do_file_rename),
    internal("file.create", 2, |call| unsafe { map_paths(call, |path| std::fs::write(path, b"").is_ok()) }),
    internal("file.append", 2, do_file_append),
    internal("file.copy", 6, do_file_copy),
    internal("dir.create", 4, do_dir_create).invisible(),
    internal("file.access", 2, do_file_access),
    internal("getwd", 0, |_| unsafe { scalar_string(&std::env::current_dir().map(|path| path.display().to_string()).unwrap_or_default()) }),
    internal("setwd", 1, do_setwd).invisible(),
    internal("Sys.glob", 2, do_sys_glob),
    internal("Sys.readlink", 1, |call| unsafe { map_strings(call, |path| std::fs::read_link(path).map(|target| target.display().to_string()).unwrap_or_default()) }),
    internal("Sys.chmod", 3, do_sys_chmod).invisible(),
    internal("Sys.umask", 1, |_| unsafe { scalar_integer(18) }),
    internal("Sys.getpid", 0, |_| unsafe { scalar_integer(std::process::id() as i32) }),
    internal("Sys.info", 0, do_sys_info),
    internal("Sys.setlocale", 2, |_| unsafe { scalar_string("C.UTF-8") }),
    internal("Sys.getlocale", 1, |_| unsafe { scalar_string("LC_CTYPE=C.UTF-8;LC_NUMERIC=C;LC_TIME=C.UTF-8;LC_COLLATE=C.UTF-8;LC_MONETARY=C.UTF-8;LC_MESSAGES=C.UTF-8;LC_PAPER=C.UTF-8;LC_NAME=C;LC_ADDRESS=C;LC_TELEPHONE=C;LC_MEASUREMENT=C.UTF-8;LC_IDENTIFICATION=C") }),
    internal("l10n_info", 0, do_l10n_info),
    internal("Sys.localeconv", 0, |_| unsafe { values::named_list(&[]) }),
    internal("Sys.sleep", 1, |call| unsafe { {
        let seconds = values::as_real_scalar(call.arg(0));
        if seconds.is_finite() && seconds > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
        }
        Ok(nil())
    } }).invisible(),
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
    internal("system", 3, do_system),
    internal("Cstack_info", 0, |_| unsafe { values::integer_vector(&[7969177, 0, 1, NA_INTEGER]) }),
    internal("setTimeLimit", 3, |_| unsafe { Ok(nil()) }).invisible(),
    internal("setSessionTimeLimit", 2, |_| unsafe { Ok(nil()) }).invisible(),
    internal("reg.finalizer", 3, do_reg_finalizer),
    internal("traceback", 1, |call| unsafe {
        crate::conditions::traceback_calls(values::as_integer_scalar(call.arg(0)).max(0) as usize)
    }),
    internal(".traceback", 1, |_| unsafe { Ok(nil()) }),
    internal("setFileTime", 2, do_sys_set_file_time).invisible(),
    internal("Encoding", 1, do_encoding),
    internal("setEncoding", 2, do_set_encoding),
    builtin("enc2native", 1, |call| unsafe { Ok(call.arg(0)) }),
    builtin("enc2utf8", 1, |call| unsafe { Ok(call.arg(0)) }),
    internal("iconv", 6, do_iconv),
    internal("validUTF8", 1, do_valid_utf8),
    internal("validEnc", 1, |call| unsafe { map_strings_bool(call, |_| true) }),
    internal("as.POSIXlt", 2, do_as_posixlt),
    internal("as.POSIXct", 2, do_as_posixct),
    internal("format.POSIXlt", 3, do_format_posixlt),
    internal("strptime", 3, do_strptime),
    internal("Date2POSIXlt", 2, do_date_to_posixlt),
    internal("POSIXlt2Date", 1, do_posixlt_to_date),
    internal("balancePOSIXlt", 3, |call| unsafe {
        balance_posixlt(call.arg(0), values::is_true(call.arg(1)), values::is_true(call.arg(2)))
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
    special("browser", -1, |_| unsafe { Ok(nil()) }).invisible(),
    builtin("tracemem", 1, |call| unsafe {
        scalar_string(&format!("<{:p}>", call.arg(0).as_ptr()))
    }),
    builtin("untracemem", 1, |_| unsafe { Ok(nil()) }).invisible(),
    builtin("retracemem", -1, |_| unsafe { Ok(nil()) }).invisible(),
    builtin(".primTrace", 1, |call| unsafe { Ok(call.arg(0)) }).invisible(),
    builtin(".primUntrace", 1, |call| unsafe { Ok(call.arg(0)) }).invisible(),
    internal("format.Date", 2, |call| unsafe { Err(call.error("format.Date is not supported")) }),
    internal("tzone_name", 0, |_| unsafe { scalar_string("UTC") }),
    internal("bindtextdomain", 2, |_| unsafe { Ok(nil()) }),
    internal("gettext", 3, |call| unsafe { Ok(call.arg(1)) }),
    internal("ngettext", 4, do_ngettext),
    internal("enc2native", 1, |call| unsafe { Ok(call.arg(0)) }),
    internal("crc64", 1, |call| unsafe { scalar_string(&crc64_hex(values::first_string(call.arg(0)).unwrap_or_default().as_bytes())) }),
    internal("mkjunction", 2, |_| unsafe { scalar_bool(false) }),
    internal("pkgload", -1, |_| unsafe { Ok(nil()) }),
];

/// GNU R's identifier for its internals interface, which `loadNamespace` compares against the one a
/// package with compiled code recorded when it was installed. Matching it lets packages installed by
/// GNU R 4.3 load unchanged.
const INTERNALS_ID: &str = "2fdf6c18-697a-4ba7-b8ef-11c0d92f1327";

/// The R installation whose base and recommended packages the session loads.
pub fn r_home() -> String {
    static mut HOME: Option<String> = None;
    // SAFETY: single-threaded access, as for the rest of the interpreter.
    #[allow(static_mut_refs)]
    unsafe {
        HOME.get_or_insert_with(find_r_home).clone()
    }
}

fn find_r_home() -> String {
    if let Ok(home) = std::env::var("R_HOME")
        && Path::new(&home).join("library/base").exists()
    {
        return home;
    }
    for candidate in ["/usr/lib/R", "/usr/local/lib/R", "/opt/R/lib/R", "/usr/lib64/R"] {
        if Path::new(candidate).join("library/base").exists() {
            return candidate.to_owned();
        }
    }
    if let Ok(output) = std::process::Command::new("R").arg("RHOME").output()
        && output.status.success()
    {
        return String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    "/usr/lib/R".to_owned()
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
        values::ints_mut(result)[index] = values::string_at(strings, index).is_some_and(|text| function(&text)) as i32;
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
    let count = if length(sources) == 0 { 0 } else { length(targets).max(length(sources)) };
    let result = alloc(SexpType::Logical, count)?;
    for index in 0..count {
        let target = values::string_at(targets, index % length(targets));
        let source = values::string_at(sources, index % length(sources));
        let appended = match (target, source) {
            (Some(target), Some(source)) => append_file(&crate::connections::expand_path(&target), &crate::connections::expand_path(&source)).is_ok(),
            _ => false,
        };
        values::ints_mut(result)[index] = appended as i32;
    }
    Ok(result)
}

/// `.Internal(file.copy(from, to, ...))`, which the R closure calls only when `to` is one existing
/// directory: each source is copied into it under its own base name.
unsafe fn do_file_copy(call: &Call<'_>) -> R<Sexp> {
    let sources = call.arg(0);
    let directory = values::require_string(call.arg(1), "to")?;
    let overwrite = values::as_logical_scalar(call.arg(2)) == 1;
    let recursive = values::as_logical_scalar(call.arg(3)) == 1;
    let options = CopyOptions {
        overwrite,
        mode: values::as_logical_scalar(call.arg(4)) == 1,
        date: values::as_logical_scalar(call.arg(5)) == 1,
    };
    let directory = PathBuf::from(crate::connections::expand_path(&directory));
    let result = alloc(SexpType::Logical, length(sources))?;
    for index in 0..length(sources) {
        let copied = match values::string_at(sources, index) {
            Some(source) => {
                let source = PathBuf::from(crate::connections::expand_path(&source));
                match source.file_name() {
                    Some(name) if recursive || !source.is_dir() => {
                        let target = directory.join(name);
                        match copy_entry(&source, &target, &options) {
                            Ok(()) => true,
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                            Err(error) => {
                                let reason = match error.raw_os_error() {
                                    Some(code) => std::ffi::CStr::from_ptr(libc::strerror(code)).to_string_lossy().into_owned(),
                                    None => error.to_string(),
                                };
                                crate::conditions::warning(format!(
                                    "problem copying {} to {}: {reason}",
                                    source.display(),
                                    target.display()
                                ))?;
                                false
                            }
                        }
                    }
                    _ => false,
                }
            }
            None => false,
        };
        values::ints_mut(result)[index] = copied as i32;
    }
    Ok(result)
}

struct CopyOptions {
    overwrite: bool,
    mode: bool,
    date: bool,
}

/// Copies a file, or a directory with everything below it. Like GNU R, an existing target file is
/// an error unless `overwrite` is set, while an existing target directory is merged into.
fn copy_entry(source: &Path, target: &Path, options: &CopyOptions) -> std::io::Result<()> {
    let metadata = std::fs::metadata(source)?;
    if metadata.is_dir() {
        if !target.is_dir() {
            std::fs::create_dir(target)?;
        }
        let mut failure = None;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            if let Err(error) = copy_entry(&entry.path(), &target.join(entry.file_name()), options) {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
    } else {
        if target.exists() && !options.overwrite {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        std::fs::copy(source, target)?;
        if !options.mode {
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o666 & !UMASK))?;
        }
    }
    if options.mode && metadata.is_dir() {
        std::fs::set_permissions(target, metadata.permissions())?;
    }
    if options.date {
        let file = std::fs::File::options().write(!metadata.is_dir()).read(metadata.is_dir()).open(target)?;
        file.set_modified(metadata.modified()?)?;
    }
    Ok(())
}

/// The umask `Sys.umask()` reports, applied wherever GNU R applies the process umask.
const UMASK: u32 = 0o022;

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

unsafe fn do_sys_chmod(call: &Call<'_>) -> R<Sexp> {
    let (paths, modes) = (call.arg(0), crate::coerce::coerce_vector(call.arg(1), SexpType::Integer)?);
    let use_umask = values::as_logical_scalar(call.arg(2)) == 1;
    let result = alloc(SexpType::Logical, length(paths))?;
    for index in 0..length(paths) {
        let mode = match length(modes) {
            0 => 0o777,
            count => values::ints(modes).get(index % count).copied().unwrap_or(0o777) as u32,
        };
        let mode = if use_umask { mode & !UMASK } else { mode };
        let changed = values::string_at(paths, index).is_some_and(|path| {
            std::fs::set_permissions(crate::connections::expand_path(&path), std::fs::Permissions::from_mode(mode)).is_ok()
        });
        values::ints_mut(result)[index] = changed as i32;
    }
    Ok(result)
}

unsafe fn do_sys_set_file_time(call: &Call<'_>) -> R<Sexp> {
    let (paths, times) = (call.arg(0), crate::coerce::coerce_vector(call.arg(1), SexpType::Real)?);
    let result = alloc(SexpType::Logical, length(paths))?;
    for index in 0..length(paths) {
        let time = match length(times) {
            0 => None,
            count => values::reals(times).get(index % count).copied().filter(|time| time.is_finite()),
        };
        let changed = match (values::string_at(paths, index), time) {
            (Some(path), Some(time)) => set_modified_time(&crate::connections::expand_path(&path), time).is_ok(),
            _ => false,
        };
        values::ints_mut(result)[index] = changed as i32;
    }
    Ok(result)
}

fn set_modified_time(path: &str, seconds: f64) -> std::io::Result<()> {
    let time = if seconds >= 0.0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(seconds)
    } else {
        std::time::UNIX_EPOCH - std::time::Duration::from_secs_f64(-seconds)
    };
    let file = if Path::new(path).is_dir() { std::fs::File::open(path)? } else { std::fs::File::options().write(true).open(path)? };
    file.set_times(std::fs::FileTimes::new().set_modified(time).set_accessed(time))
}

fn append_file(target: &str, source: &str) -> std::io::Result<()> {
    let bytes = std::fs::read(source)?;
    let mut file = std::fs::OpenOptions::new().append(true).create(true).open(target)?;
    std::io::Write::write_all(&mut file, &bytes)
}

unsafe fn do_sys_getenv(call: &Call<'_>) -> R<Sexp> {
    let names = call.arg(0);
    let unset = values::first_string(call.arg(1)).map(|text| text.into_owned());
    if length(names) == 0 {
        let mut entries: Vec<String> = std::env::vars().map(|(name, value)| format!("{name}={value}")).collect();
        entries.sort();
        return string_vector(&entries);
    }
    let count = length(names);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let name = values::string_at(names, index).map(|text| text.into_owned()).unwrap_or_default();
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
        let name = values::string_at(names, index).map(|text| text.into_owned()).unwrap_or_default();
        let value = values::string_at(new_values, index).map(|text| text.into_owned()).unwrap_or_default();
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
    let times = |who: libc::c_int| {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(who, &mut usage);
        let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
        (milliseconds(seconds(usage.ru_utime)), milliseconds(seconds(usage.ru_stime)))
    };
    let (user, system) = times(libc::RUSAGE_SELF);
    let (child_user, child_system) = times(libc::RUSAGE_CHILDREN);
    let elapsed = milliseconds(interp().started.elapsed().as_secs_f64());
    let result = values::real_vector(&[user, system, elapsed, child_user, child_system])?;
    let names = string_vector(&["user.self", "sys.self", "elapsed", "user.child", "sys.child"])?;
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
    let era = if shifted >= 0 { shifted } else { shifted - 146096 } / 146097;
    let day_of_era = shifted - era * 146097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
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
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

fn format_broken_down(time: &BrokenDown, format: &str, zone: &str) -> String {
    let mut output = String::new();
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        let Some(spec) = characters.next() else {
            output.push('%');
            break;
        };
        match spec {
            'Y' => output.push_str(&time.year.to_string()),
            'y' => output.push_str(&format!("{:02}", time.year.rem_euclid(100))),
            'm' => output.push_str(&format!("{:02}", time.month)),
            'd' => output.push_str(&format!("{:02}", time.day)),
            'e' => output.push_str(&format!("{:2}", time.day)),
            'H' => output.push_str(&format!("{:02}", time.hour)),
            'I' => output.push_str(&format!("{:02}", if time.hour % 12 == 0 { 12 } else { time.hour % 12 })),
            'M' => output.push_str(&format!("{:02}", time.minute)),
            'S' => output.push_str(&format!("{:02}", time.second.floor() as i64)),
            'p' => output.push_str(if time.hour < 12 { "AM" } else { "PM" }),
            'j' => output.push_str(&format!("{:03}", time.yearday + 1)),
            'B' => output.push_str(MONTHS[(time.month - 1) as usize]),
            'b' | 'h' => output.push_str(&MONTHS[(time.month - 1) as usize][..3]),
            'A' => output.push_str(WEEKDAYS[time.weekday as usize]),
            'a' => output.push_str(&WEEKDAYS[time.weekday as usize][..3]),
            'u' => output.push_str(&(if time.weekday == 0 { 7 } else { time.weekday }).to_string()),
            'w' => output.push_str(&time.weekday.to_string()),
            'Z' => output.push_str(zone),
            'z' => output.push_str("+0000"),
            'F' => output.push_str(&format!("{}-{:02}-{:02}", time.year, time.month, time.day)),
            'T' => output.push_str(&format!("{:02}:{:02}:{:02}", time.hour, time.minute, time.second.floor() as i64)),
            'D' => output.push_str(&format!("{:02}/{:02}/{:02}", time.month, time.day, time.year.rem_euclid(100))),
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
                            output.push_str(&format!("{:0width$.digits$}", time.second, width = width, digits = digits as usize));
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
    }
    output
}

fn format_time(seconds: f64, format: &str) -> String {
    format_broken_down(&broken_down(seconds), format, "UTC")
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
    let abbreviation = if tz == "GMT" { "GMT" } else { "UTC" };
    for (index, &value) in seconds.iter().enumerate() {
        if !value.is_finite() {
            // GNU R keeps a non-finite time, including NA versus NaN, in `sec`.
            values::reals_mut(fields[0])[index] = value;
            for field in 1..8 {
                values::ints_mut(fields[field])[index] = NA_INTEGER;
            }
            values::ints_mut(fields[8])[index] = -1;
            values::set_elt(fields[9], index, values::mkchar("UTC")?);
            values::ints_mut(fields[10])[index] = 0;
            continue;
        }
        let time = broken_down(value);
        values::reals_mut(fields[0])[index] = time.second;
        values::ints_mut(fields[1])[index] = time.minute as i32;
        values::ints_mut(fields[2])[index] = time.hour as i32;
        values::ints_mut(fields[3])[index] = time.day as i32;
        values::ints_mut(fields[4])[index] = time.month as i32 - 1;
        values::ints_mut(fields[5])[index] = time.year as i32 - 1900;
        values::ints_mut(fields[6])[index] = time.weekday as i32;
        values::ints_mut(fields[7])[index] = time.yearday as i32;
        values::ints_mut(fields[8])[index] = 0;
        values::set_elt(fields[9], index, values::mkchar(abbreviation)?);
        values::ints_mut(fields[10])[index] = 0;
    }
    let list = values::list_vector(&fields)?;
    let field_names = string_vector(&POSIXLT_NAMES)?;
    values::set_attr(list, symbols().names, field_names)?;
    let class = string_vector(&["POSIXlt", "POSIXt"])?;
    values::set_attr(list, symbols().class, class)?;
    // Every zone is computed as UTC, but the names stay as given: a GMT time stays GMT, and the
    // session zone is the empty name followed by its standard and daylight-saving abbreviations.
    let tzone = values::install("tzone")?;
    let names = if tz.is_empty() { string_vector(&["", "UTC", "UTC"])? } else { scalar_string(tz)? };
    values::set_attr(list, tzone, names)?;
    let balanced = values::install("balanced")?;
    values::set_attr(list, balanced, scalar_bool(true)?)?;
    Ok(list)
}

unsafe fn do_as_posixlt(call: &Call<'_>) -> R<Sexp> {
    let times = crate::coerce::coerce_vector(call.arg(0), SexpType::Real)?;
    let tz = values::first_string(call.arg(1)).map(|text| text.into_owned()).unwrap_or_default();
    let seconds: Vec<f64> = values::reals(times).to_vec();
    let result = posixlt_from_seconds(&seconds, &tz)?;
    set_year_names(result, values::names(call.arg(0)))?;
    Ok(result)
}

unsafe fn require_posixlt(list: Sexp) -> R<()> {
    if list.kind() != SexpType::List || length(list) < 9 {
        return Err(error("a valid \"POSIXlt\" object is a list of at least 9 elements"));
    }
    Ok(())
}

unsafe fn posixlt_seconds(list: Sexp) -> R<Vec<f64>> {
    require_posixlt(list)?;
    let field = |index: usize| -> Sexp { if length(list) > index { list.element(index) } else { nil() } };
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
        if [minute, hour, day, month, year].iter().any(|value| value.is_nan()) {
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
        let count = (0..length(list)).map(|index| length(list.element(index))).max().unwrap_or(0);
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
        let tzone = get_attr(list, values::install("tzone")?);
        let tz = values::first_string(tzone).map(|text| text.into_owned()).unwrap_or_default();
        let seconds = posixlt_seconds(list)?;
        let result = posixlt_from_seconds(&seconds, &tz)?;
        set_year_names(result, values::names(list.element(5)))?;
        result
    };
    if !classed {
        values::remove_attr(result, symbols().class);
    }
    Ok(result)
}

unsafe fn do_as_posixct(call: &Call<'_>) -> R<Sexp> {
    let seconds = posixlt_seconds(call.arg(0))?;
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

unsafe fn set_names_from_year(list: Sexp, result: Sexp) -> R<()> {
    let names = values::names(list.element(5));
    if names != nil() && length(names) == length(result) {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(())
}

unsafe fn do_format_posixlt(call: &Call<'_>) -> R<Sexp> {
    let list = call.arg(0);
    let formats = call.arg(1);
    let use_tz = values::as_logical_scalar(call.arg(2)) == 1;
    let seconds = posixlt_seconds(list)?;
    let count = seconds.len();
    let result = alloc(SexpType::String, count)?;
    for (index, &value) in seconds.iter().enumerate() {
        if is_na_real(value) {
            values::set_elt(result, index, values::na_string());
            continue;
        }
        if !value.is_finite() {
            let text = if value.is_nan() { "NaN" } else if value > 0.0 { "Inf" } else { "-Inf" };
            values::set_elt(result, index, values::mkchar(text)?);
            continue;
        }
        let format = values::string_at(formats, index % length(formats).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".into());
        let mut text = format_time(value, &format);
        if use_tz {
            text.push_str(" UTC");
        }
        values::set_elt(result, index, values::mkchar(&text)?);
    }
    let names = values::names(list.element(5));
    if names != nil() {
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// Parses `text` with a `strptime` format, returning seconds since the epoch.
fn parse_time(text: &str, format: &str) -> Option<f64> {
    let mut year = 1970i64;
    let mut month = 1i64;
    let mut day = 1i64;
    let mut hour = 0i64;
    let mut minute = 0i64;
    let mut second = 0f64;
    let mut yearday = None;
    let mut input = text.trim_start();
    let mut format_characters = format.chars();
    let number = |input: &mut &str, max_digits: usize| -> Option<i64> {
        let trimmed = input.trim_start();
        let (sign, rest) = match trimmed.strip_prefix('-') {
            Some(rest) => (-1, rest),
            None => (1, trimmed),
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).take(max_digits).collect();
        if digits.is_empty() {
            return None;
        }
        *input = &rest[digits.len()..];
        digits.parse::<i64>().ok().map(|value| sign * value)
    };
    while let Some(character) = format_characters.next() {
        if character != '%' {
            if character.is_whitespace() {
                input = input.trim_start();
                continue;
            }
            input = input.strip_prefix(character)?;
            continue;
        }
        match format_characters.next()? {
            'Y' => year = number(&mut input, 4)?,
            'y' => {
                let value = number(&mut input, 2)?;
                year = if value < 69 { 2000 + value } else { 1900 + value };
            }
            'm' => month = number(&mut input, 2)?,
            'd' | 'e' => day = number(&mut input, 2)?,
            'H' => hour = number(&mut input, 2)?,
            'M' => minute = number(&mut input, 2)?,
            'S' => second = number(&mut input, 2)? as f64,
            'O' => {
                if format_characters.next()? == 'S' {
                    let trimmed = input.trim_start();
                    let length = trimmed.chars().take_while(|c| c.is_ascii_digit() || *c == '.').count();
                    second = trimmed[..length].parse().ok()?;
                    input = &trimmed[length..];
                }
            }
            'j' => yearday = Some(number(&mut input, 3)?),
            'B' | 'b' | 'h' => {
                let lower = input.to_lowercase();
                let found = MONTHS.iter().position(|name| lower.starts_with(&name.to_lowercase()))
                    .map(|position| (position, MONTHS[position].len()))
                    .or_else(|| MONTHS.iter().position(|name| lower.starts_with(&name[..3].to_lowercase())).map(|position| (position, 3)))?;
                month = found.0 as i64 + 1;
                input = &input[found.1..];
            }
            'F' => {
                year = number(&mut input, 4)?;
                input = input.strip_prefix('-')?;
                month = number(&mut input, 2)?;
                input = input.strip_prefix('-')?;
                day = number(&mut input, 2)?;
            }
            'T' => {
                hour = number(&mut input, 2)?;
                input = input.strip_prefix(':')?;
                minute = number(&mut input, 2)?;
                input = input.strip_prefix(':')?;
                second = number(&mut input, 2)? as f64;
            }
            '%' => input = input.strip_prefix('%')?,
            _ => {}
        }
    }
    if let Some(yearday) = yearday {
        if !(1..=if is_leap(year) { 366 } else { 365 }).contains(&yearday) {
            return None;
        }
        month = 1;
        day = yearday;
    }
    if !(1..=12).contains(&month) {
        return None;
    }
    let month_days = [31, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if day < 1 || day > month_days[(month - 1) as usize] && format.contains("%d") {
        return None;
    }
    let days = days_from_civil(year, month, 1) + day - 1;
    Some(days as f64 * 86400.0 + hour as f64 * 3600.0 + minute as f64 * 60.0 + second)
}

unsafe fn do_strptime(call: &Call<'_>) -> R<Sexp> {
    let texts = call.arg(0);
    let formats = call.arg(1);
    let tz = values::first_string(call.arg(2)).map(|text| text.into_owned()).unwrap_or_default();
    let count = length(texts);
    let mut seconds = Vec::with_capacity(count);
    for index in 0..count {
        let format = values::string_at(formats, index % length(formats).max(1))
            .map(|text| text.into_owned())
            .unwrap_or_default();
        let value = values::string_at(texts, index)
            .and_then(|text| parse_time(&text, &format))
            .unwrap_or(NA_REAL);
        seconds.push(value);
    }
    let result = posixlt_from_seconds(&seconds, &tz)?;
    set_year_names(result, values::names(texts))?;
    Ok(result)
}

unsafe fn do_date_to_posixlt(call: &Call<'_>) -> R<Sexp> {
    let dates = crate::coerce::coerce_vector(call.arg(0), SexpType::Real)?;
    let seconds: Vec<f64> = values::reals(dates)
        .iter()
        .map(|&days| if days.is_finite() { days.floor() * 86400.0 } else { days })
        .collect();
    let result = posixlt_from_seconds(&seconds, "UTC")?;
    set_year_names(result, values::names(call.arg(0)))?;
    Ok(result)
}

unsafe fn do_posixlt_to_date(call: &Call<'_>) -> R<Sexp> {
    let seconds = posixlt_seconds(call.arg(0))?;
    let days: Vec<f64> = seconds
        .iter()
        .map(|&value| if value.is_finite() { (value / 86400.0).floor() } else { value })
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
        let path = values::string_at(paths, index).map(|text| crate::connections::expand_path(&text)).unwrap_or_default();
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                values::reals_mut(size)[index] = metadata.len() as f64;
                values::ints_mut(is_dir)[index] = metadata.is_dir() as i32;
                values::ints_mut(mode)[index] = (metadata.permissions().mode() & 0o777) as i32;
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
    values::named_list(&[("size", size), ("isdir", is_dir), ("mode", mode), ("mtime", mtime), ("ctime", mtime), ("atime", mtime)])
}

fn walk(directory: &Path, recursive: bool, include_dirs: bool, output: &mut Vec<PathBuf>, all: bool) {
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
    let regex = match &pattern {
        Some(pattern) => Some(
            regex::RegexBuilder::new(&crate::strings::translate_regex(pattern, true))
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
        walk(Path::new(&directory), recursive, include_dirs, &mut paths, all);
        if all && !recursive && !no_dots {
            names.push(if full { format!("{directory}/.") } else { ".".into() });
            names.push(if full { format!("{directory}/..") } else { "..".into() });
        }
        for path in paths {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(regex) = &regex
                && !regex.is_match(&name)
            {
                continue;
            }
            let shown = if full {
                path.display().to_string()
            } else {
                path.strip_prefix(&directory).map(|relative| relative.display().to_string()).unwrap_or(name)
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
        names.push(if full { directory.clone() } else { String::new() });
        let mut paths = Vec::new();
        walk(Path::new(&directory), recursive, true, &mut paths, true);
        for path in paths.into_iter().filter(|path| path.is_dir()) {
            names.push(if full {
                path.display().to_string()
            } else {
                path.strip_prefix(&directory).map(|relative| relative.display().to_string()).unwrap_or_default()
            });
        }
    }
    crate::sort::sort_strings(&mut names);
    string_vector(&names)
}

unsafe fn do_normalize_path(call: &Call<'_>) -> R<Sexp> {
    let paths = call.arg(0);
    let must_work = values::as_logical_scalar(call.arg(2));
    let count = length(paths);
    let result = alloc(SexpType::String, count)?;
    for index in 0..count {
        let Some(path) = values::string_at(paths, index) else {
            values::set_elt(result, index, values::na_string());
            continue;
        };
        let expanded = crate::connections::expand_path(&path);
        let normalized = match std::fs::canonicalize(&expanded) {
            Ok(canonical) => canonical.display().to_string(),
            Err(_) => {
                if must_work == 1 {
                    return Err(call.error(format!("path[{}]=\"{path}\": No such file or directory", index + 1)));
                }
                if must_work == NA_INTEGER {
                    crate::conditions::warning_call(call.call, format!("path[{}]=\"{path}\": No such file or directory", index + 1))?;
                }
                expanded
            }
        };
        values::set_elt(result, index, values::mkchar(&normalized)?);
    }
    Ok(result)
}

unsafe fn do_file_path(call: &Call<'_>) -> R<Sexp> {
    let parts = call.arg(0);
    let separator = values::first_string(call.arg(1)).map(|text| text.into_owned()).unwrap_or_else(|| "/".into());
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
            .map(|piece| values::string_at(*piece, index % length(*piece)).map_or_else(|| "NA".into(), |text| text.into_owned()))
            .collect();
        paths.push(parts.join(&separator));
    }
    string_vector(&paths)
}

unsafe fn do_tempfile(call: &Call<'_>) -> R<Sexp> {
    let patterns = call.arg(0);
    let directories = call.arg(1);
    let extensions = call.arg(2);
    let count = length(patterns).max(length(directories)).max(length(extensions));
    let mut paths = Vec::new();
    static mut COUNTER: u64 = 0;
    for index in 0..count {
        let pattern = values::string_at(patterns, index % length(patterns).max(1)).map(|text| text.into_owned()).unwrap_or_default();
        let directory = values::string_at(directories, index % length(directories).max(1)).map(|text| text.into_owned()).unwrap_or_else(temp_dir);
        let extension = values::string_at(extensions, index % length(extensions).max(1)).map(|text| text.into_owned()).unwrap_or_default();
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
            if recursive { std::fs::remove_dir_all(target) } else { std::fs::remove_dir(target) }
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
        let source = values::string_at(from, index).map(|text| text.into_owned()).unwrap_or_default();
        let target = values::string_at(to, index).map(|text| text.into_owned()).unwrap_or_default();
        values::ints_mut(result)[index] = std::fs::rename(source, target).is_ok() as i32;
    }
    Ok(result)
}

unsafe fn do_dir_create(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "path")?;
    let recursive = values::as_logical_scalar(call.arg(2)) == 1;
    let path = crate::connections::expand_path(&path);
    let outcome = if recursive { std::fs::create_dir_all(&path) } else { std::fs::create_dir(&path) };
    scalar_bool(outcome.is_ok())
}

unsafe fn do_setwd(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "dir")?;
    let previous = std::env::current_dir().map(|path| path.display().to_string()).unwrap_or_default();
    std::env::set_current_dir(crate::connections::expand_path(&path))
        .map_err(|_| call.error("cannot change working directory"))?;
    scalar_string(&previous)
}

unsafe fn do_sys_glob(call: &Call<'_>) -> R<Sexp> {
    let patterns = call.arg(0);
    let mut matches = Vec::new();
    for index in 0..length(patterns) {
        let Some(pattern) = values::string_at(patterns, index) else {
            continue;
        };
        let pattern = crate::connections::expand_path(&pattern);
        let (directory, file_pattern) = match pattern.rfind('/') {
            Some(position) => (pattern[..position].to_owned(), pattern[position + 1..].to_owned()),
            None => (".".to_owned(), pattern.clone()),
        };
        let regex_text = format!(
            "^{}$",
            regex::escape(&file_pattern).replace("\\*", ".*").replace("\\?", ".")
        );
        let Ok(regex) = regex::Regex::new(&regex_text) else {
            continue;
        };
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if regex.is_match(&name) && !name.starts_with('.') {
                    matches.push(if pattern.contains('/') { format!("{directory}/{name}") } else { name });
                }
            }
        }
    }
    crate::sort::sort_strings(&mut matches);
    string_vector(&matches)
}

unsafe fn do_sys_info(_call: &Call<'_>) -> R<Sexp> {
    let mut name: libc::utsname = std::mem::zeroed();
    libc::uname(&mut name);
    let field = |raw: &[libc::c_char]| -> String {
        let bytes: Vec<u8> = raw.iter().take_while(|&&byte| byte != 0).map(|&byte| byte as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let values_list = [
        field(&name.sysname),
        field(&name.release),
        field(&name.version),
        field(&name.nodename),
        field(&name.machine),
        user.clone(),
        user.clone(),
        user,
    ];
    let result = string_vector(&values_list)?;
    let names = string_vector(&["sysname", "release", "version", "nodename", "machine", "login", "user", "effective_user"])?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

unsafe fn do_l10n_info(_call: &Call<'_>) -> R<Sexp> {
    let utf8 = scalar_bool(true)?;
    let latin1 = scalar_bool(false)?;
    let mbcs = scalar_bool(true)?;
    let codeset = scalar_string("UTF-8")?;
    values::named_list(&[("MBCS", mbcs), ("UTF-8", utf8), ("Latin-1", latin1), ("codeset", codeset)])
}

unsafe fn do_version(_call: &Call<'_>) -> R<Sexp> {
    let platform = format!("{}-pc-linux-gnu", std::env::consts::ARCH);
    let fields: Vec<(&str, String)> = vec![
        ("platform", platform.clone()),
        ("arch", std::env::consts::ARCH.to_owned()),
        ("os", "linux-gnu".to_owned()),
        ("system", format!("{}, linux-gnu", std::env::consts::ARCH)),
        ("status", String::new()),
        ("major", "4".to_owned()),
        ("minor", "3.3".to_owned()),
        ("year", "2024".to_owned()),
        ("month", "02".to_owned()),
        ("day", "29".to_owned()),
        ("svn rev", "86002".to_owned()),
        ("language", "R".to_owned()),
        ("version.string", "R version 4.3.3 (2024-02-29)".to_owned()),
        ("nickname", "Angel Food Cake".to_owned()),
    ];
    let items: Vec<(&str, Sexp)> = fields
        .iter()
        .map(|(name, value)| Ok((*name, scalar_string(value)?)))
        .collect::<R<_>>()?;
    values::named_list(&items)
}

unsafe fn do_capabilities(_call: &Call<'_>) -> R<Sexp> {
    let names = [
        "jpeg", "png", "tiff", "tcltk", "X11", "aqua", "http/ftp", "sockets", "libxml", "fifo",
        "cledit", "iconv", "NLS", "Rprof", "profmem", "cairo", "ICU", "long.double", "libcurl",
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
    policy.stress = if on { GcStress::EveryAllocation } else { GcStress::Off };
    previous
}

unsafe fn do_reg_finalizer(call: &Call<'_>) -> R<Sexp> {
    let key = call.arg(0);
    let finalizer = call.arg(1);
    if !matches!(key.kind(), SexpType::Environment | SexpType::ExternalPtr) {
        return Err(call.error("first argument must be environment or external pointer"));
    }
    if !matches!(finalizer.kind(), SexpType::Closure | SexpType::Builtin | SexpType::Special) {
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
                &format!("running command '{command}' had status {status}"),
            )?;
        }
        return Ok(result);
    }
    // The child writes straight to the terminal, as GNU R's does, so earlier output must be out
    // first to keep the order.
    std::io::stdout().flush().map_err(|failure| call.error(format!("cannot flush stdout: {failure}")))?;
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

unsafe fn do_set_encoding(call: &Call<'_>) -> R<Sexp> {
    let (strings, encodings) = (call.arg(0), call.arg(1));
    if strings.kind() != SexpType::String {
        return Err(call.error("a character vector argument expected"));
    }
    if encodings.kind() != SexpType::String || length(encodings) == 0 {
        return Err(call.error("a character vector 'value' expected"));
    }
    let result = values::shallow_duplicate(strings)?;
    for index in 0..length(strings) {
        let charsxp = strings.element(index);
        if charsxp == values::na_string() {
            continue;
        }
        let encoding = match values::string_at(encodings, index % length(encodings)).as_deref() {
            Some("latin1") => Encoding::Latin1,
            Some("UTF-8") => Encoding::Utf8,
            Some("bytes") => Encoding::Bytes,
            _ => native_encoding(charsxp.bytes()),
        };
        values::set_elt(result, index, values::mkchar_bytes(charsxp.bytes(), encoding)?);
    }
    Ok(result)
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

/// `.Internal(iconv(x, from, to, sub, mark, toRaw))`, on top of the C library's `iconv`.
unsafe fn do_iconv(call: &Call<'_>) -> R<Sexp> {
    let input = call.arg(0);
    let from = values::first_string(call.arg(1)).map(|text| text.into_owned()).unwrap_or_default();
    let to = values::first_string(call.arg(2)).map(|text| text.into_owned()).unwrap_or_default();
    let substitute = values::first_string(call.arg(3)).map(|text| text.into_owned());
    let mark = values::as_logical_scalar(call.arg(4)) == 1;
    let to_raw = values::as_logical_scalar(call.arg(5)) == 1;
    let is_list = input.kind() == SexpType::List;
    if !is_list && input.kind() != SexpType::String {
        return Err(call.error("'x' must be a character vector"));
    }
    let native = |name: &str| if name.is_empty() { "UTF-8".to_owned() } else { name.to_owned() };
    let (from, to) = (native(&from), native(&to));
    let converter = Converter::open(&from, &to)
        .ok_or_else(|| error(format!("unsupported conversion from '{from}' to '{to}'")))?;
    let target = to.split("//").next().unwrap_or("").to_ascii_uppercase();
    let encoding = match target.as_str() {
        _ if !mark => Encoding::Native,
        "UTF-8" | "UTF8" => Encoding::Utf8,
        "LATIN1" | "ISO-8859-1" | "ISO8859-1" | "ISO_8859-1" => Encoding::Latin1,
        _ => Encoding::Native,
    };
    let from_utf8 = matches!(from.to_ascii_uppercase().as_str(), "UTF-8" | "UTF8");
    let count = length(input);
    let result = if to_raw {
        alloc(SexpType::List, count)?
    } else if is_list {
        alloc(SexpType::String, count)?
    } else {
        values::shallow_duplicate(input)?
    };
    for index in 0..count {
        let element = input.element(index);
        let bytes: Option<&[u8]> = if is_list {
            match element.kind() {
                SexpType::Raw => Some(values::raws(element)),
                SexpType::Nil => None,
                _ => return Err(call.error("'x' must be a character vector or a list of NULL or raw vectors")),
            }
        } else if element == values::na_string() {
            None
        } else {
            Some(element.bytes())
        };
        let converted = bytes.and_then(|bytes| converter.convert(bytes, substitute.as_deref(), from_utf8));
        let value = match (converted, to_raw) {
            (None, true) => nil(),
            (None, false) => values::na_string(),
            (Some(bytes), true) => {
                let raw = alloc(SexpType::Raw, bytes.len())?;
                values::raws_mut(raw).copy_from_slice(&bytes);
                raw
            }
            (Some(bytes), false) => {
                let encoding = if encoding == Encoding::Native { native_encoding(&bytes) } else { encoding };
                values::mkchar_bytes(&bytes, encoding)?
            }
        };
        values::set_elt(result, index, value);
    }
    Ok(result)
}

/// The mark for text in the native encoding. The native encoding is always UTF-8, and ry marks
/// valid UTF-8 as such everywhere, so that equal text is one interned string; GNU R instead keeps
/// it unmarked and translates when it compares.
fn native_encoding(bytes: &[u8]) -> Encoding {
    if std::str::from_utf8(bytes).is_ok() { Encoding::Utf8 } else { Encoding::Native }
}

/// An open `iconv_t` descriptor, closed on drop.
struct Converter(libc::iconv_t);

impl Converter {
    fn open(from: &str, to: &str) -> Option<Converter> {
        let (from, to) = (std::ffi::CString::new(from).ok()?, std::ffi::CString::new(to).ok()?);
        let descriptor = unsafe { libc::iconv_open(to.as_ptr(), from.as_ptr()) };
        (descriptor as isize != -1).then_some(Converter(descriptor))
    }

    /// Converts `input`, or returns `None` for input the target cannot represent when there is no
    /// substitute. `substitute` follows `iconv(sub = )`: `"byte"` writes `<xx>`, `"Unicode"` writes
    /// `<U+xxxx>` for a UTF-8 character, `"c99"` writes `\uxxxx`, and anything else is written as
    /// given, once per unconvertible byte.
    fn convert(&self, input: &[u8], substitute: Option<&str>, from_utf8: bool) -> Option<Vec<u8>> {
        let mut output = Vec::with_capacity(input.len() + 16);
        let mut buffer = [0u8; 4096];
        let mut position = 0;
        // Resets the shift state, so a descriptor reused across elements starts clean.
        unsafe { libc::iconv(self.0, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        loop {
            let mut in_pointer = input[position..].as_ptr() as *mut libc::c_char;
            let mut in_left = input.len() - position;
            let mut out_pointer = buffer.as_mut_ptr() as *mut libc::c_char;
            let mut out_left = buffer.len();
            let finished = in_left == 0;
            let status = unsafe {
                if finished {
                    libc::iconv(self.0, std::ptr::null_mut(), std::ptr::null_mut(), &mut out_pointer, &mut out_left)
                } else {
                    libc::iconv(self.0, &mut in_pointer, &mut in_left, &mut out_pointer, &mut out_left)
                }
            };
            let error = std::io::Error::last_os_error().raw_os_error();
            output.extend_from_slice(&buffer[..buffer.len() - out_left]);
            position = input.len() - in_left;
            if status != usize::MAX {
                if finished {
                    return Some(output);
                }
                continue;
            }
            match error {
                Some(libc::E2BIG) => continue,
                Some(libc::EILSEQ) | Some(libc::EINVAL) => {
                    let rest = &input[position..];
                    let first = *rest.first()?;
                    let character = from_utf8
                        .then(|| utf8_character(rest))
                        .flatten();
                    let (text, consumed) = match (substitute?, character) {
                        ("byte", _) => (format!("<{first:02x}>"), 1),
                        ("Unicode", Some((code, width))) => (format!("<U+{code:04X}>"), width),
                        ("c99", Some((code, width))) if code > 0xFFFF => (format!("\\U{code:08x}"), width),
                        ("c99", Some((code, width))) => (format!("\\u{code:04x}"), width),
                        ("Unicode" | "c99", None) => (format!("<{first:02x}>"), 1),
                        (text, _) => (text.to_owned(), 1),
                    };
                    output.extend_from_slice(text.as_bytes());
                    position += consumed;
                    if position >= input.len() {
                        return Some(output);
                    }
                }
                _ => return None,
            }
        }
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        unsafe { libc::iconv_close(self.0) };
    }
}

/// The code point and byte width of the UTF-8 character `bytes` starts with.
fn utf8_character(bytes: &[u8]) -> Option<(u32, usize)> {
    let width = match *bytes.first()? {
        byte if byte < 0x80 => 1,
        byte if byte >> 5 == 0b110 => 2,
        byte if byte >> 4 == 0b1110 => 3,
        byte if byte >> 3 == 0b11110 => 4,
        _ => return None,
    };
    let character = std::str::from_utf8(bytes.get(..width)?).ok()?.chars().next()?;
    Some((character as u32, width))
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
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xC96C5795D7870F42 } else { crc >> 1 };
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
        ("OS.type", string("unix")?),
        ("file.sep", string("/")?),
        ("dynlib.ext", string(".so")?),
        ("GUI", string("X11")?),
        ("endian", string("little")?),
        ("pkgType", string("source")?),
        ("path.sep", string(":")?),
        ("r_arch", string("")?),
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
    let result = string_vector(&entries.iter().map(|(_, version)| *version).collect::<Vec<_>>())?;
    let names = string_vector(&entries.iter().map(|(name, _)| *name).collect::<Vec<_>>())?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

unsafe fn do_read_renviron(call: &Call<'_>) -> R<Sexp> {
    let path = values::require_string(call.arg(0), "x")?;
    let path = crate::connections::expand_path(&path);
    let Ok(text) = std::fs::read_to_string(&path) else {
        crate::conditions::warning_call(call.call, &format!("file '{path}' cannot be opened for reading"))?;
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
        let value = value.trim().trim_matches(|quote| quote == '"' || quote == '\'');
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
