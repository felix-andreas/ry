//! The files of start-up and code images, which are named by the runtime build that wrote them.
//!
//! An image loads only into the build that wrote it (`interp::image`, `interp::code_image`), so
//! its file name carries that build's identity (`RY_RUNTIME_ID`):
//!
//! | Image | File |
//! |---|---|
//! | start-up image of an R home or base R tree | `library/base/R/ry-start.<build>.image` |
//! | code image of the lazy-load database `R/<name>.rdb` | `R/<name>.<build>.ryc` |
//!
//! Builds that share a base R tree, such as a release and a nightly, or the two sides of a
//! benchmark, each find their own images there and never overwrite another's. A build that finds
//! no start-up image of its own writes its images ([`crate::base::write_start_image`]) and then
//! [`prune`]s the tree to the [`KEPT_BUILDS`] builds that wrote theirs most recently, as the
//! tree's ledger (`library/base/R/ry-builds`) records them, which bounds the disk a tree spends on
//! images. A process holds a shared lock on its build's start-up image while it runs
//! ([`hold_start_image`]), and pruning never removes a build whose image is locked.
//!
//! The unkeyed names an older ry used (`ry-start.image`, `R/<name>.ryc`) are left alone: only a
//! build that predates the build-keyed names reads or writes them.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, TryLockError},
    io,
    path::{Path, PathBuf},
};

/// How many builds' images a base R tree keeps: the build that writes its images and the three
/// others that wrote theirs most recently. A paired benchmark needs two, and a development
/// checkout rebuilds the runtime often enough to want a little more.
pub const KEPT_BUILDS: usize = 4;

const START_PREFIX: &str = "ry-start.";
const START_SUFFIX: &str = ".image";
const CODE_SUFFIX: &str = ".ryc";
/// The ledger of a base R tree: the builds whose images it holds, one identity per line, in the
/// order they last wrote them, oldest first.
const LEDGER: &str = "library/base/R/ry-builds";

/// The start-up image the runtime build `build` loads in the R home or base R tree `home`.
pub fn start_image(home: &Path, build: &str) -> PathBuf {
    home.join("library/base/R")
        .join(format!("{START_PREFIX}{build}{START_SUFFIX}"))
}

/// The code image the runtime build `build` loads for the lazy-load database `database`
/// (`R/<name>.rdb`): `R/<name>.<build>.ryc`.
pub fn code_image(database: &Path, build: &str) -> PathBuf {
    let database = database.as_os_str();
    let stem = database
        .to_str()
        .and_then(|text| text.strip_suffix(".rdb"))
        .map_or_else(|| database.to_owned(), OsString::from);
    let mut path = stem;
    path.push(format!(".{build}{CODE_SUFFIX}"));
    PathBuf::from(path)
}

/// Records in the ledger of the base R tree `tree` that `writer` has just written its images, then
/// removes the start-up and code images of every build but `writer`, the [`KEPT_BUILDS`] `- 1`
/// others the ledger lists as most recent, and any build a running process holds
/// ([`hold_start_image`]). Builds whose images are on disk but missing from the ledger, such as
/// those of a tree an older ry pruned, count as oldest, in the order of their names. A build's
/// images go together or stay together. Returns the removed files in a stable order.
///
/// The order comes from the ledger alone, never from file times, which copying a tree or
/// unpacking an archive resets. The ledger is replaced by renaming a complete file over it, and
/// callers hold the tree's image lock (`crate::rewrite_stale_image`), so a pruner and a writer
/// never interleave. A process of a pruned build that has its images mapped keeps reading them;
/// its next start finds none and writes them again.
pub fn prune(tree: &Path, writer: &str) -> io::Result<Vec<PathBuf>> {
    let mut builds: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (build, path) in image_files(tree)? {
        builds.entry(build).or_default().push(path);
    }
    let ledger = tree.join(LEDGER);
    let recorded = match fs::read_to_string(&ledger) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let recorded: Vec<&str> = recorded.lines().filter(|line| is_build(line)).collect();
    // Oldest first: unrecorded builds by name, then the ledger's order, then the writer.
    let mut order: Vec<&str> = builds
        .keys()
        .map(String::as_str)
        .filter(|build| *build != writer && !recorded.contains(build))
        .collect();
    order.extend(
        recorded
            .iter()
            .copied()
            .filter(|build| *build != writer && builds.contains_key(*build)),
    );
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    for (newer, build) in order.iter().rev().enumerate() {
        if newer < KEPT_BUILDS - 1 {
            kept.push(*build);
            continue;
        }
        // An exclusive lock on the start-up image fails while a process of the build runs, and
        // is held while the images go, so a start that opened the file finds it gone.
        let start = start_image(tree, build);
        let lock = match File::open(&start) {
            Ok(file) => match file.try_lock() {
                Ok(()) => Some(file),
                Err(TryLockError::WouldBlock) => {
                    kept.push(*build);
                    continue;
                }
                Err(TryLockError::Error(error)) => return Err(error),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        for path in builds.get(*build).into_iter().flatten() {
            match fs::remove_file(path) {
                Ok(()) => removed.push(path.clone()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        drop(lock);
    }
    let mut ledger_text: String = order
        .iter()
        .filter(|build| kept.contains(build))
        .map(|build| format!("{build}\n"))
        .collect();
    ledger_text.push_str(&format!("{writer}\n"));
    let staged = ledger.with_extension("partial");
    if let Some(parent) = ledger.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&staged, ledger_text)?;
    fs::rename(&staged, &ledger)?;
    removed.sort();
    Ok(removed)
}

/// Whether the lazy-load database `database` (`R/<name>.rdb`) has no code image of the runtime
/// build `build` but one of another build, or an unkeyed one of a ry that predates build-keyed
/// names: the package was installed by another build, and this one runs it without its compiled
/// code.
pub fn foreign_code_image(database: &Path, build: &str) -> io::Result<bool> {
    let (Some(directory), Some(stem)) = (
        database.parent(),
        database
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".rdb")),
    ) else {
        return Ok(false);
    };
    if code_image(database, build).is_file() {
        return Ok(false);
    }
    Ok(read_directory(directory)?.iter().any(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(stem)?.strip_suffix(CODE_SUFFIX))
            .is_some_and(|rest| rest.is_empty() || rest.strip_prefix('.').is_some_and(is_build))
    }))
}

/// The packages in the library `library` whose code image belongs to another build than `build`
/// ([`foreign_code_image`]), sorted by name.
pub fn foreign_packages(library: &Path, build: &str) -> io::Result<Vec<String>> {
    let mut packages = Vec::new();
    for package in read_directory(library)? {
        let Some(name) = package.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if foreign_code_image(&package.join("R").join(format!("{name}.rdb")), build)? {
            packages.push(name.to_owned());
        }
    }
    Ok(packages)
}

/// A shared lock on the start-up image at `image`, which keeps [`prune`] from removing its
/// build's images while the lock is held; `None` when there is no image there, or a pruner
/// removed or a writer replaced it before the lock was taken.
pub fn hold_start_image(image: &Path) -> io::Result<Option<File>> {
    let file = match File::open(image) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    file.lock_shared()?;
    let current = match fs::metadata(image) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Ok(None);
        }
    }
    #[cfg(not(unix))]
    drop(current);
    Ok(Some(file))
}

/// Every build-keyed image file under the base R tree `tree`, with the build that wrote it: the
/// start-up images in `library/base/R`, and in each package's `R/` the code images of its
/// lazy-load databases. A build identity has no dot, which tells `data.table.<build>.ryc`, an
/// image of `data.table.rdb`, from an image of a `data.rdb`, and an unkeyed image is named
/// exactly after its database, which tells `data.table.ryc` from an image of `data.rdb` by a
/// build named `table`.
fn image_files(tree: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    let library = tree.join("library");
    for package in read_directory(&library)? {
        let code = package.join("R");
        let names: Vec<String> = read_directory(&code)?
            .iter()
            .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
            .collect();
        let databases: Vec<&str> = names
            .iter()
            .filter_map(|name| name.strip_suffix(".rdb"))
            .collect();
        for name in &names {
            let start = (package.file_name().is_some_and(|name| name == "base"))
                .then(|| name.strip_prefix(START_PREFIX)?.strip_suffix(START_SUFFIX))
                .flatten();
            let code_build = || {
                let keyed = name.strip_suffix(CODE_SUFFIX)?;
                if databases.contains(&keyed) {
                    return None;
                }
                databases.iter().find_map(|database| {
                    keyed
                        .strip_prefix(database)?
                        .strip_prefix('.')
                        .filter(|build| is_build(build))
                })
            };
            if let Some(build) = start.filter(|build| is_build(build)).or_else(code_build) {
                files.push((build.to_owned(), code.join(name)));
            }
        }
    }
    Ok(files)
}

fn is_build(text: &str) -> bool {
    !text.is_empty() && !text.contains('.')
}

/// The entries of `directory`, sorted; none when it does not exist or is not a directory.
fn read_directory(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::collections::BTreeSet,
        syntax::testing::{SplitMix64, for_each_seed},
    };

    #[test]
    fn image_names_carry_the_build() {
        assert_eq!(
            start_image(Path::new("/home"), "0123abcd"),
            Path::new("/home/library/base/R/ry-start.0123abcd.image")
        );
        assert_eq!(
            code_image(Path::new("/lib/data.table/R/data.table.rdb"), "0123abcd"),
            Path::new("/lib/data.table/R/data.table.0123abcd.ryc")
        );
        assert_eq!(
            code_image(Path::new("/lib/MASS/R/sysdata.rdb"), "0123abcd"),
            Path::new("/lib/MASS/R/sysdata.0123abcd.ryc")
        );
    }

    #[test]
    fn a_package_with_only_another_builds_code_image_is_foreign() {
        let library = std::env::temp_dir().join(format!("ry-foreign-{}", std::process::id()));
        for (package, files) in [
            ("own", &["own.rdb", "own.aaaa.ryc", "own.bbbb.ryc"][..]),
            ("other", &["other.rdb", "other.bbbb.ryc"]),
            ("legacy", &["legacy.rdb", "legacy.ryc"]),
            ("plain", &["plain.rdb"]),
            ("data", &["data.rdb", "data.table.bbbb.ryc"]),
        ] {
            let code = library.join(package).join("R");
            fs::create_dir_all(&code).expect("simulated package");
            for file in files {
                File::create(code.join(file)).expect("simulated file");
            }
        }
        let foreign = foreign_packages(&library, "aaaa").expect("scan the library");
        fs::remove_dir_all(&library).expect("remove the simulated library");
        assert_eq!(foreign, ["legacy", "other"]);
    }

    /// A simulated base R tree holds the images of random builds, a ledger that records some of
    /// them in a random order, unkeyed images of an older ry and files that are no image at all,
    /// with random modification times. Some builds are running: the test holds a shared lock on
    /// their start-up images. Pruning on behalf of a random build keeps exactly that build, the
    /// [`KEPT_BUILDS`] `- 1` others the ledger orders newest (unrecorded builds counting as oldest,
    /// by name) and every running build; it removes the rest whole, touches nothing else, and
    /// leaves a ledger that lists the kept builds with the writer last.
    #[test]
    fn fuzz_prune_keeps_the_most_recent_and_running_builds_whole() {
        let root = std::env::temp_dir().join(format!("ry-prune-{}", std::process::id()));
        for_each_seed(60, |seed| {
            let tree = root.join(seed.to_string());
            if tree.exists() {
                fs::remove_dir_all(&tree).expect("clear the simulated tree");
            }
            let mut random = SplitMix64(seed);
            let simulated = simulate_tree(&tree, &mut random);
            let writer = simulated.builds[random.below(simulated.builds.len())].clone();
            let mut running: Vec<(String, File)> = Vec::new();
            for build in &simulated.builds {
                if random.chance(1, 4)
                    && let Some(held) = hold_start_image(&start_image(&tree, build)).expect("lock")
                {
                    running.push((build.clone(), held));
                }
            }
            let is_running = |build: &str| running.iter().any(|(held, _)| held == build);
            let wrote: Vec<&String> = simulated
                .builds
                .iter()
                .filter(|build| simulated.wrote(build))
                .collect();
            let mut order: Vec<&String> = wrote
                .iter()
                .copied()
                .filter(|build| **build != writer && !simulated.ledger.contains(build))
                .collect();
            order.sort();
            order.extend(
                simulated
                    .ledger
                    .iter()
                    .filter(|build| **build != writer && wrote.contains(build)),
            );
            let expected: BTreeSet<&String> = order
                .iter()
                .rev()
                .enumerate()
                .filter(|(newer, build)| *newer < KEPT_BUILDS - 1 || is_running(build))
                .map(|(_, build)| *build)
                .chain(simulated.wrote(&writer).then_some(&writer))
                .collect();
            let removed: BTreeSet<PathBuf> = prune(&tree, &writer)
                .expect("prune the simulated tree")
                .into_iter()
                .collect();

            for (build, path) in &simulated.images {
                assert_eq!(
                    path.exists(),
                    expected.contains(build),
                    "{build}: {} (writer {writer})",
                    path.display()
                );
                assert_eq!(removed.contains(path), !path.exists(), "{}", path.display());
            }
            for path in &simulated.others {
                assert!(
                    path.exists(),
                    "removed {}, which is no build's image",
                    path.display()
                );
            }
            let ledger = fs::read_to_string(tree.join(LEDGER)).expect("the ledger");
            let ledger: Vec<&str> = ledger.lines().collect();
            assert_eq!(ledger.last(), Some(&writer.as_str()));
            let listed: BTreeSet<&str> = ledger.iter().copied().collect();
            let kept: BTreeSet<&str> = expected
                .iter()
                .map(|build| build.as_str())
                .chain([writer.as_str()])
                .collect();
            assert_eq!(listed, kept, "the ledger lists exactly the kept builds");
            drop(running);
            fs::remove_dir_all(&tree).expect("remove the simulated tree");
        });
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove the simulation root");
        }
    }

    struct SimulatedTree {
        builds: Vec<String>,
        /// Each build-keyed image: its build and its path.
        images: Vec<(String, PathBuf)>,
        /// The builds the ledger records, oldest first.
        ledger: Vec<String>,
        /// The files that are no build's image.
        others: Vec<PathBuf>,
    }

    impl SimulatedTree {
        fn wrote(&self, build: &str) -> bool {
            self.images.iter().any(|(owner, _)| owner == build)
        }
    }

    fn simulate_tree(tree: &Path, random: &mut SplitMix64) -> SimulatedTree {
        let builds: Vec<String> = (0..1 + random.below(7))
            .map(|_| format!("{:016x}", random.next_u64()))
            .collect();
        let packages = [
            ("base", &["base"][..]),
            ("stats", &["stats"]),
            ("MASS", &["MASS", "sysdata"]),
            ("data", &["data", "data.table"]),
        ];
        let mut images = Vec::new();
        let mut others = Vec::new();
        for (package, databases) in packages {
            let code = tree.join("library").join(package).join("R");
            for database in databases {
                for extension in ["rdb", "rdx", "ryc"] {
                    others.push(code.join(format!("{database}.{extension}")));
                }
            }
            others.push(code.join("ry-start.image"));
            others.push(code.join(format!("{package}.{}.ryc.partial", builds[0])));
            for build in &builds {
                let mut paths = Vec::new();
                if package == "base" && random.chance(3, 4) {
                    paths.push(start_image(tree, build));
                }
                for database in databases {
                    if random.chance(1, 2) {
                        paths.push(code_image(&code.join(format!("{database}.rdb")), build));
                    }
                }
                for path in paths {
                    images.push((build.clone(), path));
                }
            }
        }
        for path in &others {
            fs::create_dir_all(path.parent().expect("a parent")).expect("simulated directory");
            fs::File::create(path).expect("simulated file");
        }
        for (_, path) in &images {
            fs::create_dir_all(path.parent().expect("a parent")).expect("simulated directory");
            let file = File::create(path).expect("simulated image");
            // File times play no part in the order, so they are random.
            let seconds = 1 + random.below(5) as u64;
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .expect("set the modification time");
        }
        // The ledger records some builds in a random order, and may name a build with no images
        // left or a line that is no build.
        let mut ledger: Vec<String> = builds
            .iter()
            .filter(|_| random.chance(2, 3))
            .cloned()
            .collect();
        for index in (1..ledger.len()).rev() {
            ledger.swap(index, random.below(index + 1));
        }
        let mut text: String = ledger.iter().map(|build| format!("{build}\n")).collect();
        if random.chance(1, 4) {
            text.push_str("not.a.build\n");
        }
        let path = tree.join(LEDGER);
        fs::create_dir_all(path.parent().expect("a parent")).expect("simulated directory");
        fs::write(&path, text).expect("simulated ledger");
        SimulatedTree {
            builds,
            images,
            ledger,
            others,
        }
    }
}
