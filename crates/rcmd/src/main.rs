//! `ry-rcmd`: what `bin/R` and `bin/Rscript` in ry's R home run. `ry-rcmd CMD INSTALL|SHLIB|config`
//! and `ry-rcmd RHOME` are answered in Rust; `ry-rcmd R ...` and `ry-rcmd Rscript ...` run R
//! code on ry's interpreter, taking GNU R's command lines. The R home is the directory `R_HOME`
//! names. `ry-rcmd assemble ROOT --include DIR --libr FILE [--gnu-r-home DIR]` creates such a
//! home.

use std::{
    ffi::{CString, c_char, c_int},
    path::{Path, PathBuf},
    process::ExitCode,
};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
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
    let Some(home) = std::env::var_os("R_HOME").map(PathBuf::from) else {
        eprintln!("ry-rcmd: R_HOME is not set; run it through bin/R of an R home that `rcmd::assemble` created");
        return ExitCode::from(2);
    };
    let result = match arguments.split_first() {
        Some((front, rest)) if front == "R" => rcmd::r_command_line(rest).and_then(|session| run_r(&home, &session)),
        Some((front, rest)) if front == "Rscript" => {
            rcmd::rscript_command_line(rest).and_then(|session| run_r(&home, &session))
        }
        _ => rcmd::run(&home, &arguments).map(|output| {
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

fn assemble(arguments: &[String]) -> rcmd::Result<PathBuf> {
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
    let usage = || rcmd::Error::Usage("usage: ry-rcmd assemble ROOT --include DIR --libr FILE [--gnu-r-home DIR]".into());
    let (Some(root), Some(include), Some(libr)) = (root, include, libr) else {
        return Err(usage());
    };
    let gnu_r_home = match gnu_r_home {
        Some(home) => home,
        None => {
            let output = std::process::Command::new("R")
                .arg("RHOME")
                .output()
                .map_err(|error| rcmd::Error::Usage(format!("cannot run `R RHOME` to find GNU R: {error}")))?;
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        }
    };
    let rcmd = std::env::current_exe().map_err(|error| rcmd::Error::Io(PathBuf::from("ry-rcmd"), error))?;
    let absolute = |path: PathBuf| std::path::absolute(&path).map_err(|error| rcmd::Error::Io(path, error));
    let root = absolute(root)?;
    rcmd::assemble(
        &root,
        &rcmd::HomeSources {
            include: &absolute(include)?,
            libr: &absolute(libr)?,
            rcmd: &rcmd,
            gnu_r_home: &gnu_r_home,
            toolchain: rcmd::Toolchain::default(),
        },
    )?;
    Ok(root)
}

/// Runs a session on ry's runtime, the `libR.so` of the R home, which also holds the
/// interpreter.
fn run_r(home: &Path, session: &rcmd::Session) -> rcmd::Result<i32> {
    let (file, expressions, arguments, default_packages, site_profile, user_profile) = match session {
        rcmd::Session::Print(text) => {
            print!("{text}");
            return Ok(0);
        }
        rcmd::Session::Run {
            file,
            expressions,
            arguments,
            default_packages,
            site_profile,
            user_profile,
        } => (file, expressions, arguments, default_packages, *site_profile, *user_profile),
    };
    if let Some(packages) = default_packages {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("R_DEFAULT_PACKAGES", packages) };
    }
    // With neither a file nor `-e`, `ry_run` reads stdin as a console, one expression at a time.
    let input = expressions.clone();
    let library_path = home.join("lib/libR.so");
    // Compiled packages record `libR.so` as a needed library; loading ry's globally lets the
    // dynamic linker satisfy them from it.
    let library = unsafe {
        libloading::os::unix::Library::open(
            Some(&library_path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
        )
    }
    .map_err(|error| rcmd::Error::Usage(format!("cannot load ry's runtime {}: {error}", library_path.display())))?;
    type Run = unsafe extern "C" fn(*const c_char, *const c_char, c_int, *const *const c_char, c_int) -> c_int;
    let run = unsafe { library.get::<Run>(b"ry_run\0") }
        .map(|symbol| *symbol)
        .map_err(|error| rcmd::Error::Usage(format!("{} is not ry's runtime: {error}", library_path.display())))?;
    let text = |value: &str| {
        CString::new(value).map_err(|_| rcmd::Error::Usage("an argument contains a NUL byte".into()))
    };
    let file = file.as_deref().map(text).transpose()?;
    let input = input.as_deref().map(text).transpose()?;
    let arguments = arguments.iter().map(|argument| text(argument)).collect::<rcmd::Result<Vec<_>>>()?;
    let pointers: Vec<*const c_char> = arguments.iter().map(|argument| argument.as_ptr()).collect();
    let pointer = |value: &Option<CString>| value.as_ref().map_or(std::ptr::null(), |value| value.as_ptr());
    // Inline annotations are checked and the object systems are on, as in `ry run`'s defaults;
    // the profiles follow the command line. The bits are libr's `RUN_FLAG_*` constants.
    let flags = 0b11 | if site_profile { 1 << 2 } else { 0 } | if user_profile { 1 << 3 } else { 0 };
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
