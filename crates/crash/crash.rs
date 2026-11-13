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
                STAGE.store(STAGE_DONE, Ordering::SeqCst);
                write_native_stack(&mut out);
                chain(signal, info, context);
            }
            STAGE_HEADER_AND_R => {
                // The fault happened while printing the R call stack, whose memory was not
                // what it claimed to be.
                STAGE.store(STAGE_DONE, Ordering::SeqCst);
                let mut out = Out::new();
                out.text("(the R call stack could not be read to the end)\n");
                write_native_stack(&mut out);
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

    #[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
    fn write_native_stack(out: &mut Out) {
        let mut frames = [ptr::null_mut(); NATIVE_FRAMES];
        out.text("\nNative stack, innermost first, starting in the crash handler (module(symbol+offset) [address]):\n");
        out.flush();
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
}
