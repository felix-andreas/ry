//! Operating-system file notifications for the sources of a loaded package, so the console and
//! `ry test --watch` learn of an edit without walking the package's files.
//!
//! The notification thread only files relevant paths into an inbox; the caller drains it on its
//! own thread. [`SourceWatcher::take`] first writes a file into a private directory the same
//! watcher watches and waits for that file's notification. One watcher reports events in the
//! order they happened (on Linux and macOS), so every edit made before the call is in the inbox
//! by then, and what a reload sees depends only on the order of writes and calls, not on how
//! fast the notification thread runs.

use {
    crate::{is_dataset, is_r_code},
    notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher},
    std::{
        collections::BTreeSet,
        fs,
        path::{Component, Path, PathBuf},
        process,
        sync::{
            Arc, Condvar, Mutex, MutexGuard, PoisonError,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
};

/// What changed under a package root since the last drain.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Edits {
    /// Paths relative to the root, sorted.
    pub paths: Vec<PathBuf>,
    /// The operating system dropped notifications, so anything may have changed.
    pub lost: bool,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.lost
    }

    /// Whether a reload could see the change: code, data, compiled sources or the package's
    /// metadata, as opposed to tests and installed files, which a reload never reads.
    pub fn affects_load(&self) -> bool {
        self.lost
            || self.paths.iter().any(|path| {
                matches!(
                    first_component(path),
                    Some("R" | "data" | "src" | "DESCRIPTION" | "NAMESPACE")
                )
            })
    }
}

/// Watches `DESCRIPTION`, `NAMESPACE` and the directories in [`WATCHED`] under one root.
pub struct SourceWatcher {
    root: PathBuf,
    watcher: RecommendedWatcher,
    inbox: Arc<Inbox>,
    watched: Vec<&'static str>,
    /// The private directory of the barrier file, which [`SourceWatcher::take`] writes.
    barrier: PathBuf,
    barriers_sent: u64,
}

/// The directories under a root whose files can change what a load or a test run does.
const WATCHED: &[&str] = &["R", "data", "src", "inst", "tests"];

/// How long an edit must stay quiet before [`SourceWatcher::wait`] returns it, because an editor's
/// save is often several events (a temporary file, a rename, a metadata change).
const QUIET: Duration = Duration::from_millis(30);

/// The longest [`SourceWatcher::wait`] waits for quiet, so a stream of writes cannot starve it.
const SETTLE_LIMIT: Duration = Duration::from_millis(500);

/// How long [`SourceWatcher::take`] waits for its barrier before it reports lost notifications.
const BARRIER_LIMIT: Duration = Duration::from_secs(2);

/// Numbers the barrier directories of one process.
static BARRIERS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Inbox {
    edits: Mutex<Pending>,
    arrived: Condvar,
}

#[derive(Default)]
struct Pending {
    paths: BTreeSet<PathBuf>,
    lost: bool,
    /// Bumped by every relevant event, so a waiter can tell whether more arrived.
    events: u64,
    /// The number of the latest barrier file seen.
    barriers: u64,
}

impl SourceWatcher {
    pub fn new(root: &Path) -> notify::Result<Self> {
        let root = root.to_path_buf();
        let barrier = std::env::temp_dir().join(format!(
            "ry-watch-{}-{}",
            process::id(),
            BARRIERS.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&barrier)?;
        let inbox = Arc::new(Inbox::default());
        let handler_root = root.clone();
        let handler_barrier = barrier.clone();
        let handler_inbox = Arc::clone(&inbox);
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let mut pending = lock(&handler_inbox.edits);
            match event {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Access(_)) {
                        return;
                    }
                    let mut relevant = false;
                    for path in event.paths {
                        if path.starts_with(&handler_barrier) {
                            // A write makes several events, so a barrier counts by its number.
                            let number = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .and_then(|name| name.parse::<u64>().ok());
                            if let Some(number) = number {
                                pending.barriers = pending.barriers.max(number);
                                handler_inbox.arrived.notify_all();
                            }
                            continue;
                        }
                        let Ok(relative) = path.strip_prefix(&handler_root) else {
                            continue;
                        };
                        if is_relevant(relative) {
                            pending.paths.insert(relative.to_path_buf());
                            relevant = true;
                        }
                    }
                    if !relevant {
                        return;
                    }
                }
                Err(_) => pending.lost = true,
            }
            pending.events += 1;
            handler_inbox.arrived.notify_all();
        });
        let mut source = SourceWatcher {
            root,
            watcher: watcher?,
            inbox,
            watched: Vec::new(),
            barrier,
            barriers_sent: 0,
        };
        source
            .watcher
            .watch(&source.barrier, RecursiveMode::NonRecursive)?;
        source
            .watcher
            .watch(&source.root, RecursiveMode::NonRecursive)?;
        source.watch_directories()?;
        Ok(source)
    }

    /// Drains what changed before this call, without waiting for anything new.
    pub fn take(&mut self) -> notify::Result<Edits> {
        let previous = self.barrier.join(self.barriers_sent.to_string());
        self.barriers_sent += 1;
        fs::write(self.barrier.join(self.barriers_sent.to_string()), "")?;
        if previous.exists() {
            fs::remove_file(previous)?;
        }
        let deadline = Instant::now() + BARRIER_LIMIT;
        let mut pending = lock(&self.inbox.edits);
        while pending.barriers < self.barriers_sent {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                // The barrier never arrived, so neither may have an edit.
                pending.lost = true;
                pending.barriers = self.barriers_sent;
                break;
            };
            pending = wait_timeout(&self.inbox.arrived, pending, left);
        }
        let edits = drain(&mut pending);
        drop(pending);
        self.watch_directories()?;
        Ok(edits)
    }

    /// Waits up to `timeout` for an edit, then until the edit has been quiet for [`QUIET`], and
    /// drains it. Returns empty edits when the timeout passes first.
    pub fn wait(&mut self, timeout: Duration) -> notify::Result<Edits> {
        let mut pending = lock(&self.inbox.edits);
        let deadline = Instant::now() + timeout;
        while pending.paths.is_empty() && !pending.lost {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(Edits::default());
            };
            pending = wait_timeout(&self.inbox.arrived, pending, left);
        }
        let settle = Instant::now() + SETTLE_LIMIT;
        loop {
            let seen = pending.events;
            pending = wait_timeout(&self.inbox.arrived, pending, QUIET);
            if pending.events == seen || Instant::now() >= settle {
                break;
            }
        }
        let edits = drain(&mut pending);
        drop(pending);
        self.watch_directories()?;
        Ok(edits)
    }

    /// Adds a recursive watch on each directory of [`WATCHED`] that exists, so one created after
    /// the watcher (a first `src/`) is watched from the next drain on.
    fn watch_directories(&mut self) -> notify::Result<()> {
        for &directory in WATCHED {
            if self.watched.contains(&directory) {
                continue;
            }
            let path = self.root.join(directory);
            if path.is_dir() {
                self.watcher.watch(&path, RecursiveMode::Recursive)?;
                self.watched.push(directory);
            }
        }
        Ok(())
    }
}

impl Drop for SourceWatcher {
    fn drop(&mut self) {
        // Nothing can act on a failure here, and a leftover directory in the temporary
        // directory holds one small file.
        if fs::remove_dir_all(&self.barrier).is_err() {}
    }
}

/// Whether a change to `relative` (a path under the root) can change a load or a test run.
/// Editors' swap and backup files, and what a build or a test run writes itself (object files,
/// shared libraries, a Rust `target/`, testthat's `_snaps/`), are not, because reporting them
/// would rerun the work that wrote them.
fn is_relevant(relative: &Path) -> bool {
    let names: Vec<&str> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    let (Some(&first), Some(&name)) = (names.first(), names.last()) else {
        return false;
    };
    if names.iter().any(|name| name.starts_with('.'))
        || name.ends_with('~')
        || name.starts_with('#')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name == "4913"
    {
        return false;
    }
    let directory_itself = names.len() == 1;
    match first {
        "DESCRIPTION" | "NAMESPACE" => directory_itself,
        "R" => directory_itself || is_r_code(name) || name == "sysdata.rda",
        "data" => directory_itself || is_dataset(name),
        "src" => {
            let extension = Path::new(name)
                .extension()
                .and_then(|extension| extension.to_str());
            !names.contains(&"target")
                && !matches!(
                    extension,
                    Some("o" | "so" | "dll" | "dylib" | "a" | "def" | "rds")
                )
        }
        "inst" => true,
        "tests" => !names.contains(&"_snaps"),
        _ => false,
    }
}

fn drain(pending: &mut Pending) -> Edits {
    Edits {
        paths: std::mem::take(&mut pending.paths).into_iter().collect(),
        lost: std::mem::take(&mut pending.lost),
    }
}

fn first_component(path: &Path) -> Option<&str> {
    path.components()
        .next()
        .and_then(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
}

/// The notification thread only inserts into a set, so a panic there cannot leave the inbox
/// half-updated, and a poisoned lock is still safe to use.
fn lock(edits: &Mutex<Pending>) -> MutexGuard<'_, Pending> {
    edits.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wait_timeout<'a>(
    arrived: &Condvar,
    pending: MutexGuard<'a, Pending>,
    timeout: Duration,
) -> MutexGuard<'a, Pending> {
    arrived
        .wait_timeout(pending, timeout)
        .unwrap_or_else(PoisonError::into_inner)
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relevance_skips_build_outputs_snapshots_and_editor_files() {
        let relevant = |path: &str| is_relevant(Path::new(path));
        assert!(relevant("R/a.R"));
        assert!(relevant("R/unix/a.R"));
        assert!(relevant("DESCRIPTION"));
        assert!(relevant("src/code.c"));
        assert!(relevant("src/Makevars"));
        assert!(relevant("inst/extdata/table.csv"));
        assert!(relevant("tests/testthat/test-a.R"));
        assert!(!relevant("R/.a.R.swp"));
        assert!(!relevant("R/a.R~"));
        assert!(!relevant("R/notes.txt"));
        assert!(!relevant("src/code.o"));
        assert!(!relevant("src/demo.so"));
        assert!(!relevant("src/rust/target/debug/libdemo.a"));
        assert!(!relevant("tests/testthat/_snaps/a.md"));
        assert!(!relevant("README.md"));
        assert!(!relevant("man/a.Rd"));
    }
}
