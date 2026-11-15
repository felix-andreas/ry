//! Running a typed call again in the interpreter after its native code gave up partway, without
//! making its R calls a second time.
//!
//! Native code records the value of each R call it makes that may have done something visible
//! ([`record`]). When the code gives up after such a call, the interpreter runs the whole call
//! again ([`replay`]) on a copy of the closure's body in which each recorded call is wrapped in
//! `.ry_replay`, which gives the recorded value instead of calling R. Typed code computes nothing
//! but what its arguments and its R calls give, so the interpreter reaches the recorded calls in
//! the order the native code made them, and it carries on past the point where the native code
//! stopped. A typed function the replayed call calls replays its own recorded calls the same way,
//! in the interpreter or in native code ([`replayed`]).

use {
    crate::{
        annotations,
        builtins::{Arg, BuiltinDef, Call, special},
        bytecode,
        eval,
        unwind::R,
        values::{self, nil},
    },
    heap::{
        Marker,
        object::{Sexp, SexpType, object_size},
    },
    rustc_hash::FxHashSet,
    std::cell::RefCell,
};

pub const BUILTINS: &[BuiltinDef] = &[special(".ry_replay", 2, do_replay)];

/// The recorded calls of one native call, which [`close`] or [`replay`] ends.
#[must_use]
pub struct Segment {
    start: usize,
    bytes: usize,
    overflowed: bool,
}

/// How many R calls one native call records at most. A call that makes more cannot run again in
/// the interpreter, so giving up after them is an error.
pub const MAX_RECORDED: usize = 4096;

/// How many MiB of values one native call's records keep alive at most, counted as each value's
/// own bytes and those of a list's elements. A loop that calls R for a large vector each time
/// would otherwise keep thousands of them alive; past the limit the call records no more, as past
/// [`MAX_RECORDED`].
pub const MAX_RECORDED_MIB: usize = 64;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[derive(Default)]
struct State {
    /// The recorded calls of every native call under way, the innermost last.
    recorded: Vec<Recorded>,
    /// Where the innermost native call's recorded calls start.
    start: usize,
    /// The bytes of the values the innermost native call's records keep alive.
    bytes: usize,
    /// How many native calls are under way.
    open: usize,
    /// Whether the innermost native call made more R calls than it records.
    overflowed: bool,
    /// The calls running again in the interpreter, the innermost last.
    replays: Vec<Replay>,
}

/// An R call native code made: the call as written in the body, and the value it returned, or
/// `NULL` when the code did not use the value.
#[derive(Clone, Copy)]
struct Recorded {
    code: Sexp,
    value: Sexp,
}

struct Replay {
    /// The recorded calls in the order the native code made them. One the replay has used has
    /// `NULL` for its code.
    recorded: Vec<Recorded>,
    /// The first recorded call not used yet.
    next: usize,
    /// How many recorded calls are not used yet.
    remaining: usize,
}

/// Starts recording the R calls of a native call.
pub fn open() -> Segment {
    STATE.with_borrow_mut(|state| {
        state.open += 1;
        Segment {
            start: std::mem::replace(&mut state.start, state.recorded.len()),
            bytes: std::mem::take(&mut state.bytes),
            overflowed: std::mem::take(&mut state.overflowed),
        }
    })
}

/// Ends the native call `segment` opened, which returned or raised an error, so its recorded
/// calls are not needed: a caller that runs again replays the R call that ran this one whole.
pub fn close(segment: Segment) {
    STATE.with_borrow_mut(|state| {
        state.recorded.truncate(state.start);
        restore(state, segment);
    });
}

fn restore(state: &mut State, segment: Segment) {
    state.start = segment.start;
    state.bytes = segment.bytes;
    state.overflowed = segment.overflowed;
    state.open -= 1;
}

/// Records that the native code under way made the R call `code`, which returned `value`, or
/// `NULL` when the code does not use the value. The record keeps `value` alive ([`trace`]).
pub fn record(code: Sexp, value: Sexp) {
    STATE.with_borrow_mut(|state| {
        if state.open == 0 || state.overflowed {
            return;
        }
        state.bytes = state.bytes.saturating_add(kept_bytes(value));
        if state.recorded.len() - state.start >= MAX_RECORDED
            || state.bytes > MAX_RECORDED_MIB << 20
        {
            state.recorded.truncate(state.start);
            state.overflowed = true;
            return;
        }
        state.recorded.push(Recorded { code, value });
    });
}

/// The bytes `value` keeps alive: its own, and those of its elements when it is a list.
fn kept_bytes(value: Sexp) -> usize {
    // SAFETY: `value` is a live object an R call returned.
    unsafe {
        let own = |value: Sexp| {
            object_size(value.kind(), values::length(value)).unwrap_or(0)
        };
        let mut bytes = own(value);
        if value.kind() == SexpType::List {
            for index in 0..values::length(value) {
                bytes = bytes.saturating_add(own(value.element(index)));
            }
        }
        bytes
    }
}

/// The value the R call `code` returned when native code made it before giving up, when a
/// replay has not used it yet: the innermost replay's first one for `code`.
pub fn replayed(code: Sexp) -> Option<Sexp> {
    STATE.with_borrow_mut(|state| {
        state.replays.iter_mut().rev().find_map(|replay| {
            if replay.remaining == 0 {
                return None;
            }
            let index = replay.next
                + replay.recorded[replay.next..]
                    .iter()
                    .position(|recorded| recorded.code == code)?;
            let value = replay.recorded[index].value;
            // The recorded value stands for the whole call, so the calls in its arguments that
            // the native code made first are not evaluated now and must not be used later, where
            // a loop reaches the same code again.
            for earlier in replay.next..=index {
                let recorded = &mut replay.recorded[earlier];
                if recorded.code != nil() && (earlier == index || holds(code, recorded.code)) {
                    recorded.code = nil();
                    replay.remaining -= 1;
                }
            }
            while replay
                .recorded
                .get(replay.next)
                .is_some_and(|recorded| recorded.code == nil())
            {
                replay.next += 1;
            }
            Some(value)
        })
    })
}

/// Whether the call `code` holds the call `inner` at any depth.
fn holds(code: Sexp, inner: Sexp) -> bool {
    // SAFETY: the caller's code is a live call of a closure body.
    unsafe {
        if code.kind() != SexpType::Language {
            return false;
        }
        let mut cell = code;
        while matches!(cell.kind(), SexpType::Language | SexpType::Pairlist) {
            if cell.car() == inner || holds(cell.car(), inner) {
                return true;
            }
            cell = cell.cdr();
        }
        false
    }
}

/// Runs the call `call` of the typed closure `function` with `args` in the interpreter, after
/// its native code, whose R calls `segment` recorded, gave up partway. `None` when the code made
/// more R calls than it records, so running it again would make some twice.
pub unsafe fn replay(
    segment: Segment,
    call: Sexp,
    function: Sexp,
    args: &[Arg],
    sys_parent: Sexp,
) -> Option<R<Sexp>> {
    let recorded = STATE.with_borrow_mut(|state| {
        let recorded = state.recorded.split_off(state.start);
        let overflowed = state.overflowed;
        restore(state, segment);
        (!overflowed).then_some(recorded)
    })?;
    STATE.with_borrow_mut(|state| {
        state.replays.push(Replay {
            remaining: recorded.len(),
            recorded,
            next: 0,
        })
    });
    let result = eval::run_interpreted(call, function, args, sys_parent);
    STATE.with_borrow_mut(|state| state.replays.pop());
    Some(result)
}

/// Marks the values of the recorded calls, which no handle holds: a native loop that makes many
/// R calls keeps at most [`MAX_RECORDED`] of them, and [`MAX_RECORDED_MIB`], alive.
///
/// # Safety
///
/// Called while the collector marks, on the interpreter's thread.
pub unsafe fn trace(marker: &mut Marker<'_>) {
    STATE.with_borrow(|state| {
        let replays = state.replays.iter().flat_map(|replay| &replay.recorded);
        for recorded in state.recorded.iter().chain(replays) {
            // SAFETY: a recorded value is a live object the native call that made it returned.
            unsafe { marker.mark(recorded.value) };
        }
    });
}

/// The body a call of `function` evaluates while a replay has recorded calls left to use: its
/// own with each of those calls wrapped in `.ry_replay`. `None` when there is nothing to replay
/// or `function` is not typed, and the call runs its own body.
pub unsafe fn body(function: Sexp) -> R<Option<Sexp>> {
    if !annotations::is_typed(function) {
        return Ok(None);
    }
    let pending: FxHashSet<Sexp> = STATE.with_borrow(|state| {
        state
            .replays
            .iter()
            .flat_map(|replay| &replay.recorded[replay.next..])
            .map(|recorded| recorded.code)
            .filter(|&code| code != nil())
            .collect()
    });
    if pending.is_empty() {
        return Ok(None);
    }
    let body = bytecode::closure_body(function);
    let replay = values::install(".ry_replay")?;
    let rewritten = rewritten(body, &pending, replay)?;
    Ok((rewritten != body).then_some(rewritten))
}

/// `code` with each call in `pending` wrapped as `.ry_replay(rewritten, call)`, where `rewritten`
/// is the call with its own pending calls wrapped. Code that holds none is returned as it is.
unsafe fn rewritten(code: Sexp, pending: &FxHashSet<Sexp>, replay: Sexp) -> R<Sexp> {
    if code.kind() != SexpType::Language {
        return Ok(code);
    }
    let heap = crate::heap();
    let head = rewritten(code.car(), pending, replay)?;
    heap.root(head);
    let mut items: smallvec::SmallVec<[(Sexp, Sexp, Sexp); 8]> = smallvec::SmallVec::new();
    let mut changed = head != code.car();
    let mut cell = code.cdr();
    while cell.kind() == SexpType::Pairlist {
        let item = rewritten(cell.car(), pending, replay)?;
        heap.root(item);
        changed |= item != cell.car();
        items.push((item, cell.tag(), cell.attrib()));
        cell = cell.cdr();
    }
    let mut copy = code;
    if changed {
        let mut arguments = cell;
        for &(item, tag, attrib) in items.iter().rev() {
            arguments = values::cons_tagged(item, arguments, tag)?;
            heap.set_attrib(arguments, attrib);
            heap.root(arguments);
        }
        copy = values::lcons(head, arguments)?;
        heap.set_attrib(copy, code.attrib());
        heap.root(copy);
    }
    if !pending.contains(&code) {
        return Ok(copy);
    }
    let original = values::cons(code, nil())?;
    heap.root(original);
    let arguments = values::cons(copy, original)?;
    heap.root(arguments);
    let wrapped = values::lcons(replay, arguments)?;
    heap.root(wrapped);
    Ok(wrapped)
}

/// `.ry_replay(code, call)`: the value the R call `call` returned when native code made it, when
/// a replay has it, and otherwise the value of `code`.
unsafe fn do_replay(call: &Call<'_>) -> R<Sexp> {
    call.check_arity(2)?;
    match replayed(call.arg(1)) {
        Some(value) => {
            crate::interp().visible = true;
            Ok(value)
        }
        None => eval::eval(call.arg(0), call.env()?),
    }
}
