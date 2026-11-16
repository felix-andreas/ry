//! The report a fatal signal prints before the process dies.
//!
//! A process that ends in SIGSEGV, SIGBUS or SIGABRT prints the ry version, the signal and its
//! cause, the R call stack when a runtime is loaded, and the native stack, all to standard error,
//! and then dies by the same signal, so its exit status and core dump are what they would have
//! been without the report.
//!
//! The handler runs in a process in an unknown state, so it allocates nothing, takes no lock, and
//! reads memory only after a probe says it is readable. It runs on its own signal stack, which
//! makes a stack overflow reportable. A handler installed before ours keeps working: the report
//! comes first and the previous handler then runs, which is how Rust's own stack overflow
//! message still appears.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Writes `length` bytes at `bytes` to standard error.
pub type Emit = unsafe extern "C" fn(bytes: *const u8, length: usize);

/// Whether `length` bytes at `address` can be read without faulting.
pub type Readable = unsafe extern "C" fn(address: *const u8, length: usize) -> bool;

/// Prints the R call stack of a loaded runtime through `emit`, innermost call first, probing
/// every address it follows with `readable`. A runtime registers it with [`set_traceback`].
pub type Traceback = unsafe extern "C" fn(emit: Emit, readable: Readable);

/// Registers the runtime's printer of the R call stack. The runtime lives in a shared library
/// that the executable loads after it installs the handler, so it cannot be known earlier.
pub fn set_traceback(hook: Traceback) {
    TRACEBACK.store(hook as usize, Ordering::SeqCst);
}

/// Installs the handlers for SIGSEGV, SIGBUS and SIGABRT, and the signal stack of the calling
/// thread. `banner` names the program and its version. Installing twice only gives the calling
/// thread a signal stack.
#[cfg(unix)]
pub fn install(banner: &str) {
    unix::install(banner);
}

#[cfg(not(unix))]
pub fn install(_banner: &str) {}

static TRACEBACK: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
mod unix {
    use {
        super::*,
        std::{
            mem, ptr,
            sync::{
                OnceLock,
                atomic::{AtomicBool, AtomicI32, AtomicU8},
            },
        },
    };

    const SIGNALS: [libc::c_int; 3] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT];
    /// The signal stack holds the report's buffer, the unwinder's state and the previous
    /// handler's frame, so it is generous.
    const ALTERNATE_STACK_SIZE: usize = 256 * 1024;
    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
    const NATIVE_FRAMES: usize = 64;
    const PROBE_LIMIT: usize = 256;

    static BANNER: OnceLock<&'static str> = OnceLock::new();
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    /// How far the report got, so a fault inside the report resumes with the next part.
    static STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
    const STAGE_IDLE: u8 = 0;
    const STAGE_HEADER_AND_R: u8 = 1;
    const STAGE_NATIVE: u8 = 2;
    const STAGE_DONE: u8 = 3;
    /// The read and write ends of the pipe that probes memory.
    static PROBE: [AtomicI32; 2] = [AtomicI32::new(-1), AtomicI32::new(-1)];
    /// The handler and flags each signal had before ours, indexed like [`SIGNALS`].
    static PREVIOUS_HANDLER: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
    static PREVIOUS_FLAGS: [AtomicI32; 3] = [const { AtomicI32::new(0) }; 3];

    pub(super) fn install(banner: &str) {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            install_alternate_stack();
            return;
        }
        // The text outlives every signal, and the handler cannot allocate to copy it.
        BANNER.get_or_init(|| Box::leak(banner.to_owned().into_boxed_str()));
        // The first call loads the unwinder's library, which allocates and takes locks.
        warm_up_native_stack();
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        symbols::remember_executable();
        open_probe();
        install_alternate_stack();
        // SAFETY: the structs are zeroed before the fields the kernel reads are set.
        unsafe {
            for (index, signal) in SIGNALS.into_iter().enumerate() {
                let mut action: libc::sigaction = mem::zeroed();
                action.sa_sigaction = handle as *const () as usize;
                // NODEFER lets a fault inside the report reach the handler again, which then
                // finishes the report instead of the kernel killing the process.
                action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous: libc::sigaction = mem::zeroed();
                if libc::sigaction(signal, &action, &mut previous) == 0 {
                    PREVIOUS_HANDLER[index].store(previous.sa_sigaction, Ordering::SeqCst);
                    PREVIOUS_FLAGS[index].store(previous.sa_flags, Ordering::SeqCst);
                }
            }
        }
    }

    /// Gives the calling thread a signal stack large enough for the report, unless it has one.
    fn install_alternate_stack() {
        // SAFETY: mmap returns a fresh private region, and sigaltstack reads the struct passed.
        unsafe {
            let mut current: libc::stack_t = mem::zeroed();
            if libc::sigaltstack(ptr::null(), &mut current) == 0
                && current.ss_flags & libc::SS_DISABLE == 0
                && current.ss_size >= ALTERNATE_STACK_SIZE
            {
                return;
            }
            let memory = libc::mmap(
                ptr::null_mut(),
                ALTERNATE_STACK_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if memory == libc::MAP_FAILED {
                return;
            }
            let stack = libc::stack_t {
                ss_sp: memory,
                ss_flags: 0,
                ss_size: ALTERNATE_STACK_SIZE,
            };
            libc::sigaltstack(&stack, ptr::null_mut());
        }
    }

    fn open_probe() {
        let mut ends = [-1; 2];
        // SAFETY: `ends` has room for the two descriptors pipe writes.
        unsafe {
            if libc::pipe(ends.as_mut_ptr()) != 0 {
                return;
            }
            for end in ends {
                libc::fcntl(end, libc::F_SETFD, libc::FD_CLOEXEC);
                let flags = libc::fcntl(end, libc::F_GETFL);
                libc::fcntl(end, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        PROBE[0].store(ends[0], Ordering::SeqCst);
        PROBE[1].store(ends[1], Ordering::SeqCst);
    }

    extern "C" fn handle(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        match STAGE.load(Ordering::SeqCst) {
            STAGE_IDLE => {
                STAGE.store(STAGE_HEADER_AND_R, Ordering::SeqCst);
                let mut out = Out::new();
                write_header(&mut out, signal, info);
                out.flush();
                write_r_stack();
                STAGE.store(STAGE_NATIVE, Ordering::SeqCst);
                write_native_stack(&mut out);
                STAGE.store(STAGE_DONE, Ordering::SeqCst);
                chain(signal, info, context);
            }
            STAGE_HEADER_AND_R => {
                // The fault happened while printing the R call stack, whose memory was not
                // what it claimed to be.
                STAGE.store(STAGE_NATIVE, Ordering::SeqCst);
                let mut out = Out::new();
                out.text("(the R call stack could not be read to the end)\n");
                write_native_stack(&mut out);
                STAGE.store(STAGE_DONE, Ordering::SeqCst);
            }
            STAGE_NATIVE => {
                // Naming the frames reads mapped files, which can fault when a file changed on
                // disk; the unnamed frames still help.
                STAGE.store(STAGE_DONE, Ordering::SeqCst);
                let mut out = Out::new();
                out.text("(the native stack could not be named to the end)\n");
                out.flush();
                write_raw_native_stack();
            }
            _ => {}
        }
        die(signal, info);
    }

    /// Runs the handler that was installed before ours, so that Rust's stack overflow message
    /// still appears after the report.
    fn chain(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
        let Some(index) = SIGNALS.iter().position(|candidate| *candidate == signal) else {
            return;
        };
        let handler = PREVIOUS_HANDLER[index].load(Ordering::SeqCst);
        if handler == libc::SIG_DFL || handler == libc::SIG_IGN || handler == 0 {
            return;
        }
        if PREVIOUS_FLAGS[index].load(Ordering::SeqCst) & libc::SA_SIGINFO != 0 {
            // SAFETY: the kernel recorded this address as a three-argument handler.
            let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                unsafe { mem::transmute(handler) };
            handler(signal, info, context);
        } else {
            // SAFETY: the kernel recorded this address as a one-argument handler.
            let handler: extern "C" fn(libc::c_int) = unsafe { mem::transmute(handler) };
            handler(signal);
        }
    }

    /// Restores the default action and makes sure the signal takes it. A fault repeats when the
    /// handler returns, so it needs nothing more; a signal that a process sent does not repeat.
    fn die(signal: libc::c_int, info: *mut libc::siginfo_t) {
        // SAFETY: these calls read and write only the structs passed to them.
        unsafe {
            let mut action: libc::sigaction = mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, ptr::null_mut());
            if info.is_null() || sent_by_process((*info).si_code) {
                let mut set: libc::sigset_t = mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, signal);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, ptr::null_mut());
                libc::raise(signal);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn sent_by_process(code: libc::c_int) -> bool {
        code <= 0 || code == 0x10001
    }

    #[cfg(not(target_os = "macos"))]
    fn sent_by_process(code: libc::c_int) -> bool {
        code <= 0
    }

    fn write_header(out: &mut Out, signal: libc::c_int, info: *mut libc::siginfo_t) {
        let (name, meaning) = match signal {
            libc::SIGSEGV => ("SIGSEGV", "segmentation fault"),
            libc::SIGBUS => ("SIGBUS", "bus error"),
            _ => ("SIGABRT", "abort"),
        };
        out.text("\n*** ry caught ");
        out.text(name);
        out.text(" (");
        out.text(meaning);
        out.text(") ***\n");
        out.text(BANNER.get().copied().unwrap_or("ry"));
        out.text("\n");
        // SAFETY: the kernel passes a valid siginfo, and a null one is skipped.
        let Some(info) = (unsafe { info.as_ref() }) else {
            return;
        };
        let code = info.si_code;
        if sent_by_process(code) {
            out.text("raised by a call to kill, raise or abort, not by a fault\n");
            return;
        }
        if signal == libc::SIGABRT {
            return;
        }
        out.text("address 0x");
        // SAFETY: the signal is a fault, for which the kernel fills in the address.
        out.hex(unsafe { info.si_addr() } as usize as u64);
        out.text(", cause '");
        out.text(match (signal, code) {
            (libc::SIGSEGV, 1) => "memory not mapped",
            (libc::SIGSEGV, 2) => "invalid permissions",
            (libc::SIGBUS, 1) => "invalid address alignment",
            (libc::SIGBUS, 2) => "nonexistent physical address",
            (libc::SIGBUS, 3) => "object specific hardware error",
            _ => "unknown",
        });
        out.text("' (si_code ");
        out.decimal(code as u64);
        out.text(")\n");
    }

    fn write_r_stack() {
        let hook = TRACEBACK.load(Ordering::SeqCst);
        if hook == 0 {
            return;
        }
        // SAFETY: `set_traceback` stored a function of this type.
        let hook: Traceback = unsafe { mem::transmute(hook) };
        // SAFETY: the hook reads the runtime's frames through `readable` and writes through
        // `emit`, which are this module's functions.
        unsafe { hook(emit, readable) };
    }

    unsafe extern "C" fn emit(bytes: *const u8, length: usize) {
        // SAFETY: the hook passes bytes it owns.
        write_all(unsafe { std::slice::from_raw_parts(bytes, length) });
    }

    /// Probes by asking the kernel to copy the bytes into a pipe, which fails with a short write
    /// where a load would fault.
    unsafe extern "C" fn readable(address: *const u8, length: usize) -> bool {
        let read_end = PROBE[0].load(Ordering::SeqCst);
        let write_end = PROBE[1].load(Ordering::SeqCst);
        if address.is_null() || length > PROBE_LIMIT || read_end < 0 || write_end < 0 {
            return false;
        }
        let mut sink = [0u8; PROBE_LIMIT];
        // SAFETY: the write only reads `address`, and the reads fill the local buffer.
        unsafe {
            while libc::read(read_end, sink.as_mut_ptr().cast(), PROBE_LIMIT) > 0 {}
            let copied = libc::write(write_end, address.cast(), length);
            while libc::read(read_end, sink.as_mut_ptr().cast(), PROBE_LIMIT) > 0 {}
            copied == length as isize
        }
    }

    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
    fn warm_up_native_stack() {
        let mut frames = [ptr::null_mut(); 4];
        // SAFETY: the buffer has room for the count it is told.
        unsafe { libc::backtrace(frames.as_mut_ptr(), frames.len() as libc::c_int) };
    }

    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
    fn warm_up_native_stack() {}

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn write_native_stack(out: &mut Out) {
        let mut frames = [ptr::null_mut(); NATIVE_FRAMES];
        out.text("\nNative stack, innermost first, starting in the crash handler (module(function+offset) [address]):\n");
        // SAFETY: the buffer has room for the count it is told.
        let count = unsafe { libc::backtrace(frames.as_mut_ptr(), NATIVE_FRAMES as libc::c_int) };
        let mut files = symbols::Files::new();
        for (index, frame) in frames.iter().take(count.max(0) as usize).enumerate() {
            symbols::write_frame(out, &mut files, *frame as usize, index > 0);
            out.flush();
        }
        files.close();
    }

    #[cfg(target_os = "macos")]
    fn write_native_stack(out: &mut Out) {
        out.text("\nNative stack, innermost first, starting in the crash handler (module(symbol+offset) [address]):\n");
        out.flush();
        write_raw_native_stack();
    }

    /// The native stack as the C library names it, from the dynamic symbols only.
    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
    fn write_raw_native_stack() {
        let mut frames = [ptr::null_mut(); NATIVE_FRAMES];
        // SAFETY: the buffer has room for the count it is told, and `backtrace_symbols_fd`
        // writes without allocating.
        unsafe {
            let count = libc::backtrace(frames.as_mut_ptr(), NATIVE_FRAMES as libc::c_int);
            libc::backtrace_symbols_fd(frames.as_ptr(), count, 2);
        }
    }

    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
    fn write_native_stack(out: &mut Out) {
        out.text("\nNative stack: not available on this platform\n");
        out.flush();
    }

    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
    fn write_raw_native_stack() {}

    fn write_all(mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // SAFETY: the write reads the slice.
            let written = unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
            match written {
                1.. => bytes = &bytes[written as usize..],
                _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
                _ => return,
            }
        }
    }

    /// A buffer on the stack that formats text and numbers without allocating.
    struct Out {
        buffer: [u8; 1024],
        used: usize,
    }

    impl Out {
        fn new() -> Self {
            Self {
                buffer: [0; 1024],
                used: 0,
            }
        }

        fn text(&mut self, text: &str) {
            self.bytes(text.as_bytes());
        }

        fn decimal(&mut self, mut value: u64) {
            let mut digits = [0u8; 20];
            let mut start = digits.len();
            loop {
                start -= 1;
                digits[start] = b'0' + (value % 10) as u8;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            self.bytes(&digits[start..]);
        }

        fn hex(&mut self, mut value: u64) {
            let mut digits = [0u8; 16];
            let mut start = digits.len();
            loop {
                start -= 1;
                digits[start] = b"0123456789abcdef"[(value & 15) as usize];
                value >>= 4;
                if value == 0 {
                    break;
                }
            }
            self.bytes(&digits[start..]);
        }

        fn bytes(&mut self, bytes: &[u8]) {
            for byte in bytes {
                if self.used == self.buffer.len() {
                    self.flush();
                }
                self.buffer[self.used] = *byte;
                self.used += 1;
            }
        }

        fn flush(&mut self) {
            write_all(&self.buffer[..self.used]);
            self.used = 0;
        }
    }

    impl std::fmt::Write for Out {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            self.text(text);
            Ok(())
        }
    }

    /// Function names for native frames, read from the `.symtab` of each frame's file.
    /// `dladdr` sees only the dynamic symbols, which name the few exported functions, so it
    /// would attribute an internal function to whichever export precedes it. The files are
    /// mapped read-only for the report and unmapped after it; nothing is allocated.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    mod symbols {
        use {
            super::Out,
            std::{
                ffi::CStr,
                fmt::Write,
                mem, ptr,
                sync::atomic::{AtomicUsize, Ordering},
            },
        };

        const SHT_SYMTAB: u32 = 2;
        const STT_FUNC: u8 = 2;
        const ET_DYN: u16 = 3;
        const PT_LOAD: u32 = 1;
        const FILES: usize = 8;

        /// The load base of the executable that installed the handler. `dladdr` names the
        /// executable by `argv[0]`, which a relative path makes wrong once R changes the
        /// working directory, so its file is opened through `/proc/self/exe` instead.
        static EXECUTABLE_BASE: AtomicUsize = AtomicUsize::new(0);

        pub(super) fn remember_executable() {
            // SAFETY: the address is a function of this executable, and `info` is written by
            // dladdr.
            unsafe {
                let mut info: libc::Dl_info = mem::zeroed();
                if libc::dladdr(remember_executable as *const libc::c_void, &mut info) != 0 {
                    EXECUTABLE_BASE.store(info.dli_fbase as usize, Ordering::SeqCst);
                }
            }
        }

        /// One mapped file and where its symbol table lies.
        #[derive(Clone, Copy)]
        struct File {
            base: usize,
            map: *const u8,
            length: usize,
            /// The offset of the symbol table and its symbol count, and the offset and length
            /// of its string table; zero counts for a file without one.
            symbols: (usize, usize),
            strings: (usize, usize),
            /// What a symbol's value adds to an address in the file to give a loaded address.
            bias: usize,
        }

        pub(super) struct Files {
            files: [Option<File>; FILES],
        }

        impl Files {
            pub(super) fn new() -> Self {
                Self {
                    files: [None; FILES],
                }
            }

            pub(super) fn close(&mut self) {
                for file in self.files.iter_mut().filter_map(Option::take) {
                    // SAFETY: the region was mapped by `open` with this length.
                    unsafe { libc::munmap(file.map as *mut libc::c_void, file.length) };
                }
            }
        }

        /// Writes `module(function+0xoffset) [0xaddress]` for the return address `address`.
        pub(super) fn write_frame(
            out: &mut Out,
            files: &mut Files,
            address: usize,
            returned: bool,
        ) {
            // A return address may already belong to the next function, so a frame that
            // returns is looked up one byte before it.
            let lookup = if returned {
                address.wrapping_sub(1)
            } else {
                address
            };
            // SAFETY: dladdr writes `info`, whose name pointers stay valid while the object is
            // loaded.
            let mut info: libc::Dl_info = unsafe { mem::zeroed() };
            if unsafe { libc::dladdr(lookup as *const libc::c_void, &mut info) } == 0
                || info.dli_fname.is_null()
            {
                out.text("??? [0x");
                out.hex(address as u64);
                out.text("]\n");
                return;
            }
            // SAFETY: a non-null `dli_fname` is a NUL-terminated string.
            let module = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
            out.bytes(module);
            let base = info.dli_fbase as usize;
            let found = lookup_file(files, base, info.dli_fname)
                .and_then(|file| function_of(&file, lookup));
            let symbol = found.or_else(|| {
                // SAFETY: a non-null `dli_sname` is a NUL-terminated string.
                (!info.dli_sname.is_null()).then(|| {
                    (
                        unsafe { CStr::from_ptr(info.dli_sname) }.to_bytes(),
                        info.dli_saddr as usize,
                    )
                })
            });
            match symbol {
                Some((name, start)) => {
                    out.text("(");
                    write_name(out, name);
                    out.text("+0x");
                    out.hex(address.wrapping_sub(start) as u64);
                    out.text(")");
                }
                None => {
                    out.text("(+0x");
                    out.hex(address.wrapping_sub(base) as u64);
                    out.text(")");
                }
            }
            out.text(" [0x");
            out.hex(address as u64);
            out.text("]\n");
        }

        fn write_name(out: &mut Out, name: &[u8]) {
            match std::str::from_utf8(name).map(rustc_demangle::try_demangle) {
                // The alternate form leaves out the hash.
                Ok(Ok(demangled)) if write!(out, "{demangled:#}").is_ok() => {}
                _ => out.bytes(name),
            }
        }

        fn lookup_file(files: &mut Files, base: usize, name: *const libc::c_char) -> Option<File> {
            let free = match files
                .files
                .iter()
                .position(|file| file.is_none_or(|file| file.base == base))
            {
                Some(index) => index,
                None => return None,
            };
            if let Some(file) = files.files[free] {
                return Some(file);
            }
            let executable = EXECUTABLE_BASE.load(Ordering::SeqCst);
            let path = if base == executable && executable != 0 {
                c"/proc/self/exe".as_ptr()
            } else {
                name
            };
            let file = open(path, base)?;
            files.files[free] = Some(file);
            Some(file)
        }

        fn open(path: *const libc::c_char, base: usize) -> Option<File> {
            // SAFETY: `path` is NUL-terminated, the stat buffer is written by fstat, and the
            // mapping is read-only and private.
            let (map, length) = unsafe {
                let descriptor = libc::open(path, libc::O_RDONLY | libc::O_CLOEXEC);
                if descriptor < 0 {
                    return None;
                }
                let mut status: libc::stat = mem::zeroed();
                let length = match libc::fstat(descriptor, &mut status) {
                    0 => status.st_size as usize,
                    _ => 0,
                };
                let map = match length {
                    0 => libc::MAP_FAILED,
                    _ => libc::mmap(
                        ptr::null_mut(),
                        length,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE,
                        descriptor,
                        0,
                    ),
                };
                libc::close(descriptor);
                if map == libc::MAP_FAILED {
                    return None;
                }
                (map as *const u8, length)
            };
            let mut file = File {
                base,
                map,
                length,
                symbols: (0, 0),
                strings: (0, 0),
                bias: 0,
            };
            // SAFETY: the mapping holds `length` readable bytes until `Files::close`.
            let bytes = unsafe { std::slice::from_raw_parts(map, length) };
            if let Some((symbols, strings, bias)) = layout(bytes, base) {
                (file.symbols, file.strings, file.bias) = (symbols, strings, bias);
            }
            Some(file)
        }

        /// The symbol table, string table and load bias of a 64-bit little-endian ELF file. Every
        /// offset is checked, because the file on disk may not be the one that was loaded.
        fn layout(bytes: &[u8], base: usize) -> Option<((usize, usize), (usize, usize), usize)> {
            if bytes.get(..6)? != b"\x7fELF\x02\x01" {
                return None;
            }
            let kind = u16_at(bytes, 16)?;
            let program_headers = u64_at(bytes, 32)? as usize;
            let program_size = u16_at(bytes, 54)? as usize;
            let program_count = u16_at(bytes, 56)? as usize;
            let section_headers = u64_at(bytes, 40)? as usize;
            let section_size = u16_at(bytes, 58)? as usize;
            let section_count = u16_at(bytes, 60)? as usize;
            let entry = |table: usize, size: usize, index: usize, field: usize| {
                table
                    .checked_add(size.checked_mul(index)?)?
                    .checked_add(field)
            };
            // A shared object or position-independent executable is loaded at `base` less its
            // first segment's page-aligned address; anything else is loaded where it says.
            let bias = match kind {
                ET_DYN => {
                    let mut lowest = u64::MAX;
                    for index in 0..program_count {
                        let header = entry(program_headers, program_size, index, 0)?;
                        if u32_at(bytes, header)? == PT_LOAD {
                            lowest = lowest.min(u64_at(bytes, header.checked_add(16)?)?);
                        }
                    }
                    match lowest {
                        u64::MAX => 0,
                        lowest => base.wrapping_sub(lowest as usize & !0xfff),
                    }
                }
                _ => 0,
            };
            for index in 0..section_count {
                if u32_at(bytes, entry(section_headers, section_size, index, 4)?)? != SHT_SYMTAB {
                    continue;
                }
                let offset = u64_at(bytes, entry(section_headers, section_size, index, 24)?)?;
                let size = u64_at(bytes, entry(section_headers, section_size, index, 32)?)?;
                let symbol_size = u64_at(bytes, entry(section_headers, section_size, index, 56)?)?;
                let link =
                    u32_at(bytes, entry(section_headers, section_size, index, 40)?)? as usize;
                let strings = (
                    u64_at(bytes, entry(section_headers, section_size, link, 24)?)? as usize,
                    u64_at(bytes, entry(section_headers, section_size, link, 32)?)? as usize,
                );
                let (offset, size) = (offset as usize, size as usize);
                if symbol_size != 24
                    || offset.checked_add(size)? > bytes.len()
                    || strings.0.checked_add(strings.1)? > bytes.len()
                {
                    return None;
                }
                return Some(((offset, size / 24), strings, bias));
            }
            None
        }

        /// The name and loaded start of the function that holds `address`: the one whose
        /// extent holds it, or else the closest one that starts before it.
        fn function_of<'a>(file: &File, address: usize) -> Option<(&'a [u8], usize)> {
            // SAFETY: the mapping holds `length` readable bytes until `Files::close`.
            let bytes: &'a [u8] = unsafe { std::slice::from_raw_parts(file.map, file.length) };
            let target = address.wrapping_sub(file.bias) as u64;
            let (table, count) = file.symbols;
            let mut best: Option<(u64, u32, bool)> = None;
            for index in 0..count {
                let entry = table + index * 24;
                let Some(&info) = bytes.get(entry + 4) else {
                    break;
                };
                let (Some(value), Some(size), Some(name)) = (
                    u64_at(bytes, entry + 8),
                    u64_at(bytes, entry + 16),
                    u32_at(bytes, entry),
                ) else {
                    break;
                };
                if info & 0xf != STT_FUNC || value == 0 || value > target {
                    continue;
                }
                let holds = target < value.saturating_add(size);
                if match best {
                    None => true,
                    Some((start, _, best_holds)) => {
                        (holds && !best_holds) || (holds == best_holds && value > start)
                    }
                } {
                    best = Some((value, name, holds));
                }
            }
            let (start, name, _) = best?;
            let (strings, length) = file.strings;
            let name =
                bytes.get(strings.checked_add(name as usize)?..strings.checked_add(length)?)?;
            let name = &name[..name.iter().position(|&byte| byte == 0)?];
            Some((name, (start as usize).wrapping_add(file.bias)))
        }

        fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
            Some(u16::from_le_bytes(
                bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
            ))
        }

        fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
            Some(u32::from_le_bytes(
                bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
            ))
        }

        fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
            Some(u64::from_le_bytes(
                bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
            ))
        }
    }
}
