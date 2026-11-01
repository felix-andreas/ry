//! The parallel package's native routines: the L'Ecuyer-CMRG stream jumps behind
//! `nextRNGStream` and `nextRNGSubStream`, and the fork routines behind `mcfork`, `mclapply`
//! and `mcparallel`.
//!
//! A forked child talks to its master over a pipe with GNU R's framing: each message is its
//! length as a native-endian `R_xlen_t`, then that many bytes, and a zero length announces that
//! the child is exiting. A second pipe feeds the child's standard input. The runtime starts no
//! helper threads, so a fork copies the whole interpreter in a consistent state; what it must
//! not copy is unwritten output, which is flushed first so it is not written twice.

use {
    crate::{
        builtin_libraries::{NativeCall, NativeFn},
        coerce::coerce_vector,
        interp,
        unwind::R,
        values::{self, nil, scalar_bool, scalar_integer},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType},
    libc::{c_int, pid_t},
    std::io::Write,
};

pub const ROUTINES: &[(&str, NativeFn)] = &[
    ("nextStream", |call| unsafe {
        advance(call, &A1P127, &A2P127)
    }),
    ("nextSubStream", |call| unsafe {
        advance(call, &A1P76, &A2P76)
    }),
    ("mc_fork", |call| unsafe { fork(call) }),
    ("mc_send_master", |call| unsafe { send_master(call) }),
    ("mc_exit", |call| unsafe { exit(call) }),
    ("mc_read_child", |call| unsafe { read_child(call) }),
    ("mc_read_children", |call| unsafe { read_children(call) }),
    ("mc_select_children", |call| unsafe { select_children(call) }),
    ("mc_rm_child", |call| unsafe { remove_child(call) }),
    ("mc_kill", |call| unsafe { kill(call) }),
    ("mc_send_child_stdin", |call| unsafe { send_child_stdin(call) }),
    ("mc_children", |_| unsafe {
        reap();
        let pids: Vec<i32> = forks().children.iter().map(|child| child.pid).collect();
        values::integer_vector(&pids)
    }),
    ("mc_fds", |call| unsafe {
        let input = values::as_integer_scalar(call.arg(0)) != 0;
        let fds: Vec<i32> = forks()
            .children
            .iter()
            .map(|child| if input { child.input } else { child.output })
            .collect();
        values::integer_vector(&fds)
    }),
    ("mc_close_fds", |call| unsafe {
        let fds = coerce_vector(call.arg(0), SexpType::Integer)?;
        for &fd in values::ints(fds) {
            close_fd(fd);
        }
        scalar_bool(true)
    }),
    ("mc_close_stdout", |call| unsafe {
        redirect_standard(call, libc::STDOUT_FILENO)
    }),
    ("mc_close_stderr", |call| unsafe {
        redirect_standard(call, libc::STDERR_FILENO)
    }),
    ("mc_is_child", |_| unsafe { scalar_bool(forks().is_child) }),
    ("mc_master_fd", |_| unsafe {
        scalar_integer(forks().master.unwrap_or(-1))
    }),
    ("mc_interactive", |call| unsafe {
        let requested = values::as_logical_scalar(call.arg(0));
        if requested != heap::object::NA_LOGICAL {
            interp().interactive = requested != 0;
        }
        scalar_bool(interp().interactive)
    }),
    ("mc_affinity", |call| unsafe { affinity(call) }),
    ("mc_prepare_cleanup", |_| {
        forks().marks.push(forks().next_mark);
        forks().next_mark += 1;
        Ok(nil())
    }),
    ("mc_cleanup", |call| unsafe { cleanup(call) }),
];

/// The children this process forked and, in a child, its end of the pipe to the master.
#[derive(Default)]
pub struct Forks {
    /// Children whose result pipe is still open.
    children: Vec<Child>,
    /// Children no longer tended to (their pipe reached its end, they were removed, detached
    /// or estranged) that have not been waited for yet. Waiting only after the pipe is closed
    /// keeps a pid from being reused while it still names a tended child.
    unreaped: Vec<pid_t>,
    /// The marks `mc_prepare_cleanup` set, innermost last. `mc_cleanup` acts on the children
    /// forked since the innermost one.
    marks: Vec<u64>,
    next_mark: u64,
    /// The write end of the result pipe, in a child that has one.
    master: Option<c_int>,
    is_child: bool,
}

struct Child {
    pid: pid_t,
    /// The read end of the child's result pipe.
    output: c_int,
    /// The write end of the child's standard input.
    input: c_int,
    /// The innermost cleanup mark when the child was forked, if any.
    mark: Option<u64>,
}

fn forks() -> &'static mut Forks {
    &mut interp().forks
}

/// `mc_fork(estranged)`: forks the session. Returns the pid (0 in the child) and the two pipe
/// ends: in the master the child's result pipe and standard input, in a child its result pipe
/// and `NA`. An estranged child has no pipes, so both are `NA` on either side.
unsafe fn fork(call: &NativeCall<'_>) -> R<Sexp> {
    let estranged = values::as_logical_scalar(call.arg(0)) == 1;
    reap();
    // Buffered output would otherwise be written once by each process.
    crate::connections::flush_pending_writes()?;
    flush_standard();
    let pipes = if estranged {
        None
    } else {
        let result = make_pipe().ok_or_else(|| call.error("unable to create a pipe"))?;
        let Some(input) = make_pipe() else {
            close_fd(result[0]);
            close_fd(result[1]);
            return Err(call.error("unable to create a pipe for the child's standard input"));
        };
        Some((result, input))
    };
    let pid = libc::fork();
    if pid < 0 {
        let reason = std::io::Error::last_os_error();
        if let Some((result, input)) = pipes {
            result.into_iter().chain(input).for_each(close_fd);
        }
        return Err(call.error(format!("unable to fork, possible reason: {reason}")));
    }
    let state = forks();
    if pid == 0 {
        // The siblings' pipes belong to the master: holding a sibling's standard input open
        // here would keep it from ever seeing the end of its input.
        for child in state.children.drain(..) {
            close_fd(child.output);
            close_fd(child.input);
        }
        state.unreaped.clear();
        state.marks.clear();
        if let Some(master) = state.master.take() {
            close_fd(master);
        }
        state.is_child = true;
        let master = match pipes {
            Some((result, input)) => {
                close_fd(result[0]);
                close_fd(input[1]);
                libc::dup2(input[0], libc::STDIN_FILENO);
                close_fd(input[0]);
                state.master = Some(result[1]);
                result[1]
            }
            None => {
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
                if null >= 0 {
                    libc::dup2(null, libc::STDIN_FILENO);
                    close_fd(null);
                }
                NA_INTEGER
            }
        };
        return values::integer_vector(&[0, master, NA_INTEGER]);
    }
    match pipes {
        Some((result, input)) => {
            close_fd(result[1]);
            close_fd(input[0]);
            let mark = state.marks.last().copied();
            state.children.push(Child {
                pid,
                output: result[0],
                input: input[1],
                mark,
            });
            values::integer_vector(&[pid, result[0], input[1]])
        }
        None => {
            state.unreaped.push(pid);
            values::integer_vector(&[pid, NA_INTEGER, NA_INTEGER])
        }
    }
}

/// `mc_send_master(what)`: sends a raw vector to the master as one message.
unsafe fn send_master(call: &NativeCall<'_>) -> R<Sexp> {
    let state = forks();
    if !state.is_child {
        return Err(call.error("only children can send data to the master process"));
    }
    let Some(master) = state.master else {
        return Err(call.error("there is no pipe to the master process"));
    };
    let what = call.arg(0);
    if what.kind() != SexpType::Raw {
        return Err(call.error("content to send must be RAW, use serialize() if needed"));
    }
    let bytes = values::raws(what);
    let length = bytes.len() as isize;
    if write_all(master, &length.to_ne_bytes()).and_then(|()| write_all(master, bytes)).is_err()
    {
        close_fd(master);
        state.master = None;
        return Err(call.error("write error, closing pipe to the master"));
    }
    scalar_bool(true)
}

/// `mc_exit(code)`: announces the exit to the master and ends the child without running the
/// session's exit handlers, which belong to the master.
unsafe fn exit(call: &NativeCall<'_>) -> R<Sexp> {
    let code = values::as_integer_scalar(call.arg(0));
    let state = forks();
    if !state.is_child {
        return Err(call.error("'mcexit' can only be used in a child process"));
    }
    if let Some(master) = state.master.take() {
        // The master learns of the exit from this message or, failing it, from the pipe's end,
        // so a failed write changes nothing.
        drop(write_all(master, &0isize.to_ne_bytes()));
        close_fd(master);
    }
    flush_standard();
    libc::_exit(if code == NA_INTEGER { 0 } else { code })
}

/// `mc_read_child(pid)`: reads the next message from a child, waiting for it. Returns the raw
/// message with a `pid` attribute, or the pid alone once the child has exited, or `NULL` for a
/// process that is not a tended child.
unsafe fn read_child(call: &NativeCall<'_>) -> R<Sexp> {
    let pid = values::as_integer_scalar(call.arg(0));
    match forks().children.iter().position(|child| child.pid == pid) {
        Some(index) => read_message(index),
        None => Ok(nil()),
    }
}

/// `mc_read_children(timeout)`: reads one message from whichever child has one first. Returns
/// `NULL` when there are no children, `TRUE` on a timeout and `FALSE` on an error.
unsafe fn read_children(call: &NativeCall<'_>) -> R<Sexp> {
    let timeout = values::as_real_scalar(call.arg(0));
    match wait_readable(None, timeout, call)? {
        Readiness::NoChildren => Ok(nil()),
        Readiness::TimedOut => scalar_bool(true),
        Readiness::Failed => scalar_bool(false),
        Readiness::Ready(ready) => match ready.first() {
            Some(&pid) => match forks().children.iter().position(|child| child.pid == pid) {
                Some(index) => read_message(index),
                None => scalar_bool(true),
            },
            None => scalar_bool(true),
        },
    }
}

/// `mc_select_children(timeout, pids)`: waits until some of the children (all of them when
/// `pids` is empty) have a message or have exited. Returns their pids, `NULL` when none of them
/// is a tended child, `TRUE` on a timeout and `FALSE` on an error.
unsafe fn select_children(call: &NativeCall<'_>) -> R<Sexp> {
    let timeout = values::as_real_scalar(call.arg(0));
    let pids = coerce_vector(call.arg(1), SexpType::Integer)?;
    let pids = values::ints(pids).to_vec();
    let only = if pids.is_empty() { None } else { Some(&pids[..]) };
    match wait_readable(only, timeout, call)? {
        Readiness::NoChildren => Ok(nil()),
        Readiness::TimedOut => scalar_bool(true),
        Readiness::Failed => scalar_bool(false),
        Readiness::Ready(ready) => values::integer_vector(&ready),
    }
}

/// `mc_rm_child(pid)`: stops tending a child, closing its pipes. Returns whether it was one.
unsafe fn remove_child(call: &NativeCall<'_>) -> R<Sexp> {
    let pid = values::as_integer_scalar(call.arg(0));
    let state = forks();
    let Some(index) = state.children.iter().position(|child| child.pid == pid) else {
        return scalar_bool(false);
    };
    release(index);
    reap();
    scalar_bool(true)
}

/// `mc_kill(pid, signal)`.
unsafe fn kill(call: &NativeCall<'_>) -> R<Sexp> {
    let pid = values::as_integer_scalar(call.arg(0));
    let signal = values::as_integer_scalar(call.arg(1));
    if pid == NA_INTEGER || signal == NA_INTEGER || libc::kill(pid, signal) != 0 {
        return Err(call.error("'mckill' failed"));
    }
    scalar_bool(true)
}

/// `mc_send_child_stdin(pid, what)`: writes a raw vector to a child's standard input.
unsafe fn send_child_stdin(call: &NativeCall<'_>) -> R<Sexp> {
    let pid = values::as_integer_scalar(call.arg(0));
    let what = call.arg(1);
    if what.kind() != SexpType::Raw {
        return Err(call.error("what must be a raw vector"));
    }
    let Some(child) = forks().children.iter().find(|child| child.pid == pid && child.input >= 0)
    else {
        return Err(call.error(format!("child {pid} does not exist")));
    };
    if write_all(child.input, values::raws(what)).is_err() {
        return Err(call.error("write error"));
    }
    scalar_bool(true)
}

/// `mc_close_stdout(to.null)` and `mc_close_stderr(to.null)`: closes a standard stream, or
/// points it at `/dev/null` so writes to it still succeed.
unsafe fn redirect_standard(call: &NativeCall<'_>, fd: c_int) -> R<Sexp> {
    flush_standard();
    if values::as_logical_scalar(call.arg(0)) == 1 {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        if null < 0 {
            return Err(call.error("cannot open /dev/null"));
        }
        libc::dup2(null, fd);
        close_fd(null);
    } else {
        close_fd(fd);
    }
    Ok(nil())
}

/// `mc_affinity(cpus)`: restricts the process to the given one-based CPUs when `cpus` is not
/// `NULL`, then returns the CPUs it may run on.
unsafe fn affinity(call: &NativeCall<'_>) -> R<Sexp> {
    let requested = call.arg(0);
    if requested != nil() {
        if !matches!(requested.kind(), SexpType::Integer | SexpType::Real) {
            return Err(call.error("invalid CPU affinity specification"));
        }
        let cpus = coerce_vector(requested, SexpType::Integer)?;
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &cpu in values::ints(cpus) {
            if cpu < 1 || cpu == NA_INTEGER {
                return Err(call.error("invalid CPU affinity specification"));
            }
            if (cpu as usize) <= libc::CPU_SETSIZE as usize {
                libc::CPU_SET(cpu as usize - 1, &mut set);
            }
        }
        if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(call.error("unable to set CPU affinity"));
        }
    }
    let mut set: libc::cpu_set_t = std::mem::zeroed();
    if libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) != 0 {
        if requested == nil() {
            return Err(call.error("unable to obtain CPU affinity"));
        }
        return Ok(nil());
    }
    let cpus: Vec<i32> = (0..libc::CPU_SETSIZE as usize)
        .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
        .map(|cpu| cpu as i32 + 1)
        .collect();
    values::integer_vector(&cpus)
}

/// `mc_cleanup(kill, detach, shutdown)`: signals (`kill = TRUE` sends `SIGTERM`, a number sends
/// that signal) and, with `detach`, stops tending the children forked since the innermost
/// `mc_prepare_cleanup` mark, or all children when there is none. A call that signals or
/// detaches ends the mark's scope; `mccollect`'s `cleanup(FALSE, FALSE)` leaves it in place, so
/// the `mclapply` around it still cleans up its own children and no others.
unsafe fn cleanup(call: &NativeCall<'_>) -> R<Sexp> {
    let signal_argument = call.arg(0);
    let signal = match signal_argument.kind() {
        SexpType::Logical => (values::as_logical_scalar(signal_argument) == 1).then_some(libc::SIGTERM),
        SexpType::Integer | SexpType::Real => {
            Some(values::as_integer_scalar(signal_argument)).filter(|&signal| signal != NA_INTEGER)
        }
        _ => None,
    };
    let detach = values::as_logical_scalar(call.arg(1)) == 1;
    let state = forks();
    let mark = state.marks.last().copied();
    let in_scope = |child: &Child| mark.is_none_or(|mark| child.mark.is_some_and(|own| own >= mark));
    if let Some(signal) = signal {
        for child in state.children.iter().filter(|child| in_scope(child)) {
            libc::kill(child.pid, signal);
        }
    }
    if detach {
        let mut index = 0;
        while let Some(child) = forks().children.get(index) {
            if in_scope(child) {
                release(index);
            } else {
                index += 1;
            }
        }
    }
    if signal.is_some() || detach {
        forks().marks.pop();
    }
    reap();
    Ok(nil())
}

enum Readiness {
    NoChildren,
    TimedOut,
    Failed,
    Ready(Vec<i32>),
}

/// Waits until a tended child among `only` (all of them for `None`) has a message or has closed
/// its pipe. A negative or `NaN` timeout waits indefinitely. An interrupted wait counts as a
/// timeout, as GNU R's does: the signal is usually a child's exit, and the caller loops.
unsafe fn wait_readable(only: Option<&[i32]>, timeout: f64, call: &NativeCall<'_>) -> R<Readiness> {
    reap();
    let mut polled: Vec<libc::pollfd> = forks()
        .children
        .iter()
        .filter(|child| only.is_none_or(|only| only.contains(&child.pid)))
        .map(|child| libc::pollfd {
            fd: child.output,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    if polled.is_empty() {
        return Ok(Readiness::NoChildren);
    }
    let milliseconds = if timeout.is_nan() || timeout < 0.0 {
        -1
    } else {
        (timeout * 1000.0).ceil().min(f64::from(i32::MAX)) as c_int
    };
    let ready = libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, milliseconds);
    if ready < 0 {
        let failure = std::io::Error::last_os_error();
        if failure.kind() == std::io::ErrorKind::Interrupted {
            return Ok(Readiness::TimedOut);
        }
        call.warning(format!("error '{failure}' in select"))?;
        return Ok(Readiness::Failed);
    }
    if ready == 0 {
        return Ok(Readiness::TimedOut);
    }
    let pids = polled
        .iter()
        .filter(|entry| entry.revents != 0)
        .filter_map(|entry| forks().children.iter().find(|child| child.output == entry.fd))
        .map(|child| child.pid)
        .collect();
    Ok(Readiness::Ready(pids))
}

/// Reads one message from the child at `index`, releasing the child when its pipe announces or
/// reaches its end.
unsafe fn read_message(index: usize) -> R<Sexp> {
    let Some(child) = forks().children.get(index) else {
        return Ok(nil());
    };
    let (pid, fd) = (child.pid, child.output);
    let mut header = [0u8; size_of::<isize>()];
    let length = match read_exact(fd, &mut header) {
        Ok(()) => isize::from_ne_bytes(header),
        Err(_) => 0,
    };
    if length <= 0 {
        release(index);
        reap();
        return scalar_integer(pid);
    }
    let message = values::alloc(SexpType::Raw, length as usize)?;
    if read_exact(fd, values::raws_mut(message)).is_err() {
        release(index);
        reap();
        return scalar_integer(pid);
    }
    values::set_attr(message, values::install("pid")?, scalar_integer(pid)?)?;
    Ok(message)
}

/// Stops tending the child at `index`: its pipes close and it waits to be reaped.
fn release(index: usize) {
    let state = forks();
    if index < state.children.len() {
        let child = state.children.remove(index);
        close_fd(child.output);
        close_fd(child.input);
        state.unreaped.push(child.pid);
    }
}

/// Waits for every released child that has exited, so none lingers as a zombie.
fn reap() {
    forks().unreaped.retain(|&pid| {
        let mut status = 0;
        // SAFETY: waiting on one of this process's own children.
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) == 0 }
    });
}

fn make_pipe() -> Option<[c_int; 2]> {
    let mut fds = [0; 2];
    // Close-on-exec, so a `system()` command started later does not hold the pipes open.
    // SAFETY: `fds` has room for both ends.
    (unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == 0).then_some(fds)
}

fn close_fd(fd: c_int) {
    if fd >= 0 {
        // SAFETY: closing a descriptor this module owns; a failed close leaves nothing to undo.
        unsafe { libc::close(fd) };
    }
}

fn write_all(fd: c_int, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: the pointer and length describe `bytes`.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            let failure = std::io::Error::last_os_error();
            if failure.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(failure);
        }
        bytes = bytes.get(written as usize..).unwrap_or_default();
    }
    Ok(())
}

fn read_exact(fd: c_int, buffer: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        let rest = &mut buffer[filled..];
        // SAFETY: the pointer and length describe the unfilled part of `buffer`.
        let read = unsafe { libc::read(fd, rest.as_mut_ptr().cast(), rest.len()) };
        if read < 0 {
            let failure = std::io::Error::last_os_error();
            if failure.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(failure);
        }
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        filled += read as usize;
    }
    Ok(())
}

fn flush_standard() {
    // Output that cannot be flushed has nowhere else to go, as in GNU R.
    drop(std::io::stdout().flush());
    drop(std::io::stderr().flush());
}

const M1: u64 = 4_294_967_087;
const M2: u64 = 4_294_944_443;

/// The RngStreams jump matrices: `A1P127` and `A2P127` advance a seed by 2^127 steps (to the
/// next stream), `A1P76` and `A2P76` by 2^76 (to the next substream).
const A1P127: [[u64; 3]; 3] = [
    [2_427_906_178, 3_580_155_704, 949_770_784],
    [226_153_695, 1_230_515_664, 3_580_155_704],
    [1_988_835_001, 986_791_581, 1_230_515_664],
];
const A2P127: [[u64; 3]; 3] = [
    [1_464_411_153, 277_697_599, 1_610_723_613],
    [32_183_930, 1_464_411_153, 1_022_607_788],
    [2_824_425_944, 32_183_930, 2_093_834_863],
];
const A1P76: [[u64; 3]; 3] = [
    [82_758_667, 1_871_391_091, 4_127_413_238],
    [3_672_831_523, 69_195_019, 1_871_391_091],
    [3_672_091_415, 3_528_743_235, 69_195_019],
];
const A2P76: [[u64; 3]; 3] = [
    [1_511_326_704, 3_759_209_742, 1_610_795_712],
    [4_292_754_251, 1_511_326_704, 3_889_917_532],
    [3_859_662_829, 4_292_754_251, 3_708_466_080],
];

/// Multiplies the two halves of a `.Random.seed` (the kind code, then six seed words) by the
/// jump matrices. The arithmetic wraps in 64 bits exactly as GNU R's C code does, so the
/// streams match GNU R's bit for bit.
unsafe fn advance(call: &NativeCall<'_>, first: &[[u64; 3]; 3], second: &[[u64; 3]; 3]) -> R<Sexp> {
    let seed = coerce_vector(call.arg(0), SexpType::Integer)?;
    let seed = values::ints(seed);
    if seed.len() < 7 {
        return Err(call.error("invalid seed: a L'Ecuyer-CMRG seed has 7 integers"));
    }
    let word = |index: usize| u64::from(seed[index + 1] as u32);
    let mut next = [0i32; 7];
    next[0] = seed[0];
    for (half, (matrix, modulus)) in [(first, M1), (second, M2)].into_iter().enumerate() {
        for (row, coefficients) in matrix.iter().enumerate() {
            let mut sum: u64 = 0;
            for (column, coefficient) in coefficients.iter().enumerate() {
                sum = sum.wrapping_add(coefficient.wrapping_mul(word(half * 3 + column)));
                sum %= modulus;
            }
            next[1 + half * 3 + row] = sum as u32 as i32;
        }
    }
    values::integer_vector(&next)
}
