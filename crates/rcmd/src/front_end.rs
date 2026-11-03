//! The R front end that `bin/R` and `bin/Rscript` in ry's R home run. `CMD INSTALL|SHLIB|config`
//! and `RHOME` are answered in Rust; `R ...` and `Rscript ...` run R code on ry's interpreter,
//! taking GNU R's command lines, and so do the `R CMD` commands that are R functions of the
//! tools and utils packages, such as `R CMD build`. The R home is the directory `R_HOME` names.
//!
//! Two commands prepare homes rather than run one. `assemble ROOT --include DIR --libr FILE
//! --base DIR` creates an R home over a base R tree, and `build-base SOURCE OUTPUT --include DIR
//! --libr FILE --sha256 SUM` builds ry's base R tree from GNU R's unpacked source release
//! ([`crate::base`]).
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
    let command = arguments.first().map(String::as_str);
    if let Some(command @ ("assemble" | "build-base")) = command {
        let result = if command == "assemble" {
            assemble(&arguments[1..])
        } else {
            build_base(&arguments[1..])
        };
        return match result {
            Ok(root) => {
                println!("{}", root.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("ry-rcmd {command}: {error}");
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

/// Reads `--name value` options and positional arguments.
fn parse_options<'a>(
    arguments: &'a [String],
    names: &[&str],
) -> (Vec<&'a str>, std::collections::BTreeMap<String, &'a str>) {
    let mut positional = Vec::new();
    let mut options = std::collections::BTreeMap::new();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        match names.iter().find(|name| argument == *name) {
            Some(name) => {
                if let Some(value) = rest.next() {
                    options.insert((*name).to_owned(), value.as_str());
                }
            }
            None => positional.push(argument.as_str()),
        }
    }
    (positional, options)
}

fn absolute(path: &str) -> crate::Result<PathBuf> {
    std::path::absolute(path).map_err(|error| crate::Error::Io(PathBuf::from(path), error))
}

fn assemble(arguments: &[String]) -> crate::Result<PathBuf> {
    let (positional, options) = parse_options(arguments, &["--include", "--libr", "--base"]);
    let (&[root], Some(include), Some(libr), Some(base)) = (
        positional.as_slice(),
        options.get("--include"),
        options.get("--libr"),
        options.get("--base"),
    ) else {
        return Err(crate::Error::Usage(
            "usage: ry-rcmd assemble ROOT --include DIR --libr FILE --base DIR".into(),
        ));
    };
    let rcmd = std::env::current_exe()
        .map_err(|error| crate::Error::Io(PathBuf::from("ry-rcmd"), error))?;
    let root = absolute(root)?;
    let base = absolute(base)?;
    crate::assemble(
        &root,
        &crate::HomeSources {
            include: &absolute(include)?,
            libr: &absolute(libr)?,
            rcmd: &rcmd,
            base: &base,
            toolchain: crate::Toolchain::for_base(&base)?,
        },
    )?;
    Ok(root)
}

fn build_base(arguments: &[String]) -> crate::Result<PathBuf> {
    let (positional, options) = parse_options(arguments, &["--include", "--libr", "--sha256"]);
    let (&[source, output], Some(include), Some(libr), Some(checksum)) = (
        positional.as_slice(),
        options.get("--include"),
        options.get("--libr"),
        options.get("--sha256"),
    ) else {
        return Err(crate::Error::Usage(
            "usage: ry-rcmd build-base SOURCE OUTPUT --include DIR --libr FILE --sha256 SUM"
                .into(),
        ));
    };
    let rcmd = std::env::current_exe()
        .map_err(|error| crate::Error::Io(PathBuf::from("ry-rcmd"), error))?;
    let output = absolute(output)?;
    crate::base::build(
        &crate::base::BuildSources {
            source: &absolute(source)?,
            libr: &absolute(libr)?,
            rcmd: &rcmd,
            include: &absolute(include)?,
            checksum,
        },
        &output,
    )?;
    Ok(output)
}

/// Runs a session on ry's runtime, the libR of the R home, which also holds the interpreter.
fn run_r(home: &Path, session: &crate::Session) -> crate::Result<i32> {
    let (file, expressions, arguments, default_packages, site_profile, user_profile) = match session
    {
        crate::Session::Print(text) => {
            print!("{text}");
            return Ok(0);
        }
        crate::Session::Version => {
            print!("{}", crate::version_text(home)?);
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
