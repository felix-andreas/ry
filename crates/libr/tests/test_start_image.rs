//! A start-up image that another build of the runtime wrote, or that has an older format, is
//! never loaded: a session on it starts the slow way, and assembling ry's R home writes the image
//! again, after which sessions start from the new image and evaluate correctly. A start from the
//! image that sets the environment's effects natively leaves the session as a slow start does.

#![cfg(unix)]

mod harness;

use {
    harness::*,
    std::{
        collections::BTreeSet,
        fs,
        path::{Path, PathBuf},
        process::Command,
    },
};

/// Where the image's header keeps the identity of the build that wrote it: after the format
/// stamp, the header's length and the identity's length.
const IDENTITY_OFFSET: usize = 24;

/// Whether a session records its image touches (`RY_IMAGE_TOUCH`), which tells a start from the
/// image apart from a slow one.
const RECORDS_TOUCHES: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

#[test]
fn a_stale_start_image_is_written_again_and_never_loaded() {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("stale-start-image");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).expect("remove an earlier run's directory");
    }
    fs::create_dir_all(&scratch).expect("create the test directory");
    let installed = rcmd::base::installed()
        .expect("ry's base R is not built; build it with `just base-r`, or set RY_BASE_R");
    let base = scratch.join("base-r");
    run(Command::new("cp").arg("-a").arg(&installed).arg(&base));
    let image = base.join("library/base/R/ry-start.image");
    let libr = library_path();
    let rcmd = ry_rcmd();
    rcmd::base::write_start_image(&base, &rcmd, &libr, None).expect("write the start-up image");
    let current = fs::read(&image).expect("read the start-up image");
    assert_eq!(&current[..8], interp::image::MAGIC);
    let identity_length = u64::from_le_bytes(
        current[16..IDENTITY_OFFSET]
            .try_into()
            .expect("the identity's length is eight bytes"),
    );
    let identity_end = IDENTITY_OFFSET + identity_length as usize;

    let stale_variants: [(&str, fn(&mut Vec<u8>)); 2] = [
        ("another build's", |bytes| bytes[IDENTITY_OFFSET] ^= 1),
        ("an older format's", |bytes| bytes[7] -= 1),
    ];
    for (round, (kind, make_stale)) in stale_variants.into_iter().enumerate() {
        let mut stale = current.clone();
        make_stale(&mut stale);
        fs::write(&image, &stale).expect("write the stale image");

        // Assembling a home rewrites the stale image, and its sessions start from the new one.
        let home = scratch.join(format!("home-{round}"));
        rcmd::assemble(
            &home,
            &rcmd::HomeSources {
                include: &Path::new(env!("CARGO_MANIFEST_DIR")).join("include"),
                libr: &libr,
                rcmd: &rcmd,
                base: &base,
                toolchain: rcmd::Toolchain::default(),
            },
        )
        .expect("assemble the R home");
        let rewritten = fs::read(&image).expect("read the rewritten image");
        assert_ne!(rewritten, stale, "{kind} image was not written again");
        assert_eq!(
            rewritten[..identity_end],
            current[..identity_end],
            "the image written over {kind} image has another stamp or identity"
        );
        let touches = scratch.join(format!("touches-{round}"));
        let (stdout, stderr) = rscript(&home, Some(&touches));
        assert_eq!(stdout, "55 TRUE \n", "stderr: {stderr}");
        assert_eq!(stderr, "");
        if RECORDS_TOUCHES {
            assert!(
                touches.is_file(),
                "the session over the rewritten image did not start from it"
            );
        }

        // A session on a stale image it was not assembled for starts without it and leaves it.
        fs::write(&image, &stale).expect("write the stale image again");
        let touches = scratch.join(format!("stale-touches-{round}"));
        let (stdout, stderr) = rscript(&home, Some(&touches));
        assert_eq!(stdout, "55 TRUE \n", "stderr: {stderr}");
        assert_eq!(stderr, "", "a session on {kind} image wrote to stderr");
        assert!(!touches.exists(), "a session loaded {kind} image");
        assert_eq!(fs::read(&image).expect("read the image"), stale);
    }
    fs::remove_dir_all(&scratch).expect("remove the test directory");
}

/// The session state base R's profile and the utils, grDevices and stats hooks set from the
/// environment, which a native start from the image sets without running them.
const STATE: &str = r#"
o <- .Options
for (n in names(o)) cat("option", n, "=", deparse(o[[n]]), "\n")
cat("libPaths", .libPaths(), "\n.Library", .Library, "\n.Library.site", .Library.site, "\n")
cat("search", search(), "\npopath", base:::.popath, "\nosVersion", utils::osVersion, "\n")
e <- Sys.getenv()
for (n in setdiff(names(e), c("_", "RY_IMAGE", "RY_NATIVE_START"))) cat("env", n, "=", e[[n]], "\n")
cat("global", ls(globalenv(), all.names = TRUE), "\nbase", ls(baseenv(), all.names = TRUE), "\n")
cat("Autoloads", ls(as.environment("Autoloads"), all.names = TRUE), "\n")
"#;

/// The values a session computes for its own process, which a start from the image must not
/// take from the process that wrote the image.
const PROCESS: &str = r#"
cat("pid", Sys.getpid(), "\n")
cat("time", format(as.numeric(Sys.time()), digits = 15), "\n")
cat("args", commandArgs(trailingOnly = TRUE), "\n")
cat("tempdir under TMPDIR", startsWith(tempdir(), normalizePath(Sys.getenv("TMPDIR"))), "\n")
cat("tempdir exists", dir.exists(tempdir()), "\n")
cat("interactive", interactive(), "\n")
cat("seed", exists(".Random.seed", globalenv()), "\n")
cat("options", sort(names(.Options)), "\n")
"#;

#[test]
fn a_native_start_sets_what_base_r_profile_and_hooks_set() {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("native-start");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).expect("remove an earlier run's directory");
    }
    fs::create_dir_all(&scratch).expect("create the test directory");
    let installed = rcmd::base::installed()
        .expect("ry's base R is not built; build it with `just base-r`, or set RY_BASE_R");
    let base = scratch.join("base-r");
    run(Command::new("cp").arg("-a").arg(&installed).arg(&base));
    let libr = library_path();
    let rcmd = ry_rcmd();
    rcmd::base::write_start_image(&base, &rcmd, &libr, None).expect("write the start-up image");
    let home = scratch.join("home");
    rcmd::assemble(
        &home,
        &rcmd::HomeSources {
            include: &Path::new(env!("CARGO_MANIFEST_DIR")).join("include"),
            libr: &libr,
            rcmd: &rcmd,
            base: &base,
            toolchain: rcmd::Toolchain::default(),
        },
    )
    .expect("assemble the R home");
    // Every variable the pinned code reads by name is in the native start's table. The image
    // write above fails when that code is not the pinned code.
    let deparsed: Vec<String> = interp::session::NATIVE_START_CLOSURES
        .iter()
        .map(|(package, name)| format!("deparse(get({name:?}, envir = asNamespace({package:?})))"))
        .collect();
    let output = Command::new(home.join("bin/Rscript"))
        .args([
            "-e",
            &format!("cat(c({}), sep = '\\n')", deparsed.join(", ")),
        ])
        .env_remove("R_HOME")
        .output()
        .expect("run Rscript");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = String::from_utf8_lossy(&output.stdout).into_owned()
        + &fs::read_to_string(base.join("library/base/R/Rprofile")).expect("read the profile");
    let read: BTreeSet<&str> = code
        .split("Sys.getenv(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    let listed: BTreeSet<&str> = interp::session::START_VARIABLES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(read, listed, "the variables the start-up code reads");

    let script = scratch.join("state.R");
    fs::write(&script, STATE).expect("write the script");
    let libraries = [scratch.join("one"), scratch.join("two/4.6")];
    for library in &libraries {
        fs::create_dir_all(library).expect("create a library");
    }
    let user_home = scratch.join("user");
    fs::create_dir_all(user_home.join(".R")).expect("create a home directory");
    let [one, two] = libraries.map(|library| library.display().to_string());
    let two = two.trim_end_matches("/4.6");

    // Each environment, and whether a start from the image sets it natively.
    let environments: Vec<(Vec<(&str, String)>, bool)> = vec![
        (vec![], true),
        (vec![("R_LIBS_USER", one.clone())], true),
        (
            vec![("R_LIBS_USER", format!("{two}/%v:%U:{one}%V%V"))],
            true,
        ),
        (
            vec![
                ("R_LIBS", format!("{two}/4.6:~/missing::{one}")),
                ("R_LIBS_SITE", "NULL".to_owned()),
            ],
            true,
        ),
        (
            vec![
                ("R_LIBS_USER", "NULL".to_owned()),
                ("R_LIBS_SITE", format!("{one}:%S")),
            ],
            true,
        ),
        (vec![("R_PAPERSIZE_USER", "letter".to_owned())], true),
        (
            vec![
                ("R_DEFAULT_DEVICE", "png".to_owned()),
                ("EDITOR", "nano".to_owned()),
                ("R_UNZIPCMD", "unzip -q".to_owned()),
                ("DVIPS", String::new()),
                ("R_BROWSER", "firefox".to_owned()),
            ],
            true,
        ),
        (
            vec![
                ("LANG", "en_US.ISO-8859-1".to_owned()),
                ("LC_ALL", "en_US.ISO-8859-1".to_owned()),
            ],
            true,
        ),
        (vec![("R_REPOSITORIES", "NULL".to_owned())], false),
        (vec![("R_DEFAULT_INTERNET_TIMEOUT", "5".to_owned())], false),
        (vec![("HOME", user_home.display().to_string())], true),
        (vec![("R_DEFAULT_PACKAGES", "NULL".to_owned())], false),
    ];
    for (variables, native) in &environments {
        let state = |mode: Option<&str>| {
            let mut command = Command::new(home.join("bin/Rscript"));
            command
                .arg(&script)
                .env_remove("R_HOME")
                .env_remove("RY_IMAGE")
                .env_remove("R_LIBS")
                .env_remove("R_LIBS_USER")
                .env_remove("R_LIBS_SITE")
                .env_remove("R_DEFAULT_PACKAGES")
                .envs(variables.iter().map(|(name, value)| (name, value)));
            match mode {
                Some(mode) => command.env("RY_NATIVE_START", mode),
                None => command.env("RY_IMAGE", "0"),
            };
            let output = command.output().expect("run Rscript");
            assert!(
                output.status.success(),
                "Rscript with {variables:?} and RY_NATIVE_START={mode:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let slow = state(None);
        let from_image = state(Some(if *native { "require" } else { "1" }));
        assert_eq!(
            from_image, slow,
            "a start from the image with {variables:?}"
        );
        if *native {
            assert_eq!(
                state(Some("0")),
                slow,
                "an R start from the image with {variables:?}"
            );
        }
    }

    // Nothing process-specific comes from the image: the writer's tempdir, pid, arguments,
    // start time and random seed never reach a session that starts from it.
    let process = scratch.join("process.R");
    fs::write(&process, PROCESS).expect("write the script");
    let mut native_pids = Vec::new();
    for run in ["first", "second"] {
        let temporary = scratch.join(format!("tmp-{run}"));
        fs::create_dir_all(&temporary).expect("create a temporary directory");
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("read the clock")
            .as_secs_f64()
            - 1.0;
        let facts = |mode: Option<&str>| {
            let mut command = Command::new(home.join("bin/Rscript"));
            command
                .arg(&process)
                .arg(run)
                .env_remove("R_HOME")
                .env_remove("RY_IMAGE")
                .env("TMPDIR", &temporary);
            match mode {
                Some(mode) => command.env("RY_NATIVE_START", mode),
                None => command.env("RY_IMAGE", "0"),
            };
            let output = command.output().expect("run Rscript");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut pid = None;
            let mut time = None;
            let mut rest = String::new();
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("pid ") {
                    pid = value.trim().parse::<u32>().ok();
                } else if let Some(value) = line.strip_prefix("time ") {
                    time = value.trim().parse::<f64>().ok();
                } else {
                    rest.push_str(line);
                    rest.push('\n');
                }
            }
            (
                pid.expect("the script prints its pid"),
                time.expect("the script prints the time"),
                rest,
            )
        };
        let (pid, time, native) = facts(Some("require"));
        let (_, _, slow) = facts(None);
        assert_eq!(native, slow, "process values in the {run} start");
        assert!(
            native.contains(&format!("args {run} \n"))
                && native.contains("tempdir under TMPDIR TRUE")
                && native.contains("interactive FALSE")
                && native.contains("seed FALSE"),
            "process values in the {run} start: {native}"
        );
        assert!(
            time >= started,
            "the start time {time} predates the run ({started})"
        );
        native_pids.push(pid);
    }
    assert_ne!(native_pids[0], native_pids[1], "two starts share a pid");

    // A user's repositories file is read by utils' own code.
    fs::write(
        user_home.join(".R/repositories"),
        "menu_name\tURL\tdefault\tsource\twin.binary\tmac.binary\n\
         X\tX\thttps://x.example\tTRUE\tTRUE\tFALSE\tFALSE\n",
    )
    .expect("write a repositories file");
    let output = Command::new(home.join("bin/Rscript"))
        .args(["-e", "cat(getOption('repos'), '\\n')"])
        .env_remove("R_HOME")
        .env_remove("RY_IMAGE")
        .env("HOME", &user_home)
        .output()
        .expect("run Rscript");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "@CRAN@ https://x.example \n",
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(&scratch).expect("remove the test directory");
}

/// Runs a script in the home's `Rscript` and returns its stdout and stderr. With `touches`, the
/// session records the image objects it touches there, which it does only when it started from
/// the image.
fn rscript(home: &Path, touches: Option<&Path>) -> (String, String) {
    let mut command = Command::new(home.join("bin/Rscript"));
    command
        .args(["-e", "cat(sum(1:10), is.function(stats::lm), '\\n')"])
        .env_remove("R_HOME")
        .env_remove("RY_IMAGE")
        .env_remove("R_LIBS")
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE");
    if let Some(touches) = touches {
        command.env("RY_IMAGE_TOUCH", touches);
    }
    let output = command.output().expect("run Rscript");
    assert!(
        output.status.success(),
        "Rscript failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// `ry-rcmd`, another package's binary, built here for the profile the test runs in.
fn ry_rcmd() -> PathBuf {
    let profile_directory = profile_directory();
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "-q", "-p", "rcmd", "--bin", "ry-rcmd"]);
    // Cargo names the dev profile's directory `debug`.
    match profile_directory.file_name().and_then(|name| name.to_str()) {
        Some("debug") | None => {}
        Some(profile) => {
            build.args(["--profile", profile]);
        }
    }
    run(&mut build);
    profile_directory.join("ry-rcmd")
}

fn run(command: &mut Command) {
    let status = command.status().expect("start the command");
    assert!(status.success(), "{command:?} failed");
}
