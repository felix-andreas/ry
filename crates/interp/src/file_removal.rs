//! `unlink()`: removes files and directory trees with the semantics of GNU R's `R_unlink`.
//!
//! The walk works relative to open directory descriptors (`openat`, `readdir`, `unlinkat`), so
//! the kernel never resolves a full path again, and it reads each entry's type from `readdir`,
//! so only `force` or a file system that does not report the type costs a `stat`. Each
//! subdirectory is removed as soon as it is empty, children before parents, as GNU R removes
//! them.

use crate::unwind::{R, Unwind, error};

/// Removes `path` as `unlink(path, recursive, force)` removes one already expanded path, and
/// says how many removals failed. A path that does not exist is not a failure.
pub fn unlink(path: &str, recursive: bool, force: bool) -> R<u32> {
    if path == "." || path == ".." {
        return Ok(0);
    }
    imp::unlink(path, recursive, force)
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
            ffi::{CStr, CString},
            os::fd::{AsRawFd, FromRawFd, OwnedFd},
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
            let mut failures = match open_directory(libc::AT_FDCWD, &name) {
                Some(directory) => remove_entries(directory, name.as_bytes().len(), force)?,
                None => 1,
            };
            // SAFETY: `name` is NUL-terminated.
            failures += u32::from(unsafe { libc::rmdir(name.as_ptr()) } != 0);
            return Ok(failures);
        }
        // SAFETY: as above.
        Ok(u32::from(unsafe { libc::unlink(name.as_ptr()) } != 0))
    }

    /// GNU R tests `st_mode & S_IFDIR` rather than the file type, so sockets and block devices,
    /// whose type codes share that bit, are walked as directories: opening them fails and so does
    /// `rmdir`, and they stay. Removal must match that to give the same result.
    fn directory_like(mode: libc::mode_t) -> bool {
        mode & libc::S_IFDIR != 0
    }

    /// Removes everything in the open directory `directory`, whose path is `path_length` bytes
    /// long, and says how many removals failed.
    fn remove_entries(directory: OwnedFd, path_length: usize, force: bool) -> R<u32> {
        // SAFETY: `fdopendir` takes ownership of the descriptor, which `closedir` closes.
        let stream = unsafe { libc::fdopendir(directory.as_raw_fd()) };
        if stream.is_null() {
            return Ok(1);
        }
        std::mem::forget(directory);
        let removed = remove_stream_entries(stream, path_length, force);
        // SAFETY: `stream` came from `fdopendir` and is closed once.
        unsafe { libc::closedir(stream) };
        removed
    }

    fn remove_stream_entries(stream: *mut libc::DIR, path_length: usize, force: bool) -> R<u32> {
        // SAFETY: `stream` is an open directory stream.
        let fd = unsafe { libc::dirfd(stream) };
        let mut failures = 0;
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
            // GNU R joins the parent and the name with one '/' (a second one when the parent
            // ends in '/') into a buffer of `R_PATH_MAX` bytes.
            let length = path_length + 1 + bytes.len();
            if length >= PATH_MAX {
                return Err(path_too_long());
            }
            // SAFETY: as above.
            let kind = unsafe { (*entry).d_type };
            let mode = if force || kind == libc::DT_UNKNOWN {
                // GNU R keeps the previous entry's mode when `lstat` fails; the entry is gone
                // then, so its removal fails either way.
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
                if force {
                    make_writable(fd, name, mode, libc::S_IWUSR | libc::S_IXUSR);
                }
                failures += match open_directory(fd, name) {
                    Some(child) => remove_entries(child, length, force)?,
                    None => 1,
                };
                // SAFETY: `fd` is open and `name` is NUL-terminated.
                failures += u32::from(
                    unsafe { libc::unlinkat(fd, name.as_ptr(), libc::AT_REMOVEDIR) } != 0,
                );
            } else {
                if force {
                    make_writable(fd, name, mode, libc::S_IWUSR);
                }
                // SAFETY: as above.
                failures += u32::from(unsafe { libc::unlinkat(fd, name.as_ptr(), 0) } != 0);
            }
        }
        Ok(failures)
    }

    fn lstat(path: &CStr) -> Option<libc::stat> {
        // SAFETY: `status` is written by a successful `lstat` before it is read.
        unsafe {
            let mut status: libc::stat = std::mem::zeroed();
            (libc::lstat(path.as_ptr(), &mut status) == 0).then_some(status)
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
}

/// Windows removes with the standard library and GNU R's results: a directory needs
/// `recursive`, and a missing path is not a failure.
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
}
