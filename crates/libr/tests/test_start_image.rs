//! A start-up image that another build of the runtime wrote, or that has an older format, is
//! never loaded: a session on it starts the slow way, and assembling ry's R home writes the image
//! again, after which sessions start from the new image and evaluate correctly.

#![cfg(unix)]

mod harness;

use {
    harness::*,
    std::{
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
    let mut build = syntax::testing::cargo_build(&profile_directory);
    build.args(["-p", "rcmd", "--bin", "ry-rcmd"]);
    run(&mut build);
    profile_directory.join("ry-rcmd")
}

fn run(command: &mut Command) {
    let status = command.status().expect("start the command");
    assert!(status.success(), "{command:?} failed");
}
