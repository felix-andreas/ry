//! ry's typed core: native values (`values`) and the compiler that turns a strict typed closure
//! into machine code over them.
//!
//! A call of a typed closure from R reaches [`call`], which compiles the closure once for the
//! argument values' types (lowering its body to the IR, inserting the reference counts and
//! generating code), converts the arguments to native values, runs the code and converts its
//! value back. What the compiler cannot handle, and every call whose code gives up partway, runs in
//! the interpreter when [`call`] returns `None`. Code gives up before anything it did is visible,
//! so rerunning from the start is correct, except after it called an R function ([`ry_call_r`]),
//! which may have done something visible: from then on, giving up is an error instead of a second
//! run.

#![allow(unsafe_op_in_unsafe_fn)]
// Every `unsafe fn` here has one contract: the handles it takes are live objects of this heap.
#![allow(clippy::missing_safety_doc)]
// The lowering's `to_*` helpers take `&mut self` because they emit code, and its builder callbacks
// have long closure types.
#![allow(
    clippy::wrong_self_convention,
    clippy::type_complexity,
    clippy::explicit_counter_loop,
    clippy::needless_borrow,
    clippy::unnecessary_cast
)]

pub mod ahead;
pub mod backend;
pub mod file;
pub mod fusion;
pub mod ir;
pub mod loops;
pub mod lower;
pub mod ownership;
pub mod ranges;
pub mod shapes;
pub mod unbox;
pub mod values;

use {
    crate::{
        backend::{Callee, Code, Entry, Trampoline},
        ir::{Item, Type},
        lower::{Callees, Global, Lowered, Passed, RCall, RResult, Speculation},
        shapes::Shapes,
        values::{Boxed, List, Str},
    },
    heap::{
        Collection,
        object::{INTEGER_PREFIX, LOGICAL_PREFIX, Sexp, SexpType, Slot, gp},
        records::NativeKind,
        strings::Encoding,
    },
    interp::{
        annotations::decline,
        arrays::{Items, checked_conversion, checked_missing},
        builtins::Arg,
        bytecode::TypedCall,
        records::NativeValues,
        replay,
        unwind::Unwind,
        values as rvalues,
    },
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::{
        borrow::Cow,
        cell::{Cell, RefCell},
        mem::MaybeUninit,
        sync::{
            OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    },
};

/// The lowest frame address compiled code may reach, which every compiled function compares
/// its frame against on entry, so a deep recursion gives up before the machine stack runs out.
/// The native entry sets it for the stack segment it runs on ([`NATIVE_STACK`]); zero means no
/// limit is known.
pub static STACK_LIMIT: AtomicUsize = AtomicUsize::new(0);

/// The value a compiled function's prologue leaves in [`STACK_LIMIT`] when it stops for want of
/// depth budget or stack, above every frame address so that nothing compiled runs again before
/// the native entry restores the limit.
pub const EXHAUSTED_LIMIT: usize = usize::MAX;

/// The stack the native entry makes sure of before it runs compiled code, growing onto a fresh
/// segment of [`NATIVE_SEGMENT`] when less is left: the interpreter keeps only a small red zone,
/// so compiled code entered from deep in an interpreted recursion would otherwise start near
/// the end of its segment.
const NATIVE_STACK: usize = 1024 * 1024;
const NATIVE_SEGMENT: usize = 8 * 1024 * 1024;
/// The stack below [`STACK_LIMIT`] left for the runtime functions the deepest frame calls.
const NATIVE_RESERVE: usize = 128 * 1024;

/// Whether a native entry runs inside a call that the interpreter runs again because its native
/// code ran out of stack or of R's nesting depth: entering native code there would recurse as
/// deep and stop again at every level. The JIT's scalar natives ask too, before they run. An
/// entry outside that call forgets it.
pub unsafe fn inside_exhausted_rerun() -> bool {
    let Some(rerun) = EXHAUSTED.get() else {
        return false;
    };
    // A native entry runs before the interpreter pushes the call's frame, so the frame of the
    // call being run again sits at the depth its entry saw while that call runs.
    let inside = interp::interp()
        .frames
        .get(rerun.depth)
        .is_some_and(|frame| {
            frame.call.raw() == rerun.call && frame.function.raw() == rerun.function
        });
    if inside {
        decline(|| {
            String::from(
                "it runs inside a call whose native code ran out of stack or of R's nesting \
                 depth, which the interpreter is running instead",
            )
        });
    } else {
        EXHAUSTED.set(None);
    }
    inside
}

/// The lowest frame address native code started on the caller's stack segment may reach, or 0
/// when the segment cannot be measured. The JIT's scalar native functions take it as an
/// argument and compare their frames against it, as typed code compares against
/// [`STACK_LIMIT`].
pub fn native_stack_limit() -> usize {
    match interp::eval::stack_floor() {
        0 => 0,
        floor => floor + NATIVE_RESERVE,
    }
}

/// Why a closure does not compile, for the log.
#[derive(Clone, Debug)]
pub struct Refusal(pub Cow<'static, str>);

impl Refusal {
    pub const fn new(why: &'static str) -> Self {
        Refusal(Cow::Borrowed(why))
    }
}

impl From<ssa::Error> for Refusal {
    fn from(error: ssa::Error) -> Self {
        Refusal::new(error.0)
    }
}

/// A closure compiled for one list of argument types.
pub struct Compiled {
    pub code: Code,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
    /// The function lookups this code and every function it calls assume.
    pub speculations: Vec<Speculation>,
    /// The [`shapes::epoch`] whose confirmed shapes a `match` over any tag in this code, or in a
    /// function it calls, tests for. The code is stale once more shapes are confirmed.
    pub epoch: Option<u64>,
    /// Whether the code, or a function it calls, calls an R function.
    pub calls_r: bool,
    /// The parameters, as bits by position, that the code reads on every path before it makes a
    /// call that may run R code ([`reads_before_calling_r`]).
    pub read_before_r: u64,
    /// The parameters whose arguments the native entry may read ahead from variables although
    /// the code calls R before it reads them, by watching those variables ([`Watching`]): each
    /// R call comes, on every path, before the parameter's first read or after it, and every R
    /// call is the code's own, since a typed function it calls that calls R has sites of its own.
    pub watchable: u64,
    /// The parameters the code passes among the arguments of an R call that may run R code
    /// before it has read them on every path ([`lower::ReadsBefore::passed_late`]).
    pub passed_late: u64,
    /// The parameters some path reads before it returns, outside the arguments of calls that
    /// force them later ([`lower::ReadsBefore::returned`]), whose arguments a call that returns
    /// settles.
    pub returned: u64,
    /// The addresses of the code's own R call sites, from the first to past the last, whose
    /// [`RCall::read_before`] name the parameters of the call the native entry made. Empty when
    /// the code calls itself, since a site then runs for an activation of other arguments.
    pub sites: (usize, usize),
    /// The [`interp::numbers::conversion_epoch`] at which this code, or a function it calls,
    /// took the type of a conversion from what the interpreter saw it give
    /// ([`lower::Conversion::guessed`]). The code is stale once a conversion has given another
    /// type.
    pub guessed: Option<u64>,
    /// The functions this code, or a function it calls, calls through a cell ([`Entry::Cell`])
    /// because they were being compiled, each with the result type the call assumed. The code
    /// is right only while each of them has current code of that result type.
    pub cells: SmallVec<[(Key, Type); 1]>,
    /// Whether R returns the value invisibly ([`lower::returns_invisibly`]).
    pub invisible: bool,
}

type Key = (Sexp, SmallVec<[Type; 4]>);

thread_local! {
    /// Compiled closures and refusals, by closure and argument types.
    static COMPILED: RefCell<FxHashMap<Key, Result<&'static Compiled, Refusal>>> =
        RefCell::new(FxHashMap::default());
    /// The closures being compiled, each with the result type its recursive calls assume while
    /// it is lowered, which a call from a function compiled meanwhile assumes too.
    static COMPILING: RefCell<FxHashMap<Key, Option<Type>>> = RefCell::new(FxHashMap::default());
    /// A word per closure, argument types and result type that some code calls through
    /// ([`Entry::Cell`]), holding the address of the latest code with that result, or 0 while
    /// there is none. A cell never holds code of another result, so code that called through it
    /// while the closure recompiled to another result still gets the type it was compiled for.
    static CELLS: RefCell<FxHashMap<(Key, Type), &'static Cell<usize>>> =
        RefCell::new(FxHashMap::default());
    /// How many calls through a cell lowerings have linked, which tells a compilation that
    /// refused whether its refusal rests on a guess that the function being compiled may revise.
    static CELL_CALLS: Cell<u64> = const { Cell::new(0) };
    /// The refusals that wait on the interpreter, with what each waits for: the closure compiles
    /// again once the interpreter has done it.
    static AWAITING: RefCell<FxHashMap<Key, lower::Awaited>> = RefCell::new(FxHashMap::default());
    /// The live native objects at which [`from_box`] next runs a collection.
    static NATIVE_COLLECTION_AT: Cell<i64> = const { Cell::new(NATIVE_COLLECTION_FLOOR) };
    /// How long the compilations nested in the one under way took, measured only for the log.
    static NESTED_COMPILE: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    /// How many R calls compiled code has made, which tells [`call`] whether code that gave up
    /// did anything visible first.
    static R_CALLS: Cell<u64> = const { Cell::new(0) };
    /// Calls whose native code stopped for the interpreter to run them again.
    static STOPS: Cell<u64> = const { Cell::new(0) };
    /// The error an R call from compiled code raised, which [`call`] raises once the code has
    /// given up.
    static PARKED: RefCell<Option<Unwind>> = const { RefCell::new(None) };
    /// The conversion whose value had a type its compiled code was not compiled for, with that
    /// type, which made the code give up.
    static FAILED_GUESS: Cell<Option<(Sexp, SexpType)>> = const { Cell::new(None) };
    /// The interpreter's call whose compiled code is running, which an R call the code makes
    /// runs in a frame of. A typed callee that compiled code calls directly has no call of its
    /// own, so its R calls see its caller's.
    static RUNNING: Cell<Option<Running>> = const { Cell::new(None) };
    /// The arguments the running code read ahead from variables that an R call it makes could
    /// change before the code reads them ([`Watching`]).
    static WATCHING: Cell<Option<Watching>> = const { Cell::new(None) };
    /// Whether an R call changed a variable an argument the code had not read yet was read
    /// from, which made the code give up.
    static WATCH_TRIPPED: Cell<bool> = const { Cell::new(false) };
    /// The call the interpreter is running again because its native code ran out of depth
    /// budget or stack. Every native entry inside it declines: each would recurse as deep and
    /// stop again, which made a deep recursion quadratic.
    static EXHAUSTED: Cell<Option<Rerun>> = const { Cell::new(None) };
}

/// A call's interpreter frame, by its position on the frame stack and what it calls.
#[derive(Clone, Copy)]
struct Rerun {
    depth: usize,
    call: Sexp,
    function: Sexp,
}

/// A [`TypedCall`] without its lifetime, for [`RUNNING`], which holds it only while the call
/// runs.
#[derive(Clone, Copy)]
struct Running {
    call: Sexp,
    function: Sexp,
    args: *const Arg,
    count: usize,
    sys_parent: Sexp,
}

/// What the native entry read when it read a call's arguments ahead without forcing them.
#[derive(Default)]
pub struct ReadAhead {
    /// The arguments, as bits by position, whose values depend on variables that cannot be
    /// watched, as a left-out default's do: R evaluates it in the call's frame when the body
    /// first reads the parameter.
    pub fixed: u64,
    /// The formals, as bits by position, that the call leaves out before a later one it passes.
    /// Each has a default, which the typed core's code computes ([`Type::Absent`]).
    pub absent: u64,
    /// Each unforced promise read, with the value forcing it gives and the position of the
    /// argument it was read for. A call that succeeds settles each as forcing would have.
    pub promises: SmallVec<[(Sexp, Sexp, u32); 4]>,
    /// Each variable an argument's value was read from.
    pub watches: SmallVec<[Watch; 4]>,
}

/// A variable the argument at `param` was read from while it was bound to `bound`.
#[derive(Clone, Copy)]
pub struct Watch {
    pub variable: Watched,
    pub bound: Sexp,
    pub param: u32,
}

/// Where a [`Watch`]'s variable is: a name in an environment, or the variable a slot frame's
/// argument reference names ([`interp::eval::reference_variable`]).
#[derive(Clone, Copy)]
pub enum Watched {
    Variable { symbol: Sexp, env: Sexp },
    Reference(Sexp),
}

impl ReadAhead {
    /// The arguments, as bits by position, whose values depend on a variable. A position past
    /// the mask's width sets every bit.
    fn lazy(&self) -> u64 {
        self.watches
            .iter()
            .fold(self.fixed, |bits, watch| bits | param_bit(watch.param))
    }
}

fn param_bit(param: u32) -> u64 {
    if param < 64 { 1 << param } else { u64::MAX }
}

/// What running code watches while it calls R ([`WATCHING`]): the variables its arguments were
/// read from, and the parameters, as bits, it has settled. R forces an argument where the body
/// first reads it, so before each R call the code settles the arguments it has read
/// ([`RCall::read_before`]), and after each it checks the variables of the arguments it has not:
/// one that changed makes the code give up, and the interpreter runs the call again, forcing
/// that argument where R would.
#[derive(Clone, Copy)]
struct Watching {
    ahead: *const ReadAhead,
    settled: u64,
    /// The parameters whose variables the code checks after each R call.
    late: u64,
    /// The code's own sites ([`Compiled::sites`]).
    sites: (usize, usize),
}

impl Watching {
    fn owns(&self, site: &RCall) -> bool {
        let address = std::ptr::from_ref(site) as usize;
        (self.sites.0..self.sites.1).contains(&address)
    }
}

/// How many closures a session compiles at most, since each keeps its closure alive. The
/// functions compiled before a script starts are not counted ([`ahead::compiled_ahead`]).
const MAX_COMPILED: usize = 4096;

/// How many refusals the table keeps before it forgets them. A refusal is keyed by a closure
/// that is not rooted, so its address may be reused by another closure that deserves a try.
const MAX_REFUSALS: usize = 65536;

/// The types a recursive call's result is first assumed to have, when the closure declares
/// none. Each lowering that finds a different result type retries with the one it found.
const GUESSES: [Type; 5] = [
    Type::Dbl,
    Type::Int,
    Type::Lgl,
    Type::Str,
    Type::Tag(shapes::ANY),
];

/// How often a recursive closure's lowering may revise its result type.
const MAX_REVISIONS: usize = 3;

/// Calls the typed closure of `typed` with argument values `values`, one per formal in order,
/// through its compiled code. `None` when the closure does not compile for these values or the
/// code gives up, and the caller then makes the call in the interpreter.
///
/// `ahead` is what reading the arguments ahead read. An argument whose value depends on variables
/// is a promise R forces only where the body reads it, so code that calls R first could change
/// what forcing it gives: the code watches those variables ([`Watching`]).
pub unsafe fn call(
    typed: &TypedCall<'_>,
    values: &[Sexp],
    ahead: &ReadAhead,
) -> Option<Result<Sexp, Unwind>> {
    let function = typed.function;
    if !interp::annotations::is_typed(function) {
        return None;
    }
    if function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE) {
        decline(|| String::from("it is being debugged or traced"));
        return None;
    }
    // Converting a record or a tag confirms its shapes, which the code compiled next must
    // consider.
    let mut params: SmallVec<[Type; 4]> = SmallVec::new();
    let mut arguments = Arguments::default();
    // `values` holds the passed formals' values in order, and `params` every formal up to the
    // last passed one, at its position.
    let mut position = 0;
    for &value in values {
        while position < 64 && ahead.absent & (1 << position) != 0 {
            params.push(Type::Absent);
            position += 1;
        }
        let formal = position;
        position += 1;
        let found = type_of(value);
        let converted = match found {
            // An `I64` formal converts a whole R number at the boundary, which `Arguments::push`
            // does, or gives up for the interpreter to raise the error.
            Some(Type::Int | Type::Dbl | Type::I64)
                if interp::annotations::declares_i64(function, formal) =>
            {
                arguments.push(value, Type::I64).map(|()| Type::I64)
            }
            // A length-one vector passed where the closure declares a vector is that vector, not
            // a scalar.
            Some(ty) => {
                let ty = match ty.item() {
                    Some(item @ (Item::Int | Item::Dbl | Item::Lgl | Item::Str | Item::I64))
                        if interp::annotations::declares_vector(function, formal) =>
                    {
                        Type::vector(item)
                    }
                    _ => ty,
                };
                arguments.push(value, ty).map(|()| ty)
            }
            None if holds_boxes(value) => arguments.push_boxes(value),
            None => {
                arguments.release();
                decline(|| format!("argument {}: {}", formal + 1, boundary_refusal(value)));
                return None;
            }
        };
        match converted {
            Ok(ty) => params.push(ty),
            Err(reason) => {
                arguments.release();
                decline(|| format!("argument {} does not convert: {reason}", formal + 1));
                return None;
            }
        }
    }
    let Some(compiled) = compiled_for(function, &params) else {
        arguments.release();
        return None;
    };
    // An argument the body reads before any R call runs is the value R would force there, since
    // nothing between the entry and that read runs R code. Any other argument read from
    // variables is watched while the code calls R.
    let late = if calls_r(compiled) {
        ahead.lazy() & !compiled.read_before_r
    } else {
        0
    };
    if calls_r(compiled) && ahead.lazy() & compiled.passed_late != 0 {
        arguments.release();
        decline(|| {
            String::from(
                "it passes a parameter to an R function before reading it, and R evaluates an \
                 argument read from variables only where that function first reads it: read the \
                 parameter before the R call, or compute the argument into a variable before the \
                 call",
            )
        });
        return None;
    }
    if late & ahead.fixed != 0 {
        arguments.release();
        decline(|| {
            String::from(
                "it calls R before it reads a parameter it was called without, and R computes \
                 that parameter's default from variables only where the body first reads it: \
                 pass the argument, or read the parameter before the R call",
            )
        });
        return None;
    }
    // The masks hold 64 parameters, and a watched one past them is never settled.
    let wide = late != 0 && ahead.watches.iter().any(|watch| watch.param >= 64);
    if late & !compiled.watchable != 0 || wide {
        arguments.release();
        decline(|| {
            String::from(
                "it calls R before it reads an argument computed from variables, and native code \
                 watches such an argument only when the code makes every R call itself, not \
                 through a typed function, and each comes before the parameter's first read on \
                 every path or after it on every path: read the parameter before the R call, or \
                 compute the argument into a variable before the call",
            )
        });
        return None;
    }
    // A set or a dictionary result has no R value to cross back as, so the interpreter makes
    // the call.
    if storage(compiled.result).is_none() {
        arguments.release();
        decline(|| {
            String::from("it returns a set or a dictionary, which has no R value to return as")
        });
        return None;
    }
    // A watched value must be copied, not changed in place, by R code that assigns into its
    // variable, so that the change shows as a new binding.
    let mut held: SmallVec<[Sexp; 4]> = SmallVec::new();
    if late != 0 {
        for watch in &ahead.watches {
            if let Some(value) = held_value(watch.bound) {
                value.increment_refcount();
                held.push(value);
            }
        }
    }
    // Code that reads a promise ahead settles it before each R call that follows the read, as
    // R would have forced it there, which an error raised in R must find done.
    let watching = late != 0 || !ahead.promises.is_empty();
    let outer_watching = WATCHING.replace(watching.then_some(Watching {
        ahead: std::ptr::from_ref(ahead),
        settled: 0,
        late,
        sites: compiled.sites,
    }));
    let heap = interp::heap();
    let mark = heap.handle_mark();
    let mut out = [0u64; 2];
    let trampoline: Trampoline = std::mem::transmute(compiled.code.trampoline);
    let r_calls = R_CALLS.get();
    let segment = replay::open();
    let outer = RUNNING.replace(Some(Running {
        call: typed.call,
        function,
        args: typed.args.as_ptr(),
        count: typed.args.len(),
        sys_parent: typed.sys_parent,
    }));
    let words = arguments.words.as_ptr();
    let mut run = |limit: usize| {
        let outer_limit = STACK_LIMIT.swap(limit, Ordering::Relaxed);
        let status = trampoline(depth_budget(), words, out.as_mut_ptr());
        let exhausted = STACK_LIMIT.swap(outer_limit, Ordering::Relaxed) == EXHAUSTED_LIMIT;
        (status, exhausted)
    };
    // The interpreter's bounds answer for a call entered on its segment with room to spare,
    // which skips `stacker`'s thread-local reads.
    let marker = 0u8;
    let here = std::ptr::addr_of!(marker) as usize;
    let floor = interp::eval::stack_floor();
    let (status, exhausted) = if floor != 0 && here.saturating_sub(floor) >= NATIVE_STACK {
        run(floor + NATIVE_RESERVE)
    } else {
        stacker::maybe_grow(NATIVE_STACK, NATIVE_SEGMENT, || run(native_stack_limit()))
    };
    RUNNING.set(outer);
    WATCHING.set(outer_watching);
    for value in held {
        value.decrement_refcount();
    }
    let tripped = WATCH_TRIPPED.take();
    if status != 0 {
        arguments.release();
        let unwind = PARKED.take();
        let guess = FAILED_GUESS.take();
        if exhausted {
            EXHAUSTED.set(Some(Rerun {
                depth: interp::interp().frames.len(),
                call: typed.call,
                function,
            }));
        }
        let reason = || stop_reason(guess, exhausted, tripped);
        if unwind.is_some() || R_CALLS.get() == r_calls {
            replay::close(segment);
            heap.release_handles(mark);
            if let Some(unwind) = unwind {
                return Some(Err(unwind));
            }
            STOPS.set(STOPS.get() + 1);
            decline(|| format!("{}, and the interpreter ran the call again", reason()));
            return None;
        }
        // The code called R before it stopped, so the interpreter runs the call again with
        // what those calls returned, which their records keep alive.
        STOPS.set(STOPS.get() + 1);
        let replayed = replay::replay(segment, typed.call, function, typed.args, typed.sys_parent);
        let Some(result) = replayed else {
            heap.release_handles(mark);
            return Some(Err(interp::unwind::error(format!(
                "{}, after it had called R more than {} times or kept more than {} MiB of what \
                 those calls returned, so the interpreter cannot run the call again without \
                 calling R a second time: make fewer R calls in one typed call",
                reason(),
                replay::MAX_RECORDED,
                replay::MAX_RECORDED_MIB,
            ))));
        };
        if interp::annotations::typed_logging() {
            // Deparsing the call for the log may collect, and nothing else holds the result.
            if let Ok(value) = &result {
                heap.root(*value);
            }
            interp::annotations::log_interpreted_because(
                typed.call,
                &format!(
                    "{}, and the interpreter ran the call again with the values its R calls had \
                     returned",
                    reason()
                ),
            );
        }
        heap.release_handles(mark);
        return Some(result);
    }
    replay::close(segment);
    let value = to_r(compiled.result, out);
    drop_words(compiled.result, out);
    arguments.release();
    // Each promise read ahead for a parameter the code may have read settles with the value
    // forcing it gives, as if the interpreter had forced it: a promise shared through `...` must
    // keep the value this call saw. Settling a checked promise runs its annotation checks, whose
    // error is the call's. R never forces the promise of a parameter no path reads.
    let value = value.and_then(|value| {
        heap.root(value);
        for &(promise, read, param) in &ahead.promises {
            // The masks hold 64 parameters, and one past them settles as R may have forced it.
            if param >= 64 || (1 << param) & compiled.returned != 0 {
                interp::eval::settle_unforced(promise, read)?;
            }
        }
        Ok(value)
    });
    heap.release_handles(mark);
    interp::interp().visible = !compiled.invisible;
    Some(value)
}

/// The value R code changes when it assigns into an element of a variable bound to `bound`: the
/// value itself, or a forced promise's value.
unsafe fn held_value(bound: Sexp) -> Option<Sexp> {
    if bound.kind() != SexpType::Promise {
        return Some(bound);
    }
    let value = bound.car();
    (value != rvalues::unbound()).then_some(value)
}

/// The compiled code for calling `function` with arguments of types `params`, compiling it on
/// first use. `None` when the closure does not compile for them.
pub unsafe fn compiled_for(function: Sexp, params: &[Type]) -> Option<&'static Compiled> {
    let compiled = compiled_or_refusal(function, params).ok()?;
    // Code that calls a function still being compiled runs only once that function has code,
    // as when lowering forces a promise whose code calls it.
    let cells_filled = compiled
        .cells
        .iter()
        .all(|(callee, assumed)| cell(callee, *assumed).get() != 0);
    if !cells_filled {
        decline(|| String::from("it calls a function whose code is still being compiled"));
        return None;
    }
    Some(compiled)
}

/// [`compiled_for`], with the refusal when there is no code.
unsafe fn compiled_or_refusal(
    function: Sexp,
    params: &[Type],
) -> Result<&'static Compiled, Refusal> {
    let key: Key = (function, SmallVec::from_slice(params));
    // A refusal that waited on something the interpreter has done since is forgotten.
    let awaited = AWAITING.with_borrow(|awaiting| awaiting.get(&key).copied());
    if awaited.is_some_and(|awaited| lower::arrived(awaited)) {
        AWAITING.with_borrow_mut(|awaiting| awaiting.remove(&key));
        COMPILED.with_borrow_mut(|compiled| compiled.remove(&key));
    }
    match COMPILED
        .with_borrow(|compiled| compiled.get(&key).map(|entry| entry.as_ref().ok().copied()))
    {
        // A function the code calls was rebound since it compiled, or a `match` over any tag
        // misses shapes confirmed since: compile it again.
        Some(Some(compiled)) if !current(compiled) || !cells_hold(&key, compiled) => {}
        Some(Some(compiled)) => return Ok(compiled),
        Some(None) => {
            let refusal = COMPILED.with_borrow(|compiled| match compiled.get(&key) {
                Some(Err(refusal)) => refusal.clone(),
                _ => Refusal::new("no code"),
            });
            decline(|| format!("no code for {params:?}: {}", refusal.0));
            return Err(refusal);
        }
        None => {}
    }
    // An installed package's typed code file may hold the code already.
    if let Some(compiled) = file::attach(function, params) {
        fill_cell(key, compiled);
        return Ok(compiled);
    }
    if !ahead::compiling_ahead()
        && COMPILED.with_borrow(|compiled| compiled.values().filter(|entry| entry.is_ok()).count())
            >= MAX_COMPILED + ahead::compiled_ahead() + file::attached()
    {
        decline(|| format!("the session compiled its limit of {MAX_COMPILED} typed functions"));
        return Err(Refusal::new(
            "the session compiled its limit of typed functions",
        ));
    }
    if COMPILING.with_borrow_mut(|compiling| compiling.insert(key.clone(), None).is_some()) {
        decline(|| String::from("it is called while its own code is being compiled"));
        return Err(Refusal::new(
            "it is called while its own code is being compiled",
        ));
    }
    // The clock is read only for the log. A function's time leaves out the callees compiled
    // while it was lowered, which log their own, and a refused compilation logs its time too, so
    // the logged times add up to the time the session spent compiling.
    let started = interp::annotations::typed_logging().then(Instant::now);
    let outer = NESTED_COMPILE.replace(Duration::ZERO);
    let cell_calls = CELL_CALLS.get();
    let result = compile(function, params);
    let total = started.map_or(Duration::ZERO, |started| started.elapsed());
    let own = total.saturating_sub(NESTED_COMPILE.replace(outer + total));
    COMPILING.with_borrow_mut(|compiling| compiling.remove(&key));
    let compiled = match result {
        Ok(compiled) => {
            interp::heap().add_global_root(function);
            if started.is_some() {
                eprintln!(
                    "typed: compiled for {params:?} -> {:?} in {} us",
                    compiled.result,
                    own.as_micros()
                );
            }
            let compiled: &'static Compiled = Box::leak(Box::new(compiled));
            fill_cell(key.clone(), compiled);
            Ok(compiled)
        }
        Err(refusal) => {
            if started.is_some() {
                eprintln!("typed: refused {params:?} after {} us", own.as_micros());
            }
            decline(|| format!("no code for {params:?}: {}", refusal.0));
            Err(refusal)
        }
    };
    if let Err(refusal) = &compiled
        && lower::awaits_interpreter(refusal)
        && let Some(awaited) = lower::awaited()
    {
        // The table reads the promise later, which the namespace may have dropped by then.
        if let lower::Awaited::Promise(promise) = awaited {
            interp::heap().add_global_root(promise);
        }
        AWAITING.with_borrow_mut(|awaiting| awaiting.insert(key.clone(), awaited));
    }
    // A refusal under the guess of a function still being compiled is not this function's for
    // good, since that function may revise the guess, so it is not remembered and a later call
    // compiles it again. The outermost compilation's refusal rests on no one else's guess.
    let provisional =
        CELL_CALLS.get() != cell_calls && !COMPILING.with_borrow(|compiling| compiling.is_empty());
    if !provisional {
        remember(key, compiled.clone());
    }
    compiled
}

fn remember(key: Key, compiled: Result<&'static Compiled, Refusal>) {
    COMPILED.with_borrow_mut(|table| {
        if compiled.is_err() && table.len() >= MAX_REFUSALS {
            table.retain(|_, entry| entry.is_ok());
            AWAITING.with_borrow_mut(|awaiting| awaiting.clear());
        }
        table.insert(key, compiled);
    });
}

/// Points the cell that calls `key` assuming `compiled`'s result, if code calls through one, at
/// `compiled`.
fn fill_cell(key: Key, compiled: &Compiled) {
    if let Some(cell) = CELLS.with_borrow(|cells| cells.get(&(key, compiled.result)).copied()) {
        cell.set(compiled.code.address);
    }
}

/// How many calls on this thread stopped in native code and ran again in the interpreter.
pub fn native_stops() -> u64 {
    STOPS.get()
}

/// Why compiled code stopped partway: the conversion that gave a type the code was not compiled
/// for when that is why.
unsafe fn stop_reason(guess: Option<(Sexp, SexpType)>, exhausted: bool, tripped: bool) -> String {
    if exhausted {
        return "its native code ran out of stack or of R's nesting depth".to_owned();
    }
    if tripped {
        return "an R call it made changed a variable an argument is computed from before the \
                function read that argument, so R forces the argument to the changed value"
            .to_owned();
    }
    match guess {
        Some((conversion, kind)) => format!(
            "`{}` gave {}, which its native code was not compiled for",
            interp::deparse::deparse_first_line(conversion),
            lower::observed_name(&rvalues::symbol_name(conversion.car()), kind)
        ),
        None => "its native code stopped at an operation only the interpreter finishes, such as \
                 one that raises an error or a warning"
            .to_owned(),
    }
}

/// Whether `function` has compiled code for some argument types.
pub fn has_compiled(function: Sexp) -> bool {
    COMPILED.with_borrow(|compiled| {
        compiled
            .iter()
            .any(|((closure, _), entry)| *closure == function && entry.is_ok())
    })
}

/// Whether `compiled` is right for the session as it is: its lookups hold, and no shape or
/// conversion type it was compiled against has changed.
unsafe fn current(compiled: &Compiled) -> bool {
    still_resolves(compiled)
        && compiled.epoch.is_none_or(|epoch| epoch == shapes::epoch())
        && compiled
            .guessed
            .is_none_or(|epoch| epoch == interp::numbers::conversion_epoch())
}

/// Whether every function `compiled` (the code for `key`) calls through a cell has current
/// code of the result type the call assumed, or is being lowered with that guess. Its lookups
/// are checked here because they are not among `compiled`'s own: the callee had none yet.
unsafe fn cells_hold(key: &Key, compiled: &Compiled) -> bool {
    compiled.cells.iter().all(|(callee, assumed)| {
        if callee == key {
            return true;
        }
        if let Some(guess) = COMPILING.with_borrow(|compiling| compiling.get(callee).copied()) {
            return guess == Some(*assumed);
        }
        let found = COMPILED.with_borrow(|table| match table.get(callee) {
            Some(Ok(found)) => Some(*found),
            _ => None,
        });
        found.is_some_and(|found| found.result == *assumed && current(found))
    })
}

/// Whether `compiled`, or a function it calls, calls an R function, counting the functions it
/// calls through a cell, which `compiled.calls_r` leaves out.
fn calls_r(compiled: &Compiled) -> bool {
    compiled.calls_r
        || compiled.cells.iter().any(|(callee, _)| {
            COMPILED
                .with_borrow(|table| matches!(table.get(callee), Some(Ok(found)) if found.calls_r))
        })
}

/// The cell that holds the address of the latest code for `key` with result `result`, made on
/// first use.
fn cell(key: &Key, result: Type) -> &'static Cell<usize> {
    CELLS.with_borrow_mut(|cells| {
        *cells.entry((key.clone(), result)).or_insert_with(|| {
            let address = COMPILED.with_borrow(|table| match table.get(key) {
                Some(Ok(found)) if found.result == result => found.code.address,
                _ => 0,
            });
            Box::leak(Box::new(Cell::new(address)))
        })
    })
}

/// Whether every function lookup `compiled` assumes still finds the same function. A lookup
/// that found a definition its script had not run yet assumed no binding, only the function
/// ([`ahead::defined_later`]).
unsafe fn still_resolves(compiled: &Compiled) -> bool {
    let functions = if ahead::compiling_ahead() {
        lower::find_function
    } else {
        lower::resolve_function
    };
    compiled.speculations.iter().all(|assumed| {
        let resolve = if interp::values::is_function(assumed.function) {
            functions
        } else {
            lower::resolve_value
        };
        resolve(assumed.symbol, assumed.env).is_some_and(|now| {
            (assumed.cell == rvalues::nil() || now.cell == assumed.cell)
                && now.function == assumed.function
        })
    })
}

/// Compiles `function` for `params`. Compiled code confirms the tag shapes it constructs, and a
/// `match` over any tag lowered before them, in this code or in a function it calls, would not
/// test for them. So the shapes a lowering constructs are confirmed before any machine code is
/// generated, and the function is lowered again until its matches saw every confirmed shape.
/// Only lowering repeats, which is a small part of a compilation.
unsafe fn compile(function: Sexp, params: &[Type]) -> Result<Compiled, Refusal> {
    for _ in 0..MAX_SHAPE_ROUNDS {
        let (lowered, linker) = lower_settled(function, params)?;
        // A left-out formal's default meets its annotation where R would check it.
        let mut defaults = lowered.defaults.iter().copied();
        let mut arguments: SmallVec<[Option<(SexpType, bool)>; 4]> = SmallVec::new();
        for &ty in params {
            let ty = match ty {
                Type::Absent => defaults.next().unwrap_or(Type::Absent),
                ty => ty,
            };
            arguments.push(storage(ty));
        }
        arguments.extend(defaults.map(storage));
        let result = storage(lowered.function.result);
        if !interp::annotations::admits_storage(function, &arguments, result) {
            return Err(Refusal::new("the types fail the closure's annotations"));
        }
        for &shape in &lowered.constructed {
            shapes::confirm(shape);
        }
        let epoch = earliest(lowered.any_tag.as_ref().map(|any| any.epoch), linker.epoch);
        if epoch.is_none_or(|epoch| epoch == shapes::epoch()) {
            return generate(params, lowered, linker);
        }
    }
    Err(Refusal::new("the shapes a `match` tests for keep growing"))
}

/// How many times [`compile`] lowers before the shapes settle.
const MAX_SHAPE_ROUNDS: usize = 4;

/// Lowers `function` with a result type that its recursive calls agree with. While it lowers,
/// the function is in `COMPILING` with its guess, which a call from a function compiled
/// meanwhile assumes ([`Linker::callee`]); afterwards its entry is as it was, so a lowering
/// for a code file leaves no guess behind.
unsafe fn lower_settled(function: Sexp, params: &[Type]) -> Result<(Lowered, Linker), Refusal> {
    let key: Key = (function, SmallVec::from_slice(params));
    let before = COMPILING.with_borrow(|compiling| compiling.get(&key).copied());
    let lowered = lower_guessing(function, params, &key);
    COMPILING.with_borrow_mut(|compiling| match before {
        Some(before) => compiling.insert(key, before),
        None => compiling.remove(&key),
    });
    lowered
}

unsafe fn lower_guessing(
    function: Sexp,
    params: &[Type],
    key: &Key,
) -> Result<(Lowered, Linker), Refusal> {
    let declared = declared_result(function);
    let guesses: SmallVec<[Type; 5]> = declared.into_iter().chain(GUESSES).collect();
    let mut refusal = Refusal::new("the result type depends on the recursion");
    for &first in &guesses {
        let mut guess = first;
        for _ in 0..=MAX_REVISIONS {
            COMPILING.with_borrow_mut(|compiling| compiling.insert(key.clone(), Some(guess)));
            let mut linker = Linker::default();
            let mut returned = None;
            let lowered = match lower::lower(
                function,
                params,
                guess,
                declared,
                &mut linker,
                &mut returned,
            ) {
                Ok(lowered) => lowered,
                // A recursive call's guessed result the body cannot use, as a number where
                // the body reads a record's field, is revised to what a `return` before the
                // refusal gave.
                Err(why) => match returned {
                    Some(ty) if ty != guess => {
                        refusal = why;
                        guess = ty;
                        continue;
                    }
                    _ => {
                        refusal = why;
                        break;
                    }
                },
            };
            let result = lowered.function.result;
            // A function compiled meanwhile that calls this one back through a cell assumed the
            // guess too.
            let called_back = linker.cells.iter().any(|(callee, _)| callee == key);
            if result == guess || !(calls_itself(&lowered.function) || called_back) {
                return Ok((lowered, linker));
            }
            guess = result;
        }
    }
    Err(refusal)
}

/// Machine code for a lowered function whose shapes are confirmed.
unsafe fn generate(params: &[Type], lowered: Lowered, linker: Linker) -> Result<Compiled, Refusal> {
    let callee_calls_r = |index: usize| {
        match (linker.callees.get(index), linker.called.get(index)) {
        (Some(Callee { entry: Entry::Address(_), .. }), Some(key)) => COMPILED.with_borrow(|table| {
            !matches!(table.get(key), Some(Ok(found)) if !found.calls_r && found.cells.is_empty())
        }),
        _ => true,
    }
    };
    let mut lowered = lowered;
    let reads = reads_before_calling_r(&lowered, callee_calls_r);
    for &(site, read) in &reads.before.sites {
        if let Some(site) = lowered.r_calls.get_mut(site) {
            site.read_before = read;
        }
    }
    let through_callee = lowered.function.blocks.iter().any(|block| {
        block.insts.iter().any(|inst| match inst.op {
            ir::Op::CallTyped(lower::SELF, _) => reads.calls_r,
            ir::Op::CallTyped(index, _) => callee_calls_r(index),
            _ => false,
        })
    });
    let watchable = if through_callee {
        0
    } else {
        !reads.before.ambiguous
    };
    let calls_itself = lowered.function.blocks.iter().any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(inst.op, ir::Op::CallTyped(lower::SELF, _)))
    });
    let own_sites = if calls_itself {
        (0, 0)
    } else {
        let range = lowered.r_calls.as_ptr_range();
        (range.start as usize, range.end as usize)
    };
    let mut ir = lowered.function;
    optimize(&mut ir);
    let sites: Vec<usize> = lowered
        .r_calls
        .iter()
        .map(|site| site as *const RCall as usize)
        .collect();
    let code = backend::compile(&ir, &linker.callees, &sites)?;
    let calls_r = linker.calls_r || lowered.r_calls.iter().any(|site| acts(site));
    let guesses = lowered.r_calls.iter().any(
        |site| matches!(site.result, RResult::Converted(conversion, _) if conversion.guessed()),
    );
    let guessed = earliest(
        guesses.then(interp::numbers::conversion_epoch),
        linker.guessed,
    );
    // The code calls each function and conversion it was compiled against, and keeps every site
    // for as long as it lives, which is the whole session.
    let heap = interp::heap();
    for site in &lowered.r_calls {
        heap.add_global_root(site.function);
        heap.add_global_root(site.call);
        for conversion in site
            .arguments
            .iter()
            .filter_map(|argument| match argument.passed {
                Passed::Value(_, conversion) => conversion,
                Passed::Code => None,
            })
        {
            heap.add_global_root(conversion.function);
        }
        if let RResult::Converted(conversion, _) = site.result {
            heap.add_global_root(conversion.function);
        }
    }
    std::mem::forget(lowered.r_calls);
    let mut speculations = lowered.speculations;
    speculations.extend(linker.speculations);
    let epoch = earliest(lowered.any_tag.as_ref().map(|any| any.epoch), linker.epoch);
    Ok(Compiled {
        code,
        params: SmallVec::from_slice(params),
        result: ir.result,
        speculations,
        epoch,
        calls_r,
        read_before_r: reads.before.first,
        watchable,
        passed_late: reads.before.passed_late,
        returned: reads.before.returned,
        sites: own_sites,
        guessed,
        cells: linker.cells,
        invisible: lowered.invisible,
    })
}

/// What `lowered` reads before an R call that may change a variable or a typed call for which
/// `callee_calls_r` holds by the callee's index ([`lower::reads_before`]), and whether it makes
/// any such call. A call of the function itself calls R when the function does.
pub(crate) struct CallingR {
    pub before: lower::ReadsBefore,
    pub calls_r: bool,
}

pub(crate) unsafe fn reads_before_calling_r(
    lowered: &Lowered,
    callee_calls_r: impl Fn(usize) -> bool,
) -> CallingR {
    let runs_r = |op: &ir::Op, itself: bool| match *op {
        ir::Op::CallR(site, _) => lowered.r_calls.get(site).is_none_or(|site| acts(site)),
        ir::Op::CallTyped(lower::SELF, _) => itself,
        ir::Op::CallTyped(index, _) => callee_calls_r(index),
        _ => false,
    };
    let calls_r = lowered
        .function
        .blocks
        .iter()
        .any(|block| block.insts.iter().any(|inst| runs_r(&inst.op, false)));
    CallingR {
        before: lower::reads_before(lowered, |op| runs_r(op, calls_r)),
        calls_r,
    }
}

/// The passes between lowering and machine code.
fn optimize(ir: &mut ir::Function) {
    unbox::replace_split_lists(ir);
    loops::fuse_loops(ir);
    unbox::unbox(ir);
    unbox::remove_unread(ir);
    fusion::fuse(ir);
    ownership::insert(ir);
    if std::env::var_os("RY_TYPED_DUMP").is_some() {
        eprintln!("{ir}");
    }
}

fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Whether the function calls itself, so its result type is an assumption until confirmed.
fn calls_itself(function: &ir::Function) -> bool {
    function.blocks.iter().any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(inst.op, ir::Op::CallTyped(lower::SELF, _)))
    })
}

/// The compiled typed closures a function calls, in the order its calls index them.
#[derive(Default)]
struct Linker {
    callees: Vec<Callee>,
    /// The closure and argument types of each callee, which a typed code file names it by.
    called: Vec<Key>,
    speculations: Vec<Speculation>,
    epoch: Option<u64>,
    calls_r: bool,
    guessed: Option<u64>,
    cells: SmallVec<[(Key, Type); 1]>,
}

impl Callees for Linker {
    unsafe fn callee(&mut self, function: Sexp, params: &[Type]) -> Result<(usize, Type), Refusal> {
        let key: Key = (function, SmallVec::from_slice(params));
        // A function being compiled, which calls this one, has no code yet: the call goes through
        // its cell and assumes the result its own recursive calls assume.
        if let Some(Some(guess)) = COMPILING.with_borrow(|compiling| compiling.get(&key).copied()) {
            let cell = cell(&key, guess);
            CELL_CALLS.set(CELL_CALLS.get() + 1);
            add_cell(&mut self.cells, &key, guess);
            self.called.push(key);
            self.callees.push(Callee {
                entry: Entry::Cell(cell as *const Cell<usize> as usize),
                params: SmallVec::from_slice(params),
                result: guess,
            });
            return Ok((self.callees.len() - 1, guess));
        }
        let compiled = compiled_or_refusal(function, params).map_err(|refusal| {
            // The caller waits on the interpreter as the function it calls does.
            if lower::awaits_interpreter(&refusal) {
                refusal
            } else {
                Refusal(Cow::Owned(format!(
                    "a typed function it calls does not compile: {}",
                    refusal.0
                )))
            }
        })?;
        self.calls_r |= compiled.calls_r;
        // Code that calls another function directly is only right while that function's own
        // lookups hold.
        self.speculations.extend_from_slice(&compiled.speculations);
        self.epoch = earliest(self.epoch, compiled.epoch);
        self.guessed = earliest(self.guessed, compiled.guessed);
        for (callee, assumed) in &compiled.cells {
            // Code linked here rests on that guess as much as a call through the cell would.
            if COMPILING.with_borrow(|compiling| compiling.contains_key(callee)) {
                CELL_CALLS.set(CELL_CALLS.get() + 1);
            }
            add_cell(&mut self.cells, callee, *assumed);
        }
        self.called.push(key);
        self.callees.push(Callee {
            entry: Entry::Address(compiled.code.address),
            params: SmallVec::from_slice(params),
            result: compiled.result,
        });
        Ok((self.callees.len() - 1, compiled.result))
    }
}

fn add_cell(cells: &mut SmallVec<[(Key, Type); 1]>, key: &Key, assumed: Type) {
    if !cells.contains(&(key.clone(), assumed)) {
        cells.push((key.clone(), assumed));
    }
}

/// The type a typed closure declares it returns, where one type of the typed core is that.
unsafe fn declared_result(function: Sexp) -> Option<Type> {
    if interp::annotations::returns_i64(function) {
        return Some(Type::I64);
    }
    if let Some(ty) = interp::annotations::declared_return_type(function).and_then(declared_type) {
        return Some(ty);
    }
    match interp::annotations::return_annotation(function)?.trim() {
        "F64" => Some(Type::Dbl),
        "Str" => Some(Type::Str),
        "Bool" => Some(Type::Lgl),
        _ => None,
    }
}

fn declared_type(declared: interp::annotations::Declared) -> Option<Type> {
    let item = match declared.kind {
        SexpType::Integer => Item::Int,
        SexpType::Real => Item::Dbl,
        SexpType::Logical => Item::Lgl,
        _ => return None,
    };
    Some(if declared.scalar {
        Type::of_item(item)
    } else {
        Type::vector(item)
    })
}

/// The R storage type of a value of `ty` once it crosses back, and whether it is an `Array`.
pub(crate) fn storage(ty: Type) -> Option<(SexpType, bool)> {
    Some(match ty {
        Type::Int => (SexpType::Integer, false),
        Type::Dbl => (SexpType::Real, false),
        Type::I64 => (SexpType::I64, false),
        Type::Lgl | Type::Bool => (SexpType::Logical, false),
        Type::Str => (SexpType::String, false),
        Type::Null => (SexpType::Nil, false),
        Type::Vector { item, array } => (item_storage(item), array),
        Type::List(_) => (SexpType::List, false),
        Type::Record(_) | Type::Tag(_) => (SexpType::Record, false),
        Type::Table => (SexpType::ExternalPtr, false),
        Type::Set(_) | Type::Dict { .. } | Type::Absent => return None,
    })
}

fn item_storage(item: Item) -> SexpType {
    match item {
        Item::Int => SexpType::Integer,
        Item::Dbl => SexpType::Real,
        Item::Lgl => SexpType::Logical,
        Item::Str => SexpType::String,
        Item::I64 => SexpType::I64,
        // A ry list of records, tags or lists is an R list of them.
        Item::Record(_) | Item::Tag(_) | Item::Array(_) => SexpType::List,
    }
}

fn item_kind(item: Item) -> u32 {
    u32::from(match item {
        Item::Int => values::INT,
        Item::Dbl => values::DBL,
        Item::Lgl => values::LGL,
        Item::Str => values::STR,
        Item::I64 => values::I64,
        Item::Record(_) | Item::Tag(_) | Item::Array(_) => values::OBJECT,
    })
}

/// The typed core's type for an R value, or `None` when it has none: a length-one plain vector
/// is a scalar, a longer or empty one a vector, an `Array` an array, a plain list of length-one
/// strings, numbers or logicals of one storage type a list of them, an `Array` list of plain tags
/// an array of tags, a plain tag any tag, and a value of the table API a table.
/// A record or tuple has none here, because its type is the shape its conversion finds
/// ([`Arguments::push_boxes`]).
unsafe fn type_of(value: Sexp) -> Option<Type> {
    if (interp::interp().extensions.tables.is_table_value)(value) {
        return Some(Type::Table);
    }
    if value.attrib() != rvalues::nil() {
        return None;
    }
    if value.is_altrep() {
        if string_list_of(value).is_some() {
            return Some(Type::Vector {
                item: Item::Str,
                array: value.is_array(),
            });
        }
        return written_string_list(value).and_then(|written| type_of(written));
    }
    if value.kind() == SexpType::Record {
        return (!value.is_object() && interp::records::native_kind(value) == NativeKind::Tag)
            .then_some(Type::Tag(shapes::ANY));
    }
    let array = value.is_array();
    let length = rvalues::length(value);
    let item = match value.kind() {
        SexpType::Integer => Item::Int,
        SexpType::Real => Item::Dbl,
        SexpType::Logical => Item::Lgl,
        SexpType::I64 => Item::I64,
        SexpType::String => {
            if !(0..length).all(|index| usable_string(value.element(index))) {
                return None;
            }
            Item::Str
        }
        SexpType::List if !array => return list_type(value, length),
        // A ry list of tags, such as `[:Bond, :Cash]`, which converts item by item.
        SexpType::List
            if length > 0
                && (0..length).all(|index| {
                    let item = value.element(index);
                    item.kind() == SexpType::Record
                        && !item.is_object()
                        && interp::records::native_kind(item) == NativeKind::Tag
                }) =>
        {
            Item::Tag(shapes::ANY)
        }
        _ => return None,
    };
    if length == 1 && !array {
        return Some(Type::of_item(item));
    }
    Some(Type::Vector { item, array })
}

/// Whether `value` is a plain record or tuple, or a ry list of records, tuples, tags or lists,
/// which [`Arguments::push_boxes`] converts into boxes that also give its type.
unsafe fn holds_boxes(value: Sexp) -> bool {
    if value.is_altrep() || value.attrib() != rvalues::nil() || value.is_object() {
        return false;
    }
    match value.kind() {
        SexpType::Record => true,
        SexpType::List => value.is_array(),
        _ => false,
    }
}

/// Why an argument from R has no type in the typed core, and what to pass instead.
unsafe fn boundary_refusal(value: Sexp) -> String {
    if value.attrib() != rvalues::nil() || value.is_object() {
        return String::from(
            "it is an R value with attributes, such as a data frame or a factor, which typed code \
             cannot hold: pass its columns or plain vectors",
        );
    }
    format!(
        "it is an R value of type `{}` that typed code cannot hold, such as a list of mixed \
         types, an `NA` string or a non-UTF-8 string",
        value.kind().name()
    )
}

unsafe fn list_type(value: Sexp, length: usize) -> Option<Type> {
    let mut item = None;
    for index in 0..length {
        let element = value.element(index);
        let ty = type_of(element)?;
        let found = ty
            .item()
            .filter(|item| ty != Type::Bool && !matches!(item, Item::Record(_)))?;
        if item.is_some_and(|item| item != found) {
            return None;
        }
        item = Some(found);
    }
    // An empty list could hold anything, and the typed core only knows lists of one item type.
    item.map(Type::List)
}

/// Whether a `CHARSXP` is a string the typed core can hold: not `NA`, and UTF-8. An interned
/// string had its bytes checked when it was interned, so only another string is read here.
unsafe fn usable_string(charsxp: Sexp) -> bool {
    charsxp != interp::heap().known().na_string
        && !charsxp.has_gp(gp::LATIN1 | gp::BYTES)
        && (charsxp.is_valid_utf8() || std::str::from_utf8(charsxp.bytes()).is_ok())
}

/// The arguments of one call as machine words, with what converting them allocated.
#[derive(Default)]
struct Arguments {
    words: SmallVec<[u64; 8]>,
    views: SmallVec<[*mut List; 4]>,
    /// Lists and boxes, which both drop through [`values::ry_list_drop`].
    lists: SmallVec<[*mut List; 4]>,
    strings: SmallVec<[Str; 4]>,
}

impl Arguments {
    /// Appends `value`, of type `ty`, or says why it does not convert.
    unsafe fn push(&mut self, value: Sexp, ty: Type) -> Result<(), &'static str> {
        match ty {
            Type::Int | Type::Lgl => self.words.push(rvalues::ints(value)[0] as u32 as u64),
            Type::Dbl => self.words.push(rvalues::reals(value)[0].to_bits()),
            Type::I64 => {
                let number = match interp::numbers::read(value) {
                    Some(number) => Some(number),
                    None => interp::numbers::r_source(value)
                        .ok()
                        .and_then(interp::numbers::convert),
                };
                let Some(number) = number else {
                    return Err("an `I64` parameter takes only a whole number");
                };
                self.words.push(number as u64);
            }
            Type::Str => {
                let string = str_of(value.element(0));
                self.strings.push(string);
                self.words.extend([string.lo, string.hi]);
            }
            Type::Vector {
                item: Item::Str, ..
            } => {
                let list = string_list(value);
                if list.is_null() {
                    return Err(OUT_OF_MEMORY);
                }
                self.lists.push(list);
                self.words.push(list as u64);
            }
            Type::Vector {
                item: Item::Tag(_), ..
            } => {
                let length = rvalues::length(value);
                let (list, _) = shapes::with(|shapes| list_of_objects(shapes, value, length, 0))?;
                self.lists.push(list);
                self.words.push(list as u64);
            }
            Type::Vector { item, .. } => {
                let data = value.data();
                let view =
                    values::ry_list_view(item_kind(item), data, rvalues::length(value) as u64);
                if view.is_null() {
                    return Err(OUT_OF_MEMORY);
                }
                self.views.push(view);
                self.words.push(view as u64);
            }
            Type::List(item) => {
                let length = rvalues::length(value);
                let list = values::ry_list_new(item_kind(item), length as u64);
                if list.is_null() {
                    return Err(OUT_OF_MEMORY);
                }
                // The list owns each item as soon as it is written, so dropping the list drops
                // the items written before a failure, and the rest are null.
                self.lists.push(list);
                for index in 0..length {
                    let element = value.element(index);
                    let slot = (*list).data;
                    match item {
                        Item::Int | Item::Lgl => {
                            *(slot as *mut i32).add(index) = rvalues::ints(element)[0]
                        }
                        Item::Dbl => *(slot as *mut f64).add(index) = rvalues::reals(element)[0],
                        Item::I64 => *(slot as *mut i64).add(index) = element.slice::<i64>()[0],
                        Item::Str => *(slot as *mut Str).add(index) = str_of(element.element(0)),
                        Item::Record(_) | Item::Tag(_) => {
                            let (boxed, _) = to_box(element)?;
                            *(slot as *mut u64).add(index) = boxed;
                        }
                        // `list_type` gives no R list of lists a type.
                        Item::Array(_) => return Err(UNTYPED),
                    }
                }
                self.words.push(list as u64);
            }
            Type::Tag(_) => {
                let (boxed, _) = to_box(value)?;
                self.lists.push(boxed as *mut List);
                self.words.push(boxed);
            }
            // The caller keeps its argument alive for the call.
            Type::Table => self.words.push(value.as_ptr() as u64),
            Type::Bool
            | Type::Null
            | Type::Set(_)
            | Type::Dict { .. }
            | Type::Record(_)
            | Type::Absent => {
                return Err(UNTYPED);
            }
        }
        Ok(())
    }

    /// Appends the plain record or tuple `value`, or the ry list of records, tuples, tags or
    /// lists `value`, converted deeply, with the type the converted value has. The type comes
    /// from the value, as `list_of_array` explains.
    unsafe fn push_boxes(&mut self, value: Sexp) -> Result<Type, &'static str> {
        let (word, ty) = if value.kind() == SexpType::Record {
            let (boxed, item) = to_box(value)?;
            (boxed, Type::of_item(item))
        } else {
            let length = rvalues::length(value);
            let (list, item) = shapes::with(|shapes| list_of_objects(shapes, value, length, 0))?;
            (list as u64, Type::Vector { item, array: true })
        };
        self.lists.push(word as *mut List);
        self.words.push(word);
        Ok(ty)
    }

    unsafe fn release(&mut self) {
        for &view in &self.views {
            values::ry_view_release(view);
        }
        for &list in &self.lists {
            values::ry_list_drop(list);
        }
        for &string in &self.strings {
            values::ry_str_drop(string.lo, string.hi);
        }
        self.views.clear();
        self.lists.clear();
        self.strings.clear();
    }
}

unsafe fn str_of(charsxp: Sexp) -> Str {
    let bytes = charsxp.bytes();
    values::ry_str_from_bytes(bytes.as_ptr(), bytes.len() as u64)
}

/// A string list holding the strings of the character vector `value`: the list it shares, with
/// one more reference, or a new one.
unsafe fn string_list(value: Sexp) -> *mut List {
    if let Some(list) = string_list_of(value) {
        values::ry_list_dup(list);
        return list;
    }
    let value = written_string_list(value).unwrap_or(value);
    let length = rvalues::length(value);
    let list = values::ry_list_new(u32::from(values::STR), length as u64);
    if list.is_null() {
        return list;
    }
    let data = (*list).data as *mut Str;
    for index in 0..length {
        *data.add(index) = str_of(value.element(index));
    }
    list
}

/// Why an R value does not convert at the boundary, as a decline gives it.
const UNTYPED: &str = "typed code cannot take a value of its type from R";
const OUT_OF_MEMORY: &str = "there is no memory left to convert it";
const CLASSED: &str =
    "it holds a value with a class or attributes, which typed code cannot hold: pass plain values";
const COLLECTION: &str =
    "it holds a `Dict` or a `Set` from R, which typed code takes only from typed code";
const TOO_LARGE: &str = "it holds a record with more fields than typed code holds in one value";
const FIELD: &str = "it holds a field typed code cannot hold, such as an R vector longer than \
                     one, an `NA` string or a non-UTF-8 string";
const EMPTY_LIST: &str = "it holds an empty list whose item type is unknown, such as a bare \
                          `[]` in untyped code: give the list a type, or build the value inside \
                          a typed function";
const MIXED_LIST: &str = "it holds a list whose items have different types";

/// A new box holding the plain record, tuple or tag `value` and, deeply, every such value and
/// ry list in its fields, with its item type, or why it does not convert. Every shape the
/// conversion meets is confirmed, since a value of it now exists.
unsafe fn to_box(value: Sexp) -> Result<(u64, Item), &'static str> {
    shapes::with(|shapes| to_box_in(shapes, value, 0))
}

/// [`to_box`] with the shape table borrowed, at `depth` levels below the value converted.
unsafe fn to_box_in(
    shapes: &mut Shapes,
    value: Sexp,
    depth: u32,
) -> Result<(u64, Item), &'static str> {
    if depth.is_multiple_of(STACK_CHECK_LEVELS) {
        stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH, || {
            to_box_inner(shapes, value, depth)
        })
    } else {
        to_box_inner(shapes, value, depth)
    }
}

/// Stack space a conversion leaves before it grows the stack, and how much it adds, since a
/// deep tree recurses once per level.
const STACK_RED_ZONE: usize = 64 * 1024;
const STACK_GROWTH: usize = 1024 * 1024;

/// How many levels of a tree a conversion descends between two checks of the stack. Asking
/// how much stack is left costs a thread-local lookup, a sizable part of converting one small
/// record, and this many levels take far less than the red zone.
const STACK_CHECK_LEVELS: u32 = 8;

unsafe fn to_box_inner(
    shapes: &mut Shapes,
    value: Sexp,
    depth: u32,
) -> Result<(u64, Item), &'static str> {
    if value.kind() != SexpType::Record {
        return Err(FIELD);
    }
    if value.is_object() || value.attrib() != rvalues::nil() {
        return Err(CLASSED);
    }
    let kind = interp::records::native_kind(value);
    // A record typed code made and R never filled shares its box, so passing it back is free.
    if value.is_lazy_record() {
        let boxed = value.lazy_record_value();
        values::ry_list_dup(boxed as *mut List);
        let shape = (*(boxed as *const Boxed)).shape;
        let item = if kind == NativeKind::Tag {
            Item::Tag(shapes::ANY)
        } else {
            Item::Record(shape)
        };
        return Ok((boxed, item));
    }
    let layout = value.record_layout();
    if matches!(kind, NativeKind::Dict | NativeKind::Set) {
        return Err(COLLECTION);
    }
    let tagged = kind == NativeKind::Tag;
    let count = value.record_slot_count();
    // Every field takes at least one word, so a record with more fields never fits in a box.
    if count > MAX_WORDS {
        return Err(TOO_LARGE);
    }
    // The fields convert into buffers on the stack, laid out as the box lays them out, and the
    // box is allocated once their shape is known.
    let mut items = [const { MaybeUninit::<Item>::uninit() }; MAX_WORDS];
    let mut words = [const { MaybeUninit::<u64>::uninit() }; MAX_WORDS];
    let mut used = 0;
    for index in 0..count {
        let (item, word) = match value.record_slot(index) {
            Slot::Real(number) => (Item::Dbl, [number.to_bits(), 0]),
            Slot::Integer(number) => (Item::Int, [number as u32 as u64, 0]),
            Slot::Logical(number) => (Item::Lgl, [number as u32 as u64, 0]),
            Slot::Handle(field) => match field_word(shapes, field, depth + 1) {
                Ok(found) => found,
                Err(reason) => {
                    release_fields(&items[..index], &words[..used]);
                    return Err(reason);
                }
            },
        };
        let size = shapes::item_words(item) as usize;
        if used + size > MAX_WORDS {
            drop_field(item, word);
            release_fields(&items[..index], &words[..used]);
            return Err(TOO_LARGE);
        }
        items[index].write(item);
        words[used].write(word[0]);
        if size == 2 {
            words[used + 1].write(word[1]);
        }
        used += size;
    }
    let fields = std::slice::from_raw_parts(items.as_ptr() as *const Item, count);
    let Some(shape) = shapes.converted(layout, fields, tagged) else {
        release_fields(&items[..count], &words[..used]);
        return Err(TOO_LARGE);
    };
    let item = if tagged {
        Item::Tag(shapes::ANY)
    } else {
        Item::Record(shape.id)
    };
    if shape.words == 0 {
        return Ok((shape.constant, item));
    }
    let boxed = values::ry_box_new(shape.id, shape.objects, shape.strings, shape.words);
    std::ptr::copy_nonoverlapping(
        words.as_ptr() as *const u64,
        boxed.add(values::BOX_FIELDS) as *mut u64,
        used,
    );
    Ok((boxed as u64, item))
}

/// The most words, and so the most fields, a box holds.
const MAX_WORDS: usize = values::MAX_BOX_WORDS as usize;

/// Drops the converted fields `items`, whose words are `words` in box order, when a conversion
/// fails partway.
unsafe fn release_fields(items: &[MaybeUninit<Item>], words: &[MaybeUninit<u64>]) {
    let mut at = 0;
    for item in items {
        let item = item.assume_init();
        let second = if item == Item::Str {
            words[at + 1].assume_init()
        } else {
            0
        };
        drop_field(item, [words[at].assume_init(), second]);
        at += shapes::item_words(item) as usize;
    }
}

/// Drops one converted field.
unsafe fn drop_field(item: Item, word: [u64; 2]) {
    match item {
        Item::Str => values::ry_str_drop(word[0], word[1]),
        Item::Record(_) | Item::Tag(_) | Item::Array(_) => {
            values::ry_list_drop(word[0] as *mut List)
        }
        Item::Int | Item::Dbl | Item::Lgl | Item::I64 => {}
    }
}

/// A record field that is a handle as a box field: a plain scalar, a usable string, an `I64`,
/// a ry list, or a record, tuple or tag, converted deeply.
unsafe fn field_word(
    shapes: &mut Shapes,
    field: Sexp,
    depth: u32,
) -> Result<(Item, [u64; 2]), &'static str> {
    if field.kind() == SexpType::Record {
        let (boxed, item) = to_box_in(shapes, field, depth)?;
        return Ok((item, [boxed, 0]));
    }
    if field.attrib() != rvalues::nil() {
        return Err(FIELD);
    }
    let field = written_string_list(field).unwrap_or(field);
    if field.is_array() && (!field.is_altrep() || string_list_of(field).is_some()) {
        let (list, item) = list_of_array(shapes, field, depth)?;
        return Ok((Item::Array(shapes.array_of(item)), [list as u64, 0]));
    }
    match type_of(field).ok_or(FIELD)? {
        Type::Int => Ok((Item::Int, [rvalues::ints(field)[0] as u32 as u64, 0])),
        Type::Lgl => Ok((Item::Lgl, [rvalues::ints(field)[0] as u32 as u64, 0])),
        Type::Dbl => Ok((Item::Dbl, [rvalues::reals(field)[0].to_bits(), 0])),
        Type::I64 => Ok((Item::I64, [field.slice::<i64>()[0] as u64, 0])),
        Type::Str => {
            let string = str_of(field.element(0));
            Ok((Item::Str, [string.lo, string.hi]))
        }
        _ => Err(FIELD),
    }
}

/// A new native list holding the items of the ry list `array`, converted deeply, with its item
/// type. An `Array` of numbers, logicals or strings stores its item type, and one of records,
/// tags or lists is an R list whose items give it, so an empty one of those has none.
///
/// The type comes from the value, not the declaration, which is safe: compiled code that meets
/// a shape whose field type it cannot use declines, and the interpreter makes the call.
unsafe fn list_of_array(
    shapes: &mut Shapes,
    array: Sexp,
    depth: u32,
) -> Result<(*mut List, Item), &'static str> {
    let length = rvalues::length(array);
    let item = match array.kind() {
        SexpType::Integer => Item::Int,
        SexpType::Real => Item::Dbl,
        SexpType::Logical => Item::Lgl,
        SexpType::I64 => Item::I64,
        SexpType::String => {
            if string_list_of(array).is_none()
                && !(0..length).all(|index| usable_string(array.element(index)))
            {
                return Err(FIELD);
            }
            Item::Str
        }
        SexpType::List => return list_of_objects(shapes, array, length, depth),
        _ => return Err(FIELD),
    };
    // A bare `[]` is a logical `Array` whatever list it stands for, while one the checker typed
    // has its item's storage. Guessing `Bool` would confirm a shape that no code expects.
    if length == 0 && item == Item::Lgl {
        return Err(EMPTY_LIST);
    }
    if item == Item::Str {
        let list = string_list(array);
        return if list.is_null() {
            Err(OUT_OF_MEMORY)
        } else {
            Ok((list, item))
        };
    }
    let list = values::ry_list_new(item_kind(item), length as u64);
    if list.is_null() {
        return Err(OUT_OF_MEMORY);
    }
    let bytes = length * values::item_size((*list).kind);
    std::ptr::copy_nonoverlapping(array.data(), (*list).data, bytes);
    Ok((list, item))
}

/// [`list_of_array`] for an `Array` of records, tags or lists, whose items must share one type.
unsafe fn list_of_objects(
    shapes: &mut Shapes,
    array: Sexp,
    length: usize,
    depth: u32,
) -> Result<(*mut List, Item), &'static str> {
    if length == 0 {
        return Err(EMPTY_LIST);
    }
    let list = values::ry_list_new(item_kind(Item::Tag(shapes::ANY)), length as u64);
    if list.is_null() {
        return Err(OUT_OF_MEMORY);
    }
    // The list owns each item as soon as it is written, so dropping it drops the items written
    // before a failure, and the rest are null.
    let mut common = None;
    for index in 0..length {
        let (found, [word, _]) = match field_word(shapes, array.element(index), depth + 1) {
            Ok(converted @ (Item::Record(_) | Item::Tag(_) | Item::Array(_), _)) => converted,
            Ok((item, word)) => {
                // A scalar never sits in a list of objects, but a stray one must not leak.
                if item == Item::Str {
                    values::ry_str_drop(word[0], word[1]);
                }
                values::ry_list_drop(list);
                return Err(MIXED_LIST);
            }
            Err(reason) => {
                values::ry_list_drop(list);
                return Err(reason);
            }
        };
        *((*list).data as *mut u64).add(index) = word;
        if common.is_some_and(|item| item != found) {
            values::ry_list_drop(list);
            return Err(MIXED_LIST);
        }
        common = Some(found);
    }
    common.map(|item| (list, item)).ok_or(EMPTY_LIST)
}

/// The R value of the box `boxed`, which stays owned by the caller: a lazy record of the box's
/// layout that shares the box, so a value R only passes back to typed code is never converted.
/// R fills it one level at a time as it reads fields ([`settle`]).
unsafe fn from_box(boxed: u64) -> Result<Sexp, Unwind> {
    let (layout, fields) = shapes::fields((*(boxed as *const Boxed)).shape);
    if fields.is_empty() {
        let heap = interp::heap();
        let record = heap.alloc_record_slots(layout, &[])?;
        heap.root(record);
        return Ok(record);
    }
    collect_for_native_values();
    values::ry_list_dup(boxed as *mut List);
    interp::records::alloc_lazy(layout, fields.len(), boxed, NATIVE_VALUES)
        .inspect_err(|_| values::ry_list_drop(boxed as *mut List))
}

/// Runs a minor collection when R holds many native objects. A lazy record or a string list
/// keeps its whole native value alive while the R heap sees one small object, so values R drops
/// would never make the collector run. Native objects past a limit run a minor collection, and
/// the limit doubles past what survives, so values typed code holds itself cost collections
/// only geometrically often.
unsafe fn collect_for_native_values() {
    if values::ry_live_objects() > NATIVE_COLLECTION_AT.get() {
        interp::heap().collect(Collection::Minor);
        NATIVE_COLLECTION_AT.set((values::ry_live_objects() * 2).max(NATIVE_COLLECTION_FLOOR));
    }
}

/// The fewest live native objects at which [`collect_for_native_values`] runs a collection.
const NATIVE_COLLECTION_FLOOR: i64 = 1 << 20;

/// How R holds a `List<Str>` without converting it: a character vector over the native list,
/// whose ALTREP class the host defines (`libr`). Without a host, a string list converts each
/// time it crosses.
#[derive(Clone, Copy)]
pub struct StringLists {
    /// A character vector over `list` that takes one more reference to it.
    pub wrap: unsafe fn(*mut List) -> Result<Sexp, Unwind>,
    /// The list a character vector of that class holds, borrowed, or `None` for any other
    /// vector and for one native code has written to.
    pub list: unsafe fn(Sexp) -> Option<*mut List>,
    /// The complete character vector, flagged as the original, inside one native code has
    /// written to, or `None` for any other vector.
    pub written: unsafe fn(Sexp) -> Option<Sexp>,
}

static STRING_LISTS: OnceLock<StringLists> = OnceLock::new();

/// The shortest string list that crosses into R as a vector over its native list. A shorter
/// one is converted, which costs less than the ALTREP object and the external pointer that
/// sharing makes.
const SHARED_STRINGS_AT: usize = 64;

/// Installs the host's string lists.
pub fn install_string_lists(lists: StringLists) {
    STRING_LISTS.get_or_init(|| lists);
}

/// The character vector `vector` as a vector over a native list of its strings, for
/// `List.from_r`, or `None` when it is shorter than [`SHARED_STRINGS_AT`], already one, or holds
/// a string typed code cannot (`NA` or not UTF-8).
pub unsafe fn shared_string_vector(vector: Sexp) -> Result<Option<Sexp>, Unwind> {
    let length = rvalues::length(vector);
    if length < SHARED_STRINGS_AT
        || vector.is_altrep()
        || !(0..length).all(|index| {
            // An unmarked string in the native encoding would come back marked UTF-8.
            let string = vector.element(index);
            usable_string(string) && string.has_gp(gp::ASCII | gp::UTF8)
        })
    {
        return Ok(None);
    }
    let list = string_list(vector);
    if list.is_null() {
        return Err(interp::unwind::error("cannot allocate a ry list"));
    }
    let shared = share_strings(list);
    values::ry_list_drop(list);
    shared
}

/// A character vector over `list`, which stays owned by the caller, or `None` on a host
/// without string lists.
unsafe fn share_strings(list: *mut List) -> Result<Option<Sexp>, Unwind> {
    let Some(lists) = STRING_LISTS.get() else {
        return Ok(None);
    };
    collect_for_native_values();
    (lists.wrap)(list).map(Some)
}

/// The native list a character vector shares, borrowed.
unsafe fn string_list_of(value: Sexp) -> Option<*mut List> {
    if !value.is_altrep() {
        return None;
    }
    (STRING_LISTS.get()?.list)(value)
}

/// The complete vector inside a string list native code has written to, which converts as a
/// character vector R built.
unsafe fn written_string_list(value: Sexp) -> Option<Sexp> {
    if !value.is_altrep() {
        return None;
    }
    (STRING_LISTS.get()?.written)(value)
}

/// How R fills the lazy records [`from_box`] makes and releases their boxes.
const NATIVE_VALUES: NativeValues = NativeValues {
    settle,
    release: release_box,
};

/// Fills the lazy record `record` from its box. Scalars and strings become slots, and a record,
/// tuple or tag field becomes a lazy record of its own, so reading one field of a deep tree
/// converts one level.
unsafe fn settle(record: Sexp) -> Result<(), Unwind> {
    // Filling allocates, and a reader such as compiled code may hold `record` only in a
    // register. Were it collected mid-fill, the collection would release the box this loop
    // still reads.
    interp::heap().root(record);
    let pointer = record.lazy_record_value() as *const u8;
    let (_, fields) = shapes::fields((*(pointer as *const Boxed)).shape);
    let mut slots: SmallVec<[Slot; 4]> = SmallVec::new();
    let mut at = pointer.add(values::BOX_FIELDS) as *const u64;
    for item in fields {
        let word = *at;
        slots.push(match item {
            Item::Int => Slot::Integer(word as u32 as i32),
            Item::Lgl => Slot::Logical(word as u32 as i32),
            Item::Dbl => {
                // A double whose upper word is a raw slot's prefix is stored boxed.
                let upper = (word >> 32) as u32;
                if upper == INTEGER_PREFIX || upper == LOGICAL_PREFIX {
                    Slot::Handle(rvalues::scalar_real(f64::from_bits(word))?)
                } else {
                    Slot::Real(f64::from_bits(word))
                }
            }
            Item::I64 => Slot::Handle(interp::numbers::alloc(word as i64)?),
            Item::Str => Slot::Handle(string_vector(Str {
                lo: word,
                hi: *at.add(1),
            })?),
            Item::Record(_) | Item::Tag(_) => Slot::Handle(from_box(word)?),
            Item::Array(_) => Slot::Handle(to_r(Type::of_item(item), [word, 0])?),
        });
        at = at.add(shapes::item_words(item) as usize);
    }
    interp::heap().settle_record(record, &slots);
    Ok(())
}

/// Drops the reference a lazy record held to its box.
unsafe fn release_box(boxed: u64) {
    values::ry_list_drop(boxed as *mut List)
}

/// A rooted character vector holding `string`.
unsafe fn string_vector(string: Str) -> Result<Sexp, Unwind> {
    let vector = rvalues::alloc(SexpType::String, 1)?;
    interp::heap().set_element(vector, 0, charsxp_of(string)?);
    Ok(vector)
}

/// The R value of the native value whose words are `out`, which stays owned by the caller.
unsafe fn to_r(ty: Type, out: [u64; 2]) -> Result<Sexp, Unwind> {
    let word = out[0];
    match ty {
        Type::Int => rvalues::scalar_integer(word as u32 as i32),
        Type::Lgl | Type::Bool => rvalues::scalar_logical(word as u32 as i32),
        Type::Dbl => rvalues::scalar_real(f64::from_bits(word)),
        Type::I64 => interp::numbers::alloc(word as i64),
        Type::Null => Ok(rvalues::nil()),
        Type::Str => string_vector(Str {
            lo: out[0],
            hi: out[1],
        }),
        Type::Record(_) | Type::Tag(_) => from_box(word),
        Type::Table => Sexp::from_raw(word as *mut _)
            .ok_or_else(|| interp::unwind::error("a typed function read a table it never set")),
        Type::Absent => Err(interp::unwind::error(
            "a typed function returned a formal its call left out",
        )),
        Type::Vector {
            item: Item::Str,
            array,
        } if (*(word as *const List)).length as usize >= SHARED_STRINGS_AT
            && let Some(shared) = share_strings(word as *mut List)? =>
        {
            shared.set_array(array);
            Ok(shared)
        }
        Type::Vector { item, array } => {
            let list = word as *mut List;
            let length = (*list).length as usize;
            let vector = rvalues::alloc(item_storage(item), length)?;
            let data = (*list).data;
            match item {
                Item::Int | Item::Lgl => {
                    let items = std::slice::from_raw_parts(data as *const i32, length);
                    rvalues::ints_mut(vector).copy_from_slice(items);
                }
                Item::Dbl => {
                    let items = std::slice::from_raw_parts(data as *const f64, length);
                    rvalues::reals_mut(vector).copy_from_slice(items);
                }
                Item::I64 => {
                    let items = std::slice::from_raw_parts(data as *const i64, length);
                    vector.slice_mut::<i64>().copy_from_slice(items);
                }
                Item::Str => {
                    let items = std::slice::from_raw_parts(data as *const Str, length);
                    for (index, &string) in items.iter().enumerate() {
                        interp::heap().set_element(vector, index, charsxp_of(string)?);
                    }
                }
                Item::Record(_) | Item::Tag(_) => {
                    let items = std::slice::from_raw_parts(data as *const u64, length);
                    for (index, &boxed) in items.iter().enumerate() {
                        interp::heap().set_element(vector, index, from_box(boxed)?);
                    }
                }
                Item::Array(_) => {
                    let items = std::slice::from_raw_parts(data as *const u64, length);
                    for (index, &list) in items.iter().enumerate() {
                        let element = to_r(Type::of_item(item), [list, 0])?;
                        interp::heap().set_element(vector, index, element);
                    }
                }
            }
            vector.set_array(array);
            Ok(vector)
        }
        Type::List(item) => {
            let list = word as *mut List;
            let length = (*list).length as usize;
            let vector = rvalues::alloc(SexpType::List, length)?;
            let data = (*list).data;
            for index in 0..length {
                let element = match item {
                    Item::Int => rvalues::scalar_integer(*(data as *const i32).add(index))?,
                    Item::Lgl => rvalues::scalar_logical(*(data as *const i32).add(index))?,
                    Item::Dbl => rvalues::scalar_real(*(data as *const f64).add(index))?,
                    Item::I64 => interp::numbers::alloc(*(data as *const i64).add(index))?,
                    Item::Str => string_vector(*(data as *const Str).add(index))?,
                    Item::Record(_) | Item::Tag(_) => from_box(*(data as *const u64).add(index))?,
                    Item::Array(_) => {
                        to_r(Type::of_item(item), [*(data as *const u64).add(index), 0])?
                    }
                };
                interp::heap().set_element(vector, index, element);
            }
            Ok(vector)
        }
        Type::Set(_) | Type::Dict { .. } => Err(interp::unwind::error(
            "a typed function returned a set or a dictionary",
        )),
    }
}

/// Makes the R call `site` for compiled code, with the argument words at `words`, and writes the
/// words of its converted value to `out`. Returns 0 when the call returned, and 1 when it raised an
/// error (which [`call`] raises once the code has given up) or when a conversion whose type the
/// code guessed gave another type ([`FAILED_GUESS`]), after which [`call`] reruns it in the
/// interpreter. A call that may have done something visible is recorded ([`replay::record`]), and
/// while the interpreter reruns, a recorded call gives its recorded value instead of happening
/// twice.
///
/// # Safety
///
/// `site` is a site the code was compiled with, `words` holds the words of its arguments, and `out`
/// has room for the words of its result.
pub unsafe extern "C" fn ry_call_r(site: *const RCall, words: *const u64, out: *mut u64) -> i32 {
    let site = &*site;
    if let RResult::Global(ty) = site.result {
        return match read_global(site, ty, out) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(unwind) => {
                PARKED.set(Some(unwind));
                1
            }
        };
    }
    let replayed = replay::replayed(site.call);
    // An effect-free call changes nothing a second run would see unless it warns, so only then
    // does it count as a call the code cannot make again. A replayed call counts, since running
    // the code again must give it the same value.
    let effect_free = replayed.is_none() && changes_nothing(site);
    let warnings = interp::conditions::warnings_raised();
    let watching = WATCHING.get().filter(|watching| watching.owns(site));
    if let Some(watching) = watching
        && let Err(unwind) = settle_read(watching, site.read_before)
    {
        PARKED.set(Some(unwind));
        return 1;
    }
    let heap = interp::heap();
    let mark = heap.handle_mark();
    let outcome = match RUNNING.get() {
        Some(running) => {
            let typed = TypedCall {
                call: running.call,
                function: running.function,
                args: std::slice::from_raw_parts(running.args, running.count),
                sys_parent: running.sys_parent,
            };
            let mut made = None;
            interp::eval::run_in_frame(&typed, |env| {
                made = Some(call_r(site, words, out, env, replayed)?);
                Ok(())
            })
            .and_then(|()| {
                made.ok_or_else(|| interp::unwind::error("a typed function's R call made nothing"))
            })
        }
        None => call_r(site, words, out, site.env, replayed),
    };
    heap.release_handles(mark);
    let made = outcome.as_ref().ok().copied();
    let acted = !effect_free || interp::conditions::warnings_raised() != warnings;
    if acted {
        R_CALLS.set(R_CALLS.get() + 1);
        if let Some(made) = made {
            replay::record(site.call, made.value);
        }
    }
    if acted
        && made.is_some()
        && let Some(watching) = WATCHING.get().filter(|watching| watching.owns(site))
        && watch_tripped(watching)
    {
        WATCH_TRIPPED.set(true);
        return 1;
    }
    // Compiled code holds a table as a bare word, which the collector does not see, so the table
    // stays rooted until the call that entered typed code releases its handles.
    if matches!(site.result, RResult::Table)
        && made.is_some()
        && let Some(value) = Sexp::from_raw(*out as *mut _)
    {
        heap.root(value);
    }
    match outcome {
        Ok(Made { fits: true, .. }) => 0,
        Ok(Made { fits: false, .. }) => 1,
        Err(unwind) => {
            PARKED.set(Some(unwind));
            1
        }
    }
}

/// Whether the R call `site` may run R code that changes what a variable holds: it returns, and
/// it may change something a second run would see.
pub(crate) unsafe fn acts(site: &RCall) -> bool {
    !site.diverges && !changes_nothing(site)
}

/// Whether making the R call `site` changes nothing a second run would see unless it warns. A
/// call R evaluates among the arguments may have an effect of its own, so its site is replayed,
/// though it may still stand among another call's arguments.
unsafe fn changes_nothing(site: &RCall) -> bool {
    site.effect_free
        && !site.arguments.iter().any(|argument| {
            matches!(argument.passed, Passed::Code) && argument.code.kind() == SexpType::Language
        })
}

/// Settles the promises of the arguments at `read`, as bits by position, that `watching` has not
/// settled yet: the code has read those parameters, where R forces them, and the R call it makes
/// next must not change what they are.
unsafe fn settle_read(mut watching: Watching, read: u64) -> Result<(), Unwind> {
    let unsettled = read & !watching.settled;
    if unsettled == 0 {
        return Ok(());
    }
    watching.settled |= unsettled;
    WATCHING.set(Some(watching));
    for &(promise, value, param) in &(*watching.ahead).promises {
        if param_bit(param) & unsettled != 0 {
            interp::eval::settle_unforced(promise, value)?;
        }
    }
    Ok(())
}

/// Whether a variable an unsettled argument of `watching` was read from is bound to something
/// else now.
unsafe fn watch_tripped(watching: Watching) -> bool {
    (*watching.ahead).watches.iter().any(|watch| {
        if param_bit(watch.param) & watching.late & !watching.settled == 0 {
            return false;
        }
        let now = match watch.variable {
            Watched::Variable { symbol, env } => interp::eval::peek_variable(symbol, env),
            Watched::Reference(reference) => interp::eval::reference_variable(reference),
        };
        now != Some(watch.bound)
    })
}

/// Reads the global that `site` names for compiled code, which was compiled for a value of type
/// `ty`, and writes its words to `out`. `false` when the read runs R code or finds no global, as
/// when the binding was removed, so the interpreter runs the call again and reads it as R does.
/// A value of another type is an error, as an argument of another type is at the boundary.
unsafe fn read_global(site: &RCall, ty: Type, out: *mut u64) -> Result<bool, Unwind> {
    let value = match lower::global_binding(site.call, site.env) {
        Global::Value(value) => value,
        // R forces a promise where the variable is read, such as a package variable's lazy-load
        // promise, which code attached from a typed code file meets on its first call.
        // The forced promise keeps its value, so the binding keeps it alive.
        Global::Promise(promise) => interp::eval::force_promise(promise)?,
        Global::Unbound | Global::Enclosing | Global::Active => {
            return Ok(false);
        }
    };
    let found = type_of(value);
    if found != Some(ty) {
        let found = match found.filter(|&found| lower::reads_globally(found)) {
            Some(found) => type_name(found),
            None if rvalues::length(value) == 1 || value.is_array() => {
                format!("`{}`", value.kind().name())
            }
            None => format!(
                "a `{}` vector of length {}",
                value.kind().name(),
                rvalues::length(value)
            ),
        };
        return Err(interp::unwind::error(format!(
            "the global `{}` must be {}, the type it had when the typed function compiled, not \
             {found}",
            rvalues::symbol_name(site.call),
            type_name(ty)
        )));
    }
    if ty == Type::Table {
        // Compiled code holds a table as a bare word, which the collector does not see, so the
        // table stays rooted until the call that entered typed code releases its handles.
        interp::heap().root(value);
        *out = value.as_ptr() as u64;
        return Ok(true);
    }
    write_native(ty, value, out)?;
    Ok(true)
}

/// How an error names `ty`, a type [`lower::reads_globally`] admits.
fn type_name(ty: Type) -> String {
    let item = |item: Item| match item {
        Item::I64 => "I64",
        Item::Lgl => "Bool",
        Item::Str => "Str",
        _ => "F64",
    };
    match ty {
        Type::Table => String::from("a table value"),
        Type::Vector { item: inner, .. } => format!("`List<{}>`", item(inner)),
        Type::I64 => String::from("`I64`"),
        Type::Lgl => String::from("`Bool`"),
        Type::Str => String::from("`Str`"),
        _ => String::from("`F64`"),
    }
}

/// What an R call compiled code made gave: the value the R function returned, `NULL` when the
/// code does not use it, and whether its conversion gave the type the code was compiled for.
#[derive(Clone, Copy)]
struct Made {
    value: Sexp,
    fits: bool,
}

unsafe fn call_r(
    site: &RCall,
    words: *const u64,
    out: *mut u64,
    env: Sexp,
    replayed: Option<Sexp>,
) -> Result<Made, Unwind> {
    let heap = interp::heap();
    let value = match replayed {
        Some(value) => value,
        None => apply_r(site, words, env)?,
    };
    let (conversion, ty) = match site.result {
        RResult::Unused => {
            return Ok(Made {
                value: rvalues::nil(),
                fits: true,
            });
        }
        RResult::Converted(conversion, ty) => (conversion, ty),
        RResult::Global(_) => {
            return Err(interp::unwind::error(
                "a typed function read a global as it would call R",
            ));
        }
        RResult::Table => {
            if !(interp::interp().extensions.tables.is_table_value)(value) {
                return Err(interp::unwind::error(format!(
                    "`{}` gave a value of type `{}` where the typed function was compiled for a \
                     table value",
                    interp::deparse::deparse_first_line(site.call.car()),
                    value.kind().name()
                )));
            }
            *out = value.as_ptr() as u64;
            return Ok(Made { value, fits: true });
        }
    };
    heap.root(value);
    // A `List.from_r_or`'s default follows the arguments' words.
    let default = match conversion.default {
        Some(ty) => {
            let offset: usize = site
                .arguments
                .iter()
                .map(|argument| match argument.passed {
                    Passed::Value(ty, _) => backend::words(ty).len(),
                    Passed::Code => 0,
                })
                .sum();
            let mut native = [0u64; 2];
            for (position, word) in native.iter_mut().take(backend::words(ty).len()).enumerate() {
                *word = *words.add(offset + position);
            }
            let default = to_r(ty, native)?;
            heap.root(default);
            Some(default)
        }
        None => None,
    };
    let converted = convert(conversion, site.call, value, default, site.env)?;
    heap.root(converted);
    let fits = write_converted(conversion, ty, converted, out)?;
    Ok(Made { value, fits })
}

/// Calls the R function of `site` with the argument words at `words` in `env`.
unsafe fn apply_r(site: &RCall, mut words: *const u64, env: Sexp) -> Result<Sexp, Unwind> {
    let heap = interp::heap();
    let mut args: SmallVec<[Arg; 4]> = SmallVec::new();
    for argument in &site.arguments {
        let promise = match argument.passed {
            // A literal is its own value, as the interpreter passes it.
            Passed::Code
                if !matches!(argument.code.kind(), SexpType::Symbol | SexpType::Language) =>
            {
                interp::eval::forced_promise(argument.code, argument.code)?
            }
            Passed::Code => interp::eval::make_promise(argument.code, site.env)?,
            Passed::Value(ty, conversion) => {
                let mut native = [0u64; 2];
                for word in native.iter_mut().take(backend::words(ty).len()) {
                    *word = *words;
                    words = words.add(1);
                }
                let mut value = to_r(ty, native)?;
                heap.root(value);
                if let Some(conversion) = conversion {
                    value = convert(
                        conversion,
                        conversion.call.cdr().car(),
                        value,
                        None,
                        site.env,
                    )?;
                    heap.root(value);
                }
                interp::eval::forced_promise(argument.code, value)?
            }
        };
        heap.root(promise);
        args.push(Arg {
            tag: argument.tag,
            value: promise,
        });
    }
    interp::eval::apply(site.call, site.function, &args, env)
}

/// Applies the conversion `conversion` to `value`, the value of the code `code`, as the
/// interpreter applies it, with `default` as a `List.from_r_or`'s second argument.
unsafe fn convert(
    conversion: lower::Conversion,
    code: Sexp,
    value: Sexp,
    default: Option<Sexp>,
    env: Sexp,
) -> Result<Sexp, Unwind> {
    let heap = interp::heap();
    let promise = interp::eval::forced_promise(code, value)?;
    heap.root(promise);
    let mut arguments: SmallVec<[Arg; 2]> = SmallVec::new();
    arguments.push(Arg {
        tag: rvalues::nil(),
        value: promise,
    });
    if let Some(default) = default {
        let code = conversion.call.cdr().cdr().car();
        let promise = interp::eval::forced_promise(code, default)?;
        heap.root(promise);
        arguments.push(Arg {
            tag: rvalues::nil(),
            value: promise,
        });
    }
    interp::eval::apply(conversion.call, conversion.function, &arguments, env)
}

/// Writes the words of `value`, which the conversion `conversion` gave, as a native value of
/// `ty`, the type it gave when the code compiled. `false` when a conversion whose type the code
/// guessed gave a value of another type ([`FAILED_GUESS`]), which writes nothing.
unsafe fn write_converted(
    conversion: lower::Conversion,
    ty: Type,
    value: Sexp,
    out: *mut u64,
) -> Result<bool, Unwind> {
    let missing = rvalues::symbol_name(conversion.call.car()) == "List.from_r_missing";
    let value = match conversion.checked {
        Some(item) if rvalues::symbol_name(conversion.call.car()) == "Num.from_r" => {
            let expected = match item {
                Item::I64 => Items::I64,
                _ => Items::Double,
            };
            let number = interp::numbers::checked_number(conversion.call, value, expected)?;
            interp::heap().root(number);
            number
        }
        Some(item) if value.is_array() => {
            let expected = match item {
                Item::I64 => Items::I64,
                Item::Dbl => Items::Double,
                _ => Items::Character,
            };
            let list = if missing {
                checked_missing(conversion.call, value, expected)?
            } else {
                checked_conversion(conversion.call, value, expected)?
            };
            interp::heap().root(list);
            list
        }
        _ => value,
    };
    let expected = storage(ty).map(|(kind, _)| kind);
    if expected != Some(value.kind())
        || rvalues::length(value) != 1 && !matches!(ty, Type::Vector { .. })
    {
        if conversion.guessed() {
            FAILED_GUESS.set(Some((conversion.call, value.kind())));
            return Ok(false);
        }
        return Err(interp::unwind::error(format!(
            "`{}` gave a value of type `{}`, and the typed function was compiled for the type \
             it gave before: convert the value to one type in R first",
            interp::deparse::deparse_first_line(conversion.call),
            value.kind().name()
        )));
    }
    if let Type::Vector {
        item: Item::Tag(union),
        ..
    } = ty
    {
        match results_list(value, union)? {
            Ok(list) => *out = list as u64,
            Err(payload) if conversion.guessed() => {
                FAILED_GUESS.set(Some((conversion.call, payload)));
                return Ok(false);
            }
            Err(payload) => {
                return Err(interp::unwind::error(format!(
                    "`{}` gave results of `{}`, and the typed function was compiled for the type \
                     it gave before: convert the value to one type in R first",
                    interp::deparse::deparse_first_line(conversion.call),
                    payload.name()
                )));
            }
        }
        return Ok(true);
    }
    write_native(ty, value, out)?;
    Ok(true)
}

/// Writes the words of the R value `value` as a new native value of `ty`, a scalar or a vector
/// of scalars whose storage `value` has.
unsafe fn write_native(ty: Type, value: Sexp, out: *mut u64) -> Result<(), Unwind> {
    match ty {
        Type::Dbl => *out = rvalues::reals(value)[0].to_bits(),
        Type::I64 => *out = value.slice::<i64>()[0] as u64,
        Type::Lgl | Type::Bool => *out = rvalues::ints(value)[0] as u32 as u64,
        Type::Str => {
            let string = str_of(value.element(0));
            *out = string.lo;
            *out.add(1) = string.hi;
        }
        Type::Vector {
            item: Item::Str, ..
        } => {
            let list = string_list(value);
            if list.is_null() {
                return Err(interp::unwind::error("cannot allocate a ry list"));
            }
            *out = list as u64;
        }
        Type::Vector { item, .. } => {
            let length = rvalues::length(value);
            let list = values::ry_list_new(item_kind(item), length as u64);
            if list.is_null() {
                return Err(interp::unwind::error("cannot allocate a ry list"));
            }
            let size = match item {
                Item::Int | Item::Lgl => 4,
                _ => 8,
            };
            std::ptr::copy_nonoverlapping(
                value.data() as *const u8,
                (*list).data as *mut u8,
                size * length,
            );
            *out = list as u64;
        }
        _ => {
            return Err(interp::unwind::error(
                "a value from R has a type typed code does not take from R",
            ));
        }
    }
    Ok(())
}

/// A native list of the `:Ok(item)` and `:Err(:Missing)` tags in `value`, the R list a
/// `List.from_r_missing` gave, whose boxes must have the shapes of `union`. Otherwise the R type
/// of the first payload of another shape.
unsafe fn results_list(value: Sexp, union: u32) -> Result<Result<*mut List, SexpType>, Unwind> {
    let members = shapes::members(union).unwrap_or_default();
    let length = rvalues::length(value);
    let list = values::ry_list_new(item_kind(Item::Tag(union)), length as u64);
    if list.is_null() {
        return Err(interp::unwind::error("cannot allocate a ry list"));
    }
    // The list owns each item as soon as it is written, so dropping it drops the items written
    // before a failure, and the rest are null.
    for index in 0..length {
        let item = value.element(index);
        let boxed = match to_box(item) {
            Ok((boxed, _)) => boxed,
            Err(reason) => {
                values::ry_list_drop(list);
                return Err(interp::unwind::error(reason));
            }
        };
        *((*list).data as *mut u64).add(index) = boxed;
        if !members.contains(&(*(boxed as *const Boxed)).shape) {
            values::ry_list_drop(list);
            let payload = match item.record_slot(0) {
                Slot::Real(_) => SexpType::Real,
                Slot::Integer(_) => SexpType::Integer,
                Slot::Logical(_) => SexpType::Logical,
                Slot::Handle(field) => field.kind(),
            };
            return Ok(Err(payload));
        }
    }
    Ok(Ok(list))
}

unsafe fn charsxp_of(string: Str) -> Result<Sexp, Unwind> {
    rvalues::mkchar_bytes(string.bytes(), Encoding::Utf8)
}

/// Drops the native value whose words are `out`.
unsafe fn drop_words(ty: Type, out: [u64; 2]) {
    match ty {
        Type::Str => values::ry_str_drop(out[0], out[1]),
        Type::Vector { .. }
        | Type::List(_)
        | Type::Set(_)
        | Type::Dict { .. }
        | Type::Record(_)
        | Type::Tag(_) => values::ry_list_drop(out[0] as *mut List),
        _ => {}
    }
}

/// How many more calls deep compiled code may go before it gives up, which leaves the
/// interpreter to report running out of stack as it would.
fn depth_budget() -> i64 {
    let state = interp::interp();
    i64::from(state.max_depth) - state.frames.len() as i64
}
