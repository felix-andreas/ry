//! The R front end that `bin/R` and `bin/Rscript` in ry's R home run. `CMD INSTALL|SHLIB|config`
//! and `RHOME` are answered in Rust; `R ...` and `Rscript ...` run R code on ry's interpreter,
//! taking GNU R's command lines, and so do the `R CMD` commands that are R functions of the
//! tools and utils packages, such as `R CMD build`. The R home is the directory `R_HOME` names. `assemble ROOT
//! --include DIR --libr FILE [--gnu-r-home DIR]` creates such a home.
//!
//! The `ry-rcmd` binary is this front end alone, and `ry` is it too when started under one of
//! the names [`invoked_as_front_end`] recognizes, so a release can ship `ry` as its only file.

use std::{
    ffi::{CString, c_char, c_int},
    path::{Path, PathBuf},
    process::ExitCode,
};

/// Whether this process was started as the front end: under the name `ry-rcmd` (an R home's
/// `bin/exec/ry-rcmd`, which may link to `ry`), or on Windows as one of an R home's `R.exe`,
/// `Rscript.exe`, `Rcmd.exe` or `Rterm.exe`.
pub fn invoked_as_front_end() -> bool {
    let named_rcmd = std::env::args_os().next().is_some_and(|program| {
        Path::new(&program)
            .file_stem()
            .is_some_and(|stem| stem == "ry-rcmd")
    });
    named_rcmd || windows_front_end().is_some()
}

/// Runs the front end with `arguments`, the command line after the program name.
pub fn main(arguments: Vec<String>) -> ExitCode {
    if arguments.first().is_some_and(|first| first == "assemble") {
        return match assemble(&arguments[1..]) {
            Ok(root) => {
                println!("{}", root.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("ry-rcmd assemble: {error}");
                ExitCode::from(2)
            }
        };
    }
    // A Windows front end's home is the one it sits in, as for GNU R's: an inherited `R_HOME`
    // may name another R entirely, such as the GNU R session that started this process.
    let front_end = windows_front_end();
    let Some(home) = front_end
        .as_ref()
        .map(|(_, home)| home.clone())
        .or_else(|| std::env::var_os("R_HOME").map(PathBuf::from))
    else {
        eprintln!(
            "ry-rcmd: R_HOME is not set; run it through bin/R of an R home that `crate::assemble` created"
        );
        return ExitCode::from(2);
    };
    // Windows runs no `bin/R` script to export the home, so the front end exports it for the
    // tools R CMD INSTALL starts (make, configure, and R itself). Like GNU R's, it uses forward
    // slashes, which `Rcmd_environ` needs: an unquoted `${R_HOME}` there loses its backslashes.
    if front_end.is_some() {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("R_HOME", home.display().to_string().replace('\\', "/")) };
    }
    let arguments = match front_end.as_ref().map(|(name, _)| name.as_str()) {
        Some("R") => match arguments.first().map(String::as_str) {
            Some("CMD" | "RHOME") => arguments,
            _ => [vec!["R".to_owned()], arguments].concat(),
        },
        Some("Rterm") => [vec!["R".to_owned()], arguments].concat(),
        Some("Rscript") => [vec!["Rscript".to_owned()], arguments].concat(),
        Some("Rcmd") => [vec!["CMD".to_owned()], arguments].concat(),
        _ => arguments,
    };
    // GNU R's `Rcmd.exe` reads `etc/Rcmd_environ` before any `R CMD` command, which is what puts
    // Rtools ahead of other toolchains on `PATH`. A Unix home's `bin/Rcmd` script reads
    // `etc/Renviron` itself.
    if front_end.is_some() && arguments.first().is_some_and(|first| first == "CMD") {
        let assignments = match crate::command_environment(&home, |name| std::env::var(name).ok()) {
            Ok(assignments) => assignments,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        for (name, value) in assignments {
            // SAFETY: no other thread exists yet.
            unsafe { std::env::set_var(name, value) };
        }
    }
    let tools = match arguments.split_first() {
        Some((front, rest)) if front == "CMD" => tools_command(rest),
        _ => None,
    };
    let result = match (arguments.split_first(), tools) {
        (_, Some(session)) => run_r(&home, &session),
        (Some((front, rest)), None) if front == "R" => {
            crate::r_command_line(rest).and_then(|session| run_r(&home, &session))
        }
        (Some((front, rest)), None) if front == "Rscript" => {
            crate::rscript_command_line(rest).and_then(|session| run_r(&home, &session))
        }
        _ => crate::run(&home, &arguments).map(|output| {
            print!("{output}");
            0
        }),
    };
    match result {
        Ok(status) => ExitCode::from(u8::try_from(status).unwrap_or(1)),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The session GNU R's `R CMD <command>` starts for a command its tools or utils package
/// implements. Unix reaches these through GNU R's `bin/` scripts, which run R through ry's
/// `bin/R`; Windows has no such scripts, because GNU R's `Rcmd.exe` dispatches them itself, so
/// the front end starts the session those scripts would. Each script passes its arguments as
/// one string, every argument after the separator `nextArg`, because the R function splits
/// `commandArgs(TRUE)` on it.
fn tools_command(arguments: &[String]) -> Option<crate::Session> {
    let (command, rest) = arguments.split_first()?;
    // The function, `R_DEFAULT_PACKAGES`, whether `--vanilla` skips the profiles, and whether
    // the script collates in the C locale.
    let (entry, default_packages, vanilla, c_collation) = match command.as_str() {
        "build" => ("tools:::.build_packages()", Some(""), false, true),
        "check" => ("tools:::.check_packages()", Some(""), false, true),
        "Rprof" => ("tools:::.Rprof()", Some("utils"), true, true),
        "Rdconv" => ("tools:::.Rdconv()", Some(""), true, true),
        "Sweave" => ("utils:::.Sweave()", None, false, false),
        "Stangle" => ("utils:::.Stangle()", None, false, false),
        _ => return None,
    };
    if c_collation {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("LC_COLLATE", "C") };
    }
    Some(crate::Session::Run {
        file: None,
        expressions: Some(entry.to_owned()),
        arguments: vec![
            rest.iter()
                .map(|argument| format!("nextArg{argument}"))
                .collect(),
        ],
        default_packages: default_packages.map(str::to_owned),
        site_profile: !vanilla,
        user_profile: !vanilla,
    })
}

/// On Windows, `bin/R.exe`, `bin/Rscript.exe`, `bin/x64/Rcmd.exe` and `bin/x64/Rterm.exe` (what
/// callr starts) of an R home are copies of
/// this binary: which front end it is comes from its file name, and the home is the directory
/// above `bin/` (or `bin/x64/`).
fn windows_front_end() -> Option<(String, PathBuf)> {
    if !cfg!(windows) {
        return None;
    }
    let executable = std::env::current_exe().ok()?;
    let name = executable.file_stem()?.to_str()?;
    let name = ["R", "Rterm", "Rscript", "Rcmd"]
        .into_iter()
        .find(|front_end| front_end.eq_ignore_ascii_case(name))?;
    let mut bin = executable.parent()?;
    if bin.file_name().is_some_and(|directory| directory == "x64") {
        bin = bin.parent()?;
    }
    Some((name.to_owned(), bin.parent()?.to_path_buf()))
}

fn assemble(arguments: &[String]) -> crate::Result<PathBuf> {
    let mut root = None;
    let mut include = None;
    let mut libr = None;
    let mut gnu_r_home = None;
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let slot = match argument.as_str() {
            "--include" => &mut include,
            "--libr" => &mut libr,
            "--gnu-r-home" => &mut gnu_r_home,
            _ => {
                root = Some(PathBuf::from(argument));
                continue;
            }
        };
        *slot = rest.next().map(PathBuf::from);
    }
    let usage = || {
        crate::Error::Usage(
            "usage: ry-rcmd assemble ROOT --include DIR --libr FILE [--gnu-r-home DIR]".into(),
        )
    };
    let (Some(root), Some(include), Some(libr)) = (root, include, libr) else {
        return Err(usage());
    };
    let gnu_r_home = match gnu_r_home {
        Some(home) => home,
        None => {
            let output = std::process::Command::new("R")
                .arg("RHOME")
                .output()
                .map_err(|error| {
                    crate::Error::Usage(format!("cannot run `R RHOME` to find GNU R: {error}"))
                })?;
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        }
    };
    let rcmd = std::env::current_exe()
        .map_err(|error| crate::Error::Io(PathBuf::from("ry-rcmd"), error))?;
    let absolute =
        |path: PathBuf| std::path::absolute(&path).map_err(|error| crate::Error::Io(path, error));
    let root = absolute(root)?;
    crate::assemble(
        &root,
        &crate::HomeSources {
            include: &absolute(include)?,
            libr: &absolute(libr)?,
            rcmd: &rcmd,
            gnu_r_home: &gnu_r_home,
            toolchain: crate::Toolchain::for_gnu_r_home(&gnu_r_home)?,
        },
    )?;
    Ok(root)
}

/// Runs a session on ry's runtime, the libR of the R home, which also holds the interpreter.
fn run_r(home: &Path, session: &crate::Session) -> crate::Result<i32> {
    let (file, expressions, arguments, default_packages, site_profile, user_profile) = match session
    {
        crate::Session::Print(text) => {
            print!("{text}");
            return Ok(0);
        }
        crate::Session::Version { rscript } => {
            print!("{}", crate::version_text(home, *rscript)?);
            return Ok(0);
        }
        crate::Session::Run {
            file,
            expressions,
            arguments,
            default_packages,
            site_profile,
            user_profile,
        } => (
            file,
            expressions,
            arguments,
            default_packages,
            *site_profile,
            *user_profile,
        ),
    };
    if let Some(packages) = default_packages {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("R_DEFAULT_PACKAGES", packages) };
    }
    // With neither a file nor `-e`, `ry_run` reads stdin as a console, one expression at a time.
    let input = expressions.clone();
    let library_path = crate::libr_path(home);
    // Loading ry's libR globally is what lets compiled packages resolve R's API from it: by
    // soname on Linux, by flat lookup on macOS. On Windows a package DLL imports from "R.dll"
    // by name, which finds the module already loaded.
    #[cfg(unix)]
    let library = unsafe {
        libloading::os::unix::Library::open(
            Some(&library_path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
        )
    }
    .map(libloading::Library::from);
    #[cfg(windows)]
    let library = unsafe { libloading::Library::new(&library_path) };
    let library = library.map_err(|error| {
        crate::Error::Usage(format!(
            "cannot load ry's runtime {}: {error}",
            library_path.display()
        ))
    })?;
    type Run = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        *const *const c_char,
        c_int,
    ) -> c_int;
    let run = unsafe { library.get::<Run>(b"ry_run\0") }
        .map(|symbol| *symbol)
        .map_err(|error| {
            crate::Error::Usage(format!(
                "{} is not ry's runtime: {error}",
                library_path.display()
            ))
        })?;
    let text = |value: &str| {
        CString::new(value)
            .map_err(|_| crate::Error::Usage("an argument contains a NUL byte".into()))
    };
    let file = file.as_deref().map(text).transpose()?;
    let input = input.as_deref().map(text).transpose()?;
    let arguments = arguments
        .iter()
        .map(|argument| text(argument))
        .collect::<crate::Result<Vec<_>>>()?;
    let pointers: Vec<*const c_char> = arguments.iter().map(|argument| argument.as_ptr()).collect();
    let pointer = |value: &Option<CString>| {
        value
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr())
    };
    // Inline annotations are checked and the object systems are on, as in `ry run`'s defaults;
    // the profiles follow the command line. The bits are libr's `RUN_FLAG_*` constants.
    let flags =
        0b11 | if site_profile { 1 << 3 } else { 0 } | if user_profile { 1 << 4 } else { 0 };
    let status = unsafe {
        run(
            pointer(&file),
            pointer(&input),
            c_int::try_from(pointers.len()).unwrap_or(c_int::MAX),
            pointers.as_ptr(),
            flags,
        )
    };
    // The runtime keeps global state and threads that must not be torn down by unloading it.
    std::mem::forget(library);
    Ok(status)
}
