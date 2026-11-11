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
//! [`prune`]s the tree to the [`KEPT_BUILDS`] builds that wrote theirs most recently, which
//! bounds the disk a tree spends on images.
//!
//! The unkeyed names an older ry used (`ry-start.image`, `R/<name>.ryc`) are left alone: only a
//! build that predates the build-keyed names reads or writes them.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// How many builds' images a base R tree keeps: the build that writes its images and the three
/// others that wrote theirs most recently. A paired benchmark needs two, and a development
/// checkout rebuilds the runtime often enough to want a little more.
pub const KEPT_BUILDS: usize = 4;

const START_PREFIX: &str = "ry-start.";
const START_SUFFIX: &str = ".image";
const CODE_SUFFIX: &str = ".ryc";

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

/// Removes from the base R tree `tree` the start-up and code images of every build but `writer`
/// and the [`KEPT_BUILDS`] `- 1` others whose newest image file is the most recent, and returns
/// the removed files in a stable order. A build's images go together or stay together. Ties in
/// recency fall to the build's name, so two pruners agree.
///
/// Callers hold the tree's image lock (`crate::rewrite_stale_image`), because a pruner and a
/// writer of the same build must not interleave. A process of a pruned build that has its images
/// mapped keeps reading them; its next start finds none and writes them again.
pub fn prune(tree: &Path, writer: &str) -> io::Result<Vec<PathBuf>> {
    let mut builds: BTreeMap<String, (SystemTime, Vec<PathBuf>)> = BTreeMap::new();
    for (build, path) in image_files(tree)? {
        let modified = fs::metadata(&path)?.modified()?;
        let (newest, files) = builds
            .entry(build)
            .or_insert_with(|| (SystemTime::UNIX_EPOCH, Vec::new()));
        *newest = (*newest).max(modified);
        files.push(path);
    }
    let mut others: Vec<(&String, &SystemTime)> = builds
        .iter()
        .filter(|(build, _)| build.as_str() != writer)
        .map(|(build, (newest, _))| (build, newest))
        .collect();
    others.sort_by(|(left, left_time), (right, right_time)| {
        right_time.cmp(left_time).then_with(|| left.cmp(right))
    });
    let doomed: Vec<String> = others
        .into_iter()
        .skip(KEPT_BUILDS - 1)
        .map(|(build, _)| build.clone())
        .collect();
    let mut removed = Vec::new();
    for build in doomed {
        let Some((_, files)) = builds.remove(&build) else {
            continue;
        };
        for path in files {
            match fs::remove_file(&path) {
                Ok(()) => removed.push(path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    removed.sort();
    Ok(removed)
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

    /// A simulated base R tree holds the images of random builds, written at random times, beside
    /// unkeyed images of an older ry and files that are no image at all. Pruning on behalf of a
    /// random build keeps that build's images and those of the builds that wrote most recently,
    /// up to [`KEPT_BUILDS`], removes the rest whole, and touches nothing else.
    #[test]
    fn fuzz_prune_keeps_the_most_recent_builds_whole() {
        let root = std::env::temp_dir().join(format!("ry-prune-{}", std::process::id()));
        for_each_seed(60, |seed| {
            let tree = root.join(seed.to_string());
            if tree.exists() {
                fs::remove_dir_all(&tree).expect("clear the simulated tree");
            }
            let mut random = SplitMix64(seed);
            let simulated = simulate_tree(&tree, &mut random);
            let writer = simulated.builds[random.below(simulated.builds.len())].clone();
            let removed: BTreeSet<PathBuf> = prune(&tree, &writer)
                .expect("prune the simulated tree")
                .into_iter()
                .collect();

            let survivors: BTreeSet<&String> = simulated
                .images
                .iter()
                .filter(|(_, path, _)| path.exists())
                .map(|(build, _, _)| build)
                .collect();
            for (build, path, _) in &simulated.images {
                let whole = survivors.contains(build) == path.exists();
                assert!(
                    whole,
                    "{build} kept only some of its images: {}",
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
            assert!(survivors.contains(&writer) || !simulated.wrote(&writer));
            assert!(survivors.len() <= KEPT_BUILDS);
            let newest = |build: &String| simulated.newest(build);
            let oldest_kept = survivors
                .iter()
                .filter(|build| **build != &writer)
                .map(|build| newest(build))
                .min();
            for build in &simulated.builds {
                if !survivors.contains(build) && simulated.wrote(build) {
                    assert!(build != &writer);
                    assert!(
                        oldest_kept.is_none_or(|oldest| newest(build) <= oldest),
                        "pruned {build}, which wrote more recently than a build that was kept"
                    );
                    let kept_others = survivors.iter().filter(|kept| ***kept != writer).count();
                    assert_eq!(
                        kept_others,
                        KEPT_BUILDS - 1,
                        "pruned {build} with room to keep it"
                    );
                }
            }
            fs::remove_dir_all(&tree).expect("remove the simulated tree");
        });
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove the simulation root");
        }
    }

    struct SimulatedTree {
        builds: Vec<String>,
        /// Each build-keyed image: its build, its path, and when it was written.
        images: Vec<(String, PathBuf, SystemTime)>,
        /// The files that are no build's image.
        others: Vec<PathBuf>,
    }

    impl SimulatedTree {
        fn wrote(&self, build: &str) -> bool {
            self.images.iter().any(|(owner, _, _)| owner == build)
        }

        fn newest(&self, build: &str) -> SystemTime {
            self.images
                .iter()
                .filter(|(owner, _, _)| owner == build)
                .map(|(_, _, time)| *time)
                .max()
                .unwrap_or(SystemTime::UNIX_EPOCH)
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
                    // Few distinct times, so builds tie.
                    let seconds = 1 + random.below(5) as u64;
                    let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
                    images.push((build.clone(), path, modified));
                }
            }
        }
        for path in &others {
            fs::create_dir_all(path.parent().expect("a parent")).expect("simulated directory");
            fs::File::create(path).expect("simulated file");
        }
        for (_, path, modified) in &images {
            fs::create_dir_all(path.parent().expect("a parent")).expect("simulated directory");
            let file = fs::File::create(path).expect("simulated image");
            file.set_modified(*modified)
                .expect("set the modification time");
        }
        SimulatedTree {
            builds,
            images,
            others,
        }
    }
}
