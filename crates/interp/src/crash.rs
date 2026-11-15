//! The R call stack in a crash report.
//!
//! The report runs in a signal handler after a fatal signal, so the interpreter's state may be
//! half updated or overwritten. Nothing here allocates, and every address is probed with the
//! host's `readable` before it is followed: the frame stack, each frame, each call, and each
//! name. Every loop is bounded, so a cycle in corrupted memory ends in the cap.

use {
    crate::{Frame, interp, is_installed},
    heap::object::{Sexp, SexpType, VECTOR_DATA_OFFSET},
    std::mem::size_of,
};

/// Writes bytes to standard error.
pub type Emit = unsafe extern "C" fn(bytes: *const u8, length: usize);

/// Whether `length` bytes at `address` can be read without faulting.
pub type Readable = unsafe extern "C" fn(address: *const u8, length: usize) -> bool;

/// The frames shown before the report says how many more there are.
const FRAME_LIMIT: usize = 50;
/// The longest function name shown, in bytes.
const NAME_LIMIT: usize = 96;
/// An object header and the three words every pairlist cell and symbol carries.
const CELL_BYTES: usize = 40;

/// Prints the R call stack, innermost call first, numbered as `traceback()` numbers it. Foreign
/// calls (`.Call` and its relatives) appear between the closure calls around them, as in GNU R's
/// tracebacks.
///
/// # Safety
/// Meant for a signal handler: `emit` and `readable` must be async-signal-safe.
pub unsafe extern "C" fn write_traceback(emit: Emit, readable: Readable) {
    let mut out = Out { emit, readable };
    if !is_installed() {
        out.text(b"\nR call stack: no R session was running\n");
        return;
    }
    // SAFETY: the interpreter is installed. Only plain fields are read, and the frame stack's
    // memory is probed before it is followed.
    unsafe {
        let state = interp();
        let (frames, depth) = state.frames.crash_view();
        let foreign = state.foreign_calls.as_ptr();
        let foreign_count = state.foreign_calls.len();
        if depth == 0 || !out.readable(frames.cast(), 1) {
            out.text(b"\nR call stack: empty, the crash happened outside any R function call\n");
            return;
        }
        out.text(b"\nR call stack, innermost call first:\n");
        let mut printed = 0;
        let mut total = 0;
        let mut next_foreign = foreign_count;
        let mut position = depth;
        loop {
            // A foreign call stands above the frames it was made from.
            while next_foreign > 0 {
                let entry = foreign.add(next_foreign - 1);
                if !out.readable(entry.cast(), size_of::<(Sexp, usize)>()) || (*entry).1 < position
                {
                    break;
                }
                total += 1;
                if printed < FRAME_LIMIT {
                    printed += 1;
                    out.line(printed, (*entry).0);
                }
                next_foreign -= 1;
            }
            if position == 0 {
                break;
            }
            position -= 1;
            total += 1;
            if printed < FRAME_LIMIT {
                printed += 1;
                let frame = frames.add(position);
                if out.readable(frame.cast(), size_of::<Frame>()) {
                    out.line(printed, (*frame).call);
                } else {
                    out.numbered(printed);
                    out.text(b"<unreadable frame>\n");
                }
            }
        }
        if total > printed {
            out.text(b"   ... ");
            out.number(total - printed);
            out.text(b" more\n");
        }
    }
}

struct Out {
    emit: Emit,
    readable: Readable,
}

impl Out {
    fn text(&mut self, bytes: &[u8]) {
        // SAFETY: the host's writer takes any byte slice.
        unsafe { (self.emit)(bytes.as_ptr(), bytes.len()) };
    }

    fn readable(&self, address: *const u8, length: usize) -> bool {
        // SAFETY: the host's probe only asks the kernel to copy the bytes.
        unsafe { (self.readable)(address, length) }
    }

    fn number(&mut self, mut value: usize) {
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
        self.text(&digits[start..]);
    }

    fn numbered(&mut self, number: usize) {
        if number < 10 {
            self.text(b" ");
        }
        self.number(number);
        self.text(b": ");
    }

    /// One numbered line for `call`: the name of the function it calls, then `(...)`.
    unsafe fn line(&mut self, number: usize, call: Sexp) {
        self.numbered(number);
        // SAFETY: each step probes before it reads.
        unsafe {
            if !self.function_name(call) {
                self.text(b"<unknown call>");
            }
        }
        self.text(b"(...)\n");
    }

    /// Prints the callee of `call`: a name, or `package::name` and its relatives. Returns
    /// whether it printed anything.
    unsafe fn function_name(&mut self, call: Sexp) -> bool {
        // SAFETY: `cell` probes before it follows an object.
        unsafe {
            let Some(call) = self.cell(call, SexpType::Language) else {
                return false;
            };
            let head = call.car();
            if let Some(head) = self.cell(head, SexpType::Symbol) {
                return self.symbol(head);
            }
            // `pkg::name(...)`, `object$name(...)` and `object@name(...)`.
            let Some(head) = self.cell(head, SexpType::Language) else {
                self.text(b"<anonymous function>");
                return true;
            };
            let operator = head.car();
            let Some(operator) = self.cell(operator, SexpType::Symbol) else {
                self.text(b"<anonymous function>");
                return true;
            };
            let arguments = head.cdr();
            let left = self
                .cell(arguments, SexpType::Pairlist)
                .map(|cell| cell.car());
            let right = left
                .and_then(|_| self.cell(arguments, SexpType::Pairlist))
                .and_then(|cell| self.cell(cell.cdr(), SexpType::Pairlist))
                .map(|cell| cell.car());
            let (Some(left), Some(right)) = (left, right) else {
                self.text(b"<anonymous function>");
                return true;
            };
            let (Some(left), Some(right)) = (
                self.cell(left, SexpType::Symbol),
                self.cell(right, SexpType::Symbol),
            ) else {
                self.text(b"<anonymous function>");
                return true;
            };
            self.symbol(left) && self.symbol(operator) && self.symbol(right)
        }
    }

    /// `object` as itself when its address is readable and it has `kind`.
    unsafe fn cell(&self, object: Sexp, kind: SexpType) -> Option<Sexp> {
        let address = object.as_ptr() as *const u8;
        // SAFETY: the probe covers the header and the words `car`, `cdr` and `tag` read. The
        // kind byte is compared raw, because a byte that is no kind must not become one.
        unsafe { (self.readable(address, CELL_BYTES) && *address == kind as u8).then_some(object) }
    }

    /// Prints a symbol's name, whose bytes are a `CHARSXP` the symbol's first word points to.
    unsafe fn symbol(&mut self, symbol: Sexp) -> bool {
        // SAFETY: the symbol was probed by `cell`, and the character vector is probed here.
        unsafe {
            let name = symbol.car();
            let Some(name) = self.cell(name, SexpType::Char) else {
                return false;
            };
            let length = name.length();
            if !(0..=NAME_LIMIT as isize).contains(&length) {
                return false;
            }
            let bytes = (name.as_ptr() as *const u8).add(VECTOR_DATA_OFFSET);
            if !self.readable(bytes, length as usize) {
                return false;
            }
            self.text(std::slice::from_raw_parts(bytes, length as usize));
            true
        }
    }
}
