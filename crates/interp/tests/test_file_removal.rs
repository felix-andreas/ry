//! `unlink()` against a direct port of GNU R's `R_unlink` on generated trees.
//!
//! Each seed builds the same random tree twice, in two sandboxes: files, nested and read-only
//! directories, symbolic links into and out of the tree, dangling links, FIFOs and Unix sockets
//! (which GNU R walks as directories, because it tests `st_mode & S_IFDIR`). One copy is removed by
//! the reference below, the other by `interp::file_removal::unlink`. Both must agree on whether
//! the call failed and on everything left in the sandbox, including what symbolic links point to
//! and the modes `force` changed.
#![cfg(unix)]

use {
    interp::file_removal,
    std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        path::{Path, PathBuf},
    },
    syntax::testing::{SplitMix64, for_each_seed},
};

#[test]
fn unlink_matches_gnu_r_on_generated_trees() {
    let scratch = std::env::temp_dir().join(format!("ry-unlink-fuzz-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create the scratch directory");
    let home = std::env::current_dir().expect("read the working directory");
    for_each_seed(200, |seed| {
        let reference = scratch.join(format!("{seed}-reference"));
        let actual = scratch.join(format!("{seed}-actual"));
        let plan = Plan::generate(&mut SplitMix64(seed));
        plan.build(&reference);
        plan.build(&actual);
        let expected = plan.run(&reference, &home, |path, recursive, force| {
            gnu_r_unlink(path, recursive, force)
        });
        let got = plan.run(&actual, &home, |path, recursive, force| {
            file_removal::unlink(path.to_str().expect("UTF-8 path"), recursive, force)
                .expect("no path is too long")
        });
        let left_reference = snapshot(&reference);
        let left_actual = snapshot(&actual);
        unlock(&reference);
        unlock(&actual);
        fs::remove_dir_all(&reference).expect("remove the reference sandbox");
        fs::remove_dir_all(&actual).expect("remove the sandbox");
        assert_eq!(
            expected != 0,
            got != 0,
            "seed {seed}: GNU R fails {expected} times, ry {got} ({plan:?})"
        );
        assert_eq!(left_reference, left_actual, "seed {seed}: {plan:?}");
    });
    fs::remove_dir_all(&scratch).expect("remove the scratch directory");
}

#[derive(Debug)]
enum Entry {
    File { read_only: bool },
    Directory { read_only: bool },
    /// A link to another entry of the sandbox, by its path relative to the sandbox.
    Link(String),
    Fifo,
    Socket,
}

#[derive(Debug)]
struct Plan {
    /// Entries relative to the sandbox, parents before children. `tree` is the directory the
    /// call removes most often; `outside` holds link targets the removal must not touch.
    entries: Vec<(String, Entry)>,
    target: String,
    recursive: bool,
    force: bool,
    /// The working directory during the call, relative to the sandbox.
    working_directory: Option<String>,
}

impl Plan {
    fn generate(random: &mut SplitMix64) -> Plan {
        let mut entries = vec![
            ("outside".to_owned(), Entry::Directory { read_only: false }),
            (
                "outside/kept".to_owned(),
                Entry::File { read_only: false },
            ),
            ("tree".to_owned(), Entry::Directory { read_only: false }),
        ];
        let mut directories = vec!["tree".to_owned()];
        for index in 0..random.below(14) {
            let parent = directories[random.below(directories.len())].clone();
            let path = format!("{parent}/e{index}");
            let entry = match random.below(10) {
                0..=3 => Entry::File {
                    read_only: random.below(5) == 0,
                },
                4..=6 => {
                    directories.push(path.clone());
                    Entry::Directory { read_only: false }
                }
                7 => {
                    let targets = ["outside", "outside/kept", "tree", "missing", "tree/e0"];
                    Entry::Link(targets[random.below(targets.len())].to_owned())
                }
                8 => Entry::Fifo,
                _ => Entry::Socket,
            };
            entries.push((path, entry));
        }
        // Read-only directories are marked last, so their children could be created.
        for (path, entry) in &mut entries {
            if let Entry::Directory { read_only } = entry
                && path.starts_with("tree/")
            {
                *read_only = random.below(6) == 0;
            }
        }
        let targets: Vec<&String> = entries.iter().map(|(path, _)| path).collect();
        let target = match random.below(4) {
            0 => targets[random.below(targets.len())].clone(),
            1 => "missing".to_owned(),
            _ => "tree".to_owned(),
        };
        let working_directory = (random.below(5) == 0)
            .then(|| directories[random.below(directories.len())].clone());
        Plan {
            entries,
            target,
            recursive: random.below(5) != 0,
            force: random.below(3) == 0,
            working_directory,
        }
    }

    fn build(&self, sandbox: &Path) {
        fs::create_dir_all(sandbox).expect("create the sandbox");
        let mut read_only = Vec::new();
        for (path, entry) in &self.entries {
            let full = sandbox.join(path);
            match entry {
                Entry::File { read_only: locked } => {
                    fs::write(&full, path).expect("write a file");
                    if *locked {
                        set_mode(&full, 0o444);
                    }
                }
                Entry::Directory { read_only: locked } => {
                    fs::create_dir(&full).expect("create a directory");
                    if *locked {
                        read_only.push(full);
                    }
                }
                Entry::Link(target) => symlink(sandbox.join(target), &full).expect("link"),
                Entry::Fifo => {
                    let name = std::ffi::CString::new(full.as_os_str().as_encoded_bytes())
                        .expect("no NUL in the path");
                    // SAFETY: `name` is NUL-terminated.
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o644) }, 0, "mkfifo");
                }
                Entry::Socket => {
                    // The listener closes at once; the socket file stays.
                    std::os::unix::net::UnixListener::bind(&full).expect("bind a socket");
                }
            }
        }
        for directory in read_only.iter().rev() {
            set_mode(directory, 0o555);
        }
    }

    fn run(&self, sandbox: &Path, home: &Path, unlink: impl Fn(&Path, bool, bool) -> u32) -> u32 {
        if let Some(directory) = &self.working_directory {
            std::env::set_current_dir(sandbox.join(directory)).expect("enter the directory");
        }
        let failures = unlink(&sandbox.join(&self.target), self.recursive, self.force);
        std::env::set_current_dir(home).expect("leave the directory");
        failures
    }
}

/// GNU R's `R_unlink` (`src/main/platform.c`), with its quirks: the `S_IFDIR` bit test and
/// `chmod`, which follows symbolic links.
fn gnu_r_unlink(name: &Path, recursive: bool, force: bool) -> u32 {
    let Ok(metadata) = fs::symlink_metadata(name) else {
        return 0;
    };
    let mode = metadata.mode();
    if force {
        chmod(name, mode | 0o200);
    }
    if recursive && mode & libc::S_IFDIR != 0 {
        let mut failures = 0;
        match fs::read_dir(name) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.expect("read an entry").path();
                    let mode = fs::symlink_metadata(&path).map_or(0, |metadata| metadata.mode());
                    if mode & libc::S_IFDIR != 0 {
                        if force {
                            chmod(&path, mode | 0o300);
                        }
                        failures += gnu_r_unlink(&path, recursive, force);
                    } else {
                        if force {
                            chmod(&path, mode | 0o200);
                        }
                        failures += u32::from(fs::remove_file(&path).is_err());
                    }
                }
            }
            Err(_) => failures += 1,
        }
        return failures + u32::from(fs::remove_dir(name).is_err());
    }
    u32::from(fs::remove_file(name).is_err())
}

fn chmod(path: &Path, mode: u32) {
    // Like GNU R's `chmod`, this follows a link, and its failure is not reported.
    if let Err(failure) = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777)) {
        assert!(
            failure.kind() == std::io::ErrorKind::NotFound
                || failure.kind() == std::io::ErrorKind::PermissionDenied
                || failure.raw_os_error() == Some(libc::ELOOP),
            "chmod {}: {failure}",
            path.display()
        );
    }
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set a mode");
}

/// Everything under `sandbox`: each entry's type, mode and, for a link, its target.
fn snapshot(sandbox: &Path) -> BTreeMap<PathBuf, String> {
    let mut found = BTreeMap::new();
    let mut pending = vec![sandbox.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("read an entry").path();
            let metadata = fs::symlink_metadata(&path).expect("stat an entry");
            let relative = path.strip_prefix(sandbox).expect("inside").to_path_buf();
            let link = fs::read_link(&path)
                .map(|target| {
                    target
                        .strip_prefix(sandbox)
                        .unwrap_or(&target)
                        .display()
                        .to_string()
                })
                .unwrap_or_default();
            found.insert(relative, format!("{:o} {link}", metadata.mode()));
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    found
}

/// Makes every directory in the sandbox writable again so it can be cleaned up.
fn unlock(sandbox: &Path) {
    let mut pending = vec![sandbox.to_path_buf()];
    while let Some(directory) = pending.pop() {
        set_mode(&directory, 0o755);
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("read an entry").path();
            if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                pending.push(path);
            }
        }
    }
}
