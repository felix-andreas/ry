//! What differs between operating systems, behind one interface: CPU times, file modes, links,
//! globbing, processes, dynamic libraries, `errno` and character set conversion. Callers stay
//! free of `cfg`; each function has a Unix and a Windows implementation of the same contract.
//!
//! On Windows, R's C library conventions come from mingw-w64, which is the toolchain Rtools
//! builds packages with, and the conversion behind `iconv` is ry's own (`PortableIconv`), since
//! Windows has no `iconv(3)`.

pub use imp::*;
use std::path::Path;

/// CPU time in seconds that the process and its waited-for children have used.
pub struct CpuTimes {
    pub user: f64,
    pub system: f64,
    /// `None` where the platform does not report children's time (Windows).
    pub children: Option<(f64, f64)>,
}

/// What `glob` found: matches in pattern order, each pattern's own matches sorted.
pub struct GlobMatches {
    pub paths: Vec<String>,
    /// A directory could not be read while matching.
    pub read_error: bool,
}

/// The `uname` fields `Sys.info()` reports.
pub struct SystemName {
    pub sysname: String,
    pub release: String,
    pub version: String,
    pub nodename: String,
    pub machine: String,
    pub user: String,
}

/// Why a conversion step stopped before using up its input, as `iconv(3)` reports through
/// `errno`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconvStop {
    /// `E2BIG`: the output buffer is full.
    OutputFull,
    /// `EILSEQ`: the input holds a sequence that is invalid or has no target representation.
    Invalid,
    /// `EINVAL`: the input ends inside a multibyte sequence.
    Incomplete,
}

impl IconvStop {
    /// The `errno` value `iconv(3)` sets for this stop.
    pub fn errno(self) -> i32 {
        match self {
            IconvStop::OutputFull => libc::E2BIG,
            IconvStop::Invalid => libc::EILSEQ,
            IconvStop::Incomplete => libc::EINVAL,
        }
    }
}

/// Whether a file counts as executable when Windows reports a mode: by its extension, as GNU R
/// does there.
#[cfg_attr(unix, allow(dead_code))]
fn windows_executable(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["exe", "com", "cmd", "bat"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// A resolved path as R shows it. Windows' `canonicalize` returns a verbatim path (`\\?\C:\...`
/// or `\\?\UNC\server\...`), which GNU R never produces and many path functions mishandle, so
/// the prefix goes back to the ordinary drive or UNC form.
pub fn path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = text.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        text.into_owned()
    }
}

#[cfg(unix)]
mod imp {
    #[cfg(target_os = "linux")]
    use libc::__errno_location as errno_location;
    #[cfg(target_os = "macos")]
    use libc::__error as errno_location;
    use {
        super::{CpuTimes, GlobMatches, IconvStop, SystemName},
        std::{
            ffi::{CStr, CString, c_char, c_void},
            fs::Metadata,
            io,
            os::unix::fs::PermissionsExt,
            path::Path,
        },
    };

    pub fn cpu_times() -> CpuTimes {
        let times = |who: libc::c_int| {
            // SAFETY: `getrusage` fills the zeroed struct it is given.
            let usage = unsafe {
                let mut usage: libc::rusage = std::mem::zeroed();
                libc::getrusage(who, &mut usage);
                usage
            };
            let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
            (seconds(usage.ru_utime), seconds(usage.ru_stime))
        };
        let (user, system) = times(libc::RUSAGE_SELF);
        CpuTimes {
            user,
            system,
            children: Some(times(libc::RUSAGE_CHILDREN)),
        }
    }

    pub fn file_mode(_path: &Path, metadata: &Metadata) -> u32 {
        metadata.permissions().mode() & 0o7777
    }

    pub fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    pub fn symlink(from: &Path, to: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(from, to)
    }

    pub fn system_name() -> SystemName {
        // SAFETY: `uname` fills the zeroed struct it is given.
        let name = unsafe {
            let mut name: libc::utsname = std::mem::zeroed();
            libc::uname(&mut name);
            name
        };
        let field = |raw: &[c_char]| -> String {
            let bytes: Vec<u8> = raw
                .iter()
                .take_while(|&&byte| byte != 0)
                .map(|&byte| byte as u8)
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        SystemName {
            sysname: field(&name.sysname),
            release: field(&name.release),
            version: field(&name.version),
            nodename: field(&name.nodename),
            machine: field(&name.machine),
            user: std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
        }
    }

    /// Expands each pattern with the C library's `glob(3)`, marking directories with a trailing
    /// slash when `mark` is set. `Err` is the out-of-memory condition `glob` reports.
    pub fn glob(patterns: &[String], mark: bool) -> Result<GlobMatches, ()> {
        // SAFETY: `buffer` is zeroed before the first call, extended by each call with
        // GLOB_APPEND, and freed once on every path out.
        unsafe {
            let mut buffer: libc::glob_t = std::mem::zeroed();
            let mut appended = false;
            let mut read_error = false;
            for pattern in patterns {
                let Ok(pattern) = CString::new(pattern.as_str()) else {
                    continue;
                };
                let mut flags = if mark { libc::GLOB_MARK } else { 0 };
                if appended {
                    flags |= libc::GLOB_APPEND;
                }
                match libc::glob(pattern.as_ptr(), flags, None, &mut buffer) {
                    0 => appended = true,
                    libc::GLOB_NOSPACE => {
                        libc::globfree(&mut buffer);
                        return Err(());
                    }
                    libc::GLOB_ABORTED => read_error = true,
                    _ => {}
                }
            }
            let paths = if appended {
                (0..buffer.gl_pathc)
                    .map(|index| {
                        CStr::from_ptr(*buffer.gl_pathv.add(index))
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect()
            } else {
                Vec::new()
            };
            libc::globfree(&mut buffer);
            Ok(GlobMatches { paths, read_error })
        }
    }

    /// The home directory from the password database, of `user` or of the current user.
    pub fn home_directory(user: Option<&[u8]>) -> Option<Vec<u8>> {
        // SAFETY: the password database entry is copied out before anything else can
        // overwrite it.
        unsafe {
            let entry = match user {
                Some(user) => {
                    let user = CString::new(user).ok()?;
                    libc::getpwnam(user.as_ptr())
                }
                None => libc::getpwuid(libc::getuid()),
            };
            if entry.is_null() || (*entry).pw_dir.is_null() {
                return None;
            }
            Some(CStr::from_ptr((*entry).pw_dir).to_bytes().to_vec())
        }
    }

    /// Sends `signal` to the process; whether that worked.
    pub fn kill(pid: i32, signal: i32) -> bool {
        // SAFETY: `kill` has no memory preconditions.
        unsafe { libc::kill(pid, signal) == 0 }
    }

    /// The platform's number for a signal tools names by its BSD number.
    pub fn signal_number(bsd: i32) -> Option<i32> {
        Some(match bsd {
            1 => libc::SIGHUP,
            2 => libc::SIGINT,
            3 => libc::SIGQUIT,
            9 => libc::SIGKILL,
            15 => libc::SIGTERM,
            17 => libc::SIGSTOP,
            18 => libc::SIGTSTP,
            19 => libc::SIGCONT,
            20 => libc::SIGCHLD,
            30 => libc::SIGUSR1,
            31 => libc::SIGUSR2,
            _ => return None,
        })
    }

    /// The process's niceness, after setting it to `value` when given; `None` when the process
    /// cannot be queried.
    pub fn priority(pid: i32, value: Option<i32>) -> Option<i32> {
        // SAFETY: getpriority may return -1 legitimately, so success is told apart through
        // errno, which belongs to this thread.
        unsafe {
            *errno_location() = 0;
            let current = libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t);
            if current == -1 && *errno_location() != 0 {
                return None;
            }
            if let Some(value) = value {
                libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, value);
            }
            Some(current)
        }
    }

    pub fn set_errno(code: i32) {
        // SAFETY: errno belongs to this thread.
        unsafe { *errno_location() = code }
    }

    /// Opens a shared library, making its symbols available to libraries loaded later when
    /// `global` is set. The library stays loaded for the life of the process.
    pub fn open_library(path: &str, global: bool) -> Result<*mut c_void, String> {
        let name = CString::new(path).map_err(|_| format!("invalid library name {path}"))?;
        let visibility = if global {
            libc::RTLD_GLOBAL
        } else {
            libc::RTLD_LOCAL
        };
        // SAFETY: `name` is a valid NUL-terminated string.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | visibility) };
        if handle.is_null() {
            Err(last_library_error())
        } else {
            Ok(handle)
        }
    }

    /// A handle that finds symbols in the executable and every library loaded so far.
    pub fn process_library() -> *mut c_void {
        // SAFETY: a null name asks for the main program's handle.
        unsafe { libc::dlopen(std::ptr::null(), libc::RTLD_NOW) }
    }

    /// The address of `name` in the library, or null.
    ///
    /// # Safety
    ///
    /// `handle` came from `open_library` or `process_library`.
    pub unsafe fn library_symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY: the caller passes a live handle; `name` is NUL-terminated.
        unsafe { libc::dlsym(handle, name.as_ptr()) }
    }

    /// The file of the library that holds `address`.
    pub fn library_path(address: *const c_void) -> Option<String> {
        // SAFETY: dladdr fills the zeroed struct and leaves `dli_fname` pointing into the
        // loader's own storage.
        unsafe {
            let mut info: libc::Dl_info = std::mem::zeroed();
            if libc::dladdr(address, &mut info) == 0 || info.dli_fname.is_null() {
                return None;
            }
            Some(
                CStr::from_ptr(info.dli_fname)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    /// The loader's message for the last failed `dlopen`.
    pub fn last_library_error() -> String {
        // SAFETY: dlerror returns null or a string the loader owns until the next call.
        unsafe {
            let message = libc::dlerror();
            if message.is_null() {
                String::new()
            } else {
                CStr::from_ptr(message).to_string_lossy().into_owned()
            }
        }
    }

    /// An `iconv(3)` conversion descriptor, closed on drop.
    pub struct Iconv(libc::iconv_t);

    impl Iconv {
        pub fn open(from: &str, to: &str) -> Option<Iconv> {
            let from = CString::new(from).ok()?;
            let to = CString::new(to).ok()?;
            // SAFETY: both names are valid NUL-terminated strings.
            let descriptor = unsafe { libc::iconv_open(to.as_ptr(), from.as_ptr()) };
            if descriptor as isize == -1 {
                None
            } else {
                Some(Iconv(descriptor))
            }
        }

        /// Converts from the start of `input` into `output`, advancing `input` past what was
        /// used. Returns how many bytes were written and why it stopped early, if it did.
        pub fn convert(
            &mut self,
            input: &mut &[u8],
            output: &mut [u8],
        ) -> (usize, Result<(), IconvStop>) {
            let mut input_pointer = input.as_ptr() as *mut c_char;
            let mut input_left = input.len();
            let mut output_pointer = output.as_mut_ptr() as *mut c_char;
            let mut output_left = output.len();
            // SAFETY: the pointers and lengths describe `input` and `output`, which outlive
            // the call; iconv only reads the input.
            let status = unsafe {
                libc::iconv(
                    self.0,
                    &mut input_pointer,
                    &mut input_left,
                    &mut output_pointer,
                    &mut output_left,
                )
            };
            let error = io::Error::last_os_error().raw_os_error();
            *input = &input[input.len() - input_left..];
            let written = output.len() - output_left;
            if status != usize::MAX {
                return (written, Ok(()));
            }
            let stop = match error {
                Some(libc::E2BIG) => IconvStop::OutputFull,
                Some(libc::EINVAL) => IconvStop::Incomplete,
                _ => IconvStop::Invalid,
            };
            (written, Err(stop))
        }

        /// Writes the sequence a stateful target needs to return to its initial state, and
        /// resets the descriptor.
        pub fn finish(&mut self, output: &mut [u8]) -> (usize, Result<(), IconvStop>) {
            let mut output_pointer = output.as_mut_ptr() as *mut c_char;
            let mut output_left = output.len();
            // SAFETY: a null input asks for the final shift sequence; the output pointer and
            // length describe `output`.
            let status = unsafe {
                libc::iconv(
                    self.0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut output_pointer,
                    &mut output_left,
                )
            };
            let written = output.len() - output_left;
            if status == usize::MAX {
                (written, Err(IconvStop::OutputFull))
            } else {
                (written, Ok(()))
            }
        }

        /// Returns the descriptor to its initial shift state without writing anything.
        pub fn reset(&mut self) {
            // SAFETY: a null input and output reset the descriptor's shift state.
            unsafe {
                libc::iconv(
                    self.0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
        }
    }

    impl Drop for Iconv {
        fn drop(&mut self) {
            // SAFETY: the descriptor came from a successful iconv_open and is closed once.
            unsafe {
                libc::iconv_close(self.0);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    pub use super::PortableIconv as Iconv;
    use {
        super::{CpuTimes, GlobMatches, SystemName, windows_executable},
        std::{
            ffi::{CStr, c_void},
            fs::Metadata,
            io,
            os::windows::ffi::{OsStrExt, OsStringExt},
            path::Path,
        },
        windows_sys::Win32::{
            Foundation::{CloseHandle, FILETIME, GetLastError, HMODULE},
            System::{
                Diagnostics::Debug::{
                    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
                },
                LibraryLoader::{
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
                    GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
                    LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
                },
                Threading::{
                    GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
                    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
                },
            },
        },
    };

    unsafe extern "C" {
        /// The C runtime's per-thread `errno`.
        fn _errno() -> *mut i32;
    }

    pub fn cpu_times() -> CpuTimes {
        let seconds = |time: FILETIME| {
            ((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)) as f64 / 1e7
        };
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the pseudo handle of the current process needs no closing, and each out
        // parameter is a valid FILETIME.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            );
        }
        CpuTimes {
            user: seconds(user),
            system: seconds(kernel),
            children: None,
        }
    }

    /// The mode GNU R reports on Windows: read and write for everyone unless the file is
    /// read-only, plus execute for directories and executables.
    pub fn file_mode(path: &Path, metadata: &Metadata) -> u32 {
        let mut mode = if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        };
        if metadata.is_dir() || windows_executable(path) {
            mode |= 0o111;
        }
        mode
    }

    /// Windows keeps only a read-only flag, which the owner's write bit sets.
    pub fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_readonly(mode & 0o200 == 0);
        std::fs::set_permissions(path, permissions)
    }

    pub fn symlink(from: &Path, to: &Path) -> io::Result<()> {
        if from.is_dir() {
            std::os::windows::fs::symlink_dir(from, to)
        } else {
            std::os::windows::fs::symlink_file(from, to)
        }
    }

    pub fn system_name() -> SystemName {
        let machine = match std::env::consts::ARCH {
            "x86_64" => "x86-64",
            other => other,
        };
        SystemName {
            sysname: "Windows".into(),
            release: "10 x64".into(),
            version: String::new(),
            nodename: std::env::var("COMPUTERNAME").unwrap_or_default(),
            machine: machine.into(),
            user: std::env::var("USERNAME").unwrap_or_else(|_| "unknown".into()),
        }
    }

    /// Expands each pattern with the `glob` crate, writing forward slashes as GNU R does on
    /// Windows.
    pub fn glob(patterns: &[String], mark: bool) -> Result<GlobMatches, ()> {
        let mut paths = Vec::new();
        let mut read_error = false;
        for pattern in patterns {
            let Ok(entries) = glob::glob(pattern) else {
                continue;
            };
            for entry in entries {
                match entry {
                    Ok(path) => {
                        let mut text = path.to_string_lossy().replace('\\', "/");
                        if mark && path.is_dir() && !text.ends_with('/') {
                            text.push('/');
                        }
                        paths.push(text);
                    }
                    Err(_) => read_error = true,
                }
            }
        }
        Ok(GlobMatches { paths, read_error })
    }

    /// The current user's profile directory; another user's home is not known.
    pub fn home_directory(user: Option<&[u8]>) -> Option<Vec<u8>> {
        match user {
            Some(_) => None,
            None => std::env::var("USERPROFILE")
                .ok()
                .map(|home| home.replace('\\', "/").into_bytes()),
        }
    }

    /// Windows has no signals between processes: signal 0 asks whether the process is alive,
    /// and any other signal terminates it, as GNU R's `pskill` does there.
    pub fn kill(pid: i32, signal: i32) -> bool {
        let access = if signal == 0 {
            PROCESS_QUERY_LIMITED_INFORMATION
        } else {
            PROCESS_TERMINATE
        };
        // SAFETY: the handle is checked before use and closed once.
        unsafe {
            let process = OpenProcess(access, 0, pid as u32);
            if process.is_null() {
                return false;
            }
            let succeeded = if signal == 0 {
                let mut code = 0;
                GetExitCodeProcess(process, &mut code) != 0
            } else {
                TerminateProcess(process, 1) != 0
            };
            CloseHandle(process);
            succeeded
        }
    }

    /// mingw's `signal.h` defines only the C standard's signals, of which tools names SIGINT
    /// and SIGTERM.
    pub fn signal_number(bsd: i32) -> Option<i32> {
        matches!(bsd, 2 | 15).then_some(bsd)
    }

    /// Windows has priority classes rather than niceness, so there is nothing to report.
    pub fn priority(_pid: i32, _value: Option<i32>) -> Option<i32> {
        None
    }

    pub fn set_errno(code: i32) {
        // SAFETY: `_errno` returns this thread's errno slot.
        unsafe { *_errno() = code }
    }

    /// Opens a DLL. Windows resolves each DLL's imports against the DLLs it names, so
    /// `global` has no counterpart; the DLL's own directory is searched for its dependencies.
    pub fn open_library(path: &str, _global: bool) -> Result<*mut c_void, String> {
        let wide = wide_string(path);
        // SAFETY: `wide` is a NUL-terminated UTF-16 string.
        let handle = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        };
        if handle.is_null() {
            Err(last_library_error())
        } else {
            Ok(handle)
        }
    }

    /// The executable's module. Windows has no handle that searches every loaded DLL, so a
    /// symbol is looked up in the executable only.
    pub fn process_library() -> *mut c_void {
        // SAFETY: a null name asks for the executable's own module.
        unsafe { GetModuleHandleW(std::ptr::null()) }
    }

    /// The address of `name` in the library, or null.
    ///
    /// # Safety
    ///
    /// `handle` came from `open_library` or `process_library`.
    pub unsafe fn library_symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        // SAFETY: the caller passes a live module; `name` is NUL-terminated.
        unsafe {
            GetProcAddress(handle as HMODULE, name.as_ptr().cast())
                .map_or(std::ptr::null_mut(), |symbol| symbol as *mut c_void)
        }
    }

    /// The file of the DLL that holds `address`.
    pub fn library_path(address: *const c_void) -> Option<String> {
        let mut module: HMODULE = std::ptr::null_mut();
        // SAFETY: the flags make `address` an address inside the module and leave its
        // reference count alone; the buffer length is passed with it.
        unsafe {
            if GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                address.cast(),
                &mut module,
            ) == 0
            {
                return None;
            }
            let mut buffer = vec![0u16; 32768];
            let length = GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32);
            if length == 0 {
                return None;
            }
            buffer.truncate(length as usize);
            Some(
                std::ffi::OsString::from_wide(&buffer)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    /// The system's message for the last failed call, as GNU R words a `LoadLibrary` failure.
    pub fn last_library_error() -> String {
        // SAFETY: FormatMessageW writes at most the buffer's length.
        unsafe {
            let code = GetLastError();
            let mut buffer = [0u16; 512];
            let length = FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                std::ptr::null(),
                code,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null(),
            );
            let message = String::from_utf16_lossy(&buffer[..length as usize]);
            format!("LoadLibrary failure:  {}", message.trim_end())
        }
    }

    fn wide_string(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

/// A conversion between character sets written in Rust, for platforms without `iconv(3)`. It
/// converts one character at a time, so it can stop at any character boundary the way `iconv`
/// does when the output is full or the input is bad.
///
/// UTF-8, Latin-1, ASCII, UTF-16 and UTF-32 are converted directly. Other encodings go
/// through `encoding_rs` by their WHATWG label (`CP1252` reads as `windows-1252`); its labels
/// for Latin-1 and ASCII mean windows-1252, which is why those two are handled here first.
#[cfg_attr(unix, allow(dead_code))]
pub struct PortableIconv {
    from: Codec,
    to: Codec,
    decoder: Option<encoding_rs::Decoder>,
    encoder: Option<encoding_rs::Encoder>,
    /// Whether a byte order mark has been read from the input or written to the output.
    input_started: bool,
    output_started: bool,
}

#[derive(Clone, Copy)]
#[cfg_attr(unix, allow(dead_code))]
enum Codec {
    Utf8,
    Latin1,
    Ascii,
    /// `big_endian` is `None` for plain "UTF-16", which reads a byte order mark and writes one.
    Utf16 {
        big_endian: Option<bool>,
    },
    Utf32 {
        big_endian: Option<bool>,
    },
    Other(&'static encoding_rs::Encoding),
}

#[cfg_attr(unix, allow(dead_code))]
impl PortableIconv {
    pub fn open(from: &str, to: &str) -> Option<PortableIconv> {
        let (from, to) = (codec(from)?, codec(to)?);
        let mut converter = PortableIconv {
            from,
            to,
            decoder: None,
            encoder: None,
            input_started: false,
            output_started: false,
        };
        converter.reset();
        Some(converter)
    }

    pub fn convert(
        &mut self,
        input: &mut &[u8],
        output: &mut [u8],
    ) -> (usize, Result<(), IconvStop>) {
        let mut written = 0;
        while !input.is_empty() {
            if !self.input_started {
                self.input_started = true;
                if let Some(length) = self.read_byte_order_mark(input) {
                    *input = &input[length..];
                    continue;
                }
            }
            let (character, length) = match self.decode(input) {
                Ok(decoded) => decoded,
                Err(stop) => return (written, Err(stop)),
            };
            let mut encoded = [0u8; 16];
            let mut size = 0;
            if !self.output_started {
                size = self.byte_order_mark(&mut encoded);
            }
            match self.encode(character, &mut encoded[size..]) {
                Some(more) => size += more,
                None => return (written, Err(IconvStop::Invalid)),
            }
            let Some(target) = output.get_mut(written..written + size) else {
                return (written, Err(IconvStop::OutputFull));
            };
            target.copy_from_slice(&encoded[..size]);
            written += size;
            self.output_started = true;
            *input = &input[length..];
        }
        (written, Ok(()))
    }

    pub fn finish(&mut self, output: &mut [u8]) -> (usize, Result<(), IconvStop>) {
        let mut written = 0;
        if let Some(encoder) = self.encoder.as_mut() {
            let mut buffer = [0u8; 16];
            let (_, _, size) = encoder.encode_from_utf8_without_replacement("", &mut buffer, true);
            let Some(target) = output.get_mut(..size) else {
                return (0, Err(IconvStop::OutputFull));
            };
            target.copy_from_slice(&buffer[..size]);
            written = size;
        }
        self.reset();
        (written, Ok(()))
    }

    pub fn reset(&mut self) {
        self.decoder = match self.from {
            Codec::Other(encoding) => Some(encoding.new_decoder_without_bom_handling()),
            _ => None,
        };
        self.encoder = match self.to {
            Codec::Other(encoding) => Some(encoding.new_encoder()),
            _ => None,
        };
        self.input_started = false;
        self.output_started = false;
    }

    fn read_byte_order_mark(&mut self, input: &[u8]) -> Option<usize> {
        match self.from {
            Codec::Utf16 { big_endian: None } => {
                let big_endian = match input.get(..2)? {
                    [0xFE, 0xFF] => true,
                    [0xFF, 0xFE] => false,
                    _ => {
                        self.from = Codec::Utf16 {
                            big_endian: Some(true),
                        };
                        return None;
                    }
                };
                self.from = Codec::Utf16 {
                    big_endian: Some(big_endian),
                };
                Some(2)
            }
            Codec::Utf32 { big_endian: None } => {
                let big_endian = match input.get(..4)? {
                    [0, 0, 0xFE, 0xFF] => true,
                    [0xFF, 0xFE, 0, 0] => false,
                    _ => {
                        self.from = Codec::Utf32 {
                            big_endian: Some(true),
                        };
                        return None;
                    }
                };
                self.from = Codec::Utf32 {
                    big_endian: Some(big_endian),
                };
                Some(4)
            }
            _ => None,
        }
    }

    /// Plain "UTF-16" and "UTF-32" output starts with a little-endian byte order mark, as
    /// glibc and GNU libiconv write on little-endian machines.
    fn byte_order_mark(&self, buffer: &mut [u8]) -> usize {
        match self.to {
            Codec::Utf16 { big_endian: None } => {
                buffer[..2].copy_from_slice(&[0xFF, 0xFE]);
                2
            }
            Codec::Utf32 { big_endian: None } => {
                buffer[..4].copy_from_slice(&[0xFF, 0xFE, 0, 0]);
                4
            }
            _ => 0,
        }
    }

    /// The first character of `input` and how many bytes it takes.
    fn decode(&mut self, input: &[u8]) -> Result<(char, usize), IconvStop> {
        match self.from {
            Codec::Utf8 => {
                let length = match input[0] {
                    0x00..=0x7F => 1,
                    0xC2..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF4 => 4,
                    _ => return Err(IconvStop::Invalid),
                };
                let Some(bytes) = input.get(..length) else {
                    return match std::str::from_utf8(input) {
                        Err(failure) if failure.error_len().is_none() => Err(IconvStop::Incomplete),
                        _ => Err(IconvStop::Invalid),
                    };
                };
                std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| text.chars().next())
                    .map(|character| (character, length))
                    .ok_or(IconvStop::Invalid)
            }
            Codec::Latin1 => Ok((char::from(input[0]), 1)),
            Codec::Ascii if input[0] < 0x80 => Ok((char::from(input[0]), 1)),
            Codec::Ascii => Err(IconvStop::Invalid),
            Codec::Utf16 { big_endian } => {
                let big_endian = big_endian.unwrap_or(true);
                let unit = |bytes: &[u8]| -> Option<u32> {
                    let pair = [*bytes.first()?, *bytes.get(1)?];
                    Some(u32::from(if big_endian {
                        u16::from_be_bytes(pair)
                    } else {
                        u16::from_le_bytes(pair)
                    }))
                };
                let first = unit(input).ok_or(IconvStop::Incomplete)?;
                match first {
                    0xD800..=0xDBFF => {
                        let second = unit(&input[2..]).ok_or(IconvStop::Incomplete)?;
                        if !(0xDC00..=0xDFFF).contains(&second) {
                            return Err(IconvStop::Invalid);
                        }
                        let code = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                        char::from_u32(code)
                            .map(|character| (character, 4))
                            .ok_or(IconvStop::Invalid)
                    }
                    0xDC00..=0xDFFF => Err(IconvStop::Invalid),
                    code => char::from_u32(code)
                        .map(|character| (character, 2))
                        .ok_or(IconvStop::Invalid),
                }
            }
            Codec::Utf32 { big_endian } => {
                let Some(bytes) = input.get(..4) else {
                    return Err(IconvStop::Incomplete);
                };
                let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
                let code = if big_endian.unwrap_or(true) {
                    u32::from_be_bytes(bytes)
                } else {
                    u32::from_le_bytes(bytes)
                };
                char::from_u32(code)
                    .map(|character| (character, 4))
                    .ok_or(IconvStop::Invalid)
            }
            Codec::Other(_) => {
                let decoder = self.decoder.as_mut().ok_or(IconvStop::Invalid)?;
                // Feeding one byte at a time finds where the first character ends.
                let mut buffer = [0u8; 16];
                for length in 1..=input.len().min(8) {
                    let (result, _, written) = decoder.decode_to_utf8_without_replacement(
                        &input[length - 1..length],
                        &mut buffer,
                        false,
                    );
                    if let encoding_rs::DecoderResult::Malformed(..) = result {
                        if let Codec::Other(encoding) = self.from {
                            self.decoder = Some(encoding.new_decoder_without_bom_handling());
                        }
                        return Err(IconvStop::Invalid);
                    }
                    if written > 0 {
                        return std::str::from_utf8(&buffer[..written])
                            .ok()
                            .and_then(|text| text.chars().next())
                            .map(|character| (character, length))
                            .ok_or(IconvStop::Invalid);
                    }
                }
                if let Codec::Other(encoding) = self.from {
                    self.decoder = Some(encoding.new_decoder_without_bom_handling());
                }
                Err(IconvStop::Incomplete)
            }
        }
    }

    /// Writes `character` into `buffer`; `None` when the target cannot represent it.
    fn encode(&mut self, character: char, buffer: &mut [u8]) -> Option<usize> {
        let code = u32::from(character);
        match self.to {
            Codec::Utf8 => Some(character.encode_utf8(buffer).len()),
            Codec::Latin1 => {
                *buffer.first_mut()? = u8::try_from(code).ok()?;
                Some(1)
            }
            Codec::Ascii => {
                *buffer.first_mut()? = u8::try_from(code).ok().filter(|byte| *byte < 0x80)?;
                Some(1)
            }
            Codec::Utf16 { big_endian } => {
                let big_endian = big_endian.unwrap_or(false);
                let mut units = [0u16; 2];
                let mut size = 0;
                for unit in character.encode_utf16(&mut units).iter() {
                    let bytes = if big_endian {
                        unit.to_be_bytes()
                    } else {
                        unit.to_le_bytes()
                    };
                    buffer.get_mut(size..size + 2)?.copy_from_slice(&bytes);
                    size += 2;
                }
                Some(size)
            }
            Codec::Utf32 { big_endian } => {
                let bytes = if big_endian.unwrap_or(false) {
                    code.to_be_bytes()
                } else {
                    code.to_le_bytes()
                };
                buffer.get_mut(..4)?.copy_from_slice(&bytes);
                Some(4)
            }
            Codec::Other(_) => {
                let encoder = self.encoder.as_mut()?;
                let mut text = [0u8; 4];
                let (result, _, written) = encoder.encode_from_utf8_without_replacement(
                    character.encode_utf8(&mut text),
                    buffer,
                    false,
                );
                match result {
                    encoding_rs::EncoderResult::InputEmpty => Some(written),
                    _ => None,
                }
            }
        }
    }
}

/// The codec an `iconv` encoding name stands for, ignoring case and a `//TRANSLIT` or
/// `//IGNORE` suffix.
#[cfg_attr(unix, allow(dead_code))]
fn codec(name: &str) -> Option<Codec> {
    let base = name.split("//").next().unwrap_or(name).to_ascii_uppercase();
    Some(match base.as_str() {
        "" | "UTF-8" | "UTF8" => Codec::Utf8,
        "LATIN1" | "LATIN-1" | "ISO-8859-1" | "ISO8859-1" | "ISO_8859-1" | "L1" | "CP819" => {
            Codec::Latin1
        }
        "ASCII" | "US-ASCII" | "ANSI_X3.4-1968" | "646" => Codec::Ascii,
        "UTF-16" | "UTF16" | "UCS-2" => Codec::Utf16 { big_endian: None },
        "UTF-16BE" | "UTF16BE" | "UCS-2BE" => Codec::Utf16 {
            big_endian: Some(true),
        },
        "UTF-16LE" | "UTF16LE" | "UCS-2LE" => Codec::Utf16 {
            big_endian: Some(false),
        },
        "UTF-32" | "UTF32" | "UCS-4" => Codec::Utf32 { big_endian: None },
        "UTF-32BE" | "UTF32BE" | "UCS-4BE" => Codec::Utf32 {
            big_endian: Some(true),
        },
        "UTF-32LE" | "UTF32LE" | "UCS-4LE" => Codec::Utf32 {
            big_endian: Some(false),
        },
        other => {
            let label = match other.strip_prefix("CP") {
                Some(number) if number.bytes().all(|byte| byte.is_ascii_digit()) => {
                    format!("windows-{number}")
                }
                _ => other.to_string(),
            };
            let encoding = encoding_rs::Encoding::for_label(label.as_bytes())?;
            if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
                return None;
            }
            Codec::Other(encoding)
        }
    })
}

/// Converts all of `input`, growing the output as needed. `Err` carries the stop and the
/// output so far, with `input` advanced to the offending bytes.
pub fn convert_all(
    converter: &mut Iconv,
    input: &mut &[u8],
    output: &mut Vec<u8>,
) -> Result<(), IconvStop> {
    let mut buffer = vec![0u8; input.len() * 4 + 16];
    loop {
        let (written, outcome) = converter.convert(input, &mut buffer);
        output.extend_from_slice(&buffer[..written]);
        match outcome {
            Ok(()) => return Ok(()),
            Err(IconvStop::OutputFull) => continue,
            Err(stop) => return Err(stop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IconvStop, PortableIconv};

    fn convert(from: &str, to: &str, input: &[u8]) -> (Vec<u8>, Result<(), IconvStop>) {
        let mut converter = PortableIconv::open(from, to).expect("known encodings");
        let mut rest = input;
        let mut output = vec![0u8; 64];
        let (written, outcome) = converter.convert(&mut rest, &mut output);
        output.truncate(written);
        (output, outcome)
    }

    #[test]
    fn converts_between_unicode_forms_and_latin1() {
        assert_eq!(
            convert("UTF-8", "latin1", "café".as_bytes()),
            (b"caf\xe9".to_vec(), Ok(()))
        );
        assert_eq!(
            convert("latin1", "UTF-8", b"caf\xe9"),
            ("café".as_bytes().to_vec(), Ok(()))
        );
        assert_eq!(
            convert("UTF-8", "UTF-16LE", "a€".as_bytes()),
            (vec![b'a', 0, 0xAC, 0x20], Ok(()))
        );
        assert_eq!(
            convert("UTF-16", "UTF-8", &[0xFF, 0xFE, b'h', 0, b'i', 0]),
            (b"hi".to_vec(), Ok(()))
        );
        assert_eq!(
            convert("UTF-8", "UTF-32BE", "😀".as_bytes()),
            (vec![0, 1, 0xF6, 0x00], Ok(()))
        );
        assert_eq!(
            convert("UTF-8", "CP1252", "€".as_bytes()),
            (vec![0x80], Ok(()))
        );
        assert_eq!(
            convert("windows-1252", "UTF-8", &[0x80]),
            ("€".as_bytes().to_vec(), Ok(()))
        );
    }

    #[test]
    fn stops_where_iconv_would() {
        assert_eq!(
            convert("UTF-8", "ASCII", "aé".as_bytes()),
            (b"a".to_vec(), Err(IconvStop::Invalid))
        );
        assert_eq!(
            convert("UTF-8", "latin1", b"a\xff"),
            (b"a".to_vec(), Err(IconvStop::Invalid))
        );
        assert_eq!(
            convert("UTF-8", "latin1", b"a\xc3"),
            (b"a".to_vec(), Err(IconvStop::Incomplete))
        );
        let mut converter = PortableIconv::open("UTF-8", "UTF-16LE").expect("known encodings");
        let mut rest = "ab".as_bytes();
        let mut output = [0u8; 3];
        assert_eq!(
            converter.convert(&mut rest, &mut output),
            (2, Err(IconvStop::OutputFull))
        );
        assert_eq!(rest, b"b");
        assert!(PortableIconv::open("UTF-8", "no-such-encoding").is_none());
    }
}
