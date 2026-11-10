//! `unlink()`: removes files and directory trees with the semantics of GNU R's `R_unlink`, but
//! takes the slow step off the R thread.
//!
//! On ext4, `rmdir` waits for the journal: it blocks for 1 to 4 ms while other writes are in
//! flight, against a few microseconds for removing a file or renaming a directory, and it uses
//! almost no CPU while it waits. testthat removes a temporary directory around each test, so
//! that wait is most of a test suite's I/O time on GNU R and ry alike.
//!
//! A recursive removal therefore runs in two parts. The R thread walks the tree with directory
//! descriptors and removes every non-directory entry, which decides the result exactly as GNU R
//! would. When every removal succeeded and each remaining directory is certain to be removable,
//! the empty skeleton is renamed into a holding directory beside the session's temporary
//! directory and a background thread removes it ([`Reaper`]). After `unlink()` returns the path
//! is gone, as it is on GNU R; only the disk blocks of the empty directories are freed later.
//! When anything is uncertain (a failure, a mount point inside the tree, the working directory
//! inside it, a directory this process may not write, a forked child), the directories are
//! removed on the R thread, children before parents, as GNU R removes them.

use crate::unwind::{R, Unwind, error};

/// Removes `path` as `unlink(path, recursive, force)` removes one already expanded path, and
/// says how many removals failed. A path that does not exist is not a failure.
pub fn unlink(path: &str, recursive: bool, force: bool) -> R<u32> {
    if path == "." || path == ".." {
        return Ok(0);
    }
    imp::unlink(path, recursive, force)
}

/// Waits for the background removals to finish and removes the holding directory. A session
/// calls it once, as it ends.
pub fn finish() {
    imp::finish();
}

/// The longest path GNU R builds while it walks a tree (`R_PATH_MAX`).
const PATH_MAX: usize = 4096;

fn path_too_long() -> Unwind {
    error("path too long")
}

#[cfg(unix)]
mod imp {
    use {
        super::{PATH_MAX, R, path_too_long},
        std::{
            ffi::{CStr, CString, OsStr},
            os::{
                fd::{AsRawFd, FromRawFd, OwnedFd},
                unix::ffi::OsStrExt,
            },
            path::{Path, PathBuf},
            sync::{Mutex, mpsc},
            thread::JoinHandle,
        },
    };

    pub fn unlink(path: &str, recursive: bool, force: bool) -> R<u32> {
        let Ok(name) = CString::new(path) else {
            return Ok(0);
        };
        // GNU R reads the entry itself, not what a symbolic link points to, so a link to a
        // directory is removed as a link.
        let Some(status) = lstat(&name) else {
            return Ok(0);
        };
        if force {
            make_writable(libc::AT_FDCWD, &name, status.st_mode, libc::S_IWUSR);
        }
        if recursive && directory_like(status.st_mode) {
            return remove_tree(&name, &status, force);
        }
        // SAFETY: `name` is NUL-terminated.
        Ok(u32::from(unsafe { libc::unlink(name.as_ptr()) } != 0))
    }

    /// GNU R tests `st_mode & S_IFDIR` rather than the file type, so sockets and block devices,
    /// whose type codes share that bit, are walked as directories: opening them fails and so does
    /// `rmdir`, and they stay. Removal must match that to give the same result.
    fn directory_like(mode: libc::mode_t) -> bool {
        mode & libc::S_IFDIR != 0
    }

    fn is_directory(mode: libc::mode_t) -> bool {
        mode & libc::S_IFMT == libc::S_IFDIR
    }

    struct Walk {
        force: bool,
        failures: u32,
        /// The subdirectories, children before parents, in the order GNU R removes them; read
        /// only when the skeleton is removed on the R thread.
        directories: Vec<CString>,
        /// Whether the emptied skeleton can be handed to the reaper with the same result as
        /// removing it here.
        deferrable: bool,
        device: libc::dev_t,
        working_directory: Option<(libc::dev_t, libc::ino_t)>,
    }

    fn remove_tree(path: &CStr, status: &libc::stat, force: bool) -> R<u32> {
        let mut walk = Walk {
            force,
            failures: 0,
            directories: Vec::new(),
            deferrable: is_directory(status.st_mode) && reaper_available(),
            device: status.st_dev,
            working_directory: None,
        };
        if walk.deferrable {
            walk.working_directory =
                lstat(c".").map(|current| (current.st_dev, current.st_ino));
        }
        match open_directory(libc::AT_FDCWD, path) {
            Some(directory) => {
                walk.visit(directory.as_raw_fd(), status);
                walk.directory(path, directory)?;
            }
            None => {
                walk.failures += 1;
                walk.deferrable = false;
            }
        }
        if walk.failures == 0 && walk.deferrable && defer(path) {
            return Ok(0);
        }
        for directory in &walk.directories {
            walk.failures += rmdir(directory);
        }
        Ok(walk.failures + rmdir(path))
    }

    impl Walk {
        /// Checks that removing the open directory `fd`, once it is empty, cannot fail and needs
        /// no special order: it is on the tree's file system and is not the working directory.
        fn visit(&mut self, fd: libc::c_int, status: &libc::stat) {
            if !self.deferrable {
                return;
            }
            let status = match fstat(fd) {
                Some(status) => status,
                None => *status,
            };
            if status.st_dev != self.device
                || self.working_directory == Some((status.st_dev, status.st_ino))
            {
                self.deferrable = false;
            }
        }

        /// Removes everything below the directory `path`, open as `directory`.
        fn directory(&mut self, path: &CStr, directory: OwnedFd) -> R<()> {
            let fd = directory.as_raw_fd();
            // SAFETY: `fdopendir` takes ownership of the descriptor, which `closedir` closes.
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                self.failures += 1;
                self.deferrable = false;
                return Ok(());
            }
            std::mem::forget(directory);
            let walked = self.entries(path, stream);
            // SAFETY: `stream` came from `fdopendir` and is closed once.
            unsafe { libc::closedir(stream) };
            walked
        }

        fn entries(&mut self, path: &CStr, stream: *mut libc::DIR) -> R<()> {
            // SAFETY: `stream` is an open directory stream.
            let fd = unsafe { libc::dirfd(stream) };
            let mut holds_directories = false;
            loop {
                // SAFETY: as above; the entry stays valid until the next `readdir`.
                let entry = unsafe { libc::readdir(stream) };
                if entry.is_null() {
                    break;
                }
                // SAFETY: `d_name` is NUL-terminated.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                let bytes = name.to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                let full = join(path, bytes)?;
                // SAFETY: as above.
                let kind = unsafe { (*entry).d_type };
                // The entry's type tells most entries apart without a `stat`; `force` needs the
                // mode, and some file systems do not report the type.
                let mode = if self.force || kind == libc::DT_UNKNOWN {
                    // GNU R keeps the previous entry's mode when `lstat` fails; the entry is
                    // gone then, so its removal fails either way.
                    fstatat(fd, name).map_or(0, |status| status.st_mode)
                } else {
                    match kind {
                        libc::DT_DIR => libc::S_IFDIR,
                        libc::DT_SOCK => libc::S_IFSOCK,
                        libc::DT_BLK => libc::S_IFBLK,
                        _ => libc::S_IFREG,
                    }
                };
                if directory_like(mode) {
                    holds_directories = true;
                    if self.force {
                        make_writable(fd, name, mode, libc::S_IWUSR | libc::S_IXUSR);
                    }
                    match is_directory(mode)
                        .then(|| open_directory(fd, name))
                        .flatten()
                    {
                        Some(child) => {
                            let status = fstat(child.as_raw_fd());
                            match status {
                                Some(status) => self.visit(child.as_raw_fd(), &status),
                                None => self.deferrable = false,
                            }
                            self.directory(&full, child)?;
                        }
                        None => {
                            self.failures += 1;
                            self.deferrable = false;
                        }
                    }
                    self.directories.push(full);
                } else {
                    if self.force {
                        make_writable(fd, name, mode, libc::S_IWUSR);
                    }
                    // SAFETY: `fd` is open and `name` is NUL-terminated.
                    if unsafe { libc::unlinkat(fd, name.as_ptr(), 0) } != 0 {
                        self.failures += 1;
                    }
                }
            }
            if holds_directories && self.deferrable && !may_remove_entries(fd) {
                self.deferrable = false;
            }
            Ok(())
        }
    }

    /// Whether this process can remove any empty subdirectory of the open directory `fd`: it may
    /// write and search it, the sticky bit (which limits removal to an entry's owner) is clear,
    /// and it is not immutable or append-only.
    fn may_remove_entries(fd: libc::c_int) -> bool {
        // SAFETY: `fd` is open and the path is NUL-terminated.
        let writable = unsafe {
            libc::faccessat(fd, c".".as_ptr(), libc::W_OK | libc::X_OK, libc::AT_EACCESS)
        } == 0;
        let Some(status) = fstat(fd) else {
            return false;
        };
        writable && status.st_mode & libc::S_ISVTX == 0 && !protected(fd)
    }

    #[cfg(target_os = "linux")]
    fn protected(fd: libc::c_int) -> bool {
        const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
        const FS_IMMUTABLE_FL: libc::c_long = 0x10;
        const FS_APPEND_FL: libc::c_long = 0x20;
        let mut flags: libc::c_long = 0;
        // SAFETY: the request writes one `long` into `flags`. File systems without inode flags
        // answer ENOTTY, and then nothing is protected.
        let answered = unsafe { libc::ioctl(fd, FS_IOC_GETFLAGS, &mut flags) } == 0;
        answered && flags & (FS_IMMUTABLE_FL | FS_APPEND_FL) != 0
    }

    #[cfg(not(target_os = "linux"))]
    fn protected(_fd: libc::c_int) -> bool {
        false
    }

    fn join(path: &CStr, name: &[u8]) -> R<CString> {
        let parent = path.to_bytes();
        let mut full = Vec::with_capacity(parent.len() + name.len() + 2);
        full.extend_from_slice(parent);
        // GNU R tests the byte past the end of the parent, never a separator, so a parent that
        // ends in '/' gets a second one; the kernel reads both alike.
        full.push(b'/');
        full.extend_from_slice(name);
        if full.len() >= PATH_MAX {
            return Err(path_too_long());
        }
        CString::new(full).map_err(|_| path_too_long())
    }

    fn lstat(path: &CStr) -> Option<libc::stat> {
        // SAFETY: `status` is written by a successful `lstat` before it is read.
        unsafe {
            let mut status: libc::stat = std::mem::zeroed();
            (libc::lstat(path.as_ptr(), &mut status) == 0).then_some(status)
        }
    }

    fn fstat(fd: libc::c_int) -> Option<libc::stat> {
        // SAFETY: as in `lstat`.
        unsafe {
            let mut status: libc::stat = std::mem::zeroed();
            (libc::fstat(fd, &mut status) == 0).then_some(status)
        }
    }

    fn fstatat(fd: libc::c_int, name: &CStr) -> Option<libc::stat> {
        // SAFETY: as in `lstat`.
        unsafe {
            let mut status: libc::stat = std::mem::zeroed();
            (libc::fstatat(fd, name.as_ptr(), &mut status, libc::AT_SYMLINK_NOFOLLOW) == 0)
                .then_some(status)
        }
    }

    fn open_directory(at: libc::c_int, path: &CStr) -> Option<OwnedFd> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `path` is NUL-terminated; a non-negative result is a descriptor we own.
        let fd = unsafe { libc::openat(at, path.as_ptr(), flags) };
        // SAFETY: as above.
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// `force = TRUE` adds the permission bits `bits` before removing, as GNU R does with
    /// `chmod`, which follows a symbolic link and changes its target. The outcome is not
    /// checked, because the removal that follows reports any failure that matters.
    fn make_writable(at: libc::c_int, name: &CStr, mode: libc::mode_t, bits: libc::mode_t) {
        // SAFETY: `name` is NUL-terminated.
        unsafe { libc::fchmodat(at, name.as_ptr(), (mode | bits) & 0o7777, 0) };
    }

    fn rmdir(path: &CStr) -> u32 {
        // SAFETY: `path` is NUL-terminated.
        u32::from(unsafe { libc::rmdir(path.as_ptr()) } != 0)
    }

    /// The background removal of emptied directory skeletons.
    struct Reaper {
        /// The process that started the thread. A forked child inherits this record but not the
        /// thread, so it removes everything itself.
        owner: u32,
        /// Where skeletons wait for removal: a directory beside the session's temporary
        /// directory, so a rename into it stays on the same file system for the paths tests
        /// remove, and nothing in `tempdir()` changes.
        holding: PathBuf,
        next: u64,
        sender: mpsc::Sender<PathBuf>,
        thread: JoinHandle<()>,
    }

    /// `None` until the first deferred removal, then the reaper, or `Err` when it cannot start
    /// (no temporary directory, no thread), in which case every removal runs on the R thread.
    static REAPER: Mutex<Option<Result<Reaper, ()>>> = Mutex::new(None);

    fn reaper_available() -> bool {
        let mut reaper = lock();
        let reaper = reaper.get_or_insert_with(start_reaper);
        matches!(reaper, Ok(reaper) if reaper.owner == std::process::id())
    }

    fn start_reaper() -> Result<Reaper, ()> {
        let temp = crate::system::session_temp_dir().map_err(|_| ())?;
        let parent = Path::new(temp).parent().ok_or(())?;
        let template = parent.join("ry-unlink-XXXXXX");
        let holding =
            crate::platform::make_temp_dir(&template.to_string_lossy()).map_err(|_| ())?;
        let holding = PathBuf::from(holding);
        let (sender, receiver) = mpsc::channel::<PathBuf>();
        let thread = std::thread::Builder::new()
            .name("ry-unlink".into())
            .spawn(move || {
                for skeleton in receiver {
                    if let Err(failure) = std::fs::remove_dir_all(&skeleton) {
                        eprintln!(
                            "ry: cannot remove the emptied directory '{}': {failure}",
                            skeleton.display()
                        );
                    }
                }
            });
        let Ok(thread) = thread else {
            if let Err(failure) = std::fs::remove_dir(&holding) {
                eprintln!("ry: cannot remove '{}': {failure}", holding.display());
            }
            return Err(());
        };
        Ok(Reaper {
            owner: std::process::id(),
            holding,
            next: 0,
            sender,
            thread,
        })
    }

    /// Moves the emptied tree at `path` out of the way for the reaper; false when the rename
    /// fails (another file system, no permission to rewrite the directory's `..`), and the
    /// caller removes the tree itself.
    fn defer(path: &CStr) -> bool {
        let mut reaper = lock();
        let Some(Ok(reaper)) = reaper.as_mut() else {
            return false;
        };
        let target = reaper.holding.join(reaper.next.to_string());
        reaper.next += 1;
        let source = Path::new(OsStr::from_bytes(path.to_bytes()));
        if std::fs::rename(source, &target).is_err() {
            return false;
        }
        if let Err(mpsc::SendError(target)) = reaper.sender.send(target)
            && let Err(failure) = std::fs::remove_dir_all(&target)
        {
            eprintln!(
                "ry: cannot remove the emptied directory '{}': {failure}",
                target.display()
            );
        }
        true
    }

    pub fn finish() {
        let reaper = match lock().take() {
            Some(Ok(reaper)) => reaper,
            _ => return,
        };
        if reaper.owner != std::process::id() {
            return;
        }
        drop(reaper.sender);
        if reaper.thread.join().is_err() {
            eprintln!("ry: the background removal of emptied directories panicked");
        }
        if let Err(failure) = std::fs::remove_dir_all(&reaper.holding) {
            eprintln!("ry: cannot remove '{}': {failure}", reaper.holding.display());
        }
    }

    fn lock() -> std::sync::MutexGuard<'static, Option<Result<Reaper, ()>>> {
        match REAPER.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Windows has no `rmdir` wait to hide, so removal runs on the R thread with GNU R's results:
/// a directory needs `recursive`, and a missing path is not a failure.
#[cfg(not(unix))]
mod imp {
    use super::R;

    pub fn unlink(path: &str, recursive: bool, force: bool) -> R<u32> {
        let target = std::path::Path::new(path);
        let Ok(metadata) = std::fs::symlink_metadata(target) else {
            return Ok(0);
        };
        if force && metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            if std::fs::set_permissions(target, permissions).is_err() {
                return Ok(1);
            }
        }
        let removed = if metadata.is_dir() {
            if !recursive {
                return Ok(1);
            }
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        Ok(u32::from(removed.is_err()))
    }

    pub fn finish() {}
}
